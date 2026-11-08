//! `/v1/notebooks/*`: Lucina Notebooks (the `lucina-notebooks` crate inside
//! lucina-server on nezopt) for the apps and the MCP. The contract is
//! `docs/design/notebooks-api.md` (branch `notebooks`).
//!
//! The way there: this server → Lucina (`SECRETARY_LUCINA_URL`, the Link tunnel on
//! p1) `/api/secretary/notebooks<rest>`. `/v1/notebooks<rest>` maps 1:1: the same
//! method, query, body and status; the root is `/api/secretary/notebooks` itself.
//!
//! Who is calling: owner devices (device token, or web session with cookie + CSRF on
//! writes). The MCP reaches the routes with its device token and names its client in
//! `X-Secretary-Caller: mcp:<name>`, as it does for `/v1/computer/*`; its tokens'
//! `notebooks` scope is checked in the MCP, which this server cannot see. Service
//! tokens (the agents MCP token, the broker, Lucina's own) are not accepted.
//! `X-Lucina-Origin` is always set here (`device:<id>` or `mcp:<name>`), never taken
//! from the client; it only goes into Lucina's log.
//!
//! Streams, both ways. Responses stream (SSE chat answers, podcast audio with `Range`,
//! original files) and a few headers pass: `Content-Type`, `Accept`, `Range` and
//! `Last-Event-ID` to Lucina; `Content-Type`, `Content-Length`, `Content-Range`,
//! `Accept-Ranges`, `Content-Disposition` and `Cache-Control` back (`no-store` unless
//! Lucina says otherwise). Request bodies stream too, so an upload
//! (`POST /v1/notebooks/{nb}/sources/upload`, multipart) is never held in memory: it
//! may be [`UPLOAD_LIMIT`] long, every other body [`BODY_LIMIT`]. The limits are
//! enforced here (a `Content-Length` over the limit is refused at once, a chunked body
//! when it crosses it), because axum's `DefaultBodyLimit` does not apply to a raw body.
//!
//! At most [`STREAMS_PER_CALLER`] open event streams per caller and [`STREAMS_TOTAL`]
//! in all; each ends on shutdown or when Lucina goes quiet for [`READ_IDLE`].

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use futures_util::{Stream, StreamExt};

use crate::agents::{bearer, mcp_name, Caller, ORIGIN_HEADER};
use crate::auth::{Identity, Via, CSRF_HEADER};
use crate::devices::TOKEN_PREFIX;
use crate::error::ApiError;
use crate::timeline::StreamSlots;
use crate::{AppState, DeviceId};

/// JSON bodies (a pasted text source can be a few MB).
pub const BODY_LIMIT: usize = 8 * 1024 * 1024;
/// A multipart source upload.
pub const UPLOAD_LIMIT: usize = 200 * 1024 * 1024;
pub const STREAMS_PER_CALLER: usize = 4;
pub const STREAMS_TOTAL: usize = 20;
/// Longest silence from Lucina: a chat answer that is not streamed arrives in one piece.
pub const READ_IDLE: Duration = Duration::from_secs(300);
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// A chat answer that is not streamed.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(300);
/// The whole upload, over the Link.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const PASS: [HeaderName; 4] = [
    header::CONTENT_TYPE,
    header::ACCEPT,
    header::RANGE,
    HeaderName::from_static("last-event-id"),
];
const PASS_BACK: [HeaderName; 5] = [
    header::CONTENT_LENGTH,
    header::CONTENT_RANGE,
    header::ACCEPT_RANGES,
    header::CONTENT_DISPOSITION,
    header::CACHE_CONTROL,
];

/// The way to Lucina's notebooks.
pub struct Notebooks {
    http: reqwest::Client,
    base: String,
    auth: String,
    /// `SECRETARY_MCP_DEVICE_IDS`: devices whose token the MCP holds.
    mcp_devices: Vec<String>,
    streams: StreamSlots,
}

impl Notebooks {
    pub fn new(
        lucina_url: &str,
        lucina_token: &str,
        mcp_devices: Vec<String>,
    ) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("secretary-server/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .read_timeout(READ_IDLE)
            .no_gzip()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            http,
            base: lucina_url.trim_end_matches('/').to_owned(),
            auth: format!("Bearer {lucina_token}"),
            mcp_devices,
            streams: StreamSlots::new(STREAMS_PER_CALLER, STREAMS_TOTAL),
        })
    }
}

