//! `/v1/instagram/*`. p1's Caddy authenticates the owner (forward_auth `/v1/auth/owner`) and adds
//! `X-Lucina-Gateway: <secret>`; every route but the signed media URLs refuses a request without
//! it. Media URLs are `/v1/instagram/s/<exp>/<sig>/<token>` (HMAC, about six hours): the apps'
//! image and video loaders need no header and never contact Instagram.
//!
//! Logs carry the route template and status only: never usernames, cursors, ids or captions.

use std::io::SeekFrom;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::media::PREFIX;
use crate::service::{MediaFile, SaveItem, Service, ServiceError, SignedError};

pub struct Http {
    pub service: Arc<Service>,
    pub gateway_secret: Vec<u8>,
}

type St = Arc<Http>;

fn no_store(mut r: Response) -> Response {
    let h = r.headers_mut();
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    h.insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    r
}

fn ok(v: Value) -> Response {
    no_store(Json(json!({"ok": true, "data": v})).into_response())
}

fn err(status: StatusCode, code: &str) -> Response {
    no_store((status, Json(json!({"ok": false, "error": code}))).into_response())
}

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        match self {
            ServiceError::NotFound => err(StatusCode::NOT_FOUND, "not_found"),
            ServiceError::BadRequest(_) => err(StatusCode::BAD_REQUEST, "bad_request"),
            ServiceError::Forbidden => err(StatusCode::FORBIDDEN, "forbidden"),
            ServiceError::Gone => err(StatusCode::GONE, "media_expired"),
            ServiceError::Upstream => err(StatusCode::BAD_GATEWAY, "upstream_unavailable"),
            ServiceError::RateLimited(secs) => {
                let mut r = no_store(
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        Json(json!({"ok": false, "error": "rate_limited", "retryAfter": secs})),
                    )
                        .into_response(),
                );
                if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
                    r.headers_mut().insert(header::RETRY_AFTER, v);
                }
                r
            }
        }
    }
}

async fn gateway(State(st): State<St>, req: Request, next: Next) -> Response {
    let provided = req
        .headers()
        .get("x-lucina-gateway")
        .map(|v| v.as_bytes().to_vec())
        .unwrap_or_default();
    let good = !st.gateway_secret.is_empty()
        && provided.len() == st.gateway_secret.len()
        && bool::from(provided.ct_eq(&st.gateway_secret));
    if !good {
        return err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    next.run(req).await
}

async fn access_log(req: Request, next: Next) -> Response {
    let route = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "<unmatched>".into());
    let method = req.method().clone();
    let resp = next.run(req).await;
    tracing::info!(method = %method, route = %route, status = resp.status().as_u16(), "request");
    resp
}

pub fn router(st: St) -> Router {
    let api = Router::new()
        .route("/status", get(status))
        .route("/profile/{username}", get(profile))
        .route("/posts/{username}", get(posts))
        .route("/reels/{username}", get(reels))
        .route("/stories/{username}", get(stories))
        .route("/highlights/items", get(highlight_items))
        .route("/highlights/{username}", get(highlights))
        .route("/resolve", get(resolve))
        .route("/recent", get(recent))
        .route("/save", post(save))
        .route("/watch", get(watch_list))
        .route("/watch/{username}", put(watch_put).delete(watch_delete))
        .route("/watch/{username}/new", get(watch_new))
        .route("/watch/{username}/opened", post(watch_opened))
        .route("/badges", get(badges))
        .route("/archive/{username}", get(archive))
        .route_layer(middleware::from_fn_with_state(st.clone(), gateway));
    let signed = Router::new()
        .route("/s/{exp}/{sig}/{token}", get(media))
        .route("/a/{exp}/{sig}/{id}", get(archived));
    Router::new()
        .nest(PREFIX, api.merge(signed))
        .layer(middleware::from_fn(access_log))
        .with_state(st)
}

async fn status() -> Response {
    ok(json!({"enabled": true, "rateLimitPerSecond": 1}))
}

#[derive(Deserialize)]
struct CursorQ {
    #[serde(default)]
    cursor: Option<String>,
}

