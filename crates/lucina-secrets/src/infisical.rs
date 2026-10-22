//! The Infisical client: Universal Auth login, token renewal, one secret by
//! name and folder, and names without values.
//!
//! Endpoints (self-hosted `https://secrets.pokoin.com`):
//!
//! - `POST /api/v1/auth/universal-auth/login` `{clientId, clientSecret}` ->
//!   `{accessToken, expiresIn, accessTokenMaxTTL, tokenType}`
//! - `POST /api/v1/auth/token/renew` `{accessToken}` -> the same shape
//! - v4: `GET /api/v4/secrets/{name}?projectId&environment&secretPath`,
//!   `GET /api/v4/secrets?projectId&environment&secretPath&viewSecretValue=false`
//! - v3: `GET /api/v3/secrets/raw/{name}?workspaceId&environment&secretPath`,
//!   `GET /api/v3/secrets/raw?workspaceId&environment&secretPath` (values
//!   come back; they are wiped right after parsing)
//!
//! Response bodies that may hold values are parsed from a buffer that is
//! zeroized afterwards; error messages carry the HTTP status and
//! Infisical's message only. The machine identity's credentials and every
//! access token are registered with the redactor.

use std::path::Path;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::json;
use zeroize::Zeroize;

use crate::config::{ApiVersion, InfisicalConfig};
use crate::{redactor, SecretError, SecretValue};

/// Universal Auth credentials, from two files (systemd credentials).
pub struct Credentials {
    pub client_id: SecretString,
    pub client_secret: SecretString,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials([redacted])")
    }
}

fn read_credential(dir: &Path, name: &str) -> Result<SecretString, SecretError> {
    let path = dir.join(name);
    let mut raw = std::fs::read(&path).map_err(|e| {
        SecretError::Config(format!(
            "credential {name} in {}: {}",
            dir.display(),
            e.kind()
        ))
    })?;
    let text = String::from_utf8(raw.clone())
        .map_err(|_| SecretError::Config(format!("credential {name} is not UTF-8")));
    raw.zeroize();
    let mut text = text?;
    let value = text.trim().to_owned();
    text.zeroize();
    if value.is_empty() {
        return Err(SecretError::Config(format!("credential {name} is empty")));
    }
    redactor().register(
        &format!("INFISICAL_{}", name.to_uppercase().replace('-', "_")),
        &value,
    );
    Ok(SecretString::from(value))
}

impl Credentials {
    /// `<dir>/<id_name>` and `<dir>/<secret_name>` (trimmed).
    pub fn from_dir(dir: &Path, id_name: &str, secret_name: &str) -> Result<Self, SecretError> {
        Ok(Self {
            client_id: read_credential(dir, id_name)?,
            client_secret: read_credential(dir, secret_name)?,
        })
    }

    pub fn new(client_id: String, client_secret: String) -> Self {
        redactor().register("INFISICAL_CLIENT_ID", &client_id);
        redactor().register("INFISICAL_CLIENT_SECRET", &client_secret);
        Self {
            client_id: SecretString::from(client_id),
            client_secret: SecretString::from(client_secret),
        }
    }
}

struct Token {
    access: SecretString,
    /// Renew from here on.
    refresh_at: Instant,
    expires_at: Instant,
    /// The token's max TTL: no renewal past this.
    max_until: Instant,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginResponse {
    access_token: String,
    expires_in: u64,
    #[serde(default, rename = "accessTokenMaxTTL")]
    access_token_max_ttl: Option<u64>,
}

/// One secret as Infisical returns it (value wiped after use).
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSecret {
    secret_key: String,
    #[serde(default)]
    secret_value: Option<String>,
    #[serde(default)]
    version: Option<u64>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    secret_path: Option<String>,
}

impl Drop for RawSecret {
    fn drop(&mut self) {
        if let Some(v) = self.secret_value.as_mut() {
            v.zeroize();
        }
    }
}

#[derive(Deserialize)]
struct OneSecret {
    secret: RawSecret,
}

#[derive(Deserialize)]
struct ManySecrets {
    secrets: Vec<RawSecret>,
}

/// A secret fetched from Infisical.
pub struct Fetched {
    pub value: SecretValue,
    pub version: Option<u64>,
    pub updated_at: Option<String>,
}

/// A secret's name and metadata (no value).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretMeta {
    pub key: String,
    /// The folder, when Infisical reports it (recursive listings).
    pub path: Option<String>,
    pub version: Option<u64>,
    /// Last change (rotation) as Infisical reports it.
    pub updated_at: Option<String>,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    message: serde_json::Value,
}

pub struct InfisicalClient {
    http: reqwest::Client,
    base: String,
    api: ApiVersion,
    org: Option<String>,
    creds: Credentials,
    token: tokio::sync::Mutex<Option<Token>>,
}

impl std::fmt::Debug for InfisicalClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InfisicalClient")
            .field("base", &self.base)
            .field("api", &self.api)
            .finish_non_exhaustive()
    }
}

