//! "Add secret": the owner adds secrets and creates projects in Infisical
//! from the apps (`/v1/secrets*`).
//!
//! - Device login only (Bearer device token or the browser session with
//!   CSRF); Lucina's service token is refused like any other non-device
//!   credential. The owner's tap in the app is the confirmation.
//! - Per request this server signs a single-use **owner grant** (Ed25519,
//!   see infisical-mcp `src/grant.rs`) that names one action and all its
//!   parameters, and forwards it to Lucina (`POST /api/secretary/secrets/grant`),
//!   which relays it to the broker. The broker holds only the public key, so
//!   nothing on Lucina's machine can make a grant.
//! - The signing key is 32 random bytes made on first use in
//!   `$SECRETARY_DATA_DIR/secret-grant.key` (PKCS#8, mode 0600, never logged
//!   or returned); the public key is `GET /v1/secrets/status` and
//!   `secretary-server secret-grant-key`.
//! - The value passes through in memory only: never logged (the access log
//!   keeps the body length), stored, echoed in an error or put in a grant
//!   (the grant carries its sha256).
//! - One exception is stored here: a secret named `APNS_AUTH_KEY_<KEYID>`
//!   (an APNs `.p8`) is also installed as this server's APNs key before it
//!   is forwarded, so push turns on at once (see [`crate::notify::apns`]).

use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use axum::extract::{Extension, Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ring::signature::{Ed25519KeyPair, KeyPair};
use secretary_core::Call;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::auth::Identity;
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::AppState;

pub const KEY_FILE: &str = "secret-grant.key";
/// Signed bytes = this context + the payload.
pub const SIGNING_CONTEXT: &[u8] = b"infisical-broker-grant-v1\n";
pub const AUDIENCE: &str = "infisical-brokerd";
/// Grant lifetime (the broker accepts at most 120 s).
pub const GRANT_TTL_SECS: u64 = 90;
pub const MAX_VALUE_BYTES: usize = 65_536;

pub struct GrantSigner {
    path: PathBuf,
    key: OnceLock<Ed25519KeyPair>,
}

impl std::fmt::Debug for GrantSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrantSigner")
            .field("path", &self.path)
            .finish()
    }
}

/// The PKCS#8 bytes of the Ed25519 key at `path`, made (0600) when missing.
pub(crate) fn load_or_create(path: &Path) -> io::Result<Vec<u8>> {
    if let Some(dir) = path.parent() {
        crate::devices::ensure_dir(dir)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut f) => {
            let rng = ring::rand::SystemRandom::new();
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
                .map_err(|_| io::Error::other("key generation failed"))?;
            f.write_all(pkcs8.as_ref())?;
            f.sync_all()?;
            Ok(pkcs8.as_ref().to_vec())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let meta = std::fs::metadata(path)?;
            if meta.permissions().mode() & 0o077 != 0 {
                tracing::warn!(path = %path.display(), "grant key is readable by group/others; chmod 600 it");
            }
            let mut buf = Vec::new();
            std::fs::File::open(path)?.read_to_end(&mut buf)?;
            Ok(buf)
        }
        Err(e) => Err(e),
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl GrantSigner {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(KEY_FILE),
            key: OnceLock::new(),
        }
    }

    fn key(&self) -> io::Result<&Ed25519KeyPair> {
        if let Some(k) = self.key.get() {
            return Ok(k);
        }
        let raw = load_or_create(&self.path)?;
        let kp = Ed25519KeyPair::from_pkcs8(&raw).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "secret-grant.key is not an Ed25519 PKCS#8 key",
            )
        })?;
        let _ = self.key.set(kp);
        Ok(self.key.get().expect("key set"))
    }

    /// The public key, standard base64 (for broker.toml).
    pub fn public_key(&self) -> io::Result<String> {
        Ok(STANDARD.encode(self.key()?.public_key().as_ref()))
    }

    /// Sign a grant for `act` (an object with `act` and its fields),
    /// confirmed on `device_id`. Returns `(grant, jti)`.
    pub fn mint(&self, device_id: &str, act: Map<String, Value>) -> io::Result<(String, String)> {
        let mut jti = [0u8; 16];
        getrandom::fill(&mut jti).map_err(|e| io::Error::other(e.to_string()))?;
        let jti: String = jti.iter().map(|b| format!("{b:02x}")).collect();
        let now = now_unix();
        let mut payload = Map::new();
        payload.insert("v".into(), json!(1));
        payload.insert("aud".into(), json!(AUDIENCE));
        payload.insert("jti".into(), json!(jti));
        payload.insert("iat".into(), json!(now));
        payload.insert("exp".into(), json!(now + GRANT_TTL_SECS));
        payload.insert("sub".into(), json!(format!("device:{device_id}")));
        payload.extend(act);
        let bytes = serde_json::to_vec(&Value::Object(payload)).map_err(io::Error::other)?;
        let mut signed = SIGNING_CONTEXT.to_vec();
        signed.extend_from_slice(&bytes);
        let sig = self.key()?.sign(&signed);
        Ok((
            format!(
                "g1.{}.{}",
                URL_SAFE_NO_PAD.encode(&bytes),
                URL_SAFE_NO_PAD.encode(sig.as_ref())
            ),
            jti,
        ))
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", msg)
}

