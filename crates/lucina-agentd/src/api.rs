//! The daemon's JSON API, served on the unix socket (and optional loopback TCP).
//! secretary-server will proxy it as `/v1/agents/*` (phase 7).
//!
//! | Method | Path | Body / query | Returns |
//! |---|---|---|---|
//! | GET | `/health` | | `{ok, version}` |
//! | GET | `/providers` | | provider infos |
//! | POST | `/agents` | create request | snapshot |
//! | GET | `/agents` | `?archived=true` | `{agents: [snapshot]}` |
//! | GET | `/agents/items` | `?archived=true` | `{agents: [list item]}` |
//! | GET | `/agents/{id}` | id or unique prefix | snapshot |
//! | POST | `/agents/{id}/send` | `{prompt, activeTurnBehavior}` | `{disposition}` |
//! | POST | `/agents/{id}/stop` | | `{agentId, stopped}` |
//! | POST | `/agents/{id}/archive` | | `{agentId, archivedAt}` |
//! | POST | `/agents/{id}/unarchive` | | snapshot |
//! | DELETE | `/agents/{id}` | | `{agentId, deleted}` |
//! | POST | `/agents/{id}/wait` | `?timeoutMs=` (0 = none) | wait result |
//! | GET | `/agents/{id}/timeline` | `?afterSeq=&limit=` | `{epoch, rows, nextSeq}` |
//! | GET | `/stream` | | SSE of bus events |

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use crate::manager::{ActiveTurnBehavior, CreateRequest, Manager, ManagerError};

pub struct ApiError(StatusCode, String, String);

impl From<ManagerError> for ApiError {
    fn from(e: ManagerError) -> Self {
        let status = match e {
            ManagerError::NotFound(_) => StatusCode::NOT_FOUND,
            ManagerError::Ambiguous(_) | ManagerError::Invalid(_) | ManagerError::UnknownProvider(_) => StatusCode::BAD_REQUEST,
            ManagerError::Lifecycle(_) => StatusCode::CONFLICT,
            ManagerError::Provider(_) => StatusCode::BAD_GATEWAY,
            ManagerError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError(status, e.code().to_string(), e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": {"code": self.1, "message": self.2}}))).into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

fn to_json<T: serde::Serialize>(v: T) -> ApiResult {
    serde_json::to_value(v)
        .map(Json)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "SERIALIZE".into(), e.to_string()))
}

pub fn router(manager: Manager) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/providers", get(providers))
        .route("/agents", get(list).post(create))
        .route("/agents/items", get(items))
        .route("/agents/{id}", get(inspect).delete(delete))
        .route("/agents/{id}/send", post(send))
        .route("/agents/{id}/stop", post(stop))
        .route("/agents/{id}/archive", post(archive))
        .route("/agents/{id}/unarchive", post(unarchive))
        .route("/agents/{id}/wait", post(wait))
        .route("/agents/{id}/timeline", get(timeline))
        .route("/stream", get(stream))
        .with_state(manager)
}

async fn health() -> Json<Value> {
    Json(json!({"ok": true, "version": env!("CARGO_PKG_VERSION")}))
}

async fn providers(State(m): State<Manager>) -> ApiResult {
    to_json(m.providers())
}

#[derive(Deserialize, Default)]
struct ListQuery {
    #[serde(default)]
    archived: bool,
}

async fn list(State(m): State<Manager>, Query(q): Query<ListQuery>) -> ApiResult {
    to_json(json!({"agents": m.snapshots(q.archived)}))
}

async fn items(State(m): State<Manager>, Query(q): Query<ListQuery>) -> ApiResult {
    to_json(json!({"agents": m.list(q.archived)}))
}

async fn create(State(m): State<Manager>, Json(req): Json<CreateRequest>) -> ApiResult {
    to_json(m.create(req).await?)
}

async fn inspect(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    let id = m.resolve(&id)?;
    to_json(m.snapshot(&id)?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendBody {
    prompt: String,
    #[serde(default)]
    active_turn_behavior: ActiveTurnBehavior,
}

async fn send(State(m): State<Manager>, Path(id): Path<String>, Json(body): Json<SendBody>) -> ApiResult {
    let id = m.resolve(&id)?;
    let disposition = m.send(&id, &body.prompt, body.active_turn_behavior).await?;
    to_json(json!({"agentId": id, "disposition": disposition}))
}

async fn stop(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    let id = m.resolve(&id)?;
    let stopped = m.stop(&id).await?;
    to_json(json!({"agentId": id, "stopped": stopped}))
}

async fn archive(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    let id = m.resolve(&id)?;
    let archived_at = m.archive(&id).await?;
    to_json(json!({"agentId": id, "archivedAt": archived_at}))
}

async fn unarchive(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    let id = m.resolve(&id)?;
    to_json(m.unarchive(&id)?)
}

async fn delete(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    let id = m.resolve(&id)?;
    m.delete(&id).await?;
    to_json(json!({"agentId": id, "deleted": true}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WaitQuery {
    #[serde(default)]
    timeout_ms: u64,
}

async fn wait(State(m): State<Manager>, Path(id): Path<String>, Query(q): Query<WaitQuery>) -> ApiResult {
    let id = m.resolve(&id)?;
    let timeout = (q.timeout_ms > 0).then(|| Duration::from_millis(q.timeout_ms));
    to_json(m.wait(&id, timeout).await?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimelineQuery {
    #[serde(default)]
    after_seq: u64,
    #[serde(default)]
    limit: usize,
}

async fn timeline(State(m): State<Manager>, Path(id): Path<String>, Query(q): Query<TimelineQuery>) -> ApiResult {
    let id = m.resolve(&id)?;
    let (epoch, rows) = m.timeline(&id, q.after_seq, q.limit)?;
    let next_seq = rows.last().map(|r| r.seq).unwrap_or(q.after_seq);
    to_json(json!({"epoch": epoch, "rows": rows, "nextSeq": next_seq}))
}

async fn stream(State(m): State<Manager>) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let events = BroadcastStream::new(m.subscribe()).filter_map(|msg| {
        let event = msg.ok()?;
        let data = serde_json::to_string(&event).ok()?;
        Some(Ok(SseEvent::default().data(data)))
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}
