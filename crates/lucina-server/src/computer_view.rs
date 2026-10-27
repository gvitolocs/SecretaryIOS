//! The live-view proxy: `/api/secretary/computer/view/{viewId}/{*rest}` →
//! the sandbox's own cua-spacesd viewer (`crate::computer` mints the view).
//!
//! secretary-server forwards the apps' viewer traffic here (bearer-authorized
//! like every `/api/secretary` call); the view id is an unguessable capability
//! bound to one sandbox, one mode and, for control views, the driver epoch at
//! minting. Only what Cua's web viewer needs passes:
//!
//! - `GET viewer/…`: the viewer's own seven static files;
//! - `POST`/`OPTIONS cua.env.v1.{Stream,Presence}Service/*` and
//!   `SystemService/{GetCapabilities,Health}` (Connect RPCs, authorized by the
//!   viewer ticket the sandbox checks itself);
//! - `GET media` with `Upgrade: websocket`: the H.264/JPEG and input socket,
//!   spliced byte for byte.
//!
//! Files, ticket minting and every other RPC are refused. A control view dies
//! when the owner gives the desktop back or the lease lapses: requests answer
//! 410 and its socket is closed within two seconds. Socket traffic from the
//! owner's browser is input, logged as byte counts every five seconds. URLs
//! carry the ticket (`/media?ticket=`), so query strings are never logged.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use hyper_util::rt::TokioIo;
use lucina_computer_use::{control, SandboxId, ViewMode};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::api::ApiError;
use crate::AppState;

/// Views kept at once (each app tab holds one).
const MAX_VIEWS: usize = 32;
/// Largest RPC body the viewer sends.
const RPC_BODY_LIMIT: usize = 256 * 1024;
const INPUT_LOG_EVERY: Duration = Duration::from_secs(5);
const WATCHDOG_EVERY: Duration = Duration::from_secs(2);

const ASSETS: [&str; 7] = [
    "viewer",
    "viewer/",
    "viewer/index.html",
    "viewer/index.css",
    "viewer/viewer.js",
    "viewer/audioWorklet.js",
    "viewer/captureWorklet.js",
];

/// Request headers passed to the sandbox.
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

/// Response headers passed back (plus `grpc-*`/`connect-*`/`access-control-*`).
const PASS_RESPONSE: [&str; 5] = [
    "content-type",
    "content-encoding",
    "cache-control",
    "etag",
    "last-modified",
];

/// The viewer runs in an opaque origin: even when opened at top level, Cua's
/// page never gets the app's origin (cookies, storage).
pub const VIEW_CSP: &str = "sandbox allow-scripts allow-pointer-lock; frame-ancestors 'self'";

#[derive(Debug, Clone)]
struct View {
    sandbox: SandboxId,
    mode: ViewMode,
    device: String,
    epoch: u64,
    machine: String,
    expires: Instant,
}

fn views() -> &'static Mutex<HashMap<String, View>> {
    static V: OnceLock<Mutex<HashMap<String, View>>> = OnceLock::new();
    V.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Registers a minted view and returns its id (64 hex chars, 244 random bits).
pub fn open(sandbox: SandboxId, mode: ViewMode, device: &str, epoch: u64, machine: &str) -> String {
    let id = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let mut map = views().lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    map.retain(|_, v| v.expires > now);
    if map.len() >= MAX_VIEWS {
        // Oldest first: a forgotten tab never blocks a new view.
        if let Some(oldest) = map.iter().min_by_key(|(_, v)| v.expires).map(|(k, _)| k.clone()) {
            map.remove(&oldest);
        }
    }
    map.insert(
        id.clone(),
        View {
            sandbox,
            mode,
            device: device.to_owned(),
            epoch,
            machine: machine.to_owned(),
            // A little longer than the ticket: the sandbox refuses it first.
            expires: now + lucina_computer_use::viewer::VIEW_TTL + Duration::from_secs(30),
        },
    );
    id
}

