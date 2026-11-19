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
//! | PATCH | `/agents/{id}` | `{title}` | snapshot |
//! | POST | `/agents/{id}/send` | `{prompt, activeTurnBehavior}` | `{disposition}` |
//! | POST | `/agents/{id}/stop` | | `{agentId, stopped}` |
//! | POST | `/agents/{id}/archive` | | `{agentId, archivedAt}` |
//! | POST | `/agents/{id}/unarchive` | | snapshot |
//! | DELETE | `/agents/{id}` | | `{agentId, deleted}` |
//! | POST | `/agents/{id}/wait` | `?timeoutMs=` (0 = none) | wait result |
//! | GET | `/agents/{id}/timeline` | `?afterSeq=&limit=` (`&subagent=<id>`: that provider subagent's, see below) | `{epoch, rows, nextSeq}` |
//! | GET | `/agents/{id}/provider-subagents` | | `{agentId, subagents: [descriptor]}` |
//! | GET | `/agents/{id}/provider-subagents/{subId}` | id or unique prefix | descriptor |
//! | GET | `/agents/{id}/provider-subagents/{subId}/timeline` | `?afterSeq=&limit=&direction=after\|tail` | `{agentId, subagentId, provider, direction, epoch, rows, nextSeq, window, hasOlder, hasNewer, subagent}` |
//! | POST | `/agents/{id}/permissions/{requestId}` | permission response | `{agentId, requestId, behavior}` |
//! | GET | `/permissions` | `?all=true` | `{permissions: [view]}` (agent and external) |
//! | POST | `/permissions` | external request | view |
//! | GET | `/permissions/{id}` | `?waitMs=` (long-poll, up to 120 s) | view |
//! | POST | `/permissions/{id}` | `{behavior, message}` | view |
//! | GET | `/stream` | `Last-Event-ID` (or `?lastEventId=`) | SSE of numbered bus events, resumable |
//! | POST | `/agents/{id}/events` | `{events: [StreamEvent]}` | `{agentId, applied}` (external agents, owner device only) |
//! | POST | `/agents/{id}/commands/{commandId}/ack` | | `{agentId, commandId, acked}` (owner device only) |
//! | GET | `/commands` | | `{commands: [AgentCommand]}` waiting for the calling device |
//!
//! The stream: every event has an `id:` (see [`crate::journal`]). A client that
//! reconnects with `Last-Event-ID` gets the kept events after it, or `event:
//! stream.reset` when it fell too far behind. `agent_command` events go only to the
//! device that owns the agent; a device that connects without `Last-Event-ID` first
//! gets its waiting commands (without ids).
//!
//! Projects, workspaces, diffs, files and checkout operations: [`workspaces`].
//! Terminals, scripts and the service proxy: [`terminals`].
//! Schedules, heartbeats, search, prompts, detach and usage: [`orch`].
//! Rewind and fork (with chat-history context): [`rewind`].
//! The Homelab service catalog (project tree, health, impact, drift): [`catalog`].
//! Tasks, the merge and deploy queue, device leases and the board: [`pipeline`].
//! Installs on leased devices (the iPhone) and test-run grants: [`devices`].
//! Work jobs that run through `lane`: [`work`].
//! Identifiers of runs, changes, versions and findings: [`ids`].
//!
//! A process an agent started acts as that agent ([`Caller::Agent`]): its `create`
//! makes a subagent (depth-limited, supervised), its `send` is a queued agent message,
//! and it may stop, archive, rename or delete only its own subagents.
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
use lucina_agents::external::EventsBody;
use lucina_agents::stream::Resume;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use lucina_agents::permission_gate::{Origin, Verdict};
use lucina_agents::PermissionResponse;

use crate::journal::{Sequenced, Subscription};
use crate::manager::{
    ActiveTurnBehavior, Caller, CreateRequest, ExternalRequest, Manager, ManagerError,
    SubagentDirection,
};
use crate::peer::{self, origin, Peer};
use crate::workspaces::WsError;

mod catalog;
mod devices;
mod ids;
mod orch;
mod pipeline;
mod rewind;
mod sessions;
pub mod terminals;
mod work;
mod workspaces;

