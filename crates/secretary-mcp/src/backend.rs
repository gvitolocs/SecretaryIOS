//! Client for secretary-server's `/v1` API (docs/server-api.md).
//!
//! Every call uses the MCP service's own device token
//! (`SECRETARY_MCP_BACKEND_TOKEN_FILE`), except `GET /v1/insights/summary`
//! (the service token) and the owner password check of the OAuth login
//! (`POST /v1/session`, no token). Bodies and secrets are never logged.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE, COOKIE, RETRY_AFTER, SET_COOKIE};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};

/// Chat is an LLM turn: secretary-server waits up to 120 s.
pub const CHAT_TIMEOUT: Duration = Duration::from_secs(125);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

/// A failed backend call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendError {
    /// HTTP status from secretary-server; 0 when no response arrived.
    pub status: u16,
    /// secretary-server's `error.code` (or the engine `code`), or one of
    /// `backend_unreachable`, `backend_timeout`, `backend_not_configured`,
    /// `backend_bad_response`.
    pub code: String,
    pub message: String,
    /// `Retry-After` seconds on a 429.
    pub retry_after: Option<u64>,
}

impl BackendError {
    fn local(code: &str, message: impl Into<String>) -> Self {
        Self {
            status: 0,
            code: code.into(),
            message: message.into(),
            retry_after: None,
        }
    }

    /// v2 shopping missing on this deployment (route or Lucina side), as
    /// opposed to a real `list_not_found` / `item_not_found`.
    pub fn is_v2_unavailable(&self) -> bool {
        self.status == 502
            || self.status == 404
                && !matches!(self.code.as_str(), "list_not_found" | "item_not_found")
    }
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.status == 0 {
            write!(f, "{}: {}", self.code, self.message)
        } else {
            write!(f, "{} (HTTP {}): {}", self.code, self.status, self.message)
        }
    }
}

pub type BResult<T> = Result<T, BackendError>;

/// Which credential a call carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cred {
    Device,
    Service,
    None,
}

/// Result of the owner password check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerCheck {
    Ok,
    Invalid,
    /// Locked out or rate limited; retry after this many seconds.
    Locked(u64),
}

#[derive(Clone)]
pub struct Backend {
    http: reqwest::Client,
    base: String,
    device_token: Option<String>,
    service_token: Option<String>,
}

impl Backend {
    pub fn new(base: &str, device_token: Option<String>, service_token: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("secretary-mcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client");
        Self {
            http,
            base: base.trim_end_matches('/').to_owned(),
            device_token,
            service_token,
        }
    }

    pub fn has_device_token(&self) -> bool {
        self.device_token.is_some()
    }

    pub fn has_service_token(&self) -> bool {
        self.service_token.is_some()
    }

