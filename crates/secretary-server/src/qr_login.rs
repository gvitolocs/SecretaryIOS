//! QR sign-in, like Discord's: a signed-out browser asks for a request,
//! shows its approve link as a QR code and polls; a device that is already
//! signed in opens the link, sees where the request came from and approves;
//! the browser's next poll gets the session cookie. No password is typed.
//!
//! - `code` (in the QR, so anyone who sees the screen has it) only names the
//!   request; approving it needs an authenticated device (`require_device`,
//!   so cookie approvals need CSRF). `pollToken` stays with the requesting
//!   page and is the only thing that can collect the session. Both are 128
//!   random bits, base64url.
//! - In memory (one server instance), [`TTL`] long, one-time: the poll that
//!   sees `approved` or `denied` removes the request. A restart drops them.
//! - Creating a request costs one login attempt of the client IP
//!   (`login_ip_check`); image and poll requests have their own per-IP
//!   bucket ([`POLLS_PER_MINUTE`]), so a page polling every 2 s does not use
//!   up the password logins.
//!
//! | route | auth | |
//! |---|---|---|
//! | `POST /v1/session/qr` | public | `{code, pollToken, approveUrl, expiresIn}` |
//! | `GET /v1/session/qr/image?c=` | public | the QR of `approveUrl`, SVG |
//! | `GET /v1/session/qr/poll?t=` | public | `{status: pending\|approved\|denied\|expired}`; `approved` sets the cookie |
//! | `GET /v1/session/qr/info?c=` | device | `{ip, userAgent, createdAt, expiresIn}` for the approve screen |
//! | `POST /v1/session/qr/approve {code, approve}` | device | `{status: approved\|denied}` |

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::json;
use subtle::ConstantTimeEq;

use crate::auth::{
    client_ip, login_ip_check, session_cookie, web_login_identity, with_cookie, DeviceId, Identity,
};
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::AppState;

/// How long a request waits for a phone.
pub const TTL: Duration = Duration::from_secs(120);
/// Requests alive at once (all callers); more get 503 until some expire.
pub const MAX_PENDING: usize = 256;
/// Image and poll requests per client IP per minute (a page polls every 2 s).
pub const POLLS_PER_MINUTE: u32 = 90;
/// Longest User-Agent kept for the approve screen.
const MAX_USER_AGENT: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Pending,
    Approved,
    Denied,
}

struct Pending {
    poll_token: String,
    ip: String,
    user_agent: String,
    created_at: String,
    born: Instant,
    decision: Decision,
}

/// What the requesting page learns from a poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Poll {
    Pending,
    Approved,
    Denied,
    /// Expired, already collected, or never existed.
    Expired,
}

impl Poll {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Expired => "expired",
        }
    }
}

/// The requester, for the approve screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub ip: String,
    pub user_agent: String,
    pub created_at: String,
    pub expires_in: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecideError {
    /// Expired, collected, or never existed.
    NotFound,
    AlreadyDecided,
}

/// The pending requests, by `code`.
pub struct QrLogins {
    ttl: Duration,
    approve_base: String,
    map: Mutex<HashMap<String, Pending>>,
}

