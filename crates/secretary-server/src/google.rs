//! Google OAuth 2.0 (authorization code + PKCE, offline access) and the
//! Calendar API v3 calls the server needs. Endpoints are configurable so the
//! tests can point them at a mock.

use std::time::Duration;

use reqwest::{Method, StatusCode, Url};
use serde::Deserialize;
use serde_json::Value;

pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
pub const API_BASE: &str = "https://www.googleapis.com";
pub const DEFAULT_REDIRECT: &str = "https://getsecretary.tech/v1/links/google/callback";

pub const SCOPE_CALENDAR_EVENTS: &str = "https://www.googleapis.com/auth/calendar.events";
pub const SCOPE_CALENDAR_READONLY: &str = "https://www.googleapis.com/auth/calendar.readonly";
pub const SCOPE_GMAIL_READONLY: &str = "https://www.googleapis.com/auth/gmail.readonly";
pub const SCOPE_DRIVE_READONLY: &str = "https://www.googleapis.com/auth/drive.readonly";

/// The scopes a service asks for.
pub fn service_scopes(service: &str) -> Option<&'static [&'static str]> {
    match service {
        "calendar" => Some(&[SCOPE_CALENDAR_EVENTS, SCOPE_CALENDAR_READONLY]),
        "gmail" => Some(&[SCOPE_GMAIL_READONLY]),
        "drive" => Some(&[SCOPE_DRIVE_READONLY]),
        _ => None,
    }
}

/// Which services a granted scope list covers.
pub fn services_of(scopes: &[String]) -> Vec<&'static str> {
    ["calendar", "gmail", "drive"]
        .into_iter()
        .filter(|s| {
            service_scopes(s)
                .unwrap_or(&[])
                .iter()
                .all(|need| scopes.iter().any(|g| g == need))
        })
        .collect()
}

#[derive(Clone)]
pub struct GoogleConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub auth_url: String,
    pub token_url: String,
    pub revoke_url: String,
    pub api_base: String,
}

impl std::fmt::Debug for GoogleConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleConfig")
            .field("client_id", &"<set>")
            .field("client_secret", &"<set>")
            .field("redirect_uri", &self.redirect_uri)
            .field("api_base", &self.api_base)
            .finish()
    }
}

impl GoogleConfig {
    pub fn new(client_id: String, client_secret: String) -> Self {
        Self {
            client_id,
            client_secret,
            redirect_uri: DEFAULT_REDIRECT.into(),
            auth_url: AUTH_URL.into(),
            token_url: TOKEN_URL.into(),
            revoke_url: REVOKE_URL.into(),
            api_base: API_BASE.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GError {
    /// No Google account is connected (or Google is not configured).
    NotConnected,
    /// The refresh token was revoked or expired (`invalid_grant`): reconnect.
    Revoked,
    /// `410 Gone`: the sync token expired; do a full sync.
    Gone,
    NotFound,
    Http {
        status: u16,
        message: String,
    },
    Network(String),
}

impl std::fmt::Display for GError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GError::NotConnected => f.write_str("Google is not connected"),
            GError::Revoked => f.write_str("Google access was revoked or expired"),
            GError::Gone => f.write_str("sync token expired"),
            GError::NotFound => f.write_str("not found on Google"),
            GError::Http { status, message } => write!(f, "Google answered {status}: {message}"),
            GError::Network(m) => write!(f, "Google unreachable: {m}"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub scope: Option<String>,
}

pub struct GoogleClient {
    pub cfg: GoogleConfig,
    http: reqwest::Client,
}

fn google_message(v: &Value) -> String {
    let m = v
        .pointer("/error/message")
        .or_else(|| v.get("error_description"))
        .or_else(|| v.get("error"))
        .and_then(Value::as_str)
        .unwrap_or("request failed");
    m.chars().take(200).collect()
}

impl GoogleClient {
    pub fn new(cfg: GoogleConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("secretary-server/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { cfg, http })
    }

    /// The consent URL (`access_type=offline`, `prompt=consent`, PKCE S256).
    pub fn authorize_url(&self, state: &str, challenge: &str, scopes: &[&str]) -> String {
        let scope = scopes.join(" ");
        Url::parse_with_params(
            &self.cfg.auth_url,
            &[
                ("response_type", "code"),
                ("client_id", self.cfg.client_id.as_str()),
                ("redirect_uri", self.cfg.redirect_uri.as_str()),
                ("scope", scope.as_str()),
                ("state", state),
                ("code_challenge", challenge),
                ("code_challenge_method", "S256"),
                ("access_type", "offline"),
                ("prompt", "consent"),
                ("include_granted_scopes", "true"),
            ],
        )
        .map_or_else(|_| self.cfg.auth_url.clone(), |u| u.to_string())
    }

    async fn token_request(&self, form: &[(&str, &str)]) -> Result<TokenResponse, GError> {
        let resp = self
            .http
            .post(&self.cfg.token_url)
            .form(form)
            .send()
            .await
            .map_err(|e| GError::Network(e.without_url().to_string()))?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return serde_json::from_value(body).map_err(|_| GError::Http {
                status: status.as_u16(),
                message: "token response without access_token".into(),
            });
        }
        if body.get("error").and_then(Value::as_str) == Some("invalid_grant") {
            return Err(GError::Revoked);
        }
        Err(GError::Http {
            status: status.as_u16(),
            message: google_message(&body),
        })
    }

    pub async fn exchange(&self, code: &str, verifier: &str) -> Result<TokenResponse, GError> {
        self.token_request(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", verifier),
            ("client_id", &self.cfg.client_id),
            ("client_secret", &self.cfg.client_secret),
            ("redirect_uri", &self.cfg.redirect_uri),
        ])
        .await
    }

    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenResponse, GError> {
        self.token_request(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &self.cfg.client_id),
            ("client_secret", &self.cfg.client_secret),
        ])
        .await
    }