fn signer_error(e: &io::Error) -> ApiError {
    tracing::error!(error = %e, "secret grant key unavailable");
    ApiError::internal("secret grant key unavailable")
}

fn id_ok(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A secret name: `[A-Za-z0-9_.@-]`, 1-256.
pub fn name_ok(s: &str) -> bool {
    (1..=256).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'@' | b'-'))
}

/// `/`, `/a/b`, `a/b/` -> `/a/b`; segments `[A-Za-z0-9_.@-]`, not `.`/`..`.
pub fn normalize_folder(path: &str) -> Option<String> {
    let t = path.trim().trim_matches('/');
    if t.is_empty() {
        return Some("/".into());
    }
    let segs: Vec<&str> = t.split('/').collect();
    let ok = segs
        .iter()
        .all(|s| !s.is_empty() && s.len() <= 128 && *s != "." && *s != ".." && name_ok(s));
    ok.then(|| format!("/{}", segs.join("/")))
}

fn str_field<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str)
}

fn need_id(body: &Value, key: &str) -> Result<String, ApiError> {
    match str_field(body, key) {
        Some(s) if id_ok(s, 128) => Ok(s.to_owned()),
        _ => Err(bad(format!("{key}: required, letters, digits, - and _"))),
    }
}

fn confirmed(body: &Value) -> Result<(), ApiError> {
    if body.get("confirmed") == Some(&Value::Bool(true)) {
        Ok(())
    } else {
        Err(bad(
            "confirmed: must be true (the owner confirms in the app)",
        ))
    }
}

async fn forward_grant(
    st: &AppState,
    grant: String,
    value: Option<String>,
    held: Option<String>,
    request: Option<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let call = Call::secrets_grant(grant, value, held, request)?;
    let (status, data) = st.lucina()?.call_with_status::<Value>(&call).await?;
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(data),
    ))
}

fn mint(st: &AppState, who: &Identity, act: Value) -> Result<String, ApiError> {
    let Value::Object(act) = act else {
        return Err(ApiError::internal("bad grant"));
    };
    let (grant, jti) = st
        .grant_signer
        .mint(&who.device_id, act)
        .map_err(|e| signer_error(&e))?;
    tracing::info!(device = %who.device_id, jti = %jti, "owner grant minted");
    Ok(grant)
}

/// `GET /v1/secrets/status`: the public key the broker must trust.
async fn status(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let key = st.grant_signer.public_key().map_err(|e| signer_error(&e))?;
    Ok(Json(json!({"grantKey": key, "audience": AUDIENCE})))
}

/// `GET /v1/secrets/catalog`: projects and environments (names only).
async fn catalog(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let grant = mint(&st, &who, json!({"act": "catalog"}))?;
    forward_grant(&st, grant, None, None, None).await
}

#[derive(serde::Deserialize, Default)]
struct NamesQuery {
    project: Option<String>,
    environment: Option<String>,
    path: Option<String>,
}

