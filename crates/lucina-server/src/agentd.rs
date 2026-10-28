//! `/api/secretary/agentd/{*path}`: how secretary-server (on p1, through the Lucina
//! Link tunnel like every `/api/secretary` route) reaches lucina-agentd, which listens
//! only on a unix socket here on nezopt (Lucina Agents phase 7,
//! `docs/design/agents-api.md`).
//!
//! A raw forwarder: `/api/secretary/agentd/<path>?<query>` goes to the daemon's
//! `/<path>?<query>`, with the method, body and status unchanged and the response
//! streamed (the daemon's SSE `/stream`, long-polls and `wait`). secretary-server maps
//! its public `/v1/agents/*` paths onto the daemon's.
//!
//! The Secretary bearer is checked before this runs (`api::secretary`). Then:
//! - `X-Lucina-Origin` is required and must be one that secretary-server sets
//!   (`device:<id>`, `mcp:<id>`, `service:<name>`). It is never empty or `local`, so a
//!   request through here never gets the host owner's (`lagent`'s) rights.
//! - Only `Content-Type`, `Accept`, `Last-Event-ID`, `X-Lucina-Delegation-Depth` and the
//!   origin are passed on; for a workspace service (`/workspaces/<id>/services/<name>/…`,
//!   phase 5) also the conditional, range and language headers a dev server needs.
//!   Cookies and credentials never are.
//! - Its own errors use the daemon's shape, `{"error": {"code", "message"}}`, not the
//!   `/api/secretary` envelope.
//! - The socket: `LUCINA_AGENTD_SOCKET`, else `$XDG_RUNTIME_DIR/lucina-agentd/agentd.sock`
//!   (the daemon's own default).

use std::path::PathBuf;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;

pub const ORIGIN_HEADER: &str = "x-lucina-origin";
const PREFIX: &str = "/api/secretary/agentd";
/// Agent registrations, events and file writes stay well under this.
const MAX_BODY: usize = 2 * 1024 * 1024;
const PASS: [&str; 4] = ["content-type", "accept", "last-event-id", "x-lucina-delegation-depth"];
/// Also passed on for a workspace service (the proxied dev server).
const PASS_SERVICE: [&str; 5] = ["accept-language", "if-none-match", "if-modified-since", "range", "x-requested-with"];
/// Response headers passed back (the daemon's, and a proxied service's).
const PASS_BACK: [header::HeaderName; 10] = [
    header::CONTENT_TYPE,
    header::CACHE_CONTROL,
    header::CONTENT_ENCODING,
    header::LOCATION,
    header::ETAG,
    header::LAST_MODIFIED,
    header::CONTENT_DISPOSITION,
    header::CONTENT_SECURITY_POLICY,
    header::CONTENT_RANGE,
    header::ACCEPT_RANGES,
];

/// `LUCINA_AGENTD_SOCKET`, else the daemon's default under `$XDG_RUNTIME_DIR` (or
/// `~/.local/state`), as `lucina-agentd`'s `config::default_socket`.
pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("LUCINA_AGENTD_SOCKET") {
        return PathBuf::from(p);
    }
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("lucina-agentd").join("agentd.sock"),
        None => {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
            home.join(".local/state/lucina-agentd/agentd.sock")
        }
    }
}

fn token(s: &str, max: usize, extra: &[u8]) -> bool {
    (1..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
}

/// The origins secretary-server sends.
pub fn valid_origin(v: &str) -> bool {
    if let Some(d) = v.strip_prefix("device:") {
        return token(d, 64, b"_-");
    }
    if let Some(m) = v.strip_prefix("mcp:") {
        return token(m, 64, b"_-:");
    }
    if let Some(s) = v.strip_prefix("service:") {
        return token(s, 40, b"-") && s.bytes().all(|b| !b.is_ascii_uppercase());
    }
    false
}

/// A workspace service path (`/workspaces/<id>/services/<name>[/<path>]`, phase 5):
/// the service's own part may end in `/` and use any URL path character
/// (RFC 3986 `pchar`), still without `.` or `..` segments.
pub fn service_path(rest: &str) -> bool {
    let mut segs = rest.strip_prefix('/').unwrap_or("").splitn(5, '/');
    let (Some("workspaces"), Some(id), Some("services"), Some(name)) = (segs.next(), segs.next(), segs.next(), segs.next()) else { return false };
    let strict = |s: &str| !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b)) && s != "." && s != "..";
    if !strict(id) || !strict(name) {
        return false;
    }
    let tail = segs.next().unwrap_or("");
    rest.len() <= 2048
        && tail.split('/').all(|seg| seg != "." && seg != ".." && seg.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~%!$&'()*+,;=:@".contains(&b)))
}

/// The daemon path for a request path under [`PREFIX`]: segments of URL-safe
/// characters, no `.` or `..`, no empty segment ([`service_path`] is looser).
pub fn daemon_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix(PREFIX)?;
    if service_path(rest) {
        return Some(rest);
    }
    let segs = rest.strip_prefix('/')?;
    let ok = !segs.is_empty()
        && rest.len() <= 512
        && segs.split('/').all(|seg| {
            !seg.is_empty() && seg != "." && seg != ".." && seg.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~%:@".contains(&b))
        });
    ok.then_some(rest)
}

/// Errors in the daemon's shape (`{"error": {"code", "message"}}`), so secretary-server
/// passes one kind of error body on whether it came from here or from the daemon.
fn error(status: u16, code: &str, message: &str) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut resp = (status, axum::Json(serde_json::json!({"error": {"code": code, "message": message}}))).into_response();
    resp.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