pub struct ApiError(StatusCode, String, String);

impl From<ManagerError> for ApiError {
    fn from(e: ManagerError) -> Self {
        let status = match e {
            ManagerError::NotFound(_) => StatusCode::NOT_FOUND,
            ManagerError::Ambiguous(_)
            | ManagerError::Invalid(_)
            | ManagerError::UnknownProvider(_) => StatusCode::BAD_REQUEST,
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
            ManagerError::NotOwner(_) => StatusCode::FORBIDDEN,
            ManagerError::CommandNotFound(_) => StatusCode::NOT_FOUND,
            ManagerError::CommandExpired(_) => StatusCode::GONE,
            ManagerError::DelegationDepth(_)
            | ManagerError::NotYourAgent(_)
            | ManagerError::ScheduleForbidden(_) => StatusCode::FORBIDDEN,
            ManagerError::ScheduleNotFound(_) | ManagerError::SubagentNotFound(_) => {
                StatusCode::NOT_FOUND
            }
            ManagerError::ScheduleConflict(_) => StatusCode::CONFLICT,
            ManagerError::RewindUnsupported(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ManagerError::RewindTargetNotFound(_) => StatusCode::NOT_FOUND,
            ManagerError::RewindForbidden(_) => StatusCode::FORBIDDEN,
            ManagerError::ForkBoundary(_) | ManagerError::Busy(_) => StatusCode::CONFLICT,
            ManagerError::CodexNotAllowed(_) => StatusCode::FORBIDDEN,
            ManagerError::JunoToolRefused(_) => StatusCode::FORBIDDEN,
            ManagerError::WorkNotFound(_) | ManagerError::IdNotFound(_) => StatusCode::NOT_FOUND,
            ManagerError::TooManyAgents(_) => StatusCode::TOO_MANY_REQUESTS,
            ManagerError::ProviderUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        };
        ApiError(status, e.code().to_string(), e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(json!({"error": {"code": self.1, "message": self.2}})),
        )
            .into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

fn to_json<T: serde::Serialize>(v: T) -> ApiResult {
    serde_json::to_value(v).map(Json).map_err(|e| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "SERIALIZE".into(),
            e.to_string(),
        )
    })
}

pub fn router(manager: Manager) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/providers", get(providers))
        .route("/providers/{id}/clear", post(clear_provider))
        .route("/agents", get(list).post(create))
        .route("/agents/items", get(items))
        .route("/agents/{id}", get(inspect).delete(delete).patch(rename))
        .route("/agents/{id}/events", post(post_events))
        .route("/agents/{id}/commands/{command_id}/ack", post(ack_command))
        .route("/commands", get(commands))
        .route("/agents/{id}/send", post(send))
        .route("/agents/{id}/stop", post(stop))
        .route("/agents/{id}/archive", post(archive))
        .route("/agents/{id}/unarchive", post(unarchive))
        .route("/agents/{id}/wait", post(wait))
        .route("/agents/{id}/timeline", get(timeline))
        .route("/agents/{id}/provider-subagents", get(provider_subagents))
        .route(
            "/agents/{id}/provider-subagents/{sub}",
            get(provider_subagent),
        )
        .route(
            "/agents/{id}/provider-subagents/{sub}/timeline",
            get(provider_subagent_timeline),
        )
        .route(
            "/agents/{id}/permissions/{request_id}",
            post(respond_permission),
        )
        .route("/permissions", get(permissions).post(request_permission))
        .route("/permissions/{id}", get(permission).post(answer_permission))
        .route("/stream", get(stream))
        .merge(workspaces::routes())
        .merge(sessions::routes())
        .merge(terminals::routes())
        .merge(orch::routes())
        .merge(rewind::routes())
        .merge(catalog::routes())
        .merge(pipeline::routes())
        .merge(devices::routes())
        .merge(work::routes())
        .merge(ids::routes())
        // Last, so it sees every request (`Host: <label>.localhost` goes to that service).
        .layer(axum::middleware::from_fn_with_state(
            manager.clone(),
            terminals::host_proxy,
        ))
        .with_state(manager)
}

