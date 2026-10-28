//! Terminals, scripts and services (phase 5; contract `docs/design/agents-api.md`).
//!
//! | Method | Path | Body / query | Returns |
//! |---|---|---|---|
//! | GET/POST | `/terminals` | `?workspaceId=&cwd=` / `{workspaceId, cwd, name, command, args, rows, cols, agentId}` | `{terminals}` / terminal |
//! | GET/PATCH/DELETE | `/terminals/{id}` | id, prefix or name / `{title}` | terminal / `{terminalId, success}` |
//! | GET | `/terminals/{id}/capture` | `?start=&end=&scrollback=&stripAnsi=` | `{terminalId, lines, totalLines}` |
//! | POST | `/terminals/{id}/input` | `{data}` or `{keys, literal}` | `{terminalId, keysSent}` |
//! | POST | `/terminals/{id}/resize` | `{rows, cols}` | terminal |
//! | POST | `/terminals/{id}/paste` | `{name, dataBase64}` | `{terminalId, path}` |
//! | GET | `/terminals/{id}/stream` | `Last-Event-ID` or `?offset=` | SSE: `snapshot`, `output` (`id:` = offset), `exit` |
//! | POST | `/terminal-activity` | `{terminalId, token, state}` | 204 (`lagent hooks`, the terminal's token) |
//! | GET | `/workspaces/{id}/scripts` | | `{workspaceId, scripts}` |
//! | POST | `/workspaces/{id}/scripts/{name}/start\|stop` | | script |
//! | GET | `/workspaces/{id}/scripts/{name}/logs` | `?lines=` | `{workspaceId, scriptName, terminalId, lines, totalLines}` |
//! | GET | `/workspaces/{id}/services` | | `{workspaceId, services}` |
//! | any | `/workspaces/{id}/services/{name}/{path}` | | the service (owner only) |
//!
//! Who may do what (the caller is a [`Principal`]):
//! - the owner (`lagent`, the owner's devices): everything;
//! - an agent (a process under its provider): the terminals it serves or opened, and
//!   new terminals and scripts in its own workspace, inside the workspace directory;
//! - an MCP token: list and read the terminals of agents it started and the ones its
//!   scripts run in, and type only into those it or its agents opened (not one the
//!   owner opened for its agent); scripts of the workspaces it made (label [`mcp_label`])
//!   or runs agents in. No new raw terminals, no kill, no services;
//! - a process in a terminal no agent owns: that terminal only.
//!
//! On agentd's loopback TCP listener, a `Host: <label>.localhost` request goes to that
//! running service (Paseo's hostname proxy; HTTP only).

use std::collections::{BTreeMap, HashSet};
use std::convert::Infallible;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use base64::Engine;
use futures_util::StreamExt;
use lucina_agents::permission_gate::Origin;
use lucina_agents::workspace::WorkspaceRecord;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use super::{to_json, ApiError, ApiResult};
use crate::manager::Manager;
use crate::peer::{self, Peer};
use crate::terminals::procs::{self, Table};
use crate::terminals::scripts::{self, Ctx};
use crate::terminals::{keys, Frame, Hub, Spawn, Start, Term};

/// The label an MCP token's workspaces carry: `mcp:` and a hash of the token id
/// (labels are lowercased, token ids are not).
pub fn mcp_label(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let h: String = Sha256::digest(token.as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("mcp:{h}")
}

pub const MCP_TOKEN_LABEL: &str = "lucina.mcp-token";

/// Who is calling, for terminals and scripts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    Owner,
    Agent(String),
    Mcp(String),
    /// A process in a terminal that no agent owns.
    Terminal(String),
    Unknown,
}

impl Principal {
    fn owner_label(&self) -> String {
        match self {
            Principal::Owner => "owner".into(),
            Principal::Agent(a) => format!("agent:{a}"),
            Principal::Mcp(t) => format!("mcp:{t}"),
            Principal::Terminal(t) => format!("terminal:{t}"),
            Principal::Unknown => "unknown".into(),
        }
    }
}

