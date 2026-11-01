//! `/v1/pixiv/*`. p1's Caddy authenticates the owner (forward_auth `/v1/auth/owner`) and adds
//! `X-Lucina-Gateway: <secret>`; every route but the signed image URLs refuses a request without
//! it. Image URLs are `/v1/pixiv/s/<expiry>/<token>/<sig>` (HMAC, 30 days, the expiry rounded to the
//! day; the token is the i.pximg.net URL itself, in lowercase hex), so the apps' image loaders need no header and never contact pixiv.
//!
//! Adult content: `adult=1` (query) switches R-18 on for that request; without it R-18 modes and
//! works answer 403 `adult_off` and R-18 items never appear in a list. Lists take a content
//! filter (`safe|all|r18`: search `mode`, bookmarks `content`).
//!
//! Logs carry the route template and status only: never queries, ids, titles or tags.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;

use crate::error::SvcError;
use crate::images::{allowed, grid_thumb};
use crate::model::{Content, Illust, Listing, Summary};
use crate::service::{SearchParams, Service};
use crate::store::Store;

pub const PREFIX: &str = "/v1/pixiv";
/// Longer than the apps keep a cached page (7 days, web lib/pageCache), so a page opened from its copy never shows
/// expired links; an older copy re-signs them with `POST /resign`.
const URL_LIFETIME: Duration = Duration::from_secs(30 * 86_400);
/// At most this many links re-signed per request.
const RESIGN_MAX: usize = 300;

pub struct Http {
    pub service: Service,
    pub store: Store,
    pub gateway_secret: Vec<u8>,
    pub signing_key: Vec<u8>,
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

impl IntoResponse for SvcError {
    fn into_response(self) -> Response {
        match self {
            SvcError::NotFound => err(StatusCode::NOT_FOUND, "not_found"),
            SvcError::BadRequest(_) => err(StatusCode::BAD_REQUEST, "bad_request"),
            SvcError::AdultOff => err(StatusCode::FORBIDDEN, "adult_off"),
            SvcError::RateLimited(s) => {
                let mut r = no_store(
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        Json(json!({"ok": false, "error": "rate_limited", "retryAfter": s})),
                    )
                        .into_response(),
                );
                if let Ok(v) = HeaderValue::from_str(&s.to_string()) {
                    r.headers_mut().insert(header::RETRY_AFTER, v);
                }
                r
            }
            SvcError::Upstream => err(StatusCode::BAD_GATEWAY, "upstream_unavailable"),
        }
    }
}

fn sign(key: &[u8], msg: &str) -> String {
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).expect("hmac key");
    mac.update(msg.as_bytes());
    hex::encode(&mac.finalize().into_bytes()[..16])
}

impl Http {
    /// Signs any URL, allowed or not. The route refuses what is not allowed; this exists so that
    /// the tests can prove it.
    pub fn sign_unchecked(&self, upstream: &str) -> String {
        let exp = chrono::Utc::now().timestamp() + URL_LIFETIME.as_secs() as i64;
        // Round the expiry to the day so the URLs stay stable (client image caches).
        self.sign_at(upstream, exp - exp % 86_400 + 86_400)
    }

    /// Same, with an explicit expiry (tests use a past one).
    pub fn sign_at(&self, upstream: &str, exp: i64) -> String {
        // Lowercase hex, not base64: mixed-case base64 reads as a random secret to Lucina's
        // redactor, which would mask the link when she shows it in chat.
        let token = hex::encode(upstream.as_bytes());
        let msg = format!("{exp}/{token}");
        format!("{PREFIX}/s/{msg}/{}", sign(&self.signing_key, &msg))
    }

    /// The signed route for an i.pximg.net URL; empty for anything else (and for no URL).
    pub fn signed_url(&self, upstream: &str) -> String {
        if allowed(upstream).is_none() {
            return String::new();
        }
        self.sign_unchecked(upstream)
    }