impl QrLogins {
    pub fn new(ttl: Duration, public_url: &str) -> Self {
        Self {
            ttl,
            approve_base: format!("{}/approve?c=", public_url.trim_end_matches('/')),
            map: Mutex::new(HashMap::new()),
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// The link the QR code carries.
    pub fn approve_url(&self, code: &str) -> String {
        format!("{}{code}", self.approve_base)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
        self.map.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn alive(&self, p: &Pending, now: Instant) -> bool {
        now.saturating_duration_since(p.born) < self.ttl
    }

    /// A new request: `Ok(Some((code, poll_token)))`, `Ok(None)` when
    /// [`MAX_PENDING`] are already waiting.
    pub fn create(&self, ip: &str, user_agent: &str) -> io::Result<Option<(String, String)>> {
        self.create_at(ip, user_agent, Instant::now())
    }

    fn create_at(
        &self,
        ip: &str,
        user_agent: &str,
        now: Instant,
    ) -> io::Result<Option<(String, String)>> {
        let code = crate::devices::random_b64::<16>()?;
        let poll_token = crate::devices::random_b64::<16>()?;
        let mut map = self.lock();
        map.retain(|_, p| self.alive(p, now));
        if map.len() >= MAX_PENDING {
            return Ok(None);
        }
        let user_agent: String = user_agent.chars().take(MAX_USER_AGENT).collect();
        map.insert(
            code.clone(),
            Pending {
                poll_token: poll_token.clone(),
                ip: ip.to_owned(),
                user_agent,
                created_at: crate::devices::now_rfc3339(),
                born: now,
                decision: Decision::Pending,
            },
        );
        Ok(Some((code, poll_token)))
    }

    /// The requester of a live, undecided request.
    pub fn info(&self, code: &str) -> Option<Info> {
        self.info_at(code, Instant::now())
    }

    fn info_at(&self, code: &str, now: Instant) -> Option<Info> {
        let map = self.lock();
        let p = map
            .get(code)
            .filter(|p| p.decision == Decision::Pending && self.alive(p, now))?;
        Some(Info {
            ip: p.ip.clone(),
            user_agent: p.user_agent.clone(),
            created_at: p.created_at.clone(),
            expires_in: self
                .ttl
                .saturating_sub(now.saturating_duration_since(p.born))
                .as_secs(),
        })
    }

    /// Approve or deny a live request (once).
    pub fn decide(&self, code: &str, approve: bool) -> Result<(), DecideError> {
        self.decide_at(code, approve, Instant::now())
    }

    fn decide_at(&self, code: &str, approve: bool, now: Instant) -> Result<(), DecideError> {
        let mut map = self.lock();
        let alive = map.get(code).is_some_and(|p| self.alive(p, now));
        let p = map
            .get_mut(code)
            .filter(|_| alive)
            .ok_or(DecideError::NotFound)?;
        if p.decision != Decision::Pending {
            return Err(DecideError::AlreadyDecided);
        }
        p.decision = if approve {
            Decision::Approved
        } else {
            Decision::Denied
        };
        Ok(())
    }

    /// The requesting page's poll. `Approved` and `Denied` are reported
    /// once: the request is removed with them.
    pub fn poll(&self, poll_token: &str) -> Poll {
        self.poll_at(poll_token, Instant::now())
    }

    fn poll_at(&self, poll_token: &str, now: Instant) -> Poll {
        let mut map = self.lock();
        let found = map
            .iter()
            .find(|(_, p)| bool::from(p.poll_token.as_bytes().ct_eq(poll_token.as_bytes())))
            .map(|(code, p)| (code.clone(), self.alive(p, now), p.decision));
        let Some((code, alive, decision)) = found else {
            return Poll::Expired;
        };
        if !alive {
            map.remove(&code);
            return Poll::Expired;
        }
        match decision {
            Decision::Pending => Poll::Pending,
            Decision::Approved => {
                map.remove(&code);
                Poll::Approved
            }
            Decision::Denied => {
                map.remove(&code);
                Poll::Denied
            }
        }
    }
}

fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "qr_not_found",
        "this sign-in code expired or was already used",
    )
}

fn peer_ip(st: &AppState, req: &Request) -> String {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    client_ip(st, req.headers(), peer)
}

fn poll_ip_check(st: &AppState, req: &Request) -> Result<(), ApiError> {
    st.qr_limiter
        .check(&peer_ip(st, req))
        .map_err(ApiError::rate_limited)
}

fn query<T: DeserializeOwned>(req: &Request) -> Result<T, ApiError> {
    Query::<T>::try_from_uri(req.uri())
        .map(|Query(v)| v)
        .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, "bad_request", e.body_text()))
}

#[derive(Deserialize)]
struct CodeQuery {
    c: String,
}

#[derive(Deserialize)]
struct TokenQuery {
    t: String,
}

#[derive(Deserialize)]
struct ApproveBody {
    code: String,
    approve: bool,
}

/// `POST /v1/session/qr`
async fn create(State(st): State<AppState>, req: Request) -> Result<Response, ApiError> {
    login_ip_check(&st, &req)?;
    let ip = peer_ip(&st, &req);
    let user_agent = req
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let created = st
        .qr_logins
        .create(&ip, &user_agent)
        .map_err(|_| ApiError::internal("random source failed"))?;
    let Some((code, poll_token)) = created else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "qr_busy",
            "too many sign-in codes are waiting; try again in a minute",
        ));
    };
    Ok(Json(json!({
        "approveUrl": st.qr_logins.approve_url(&code),
        "code": code,
        "pollToken": poll_token,
        "expiresIn": st.qr_logins.ttl().as_secs(),
    }))
    .into_response())
}

/// `GET /v1/session/qr/image?c=`: only for a live request.
async fn image(State(st): State<AppState>, req: Request) -> Result<Response, ApiError> {
    poll_ip_check(&st, &req)?;
    let CodeQuery { c } = query(&req)?;
    if st.qr_logins.info(&c).is_none() {
        return Err(not_found());
    }
    let svg = render_svg(&st.qr_logins.approve_url(&c))?;
    Ok((
        [
            (header::CONTENT_TYPE, "image/svg+xml"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        svg,
    )
        .into_response())
}

/// The QR code of `text` as SVG (black on white, with the quiet zone).
pub fn render_svg(text: &str) -> Result<String, ApiError> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M)
        .map_err(|_| ApiError::internal("QR code too long"))?;
    Ok(code
        .render::<svg::Color<'_>>()
        .min_dimensions(240, 240)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build())
}

/// `GET /v1/session/qr/poll?t=`
async fn poll(State(st): State<AppState>, req: Request) -> Result<Response, ApiError> {
    poll_ip_check(&st, &req)?;
    let TokenQuery { t } = query(&req)?;
    let outcome = st.qr_logins.poll(&t);
    if outcome != Poll::Approved {
        return Ok(Json(json!({"status": outcome.as_str()})).into_response());
    }
    let ident = web_login_identity(&st).await?;
    let cookie = session_cookie(&st, &ident).await?;
    tracing::info!(device = %ident.device_id, "QR sign-in collected");
    let mut resp = Json(json!({
        "status": "approved",
        "authenticated": true,
        "deviceId": ident.device_id,
        "deviceName": ident.device_name,
    }))
    .into_response();
    resp.extensions_mut().insert(DeviceId(ident.device_id));
    Ok(with_cookie(resp, &cookie))
}