/// Forgets every control view of a sandbox (the owner gave it back).
pub fn drop_control_views(sb: SandboxId) {
    let mut map = views().lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|_, v| !(v.sandbox == sb && v.mode == ViewMode::Control));
}

/// The view, if it is still alive. A control view also needs the owner's lease
/// under the epoch it was minted in.
fn live(id: &str) -> Option<View> {
    let mut map = views().lock().unwrap_or_else(|e| e.into_inner());
    let v = map.get(id)?.clone();
    let alive = v.expires > Instant::now()
        && (v.mode == ViewMode::Watch
            || (control().owner_driving(v.sandbox) && control().epoch(v.sandbox) == v.epoch));
    if !alive {
        map.remove(id);
        return None;
    }
    Some(v)
}

#[derive(Debug, PartialEq, Eq)]
enum Target {
    Asset,
    Rpc,
    Socket,
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn classify(method: &Method, rest: &str, upgrade: bool) -> Option<Target> {
    if upgrade {
        return (method == Method::GET && rest == "media").then_some(Target::Socket);
    }
    if ASSETS.contains(&rest) {
        return (method == Method::GET || method == Method::HEAD).then_some(Target::Asset);
    }
    let (svc, rpc) = rest.strip_prefix("cua.env.v1.")?.split_once('/')?;
    if !is_ident(svc) || !is_ident(rpc) || !(method == Method::POST || method == Method::OPTIONS) {
        return None;
    }
    let allowed = matches!(svc, "StreamService" | "PresenceService")
        || (svc == "SystemService" && matches!(rpc, "GetCapabilities" | "Health"));
    allowed.then_some(Target::Rpc)
}

fn is_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

fn harden(h: &mut HeaderMap) {
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(VIEW_CSP));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(
        HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-site"),
    );
}

fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .http1_only()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("view proxy client")
    })
}

/// `ANY /api/secretary/computer/view/{viewId}/{*rest}` (bearer-authorized by the caller).
pub async fn proxy(
    State(st): State<AppState>,
    Path((view_id, rest)): Path<(String, String)>,
    req: Request,
) -> Response {
    if !crate::api::bearer_ok(&st, req.headers()) {
        return ApiError::new(401, "unauthorized", "").into_response();
    }
    let Some(view) = live(&view_id) else {
        return ApiError::new(410, "view_ended", "this live view has ended; open a new one").into_response();
    };
    let upgrade = is_upgrade(req.headers());
    let Some(target) = classify(req.method(), &rest, upgrade) else {
        tracing::warn!(sandbox = view.sandbox.as_str(), method = %req.method(), path = %rest, "live view request refused");
        return ApiError::new(403, "not_allowed", "").into_response();
    };
    let mut resp = match target {
        Target::Socket => socket(view, req).await,
        Target::Asset | Target::Rpc => forward(&view, &rest, req).await,
    };
    harden(resp.headers_mut());
    resp
}

async fn forward(view: &View, rest: &str, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let mut url = format!("{}/{}", view.machine, rest);
    if let Some(q) = parts.uri.query() {
        url.push('?');
        url.push_str(q);
    }
    let body = match axum::body::to_bytes(body, RPC_BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => return ApiError::new(413, "too_large", "").into_response(),
    };
    let mut rb = http().request(parts.method.clone(), &url).body(body);
    for name in PASS_REQUEST {
        if let Some(v) = parts.headers.get(name) {
            rb = rb.header(name, v);
        }
    }
    let up = match rb.send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(sandbox = view.sandbox.as_str(), error = %e.without_url(), "live view upstream failed");
            return ApiError::new(502, "sandbox_unreachable", "").into_response();
        }
    };
    let status = up.status();
    let mut headers = HeaderMap::new();
    for (k, v) in up.headers() {
        let n = k.as_str();
        if PASS_RESPONSE.contains(&n)
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

/// Reads an HTTP/1.1 response head; returns `(status, headers, bytes after the head)`.
async fn read_head(s: &mut TcpStream) -> std::io::Result<(u16, Vec<(String, String)>, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = s.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "closed before the response head"));
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
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "response head too large"));
        }
    }
}

