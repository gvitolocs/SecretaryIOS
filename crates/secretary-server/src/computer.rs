//! Lucina's own computer for the apps and the MCP: `/v1/computer/*`, forwarded
//! to Lucina's `/api/secretary/computer/*` (crate `lucina-server`, module
//! `computer`).
//!
//! Owner devices only (device token or web session; Lucina's service token is
//! not accepted). The MCP reaches these routes with the owner device token it
//! was configured with and names its client in `X-Secretary-Caller`
//! (`mcp:claude`), which travels to Lucina as `x-lucina-caller` for her log.
//! The audit `device` of takeovers, input and views is the authenticated
//! device's name, never a value from the body.
//!
//! - `GET  /v1/computer`                  status and who drives each sandbox
//! - `POST /v1/computer/task`             `{task}` → 202 `{jobId}` (always a job: tasks run up to 200 s)
//! - `GET  /v1/computer/task/{id}`        the job; `POST …/{id}/cancel` stops it
//! - `POST /v1/computer/call`             `{sandbox?, op, ...}` direct sandbox call
//! - `GET  /v1/computer/screenshot?sandbox=` PNG
//! - `GET|POST /v1/computer/control`      takeover state / `{sandbox, action: take|release|heartbeat}`
//! - `POST /v1/computer/input`            `{sandbox, events}` (fallback view input)
//! - `POST /v1/computer/view`             `{sandbox, mode}` → `{viewUrl, …}`
//! - `POST /v1/computer/view/{id}/ticket` `{mode?}`: the same view, re-issued
//!   (a renewal, or the watch↔control switch of a take-over) so the running
//!   viewer takes the new ticket instead of the page reloading it
//! - `GET  /v1/computer/viewer-assets/{version}/{file}`: Cua's viewer files and
//!   our shim, immutable and ticket-free (the view itself keeps its ticket)
//! - `ANY  /v1/computer/view/{viewId}/{*rest}` the live view itself, **without**
//!   device auth: an iframe or WebView cannot send the bearer, and a WebSocket
//!   cannot carry it. The view id (64 hex, minted for an owner device by
//!   `POST /v1/computer/view`, expiring with its ticket, killed on give-back)
//!   is the capability; Lucina checks it and the sandbox checks the ticket.
//!   Cookies and `Authorization` are never forwarded, and every response is
//!   served under a CSP `sandbox`, so Cua's viewer runs in an opaque origin.

use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Extension, Json, Router};
use hyper_util::rt::TokioIo;
use secretary_core::Call;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::auth::Identity;
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::AppState;

type ApiResult = Result<(StatusCode, Json<Value>), ApiError>;

/// The client the MCP names (`mcp:claude`, `mcp:grok`).
pub const CALLER_HEADER: &str = "x-secretary-caller";

/// Same policy Lucina sets: Cua's page never gets this origin.
pub const VIEW_CSP: &str = "sandbox allow-scripts allow-pointer-lock; frame-ancestors 'self'";

/// Viewer RPC bodies are small.
const VIEW_BODY_LIMIT: usize = 256 * 1024;

const PASS_REQUEST: [&str; 9] = [
    "content-type",
    "connect-protocol-version",
    "connect-timeout-ms",
    "x-cua-env-authorization",
    "accept",
    "access-control-request-method",
    "access-control-request-headers",
    "x-grpc-web",
    "x-user-agent",
];

pub(crate) fn caller(headers: &HeaderMap) -> String {
    headers
        .get(CALLER_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| {
            s.starts_with("mcp:")
                && (5..=40).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b':' || b == b'-')
        })
        .unwrap_or("app")
        .to_owned()
}

/// The device label Lucina logs: the device's own name, plus the MCP client.
pub(crate) fn device_label(ident: &Identity, caller: &str) -> String {
    let name: String = ident.device_name.chars().filter(|c| !c.is_control()).take(40).collect();
    if caller == "app" {
        name
    } else {
        format!("{name} ({caller})")
    }
}

async fn forward(st: &AppState, call: Call) -> ApiResult {
    let (status, data) = st.lucina()?.call_with_status::<Value>(&call).await?;
    Ok((StatusCode::from_u16(status).unwrap_or(StatusCode::OK), Json(data)))
}

fn sandbox_of(body: &Value) -> Result<&str, ApiError> {
    match body.get("sandbox").and_then(Value::as_str).unwrap_or("workspace") {
        s @ ("workspace" | "android") => Ok(s),
        _ => Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "sandbox: must be workspace or android")),
    }
}