async fn profile(State(st): State<St>, Path(username): Path<String>) -> Response {
    match st.service.profile(&username).await {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

async fn posts(
    State(st): State<St>,
    Path(username): Path<String>,
    Query(q): Query<CursorQ>,
) -> Response {
    match st.service.posts(&username, &q.cursor).await {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

async fn reels(
    State(st): State<St>,
    Path(username): Path<String>,
    Query(q): Query<CursorQ>,
) -> Response {
    match st.service.reels(&username, &q.cursor).await {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

async fn stories(
    State(st): State<St>,
    Path(username): Path<String>,
    Query(q): Query<CursorQ>,
) -> Response {
    match st.service.stories(&username, &q.cursor).await {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

async fn highlights(State(st): State<St>, Path(username): Path<String>) -> Response {
    match st.service.highlights(&username).await {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
struct HighlightQ {
    #[serde(default)]
    id: String,
    #[serde(default)]
    cursor: Option<String>,
}

async fn highlight_items(State(st): State<St>, Query(q): Query<HighlightQ>) -> Response {
    match st.service.highlight_items(&q.id, &q.cursor).await {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
struct ResolveQ {
    #[serde(default)]
    url: String,
}

async fn resolve(State(st): State<St>, Query(q): Query<ResolveQ>) -> Response {
    match st.service.resolve(&q.url).await {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

async fn recent(State(st): State<St>) -> Response {
    ok(st.service.recent_json())
}

#[derive(Deserialize)]
struct SaveBody {
    items: Vec<SaveItem>,
}

async fn save(State(st): State<St>, body: Bytes) -> Response {
    let Ok(b) = serde_json::from_slice::<SaveBody>(&body) else {
        return err(StatusCode::BAD_REQUEST, "bad_request");
    };
    match st.service.save(b.items).await {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

async fn watch_list(State(st): State<St>) -> Response {
    ok(st.service.watch_list())
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct WatchBody {
    #[serde(default)]
    auto_save: Option<bool>,
}

async fn watch_put(State(st): State<St>, Path(username): Path<String>, body: Bytes) -> Response {
    let b: WatchBody = if body.iter().all(u8::is_ascii_whitespace) {
        WatchBody::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return err(StatusCode::BAD_REQUEST, "bad_request"),
        }
    };
    match st.service.watch_put(&username, b.auto_save) {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

async fn watch_delete(State(st): State<St>, Path(username): Path<String>) -> Response {
    match st.service.watch_delete(&username) {
        Ok(()) => ok(json!({"removed": true})),
        Err(e) => e.into_response(),
    }
}

async fn watch_new(State(st): State<St>, Path(username): Path<String>) -> Response {
    match st.service.watch_new(&username) {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

async fn watch_opened(State(st): State<St>, Path(username): Path<String>) -> Response {
    match st.service.watch_opened(&username) {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

async fn badges(State(st): State<St>) -> Response {
    ok(st.service.badges_json())
}

async fn archive(
    State(st): State<St>,
    Path(username): Path<String>,
    Query(q): Query<CursorQ>,
) -> Response {
    match st.service.archive_json(&username, &q.cursor) {
        Ok(v) => ok(v),
        Err(e) => e.into_response(),
    }
}

/// `bytes=a-b`, `bytes=a-`, `bytes=-n` against a file of `len` bytes.
enum Sel {
    Full,
    Part(u64, u64),
    Bad,
}

fn parse_range(h: Option<&str>, len: u64) -> Sel {
    let Some(h) = h else { return Sel::Full };
    let Some(spec) = h.trim().strip_prefix("bytes=") else {
        return Sel::Full;
    };
    if spec.contains(',') || len == 0 {
        return Sel::Bad;
    }
    let Some((a, b)) = spec.split_once('-') else {
        return Sel::Bad;
    };
    let (start, end) = match (a.trim(), b.trim()) {
        ("", n) => {
            let Ok(n) = n.parse::<u64>() else {
                return Sel::Bad;
            };
            if n == 0 {
                return Sel::Bad;
            }
            (len.saturating_sub(n), len - 1)
        }
        (s, "") => {
            let Ok(s) = s.parse::<u64>() else {
                return Sel::Bad;
            };
            (s, len - 1)
        }
        (s, e) => {
            let (Ok(s), Ok(e)) = (s.parse::<u64>(), e.parse::<u64>()) else {
                return Sel::Bad;
            };
            (s, e.min(len - 1))
        }
    };
    if start > end || start >= len {
        return Sel::Bad;
    }
    Sel::Part(start, end)
}

async fn media(
    State(st): State<St>,
    Path((exp, sig, token)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let Ok(exp) = exp.parse::<i64>() else {
        return err(StatusCode::FORBIDDEN, "forbidden");
    };
    let file = match st.service.signed_media(exp, &sig, &token).await {
        Ok(f) => f,
        Err(SignedError::Sign(_)) => return err(StatusCode::FORBIDDEN, "forbidden"),
        Err(SignedError::Service(e)) => return e.into_response(),
    };
    serve_file(file, &headers).await
}

async fn archived(
    State(st): State<St>,
    Path((exp, sig, id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let Ok(exp) = exp.parse::<i64>() else {
        return err(StatusCode::FORBIDDEN, "forbidden");
    };
    match st.service.archive_file(exp, &sig, &id).await {
        Ok(f) => serve_file(f, &headers).await,
        Err(SignedError::Sign(_)) => err(StatusCode::FORBIDDEN, "forbidden"),
        Err(SignedError::Service(e)) => e.into_response(),
    }
}

/// A file with Range support (a browser's video player asks for pieces).
async fn serve_file(file: MediaFile, headers: &HeaderMap) -> Response {
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let (status, start, end) = match parse_range(range, file.len) {
        Sel::Full => (StatusCode::OK, 0, file.len.saturating_sub(1)),
        Sel::Part(s, e) => (StatusCode::PARTIAL_CONTENT, s, e),
        Sel::Bad => {
            let mut r = err(StatusCode::RANGE_NOT_SATISFIABLE, "range_not_satisfiable");
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{}", file.len)) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            return r;
        }
    };
    let n = if file.len == 0 { 0 } else { end - start + 1 };
    let Ok(mut f) = tokio::fs::File::open(&file.path).await else {
        return err(StatusCode::BAD_GATEWAY, "upstream_unavailable");
    };
    if start > 0 && f.seek(SeekFrom::Start(start)).await.is_err() {
        return err(StatusCode::BAD_GATEWAY, "upstream_unavailable");
    }
    let body = Body::from_stream(ReaderStream::new(f.take(n)));
    let mut r = Response::new(body);
    *r.status_mut() = status;
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(file.ctype));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(n));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if status == StatusCode::PARTIAL_CONTENT {
        if let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{end}/{}", file.len)) {
            h.insert(header::CONTENT_RANGE, v);
        }
    }
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        "x-robots-tag",
        HeaderValue::from_static("noindex, nofollow"),
    );
    // The URL is signed and expires on its own, so the browser may keep the bytes meanwhile.
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=21600"),
    );
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert!(matches!(parse_range(None, 100), Sel::Full));
        assert!(matches!(
            parse_range(Some("bytes=0-9"), 100),
            Sel::Part(0, 9)
        ));
        assert!(matches!(
            parse_range(Some("bytes=90-"), 100),
            Sel::Part(90, 99)
        ));
        assert!(matches!(
            parse_range(Some("bytes=-10"), 100),
            Sel::Part(90, 99)
        ));
        assert!(matches!(
            parse_range(Some("bytes=50-500"), 100),
            Sel::Part(50, 99)
        ));
        assert!(matches!(parse_range(Some("bytes=100-"), 100), Sel::Bad));
        assert!(matches!(parse_range(Some("bytes=5-2"), 100), Sel::Bad));
        assert!(matches!(parse_range(Some("bytes=0-1,5-6"), 100), Sel::Bad));
        assert!(matches!(parse_range(Some("items=0-1"), 100), Sel::Full));
    }
}