    fn summary_json(&self, s: &Summary) -> Value {
        json!({
            "id": s.id,
            "title": s.title,
            "author": s.author,
            "authorId": s.author_id,
            "tags": s.tags,
            "thumbUrl": self.signed_url(&grid_thumb(&s.thumb)),
            "pages": s.pages,
            "r18": s.r18,
            "popularTier": s.popular_tier(),
            "bookmarked": self.store.contains(&s.id),
        })
    }

    fn listing_json(&self, l: &Listing) -> Value {
        json!({
            "items": l.items.iter().map(|s| self.summary_json(s)).collect::<Vec<_>>(),
            "page": l.page,
            "hasMore": l.has_more,
            "total": l.total,
            "numPages": l.num_pages,
            "notice": l.notice,
            "sort": l.sort,
        })
    }

    fn illust_json(&self, i: &Illust) -> Value {
        json!({
            "id": i.id,
            "title": i.title,
            "author": i.author,
            "authorId": i.author_id,
            "created": i.created,
            "kind": match i.kind { 1 => "manga", 2 => "ugoira", _ => "illust" },
            "ugoira": i.kind == 2,
            "r18": i.r18,
            "description": i.description,
            "stats": {
                "views": i.stats.views,
                "likes": i.stats.likes,
                "bookmarks": i.stats.bookmarks,
                "comments": i.stats.comments,
            },
            "pageCount": i.page_count,
            "coverUrl": self.signed_url(&i.cover),
            "tags": i.tags,
            "pages": i.pages.iter().enumerate().map(|(n, p)| json!({
                "index": n,
                "thumbUrl": self.signed_url(&grid_thumb(&p.thumb)),
                "url": self.signed_url(&p.regular),
                "originalUrl": self.signed_url(&p.orig),
            })).collect::<Vec<_>>(),
            "bookmarked": self.store.contains(&i.id),
        })
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

pub fn router(st: St) -> Router {
    let api = Router::new()
        .route("/status", get(status))
        .route("/connect/start", post(connect_start))
        .route("/connect/finish", post(connect_finish))
        .route("/disconnect", post(disconnect))
        .route("/settings", get(settings).put(update_settings))
        .route("/resign", post(resign))
        .route("/ranking", get(ranking))
        .route("/search", get(search))
        .route("/illust/{id}", get(illust))
        .route("/bookmarks", get(bookmarks))
        .route("/bookmarks/{id}", put(add_bookmark).delete(remove_bookmark))
        .route("/save", axum::routing::post(save))
        .route_layer(middleware::from_fn_with_state(st.clone(), gateway));
    let signed = Router::new().route("/s/{exp}/{token}/{sig}", get(image));
    Router::new()
        .nest(PREFIX, api.merge(signed))
        .layer(middleware::from_fn(access_log))
        .with_state(st)
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

fn truthy(v: &Option<String>) -> bool {
    matches!(v.as_deref(), Some("1" | "true"))
}

fn page_of(v: &Option<String>) -> Result<u32, SvcError> {
    match v.as_deref() {
        None | Some("") => Ok(1),
        Some(s) => s.parse::<u32>().map_err(|_| SvcError::BadRequest("page")),
    }
}

async fn status(State(st): State<St>) -> Response {
    let account = st.service.account().await;
    ok(json!({
        "enabled": true,
        "account": account,
        "connection": st.service.connection().map(|c| c.json()).unwrap_or_else(|| crate::connection::Connection::new(Default::default()).json()),
        "settings": st.store.settings(),
        "cacheBytes": st.service.cache.total_bytes(),
        "cacheMaxBytes": st.service.cache.max_bytes(),
        "apiIntervalMs": st.service.config().gate.interval.as_millis() as u64,
    }))
}

fn connection_error(e: crate::connection::ConnectError) -> Response {
    err(StatusCode::from_u16(e.0).unwrap(), e.1)
}
async fn connect_start(State(st): State<St>) -> Response {
    match st.service.connection() {
        Some(c) => match c.start() {
            Ok(v) => ok(v),
            Err(e) => connection_error(e),
        },
        None => err(StatusCode::SERVICE_UNAVAILABLE, "broker_unconfigured"),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Finish {
    state: String,
    code: String,
}
fn url_safe(s: &str) -> bool {
    (1..=512).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'))
}
async fn connect_finish(
    State(st): State<St>,
    body: Result<Json<Finish>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(f)) = body else {
        return err(StatusCode::BAD_REQUEST, "bad_request");
    };
    if !url_safe(&f.state) || !url_safe(&f.code) {
        return err(StatusCode::BAD_REQUEST, "bad_request");
    }
    match st.service.connection() {
        Some(c) => {
            let result = c.finish(&f.state, &f.code).await;
            st.service.reset_account().await;
            match result {
                Ok(v) => ok(v),
                Err(e) => connection_error(e),
            }
        }
        None => err(StatusCode::BAD_REQUEST, "connect_expired"),
    }
}
async fn disconnect(State(st): State<St>) -> Response {
    let v = st
        .service
        .connection()
        .map(|c| c.disconnect())
        .unwrap_or_else(|| crate::connection::Connection::new(Default::default()).disconnect());
    st.service.reset_account().await;
    ok(v)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resign {
    tokens: Vec<String>,
}

/// `POST /resign {tokens: [hex]}`: fresh signed URLs for the tokens of old ones (a page cached in an app holds links
/// that may have expired). Answers `{urls: {token: url}}`; a token that is not an i.pximg.net URL gets `""`.
async fn resign(
    State(st): State<St>,
    body: Result<Json<Resign>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(r)) = body else {
        return err(StatusCode::BAD_REQUEST, "bad_request");
    };
    if r.tokens.len() > RESIGN_MAX || r.tokens.iter().any(|t| t.len() > 4096) {
        return err(StatusCode::BAD_REQUEST, "bad_request");
    }
    let urls: serde_json::Map<String, Value> = r
        .tokens
        .into_iter()
        .map(|t| {
            let url = hex::decode(t.as_bytes())
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .map(|u| st.signed_url(&u))
                .unwrap_or_default();
            (t, Value::String(url))
        })
        .collect();
    ok(json!({ "urls": urls }))
}

async fn settings(State(st): State<St>) -> Response {
    ok(json!(st.store.settings()))
}
async fn update_settings(
    State(st): State<St>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(patch)) = body else {
        return err(StatusCode::BAD_REQUEST, "bad_request");
    };
    match st.store.update_settings(&patch) {
        Ok(settings) => ok(json!(settings)),
        Err(crate::store::SettingsError::Invalid) => err(StatusCode::BAD_REQUEST, "bad_request"),
        Err(crate::store::SettingsError::Io) => {
            err(StatusCode::INTERNAL_SERVER_ERROR, "settings_failed")
        }
    }
}

#[derive(Deserialize)]
struct RankingQ {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    page: Option<String>,
    #[serde(default)]
    adult: Option<String>,
}

async fn ranking(State(st): State<St>, Query(q): Query<RankingQ>) -> Response {
    let page = match page_of(&q.page) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let mode = q.mode.as_deref().unwrap_or("day");
    match st.service.ranking(mode, page, truthy(&q.adult)).await {
        Ok(l) => ok(st.listing_json(&l)),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
struct SearchQ {
    #[serde(default)]
    q: String,
    #[serde(default)]
    order: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    page: Option<String>,
    #[serde(default)]
    adult: Option<String>,
}

async fn search(State(st): State<St>, Query(q): Query<SearchQ>) -> Response {
    let page = match page_of(&q.page) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let params = SearchParams {
        q: q.q,
        order: q.order.unwrap_or_else(|| "date_d".into()),
        mode: q.mode.unwrap_or_else(|| "safe".into()),
        kind: q.kind.unwrap_or_else(|| "all".into()),
        page,
    };
    match st.service.search(&params, truthy(&q.adult)).await {
        Ok(l) => ok(st.listing_json(&l)),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
struct AdultQ {
    #[serde(default)]
    adult: Option<String>,
}

async fn illust(State(st): State<St>, Path(id): Path<String>, Query(q): Query<AdultQ>) -> Response {
    match st.service.illust(&id, truthy(&q.adult)).await {
        Ok(i) => ok(st.illust_json(&i)),
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
struct BookmarksQ {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    adult: Option<String>,
}

async fn bookmarks(State(st): State<St>, Query(q): Query<BookmarksQ>) -> Response {
    let adult = truthy(&q.adult);
    let content = match q.content.as_deref() {
        None | Some("") => Some(if adult { Content::All } else { Content::Safe }),
        Some(c) => Content::parse(c),
    };
    let Some(content) = content else {
        return SvcError::BadRequest("content").into_response();
    };
    let Some(content) = content.gated(adult) else {
        return SvcError::AdultOff.into_response();
    };
    let items: Vec<Value> = st
        .store
        .list()
        .iter()
        .filter(|b| content.keeps(b.illust.r18))
        .map(|b| {
            let mut v = st.summary_json(&b.illust);
            v["addedAt"] = json!(b.added_at);
            v
        })
        .collect();
    ok(json!({"items": items}))
}

async fn add_bookmark(
    State(st): State<St>,
    Path(id): Path<String>,
    Query(q): Query<AdultQ>,
) -> Response {
    match st.service.illust(&id, truthy(&q.adult)).await {
        Ok(i) => {
            st.store.add(i.summary());
            ok(json!({"bookmarked": true}))
        }
        Err(e) => e.into_response(),
    }
}

async fn remove_bookmark(State(st): State<St>, Path(id): Path<String>) -> Response {
    st.store.remove(&id);
    ok(json!({"bookmarked": false}))
}

#[derive(Deserialize)]
struct SaveBody {
    id: Value,
    #[serde(default)]
    pages: Option<Vec<usize>>,
    #[serde(default)]
    adult: Option<bool>,
}

async fn save(State(st): State<St>, Query(q): Query<AdultQ>, body: axum::body::Bytes) -> Response {
    let Ok(b) = serde_json::from_slice::<SaveBody>(&body) else {
        return err(StatusCode::BAD_REQUEST, "bad_request");
    };
    let id = match &b.id {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return err(StatusCode::BAD_REQUEST, "bad_request"),
    };
    let adult = truthy(&q.adult) || b.adult.unwrap_or(false);
    match st.service.save(&id, b.pages, adult).await {
        Ok(out) => ok(serde_json::to_value(out).unwrap_or(Value::Null)),
        Err(e) => e.into_response(),
    }
}

async fn image(
    State(st): State<St>,
    Path((exp, token, sig)): Path<(i64, String, String)>,
) -> Response {
    let msg = format!("{exp}/{token}");
    let expected = sign(&st.signing_key, &msg);
    let valid =
        sig.len() == expected.len() && bool::from(sig.as_bytes().ct_eq(expected.as_bytes()));
    if !valid || exp < chrono::Utc::now().timestamp() {
        return err(StatusCode::FORBIDDEN, "forbidden");
    }
    let Some(url) = hex::decode(token.as_bytes())
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
    else {
        return err(StatusCode::FORBIDDEN, "forbidden");
    };
    // Defence in depth: a signature only ever vouches for an i.pximg.net URL.
    if allowed(&url).is_none() {
        return err(StatusCode::FORBIDDEN, "forbidden");
    }
    match st.service.image(&url).await {
        Ok((bytes, ctype)) => {
            let mut r = (StatusCode::OK, bytes).into_response();
            if let Ok(v) = HeaderValue::from_str(&ctype) {
                r.headers_mut().insert(header::CONTENT_TYPE, v);
            }
            let h = r.headers_mut();
            h.insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            // The bytes behind a signed URL never change: browsers keep them until the URL expires.
            let max_age = (exp - chrono::Utc::now().timestamp()).clamp(0, URL_LIFETIME.as_secs() as i64 + 86_400);
            if let Ok(v) = HeaderValue::from_str(&format!("private, max-age={max_age}, immutable")) {
                h.insert(header::CACHE_CONTROL, v);
            }
            h.insert(
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            );
            r
        }
        Err(e) => e.into_response(),
    }
}