fn object(body: Value) -> Result<serde_json::Map<String, Value>, ApiError> {
    match body {
        Value::Object(m) => Ok(m),
        _ => Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "body must be a JSON object")),
    }
}

#[derive(serde::Deserialize, Default)]
struct SandboxQuery {
    sandbox: Option<String>,
}

async fn status(State(st): State<AppState>, headers: HeaderMap) -> ApiResult {
    forward(&st, Call::computer_status().with_caller(&caller(&headers))).await
}

async fn task(State(st): State<AppState>, headers: HeaderMap, JsonBody(body): JsonBody<Value>) -> ApiResult {
    let task = body.get("task").and_then(Value::as_str).unwrap_or("");
    forward(&st, Call::computer_task(task)?.with_caller(&caller(&headers))).await
}

async fn job(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    forward(&st, Call::computer_job(&id)?.with_caller(&caller(&headers))).await
}

async fn job_cancel(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    forward(&st, Call::computer_job_cancel(&id)?.with_caller(&caller(&headers))).await
}

async fn call(State(st): State<AppState>, headers: HeaderMap, JsonBody(body): JsonBody<Value>) -> ApiResult {
    let mut m = object(body)?;
    let sandbox = sandbox_of(&Value::Object(m.clone()))?.to_owned();
    m.insert("sandbox".into(), json!(sandbox));
    forward(&st, Call::computer_call(Value::Object(m)).with_caller(&caller(&headers))).await
}

async fn screenshot(State(st): State<AppState>, QueryArgs(q): QueryArgs<SandboxQuery>) -> Result<Response, ApiError> {
    let call = Call::computer_screenshot(q.sandbox.as_deref().unwrap_or("workspace"))?;
    let (content_type, body) = st.lucina()?.bytes(&call).await?;
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        body,
    )
        .into_response())
}

async fn control_state(State(st): State<AppState>, QueryArgs(q): QueryArgs<SandboxQuery>) -> ApiResult {
    forward(&st, Call::computer_control_state(q.sandbox.as_deref().unwrap_or("workspace"))?).await
}

async fn control(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult {
    let sandbox = sandbox_of(&body)?.to_owned();
    let action = body.get("action").and_then(Value::as_str).unwrap_or("");
    if !matches!(action, "take" | "release" | "heartbeat") {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "action: must be take, release or heartbeat"));
    }
    let who = caller(&headers);
    let call = Call::computer_control(json!({
        "sandbox": sandbox,
        "action": action,
        "device": device_label(&ident, &who),
    }));
    forward(&st, call.with_caller(&who)).await
}

async fn input(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult {
    let sandbox = sandbox_of(&body)?.to_owned();
    let events = body.get("events").cloned().unwrap_or(Value::Null);
    let who = caller(&headers);
    let call = Call::computer_input(json!({
        "sandbox": sandbox,
        "events": events,
        "device": device_label(&ident, &who),
    }));
    forward(&st, call.with_caller(&who)).await
}

async fn view(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult {
    let sandbox = sandbox_of(&body)?.to_owned();
    let mode = match body.get("mode").and_then(Value::as_str).unwrap_or("watch") {
        m @ ("watch" | "control") => m,
        _ => return Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "mode: must be watch or control")),
    };
    let who = caller(&headers);
    let call = Call::computer_view(json!({
        "sandbox": sandbox,
        "mode": mode,
        "device": device_label(&ident, &who),
    }));
    let (status, Json(data)) = forward(&st, call.with_caller(&who)).await?;
    let view_id = data.get("viewId").and_then(Value::as_str).unwrap_or("");
    let ticket = data.get("ticket").and_then(Value::as_str).unwrap_or("");
    if !valid_view_id(view_id) || ticket.is_empty() || ticket.contains(['#', '&', ' ']) {
        return Err(ApiError::new(StatusCode::BAD_GATEWAY, "lucina_error", "Lucina sent no usable view"));
    }
    // Cua's viewer reads these from the fragment; audio stays off (the Android
    // sandbox has no sound server, and the owner watches silently).
    let clipboard = if mode == "control" { "on" } else { "off" };
    Ok((
        status,
        Json(json!({
            "viewId": view_id,
            "viewUrl": format!(
                "/v1/computer/view/{view_id}/viewer/#ticket={ticket}&clipboard={clipboard}&audio=off&mode={mode}"
            ),
            "sandbox": sandbox,
            "mode": mode,
            "expiresAt": data.get("expiresAt"),
            "ttlSecs": data.get("ttlSecs"),
            "epoch": data.get("epoch"),
        })),
    ))
}

