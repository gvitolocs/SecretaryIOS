//! Caller screening for the apps and the MCP: `/v1/callers/*`, forwarded to
//! Lucina's `/api/secretary/callers/*` (Jev's verdict on unknown callers).
//!
//! Behind `auth::require_device` (any owner device or the MCP's device token).
//! The body is checked here (`Call::callers_screen`); Lucina's JSON passes
//! through unchanged.
//!
//! - `POST /v1/callers/screen`    `{numbers, lang?, context?, call?}` → `{lang, results, table}`
//! - `GET  /v1/callers/directory` `{generatedAt, entries: [{number, label}]}` (iOS Call Directory)
//!
//! Lucina unreachable, timing out or lacking the routes → 503 `callers_unavailable`.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use secretary_core::Call;
use serde_json::Value;

use crate::error::{ApiError, JsonBody};
use crate::AppState;

type ApiResult = Result<(StatusCode, Json<Value>), ApiError>;

/// Lucina failures that mean "caller screening is not there right now".
fn unavailable(e: secretary_core::Error) -> ApiError {
    use secretary_core::Error as E;
    match e {
        E::Unreachable(_) | E::Timeout => callers_unavailable(),
        E::Api { status, .. } if status >= 500 || status == 404 || status == 405 => {
            callers_unavailable()
        }
        other => other.into(),
    }
}

fn callers_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "callers_unavailable",
        "Caller screening is not reachable right now",
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

async fn screen(
    State(st): State<AppState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult {
    forward(
        &st,
        Call::callers_screen(body)?.with_caller(&crate::computer::caller(&headers)),
    )
    .await
}

async fn directory(State(st): State<AppState>) -> ApiResult {
    forward(&st, Call::callers_directory()).await
}

/// Owner routes (behind `auth::require_device`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/callers/screen", post(screen))
        .route("/v1/callers/directory", get(directory))
}
