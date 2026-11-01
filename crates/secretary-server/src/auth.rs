//! Who is calling: a native client with `Authorization: Bearer <device
//! token>`, or a browser with the `sec_session` cookie.
//!
//! - If an `Authorization` header is present, only it counts (no fallback
//!   to the cookie).
//! - Cookie-authenticated requests with a state-changing method must carry
//!   `X-Secretary-CSRF: 1`, else 403 `csrf`. A cross-site page cannot add
//!   that header without a CORS preflight, and the server sends no CORS
//!   headers. Bearer requests are exempt.
//! - `/v1/session` (login/status/logout) and `/v1/auth/device` are public;
//!   logins are rate limited per client IP. QR sign-in (a signed-in phone
//!   approves a new browser) is in [`crate::qr_login`]. Login is a device token
//!   (`{token}`) or the single owner account (`{email, password}`, see
//!   [`crate::owner`]; password logins also lock out after repeated failures).

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;

use crate::devices::{AuthResult, TOKEN_PREFIX};
use crate::error::{ApiError, JsonBody};
use crate::owner::Verdict;
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
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    authorize(&st, &st.limiter, req, next).await
}

/// The device id the service token (Lucina) gets on routes that accept it.
pub const SERVICE_DEVICE_ID: &str = "svc";

/// [`require_device`], plus Lucina's service token (`svc_…`) for the
/// calendar routes. The service caller is `Identity { device_id: "svc",
/// device_name: "lucina" }` and shares the service rate-limit bucket.
pub async fn require_device_or_service(
    State(st): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let svc = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, t)| t.trim().to_owned())
        .filter(|t| t.starts_with(crate::service::SERVICE_PREFIX));
    let Some(token) = svc else {
        return authorize(&st, &st.limiter, req, next).await;
    };
    match &st.service_token {
        Some(expected) if expected.matches(&token) => {}
        Some(_) => return Err(ApiError::unauthorized("unknown service token")),
        None => {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_token_not_configured",
                "SECRETARY_SERVICE_TOKEN_FILE is not set on the server",
            ))
        }
    }
    if let Err(wait) = st.limiter.check(crate::service::RATE_KEY) {
        return Err(ApiError::rate_limited(wait));
    }
    req.extensions_mut().insert(Identity {
        device_id: SERVICE_DEVICE_ID.into(),
        device_name: "lucina".into(),
        via: Via::Bearer,
    });
    let mut resp = next.run(req).await;
    resp.extensions_mut()
        .insert(DeviceId(SERVICE_DEVICE_ID.into()));
    Ok(resp)
}

/// [`require_device`] with the device-events bucket
/// (`SECRETARY_DEVICE_EVENTS_PER_MINUTE`) instead of the general one, so a
/// phone flushing its offline queue does not starve the app's own requests.
pub async fn require_device_events(
    State(st): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    authorize(&st, &st.device_limiter, req, next).await
}