/// Owner routes (behind `auth::require_device`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/computer", get(status))
        .route("/v1/computer/task", post(task))
        .route("/v1/computer/task/{id}", get(job))
        .route("/v1/computer/task/{id}/cancel", post(job_cancel))
        .route("/v1/computer/call", post(call))
        .route("/v1/computer/screenshot", get(screenshot))
        .route("/v1/computer/control", get(control_state).post(control))
        .route("/v1/computer/input", post(input))
        .route("/v1/computer/view", post(view))
        .route("/v1/computer/view/{view_id}/ticket", post(view_ticket))
}

/// The live view (capability URL; see the module doc).
pub fn view_routes() -> Router<AppState> {
    Router::new()
        .route("/v1/computer/view/{view_id}/{*rest}", any(view_proxy))
        .route("/v1/computer/viewer-assets/{version}/{file}", get(viewer_asset))
}

/// The files `viewer-assets` may serve: Cua's four and our shim.
const VIEWER_ASSETS: [&str; 5] = [
    "viewer.js",
    "index.css",
    "audioWorklet.js",
    "captureWorklet.js",
    "lucina-shim.js",
];

fn valid_version(v: &str) -> bool {
    v.len() == 16 && v.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `GET /v1/computer/viewer-assets/{version}/{file}`: immutable viewer assets,
/// reachable without a device token because the viewer document is an opaque
/// origin (and because they are Cua's public static files, not a capability).
async fn viewer_asset(
    State(st): State<AppState>,
    Path((version, file)): Path<(String, String)>,
) -> Response {
    if !valid_version(&version) || !VIEWER_ASSETS.contains(&file.as_str()) {
        return ApiError::new(StatusCode::NOT_FOUND, "not_found", "").into_response();
    }
    let Some((addr, auth)) = st.lucina.as_ref().and_then(|l| l.passthrough()) else {
        return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "lucina_not_configured", "").into_response();
    };
    let path = format!("/api/secretary/computer/viewer-assets/{version}/{file}");
    let mut resp = match passthrough_client()
        .get(format!("http://{addr}{path}"))
        .header(header::AUTHORIZATION, auth)
        .send()
        .await
    {
        Ok(up) => {
            let status = up.status();
            let mut headers = HeaderMap::new();
            for (k, v) in up.headers() {
                if matches!(
                    k.as_str(),
                    "content-type" | "cache-control" | "content-length" | "access-control-allow-origin"
                ) {
                    headers.append(k.clone(), v.clone());
                }
            }
            let mut resp = Response::new(Body::from_stream(up.bytes_stream()));
            *resp.status_mut() = status;
            *resp.headers_mut() = headers;
            resp
        }
        Err(e) => {
            tracing::warn!(error = %e.without_url(), "computer viewer assets: Lucina unreachable");
            ApiError::new(StatusCode::BAD_GATEWAY, "lucina_unreachable", "").into_response()
        }
    };
    let status = resp.status();
    harden_asset(status, resp.headers_mut());
    resp
}

/// `POST /v1/computer/view/{viewId}/ticket` `{mode?}`: the running view's ticket
/// again, renewed or switched to the other mode. Owner devices only; the
/// control switch still needs the lease Lucina holds for `POST /v1/computer/control`.
async fn view_ticket(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    headers: HeaderMap,
    Path(view_id): Path<String>,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult {
    if !valid_view_id(&view_id) {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "not_found", ""));
    }
    let who = caller(&headers);
    let mut m = object(body.clone()).unwrap_or_default();
    m.insert("sandbox".into(), json!(sandbox_of(&body)?));
    m.insert("device".into(), json!(device_label(&ident, &who)));
    m.remove("view_id");
    let call = Call::computer_view_ticket(&view_id, Value::Object(m));
    forward(&st, call.with_caller(&who)).await
}

fn valid_view_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn valid_rest(rest: &str) -> bool {
    !rest.is_empty()
        && rest.len() <= 128
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
        && !rest.split('/').any(|seg| seg == ".." || seg == ".")
}

fn harden(h: &mut HeaderMap) {
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(VIEW_CSP));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