/// The caller from the request's credentials (module docs).
pub fn authenticate(
    st: &AppState,
    mcp_devices: &[String],
    method: &Method,
    headers: &HeaderMap,
) -> Result<Caller, ApiError> {
    let ident: Identity = crate::auth::identify(st, headers)?.map_err(ApiError::unauthorized)?;
    if ident.via == Via::Bearer && !bearer(headers).is_some_and(|t| t.starts_with(TOKEN_PREFIX)) {
        return Err(ApiError::unauthorized("missing or malformed device token"));
    }
    let writes = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS);
    if ident.via == Via::Cookie
        && writes
        && headers
            .get(CSRF_HEADER)
            .is_none_or(|v| v.as_bytes() != b"1")
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "csrf",
            "cookie-authenticated requests that change state need X-Secretary-CSRF: 1",
        ));
    }
    if let Some(name) = mcp_name(headers) {
        return Ok(Caller::Mcp(name));
    }
    if mcp_devices.iter().any(|d| d == &ident.device_id) {
        return Ok(Caller::Mcp("device".into()));
    }
    Ok(Caller::Device(ident.device_id))
}

/// The part after `/api/secretary/notebooks` for a `/v1/notebooks…` path (empty for the
/// root), or `None` for a bad one: no empty, `.` or `..` segments and only URL-safe characters.
pub fn upstream_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/v1/notebooks")?;
    if rest.is_empty() || rest == "/" {
        return Some(String::new());
    }
    let segs = rest.strip_prefix('/')?;
    let ok = rest.len() <= 512
        && segs.split('/').all(|seg| {
            !seg.is_empty()
                && seg != "."
                && seg != ".."
                && seg
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._~%:@".contains(&b))
        });
    ok.then(|| rest.to_owned())
}

/// `POST /{nb}/sources/upload`.
fn is_upload(method: &Method, rest: &str) -> bool {
    *method == Method::POST
        && matches!(
            rest.trim_start_matches('/')
                .split('/')
                .collect::<Vec<_>>()
                .as_slice(),
            [_, "sources", "upload"]
        )
}

/// An SSE answer is asked for.
fn wants_events(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/event-stream"))
}

/// How long the whole call may take; `None` = until it ends (event streams, and audio and
/// files, which only the idle timeout guards).
fn timeout_for(method: &Method, rest: &str, events: bool) -> Option<Duration> {
    if events || (*method == Method::GET && (rest.ends_with("/audio") || rest.ends_with("/file"))) {
        return None;
    }
    Some(if is_upload(method, rest) {
        UPLOAD_TIMEOUT
    } else if *method == Method::POST && rest.ends_with("/messages") {
        ANSWER_TIMEOUT
    } else {
        DEFAULT_TIMEOUT
    })
}

/// The request body as it goes to Lucina, failing (and saying so in `exceeded`) once it is
/// longer than `limit`.
fn limited<S>(
    body: S,
    limit: usize,
    exceeded: Arc<AtomicBool>,
) -> impl Stream<Item = Result<Bytes, io::Error>>
where
    S: Stream<Item = Result<Bytes, axum::Error>>,
{
    let mut seen = 0usize;
    body.map(move |chunk| {
        let chunk = chunk.map_err(io::Error::other)?;
        seen += chunk.len();
        if seen > limit {
            exceeded.store(true, Ordering::SeqCst);
            return Err(io::Error::other("request body too large"));
        }
        Ok(chunk)
    })
}

fn too_large(limit: usize) -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
        format!(
            "the request body is limited to {} MB here",
            limit / (1024 * 1024)
        ),
    )
}

fn upstream_error(e: &reqwest::Error) -> ApiError {
    tracing::warn!(timeout = e.is_timeout(), "notebooks: Lucina unreachable");
    if e.is_timeout() {
        ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "upstream_timeout",
            "Lucina did not answer in time",
        )
    } else {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "lucina_unreachable",
            "Lucina is unreachable",
        )
    }
}