fn header_line_ok(v: &str) -> bool {
    !v.bytes().any(|b| b == b'\r' || b == b'\n')
}

/// Splices the viewer's `/media` WebSocket to the sandbox.
async fn socket(view: View, mut req: Request) -> Response {
    let Some(addr) = view.machine.strip_prefix("http://") else {
        return ApiError::new(502, "sandbox_unreachable", "").into_response();
    };
    let path_q = match req.uri().query() {
        Some(q) => format!("/media?{q}"),
        None => "/media".to_owned(),
    };
    let mut head = format!("GET {path_q} HTTP/1.1\r\nHost: {addr}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n");
    for name in [
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-protocol",
        "sec-websocket-extensions",
        "x-cua-env-authorization",
    ] {
        if let Some(v) = req.headers().get(name).and_then(|v| v.to_str().ok()) {
            if !header_line_ok(v) || !header_line_ok(&path_q) {
                return ApiError::bad_request("bad header").into_response();
            }
            head.push_str(&format!("{name}: {v}\r\n"));
        }
    }
    head.push_str("\r\n");
    let mut upstream = match tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(addr)).await {
        Ok(Ok(s)) => s,
        _ => return ApiError::new(502, "sandbox_unreachable", "").into_response(),
    };
    let _ = upstream.set_nodelay(true);
    if upstream.write_all(head.as_bytes()).await.is_err() {
        return ApiError::new(502, "sandbox_unreachable", "").into_response();
    }
    let (status, up_headers, leftover) =
        match tokio::time::timeout(Duration::from_secs(10), read_head(&mut upstream)).await {
            Ok(Ok(h)) => h,
            _ => return ApiError::new(502, "sandbox_unreachable", "").into_response(),
        };
    if status != 101 {
        tracing::warn!(sandbox = view.sandbox.as_str(), status, "live view socket refused by the sandbox");
        return ApiError::new(502, "sandbox_refused", format!("sandbox answered {status}")).into_response();
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
    control().note(view.sandbox, "view_socket_open", json!({"device": view.device, "mode": view.mode.as_str()}));
    tokio::spawn(async move {
        let client = match on_upgrade.await {
            Ok(u) => TokioIo::new(u),
            Err(e) => {
                tracing::warn!(error = %e, "live view upgrade failed");
                return;
            }
        };
        splice(view, client, upstream, leftover).await;
    });
    resp
}