/// [`harden`], except that a successful versioned asset keeps Lucina's
/// year-long `immutable` header: the one cacheable thing under the live view.
fn harden_asset(status: StatusCode, h: &mut HeaderMap) {
    let upstream = h.get(header::CACHE_CONTROL).cloned();
    harden(h);
    if status.is_success() {
        if let Some(v) = upstream.filter(|v| v.to_str().is_ok_and(|s| s.contains("immutable"))) {
            h.insert(header::CACHE_CONTROL, v);
        }
    }
}

fn passthrough_client() -> &'static reqwest::Client {
    static C: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .http1_only()
            .no_gzip()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("view proxy client")
    })
}

async fn view_proxy(
    State(st): State<AppState>,
    Path((view_id, rest)): Path<(String, String)>,
    req: Request,
) -> Response {
    if !valid_view_id(&view_id) || !valid_rest(&rest) {
        return ApiError::new(StatusCode::NOT_FOUND, "not_found", "").into_response();
    }
    let Some((addr, auth)) = st.lucina.as_ref().and_then(|l| l.passthrough()) else {
        return ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "lucina_not_configured", "").into_response();
    };
    let upgrade = req
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let path = format!("/api/secretary/computer/view/{view_id}/{rest}");
    let mut resp = if upgrade {
        socket(&addr, &auth, &path, req).await
    } else {
        http(&addr, &auth, &path, req).await
    };
    harden(resp.headers_mut());
    resp
}

async fn http(addr: &str, auth: &str, path: &str, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let mut url = format!("http://{addr}{path}");
    if let Some(q) = parts.uri.query() {
        url.push('?');
        url.push_str(q);
    }
    let body = match axum::body::to_bytes(body, VIEW_BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => return ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "").into_response(),
    };
    let mut rb = passthrough_client()
        .request(parts.method.clone(), &url)
        .header(header::AUTHORIZATION, auth)
        .body(body);
    for name in PASS_REQUEST {
        if let Some(v) = parts.headers.get(name) {
            rb = rb.header(name, v);
        }
    }
    let up = match rb.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e.without_url(), "computer view: Lucina unreachable");
            return ApiError::new(StatusCode::BAD_GATEWAY, "lucina_unreachable", "").into_response();
        }
    };
    let status = up.status();
    let mut headers = HeaderMap::new();
    for (k, v) in up.headers() {
        let n = k.as_str();
        if matches!(n, "content-type" | "content-encoding" | "etag" | "last-modified")
            || n.starts_with("grpc-")
            || n.starts_with("connect-")
            || n.starts_with("access-control-")
        {
            headers.append(k.clone(), v.clone());
        }
    }
    let mut resp = Response::new(Body::from_stream(up.bytes_stream()));
    *resp.status_mut() = status;
    *resp.headers_mut() = headers;
    resp
}

async fn read_head(s: &mut TcpStream) -> std::io::Result<(u16, Vec<(String, String)>, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).into_owned();
            let rest = buf[end + 4..].to_vec();
            let mut lines = head.split("\r\n");
            let status = lines
                .next()
                .and_then(|l| l.split(' ').nth(1))
                .and_then(|c| c.parse().ok())
                .unwrap_or(0);
            let headers = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                .collect();
            return Ok((status, headers, rest));
        }
        if buf.len() > 16 * 1024 {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
    }
}

