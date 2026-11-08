//! Lucina's flaw tracker for the apps and the MCP: `/v1/flaws/*`, forwarded to Lucina's
//! `/api/secretary/flaws/*` (crate `lucina-server`, module `flaws`).
//!
//! Owner devices only (device token or web session; Lucina's service token is not accepted),
//! like `/v1/ops`. The MCP names its client in `X-Secretary-Caller`, which travels to Lucina
//! as `x-lucina-caller`: she takes the reporter from it, never from the body. Lucina validates
//! the fields and enforces the evidence rule (closing without evidence is 422).
//!
//! - `GET|POST /v1/flaws`                     list / report
//! - `GET /v1/flaws/matrix`                   every live flaw against every client
//! - `GET|PATCH /v1/flaws/{id}`               one flaw with its history / change it
//! - `PUT /v1/flaws/{id}/targets/{platform}`  one client's row
//! - `POST /v1/flaws/{id}/dispatch`           `{platform}`: start that task now

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use secretary_core::Call;
use serde_json::Value;

use crate::computer::caller;
use crate::config::BODY_LIMIT;
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::AppState;

type ApiResult = Result<(StatusCode, Json<Value>), ApiError>;

async fn forward(st: &AppState, headers: &HeaderMap, call: Call) -> ApiResult {
    let call = call.with_caller(&caller(headers));
    let (status, data) = st.lucina()?.call_with_status::<Value>(&call).await?;
    Ok((StatusCode::from_u16(status).unwrap_or(StatusCode::OK), Json(data)))
}

fn object(body: &Value) -> Result<(), ApiError> {
    if body.is_object() {
        return Ok(());
    }
    Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_request", "body must be a JSON object"))
}

#[derive(serde::Deserialize, Default)]
struct ListArgs {
    q: Option<String>,
    state: Option<String>,
    platform: Option<String>,
    status: Option<String>,
    limit: Option<u32>,
}

async fn list(State(st): State<AppState>, headers: HeaderMap, QueryArgs(q): QueryArgs<ListArgs>) -> ApiResult {
    let call = Call::flaws_list(q.q.as_deref(), q.state.as_deref(), q.platform.as_deref(), q.status.as_deref(), q.limit)?;
    forward(&st, &headers, call).await
}

async fn report(State(st): State<AppState>, headers: HeaderMap, JsonBody(body): JsonBody<Value>) -> ApiResult {
    object(&body)?;
    forward(&st, &headers, Call::flaw_report(body)).await
}

async fn matrix(State(st): State<AppState>, headers: HeaderMap) -> ApiResult {
    forward(&st, &headers, Call::flaws_matrix()).await
}

async fn one(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    forward(&st, &headers, Call::flaw_get(&id)?).await
}

async fn patch(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>, JsonBody(body): JsonBody<Value>) -> ApiResult {
    object(&body)?;
    forward(&st, &headers, Call::flaw_patch(&id, body)?).await
}

async fn target(State(st): State<AppState>, headers: HeaderMap, Path((id, platform)): Path<(String, String)>, JsonBody(body): JsonBody<Value>) -> ApiResult {
    object(&body)?;
    forward(&st, &headers, Call::flaw_target(&id, &platform, body)?).await
}

async fn dispatch(State(st): State<AppState>, headers: HeaderMap, Path(id): Path<String>, JsonBody(body): JsonBody<Value>) -> ApiResult {
    object(&body)?;
    forward(&st, &headers, Call::flaw_dispatch(&id, body)?).await
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/flaws", get(list).post(report).layer(DefaultBodyLimit::max(BODY_LIMIT)))
        .route("/v1/flaws/matrix", get(matrix))
        .route("/v1/flaws/{id}", get(one).patch(patch))
        .route("/v1/flaws/{id}/targets/{platform}", put(target))
        .route("/v1/flaws/{id}/dispatch", post(dispatch))
}