async fn handle(
    State(st): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let Some(nb) = st.notebooks.as_ref() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "lucina_not_configured",
            "SECRETARY_LUCINA_TOKEN_FILE is not set on the server",
        ));
    };
    let caller = authenticate(&st, &nb.mcp_devices, &method, &headers)?;
    let rest = upstream_path(uri.path()).ok_or_else(ApiError::not_found)?;
    st.limiter
        .check(&caller.key())
        .map_err(ApiError::rate_limited)?;

    let limit = if is_upload(&method, &rest) {
        UPLOAD_LIMIT
    } else {
        BODY_LIMIT
    };
    let sends_body = matches!(method, Method::POST | Method::PUT | Method::PATCH);
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if sends_body && declared.is_some_and(|n| n > limit as u64) {
        return Err(too_large(limit));
    }
    let events = wants_events(&headers);
    let slot = match events {
        true => Some(nb.streams.try_acquire(&caller.key()).ok_or_else(|| {
            let mut e = ApiError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_streams",
                "too many open notebook streams; close one first",
            );
            e.retry_after = Some(5);
            e
        })?),
        false => None,
    };

    let mut url = format!("{}/api/secretary/notebooks{rest}", nb.base);
    if let Some(q) = uri.query().filter(|q| !q.is_empty()) {
        url.push('?');
        url.push_str(q);
    }
    let mut req = nb
        .http
        .request(method.clone(), &url)
        .header(header::AUTHORIZATION, &nb.auth)
        .header(ORIGIN_HEADER, caller.origin());
    for name in &PASS {
        if let Some(v) = headers.get(name) {
            req = req.header(name, v);
        }
    }
    if let Some(t) = timeout_for(&method, &rest, events) {
        req = req.timeout(t);
    }
    let exceeded = Arc::new(AtomicBool::new(false));
    if sends_body {
        if let Some(n) = declared {
            req = req.header(header::CONTENT_LENGTH, n);
        }
        req = req.body(reqwest::Body::wrap_stream(limited(
            body.into_data_stream(),
            limit,
            exceeded.clone(),
        )));
    }
    let up = match req.send().await {
        Ok(up) => up,
        Err(_) if exceeded.load(Ordering::SeqCst) => return Err(too_large(limit)),
        Err(e) => return Err(upstream_error(&e)),
    };
    let status = up.status();
    if status == StatusCode::UNAUTHORIZED {
        // Lucina refused this server's token: not the client's fault.
        tracing::error!("notebooks: Lucina refused the server's token");
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "lucina_auth",
            "Lucina refused the server's credentials",
        ));
    }
    let content_type = up.headers().get(header::CONTENT_TYPE).cloned();
    let back: Vec<(HeaderName, HeaderValue)> = PASS_BACK
        .iter()
        .filter_map(|n| Some((n.clone(), up.headers().get(n)?.clone())))
        .collect();
    let mut resp = if events && status.is_success() {
        let mut shutdown = st.shutdown.subscribe();
        let body = up
            .bytes_stream()
            .take_while(|chunk| std::future::ready(chunk.is_ok()))
            .take_until(async move {
                let _ = shutdown.wait_for(|stop| *stop).await;
            })
            .map(move |chunk| {
                let _held = &slot;
                chunk
            });
        let mut resp = Body::from_stream(body).into_response();
        resp.headers_mut().insert(
            HeaderName::from_static("x-accel-buffering"),
            HeaderValue::from_static("no"),
        );
        resp
    } else {
        Body::from_stream(up.bytes_stream()).into_response()
    };
    *resp.status_mut() = status;
    if let Some(ct) = content_type {
        resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    for (n, v) in back {
        resp.headers_mut().insert(n, v);
    }
    resp.extensions_mut().insert(DeviceId(caller.key()));
    Ok(resp)
}

