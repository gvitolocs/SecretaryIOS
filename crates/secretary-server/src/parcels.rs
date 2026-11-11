//! The Parcels API for the apps: `/v1/parcels*`, forwarded to Lucina's `/api/secretary/parcels*`
//! (crate `lucina-server`, module `parcels`; the contract is in `docs/lucina-rust/parcels.md`).
//!
//! Behind `auth::require_device` (any owner device or the MCP's device token). The body is
//! checked here (`Call::parcels_*`); Lucina's JSON passes through unchanged.
//!
//! - `GET|POST /v1/parcels`             `{parcels, digestHour}` / follow a parcel → `{parcel}`
//! - `GET|PATCH|DELETE /v1/parcels/{id}` one parcel / rename it / forget it
//! - `POST /v1/parcels/{id}/refresh`    check the carrier now (not within 10 minutes of a check)
//!
//! Lucina unreachable, timing out, off or lacking the routes → 503 `parcels_unavailable`. On the
//! item routes a 404 `not_found` from Lucina (an unknown parcel) reaches the app as a 404.

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use secretary_core::Call;
use serde_json::Value;

use crate::config::BODY_LIMIT;
use crate::error::{ApiError, JsonBody};
use crate::AppState;

type ApiResult = Result<(StatusCode, Json<Value>), ApiError>;

/// Lucina failures that mean "parcels are not there right now".
fn unavailable(e: secretary_core::Error) -> ApiError {
    use secretary_core::Error as E;
    match e {
        E::Unreachable(_) | E::Timeout => parcels_unavailable(),
        E::Api { status, .. } if status >= 500 || status == 404 || status == 405 => {
            parcels_unavailable()
        }
        other => other.into(),
    }
}

/// Like [`unavailable`], except that Lucina's own "no such parcel" is the app's 404.
fn item_error(e: secretary_core::Error) -> ApiError {
    match e {
        secretary_core::Error::Api { status: 404, code, message } if code == "not_found" => {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                message.unwrap_or_else(|| "no such parcel".into()),
            )
        }
        other => unavailable(other),
    }
}

fn parcels_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "parcels_unavailable",
        "Parcel tracking is not reachable right now",
    )
}

async fn forward(
    st: &AppState,
    headers: &HeaderMap,
    call: Call,
    on_error: fn(secretary_core::Error) -> ApiError,
) -> ApiResult {
    let call = call.with_caller(&crate::computer::caller(headers));
    let data = st.lucina()?.call::<Value>(&call).await.map_err(on_error)?;
    Ok((StatusCode::OK, Json(data)))
}

async fn list(State(st): State<AppState>, headers: HeaderMap) -> ApiResult {
    forward(&st, &headers, Call::parcels_list(), unavailable).await
}

async fn add(State(st): State<AppState>, headers: HeaderMap, JsonBody(body): JsonBody<Value>) -> ApiResult {
    forward(&st, &headers, Call::parcels_add(body)?, unavailable).await
}

async fn one(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    forward(&st, &headers, Call::parcels_get(&id)?, item_error).await
}

async fn rename(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>, JsonBody(body): JsonBody<Value>) -> ApiResult {
    forward(&st, &headers, Call::parcels_rename(&id, body)?, item_error).await
}

async fn remove(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    forward(&st, &headers, Call::parcels_remove(&id)?, item_error).await
}

async fn refresh(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    forward(&st, &headers, Call::parcels_refresh(&id)?, item_error).await
}

/// Owner routes (behind `auth::require_device`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/parcels", get(list).post(add).layer(DefaultBodyLimit::max(BODY_LIMIT)))
        .route("/v1/parcels/{id}", get(one).patch(rename).delete(remove))
        .route("/v1/parcels/{id}/refresh", post(refresh))
}