/// `GET /v1/secrets/names?project=&environment=&path=`: folders and secret
/// names in one folder (no values).
async fn names(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
    QueryArgs(q): QueryArgs<NamesQuery>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let fields = json!({"project": q.project, "environment": q.environment});
    let project = need_id(&fields, "project")?;
    let environment = need_id(&fields, "environment")?;
    let path = normalize_folder(q.path.as_deref().unwrap_or("/"))
        .ok_or_else(|| bad("path: folders of letters, digits, _ . @ -"))?;
    let grant = mint(
        &st,
        &who,
        json!({"act": "catalog", "project": project, "environment": environment, "path": path}),
    )?;
    forward_grant(&st, grant, None, None, None).await
}

const APNS_KEY_PREFIX: &str = "APNS_AUTH_KEY_";

/// The key id in a secret named `APNS_AUTH_KEY_<10 letters/digits>` (prefix
/// in any case).
pub fn apns_key_id(name: &str) -> Option<&str> {
    let head = name.get(..APNS_KEY_PREFIX.len())?;
    let id = &name[APNS_KEY_PREFIX.len()..];
    (head.eq_ignore_ascii_case(APNS_KEY_PREFIX) && crate::notify::apns::valid_id(id)).then_some(id)
}

/// Install an APNs key from Add secret (the value never reaches an error or a log).
async fn install_apns_key(
    st: &AppState,
    who: &Identity,
    key_id: &str,
    value: &str,
) -> Result<(), ApiError> {
    use crate::notify::apns::InstallError;
    let n = st.notify.clone();
    let (id, pem) = (key_id.to_owned(), value.to_owned());
    let r = tokio::task::spawn_blocking(move || n.pusher.apns.install(&id, &pem))
        .await
        .map_err(|_| ApiError::internal("APNs key install failed"))?;
    match r {
        Ok(()) => {
            tracing::info!(device = %who.device_id, key_id = %key_id.to_ascii_uppercase(), "APNs key installed from Add secret");
            Ok(())
        }
        Err(InstallError::BadKey | InstallError::BadKeyId) => {
            Err(bad("value: not an APNs auth key (.p8)"))
        }
        Err(InstallError::NoTeam) => Err(bad(
            "this server has no SECRETARY_APNS_TEAM_ID; APNs keys cannot be installed",
        )),
        Err(InstallError::FixedKey) => Err(bad(
            "this server uses SECRETARY_APNS_KEY_FILE; APNs keys cannot be installed",
        )),
        Err(InstallError::Io(e)) => {
            tracing::error!(error = %e, "could not install the APNs key");
            Err(ApiError::internal("could not install the APNs key"))
        }
    }
}

/// `POST /v1/secrets {name, value | held, project, environment, path?,
/// overwrite?, createFolder?, confirmed: true}`.
async fn put_secret(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
    JsonBody(body): JsonBody<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    confirmed(&body)?;
    let name = match str_field(&body, "name") {
        Some(n) if name_ok(n) => n.to_owned(),
        _ => return Err(bad("name: 1-256 of letters, digits, _ . @ -")),
    };
    let project = need_id(&body, "project")?;
    let environment = need_id(&body, "environment")?;
    let path = normalize_folder(str_field(&body, "path").unwrap_or("/"))
        .ok_or_else(|| bad("path: folders of letters, digits, _ . @ -"))?;
    let overwrite = body.get("overwrite") == Some(&Value::Bool(true));
    // The queue entry this save answers: its card follows; a bad id is simply not passed on.
    let request = str_field(&body, "request").filter(|r| id_ok(r, 64)).map(str::to_owned);
    let create_folder = body.get("createFolder") == Some(&Value::Bool(true));
    let mut act = json!({
        "act": "put_secret",
        "project": project,
        "environment": environment,
        "path": path,
        "key": name,
        "overwrite": overwrite,
        "create_folder": create_folder,
    });
    let (value, held) = match (body.get("value"), str_field(&body, "held")) {
        (Some(Value::String(v)), None) => {
            if v.is_empty() || v.len() > MAX_VALUE_BYTES {
                return Err(bad("value: 1-65536 bytes"));
            }
            act["value_sha256"] = json!(sha_hex(v.as_bytes()));
            (Some(v.clone()), None)
        }
        (None, Some(h)) if id_ok(h, 64) && h.len() >= 8 => {
            act["held"] = json!(h);
            (None, Some(h.to_owned()))
        }
        _ => return Err(bad("give value (a string) or held, not both")),
    };
    let apns_installed = match (apns_key_id(&name), value.as_deref()) {
        (Some(key_id), Some(v)) => {
            install_apns_key(&st, &who, key_id, v).await?;
            true
        }
        _ => false,
    };
    let res = match mint(&st, &who, act) {
        Ok(grant) => forward_grant(&st, grant, value, held, request).await,
        Err(e) => Err(e),
    };
    if !apns_installed {
        return res;
    }
    // The key is installed locally whatever Infisical answered.
    match res {
        Ok((status, Json(mut v))) => {
            if let Some(o) = v.as_object_mut() {
                o.insert("apnsInstalled".into(), json!(true));
            }
            Ok((status, Json(v)))
        }
        Err(e) => Ok((
            e.status,
            Json(json!({"error": {"code": e.code, "message": e.message}, "apnsInstalled": true})),
        )),
    }
}