/// The MCP token in `X-Lucina-Origin: mcp:<token>` (`unknown` without one).
pub fn mcp_token(headers: &HeaderMap) -> Option<String> {
    let h = headers.get(peer::ORIGIN_HEADER)?.to_str().ok()?;
    match h {
        "mcp" => Some("unknown".into()),
        _ => h.strip_prefix("mcp:").filter(|t| !t.is_empty()).map(str::to_string),
    }
}

pub fn principal(m: &Manager, p: &Peer, headers: &HeaderMap) -> Principal {
    if !p.unix {
        return Principal::Unknown;
    }
    let Some(pid) = p.pid else { return Principal::Unknown };
    if let Some(leader) = peer::provider_of(pid, &m.provider_pids()) {
        if let Some(t) = m.terminals().by_pid(leader) {
            return match &t.agent_id {
                Some(a) => Principal::Agent(a.clone()),
                None => Principal::Terminal(t.id.clone()),
            };
        }
        return procs::agent_id_of(leader).map(Principal::Agent).unwrap_or(Principal::Unknown);
    }
    match peer::origin(p, headers, &m.provider_pids()) {
        Origin::Local | Origin::OwnerDevice => Principal::Owner,
        Origin::Mcp => Principal::Mcp(mcp_token(headers).unwrap_or_else(|| "unknown".into())),
        _ => Principal::Unknown,
    }
}

fn err(status: StatusCode, code: &str, message: impl Into<String>) -> ApiError {
    ApiError(status, code.into(), message.into())
}

fn forbidden(message: impl Into<String>) -> ApiError {
    err(StatusCode::FORBIDDEN, "TERMINAL_FORBIDDEN", message)
}

fn agent_token(m: &Manager, agent: &str) -> Option<String> {
    m.snapshot(agent).ok().and_then(|s| s.labels.get(MCP_TOKEN_LABEL).cloned())
}

/// Whether `who` may see and type into `t`.
pub fn may_use(m: &Manager, who: &Principal, t: &Term) -> bool {
    match who {
        Principal::Owner => true,
        Principal::Agent(a) => t.agent_id.as_deref() == Some(a.as_str()) || t.owner == format!("agent:{a}"),
        Principal::Mcp(tok) => t.owner == format!("mcp:{tok}") || t.agent_id.as_deref().is_some_and(|a| agent_token(m, a).as_deref() == Some(tok.as_str())),
        Principal::Terminal(id) => &t.id == id,
        Principal::Unknown => false,
    }
}

/// Whether `who` may type into, resize, rename or paste into `t`. An MCP token
/// only into terminals it opened (its scripts) or its agents opened, never one the
/// owner opened for its agent (design: "keys into a terminal the agent owns").
pub fn may_write(m: &Manager, who: &Principal, t: &Term) -> bool {
    match who {
        Principal::Mcp(tok) => {
            t.owner == format!("mcp:{tok}") || t.owner.strip_prefix("agent:").is_some_and(|a| agent_token(m, a).as_deref() == Some(tok.as_str()))
        }
        other => may_use(m, other, t),
    }
}

/// The terminal `id` names, for writing (403 when `who` may only read it).
fn term_mut(m: &Manager, who: &Principal, id: &str) -> Result<Arc<Term>, ApiError> {
    let t = term(m, who, id)?;
    if !may_write(m, who, &t) {
        return Err(forbidden("MCP callers type only into terminals they or their agents opened"));
    }
    Ok(t)
}

/// Whether `who` may run scripts in (and open terminals for) `w`.
pub fn may_use_workspace(m: &Manager, who: &Principal, w: &WorkspaceRecord) -> bool {
    match who {
        Principal::Owner => true,
        Principal::Agent(a) => m.snapshot(a).ok().is_some_and(|s| s.workspace_id.as_deref() == Some(w.workspace_id.as_str())),
        Principal::Mcp(tok) => {
            w.labels.contains(&mcp_label(tok))
                || m.snapshots(true).iter().any(|s| s.workspace_id.as_deref() == Some(w.workspace_id.as_str()) && s.labels.get(MCP_TOKEN_LABEL) == Some(tok))
        }
        Principal::Terminal(_) | Principal::Unknown => false,
    }
}