/// `GET /v1/session/qr/info?c=` (authenticated)
async fn info(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<CodeQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let info = st.qr_logins.info(&q.c).ok_or_else(not_found)?;
    Ok(Json(json!({
        "ip": info.ip,
        "userAgent": info.user_agent,
        "createdAt": info.created_at,
        "expiresIn": info.expires_in,
    })))
}

/// `POST /v1/session/qr/approve {code, approve}` (authenticated)
async fn approve(
    State(st): State<AppState>,
    axum::Extension(ident): axum::Extension<Identity>,
    JsonBody(body): JsonBody<ApproveBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    match st.qr_logins.decide(&body.code, body.approve) {
        Ok(()) => {}
        Err(DecideError::NotFound) => return Err(not_found()),
        Err(DecideError::AlreadyDecided) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "qr_already_decided",
                "this sign-in request was already answered",
            ))
        }
    }
    tracing::info!(device = %ident.device_id, approve = body.approve, "QR sign-in answered");
    let status = if body.approve { "approved" } else { "denied" };
    Ok(Json(json!({"status": status})))
}

/// The public QR routes (next to `/v1/session`).
pub fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/v1/session/qr", post(create))
        .route("/v1/session/qr/image", get(image))
        .route("/v1/session/qr/poll", get(poll))
}

/// The approving device's routes (behind `require_device`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/session/qr/info", get(info))
        .route("/v1/session/qr/approve", post(approve))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> QrLogins {
        QrLogins::new(TTL, "https://example.test/")
    }

    #[test]
    fn approve_url_uses_the_public_origin() {
        assert_eq!(
            store().approve_url("abc"),
            "https://example.test/approve?c=abc"
        );
    }

    #[test]
    fn codes_and_poll_tokens_are_distinct_128_bit_values() {
        let s = store();
        let (code, token) = s.create("1.2.3.4", "ua").unwrap().unwrap();
        assert_ne!(code, token);
        // 16 bytes = 22 base64url characters, no padding.
        assert_eq!(code.len(), 22);
        assert_eq!(token.len(), 22);
        // The code is not a poll token, and the token is not a code.
        assert_eq!(s.poll(&code), Poll::Expired);
        assert!(s.info(&token).is_none());
    }

    #[test]
    fn approval_is_collected_once() {
        let s = store();
        let (code, token) = s.create("1.2.3.4", "ua").unwrap().unwrap();
        assert_eq!(s.poll(&token), Poll::Pending);
        s.decide(&code, true).unwrap();
        assert_eq!(s.decide(&code, false), Err(DecideError::AlreadyDecided));
        assert!(s.info(&code).is_none(), "no info once answered");
        assert_eq!(s.poll(&token), Poll::Approved);
        assert_eq!(s.poll(&token), Poll::Expired);
        assert_eq!(s.decide(&code, true), Err(DecideError::NotFound));
    }

    #[test]
    fn requests_expire() {
        let s = store();
        let t0 = Instant::now();
        let (code, token) = s.create_at("1.2.3.4", "ua", t0).unwrap().unwrap();
        let info = s.info_at(&code, t0 + Duration::from_secs(20)).unwrap();
        assert_eq!(info.expires_in, 100);
        let late = t0 + TTL;
        assert!(s.info_at(&code, late).is_none());
        assert_eq!(s.decide_at(&code, true, late), Err(DecideError::NotFound));
        assert_eq!(s.poll_at(&token, late), Poll::Expired);
        // An approval that is not collected in time expires too.
        let (code, token) = s.create_at("1.2.3.4", "ua", t0).unwrap().unwrap();
        s.decide_at(&code, true, t0).unwrap();
        assert_eq!(s.poll_at(&token, late), Poll::Expired);
    }

    #[test]
    fn pending_requests_are_capped_and_swept() {
        let s = store();
        let t0 = Instant::now();
        for _ in 0..MAX_PENDING {
            assert!(s.create_at("ip", "ua", t0).unwrap().is_some());
        }
        assert!(s.create_at("ip", "ua", t0).unwrap().is_none());
        assert!(s.create_at("ip", "ua", t0 + TTL).unwrap().is_some());
        assert_eq!(s.lock().len(), 1);
    }

    #[test]
    fn long_user_agents_are_cut() {
        let s = store();
        let (code, _) = s.create("ip", &"x".repeat(5000)).unwrap().unwrap();
        assert_eq!(s.info(&code).unwrap().user_agent.len(), MAX_USER_AGENT);
    }

    #[test]
    fn renders_an_svg() {
        let svg = render_svg("https://example.test/approve?c=abc").unwrap();
        assert!(svg.contains("<svg"), "{svg}");
    }
}