async fn authorize(
    st: &AppState,
    limiter: &crate::ratelimit::RateLimiter,
    mut req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let ident = identify(st, req.headers())?.map_err(ApiError::unauthorized)?;
    if ident.via == Via::Cookie && state_changing(req.method()) && !csrf_ok(req.headers()) {
        return Err(csrf_error());
    }
    if let Err(wait) = limiter.check(&ident.device_id) {
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

/// `{token}` (device token) or `{email, password}` (the owner account).
#[derive(Deserialize)]
struct LoginBody {
    token: Option<String>,
    email: Option<String>,
    password: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceLoginBody {
    email: String,
    password: String,
    device_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasswordChangeBody {
    current_password: String,
    new_password: String,
}

pub fn invalid_credentials() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
        "wrong email or password",
    )
}

fn account_locked(wait: std::time::Duration) -> ApiError {
    let mut e = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "account_locked",
        "too many failed password attempts; try again later",
    );
    e.retry_after = Some(wait.as_secs().max(1));
    e
}

/// Check the owner's email + password off the async runtime.
/// `Ok(())`, 401 `invalid_credentials` or 429 `account_locked`.
pub(crate) async fn check_password(
    st: &AppState,
    email: String,
    password: String,
) -> Result<(), ApiError> {
    let store = st.owner.clone();
    let verdict = tokio::task::spawn_blocking(move || store.verify(&email, &password))
        .await
        .map_err(|_| ApiError::internal("password check failed"))?
        .map_err(|e| registry_error(&e))?;
    match verdict {
        Verdict::Ok => Ok(()),
        Verdict::Invalid => Err(invalid_credentials()),
        Verdict::Locked(wait) => Err(account_locked(wait)),
    }
}

pub(crate) fn login_ip_check(st: &AppState, req: &Request) -> Result<(), ApiError> {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let ip = client_ip(st, req.headers(), peer);
    st.login_limiter.check(&ip).map_err(ApiError::rate_limited)
}

/// Start a browser session for `ident`; returns its `Set-Cookie` value.
pub(crate) async fn session_cookie(st: &AppState, ident: &Identity) -> Result<String, ApiError> {
    let store = st.sessions.store().clone();
    let did = ident.device_id.clone();
    let sid = tokio::task::spawn_blocking(move || store.create(&did))
        .await
        .map_err(|_| ApiError::internal("session store failed"))?
        .map_err(|e| registry_error(&e))?;
    Ok(set_cookie(&sid))
}

async fn new_session(st: &AppState, ident: Identity) -> Result<Response, ApiError> {
    let cookie = session_cookie(st, &ident).await?;
    let mut resp = session_json(Some(&ident)).into_response();
    resp.extensions_mut().insert(DeviceId(ident.device_id));
    Ok(with_cookie(resp, &cookie))
}

/// The owner's browser device (`web_login`, created on first use): the
/// device behind password and QR sign-ins.
pub(crate) async fn web_login_identity(st: &AppState) -> Result<Identity, ApiError> {
    let devices = st.devices.store().clone();
    let dev = tokio::task::spawn_blocking(move || devices.ensure_web_login())
        .await
        .map_err(|_| ApiError::internal("device store failed"))?
        .map_err(|e| registry_error(&e))?;
    Ok(Identity {
        device_id: dev.id,
        device_name: dev.name,
        via: Via::Cookie,
    })
}

fn session_json(ident: Option<&Identity>) -> Json<serde_json::Value> {
    Json(json!({
        "authenticated": ident.is_some(),
        "deviceId": ident.map(|i| i.device_id.clone()),
        "deviceName": ident.map(|i| i.device_name.clone()),
    }))
}

pub(crate) fn with_cookie(mut resp: Response, cookie: &str) -> Response {
    if let Ok(v) = HeaderValue::from_str(cookie) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
    resp
}

/// `POST /v1/session {token}` or `{email, password}`: log in a browser
/// (sets the session cookie).
async fn login(State(st): State<AppState>, req: Request) -> Result<Response, ApiError> {
    login_ip_check(&st, &req)?;
    let JsonBody(body) =
        <JsonBody<LoginBody> as axum::extract::FromRequest<AppState>>::from_request(req, &st)
            .await?;
    let token = match body {
        LoginBody {
            token: Some(token), ..
        } => token,
        LoginBody {
            email: Some(email),
            password: Some(password),
            ..
        } => {
            check_password(&st, email, password).await?;
            let ident = web_login_identity(&st).await?;
            return new_session(&st, ident).await;
        }
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "send {token} or {email, password}",
            ))
        }
    };
    let token = token.trim();
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
    let ident = Identity {
        device_id,
        device_name: name,
        via: Via::Cookie,
    };
    new_session(&st, ident).await
}

/// Register a new device and return it with its token (shown once). A bad
/// name is a 400 (`deviceName: …`). Also raises the `system` notification
/// (off by default) that a new device signed in. Used by `POST
/// /v1/auth/device` and by the QR sign-in of a headless device.
pub(crate) async fn mint_device(
    st: &AppState,
    name: String,
) -> Result<(crate::devices::Device, String), ApiError> {
    let devices = st.devices.store().clone();
    let issued = tokio::task::spawn_blocking(move || devices.issue(&name))
        .await
        .map_err(|_| ApiError::internal("device store failed"))?;
    let (dev, token) = match issued {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::InvalidInput => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                format!("deviceName: {e}"),
            ))
        }
        Err(e) => return Err(registry_error(&e)),
    };
    let notifier = st.notify.clone();
    let lang = notifier.settings.read(|s| s.language.clone());
    let notice = crate::notify::NewNotification::new(
        crate::notify::Category::System,
        crate::notify::text::new_device(&dev.name, &lang),
    );
    tokio::spawn(async move {
        let _ = notifier.publish(notice).await;
    });
    Ok((dev, token))
}