pub fn routes() -> Router<Manager> {
    Router::new()
        .route("/terminals", get(list).post(create))
        .route("/terminals/{id}", get(inspect).patch(rename).delete(kill))
        .route("/terminals/{id}/capture", get(capture))
        .route("/terminals/{id}/input", post(input))
        .route("/terminals/{id}/resize", post(resize))
        .route("/terminals/{id}/paste", post(paste))
        .route("/terminals/{id}/stream", get(stream))
        .route("/terminal-activity", post(activity))
        .route("/workspaces/{id}/scripts", get(scripts_list))
        .route("/workspaces/{id}/scripts/{name}/start", post(script_start))
        .route("/workspaces/{id}/scripts/{name}/stop", post(script_stop))
        .route("/workspaces/{id}/scripts/{name}/logs", get(script_logs))
        .route("/workspaces/{id}/services", get(services))
        .route("/workspaces/{id}/services/{name}", any(proxy_root))
        .route("/workspaces/{id}/services/{name}/", any(proxy_root))
        .route("/workspaces/{id}/services/{name}/{*rest}", any(proxy_rest))
}

fn visible(m: &Manager, who: &Principal) -> Vec<Arc<Term>> {
    m.terminals().all().into_iter().filter(|t| may_use(m, who, t)).collect()
}

/// The terminal `id` names, if `who` may use it (404 either way, so ids don't leak).
fn term(m: &Manager, who: &Principal, id: &str) -> Result<Arc<Term>, ApiError> {
    Hub::resolve_in(&visible(m, who), id).ok_or_else(|| err(StatusCode::NOT_FOUND, "TERMINAL_NOT_FOUND", format!("No terminal found matching: {id}")))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ListQuery {
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
}

async fn list(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Query(q): Query<ListQuery>) -> ApiResult {
    let who = principal(&m, &p, &headers);
    let terms: Vec<Arc<Term>> = visible(&m, &who)
        .into_iter()
        .filter(|t| q.workspace_id.as_ref().is_none_or(|w| &t.workspace_id == w))
        .filter(|t| q.cwd.as_ref().is_none_or(|c| t.cwd == c.trim_end_matches('/')))
        .collect();
    to_json(json!({"terminals": m.terminals().views(&terms)}))
}

fn workspace(m: &Manager, id: &str) -> Result<WorkspaceRecord, ApiError> {
    m.workspaces()
        .get(id)
        .filter(|w| w.archived_at.is_none())
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "WORKSPACE_NOT_FOUND", format!("Workspace not found: {id}")))
}