/// Splices an owner WebSocket (the live view's, LucinaDesk's) to Lucina, byte for byte.
pub(crate) async fn socket(addr: &str, auth: &str, path: &str, mut req: Request) -> Response {
    let target = match req.uri().query() {
        Some(q) => format!("{path}?{q}"),
        None => path.to_owned(),
    };
    let ok_line = |v: &str| !v.bytes().any(|b| b == b'\r' || b == b'\n');
    if !ok_line(&target) {
        return ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "").into_response();
    }
    let mut head = format!(
        "GET {target} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {auth}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n"
    );
    for name in [
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-protocol",
        "sec-websocket-extensions",
    ] {
        if let Some(v) = req.headers().get(name).and_then(|v| v.to_str().ok()) {
            if !ok_line(v) {
                return ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "").into_response();
            }
            head.push_str(&format!("{name}: {v}\r\n"));
        }
    }
    head.push_str("\r\n");
    let mut upstream = match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await {
        Ok(Ok(s)) => s,
        _ => return ApiError::new(StatusCode::BAD_GATEWAY, "lucina_unreachable", "").into_response(),
    };
    let _ = upstream.set_nodelay(true);
    if upstream.write_all(head.as_bytes()).await.is_err() {
        return ApiError::new(StatusCode::BAD_GATEWAY, "lucina_unreachable", "").into_response();
    }
    let (status, up_headers, leftover) =
        match tokio::time::timeout(Duration::from_secs(15), read_head(&mut upstream)).await {
            Ok(Ok(h)) => h,
            _ => return ApiError::new(StatusCode::BAD_GATEWAY, "lucina_unreachable", "").into_response(),
        };
    if status != 101 {
        // 410 view_ended / 403 / 502 from Lucina: pass the status on.
        let st = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
        return ApiError::new(st, "view_refused", "").into_response();
    }
    let on_upgrade = hyper::upgrade::on(&mut req);
    let mut resp = Response::new(Body::empty());
    *resp.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    for (k, v) in &up_headers {
        if matches!(
            k.as_str(),
            "upgrade" | "connection" | "sec-websocket-accept" | "sec-websocket-protocol" | "sec-websocket-extensions"
        ) {
            if let (Ok(n), Ok(v)) = (HeaderName::try_from(k.as_str()), HeaderValue::from_str(v)) {
                resp.headers_mut().insert(n, v);
            }
        }
    }
    tokio::spawn(async move {
        let mut client = match on_upgrade.await {
            Ok(u) => TokioIo::new(u),
            Err(e) => {
                tracing::warn!(error = %e, "computer view: upgrade failed");
                return;
            }
        };
        if !leftover.is_empty() && client.write_all(&leftover).await.is_err() {
            return;
        }
        let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    });
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_ids_and_paths_are_strict() {
        assert!(valid_view_id(&"a1".repeat(32)));
        assert!(!valid_view_id(&"A1".repeat(32)));
        assert!(!valid_view_id("abc"));
        assert!(!valid_view_id(&"g".repeat(64)));
        assert!(valid_rest("viewer/"));
        assert!(valid_rest("viewer/viewer.js"));
        assert!(valid_rest("cua.env.v1.StreamService/OpenMedia"));
        assert!(valid_rest("media"));
        for bad in ["", "../x", "viewer/../../x", "a b", "a%2e", "x?y", &"a".repeat(129)] {
            assert!(!valid_rest(bad), "{bad}");
        }
    }

    #[test]
    fn only_versioned_viewer_assets_are_cacheable() {
        assert!(valid_version("0123456789abcdef"));
        for bad in ["", "0123456789abcde", "0123456789abcdefa", "0123456789abcdeg", "../../../../etc"] {
            assert!(!valid_version(bad), "{bad}");
        }
        let year = HeaderValue::from_static("public, max-age=31536000, immutable");
        // The view: no-store, whatever the sandbox said.
        let mut h = HeaderMap::new();
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300"));
        harden(&mut h);
        assert_eq!(h[header::CACHE_CONTROL], "no-store");
        assert_eq!(h[header::CONTENT_SECURITY_POLICY], VIEW_CSP);
        // An asset keeps the immutable header, an error never does.
        let mut h = HeaderMap::new();
        h.insert(header::CACHE_CONTROL, year.clone());
        harden_asset(StatusCode::OK, &mut h);
        assert_eq!(h[header::CACHE_CONTROL], year);
        let mut h = HeaderMap::new();
        h.insert(header::CACHE_CONTROL, year);
        harden_asset(StatusCode::NOT_FOUND, &mut h);
        assert_eq!(h[header::CACHE_CONTROL], "no-store");
        let mut h = HeaderMap::new();
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300"));
        harden_asset(StatusCode::OK, &mut h);
        assert_eq!(h[header::CACHE_CONTROL], "no-store");
    }

    #[test]
    fn mcp_caller_names_only() {
        let mut h = HeaderMap::new();
        assert_eq!(caller(&h), "app");
        h.insert(CALLER_HEADER, HeaderValue::from_static("mcp:claude"));
        assert_eq!(caller(&h), "mcp:claude");
        h.insert(CALLER_HEADER, HeaderValue::from_static("lucina"));
        assert_eq!(caller(&h), "app", "only MCP clients name themselves");
        h.insert(CALLER_HEADER, HeaderValue::from_static("mcp:Bad Name"));
        assert_eq!(caller(&h), "app");
    }
}
