//! Who is calling: a native client with `Authorization: Bearer <device
//! token>`, or a browser with the `sec_session` cookie.
//!
//! - If an `Authorization` header is present, only it counts (no fallback
//!   to the cookie).
//! - Cookie-authenticated requests with a state-changing method must carry
//!   `X-Secretary-CSRF: 1`, else 403 `csrf`. A cross-site page cannot add
//!   that header without a CORS preflight, and the server sends no CORS
//!   headers. Bearer requests are exempt.
//! - `/v1/session` (login/status/logout) is public; login is rate limited
//!   per client IP.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::devices::{AuthResult, TOKEN_PREFIX};
use crate::error::{ApiError, JsonBody};
use crate::sessions::{clear_cookie, cookie_value, set_cookie};
use crate::AppState;

pub const CSRF_HEADER: &str = "x-secretary-csrf";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    Bearer,
    Cookie,
}

/// The authenticated device (inserted into request and response extensions).
#[derive(Debug, Clone)]
pub struct Identity {
    pub device_id: String,
    pub device_name: String,
    pub via: Via,
}

/// Back-compat alias used by the access log.
#[derive(Debug, Clone)]
pub struct DeviceId(pub String);

fn bearer(headers: &HeaderMap) -> Option<Result<&str, ()>> {
    let v = headers.get(header::AUTHORIZATION)?;
    let parsed = v
        .to_str()
        .ok()
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, t)| t.trim())
        .filter(|t| t.starts_with(TOKEN_PREFIX));
    Some(parsed.ok_or(()))
}

fn registry_error(e: &std::io::Error) -> ApiError {
    tracing::error!(error = %e, "device/session registry unreadable; denying");
    ApiError::internal("device registry unavailable")
}

/// Resolve the caller. `Ok(None)` = not authenticated (with the reason for
/// the 401 message).
pub fn identify(
    st: &AppState,
    headers: &HeaderMap,
) -> Result<Result<Identity, &'static str>, ApiError> {
    if let Some(b) = bearer(headers) {
        let Ok(token) = b else {
            return Ok(Err("missing or malformed device token"));
        };
        return match st
            .devices
            .authenticate(token)
            .map_err(|e| registry_error(&e))?
        {
            AuthResult::Ok { device_id, name } => Ok(Ok(Identity {
                device_id,
                device_name: name,
                via: Via::Bearer,
            })),
            AuthResult::Revoked => Ok(Err("device token revoked")),
            AuthResult::Unknown => Ok(Err("unknown device token")),
        };
    }
    let Some(sid) = cookie_value(headers) else {
        return Ok(Err("missing or malformed device token"));
    };
    let Some(device_id) = st
        .sessions
        .device_for(&sid)
        .map_err(|e| registry_error(&e))?
    else {
        return Ok(Err("session expired or invalid"));
    };
    match st
        .devices
        .active_device(&device_id)
        .map_err(|e| registry_error(&e))?
    {
        Some(d) => Ok(Ok(Identity {
            device_id: d.id,
            device_name: d.name,
            via: Via::Cookie,
        })),
        None => Ok(Err("device token revoked")),
    }
}

fn state_changing(m: &Method) -> bool {
    !matches!(*m, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn csrf_ok(headers: &HeaderMap) -> bool {
    headers
        .get(CSRF_HEADER)
        .is_some_and(|v| v.as_bytes() == b"1")
}

fn csrf_error() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "csrf",
        "cookie-authenticated requests that change state need X-Secretary-CSRF: 1",
    )
}

/// Middleware for every protected `/v1` route: auth (Bearer or cookie),
/// CSRF for cookie writes, per-device rate limit.
pub async fn require_device(
    State(st): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let ident = identify(&st, req.headers())?.map_err(ApiError::unauthorized)?;
    if ident.via == Via::Cookie && state_changing(req.method()) && !csrf_ok(req.headers()) {
        return Err(csrf_error());
    }
    if let Err(wait) = st.limiter.check(&ident.device_id) {
        return Err(ApiError::rate_limited(wait));
    }
    let id = ident.device_id.clone();
    req.extensions_mut().insert(ident);
    let mut resp = next.run(req).await;
    // For the access log (the id is not a secret).
    resp.extensions_mut().insert(DeviceId(id));
    Ok(resp)
}