/// `cwd` inside the workspace directory, symlinks resolved.
fn inside(root: &str, cwd: Option<&str>) -> Result<PathBuf, ApiError> {
    let root = std::fs::canonicalize(root).map_err(|e| err(StatusCode::BAD_REQUEST, "INVALID_CWD", e.to_string()))?;
    let Some(c) = cwd.filter(|c| !c.is_empty()) else { return Ok(root) };
    let p = if FsPath::new(c).is_absolute() { PathBuf::from(c) } else { root.join(c) };
    let p = std::fs::canonicalize(&p).map_err(|e| err(StatusCode::BAD_REQUEST, "INVALID_CWD", format!("{c}: {e}")))?;
    if !p.starts_with(&root) {
        return Err(forbidden(format!("{c} is outside the workspace")));
    }
    Ok(p)
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CreateBody {
    workspace_id: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    rows: u16,
    #[serde(default)]
    cols: u16,
    /// The agent this terminal serves (owner only; an agent's terminals are its own).
    #[serde(default)]
    agent_id: Option<String>,
}

async fn create(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Json(b): Json<CreateBody>) -> ApiResult {
    let who = principal(&m, &p, &headers);
    let w = workspace(&m, &b.workspace_id)?;
    let agent_id = match &who {
        Principal::Owner => match &b.agent_id {
            Some(a) => Some(m.resolve(a)?),
            None => None,
        },
        Principal::Agent(a) if may_use_workspace(&m, &who, &w) => Some(a.clone()),
        Principal::Agent(_) => return Err(forbidden("Agents open terminals only in their own workspace")),
        _ => return Err(forbidden("Only the owner and agents open terminals")),
    };
    let cwd = match who {
        Principal::Owner => match b.cwd.as_deref().filter(|c| !c.is_empty()) {
            Some(c) => std::fs::canonicalize(c).map_err(|e| err(StatusCode::BAD_REQUEST, "INVALID_CWD", format!("{c}: {e}")))?,
            None => PathBuf::from(&w.cwd),
        },
        _ => inside(&w.cwd, b.cwd.as_deref())?,
    };
    if !cwd.is_dir() {
        return Err(err(StatusCode::BAD_REQUEST, "INVALID_CWD", format!("Not a directory: {}", cwd.display())));
    }
    let t = m
        .terminals()
        .spawn(Spawn {
            name: b.name.unwrap_or_default(),
            cwd,
            workspace_id: w.workspace_id.clone(),
            agent_id,
            owner: who.owner_label(),
            program: b.command.filter(|c| !c.trim().is_empty()),
            args: b.args,
            rows: b.rows,
            cols: b.cols,
            ..Spawn::default()
        })
        .map_err(|e| err(StatusCode::BAD_REQUEST, "TERMINAL_CREATE_FAILED", e))?;
    to_json(t.view())
}

async fn inspect(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let t = term(&m, &principal(&m, &p, &headers), &id)?;
    to_json(m.terminals().views(&[t]).pop())
}

#[derive(Deserialize)]
struct TitleBody {
    title: String,
}

async fn rename(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(b): Json<TitleBody>) -> ApiResult {
    let t = term_mut(&m, &principal(&m, &p, &headers), &id)?;
    to_json(m.terminals().rename(&t.id, &b.title))
}

async fn kill(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let who = principal(&m, &p, &headers);
    let t = term(&m, &who, &id)?;
    if matches!(who, Principal::Mcp(_)) {
        return Err(forbidden("MCP callers can't kill terminals; stop the script or the agent"));
    }
    let ok = m.terminals().kill(&t.id).await;
    to_json(json!({"terminalId": t.id, "success": ok}))
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CaptureQuery {
    #[serde(default)]
    start: Option<i64>,
    #[serde(default)]
    end: Option<i64>,
    #[serde(default)]
    scrollback: bool,
    #[serde(default)]
    strip_ansi: Option<bool>,
}

async fn capture(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<CaptureQuery>) -> ApiResult {
    let t = term(&m, &principal(&m, &p, &headers), &id)?;
    let start = if q.scrollback { Some(0) } else { q.start };
    let (lines, total) = t.capture(start, q.end, !q.strip_ansi.unwrap_or(true));
    to_json(json!({"terminalId": t.id, "lines": lines, "totalLines": total}))
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Keys {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct InputBody {
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    keys: Option<Keys>,
    #[serde(default)]
    literal: bool,
}

async fn input(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(b): Json<InputBody>) -> ApiResult {
    let t = term_mut(&m, &principal(&m, &p, &headers), &id)?;
    let mode = t.input_mode();
    let data = match (b.data, b.keys) {
        (Some(d), None) => d,
        (None, Some(Keys::One(k))) => keys::resolve(&k, b.literal, mode),
        (None, Some(Keys::Many(ks))) if b.literal => ks.concat(),
        (None, Some(Keys::Many(ks))) => ks.iter().map(|k| keys::resolve(k, false, mode)).collect(),
        _ => return Err(err(StatusCode::BAD_REQUEST, "INVALID_INPUT", "Send either data or keys")),
    };
    if data.len() > 64 * 1024 {
        return Err(err(StatusCode::BAD_REQUEST, "INVALID_INPUT", "At most 64 KB of input at a time"));
    }
    t.write(data.as_bytes()).map_err(|e| err(StatusCode::CONFLICT, "TERMINAL_EXITED", e))?;
    to_json(json!({"terminalId": t.id, "keysSent": data.encode_utf16().count()}))
}

#[derive(Deserialize)]
struct ResizeBody {
    rows: u16,
    cols: u16,
}

async fn resize(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(b): Json<ResizeBody>) -> ApiResult {
    let t = term_mut(&m, &principal(&m, &p, &headers), &id)?;
    t.resize(b.rows, b.cols).map_err(|e| err(StatusCode::CONFLICT, "TERMINAL_RESIZE_FAILED", e))?;
    m.terminals().resized(&t);
    to_json(t.view())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PasteBody {
    name: String,
    data_base64: String,
}

async fn paste(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Json(b): Json<PasteBody>) -> ApiResult {
    let t = term_mut(&m, &principal(&m, &p, &headers), &id)?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(b.data_base64.trim()).map_err(|e| err(StatusCode::BAD_REQUEST, "INVALID_PASTE", e.to_string()))?;
    let path = m.terminals().paste(&t, &b.name, &bytes).map_err(|e| err(StatusCode::BAD_REQUEST, "INVALID_PASTE", e))?;
    to_json(json!({"terminalId": t.id, "path": path.display().to_string()}))
}

#[derive(Deserialize, Default)]
struct StreamQuery {
    #[serde(default)]
    offset: Option<u64>,
}

type Frames = mpsc::Sender<Result<SseEvent, Infallible>>;

fn start_event(start: &Start) -> (SseEvent, u64) {
    match start {
        Start::Snapshot { rows, cols, data, offset } => {
            (SseEvent::default().event("snapshot").id(offset.to_string()).data(json!({"rows": rows, "cols": cols, "data": data, "offset": offset}).to_string()), *offset)
        }
        Start::Replay { bytes, end } => (output_event(bytes, *end), *end),
    }
}

fn output_event(bytes: &[u8], end: u64) -> SseEvent {
    SseEvent::default().event("output").id(end.to_string()).data(json!({"b64": Hub::b64(bytes)}).to_string())
}

fn exit_event(code: Option<i32>) -> SseEvent {
    SseEvent::default().event("exit").data(json!({"exitCode": code}).to_string())
}

async fn stream(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<StreamQuery>) -> Result<Response, ApiError> {
    let t = term(&m, &principal(&m, &p, &headers), &id)?;
    let offset = headers.get("last-event-id").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).or(q.offset);
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(pump(m, t, offset, tx));
    Ok(Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default()).into_response())
}

async fn pump(m: Manager, t: Arc<Term>, offset: Option<u64>, tx: Frames) {
    let (start, mut live) = t.attach(offset);
    let (first, mut sent) = start_event(&start);
    if tx.send(Ok(first)).await.is_err() {
        return;
    }
    if let Some(code) = t.exit_code() {
        // Exited before we subscribed: its exit frame may already be gone.
        let _ = tx.send(Ok(exit_event(code))).await;
        return;
    }
    let mut closing = m.closing();
    loop {
        let next = tokio::select! {
            r = live.recv() => r,
            _ = tx.closed() => return,
            _ = closing.wait_for(|c| *c) => return,
        };
        match next {
            Ok(Frame::Output { end, bytes }) => {
                if end <= sent {
                    continue;
                }
                sent = end;
                if tx.send(Ok(output_event(&bytes, end))).await.is_err() {
                    return;
                }
            }
            Ok(Frame::Exit { code }) => {
                let _ = tx.send(Ok(exit_event(code))).await;
                return;
            }
            Err(RecvError::Lagged(_)) => {
                let (start, rx) = t.attach(Some(sent));
                live = rx;
                let (ev, at) = start_event(&start);
                sent = at;
                if tx.send(Ok(ev)).await.is_err() {
                    return;
                }
            }
            Err(RecvError::Closed) => return,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityBody {
    terminal_id: String,
    token: String,
    state: String,
}

async fn activity(State(m): State<Manager>, Json(b): Json<ActivityBody>) -> Response {
    if m.terminals().activity(&b.terminal_id, &b.token, &b.state) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

// -- scripts ---------------------------------------------------------------

pub fn ctx(m: &Manager, w: &WorkspaceRecord) -> Ctx {
    let project = m.workspaces().project(&w.project_id);
    let root = project.as_ref().map(|p| p.root_path.clone()).or_else(|| w.main_repo_root.clone()).unwrap_or_else(|| w.cwd.clone());
    Ctx {
        workspace_id: w.workspace_id.clone(),
        cwd: PathBuf::from(&w.cwd),
        source: PathBuf::from(w.main_repo_root.clone().unwrap_or_else(|| root.clone())),
        branch: w.branch.clone(),
        project_slug: scripts::project_service_slug(&root, &w.project_id),
        worktree_port: w.port,
    }
}

fn scripts_workspace(m: &Manager, who: &Principal, id: &str) -> Result<WorkspaceRecord, ApiError> {
    let w = workspace(m, id)?;
    if !may_use_workspace(m, who, &w) {
        return Err(forbidden("This workspace's scripts are not yours to use"));
    }
    Ok(w)
}

async fn scripts_list(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let w = scripts_workspace(&m, &principal(&m, &p, &headers), &id)?;
    to_json(json!({"workspaceId": id, "scripts": m.terminals().scripts(&ctx(&m, &w))}))
}

async fn script_start(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path((id, name)): Path<(String, String)>) -> ApiResult {
    let who = principal(&m, &p, &headers);
    let w = scripts_workspace(&m, &who, &id)?;
    if let Some(u) = &w.untrusted_source {
        return Err(forbidden(format!("Scripts are blocked for PR #{} from {}. The owner allows them with `lagent workspace setup`.", u.number, u.head_repository)));
    }
    let agent = match &who {
        Principal::Agent(a) => Some(a.clone()),
        _ => None,
    };
    let script = m
        .terminals()
        .start_script(&ctx(&m, &w), &name, &who.owner_label(), agent)
        .await
        .map_err(|e| err(StatusCode::BAD_REQUEST, "WORKSPACE_SCRIPT_START_FAILED", e))?;
    to_json(script)
}

async fn script_stop(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path((id, name)): Path<(String, String)>) -> ApiResult {
    let w = scripts_workspace(&m, &principal(&m, &p, &headers), &id)?;
    let script = m.terminals().stop_script(&ctx(&m, &w), &name).await.map_err(|e| err(StatusCode::BAD_REQUEST, "WORKSPACE_SCRIPT_STOP_FAILED", e))?;
    to_json(script)
}

#[derive(Deserialize, Default)]
struct LogsQuery {
    #[serde(default)]
    lines: Option<i64>,
}

async fn script_logs(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path((id, name)): Path<(String, String)>, Query(q): Query<LogsQuery>) -> ApiResult {
    let w = scripts_workspace(&m, &principal(&m, &p, &headers), &id)?;
    let script = m.terminals().scripts(&ctx(&m, &w)).into_iter().find(|s| s["scriptName"] == name.as_str());
    let terminal = script.as_ref().and_then(|s| s["terminalId"].as_str()).and_then(|t| m.terminals().get(t));
    let Some(t) = terminal else {
        return Err(err(StatusCode::NOT_FOUND, "SCRIPT_NOT_STARTED", format!("Script '{name}' has no output (not started since agentd started)")));
    };
    let n = q.lines.unwrap_or(200).clamp(1, 10_000);
    let (lines, total) = t.capture(Some(-n), None, false);
    // Drop the empty rows below the last output.
    let mut lines = lines;
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    to_json(json!({"workspaceId": id, "scriptName": name, "terminalId": t.id, "lines": lines, "totalLines": total}))
}

/// Ports the workspace's terminals and agents listen on: `port -> (pid, terminal, agent)`.
fn detected(m: &Manager, w: &WorkspaceRecord) -> BTreeMap<u16, (u32, Option<String>, Option<String>)> {
    let table = Table::read();
    let mut out = BTreeMap::new();
    for t in m.terminals().all().iter().filter(|t| t.workspace_id == w.workspace_id && t.running()) {
        let Some(pid) = t.pid else { continue };
        for l in procs::listening(&table.members(&[pid].into(), &HashSet::new())) {
            out.entry(l.port).or_insert((l.pid, Some(t.id.clone()), t.agent_id.clone()));
        }
    }
    let agents: HashSet<String> = m.snapshots(false).into_iter().filter(|s| s.workspace_id.as_deref() == Some(w.workspace_id.as_str())).map(|s| s.id).collect();
    let terminals = m.terminals().session_pids();
    for pid in m.provider_pids().into_iter().filter(|p| !terminals.contains(p)) {
        let Some(agent) = procs::agent_id_of(pid).filter(|a| agents.contains(a)) else { continue };
        for l in procs::listening(&table.members(&HashSet::new(), &[pid].into())) {
            out.entry(l.port).or_insert((l.pid, None, Some(agent.clone())));
        }
    }
    out
}

async fn services(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    let w = scripts_workspace(&m, &principal(&m, &p, &headers), &id)?;
    let running = m.terminals().running_services(&id);
    let mut out: Vec<Value> = running
        .iter()
        .map(|(name, port, term, health)| {
            let agent = m.terminals().get(term).and_then(|t| t.agent_id.clone());
            json!({"name": name, "kind": "script", "scriptName": name, "port": port, "terminalId": term, "agentId": agent, "pid": null, "health": health, "path": scripts::service_path(&id, name)})
        })
        .collect();
    let taken: HashSet<u16> = running.iter().map(|r| r.1).collect();
    for (port, (pid, term, agent)) in detected(&m, &w) {
        if taken.contains(&port) {
            continue;
        }
        let name = format!("port-{port}");
        out.push(json!({"name": name, "kind": "detected", "scriptName": null, "port": port, "terminalId": term, "agentId": agent, "pid": pid, "health": null, "path": scripts::service_path(&id, &name)}));
    }
    to_json(json!({"workspaceId": id, "services": out}))
}

// -- the service proxy ------------------------------------------------------

const HOP: [&str; 9] = ["connection", "transfer-encoding", "keep-alive", "upgrade", "proxy-connection", "proxy-authenticate", "proxy-authorization", "te", "trailer"];
/// The proxied app gets an opaque origin: it can't use the Secretary session.
pub const SANDBOX_CSP: &str = "sandbox allow-scripts allow-forms allow-popups allow-modals allow-downloads";
const PROXY_BODY_LIMIT: usize = 16 << 20;

async fn proxy_root(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, Path((id, name)): Path<(String, String)>, req: Request) -> Response {
    proxy(m, p, id, name, String::new(), req).await
}

async fn proxy_rest(State(m): State<Manager>, ConnectInfo(p): ConnectInfo<Peer>, Path((id, name, rest)): Path<(String, String, String)>, req: Request) -> Response {
    proxy(m, p, id, name, rest, req).await
}

async fn proxy(m: Manager, p: Peer, id: String, name: String, rest: String, req: Request) -> Response {
    if principal(&m, &p, req.headers()) != Principal::Owner {
        return forbidden("Only the owner opens workspace services").into_response();
    }
    let w = match workspace(&m, &id) {
        Ok(w) => w,
        Err(e) => return e.into_response(),
    };
    let port = m.terminals().service_port(&id, &name).or_else(|| {
        let n: u16 = name.strip_prefix("port-")?.parse().ok()?;
        detected(&m, &w).contains_key(&n).then_some(n)
    });
    let Some(port) = port else {
        return err(StatusCode::NOT_FOUND, "SERVICE_NOT_RUNNING", format!("No running service '{name}' in workspace {id}")).into_response();
    };
    forward(port, &rest, Some(&scripts::service_path(&id, &name)), req).await
}

/// Forward `req` to `127.0.0.1:<port>/<rest>`. `prefix`: the public path the app is
/// served under (`X-Forwarded-Prefix`, and absolute redirects are moved under it).
pub async fn forward(port: u16, rest: &str, prefix: Option<&str>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let mut url = format!("http://127.0.0.1:{port}/{}", rest.trim_start_matches('/'));
    if let Some(q) = parts.uri.query() {
        url.push('?');
        url.push_str(q);
    }
    let Ok(body) = axum::body::to_bytes(body, PROXY_BODY_LIMIT).await else {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "PAYLOAD_TOO_LARGE", "").into_response();
    };
    let client = match reqwest::Client::builder().no_gzip().redirect(reqwest::redirect::Policy::none()).connect_timeout(Duration::from_secs(5)).build() {
        Ok(c) => c,
        Err(e) => return err(StatusCode::BAD_GATEWAY, "SERVICE_UNREACHABLE", e.to_string()).into_response(),
    };
    let mut up = client.request(parts.method.clone(), &url);
    for (k, v) in parts.headers.iter() {
        let n = k.as_str();
        if HOP.contains(&n) || matches!(n, "host" | "authorization" | "cookie" | "content-length") || n.starts_with("x-lucina-") || n.starts_with("x-secretary-") {
            continue;
        }
        up = up.header(k, v);
    }
    up = up.header("x-forwarded-proto", "https");
    if let Some(pre) = prefix {
        up = up.header("x-forwarded-prefix", pre.trim_end_matches('/'));
    }
    if !body.is_empty() || matches!(parts.method, Method::POST | Method::PUT | Method::PATCH) {
        up = up.body(body);
    }
    let resp = match up.send().await {
        Ok(r) => r,
        Err(e) => return err(StatusCode::BAD_GATEWAY, "SERVICE_UNREACHABLE", format!("The service on port {port} did not answer: {e}")).into_response(),
    };
    let status = resp.status();
    let mut headers = HeaderMap::new();
    for (k, v) in resp.headers() {
        let n = k.as_str();
        if HOP.contains(&n) || matches!(n, "set-cookie" | "content-length") {
            continue;
        }
        if n == "location" {
            if let (Some(pre), Ok(loc)) = (prefix, v.to_str()) {
                if loc.starts_with('/') && !loc.starts_with("//") {
                    if let Ok(v) = HeaderValue::from_str(&format!("{}{loc}", pre.trim_end_matches('/'))) {
                        headers.append(header::LOCATION, v);
                    }
                    continue;
                }
            }
        }
        headers.append(k.clone(), v.clone());
    }
    headers.append(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(SANDBOX_CSP));
    let mut out = Response::new(Body::from_stream(resp.bytes_stream().map(|c| c.map_err(std::io::Error::other))));
    *out.status_mut() = status;
    *out.headers_mut() = headers;
    out
}

/// `Host: <label>.localhost[:port]` on the loopback listener goes to that service.
pub async fn host_proxy(State(m): State<Manager>, req: Request, next: Next) -> Response {
    let label = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.rsplit_once(':').map(|(a, _)| a).unwrap_or(h).to_ascii_lowercase())
        .and_then(|h| h.strip_suffix(".localhost").map(str::to_string));
    let Some(label) = label else { return next.run(req).await };
    match m.terminals().port_for_label(&label) {
        Some(port) => {
            let rest = req.uri().path().to_string();
            forward(port, &rest, None, req).await
        }
        None => err(StatusCode::NOT_FOUND, "SERVICE_NOT_RUNNING", format!("No running service is called {label}")).into_response(),
    }
}
