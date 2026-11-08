//! AU Find (Aarhus University people, buildings, campus maps) for the apps
//! and the MCP: `/v1/au/*`, forwarded to Lucina's `/api/secretary/au/*`.
//!
//! Read-only, behind `auth::require_device` (any owner device or the MCP's
//! device token). The query is validated here (`Call::au_*`) and only the
//! known parameters travel on; Lucina's JSON passes through unchanged.
//!
//! - `GET /v1/au/health`                         `{reachable, af_server, error?}`
//! - `GET /v1/au/people?q=&lang=da|en`           `{source, status, query, tried, total, people, …}`
//! - `GET /v1/au/buildings?q=`                   `{source, status, buildings, map?}`
//! - `GET /v1/au/map.png?building=&z=` | `?lat=&lng=&z=`  `image/png` (800×600),
//!   `cache-control: private, max-age=3600`
//!
//! Lucina (or AU Find behind it) unreachable, timing out or lacking the
//! routes → 503 `au_unavailable`.

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use secretary_core::Call;
use serde_json::Value;

use crate::error::{ApiError, QueryArgs};
use crate::AppState;

type ApiResult = Result<(StatusCode, Json<Value>), ApiError>;

/// Lucina failures that mean "AU Find is not there right now".
fn unavailable(e: secretary_core::Error) -> ApiError {
    use secretary_core::Error as E;
    match e {
        E::Unreachable(_) | E::Timeout => au_unavailable(),
        E::Api { status, .. } if status >= 500 || status == 404 || status == 405 => {
            au_unavailable()
        }
        other => other.into(),
    }
}

fn au_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "au_unavailable",
        "AU Find is not reachable right now",
    )
}

async fn forward(st: &AppState, call: Call) -> ApiResult {
    let data = st
        .lucina()?
        .call::<Value>(&call)
        .await
        .map_err(unavailable)?;
    Ok((StatusCode::OK, Json(data)))
}

async fn health(State(st): State<AppState>) -> ApiResult {
    forward(&st, Call::au_health()).await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PeopleQuery {
    q: Option<String>,
    lang: Option<String>,
}

async fn people(State(st): State<AppState>, QueryArgs(q): QueryArgs<PeopleQuery>) -> ApiResult {
    forward(&st, Call::au_people(q.q.as_deref(), q.lang.as_deref())?).await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildingsQuery {
    q: Option<String>,
}

async fn buildings(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<BuildingsQuery>,
) -> ApiResult {
    forward(&st, Call::au_buildings(q.q.as_deref())?).await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MapQuery {
    building: Option<String>,
    lat: Option<String>,
    lng: Option<String>,
    z: Option<String>,
}

/// The campus map as PNG, relayed as is (cacheable by the device for an hour).
async fn map(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<MapQuery>,
) -> Result<Response, ApiError> {
    let call = Call::au_map(
        q.building.as_deref(),
        q.lat.as_deref(),
        q.lng.as_deref(),
        q.z.as_deref(),
    )?;
    let (content_type, body) = st.lucina()?.bytes(&call).await.map_err(unavailable)?;
    if !content_type.starts_with("image/png") {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "lucina_error",
            "Lucina sent a map that is not a PNG",
        ));
    }
    Ok((
        [
            (header::CONTENT_TYPE, "image/png".to_owned()),
            (header::CACHE_CONTROL, "private, max-age=3600".to_owned()),
        ],
        body,
    )
        .into_response())
}

/// Owner routes (behind `auth::require_device`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/au/health", get(health))
        .route("/v1/au/people", get(people))
        .route("/v1/au/buildings", get(buildings))
        .route("/v1/au/map.png", get(map))
}
