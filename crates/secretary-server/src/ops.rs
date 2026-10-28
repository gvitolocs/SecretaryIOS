//! Lucina's ops record for the apps and the MCP: `/v1/ops/*`, forwarded to
//! Lucina's `/api/secretary/ops/*` (crate `lucina-server`, module `ops`).
//!
//! Every Claude session, coding agent and Lucina herself reports deploys,
//! incidents, decisions, status and findings here; Lucina writes each record
//! to the Obsidian folder (daily log, project page, incident page) and to
//! Honcho, and answers searches over them.
//!
//! Owner devices only (device token or web session; Lucina's service token is
//! not accepted), like `/v1/computer`. The MCP reaches these routes with the
//! owner device token it was configured with and names its client in
//! `X-Secretary-Caller` (`mcp:claude`), which travels to Lucina as
//! `x-lucina-caller`: she takes the reporter from it, never from the body.
//!
//! - `POST /v1/ops/records`       a `NewRecord` JSON object (≤ 256 KiB) → 201 new /
//!   200 duplicate `{id, duplicate, redacted, honcho, paths}`; Lucina validates
//!   the fields and denies unknown ones
//! - `GET  /v1/ops/records/{id}`  one record (id = 16 lowercase hex)
//! - `GET  /v1/ops/search?q=&project=&kind=&since=&until=&limit=` `{items, honcho}`

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use secretary_core::Call;
use serde_json::Value;

use crate::computer::caller;
use crate::config::BODY_LIMIT;
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::AppState;

type ApiResult = Result<(StatusCode, Json<Value>), ApiError>;

async fn forward(st: &AppState, call: Call) -> ApiResult {
    let (status, data) = st.lucina()?.call_with_status::<Value>(&call).await?;
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(data),
    ))
}

async fn record(
    State(st): State<AppState>,
    headers: HeaderMap,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult {
    if !body.is_object() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "body must be a JSON object",
        ));
    }
    forward(&st, Call::ops_record(body).with_caller(&caller(&headers))).await
}

async fn record_get(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    forward(
        &st,
        Call::ops_record_get(&id)?.with_caller(&caller(&headers)),
    )
    .await
}

#[derive(serde::Deserialize, Default)]
struct SearchQuery {
    q: Option<String>,
    project: Option<String>,
    kind: Option<String>,
    since: Option<String>,
    until: Option<String>,
    limit: Option<u32>,
}

async fn search(
    State(st): State<AppState>,
    headers: HeaderMap,
    QueryArgs(q): QueryArgs<SearchQuery>,
) -> ApiResult {
    let call = Call::ops_search(
        q.q.as_deref(),
        q.project.as_deref(),
        q.kind.as_deref(),
        q.since.as_deref(),
        q.until.as_deref(),
        q.limit,
    )?;
    forward(&st, call.with_caller(&caller(&headers))).await
}

/// Owner routes (behind `auth::require_device`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/ops/records",
            post(record).layer(DefaultBodyLimit::max(BODY_LIMIT)),
        )
        .route("/v1/ops/records/{id}", get(record_get))
        .route("/v1/ops/search", get(search))
}