    #[allow(clippy::too_many_arguments)]
    async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        cred: Cred,
        timeout: Duration,
        extra: HeaderMap,
    ) -> BResult<(StatusCode, HeaderMap, Value)> {
        let mut req = self
            .http
            .request(method, format!("{}{path}", self.base))
            .timeout(timeout)
            .headers(extra);
        if !query.is_empty() {
            req = req.query(query);
        }
        let token = match cred {
            Cred::Device => Some(self.device_token.as_deref().ok_or_else(|| {
                BackendError::local(
                    "backend_not_configured",
                    "SECRETARY_MCP_BACKEND_TOKEN_FILE is not set",
                )
            })?),
            Cred::Service => Some(self.service_token.as_deref().ok_or_else(|| {
                BackendError::local(
                    "backend_not_configured",
                    "SECRETARY_MCP_SERVICE_TOKEN_FILE is not set",
                )
            })?),
            Cred::None => None,
        };
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                BackendError::local("backend_timeout", "secretary-server did not answer in time")
            } else {
                BackendError::local("backend_unreachable", e.without_url().to_string())
            }
        })?;
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.bytes().await.map_err(|e| {
            if e.is_timeout() {
                BackendError::local("backend_timeout", "secretary-server did not answer in time")
            } else {
                BackendError::local("backend_unreachable", e.without_url().to_string())
            }
        })?;
        let value: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        if status.is_success() {
            return Ok((status, headers, value));
        }
        let mut err = error_from(status, &value);
        err.retry_after = headers
            .get(RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok());
        Err(err)
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> BResult<Value> {
        self.send(
            method,
            path,
            query,
            body,
            Cred::Device,
            DEFAULT_TIMEOUT,
            HeaderMap::new(),
        )
        .await
        .map(|(_, _, v)| v)
    }

    /// Library reads accept either the configured device or Lucina service token.
    pub async fn library_get(&self, path: &str, query: &[(&str, String)]) -> BResult<Value> {
        self.send(
            Method::GET,
            path,
            query,
            None,
            if self.has_device_token() {
                Cred::Device
            } else {
                Cred::Service
            },
            DEFAULT_TIMEOUT,
            HeaderMap::new(),
        )
        .await
        .map(|(_, _, v)| v)
    }

    /// Generate an extractive brief; the caller's MCP write scope is checked first.
    pub async fn library_brief(&self, id: &str) -> BResult<Value> {
        self.send(
            Method::POST,
            &format!("/v1/library/books/{}/brief", seg(id)),
            &[],
            Some(&json!({})),
            if self.has_device_token() {
                Cred::Device
            } else {
                Cred::Service
            },
            DEFAULT_TIMEOUT,
            HeaderMap::new(),
        )
        .await
        .map(|(_, _, v)| v)
    }

    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> BResult<Value> {
        self.call(Method::GET, path, query, None).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> BResult<Value> {
        self.call(Method::POST, path, &[], Some(body)).await
    }

    pub async fn patch(&self, path: &str, body: &Value) -> BResult<Value> {
        self.call(Method::PATCH, path, &[], Some(body)).await
    }

    pub async fn delete(&self, path: &str) -> BResult<Value> {
        self.call(Method::DELETE, path, &[], None).await
    }

    /// `POST /v1/chat` as the web channel.
    // The turn enters as channel `web`. Lucina recognises the `mcp-<caller>`
    // session id and `[via Lucina MCP, from …]` prefix, tagging both timeline
    // items with meta.via = "mcp" and meta.caller; clients label them by caller.
    pub async fn chat(&self, body: &Value) -> BResult<Value> {
        let mut h = HeaderMap::new();
        h.insert("x-secretary-client", HeaderValue::from_static("web"));
        self.send(
            Method::POST,
            "/v1/chat",
            &[],
            Some(body),
            Cred::Device,
            CHAT_TIMEOUT,
            h,
        )
        .await
        .map(|(_, _, v)| v)
    }

    /// `GET /v1/insights/summary` with the service token.
    pub async fn insights(&self, timezone: &str, workouts: u32) -> BResult<Value> {
        let q = [
            ("timezone", timezone.to_owned()),
            ("workouts", workouts.to_string()),
        ];
        self.send(
            Method::GET,
            "/v1/insights/summary",
            &q,
            None,
            Cred::Service,
            DEFAULT_TIMEOUT,
            HeaderMap::new(),
        )
        .await
        .map(|(_, _, v)| v)
    }

    /// `GET /v1/health` (no auth).
    pub async fn health(&self) -> BResult<Value> {
        self.send(
            Method::GET,
            "/v1/health",
            &[],
            None,
            Cred::None,
            Duration::from_secs(5),
            HeaderMap::new(),
        )
        .await
        .map(|(_, _, v)| v)
    }

    /// `GET /v1/session` with the device token: is it still accepted?
    pub async fn token_accepted(&self) -> BResult<bool> {
        let v = self
            .send(
                Method::GET,
                "/v1/session",
                &[],
                None,
                Cred::Device,
                Duration::from_secs(5),
                HeaderMap::new(),
            )
            .await?
            .2;
        Ok(v.get("authenticated")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    /// Check the owner's email + password with secretary-server's own login
    /// (`POST /v1/session`): its Argon2 store, lockout and per-IP limit stay
    /// the single source of truth. The browser session it creates is ended
    /// at once (`DELETE /v1/session`). `client_ip` goes in
    /// `X-Forwarded-For` so the server's per-IP limit sees the real client
    /// (secretary-server runs with `SECRETARY_TRUST_PROXY=1`).
    pub async fn check_owner(
        &self,
        email: &str,
        password: &str,
        client_ip: &str,
    ) -> BResult<OwnerCheck> {
        let mut h = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(client_ip) {
            h.insert("x-forwarded-for", v);
        }
        let body = json!({"email": email, "password": password});
        match self
            .send(
                Method::POST,
                "/v1/session",
                &[],
                Some(&body),
                Cred::None,
                DEFAULT_TIMEOUT,
                h.clone(),
            )
            .await
        {
            Ok((_, headers, _)) => {
                if let Some(cookie) = session_cookie(&headers) {
                    let mut del = h;
                    if let Ok(v) = HeaderValue::from_str(&cookie) {
                        del.insert(COOKIE, v);
                    }
                    del.insert("x-secretary-csrf", HeaderValue::from_static("1"));
                    del.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                    if let Err(e) = self
                        .send(
                            Method::DELETE,
                            "/v1/session",
                            &[],
                            None,
                            Cred::None,
                            DEFAULT_TIMEOUT,
                            del,
                        )
                        .await
                    {
                        tracing::warn!(code = %e.code, "could not end the login check session");
                    }
                }
                Ok(OwnerCheck::Ok)
            }
            Err(e) if e.status == 401 || e.status == 400 => Ok(OwnerCheck::Invalid),
            Err(e) if e.status == 429 => Ok(OwnerCheck::Locked(e.retry_after.unwrap_or(60))),
            Err(e) => Err(e),
        }
    }
}

/// `sec_session=<v>` from a `Set-Cookie`, ready for a `Cookie` header.
fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers.get_all(SET_COOKIE).iter().find_map(|v| {
        let s = v.to_str().ok()?;
        let pair = s.split(';').next()?.trim();
        pair.starts_with("sec_session=").then(|| pair.to_owned())
    })
}

fn error_from(status: StatusCode, v: &Value) -> BackendError {
    // secretary-server: {"error": {"code", "message"}}; engines: {ok:false, code, error}.
    let (code, message) = match v.get("error") {
        Some(Value::Object(o)) => (
            o.get("code").and_then(Value::as_str).map(str::to_owned),
            o.get("message").and_then(Value::as_str).map(str::to_owned),
        ),
        Some(Value::String(msg)) => (
            v.get("code").and_then(Value::as_str).map(str::to_owned),
            Some(msg.clone()),
        ),
        _ => (None, None),
    };
    BackendError {
        status: status.as_u16(),
        code: code.unwrap_or_else(|| format!("http_{}", status.as_u16())),
        message: message.unwrap_or_else(|| status.canonical_reason().unwrap_or("error").to_owned()),
        retry_after: None,
    }
}

/// Percent-encode one path segment (ids are validated first; this is a
/// second line of defence).
pub fn seg(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b':') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_shapes() {
        let e = error_from(
            StatusCode::NOT_FOUND,
            &json!({"error": {"code": "list_not_found", "message": "no list"}}),
        );
        assert_eq!((e.status, e.code.as_str()), (404, "list_not_found"));
        assert!(!e.is_v2_unavailable());
        let e = error_from(
            StatusCode::NOT_FOUND,
            &json!({"error": {"code": "not_found"}}),
        );
        assert!(e.is_v2_unavailable());
        let e = error_from(StatusCode::BAD_GATEWAY, &Value::Null);
        assert_eq!(e.code, "http_502");
        assert!(e.is_v2_unavailable());
        let e = error_from(
            StatusCode::CONFLICT,
            &json!({"ok": false, "code": "conflict", "error": "State changed."}),
        );
        assert_eq!(
            (e.code.as_str(), e.message.as_str()),
            ("conflict", "State changed.")
        );
    }

    #[test]
    fn segments_and_cookies() {
        assert_eq!(seg("tl_01:a.b"), "tl_01:a.b");
        assert_eq!(seg("a/b c"), "a%2Fb%20c");
        let mut h = HeaderMap::new();
        h.append(SET_COOKIE, HeaderValue::from_static("other=1; Path=/"));
        h.append(
            SET_COOKIE,
            HeaderValue::from_static("sec_session=abc; HttpOnly; Secure; SameSite=Strict"),
        );
        assert_eq!(session_cookie(&h).as_deref(), Some("sec_session=abc"));
    }
}