async fn health() -> Json<Value> {
    Json(json!({"ok": true, "version": env!("CARGO_PKG_VERSION")}))
}

async fn providers(State(m): State<Manager>) -> ApiResult {
    to_json(m.providers())
}

/// `POST /providers/{id}/clear` (`lagent providers clear <provider>`): the owner lets agents start
/// for a provider whose breaker tripped (see `manager/breaker.rs`).
async fn clear_provider(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    if !who(&m, &peer, &headers).is_owner() {
        return Err(
            ManagerError::Forbidden("Only the owner clears a provider's breaker".into()).into(),
        );
    }
    let cleared = m.clear_provider(&id);
    to_json(json!({"provider": id, "cleared": cleared}))
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateBody {
    #[serde(flatten)]
    req: CreateRequest,
    /// The calling agent hears when the child's run ends (default: yes for an agent).
    #[serde(default)]
    notify_on_finish: Option<bool>,
}

/// An owner's text that looks like an agent envelope is quoted as the owner's
/// (Paseo `prepareAgentMessage`), so nobody can pose as an agent or a schedule.
pub(super) fn quote(text: String) -> String {
    lucina_agents::envelope::quote_reserved(&text, &uuid::Uuid::new_v4().to_string())
}

async fn create(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Json(body): Json<CreateBody>,
) -> ApiResult {
    let CreateBody {
        mut req,
        notify_on_finish,
    } = body;
    // Only the owner may run an agent in a main checkout (worktree guard).
    let origin = caller(&m, &peer, &headers);
    if req.allow_main_checkout && !matches!(origin, Origin::Local | Origin::OwnerDevice) {
        req.allow_main_checkout = false;
    }
    // An external agent belongs to the device registering it, never to a body field.
    req.owner_device_id = peer::device(origin, &headers);
    // Set here from the origin, never from a body field: only such a run alerts him when it finishes.
    req.labels.remove(crate::manager::STARTED_ON_DEVICE_LABEL);
    if origin == Origin::OwnerDevice {
        req.labels.insert(
            crate::manager::STARTED_ON_DEVICE_LABEL.into(),
            "true".into(),
        );
    }
    req.prompt = req.prompt.map(quote);
    let caller = who(&m, &peer, &headers);
    // The create caps count by this identity; never from the body.
    req.created_by = Some(caller.origin());
    let notify = m.prepare_create(&caller, &mut req)? && notify_on_finish.unwrap_or(true);
    let prompted = req.prompt.as_deref().is_some_and(|p| !p.trim().is_empty());
    let snap = m.create(req).await?;
    if let (Caller::Agent(parent), true) = (&caller, notify) {
        m.subscribe_finish(parent, &snap.id, prompted);
    }
    to_json(snap)
}

/// Who is calling, with the agent behind an agent's process.
pub(super) fn who(m: &Manager, peer: &Peer, headers: &HeaderMap) -> Caller {
    let origin = caller(m, peer, headers);
    let header = headers
        .get(peer::ORIGIN_HEADER)
        .and_then(|v| v.to_str().ok());
    let agent = if origin == Origin::Agent {
        m.calling_agent(peer.pid)
    } else {
        None
    };
    Caller::from_origin(origin, header, agent)
}

async fn inspect(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    let id = m.resolve(&id)?;
    to_json(m.snapshot(&id)?)
}

#[derive(Deserialize)]
struct RenameBody {
    title: String,
}

async fn rename(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RenameBody>,
) -> ApiResult {
    let id = m.resolve(&id)?;
    m.may_touch(&who(&m, &peer, &headers), &id)?;
    to_json(m.rename(&id, &body.title)?)
}

fn device_of(m: &Manager, peer: &Peer, headers: &HeaderMap) -> Option<String> {
    peer::device(caller(m, peer, headers), headers)
}

async fn post_events(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<EventsBody>,
) -> ApiResult {
    let id = m.resolve(&id)?;
    let applied = m
        .post_events(&id, device_of(&m, &peer, &headers).as_deref(), body.events)
        .await?;
    to_json(json!({"agentId": id, "applied": applied}))
}

async fn ack_command(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Path((id, command_id)): Path<(String, String)>,
) -> ApiResult {
    let id = m.resolve(&id)?;
    m.ack_command(&id, &command_id, device_of(&m, &peer, &headers).as_deref())?;
    to_json(json!({"agentId": id, "commandId": command_id, "acked": true}))
}

async fn commands(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
) -> ApiResult {
    to_json(json!({"commands": m.pending_commands(device_of(&m, &peer, &headers).as_deref())?}))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendBody {
    prompt: String,
    #[serde(default)]
    active_turn_behavior: ActiveTurnBehavior,
    /// From an agent: hear when the recipient's run ends (default: yes for a subagent).
    #[serde(default)]
    notify_on_finish: Option<bool>,
}

async fn send(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<SendBody>,
) -> ApiResult {
    let id = m.resolve(&id)?;
    match who(&m, &peer, &headers) {
        Caller::Agent(from) => {
            if body.prompt.trim().is_empty() {
                return Err(ManagerError::Invalid("A prompt is required".into()).into());
            }
            m.may_message(&from, &id)?;
            let notify = body.notify_on_finish.unwrap_or(true) && m.is_child_of(&id, &from);
            let message_id = m.send_from_agent(&from, &id, &body.prompt, notify)?;
            to_json(
                json!({"agentId": id, "disposition": crate::manager::Disposition::Queued, "deliveryStatus": "queued", "messageId": message_id}),
            )
        }
        _ => {
            let disposition = m
                .send(&id, &quote(body.prompt), body.active_turn_behavior)
                .await?;
            to_json(json!({"agentId": id, "disposition": disposition}))
        }
    }
}

async fn stop(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    let id = m.resolve(&id)?;
    m.may_touch(&who(&m, &peer, &headers), &id)?;
    let stopped = m.stop(&id).await?;
    to_json(json!({"agentId": id, "stopped": stopped}))
}

async fn archive(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    let id = m.resolve(&id)?;
    m.may_touch(&who(&m, &peer, &headers), &id)?;
    let archived_at = m.archive(&id).await?;
    to_json(json!({"agentId": id, "archivedAt": archived_at}))
}

async fn unarchive(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    let id = m.resolve(&id)?;
    m.may_touch(&who(&m, &peer, &headers), &id)?;
    to_json(m.unarchive(&id)?)
}

async fn delete(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult {
    let id = m.resolve(&id)?;
    m.may_touch(&who(&m, &peer, &headers), &id)?;
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

async fn wait(
    State(m): State<Manager>,
    Path(id): Path<String>,
    Query(q): Query<WaitQuery>,
) -> ApiResult {
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
    /// A provider subagent of the agent (id or unique prefix): its timeline, as
    /// `/provider-subagents/{subId}/timeline` returns it.
    #[serde(default)]
    subagent: Option<String>,
    #[serde(default)]
    direction: Option<String>,
}

fn direction(q: &TimelineQuery) -> Result<SubagentDirection, ManagerError> {
    match q.direction.as_deref() {
        None | Some("after") => Ok(SubagentDirection::After),
        Some("tail") => Ok(SubagentDirection::Tail),
        Some(other) => Err(ManagerError::Invalid(format!(
            "Unknown timeline direction: {other} (after or tail)"
        ))),
    }
}

async fn timeline(
    State(m): State<Manager>,
    Path(id): Path<String>,
    Query(q): Query<TimelineQuery>,
) -> ApiResult {
    let id = m.resolve(&id)?;
    if let Some(sub) = &q.subagent {
        return to_json(m.provider_subagent_timeline(
            &id,
            sub,
            direction(&q)?,
            q.after_seq,
            q.limit,
        )?);
    }
    let (epoch, rows) = m.timeline(&id, q.after_seq, q.limit)?;
    let next_seq = rows.last().map(|r| r.seq).unwrap_or(q.after_seq);
    to_json(json!({"epoch": epoch, "rows": rows, "nextSeq": next_seq}))
}

async fn provider_subagents(State(m): State<Manager>, Path(id): Path<String>) -> ApiResult {
    let id = m.resolve(&id)?;
    to_json(json!({"agentId": id, "subagents": m.provider_subagents(&id)?}))
}

async fn provider_subagent(
    State(m): State<Manager>,
    Path((id, sub)): Path<(String, String)>,
) -> ApiResult {
    let id = m.resolve(&id)?;
    to_json(m.provider_subagent(&id, &sub)?)
}

async fn provider_subagent_timeline(
    State(m): State<Manager>,
    Path((id, sub)): Path<(String, String)>,
    Query(q): Query<TimelineQuery>,
) -> ApiResult {
    let id = m.resolve(&id)?;
    to_json(m.provider_subagent_timeline(&id, &sub, direction(&q)?, q.after_seq, q.limit)?)
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
    m.respond_permission(&id, &request_id, response, who)
        .await?;
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
    let who = peer::requester(&peer, &headers, &m.provider_pids());
    let view = m.request_external(req, who)?;
    Ok((StatusCode::CREATED, to_json(view)?))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PermissionQuery {
    #[serde(default)]
    wait_ms: u64,
}

async fn permission(
    State(m): State<Manager>,
    Path(id): Path<String>,
    Query(q): Query<PermissionQuery>,
) -> ApiResult {
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

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct StreamQuery {
    #[serde(default)]
    last_event_id: Option<String>,
}

const LAST_EVENT_ID: &str = "last-event-id";
const RESET_DATA: &str = r#"{"type":"stream.reset"}"#;

fn numbered(e: &Sequenced) -> SseEvent {
    SseEvent::default().id(e.id.to_string()).data(&e.data)
}

fn reset(id: u64) -> SseEvent {
    SseEvent::default()
        .event("stream.reset")
        .id(id.to_string())
        .data(RESET_DATA)
}

type Frames = mpsc::Sender<Result<SseEvent, Infallible>>;

async fn stream(
    State(m): State<Manager>,
    ConnectInfo(peer): ConnectInfo<Peer>,
    headers: HeaderMap,
    Query(q): Query<StreamQuery>,
) -> Response {
    let device = device_of(&m, &peer, &headers);
    let last = headers
        .get(LAST_EVENT_ID)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .or(q.last_event_id);
    let sub = m.journal().subscribe(last.as_deref());
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(pump(m, sub, device, tx));
    Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Feed one stream: the reset or replay, then live events in id order. A subscriber
/// that lags behind the bus catches up from the journal (or gets a reset).
async fn pump(m: Manager, sub: Subscription, device: Option<String>, tx: Frames) {
    let Subscription {
        resume,
        replay,
        last,
        mut live,
    } = sub;
    let visible = |e: &Sequenced| e.event.visible_to(device.as_deref());
    match resume {
        Resume::Reset => {
            if tx.send(Ok(reset(last))).await.is_err() {
                return;
            }
        }
        Resume::Live => {
            for c in m.pending_commands(device.as_deref()).unwrap_or_default() {
                let mut v = serde_json::to_value(&c).unwrap_or_default();
                v["type"] = json!("agent_command");
                if tx
                    .send(Ok(SseEvent::default().data(v.to_string())))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
        Resume::After(_) => {}
    }
    for e in replay.iter().filter(|e| visible(e)) {
        if tx.send(Ok(numbered(e))).await.is_err() {
            return;
        }
    }
    let mut sent = last;
    let mut closing = m.closing();
    loop {
        let next = tokio::select! {
            r = live.recv() => r,
            _ = tx.closed() => return,
            _ = closing.wait_for(|c| *c) => return,
        };
        let batch = match next {
            Ok(e) => vec![e],
            Err(RecvError::Lagged(_)) => match m.journal().since(sent) {
                Some(missed) => missed,
                None => {
                    sent = m.journal().last_id();
                    if tx.send(Ok(reset(sent))).await.is_err() {
                        return;
                    }
                    continue;
                }
            },
            Err(RecvError::Closed) => return,
        };
        for e in batch {
            if e.id <= sent {
                continue;
            }
            sent = e.id;
            if visible(&e) && tx.send(Ok(numbered(&e))).await.is_err() {
                return;
            }
        }
    }
}
