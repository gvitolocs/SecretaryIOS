//! Shopping v2 (`docs/shopping-v2.md`): the static catalog and the list /
//! ops routes proxied to Lucina's grocery store.
//!
//! - `GET /v1/shopping/catalog?lang=it|en|da` serves
//!   `{SECRETARY_CATALOG_DIR}/catalog.{lang}.json` with a strong ETag
//!   (SHA-256 of the bytes), `Cache-Control: public, max-age=3600`, and
//!   `304` for a matching `If-None-Match`. No Lucina call.
//! - Lists and ops are validated by `secretary-core` (`Call::shopping_*`),
//!   forwarded with the server's Lucina token and decoded into typed models
//!   that keep unknown fields.
//! - `?lang=it|en|da` on the list state, the ops and the legacy list is
//!   passed through (the ops route also takes a body `lang`): item names come
//!   back in that language; without it Lucina uses the owner's.
//! - `GET /v1/shopping/items`: the owner's custom item objects.
//! - Lucina `409` on the ops route becomes `409 revision_conflict`; other
//!   Lucina 4xx codes (`list_not_found`, `item_not_found`, 422 ...) pass
//!   through as usual (see [`crate::error`]).

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use secretary_core::models::{
    CreateList, ItemObject, ListState, OpsBatch, OpsResult, ShoppingList, ShoppingListData,
    ShoppingListsData, UpdateList,
};
use secretary_core::Call;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::AppState;

/// Catalog languages (file `catalog.{lang}.json`).
pub const CATALOG_LANGS: &[&str] = &["it", "en", "da"];
/// The language when `lang` is absent.
pub const DEFAULT_LANG: &str = "it";
pub const CATALOG_CACHE_CONTROL: &str = "public, max-age=3600";

#[derive(Deserialize)]
struct CatalogQuery {
    lang: Option<String>,
}

/// `?lang=` of the list routes (validated by `Call::with_lang`).
#[derive(Deserialize)]
pub(crate) struct LangQuery {
    pub(crate) lang: Option<String>,
}

/// Strong ETag: `"<base64url(sha256)>"`.
pub fn etag(bytes: &[u8]) -> String {
    format!("\"{}\"", URL_SAFE_NO_PAD.encode(Sha256::digest(bytes)))
}

/// `If-None-Match` matches `etag` (weak comparison, as RFC 9110 asks for
/// this header; `*` matches anything).
fn none_match(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .any(|t| t == "*" || t.strip_prefix("W/").unwrap_or(t) == etag)
}

async fn catalog(
    State(st): State<AppState>,
    headers: HeaderMap,
    QueryArgs(q): QueryArgs<CatalogQuery>,
) -> Result<Response, ApiError> {
    let lang = q.lang.as_deref().unwrap_or(DEFAULT_LANG);
    if !CATALOG_LANGS.contains(&lang) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("lang: must be one of {}", CATALOG_LANGS.join(", ")),
        ));
    }
    // `lang` is whitelisted, so the path stays inside the catalog dir.
    let path = st.catalog_dir.join(format!("catalog.{lang}.json"));
    let bytes = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "catalog file unreadable");
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "catalog_unavailable",
                "the shopping catalog is not installed on the server",
            ));
        }
    };
    let tag = etag(&bytes);
    let tag_value = HeaderValue::from_str(&tag).map_err(|_| ApiError::internal("bad etag"))?;
    let mut resp = if none_match(&headers, &tag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let mut r = Body::from(bytes).into_response();
        r.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        r
    };
    let h = resp.headers_mut();
    h.insert(header::ETAG, tag_value);
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CATALOG_CACHE_CONTROL),
    );
    // Authenticated responses differ per credential only in access, not
    // content; still, keep shared caches keyed on it.
    h.insert(
        header::VARY,
        HeaderValue::from_static("Authorization, Cookie"),
    );
    Ok(resp)
}

type Typed<T> = Result<(StatusCode, Json<T>), ApiError>;

