//! `/v1/nclient/*`. p1's Caddy authenticates the device (forward_auth, as for `/v1/media`) and
//! adds `X-Lucina-Gateway: <secret>`; every route but the signed image URLs refuses a request
//! without it. Image URLs are `/v1/nclient/s/<gallery>/<kind>/<index>/<expiry>/<sig>` (HMAC,
//! six hours), so the apps' image loaders need no header and never contact the site.
//!
//! Logs carry the route template and status only: never queries, titles, ids or tags.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;

use crate::service::{parse_sort, Detail, ImageKind, SearchParams, Service, ServiceError, Summary};
use crate::store::Store;

pub const PREFIX: &str = "/v1/nclient";
const URL_LIFETIME: Duration = Duration::from_secs(6 * 3600);

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

impl IntoResponse for ServiceError {
    fn into_response(self) -> Response {
        match self {
            ServiceError::NotFound => err(StatusCode::NOT_FOUND, "not_found"),
            ServiceError::BadRequest(_) => err(StatusCode::BAD_REQUEST, "bad_request"),
            ServiceError::Upstream(crate::upstream::UpstreamError::Status(429)) => {
                err(StatusCode::TOO_MANY_REQUESTS, "upstream_busy")
            }
            ServiceError::Upstream(_) | ServiceError::Parse => {
                err(StatusCode::BAD_GATEWAY, "upstream_unavailable")
            }
        }
    }
}

fn sign(key: &[u8], msg: &str) -> String {
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).expect("hmac key");
    mac.update(msg.as_bytes());
    hex::encode(&mac.finalize().into_bytes()[..16])
}

fn kind_parts(kind: ImageKind) -> (&'static str, usize) {
    match kind {
        ImageKind::Thumb => ("thumb", 0),
        ImageKind::Cover => ("cover", 0),
        ImageKind::Page(i) => ("page", i),
        ImageKind::PageThumb(i) => ("pthumb", i),
    }
}

fn parse_kind(kind: &str, i: usize) -> Option<ImageKind> {
    Some(match kind {
        "thumb" => ImageKind::Thumb,
        "cover" => ImageKind::Cover,
        "page" => ImageKind::Page(i),
        "pthumb" => ImageKind::PageThumb(i),
        _ => return None,
    })
}

impl Http {
    pub fn image_url(&self, id: i32, kind: ImageKind) -> String {
        let exp = chrono::Utc::now().timestamp() + URL_LIFETIME.as_secs() as i64;
        // Round the expiry to the hour so the URLs stay stable (client image caches).
        let exp = exp - exp % 3600 + 3600;
        let (k, i) = kind_parts(kind);
        let msg = format!("{id}/{k}/{i}/{exp}");
        format!("{PREFIX}/s/{msg}/{}", sign(&self.signing_key, &msg))
    }

    fn summary_json(&self, s: &Summary) -> Value {
        json!({
            "id": s.id,
            "title": s.title,
            "pages": s.pages,
            "language": s.language,
            "tags": s.tags,
            "thumbUrl": self.image_url(s.id, ImageKind::Thumb),
            "favorite": self.store.is_favorite(s.id),
            "progress": self.store.progress(s.id),
        })
    }