/// `GET /v1/secrets/requests`: the queue the chat's cards draw (names and fingerprints only).
async fn requests(State(st): State<AppState>) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (status, data) = st.lucina()?.call_with_status::<Value>(&Call::secret_requests()).await?;
    Ok((StatusCode::from_u16(status).unwrap_or(StatusCode::OK), Json(data)))
}

/// `POST /v1/secrets/requests/{id}/dismiss`: "Not now"; the next card follows.
async fn dismiss_request(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (status, data) = st.lucina()?.call_with_status::<Value>(&Call::secret_request_dismiss(&id)?).await?;
    Ok((StatusCode::from_u16(status).unwrap_or(StatusCode::OK), Json(data)))
}

/// `POST /v1/secrets/projects {name, slug?, confirmed: true}`.
async fn create_project(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
    JsonBody(body): JsonBody<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    confirmed(&body)?;
    let name = str_field(&body, "name").unwrap_or("");
    let n = name.chars().count();
    if !(1..=64).contains(&n) || name.trim() != name || name.chars().any(char::is_control) {
        return Err(bad("name: 1-64 characters, no leading or trailing spaces"));
    }
    let mut act = json!({"act": "create_project", "project_name": name});
    if let Some(slug) = str_field(&body, "slug") {
        let ok = id_ok(slug, 64)
            && slug
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !ok {
            return Err(bad("slug: lowercase letters, digits and -"));
        }
        act["slug"] = json!(slug);
    }
    let grant = mint(&st, &who, act)?;
    forward_grant(&st, grant, None, None, None).await
}

/// The device-only routes (mounted behind [`crate::auth::require_device`]).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/secrets", post(put_secret))
        .route("/v1/secrets/status", get(status))
        .route("/v1/secrets/catalog", get(catalog))
        .route("/v1/secrets/names", get(names))
        .route("/v1/secrets/projects", post(create_project))
        .route("/v1/secrets/requests", get(requests))
        .route("/v1/secrets/requests/{id}/dismiss", post(dismiss_request))
}

fn sha_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::apns_key_id;

    #[test]
    fn apns_key_names() {
        assert_eq!(apns_key_id("APNS_AUTH_KEY_AB12CD34EF"), Some("AB12CD34EF"));
        assert_eq!(apns_key_id("APNS_AUTH_KEY_ab12cd34ef"), Some("ab12cd34ef"));
        assert_eq!(apns_key_id("apns_auth_key_AB12CD34EF"), Some("AB12CD34EF"));
        for no in [
            "APNS_AUTH_KEY_SHORT",
            "MY_APNS_AUTH_KEY_AB12CD34EF",
            "APNS_AUTH_KEY_AB12CD34EF1",
            "APNS_AUTH_KEY_AB12CD-4EF",
            "APNS_AUTH_KEY_",
            "APNS",
        ] {
            assert_eq!(apns_key_id(no), None, "{no}");
        }
    }
}