/// Forward `call` and decode `data` as `D`, keeping Lucina's 2xx status.
async fn send<D: serde::de::DeserializeOwned>(
    st: &AppState,
    call: &Call,
) -> Result<(StatusCode, D), ApiError> {
    let (status, data) = st.lucina()?.call_with_status::<D>(call).await?;
    Ok((StatusCode::from_u16(status).unwrap_or(StatusCode::OK), data))
}

async fn lists(State(st): State<AppState>) -> Typed<Vec<ShoppingList>> {
    let (s, d) = send::<ShoppingListsData>(&st, &Call::shopping_lists()).await?;
    Ok((s, Json(d.into_lists())))
}

async fn create_list(
    State(st): State<AppState>,
    JsonBody(req): JsonBody<CreateList>,
) -> Typed<ShoppingList> {
    let (s, d) = send::<ShoppingListData>(&st, &Call::shopping_list_create(&req)?).await?;
    Ok((s, Json(d.into_list())))
}

async fn list(
    State(st): State<AppState>,
    Path(id): Path<String>,
    QueryArgs(q): QueryArgs<LangQuery>,
) -> Typed<ListState> {
    let call = Call::shopping_list(&id)?.with_lang(q.lang.as_deref())?;
    let (s, d) = send::<ListState>(&st, &call).await?;
    Ok((s, Json(d)))
}

async fn items(State(st): State<AppState>) -> Typed<Vec<ItemObject>> {
    let (s, d) = send::<Vec<ItemObject>>(&st, &Call::shopping_items()).await?;
    Ok((s, Json(d)))
}

async fn update_list(
    State(st): State<AppState>,
    Path(id): Path<String>,
    JsonBody(req): JsonBody<UpdateList>,
) -> Typed<ShoppingList> {
    let (s, d) = send::<ShoppingListData>(&st, &Call::shopping_list_update(&id, &req)?).await?;
    Ok((s, Json(d.into_list())))
}

async fn delete_list(State(st): State<AppState>, Path(id): Path<String>) -> Typed<Value> {
    let (s, d) = send::<Value>(&st, &Call::shopping_list_delete(&id)?).await?;
    Ok((s, Json(d)))
}

async fn ops(
    State(st): State<AppState>,
    Path(id): Path<String>,
    QueryArgs(q): QueryArgs<LangQuery>,
    JsonBody(mut batch): JsonBody<OpsBatch>,
) -> Typed<OpsResult> {
    if batch.lang.is_none() {
        batch.lang = q.lang;
    }
    let call = Call::shopping_ops(&id, &batch)?;
    let (status, data) = match st.lucina()?.call_with_status::<OpsResult>(&call).await {
        Ok(v) => v,
        Err(secretary_core::Error::Api {
            status: 409,
            message,
            ..
        }) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                message.unwrap_or_else(|| {
                    "the list changed and an op conflicts; refetch and retry with new opIds".into()
                }),
            ))
        }
        Err(e) => return Err(e.into()),
    };
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(data),
    ))
}

/// Behind `require_device` (cookie writes need the CSRF header).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/shopping/catalog", get(catalog))
        .route("/v1/shopping/items", get(items))
        .route("/v1/shopping/lists", get(lists).post(create_list))
        .route(
            "/v1/shopping/lists/{id}",
            get(list).patch(update_list).delete(delete_list),
        )
        .route("/v1/shopping/lists/{id}/ops", axum::routing::post(ops))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etag_matching() {
        let tag = etag(b"{}");
        assert!(tag.starts_with('"') && tag.ends_with('"') && !tag.starts_with("W/"));
        assert_ne!(tag, etag(b"{ }"));
        let mut h = HeaderMap::new();
        assert!(!none_match(&h, &tag));
        h.insert(
            header::IF_NONE_MATCH,
            HeaderValue::from_str(&format!("\"x\", W/{tag}")).unwrap(),
        );
        assert!(none_match(&h, &tag));
        h.insert(header::IF_NONE_MATCH, HeaderValue::from_static("*"));
        assert!(none_match(&h, &tag));
        h.insert(header::IF_NONE_MATCH, HeaderValue::from_static("\"other\""));
        assert!(!none_match(&h, &tag));
    }
}