/// Client IP for the login rate limit. With `SECRETARY_TRUST_PROXY=1` the
/// right-most `X-Forwarded-For` entry (the one our own proxy appended) is
/// used; otherwise the TCP peer.
pub fn client_ip(st: &AppState, req_headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    if st.trust_proxy {
        let xff = req_headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .map(str::trim)
            .rfind(|s| !s.is_empty())
            .and_then(|s| s.parse::<IpAddr>().ok());
        if let Some(ip) = xff {
            return ip.to_string();
        }
    }
    peer.map_or_else(|| "unknown".to_owned(), |p| p.ip().to_string())
}

#[derive(Deserialize)]
struct LoginBody {
    token: String,
}

fn session_json(ident: Option<&Identity>) -> Json<serde_json::Value> {
    Json(json!({
        "authenticated": ident.is_some(),
        "deviceName": ident.map(|i| i.device_name.clone()),
    }))
}

fn with_cookie(mut resp: Response, cookie: &str) -> Response {
    if let Ok(v) = HeaderValue::from_str(cookie) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
    resp
}

/// `POST /v1/session {token}`: exchange a device token for a cookie.
async fn login(State(st): State<AppState>, req: Request) -> Result<Response, ApiError> {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let ip = client_ip(&st, req.headers(), peer);
    if let Err(wait) = st.login_limiter.check(&ip) {
        return Err(ApiError::rate_limited(wait));
    }
    let JsonBody(body) =
        <JsonBody<LoginBody> as axum::extract::FromRequest<AppState>>::from_request(req, &st)
            .await?;
    let token = body.token.trim();
    if !token.starts_with(TOKEN_PREFIX) {
        return Err(ApiError::unauthorized("unknown device token"));
    }
    let (device_id, name) = match st
        .devices
        .authenticate(token)
        .map_err(|e| registry_error(&e))?
    {
        AuthResult::Ok { device_id, name } => (device_id, name),
        AuthResult::Revoked => return Err(ApiError::unauthorized("device token revoked")),
        AuthResult::Unknown => return Err(ApiError::unauthorized("unknown device token")),
    };
    let store = st.sessions.store().clone();
    let did = device_id.clone();
    let sid = tokio::task::spawn_blocking(move || store.create(&did))
        .await
        .map_err(|_| ApiError::internal("session store failed"))?
        .map_err(|e| registry_error(&e))?;
    let ident = Identity {
        device_id,
        device_name: name,
        via: Via::Cookie,
    };
    let mut resp = session_json(Some(&ident)).into_response();
    resp.extensions_mut().insert(DeviceId(ident.device_id));
    Ok(with_cookie(resp, &set_cookie(&sid)))
}

/// `GET /v1/session`: `{authenticated, deviceName}`; never 401.
async fn status(State(st): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    let ident = identify(&st, &headers)?.ok();
    Ok(session_json(ident.as_ref()).into_response())
}

/// `DELETE /v1/session`: log out (cookie sessions); always clears the cookie.
async fn logout(State(st): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    if let Some(sid) = cookie_value(&headers) {
        if !csrf_ok(&headers) {
            return Err(csrf_error());
        }
        let store = st.sessions.store().clone();
        tokio::task::spawn_blocking(move || store.delete(&sid))
            .await
            .map_err(|_| ApiError::internal("session store failed"))?
            .map_err(|e| registry_error(&e))?;
    }
    Ok(with_cookie(
        session_json(None).into_response(),
        &clear_cookie(),
    ))
}

/// The public session routes.
pub fn session_routes() -> Router<AppState> {
    Router::new().route("/v1/session", get(status).post(login).delete(logout))
}