/// Read a body into a buffer we can wipe.
async fn body_bytes(resp: reqwest::Response) -> Result<BytesMut, SecretError> {
    let b = resp
        .bytes()
        .await
        .map_err(|e| SecretError::Network(e.without_url().to_string()))?;
    Ok(match b.try_into_mut() {
        Ok(m) => m,
        Err(b) => BytesMut::from(&b[..]),
    })
}

fn wipe(mut b: BytesMut) {
    b.as_mut().zeroize();
}

async fn error_for(resp: reqwest::Response) -> SecretError {
    let status = resp.status().as_u16();
    let msg = match body_bytes(resp).await {
        Ok(b) => {
            let m = serde_json::from_slice::<ErrorBody>(&b)
                .ok()
                .map(|e| match e.message {
                    serde_json::Value::String(s) => s,
                    serde_json::Value::Null => String::new(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            wipe(b);
            m
        }
        Err(_) => String::new(),
    };
    let msg: String = redactor().redact(&msg).chars().take(200).collect();
    match status {
        401 | 403 => SecretError::Unauthorized(format!("HTTP {status}: {msg}")),
        _ => SecretError::Http(status, msg),
    }
}

impl InfisicalClient {
    pub fn new(cfg: &InfisicalConfig, creds: Credentials) -> Result<Self, SecretError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("lucina-secrets/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(cfg.timeout_secs.clamp(1, 60)))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| SecretError::Config(e.to_string()))?;
        Ok(Self {
            http,
            base: cfg.url.trim_end_matches('/').to_owned(),
            api: cfg.api,
            org: cfg.organization_slug.clone(),
            creds,
            token: tokio::sync::Mutex::new(None),
        })
    }

    fn adopt(&self, r: LoginResponse, now: Instant, max_until: Option<Instant>) -> Token {
        redactor().register("INFISICAL_ACCESS_TOKEN", &r.access_token);
        let ttl = Duration::from_secs(r.expires_in.max(1));
        let max_until = max_until.unwrap_or_else(|| {
            now + Duration::from_secs(r.access_token_max_ttl.unwrap_or(r.expires_in).max(1))
        });
        let mut r = r;
        let token = Token {
            access: SecretString::from(std::mem::take(&mut r.access_token)),
            refresh_at: now + ttl.mul_f64(0.8),
            expires_at: now + ttl,
            max_until,
        };
        r.access_token.zeroize();
        token
    }

    async fn login(&self) -> Result<Token, SecretError> {
        let mut body = json!({
            "clientId": self.creds.client_id.expose_secret(),
            "clientSecret": self.creds.client_secret.expose_secret(),
        });
        if let Some(org) = &self.org {
            body["organizationSlug"] = json!(org);
        }
        let payload = serde_json::to_vec(&body).unwrap_or_default();
        if let Some(o) = body.as_object_mut() {
            for v in o.values_mut() {
                if let serde_json::Value::String(s) = v {
                    s.zeroize();
                }
            }
        }
        let resp = self
            .http
            .post(format!("{}/api/v1/auth/universal-auth/login", self.base))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload)
            .send()
            .await
            .map_err(|e| SecretError::Network(e.without_url().to_string()))?;
        if !resp.status().is_success() {
            return Err(error_for(resp).await);
        }
        let b = body_bytes(resp).await?;
        let parsed = serde_json::from_slice::<LoginResponse>(&b)
            .map_err(|_| SecretError::Http(200, "unexpected login response".into()));
        wipe(b);
        Ok(self.adopt(parsed?, Instant::now(), None))
    }

    async fn renew(&self, old: &Token) -> Result<Token, SecretError> {
        let resp = self
            .http
            .post(format!("{}/api/v1/auth/token/renew", self.base))
            .json(&json!({"accessToken": old.access.expose_secret()}))
            .send()
            .await
            .map_err(|e| SecretError::Network(e.without_url().to_string()))?;
        if !resp.status().is_success() {
            return Err(error_for(resp).await);
        }
        let b = body_bytes(resp).await?;
        let parsed = serde_json::from_slice::<LoginResponse>(&b)
            .map_err(|_| SecretError::Http(200, "unexpected renew response".into()));
        wipe(b);
        Ok(self.adopt(parsed?, Instant::now(), Some(old.max_until)))
    }

    /// A valid access token: cached, renewed at 80 % of its TTL (while the
    /// max TTL allows), otherwise a fresh login.
    async fn access_token(&self, force_login: bool) -> Result<SecretString, SecretError> {
        let mut slot = self.token.lock().await;
        let now = Instant::now();
        if !force_login {
            if let Some(t) = slot.as_ref() {
                if now < t.refresh_at {
                    return Ok(t.access.clone());
                }
                if now + Duration::from_secs(30) < t.max_until && now < t.expires_at {
                    match self.renew(t).await {
                        Ok(nt) => {
                            let out = nt.access.clone();
                            *slot = Some(nt);
                            return Ok(out);
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "infisical token renew failed; logging in again")
                        }
                    }
                }
            }
        }
        let t = self.login().await?;
        let out = t.access.clone();
        *slot = Some(t);
        Ok(out)
    }

    async fn get_json(&self, path: &str, query: &[(&str, &str)]) -> Result<BytesMut, SecretError> {
        let mut forced = false;
        loop {
            let token = self.access_token(forced).await?;
            let mut auth = reqwest::header::HeaderValue::from_str(&format!(
                "Bearer {}",
                token.expose_secret()
            ))
            .map_err(|_| SecretError::Unauthorized("bad token".into()))?;
            auth.set_sensitive(true);
            let resp = self
                .http
                .get(format!("{}{path}", self.base))
                .query(query)
                .header(reqwest::header::AUTHORIZATION, auth)
                .send()
                .await
                .map_err(|e| SecretError::Network(e.without_url().to_string()))?;
            if resp.status().as_u16() == 401 && !forced {
                forced = true;
                continue;
            }
            if !resp.status().is_success() {
                return Err(error_for(resp).await);
            }
            return body_bytes(resp).await;
        }
    }

    fn scope_query<'a>(
        &self,
        project_id: &'a str,
        environment: &'a str,
        folder: &'a str,
    ) -> Vec<(&'static str, &'a str)> {
        let project_param = match self.api {
            ApiVersion::V4 => "projectId",
            ApiVersion::V3 => "workspaceId",
        };
        vec![
            (project_param, project_id),
            ("environment", environment),
            ("secretPath", folder),
        ]
    }

    /// One secret's value.
    pub async fn get_secret(
        &self,
        project_id: &str,
        environment: &str,
        folder: &str,
        key: &str,
    ) -> Result<Fetched, SecretError> {
        let mut q = self.scope_query(project_id, environment, folder);
        q.push(("type", "shared"));
        let path = match self.api {
            ApiVersion::V4 => {
                q.push(("viewSecretValue", "true"));
                q.push(("expandSecretReferences", "true"));
                format!("/api/v4/secrets/{key}")
            }
            ApiVersion::V3 => {
                q.push(("expandSecretReferences", "true"));
                format!("/api/v3/secrets/raw/{key}")
            }
        };
        let b = match self.get_json(&path, &q).await {
            Err(SecretError::Http(404, _)) => return Err(SecretError::NotFound(key.to_owned())),
            other => other?,
        };
        let parsed = serde_json::from_slice::<OneSecret>(&b);
        wipe(b);
        let mut one =
            parsed.map_err(|_| SecretError::Http(200, "unexpected secret response".into()))?;
        let value = one
            .secret
            .secret_value
            .take()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| SecretError::NotFound(key.to_owned()))?;
        Ok(Fetched {
            value: SecretValue::new(value),
            version: one.secret.version,
            updated_at: one
                .secret
                .updated_at
                .clone()
                .or_else(|| one.secret.created_at.clone()),
        })
    }

    /// Names and metadata in a folder, without values.
    pub async fn list(
        &self,
        project_id: &str,
        environment: &str,
        folder: &str,
    ) -> Result<Vec<SecretMeta>, SecretError> {
        let mut q = self.scope_query(project_id, environment, folder);
        let path = match self.api {
            ApiVersion::V4 => {
                q.push(("viewSecretValue", "false"));
                q.push(("includeImports", "false"));
                q.push(("recursive", "true"));
                "/api/v4/secrets"
            }
            ApiVersion::V3 => {
                q.push(("include_imports", "false"));
                q.push(("recursive", "true"));
                "/api/v3/secrets/raw"
            }
        };
        let b = self.get_json(path, &q).await?;
        let parsed = serde_json::from_slice::<ManySecrets>(&b);
        wipe(b);
        let many = parsed.map_err(|_| SecretError::Http(200, "unexpected list response".into()))?;
        Ok(many
            .secrets
            .iter()
            .map(|s| SecretMeta {
                key: s.secret_key.clone(),
                path: s
                    .secret_path
                    .as_deref()
                    .map(|p| format!("/{}", p.trim_matches('/')))
                    .filter(|p| p.as_str() != "/" || folder == "/"),
                version: s.version,
                updated_at: s.updated_at.clone().or_else(|| s.created_at.clone()),
            })
            .collect())
    }

    /// Every value in a folder, handed to `each` one at a time and wiped
    /// right after (the redactor preload).
    pub async fn for_each_value(
        &self,
        project_id: &str,
        environment: &str,
        folder: &str,
        mut each: impl FnMut(&str, &str),
    ) -> Result<usize, SecretError> {
        let mut q = self.scope_query(project_id, environment, folder);
        let path = match self.api {
            ApiVersion::V4 => {
                q.push(("viewSecretValue", "true"));
                q.push(("includeImports", "false"));
                q.push(("recursive", "true"));
                "/api/v4/secrets"
            }
            ApiVersion::V3 => {
                q.push(("include_imports", "false"));
                "/api/v3/secrets/raw"
            }
        };
        let b = self.get_json(path, &q).await?;
        let parsed = serde_json::from_slice::<ManySecrets>(&b);
        wipe(b);
        let many = parsed.map_err(|_| SecretError::Http(200, "unexpected list response".into()))?;
        let mut n = 0;
        for s in &many.secrets {
            if let Some(v) = s.secret_value.as_deref().filter(|v| !v.is_empty()) {
                each(&s.secret_key, v);
                n += 1;
            }
        }
        Ok(n)
    }
}