/// The routes, with their own authentication (module docs). No `DefaultBodyLimit` here:
/// the handler takes the raw body and enforces [`BODY_LIMIT`] and [`UPLOAD_LIMIT`] itself.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/notebooks", any(handle))
        .route("/v1/notebooks/", any(handle))
        .route("/v1/notebooks/{*rest}", any(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_the_contract() {
        for (public, rest) in [
            ("/v1/notebooks", ""),
            ("/v1/notebooks/", ""),
            ("/v1/notebooks/nb4f1c09a2e7", "/nb4f1c09a2e7"),
            (
                "/v1/notebooks/nb4f1c09a2e7/sources",
                "/nb4f1c09a2e7/sources",
            ),
            (
                "/v1/notebooks/nb4f1c09a2e7/sources/upload",
                "/nb4f1c09a2e7/sources/upload",
            ),
            (
                "/v1/notebooks/sources/s0a9be3c112/chunks",
                "/sources/s0a9be3c112/chunks",
            ),
            (
                "/v1/notebooks/chats/c0123456789/messages/m0123456789/note",
                "/chats/c0123456789/messages/m0123456789/note",
            ),
            (
                "/v1/notebooks/nb1/notes/rm-class-2-summary",
                "/nb1/notes/rm-class-2-summary",
            ),
            (
                "/v1/notebooks/transformations/t-dense-summary",
                "/transformations/t-dense-summary",
            ),
            (
                "/v1/notebooks/podcasts/p0123456789/audio",
                "/podcasts/p0123456789/audio",
            ),
            ("/v1/notebooks/search", "/search"),
            ("/v1/notebooks/health", "/health"),
        ] {
            assert_eq!(upstream_path(public).as_deref(), Some(rest), "{public}");
        }
        for bad in [
            "/v1/notebooksx",
            "/v1/notebooks//x",
            "/v1/notebooks/nb1/",
            "/v1/notebooks/../x",
            "/v1/notebooks/nb1/./sources",
            "/v1/notebooks/nb1/a b",
            "/v1/notebooks/nb1/a$b",
            "/v1/notebooks/nb1/a,b",
            "/v1/agents",
        ] {
            assert_eq!(upstream_path(bad), None, "{bad}");
        }
        assert_eq!(
            upstream_path(&format!("/v1/notebooks/{}", "a".repeat(600))),
            None,
            "too long"
        );
    }

    #[test]
    fn uploads_and_timeouts() {
        assert!(is_upload(&Method::POST, "/nb1/sources/upload"));
        for (m, p) in [
            (Method::GET, "/nb1/sources/upload"),
            (Method::POST, "/nb1/sources"),
            (Method::POST, "/sources/upload"),
            (Method::POST, "/nb1/sources/upload/x"),
        ] {
            assert!(!is_upload(&m, p), "{m} {p}");
        }
        assert_eq!(
            timeout_for(&Method::POST, "/nb1/sources/upload", false),
            Some(UPLOAD_TIMEOUT)
        );
        assert_eq!(
            timeout_for(&Method::POST, "/chats/c1/messages", false),
            Some(ANSWER_TIMEOUT)
        );
        assert_eq!(timeout_for(&Method::POST, "/chats/c1/messages", true), None);
        assert_eq!(timeout_for(&Method::GET, "/podcasts/p1/audio", false), None);
        assert_eq!(timeout_for(&Method::GET, "/sources/s1/file", false), None);
        assert_eq!(
            timeout_for(&Method::GET, "/nb1", false),
            Some(DEFAULT_TIMEOUT)
        );
        assert_eq!(
            timeout_for(&Method::DELETE, "/podcasts/p1/audio", false),
            Some(DEFAULT_TIMEOUT)
        );
        let mut h = HeaderMap::new();
        assert!(!wants_events(&h));
        h.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
        assert!(wants_events(&h));
        h.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        assert!(!wants_events(&h));
    }

    #[tokio::test]
    async fn the_body_limit_stops_a_chunked_body() {
        let chunks = |n: usize| {
            futures_util::stream::iter(
                (0..n).map(|_| Ok::<_, axum::Error>(Bytes::from_static(&[7u8; 10]))),
            )
        };
        let flag = Arc::new(AtomicBool::new(false));
        let all: Vec<_> = limited(chunks(3), 30, flag.clone()).collect().await;
        assert!(
            all.iter().all(Result::is_ok) && all.len() == 3 && !flag.load(Ordering::SeqCst),
            "exactly the limit passes"
        );
        let all: Vec<_> = limited(chunks(4), 30, flag.clone()).collect().await;
        assert!(
            all[3].is_err() && flag.load(Ordering::SeqCst),
            "one byte over fails the stream"
        );
    }
}
