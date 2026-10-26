//! `GET /v1/insights/summary`: a compact, read-only view of the gym and body
//! modules for Lucina. Service token only (see [`crate::service`]). The
//! numbers come from `secretary_gym::insights` and `secretary_body::insights`;
//! this handler only composes them.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{middleware, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::engines::with_engine;
use crate::error::{ApiError, QueryArgs};
use crate::AppState;

pub const DEFAULT_WORKOUTS: usize = 5;
pub const MAX_WORKOUTS: usize = 20;

#[derive(Deserialize)]
struct SummaryQuery {
    timezone: Option<String>,
    workouts: Option<usize>,
}

async fn summary(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<SummaryQuery>,
) -> Result<Json<Value>, ApiError> {
    let tz_name = q.timezone.unwrap_or_else(|| "UTC".into());
    let tz: chrono_tz::Tz = tz_name.parse().map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "timezone: unknown IANA timezone",
        )
    })?;
    let workouts = q
        .workouts
        .unwrap_or(DEFAULT_WORKOUTS)
        .clamp(1, MAX_WORKOUTS);
    let now = secretary_gym::now_ms();
    let gym = with_engine(&st.engines.gym, move |e| {
        secretary_gym::insights::summary(e.state(), now, tz, workouts)
    })
    .await?;
    let body = with_engine(&st.engines.body, move |e| {
        secretary_body::insights(e.state(), now)
    })
    .await?;
    let tasks = crate::tasks::insights(&st, tz).await;
    Ok(Json(json!({
        "generated_ms": now,
        "timezone": tz_name,
        "gym": gym,
        "body": body,
        "tasks": tasks,
    })))
}

/// The service-token routes.
pub fn routes(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/v1/insights/summary", get(summary))
        .route_layer(middleware::from_fn_with_state(
            state,
            crate::service::require_service,
        ))
}