/// `POST /v1/auth/device {email, password, deviceName}`: native apps trade
/// the owner's password for a new device token (shown once).
async fn device_login(State(st): State<AppState>, req: Request) -> Result<Response, ApiError> {
    login_ip_check(&st, &req)?;
    let JsonBody(body) =
        <JsonBody<DeviceLoginBody> as axum::extract::FromRequest<AppState>>::from_request(req, &st)
            .await?;
    check_password(&st, body.email, body.password).await?;
    let (dev, token) = mint_device(&st, body.device_name).await?;
    let mut resp = (
        StatusCode::CREATED,
        Json(json!({"token": token, "deviceId": dev.id, "deviceName": dev.name})),
    )
        .into_response();
    resp.extensions_mut().insert(DeviceId(dev.id));
    Ok(resp)
}

/// `POST /v1/session/password {currentPassword, newPassword}` (authenticated):
/// change the owner's password and end every other browser session.
async fn change_password(
    State(st): State<AppState>,
    axum::Extension(ident): axum::Extension<Identity>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<PasswordChangeBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let owner = st.owner.clone();
    let current = tokio::task::spawn_blocking(move || owner.load())
        .await
        .map_err(|_| ApiError::internal("owner store failed"))?
        .map_err(|e| registry_error(&e))?;
    let Some(current) = current else {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "no_owner_account",
            "no owner account; run `secretary-server user set` on the server",
        ));
    };
    if let Err(e) = crate::owner::check_new_password(&body.new_password) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("newPassword: {e}"),
        ));
    }
    // A wrong current password is 403, not 401: the caller is still logged in.
    match check_password(&st, current.email, body.current_password).await {
        Err(e) if e.code == "invalid_credentials" => {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "invalid_credentials",
                "the current password is wrong",
            ))
        }
        other => other?,
    }
    let owner = st.owner.clone();
    let sessions = st.sessions.store().clone();
    let keep = (ident.via == Via::Cookie)
        .then(|| cookie_value(&headers))
        .flatten();
    let revoked = tokio::task::spawn_blocking(move || {
        owner.change_password(&body.new_password)?;
        sessions.delete_all_except(keep.as_deref())
    })
    .await
    .map_err(|_| ApiError::internal("owner store failed"))?
    .map_err(|e| registry_error(&e))?;
    Ok(Json(json!({"changed": true, "sessionsRevoked": revoked})))
}

/// Authenticated account routes (behind `require_device`).
pub fn account_routes() -> Router<AppState> {
    Router::new().route("/v1/session/password", post(change_password))
}

/// `GET /v1/session`: `{authenticated, deviceId, deviceName}`; never 401.
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
    Router::new()
        .route("/v1/session", get(status).post(login).delete(logout))
        .route("/v1/auth/device", post(device_login))
}

/// `GET|HEAD /v1/auth/owner`: the forward_auth target of the owner-only
/// plugins (Instagram, Pixiv, NClient) behind Caddy.
///
/// - 204: a device session (Bearer device token or `sec_session` cookie).
/// - 401: no or bad credentials, a revoked device.
/// - 403 `owner_only`: Lucina's service token. She reaches the plugin
///   services on loopback with their own gateway secret, never through p1.
///
/// Unlike [`require_device`] this route has its own, larger bucket
/// (`SECRETARY_OWNER_AUTH_PER_MINUTE`, per device): Caddy asks once per
/// plugin API call, and those must not eat the 60/min the app's own
/// requests share. No CSRF check: Caddy turns the subrequest into a GET and
/// gates the real method itself.
pub fn owner_routes() -> Router<AppState> {
    Router::new().route("/v1/auth/owner", get(owner_check))
}

async fn owner_check(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let svc = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, t)| t.trim())
        .filter(|t| t.starts_with(crate::service::SERVICE_PREFIX));
    if let Some(token) = svc {
        return match &st.service_token {
            Some(expected) if expected.matches(token) => Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "owner_only",
                "this is for the owner's devices, not Lucina's service token",
            )),
            _ => Err(ApiError::unauthorized("unknown service token")),
        };
    }
    let ident = identify(&st, &headers)?.map_err(ApiError::unauthorized)?;
    st.owner_limiter
        .check(&ident.device_id)
        .map_err(ApiError::rate_limited)?;
    Ok(StatusCode::NO_CONTENT)
}