async fn splice<C>(view: View, client: C, upstream: TcpStream, leftover: Vec<u8>)
where
    C: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut cr, mut cw) = tokio::io::split(client);
    let (mut ur, mut uw) = upstream.into_split();
    let input = Arc::new(AtomicU64::new(0));
    let up_count = input.clone();
    let up = async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = match cr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            up_count.fetch_add(n as u64, Ordering::Relaxed);
            if uw.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
    };
    let down = async move {
        if !leftover.is_empty() && cw.write_all(&leftover).await.is_err() {
            return;
        }
        let _ = tokio::io::copy(&mut ur, &mut cw).await;
    };
    let watch_view = view.clone();
    let watchdog = async move {
        let mut tick = tokio::time::interval(WATCHDOG_EVERY);
        let mut last_log = Instant::now();
        let mut logged = 0u64;
        loop {
            tick.tick().await;
            if watch_view.expires <= Instant::now() {
                return "expired";
            }
            if watch_view.mode == ViewMode::Control {
                if !control().owner_driving(watch_view.sandbox) || control().epoch(watch_view.sandbox) != watch_view.epoch {
                    return "control ended";
                }
                if last_log.elapsed() >= INPUT_LOG_EVERY {
                    let total = input.load(Ordering::Relaxed);
                    if total > logged {
                        let batch = json!({"via": "live", "bytes": total - logged});
                        if control().owner_input(watch_view.sandbox, &watch_view.device, batch).is_err() {
                            return "control ended";
                        }
                        logged = total;
                    }
                    last_log = Instant::now();
                }
            }
        }
    };
    let why = tokio::select! {
        _ = up => "viewer closed",
        _ = down => "sandbox closed",
        why = watchdog => why,
    };
    control().note(view.sandbox, "view_socket_closed", json!({"device": view.device, "mode": view.mode.as_str(), "why": why}));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_viewer_surface_is_reachable() {
        let get = Method::GET;
        let post = Method::POST;
        assert_eq!(classify(&get, "viewer/", false), Some(Target::Asset));
        assert_eq!(classify(&get, "viewer/viewer.js", false), Some(Target::Asset));
        assert_eq!(classify(&post, "viewer/viewer.js", false), None);
        assert_eq!(classify(&post, "cua.env.v1.StreamService/OpenMedia", false), Some(Target::Rpc));
        assert_eq!(classify(&Method::OPTIONS, "cua.env.v1.PresenceService/Join", false), Some(Target::Rpc));
        assert_eq!(classify(&post, "cua.env.v1.SystemService/GetCapabilities", false), Some(Target::Rpc));
        assert_eq!(classify(&get, "media", true), Some(Target::Socket));
        for (m, p, up) in [
            (&post, "cua.env.v1.SystemService/CreateViewerTicket", false),
            (&post, "cua.env.v1.FilesService/Read", false),
            (&get, "cua.env.v1.StreamService/OpenMedia", false),
            (&post, "cua.env.v1.Stream.Service/X", false),
            (&get, "viewer/../files", false),
            (&get, "viewer/unknown.js", false),
            (&get, "files", false),
            (&get, "media", false),
            (&get, "files", true),
            (&post, "media", true),
            (&get, "", false),
        ] {
            assert_eq!(classify(m, p, up), None, "{m} {p} {up}");
        }
    }

    #[test]
    fn control_views_die_with_the_lease() {
        let sb = SandboxId::Android;
        control().take(sb, "test");
        let epoch = control().epoch(sb);
        let c = open(sb, ViewMode::Control, "test", epoch, "http://127.0.0.1:1");
        let w = open(sb, ViewMode::Watch, "test", epoch, "http://127.0.0.1:1");
        assert!(live(&c).is_some());
        control().release(sb, "test");
        assert!(live(&c).is_none(), "a control view ends with the takeover");
        assert!(live(&w).is_some(), "watching goes on");
        // A control view from an older takeover stays dead after a new one.
        control().take(sb, "test");
        assert!(live(&c).is_none());
        control().release(sb, "test");
        assert!(live("nope").is_none());
    }

    #[tokio::test]
    async fn the_socket_is_spliced_byte_for_byte() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).await.unwrap();
            s.write_all(&buf).await.unwrap();
        });
        let upstream = TcpStream::connect(addr).await.unwrap();
        let (mut ours, theirs) = tokio::io::duplex(64);
        let view = View {
            sandbox: SandboxId::Workspace,
            mode: ViewMode::Watch,
            device: "t".into(),
            epoch: 0,
            machine: format!("http://{addr}"),
            expires: Instant::now() + Duration::from_secs(5),
        };
        let task = tokio::spawn(splice(view, theirs, upstream, b"hi".to_vec()));
        let mut got = [0u8; 2];
        ours.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hi", "bytes after the 101 head reach the viewer first");
        ours.write_all(b"ping").await.unwrap();
        let mut echo = [0u8; 4];
        ours.read_exact(&mut echo).await.unwrap();
        assert_eq!(&echo, b"ping");
        drop(ours);
        task.await.unwrap();
    }
}