    /// Best effort: tell Google to drop the grant.
    pub async fn revoke(&self, token: &str) -> Result<(), GError> {
        let resp = self
            .http
            .post(&self.cfg.revoke_url)
            .form(&[("token", token)])
            .send()
            .await
            .map_err(|e| GError::Network(e.without_url().to_string()))?;
        if resp.status().is_success() || resp.status() == StatusCode::BAD_REQUEST {
            // 400 = already invalid: the grant is gone either way.
            Ok(())
        } else {
            Err(GError::Http {
                status: resp.status().as_u16(),
                message: "revoke failed".into(),
            })
        }
    }

    /// One Calendar API call. `401` → [`GError::Revoked`] is decided by the
    /// caller (it refreshes once first).
    pub async fn api(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        access_token: &str,
    ) -> Result<Value, (StatusCode, GError)> {
        let url = format!("{}{path}", self.cfg.api_base);
        let mut req = self
            .http
            .request(method, &url)
            .bearer_auth(access_token)
            .query(query);
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await.map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                GError::Network(e.without_url().to_string()),
            )
        })?;
        let status = resp.status();
        let v: Value = if status == StatusCode::NO_CONTENT {
            Value::Null
        } else {
            resp.json().await.unwrap_or(Value::Null)
        };
        if status.is_success() {
            return Ok(v);
        }
        let err = match status {
            StatusCode::GONE => GError::Gone,
            StatusCode::NOT_FOUND => GError::NotFound,
            _ => GError::Http {
                status: status.as_u16(),
                message: google_message(&v),
            },
        };
        Err((status, err))
    }
}

/// `seg` for a path segment (event ids are base32hex, but be safe).
pub fn seg(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_and_scopes() {
        let c = GoogleClient::new(GoogleConfig::new("cid".into(), "S3CRET-VALUE".into())).unwrap();
        let u = c.authorize_url("st", "ch", service_scopes("calendar").unwrap());
        let parsed = Url::parse(&u).unwrap();
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(q["access_type"], "offline");
        assert_eq!(q["prompt"], "consent");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["redirect_uri"], DEFAULT_REDIRECT);
        assert!(q["scope"].contains("calendar.events") && q["scope"].contains("calendar.readonly"));
        assert!(!u.contains("S3CRET"));
        let granted = vec![
            SCOPE_CALENDAR_EVENTS.to_owned(),
            SCOPE_CALENDAR_READONLY.to_owned(),
        ];
        assert_eq!(services_of(&granted), vec!["calendar"]);
        assert_eq!(seg("a b/c"), "a%20b%2Fc");
        assert!(!format!("{:?}", c.cfg).contains("S3CRET"));
    }
}