pub async fn forward(method: &Method, uri: &Uri, headers: &HeaderMap, body: Body) -> Response {
    forward_to(&socket_path(), method, uri, headers, body).await
}

/// [`forward`] to a given socket (tests).
pub async fn forward_to(socket: &std::path::Path, method: &Method, uri: &Uri, headers: &HeaderMap, body: Body) -> Response {
    let Some(path) = daemon_path(uri.path()) else {
        return error(404, "not_found", "");
    };
    let origin = headers.get(ORIGIN_HEADER).and_then(|v| v.to_str().ok()).map(str::trim).unwrap_or("");
    if !valid_origin(origin) {
        return error(400, "bad_request", "X-Lucina-Origin: device:<id>, mcp:<id> or service:<name>");
    }
    let target = match uri.query() {
        Some(q) if q.len() > 4096 || q.bytes().any(|b| b.is_ascii_control() || b == b' ') => {
            return error(400, "bad_request", "query")
        }
        Some(q) => format!("{path}?{q}"),
        None => path.to_owned(),
    };
    let body = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => return error(413, "payload_too_large", ""),
    };
    let mut req = Request::builder()
        .method(method.clone())
        .uri(&target)
        .header(header::HOST, "lucina-agentd")
        .header(ORIGIN_HEADER, origin);
    let service = service_path(path);
    for name in PASS.iter().chain(if service { PASS_SERVICE.as_slice() } else { &[] }) {
        if let Some(v) = headers.get(*name) {
            req = req.header(*name, v);
        }
    }
    let req = match req.header(header::CONTENT_LENGTH, body.len()).body(http_body_util::Full::new(body)) {
        Ok(r) => r,
        Err(_) => return error(400, "bad_request", ""),
    };
    let stream = match tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(socket)).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, socket = %socket.display(), "agentd unreachable");
            return error(503, "agentd_unavailable", "lucina-agentd is not running");
        }
        Err(_) => return error(503, "agentd_unavailable", "lucina-agentd did not accept the connection"),
    };
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "agentd handshake failed");
            return error(502, "agentd_error", "");
        }
    };
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::debug!(error = %e, "agentd connection ended");
        }
    });
    let resp = match sender.send_request(req).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "agentd request failed");
            return error(502, "agentd_error", "");
        }
    };
    let (parts, incoming) = resp.into_parts();
    if parts.status == StatusCode::SWITCHING_PROTOCOLS {
        // No upgrades through here: terminal output streams as SSE, and the service proxy is HTTP only.
        return error(502, "agentd_error", "");
    }
    let mut out = Response::new(Body::new(incoming));
    *out.status_mut() = parts.status;
    for name in PASS_BACK {
        if let Some(v) = parts.headers.get(&name) {
            out.headers_mut().insert(name, v.clone());
        }
    }
    let sse = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    if sse {
        out.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        out.headers_mut().insert("x-accel-buffering", HeaderValue::from_static("no"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_secretary_server_sends() {
        for ok in ["device:dev_1", "mcp:hermes", "mcp:tok:abc", "service:infisical-broker"] {
            assert!(valid_origin(ok), "{ok}");
        }
        for bad in ["", "local", "mcp", "device:", "device:a b", "device:a/b", "service:Broker", "service:", "owner:x", &format!("device:{}", "a".repeat(65))] {
            assert!(!valid_origin(bad), "{bad}");
        }
    }

    #[test]
    fn daemon_paths() {
        assert_eq!(daemon_path("/api/secretary/agentd/agents"), Some("/agents"));
        assert_eq!(daemon_path("/api/secretary/agentd/agents/abc-1/send"), Some("/agents/abc-1/send"));
        assert_eq!(daemon_path("/api/secretary/agentd/permissions/ext_1"), Some("/permissions/ext_1"));
        for bad in [
            "/api/secretary/agentd",
            "/api/secretary/agentd/",
            "/api/secretary/agentd/../x",
            "/api/secretary/agentd/agents//x",
            "/api/secretary/agentd/a b",
            "/api/secretary/agentdx/agents",
            "/api/secretary/agentd/./agents",
        ] {
            assert_eq!(daemon_path(bad), None, "{bad}");
        }
    }

    #[test]
    fn service_paths_are_looser() {
        for ok in [
            "/api/secretary/agentd/workspaces/wks_1/services/web/",
            "/api/secretary/agentd/workspaces/wks_1/services/web",
            "/api/secretary/agentd/workspaces/wks_1/services/port-4173/assets/index-a1+b=.js",
            "/api/secretary/agentd/workspaces/wks_1/services/web/@vite/client",
        ] {
            assert!(daemon_path(ok).is_some(), "{ok}");
        }
        for bad in [
            "/api/secretary/agentd/workspaces/wks_1/services/web/../../agents",
            "/api/secretary/agentd/workspaces/wks_1/services/web/./x",
            "/api/secretary/agentd/workspaces/wks 1/services/web/",
            "/api/secretary/agentd/workspaces/wks_1/services/../x",
            "/api/secretary/agentd/workspaces/wks_1/services/web/a b",
            "/api/secretary/agentd/workspaces/wks_1/services/web/a\\b",
        ] {
            assert_eq!(daemon_path(bad), None, "{bad}");
        }
        assert_eq!(daemon_path("/api/secretary/agentd/terminals/term_1/stream"), Some("/terminals/term_1/stream"));
    }
}
