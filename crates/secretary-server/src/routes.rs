//! `/v1/*` app routes. Each handler validates through `secretary-core`
//! (`Call::*` constructors), forwards with the server's Lucina token and
//! returns Lucina's `data` unwrapped.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, patch};
use axum::{Json, Router};
use secretary_core::models::{AgendaQuery, ChatRequest, CreateReminder, ReminderQuery};
use secretary_core::Call;
use serde_json::{json, Value};

use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::AppState;

type ApiResult = Result<(StatusCode, Json<Value>), ApiError>;

/// Forward and return Lucina's `data` (after the call's unwrap) with
/// Lucina's 2xx status (201 for a created reminder or shopping item).
async fn forward(st: &AppState, call: Call) -> ApiResult {
    let (status, data) = st.lucina()?.call_with_status::<Value>(&call).await?;
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    Ok((status, Json(data)))
}

async fn health(State(st): State<AppState>) -> Json<Value> {
    let lucina = st.health.status(st.lucina.as_ref()).await;
    Json(json!({
        "ok": true,
        "version": crate::VERSION,
        "lucina": lucina,
    }))
}

/// Header naming the client surface (`web` | `ios`) for chat.
pub const CLIENT_HEADER: &str = "x-secretary-client";

/// The chat `channel`: the `X-Secretary-Client` header, else the body's
/// `channel`, else `web`. Explicit values must be `web` or `ios`.
fn chat_channel(headers: &HeaderMap, body: Option<&str>) -> Result<String, ApiError> {
    let channel = match headers.get(CLIENT_HEADER) {
        Some(v) => v.to_str().map(str::trim).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "X-Secretary-Client: must be one of web, ios",
            )
        })?,
        None => body.unwrap_or("web"),
    };
    secretary_core::validate::chat_channel("X-Secretary-Client", channel).map_err(|_| {
        let field = if headers.contains_key(CLIENT_HEADER) {
            "X-Secretary-Client"
        } else {
            "channel"
        };
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            format!("{field}: must be one of web, ios"),
        )
    })?;
    Ok(channel.to_owned())
}

/// Lucina's reply (with `userItemId`/`lucinaItemId`) passes through as is.
async fn chat(
    State(st): State<AppState>,
    headers: HeaderMap,
    JsonBody(mut req): JsonBody<ChatRequest>,
) -> ApiResult {
    req.channel = Some(chat_channel(&headers, req.channel.as_deref())?);
    forward(&st, Call::chat(&req)?).await
}

async fn job(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult {
    forward(&st, Call::job(&id)?).await
}

async fn reminders(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<ReminderQuery>,
) -> ApiResult {
    forward(&st, Call::reminders(&q)?).await
}

async fn create_reminder(
    State(st): State<AppState>,
    JsonBody(req): JsonBody<CreateReminder>,
) -> ApiResult {
    forward(&st, Call::create_reminder(&req)?).await
}

async fn cancel_reminder(State(st): State<AppState>, Path(id): Path<String>) -> ApiResult {
    forward(&st, Call::cancel_reminder(&id)?).await
}

/// Lucina's agenda plus the calendar events of that date (all sources).
async fn agenda(State(st): State<AppState>, QueryArgs(q): QueryArgs<AgendaQuery>) -> ApiResult {
    let (status, Json(mut data)) = forward(&st, Call::agenda(&q)?).await?;
    let date = data
        .get("date")
        .and_then(Value::as_str)
        .or(q.date.as_deref())
        .and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .unwrap_or_else(|| chrono::Utc::now().with_timezone(&st.links.tz).date_naive());
    if let Some(obj) = data.as_object_mut() {
        let events = crate::calendar::events_on(&st.links, date).await;
        obj.insert("events".into(), json!(events));
    }
    Ok((status, Json(data)))
}

async fn shopping(State(st): State<AppState>) -> ApiResult {
    forward(&st, Call::shopping()).await
}

/// `GET /v1/grocery/deals/map`: Lucina's deals map around home, plus the
/// server's client-side Google Maps keys so the app can draw the map itself.
async fn deals_map(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<DealsMapQuery>,
) -> ApiResult {
    let radius = match q.radius_m.as_deref() {
        None => None,
        Some(raw) => Some(secretary_core::validate::bounded_int(
            "radiusM",
            raw,
            secretary_core::validate::DEALS_RADIUS_MIN,
            secretary_core::validate::DEALS_RADIUS_MAX,
        )?),
    };
    let (status, Json(mut data)) = forward(&st, Call::deals_map(radius)?).await?;
    if let Some(obj) = data.as_object_mut() {
        // The keys are client-side, referrer/bundle-id restricted in Google
        // Cloud, so serving them to an authenticated device is intended.
        obj.insert(
            "maps".into(),
            json!({
                "webKey": st.maps.web,
                "iosKey": st.maps.ios,
                "mapId": st.maps.map_id,
            }),
        );
    }
    Ok((status, Json(data)))
}

/// Query of `GET /v1/grocery/deals/map` (optional `radiusM` in meters).
#[derive(serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct DealsMapQuery {
    radius_m: Option<String>,
}

async fn shopping_add(State(st): State<AppState>, JsonBody(body): JsonBody<Value>) -> ApiResult {
    forward(&st, Call::shopping_add(body)?).await
}

async fn shopping_update(
    State(st): State<AppState>,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<Value>,
) -> ApiResult {
    forward(&st, Call::shopping_update(&id, body)?).await
}

/// Routes without auth.
pub fn public() -> Router<AppState> {
    Router::new().route("/v1/health", get(health))
}

/// Routes behind the device-token middleware.
pub fn protected() -> Router<AppState> {
    Router::new()
        .route("/v1/chat", axum::routing::post(chat))
        .route("/v1/jobs/{id}", get(job))
        .route("/v1/reminders", get(reminders).post(create_reminder))
        .route("/v1/reminders/{id}", delete(cancel_reminder))
        .route("/v1/agenda", get(agenda))
        .route("/v1/shopping", get(shopping).post(shopping_add))
        .route("/v1/shopping/{id}", patch(shopping_update))
        .route("/v1/grocery/deals/map", get(deals_map))
}