    fn detail_json(&self, d: &Detail) -> Value {
        json!({
            "id": d.id,
            "title": d.title,
            "titles": {"english": d.english, "japanese": d.japanese, "pretty": d.pretty},
            "tags": d.tags,
            "language": d.language,
            "uploaded": d.uploaded,
            "favorites": d.favorites,
            "coverUrl": self.image_url(d.id, ImageKind::Cover),
            "thumbUrl": self.image_url(d.id, ImageKind::Thumb),
            "pages": d.pages.iter().enumerate().map(|(i, p)| json!({
                "index": i,
                "width": p.width,
                "height": p.height,
                "url": self.image_url(d.id, ImageKind::Page(i)),
                "thumbUrl": self.image_url(d.id, ImageKind::PageThumb(i)),
            })).collect::<Vec<_>>(),
            "related": d.related.iter().map(|s| self.summary_json(s)).collect::<Vec<_>>(),
            "favorite": self.store.is_favorite(d.id),
            "progress": self.store.progress(d.id),
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
        .route("/search", get(search))
        .route("/random", get(random))
        .route("/galleries/{id}", get(gallery))
        .route("/galleries/{id}/related", get(related))
        .route("/tags", get(tags))
        .route("/favorites", get(favorites))
        .route(
            "/favorites/{id}",
            axum::routing::put(add_favorite).delete(remove_favorite),
        )
        .route("/progress", get(all_progress))
        .route("/progress/{id}", axum::routing::put(set_progress))
        .route_layer(middleware::from_fn_with_state(st.clone(), gateway));
    let signed = Router::new().route("/s/{id}/{kind}/{index}/{exp}/{sig}", get(image));
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

async fn status() -> Response {
    ok(json!({"enabled": true, "rateLimitPerSecond": 1}))
}

#[derive(Deserialize)]
struct SearchQ {
    #[serde(default)]
    q: String,
    #[serde(default)]
    page: Option<i32>,
    #[serde(default)]
    sort: Option<String>,
    /// comma-separated tag ids
    #[serde(default)]
    tags: Option<String>,
    #[serde(default)]
    exclude: Option<String>,
}

fn ids(s: &Option<String>) -> Result<Vec<i32>, ServiceError> {
    let Some(s) = s else { return Ok(vec![]) };
    let v: Result<Vec<i32>, _> = s
        .split(',')
        .filter(|x| !x.trim().is_empty())
        .map(|x| x.trim().parse::<i32>())
        .collect();
    let v = v.map_err(|_| ServiceError::BadRequest("tag ids"))?;
    if v.len() > 20 {
        return Err(ServiceError::BadRequest("too many tags"));
    }
    Ok(v)
}

async fn search(State(st): State<St>, Query(q): Query<SearchQ>) -> Response {
    let params = match (ids(&q.tags), ids(&q.exclude)) {
        (Ok(include), Ok(exclude)) => SearchParams {
            query: q.q,
            page: q.page.unwrap_or(1).clamp(1, 10_000),
            sort: parse_sort(q.sort.as_deref().unwrap_or("")),
            include,
            exclude,
        },
        (Err(e), _) | (_, Err(e)) => return e.into_response(),
    };
    match st.service.search(&params).await {
        Ok(p) => ok(json!({
            "items": p.items.iter().map(|s| st.summary_json(s)).collect::<Vec<_>>(),
            "page": p.page,
            "numPages": p.num_pages,
        })),
        Err(e) => e.into_response(),
    }
}

async fn random(State(st): State<St>) -> Response {
    match st.service.random().await {
        Ok(d) => ok(st.detail_json(&d)),
        Err(e) => e.into_response(),
    }
}

async fn gallery(State(st): State<St>, Path(id): Path<i32>) -> Response {
    match st.service.gallery(id).await {
        Ok(d) => ok(st.detail_json(&d)),
        Err(ServiceError::NotFound) => {
            st.store.forget(id);
            ServiceError::NotFound.into_response()
        }
        Err(e) => e.into_response(),
    }
}

async fn related(State(st): State<St>, Path(id): Path<i32>) -> Response {
    match st.service.gallery(id).await {
        Ok(d) => {
            ok(json!({"items": d.related.iter().map(|s| st.summary_json(s)).collect::<Vec<_>>()}))
        }
        Err(e) => e.into_response(),
    }
}

#[derive(Deserialize)]
struct TagsQ {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    q: String,
    #[serde(default)]
    limit: Option<usize>,
}

async fn tags(State(st): State<St>, Query(q): Query<TagsQ>) -> Response {
    let list = st
        .service
        .tags
        .list(q.kind.as_deref(), &q.q, q.limit.unwrap_or(50).min(200));
    ok(json!({"tags": list}))
}

async fn favorites(State(st): State<St>) -> Response {
    let mut items = Vec::new();
    for f in st.store.favorites() {
        if st.service.still_allowed(&f.gallery) {
            let mut v = st.summary_json(&f.gallery);
            v["addedAt"] = json!(f.added_at);
            items.push(v);
        } else {
            st.store.forget(f.gallery.id);
        }
    }
    ok(json!({"items": items}))
}

async fn add_favorite(State(st): State<St>, Path(id): Path<i32>) -> Response {
    match st.service.gallery(id).await {
        Ok(d) => {
            st.store.add_favorite(crate::service::summary_of(&d));
            ok(json!({"favorite": true}))
        }
        Err(e) => e.into_response(),
    }
}

async fn remove_favorite(State(st): State<St>, Path(id): Path<i32>) -> Response {
    st.store.remove_favorite(id);
    ok(json!({"favorite": false}))
}

async fn all_progress(State(st): State<St>) -> Response {
    ok(json!({"progress": st.store.all_progress()}))
}

#[derive(Deserialize)]
struct ProgressBody {
    page: i32,
    pages: i32,
}

async fn set_progress(
    State(st): State<St>,
    Path(id): Path<i32>,
    Json(b): Json<ProgressBody>,
) -> Response {
    if id <= 0 || b.page < 0 || b.pages < 0 || b.page > 100_000 {
        return err(StatusCode::BAD_REQUEST, "bad_request");
    }
    st.store.set_progress(id, b.page, b.pages);
    ok(json!({"page": b.page}))
}

async fn image(
    State(st): State<St>,
    Path((id, kind, index, exp, sig)): Path<(i32, String, usize, i64, String)>,
) -> Response {
    let msg = format!("{id}/{kind}/{index}/{exp}");
    let expected = sign(&st.signing_key, &msg);
    let valid =
        sig.len() == expected.len() && bool::from(sig.as_bytes().ct_eq(expected.as_bytes()));
    if !valid || exp < chrono::Utc::now().timestamp() {
        return err(StatusCode::FORBIDDEN, "forbidden");
    }
    let Some(kind) = parse_kind(&kind, index) else {
        return err(StatusCode::NOT_FOUND, "not_found");
    };
    match st.service.image(id, kind).await {
        Ok((bytes, ctype)) => {
            let mut r = (StatusCode::OK, bytes).into_response();
            if let Ok(v) = HeaderValue::from_str(&ctype) {
                r.headers_mut().insert(header::CONTENT_TYPE, v);
            }
            r.headers_mut().insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            no_store(r)
        }
        Err(e) => e.into_response(),
    }
}
