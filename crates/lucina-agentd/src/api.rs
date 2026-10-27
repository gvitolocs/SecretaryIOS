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
//! | POST | `/agents/{id}/permissions/{requestId}` | permission response | `{agentId, requestId, behavior}` |
//! | GET | `/permissions` | `?all=true` | `{permissions: [view]}` (agent and external) |
//! | POST | `/permissions` | external request | view |
//! | GET | `/permissions/{id}` | `?waitMs=` (long-poll, up to 120 s) | view |
//! | POST | `/permissions/{id}` | `{behavior, message}` | view |
//! | GET | `/stream` | | SSE of bus events |
//!
//! Projects, workspaces, diffs, files and checkout operations: [`workspaces`].
//!
//! Answers and external requests check the caller ([`crate::peer`]): only the owner
//! answers, never an MCP token or a process an agent started.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

use lucina_agents::permission_gate::{Origin, Verdict};
use lucina_agents::PermissionResponse;

use crate::manager::{ActiveTurnBehavior, CreateRequest, ExternalRequest, Manager, ManagerError};
use crate::peer::{origin, Peer};
use crate::workspaces::WsError;

mod workspaces;

pub struct ApiError(StatusCode, String, String);

impl From<ManagerError> for ApiError {
    fn from(e: ManagerError) -> Self {
        let status = match e {
            ManagerError::NotFound(_) => StatusCode::NOT_FOUND,
            ManagerError::Ambiguous(_) | ManagerError::Invalid(_) | ManagerError::UnknownProvider(_) => StatusCode::BAD_REQUEST,
            ManagerError::Lifecycle(_) => StatusCode::CONFLICT,
            ManagerError::Provider(_) => StatusCode::BAD_GATEWAY,
            ManagerError::Store(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ManagerError::Forbidden(_) => StatusCode::FORBIDDEN,
            ManagerError::PermissionNotFound(_) => StatusCode::NOT_FOUND,
            ManagerError::AlreadyAnswered(_) => StatusCode::CONFLICT,
            ManagerError::Expired(_) => StatusCode::GONE,
            ManagerError::Unconfirmed(_) => StatusCode::GATEWAY_TIMEOUT,
            ManagerError::Workspace(WsError::NotFound(_)) => StatusCode::NOT_FOUND,
            ManagerError::Workspace(WsError::Invalid(_)) => StatusCode::BAD_REQUEST,
            ManagerError::Workspace(WsError::Git(_)) => StatusCode::BAD_GATEWAY,
            ManagerError::Workspace(WsError::Store(_)) => StatusCode::INTERNAL_SERVER_ERROR,
            ManagerError::Guard(_) => StatusCode::FORBIDDEN,
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
        .route("/agents/{id}/permissions/{request_id}", post(respond_permission))
        .route("/permissions", get(permissions).post(request_permission))
        .route("/permissions/{id}", get(permission).post(answer_permission))
        .route("/stream", get(stream))
        .merge(workspaces::routes())
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

async fn create(State(m): State<Manager>, ConnectInfo(peer): ConnectInfo<Peer>, headers: HeaderMap, Json(mut req): Json<CreateRequest>) -> ApiResult {
    // Only the owner may run an agent in a main checkout (worktree guard).
    if req.allow_main_checkout && !matches!(caller(&m, &peer, &headers), Origin::Local | Origin::OwnerDevice) {
        req.allow_main_checkout = false;
    }
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
    /// Also wait for the finished-run gate.
    #[serde(default)]
    gate: bool,
}

async fn wait(State(m): State<Manager>, Path(id): Path<String>, Query(q): Query<WaitQuery>) -> ApiResult {
    let id = m.resolve(&id)?;
    let timeout = (q.timeout_ms > 0).then(|| Duration::from_millis(q.timeout_ms));
    to_json(m.wait(&id, timeout, q.gate).await?)
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

fn caller(m: &Manager, peer: &Peer, headers: &HeaderMap) -> Origin {
    origin(peer, headers, &m.provider_pids())
}

async fn respond_permission(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Path((id, request_id)): Path<(String, String)>,
    Json(response): Json<PermissionResponse>,
) -> ApiResult {
    let id = m.resolve(&id)?;
    let who = caller(&m, &peer, &headers);
    let behavior = match response {
        PermissionResponse::Allow { .. } => "allow",
        PermissionResponse::Deny { .. } => "deny",
    };
    m.respond_permission(&id, &request_id, response, who).await?;
    to_json(json!({"agentId": id, "requestId": request_id, "behavior": behavior}))
}

#[derive(Deserialize, Default)]
struct PermissionsQuery {
    #[serde(default)]
    all: bool,
}

async fn permissions(State(m): State<Manager>, Query(q): Query<PermissionsQuery>) -> ApiResult {
    to_json(json!({"permissions": m.permissions(q.all)}))
}

async fn request_permission(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Json(req): Json<ExternalRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let who = caller(&m, &peer, &headers);
    let view = m.request_external(req, who)?;
    Ok((StatusCode::CREATED, to_json(view)?))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PermissionQuery {
    #[serde(default)]
    wait_ms: u64,
}

async fn permission(State(m): State<Manager>, Path(id): Path<String>, Query(q): Query<PermissionQuery>) -> ApiResult {
    if q.wait_ms == 0 {
        return to_json(m.permission(&id)?);
    }
    let timeout = Duration::from_millis(q.wait_ms.min(120_000));
    to_json(m.wait_permission(&id, timeout).await?)
}

#[derive(Deserialize)]
struct AnswerBody {
    behavior: Verdict,
    #[serde(default)]
    message: Option<String>,
}

async fn answer_permission(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AnswerBody>,
) -> ApiResult {
    let who = caller(&m, &peer, &headers);
    to_json(m.answer_external(&id, body.behavior, body.message, who)?)
}

async fn stream(State(m): State<Manager>) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let events = BroadcastStream::new(m.subscribe()).filter_map(|msg| {
        let event = msg.ok()?;
        let data = serde_json::to_string(&event).ok()?;
        Some(Ok(SseEvent::default().data(data)))
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}
