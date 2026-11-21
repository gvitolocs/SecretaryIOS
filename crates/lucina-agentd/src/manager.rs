//! Agents, turns and permissions (Paseo `server/agent/agent-manager.js`).
//!
//! Every state change goes through [`lucina_agents::lifecycle::step`]; the manager
//! carries out the effects it returns. Provider events arrive on one channel per
//! session and are applied in order by a pump task. State lives behind one mutex
//! that is never held across an await; provider calls run outside it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use lucina_agents::bucket::{state_bucket, BucketInput};
use lucina_agents::events::{format_turn_failed, system_error_item};
use lucina_agents::external::{self as ext, CommandBody};
use lucina_agents::lifecycle::{self, AgentState, Effect, Event, LifecycleError};
use lucina_agents::title::resolve_titles;
use lucina_agents::types::{ActiveTurn, AgentConfig, AgentMode, Capabilities};
use lucina_agents::{
    AgentListItem, AgentRecord, AgentSnapshot, AgentStatus, AttentionReason, PermissionRequest,
    PermissionResponse, StreamEvent, TimelineItem, TimelineRow,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{mpsc, watch};

use crate::external::{Commands, ExternalProvider};
use crate::journal::Journal;
use crate::lane::{Lane, LaneConfig, Outcome};
use crate::notify::Notifier;
use crate::now_iso;
use crate::process::Registry;
use crate::provider::{Provider, ProviderInfo, Session, SessionConfig};
use crate::store::{Store, StoreError};

mod breaker;
mod external_agents;
mod gate;
mod handoff;
mod ids;
mod orch;
mod permissions;
pub(crate) mod pipeline;
mod provider_subagents;
mod rewind;
mod schedules;
mod search;
mod sessions;
mod work;
mod workspaces;

pub use breaker::{Breaker, CapsConfig, Trip, CALLER_LABEL};
pub use gate::{GateConfig, GateRequest, CONFIG_FILE};
pub use handoff::{
    CONTINUED_FROM_LABEL, CONTINUED_ON_LABEL, CONTINUED_PROVIDER_LABEL, HANDOFF_LABEL,
};
pub use ids::{ImportIds, NewId, StateId};
pub use orch::{Caller, DEPTH_LABEL, MAX_DEPTH, MCP_TOKEN_LABEL, ORIGIN_LABEL};
pub use permissions::{
    ApprovalCandidate, ExternalRequest, PermissionView, Resolution, EXPIRED_MESSAGE,
};
pub use pipeline::{
    AdoptTask, DeployRequest, LeaseRequest, MergeRequest, ReleaseRequest, StartTask, STALE_DAYS,
};
pub use provider_subagents::{Direction as SubagentDirection, SubagentTimeline};
pub use rewind::{ForkOutcome, ForkRequest, RewindOutcome, RewindRequest};
pub use schedules::{owner_timezone, ScheduleInput, RUN_LABEL, SCHEDULE_LABEL};
pub use search::{PromptEntry, SearchMatch, SearchResult};
pub use work::{StartWork, WorkConfig, WorkList};
pub use workspaces::{metadata_provider, SWEEP_EVERY};

use crate::workspaces::{Registry as Workspaces, WsError};
use lucina_agents::workspace::WorkspaceRecord;

/// Daemon-wide settings beyond the store, providers and lanes.
pub struct Options {
    /// Provider processes, to recognise callers that agents started.
    pub registry: Option<Registry>,
    pub notifier: Option<Notifier>,
    /// How long an agent's permission request waits for the owner.
    pub permission_ttl: Duration,
    pub gate: GateConfig,
    pub workspaces: crate::workspaces::Config,
    pub terminals: crate::terminals::Config,
    pub pipeline: crate::pipeline::Config,
    /// Installs on leased devices (the owner's iPhone).
    pub devices: crate::devices::Config,
    /// Work jobs run through `lane` (`LUCINA_AGENTD_LANE`).
    pub work: WorkConfig,
    /// The create caps (`LUCINA_AGENTD_MAX_AGENTS_PER_CALLER`, `LUCINA_AGENTD_MAX_CREATES_PER_MINUTE`,
    /// `LUCINA_AGENTD_MAX_AGENTS_HOST`); see [`breaker`].
    pub caps: CapsConfig,
    /// Where the provider breaker keeps its state (`<state>/provider-breaker.json`); `None`: memory only.
    pub breaker_file: Option<std::path::PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            registry: None,
            notifier: None,
            permission_ttl: Duration::from_secs(3600),
            gate: GateConfig::default(),
            workspaces: crate::workspaces::Config::from_env(),
            terminals: crate::terminals::Config::default(),
            pipeline: crate::pipeline::Config::from_env(
                &std::env::temp_dir().join("lucina-agentd"),
            ),
            devices: crate::devices::Config::default(),
            work: WorkConfig::default(),
            caps: CapsConfig::from_env(),
            breaker_file: None,
        }
    }
}

/// Label that links a child agent to its parent (Paseo `paseo.parent-agent-id`).
pub const PARENT_LABEL: &str = "lucina.parent-agent-id";

/// The owner started this run on one of his devices (set by the API from the caller's origin).
pub const STARTED_ON_DEVICE_LABEL: &str = "lucina.started-on-device";

/// The agent label that opts into the Juno shell tool: `codecor.juno=bash` gives a
/// Claude agent the `juno_bash` tool (server `juno`, `lagent juno-mcp`; see
/// [`crate::juno_mcp`]). By default (owner decision 2026-11-08) juno tools are
/// allowed; in ask mode (`juno-decide.ask`, [`crate::juno_mcp::juno_ask_mode`]) the
/// agent must be in a mode that asks before each call, and every call shows the
/// owner an Allow/Deny card.
pub const JUNO_LABEL: &str = "codecor.juno";
pub const JUNO_LABEL_VALUE: &str = "bash";
/// The modes where Claude asks before an MCP tool call.
pub const JUNO_SAFE_MODES: &[&str] = &["default", "plan", "acceptEdits"];

/// Whether these labels opt into the Juno shell tool.
pub fn juno_tool(labels: &std::collections::BTreeMap<String, String>) -> bool {
    labels
        .get(JUNO_LABEL)
        .is_some_and(|v| v == JUNO_LABEL_VALUE)
}

/// Refuse the Juno shell tool unless the agent is a Claude agent; in ask mode
/// (the 2026-11-07 behaviour, on via `juno-decide.ask` / `JUNO_DECIDE_ASK=1`) it
/// must also be in a mode that asks before each call and its tool policy must
/// preapprove no `mcp__juno__*` tool. Allowed by default (owner decision 2026-11-08):
/// only the provider is checked.
pub fn check_juno_tool(
    provider_id: &str,
    mode: Option<&str>,
    tool_policy: Option<&Value>,
) -> Result<(), ManagerError> {
    check_juno_tool_with(
        provider_id,
        mode,
        tool_policy,
        crate::juno_mcp::juno_ask_mode(),
    )
}

fn check_juno_tool_with(
    provider_id: &str,
    mode: Option<&str>,
    tool_policy: Option<&Value>,
    ask_mode: bool,
) -> Result<(), ManagerError> {
    if provider_id != crate::providers::claude::ID {
        return Err(ManagerError::JunoToolRefused(format!(
            "provider {provider_id:?} is not claude"
        )));
    }
    if ask_mode {
        let mode = mode.unwrap_or("default");
        if !JUNO_SAFE_MODES.contains(&mode) {
            return Err(ManagerError::JunoToolRefused(format!(
                "mode {mode:?} does not ask before each call"
            )));
        }
        if let Some(tp) = tool_policy {
            let policy: lucina_agents::permission_gate::ToolPolicy =
                serde_json::from_value(tp.clone()).unwrap_or_default();
            let pre = policy.preapproved.iter().any(|g| {
                // `ToolPolicy` matches exact `mcp__<server>__<tool>` names, no wildcards.
                let name = format!("mcp__{}__{}", g.server, g.tool);
                name.starts_with("mcp__juno__") && policy.preapproves(&name)
            });
            if pre {
                return Err(ManagerError::JunoToolRefused(
                    "the tool policy preapproves a juno tool".into(),
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("No agent found matching: {0}")]
    NotFound(String),
    #[error("Agent id prefix {0} matches more than one agent")]
    Ambiguous(String),
    #[error("Unknown agent provider: {0}")]
    UnknownProvider(String),
    #[error("{0}")]
    Lifecycle(#[from] LifecycleError),
    #[error("{0}")]
    Provider(String),
    #[error("{0}")]
    Invalid(String),
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    PermissionNotFound(String),
    #[error("{0}")]
    AlreadyAnswered(String),
    #[error("{0}")]
    Expired(String),
    #[error("{0}")]
    Unconfirmed(String),
    #[error("{0}")]
    Workspace(#[from] WsError),
    /// The worktree guard refused the agent's workspace.
    #[error("{0}")]
    Guard(String),
    /// Only an external agent's owner device may do this.
    #[error("{0}")]
    NotOwner(String),
    #[error("{0}")]
    CommandNotFound(String),
    #[error(
        "no work job \"{0}\"; a run started with lane directly is read with `lane status {0}`"
    )]
    WorkNotFound(String),
    /// An id nobody issued (`lagent id show`, `lagent trace`).
    #[error("{0}")]
    IdNotFound(String),
    #[error("{0}")]
    CommandExpired(String),
    /// A subagent tried to start another agent (the depth limit).
    #[error("{0}")]
    DelegationDepth(String),
    /// An agent acted on an agent that is not its subagent.
    #[error("{0}")]
    NotYourAgent(String),
    #[error("{0}")]
    ScheduleNotFound(String),
    #[error("{0}")]
    ScheduleConflict(String),
    /// An MCP token or an agent changing a schedule it didn't create.
    #[error("{0}")]
    ScheduleForbidden(String),
    #[error("No provider subagent found matching: {0}")]
    SubagentNotFound(String),
    /// The provider can't rewind this way (Paseo `RewindCapabilityError`), or the agent
    /// isn't one a provider runs.
    #[error("{0}")]
    RewindUnsupported(String),
    /// The message to rewind to isn't a user message of the agent.
    #[error("{0}")]
    RewindTargetNotFound(String),
    /// A rewind of files the caller may not touch.
    #[error("{0}")]
    RewindForbidden(String),
    /// The fork boundary is gone or changed (Paseo's `fork_context` errors).
    #[error("{0}")]
    ForkBoundary(String),
    /// A rewind or a lane run holds the agent right now.
    #[error("{0}")]
    Busy(String),
    /// The owner's rule: Codex is not used on Lucina projects.
    #[error("{0}")]
    CodexNotAllowed(String),
    /// The Juno shell tool (`codecor.juno=bash`) needs a Claude agent in a mode that
    /// asks before each call, and no preapproved juno tool.
    #[error("The Juno shell tool needs a Claude agent in a mode that asks before each call ({0})")]
    JunoToolRefused(String),
    /// A create over a cap (HTTP 429).
    #[error("{0}")]
    TooManyAgents(String),
    /// The provider's breaker is tripped (HTTP 503).
    #[error("{0}")]
    ProviderUnavailable(String),
}

impl ManagerError {
    pub fn code(&self) -> &'static str {
        match self {
            ManagerError::NotFound(_) => "AGENT_NOT_FOUND",
            ManagerError::Ambiguous(_) => "AGENT_AMBIGUOUS",
            ManagerError::UnknownProvider(_) => "UNKNOWN_PROVIDER",
            ManagerError::Lifecycle(_) => "INVALID_STATE",
            ManagerError::Provider(_) => "PROVIDER_ERROR",
            ManagerError::Invalid(_) => "INVALID_REQUEST",
            ManagerError::Store(_) => "STORE_ERROR",
            ManagerError::Forbidden(_) => "PERMISSION_FORBIDDEN",
            ManagerError::PermissionNotFound(_) => "PERMISSION_NOT_FOUND",
            ManagerError::AlreadyAnswered(_) => "PERMISSION_ALREADY_ANSWERED",
            ManagerError::Expired(_) => "PERMISSION_EXPIRED",
            ManagerError::Unconfirmed(_) => "PERMISSION_RESPONSE_UNCONFIRMED",
            ManagerError::Workspace(WsError::NotFound(_)) => "WORKSPACE_NOT_FOUND",
            ManagerError::Workspace(WsError::Invalid(_)) => "WORKSPACE_INVALID",
            ManagerError::Workspace(WsError::Git(_)) => "WORKSPACE_GIT_FAILED",
            ManagerError::Workspace(WsError::Store(_)) => "STORE_ERROR",
            ManagerError::Guard(_) => "WORKSPACE_GUARD",
            ManagerError::NotOwner(_) => "NOT_OWNER",
            ManagerError::CommandNotFound(_) => "COMMAND_NOT_FOUND",
            ManagerError::CommandExpired(_) => "COMMAND_EXPIRED",
            ManagerError::DelegationDepth(_) => "DELEGATION_DEPTH",
            ManagerError::NotYourAgent(_) => "NOT_YOUR_AGENT",
            ManagerError::ScheduleNotFound(_) => "SCHEDULE_NOT_FOUND",
            ManagerError::ScheduleConflict(_) => "SCHEDULE_CONFLICT",
            ManagerError::ScheduleForbidden(_) => "SCHEDULE_FORBIDDEN",
            ManagerError::SubagentNotFound(_) => "PROVIDER_SUBAGENT_NOT_FOUND",
            ManagerError::RewindUnsupported(_) => "REWIND_UNSUPPORTED",
            ManagerError::RewindTargetNotFound(_) => "REWIND_TARGET_NOT_FOUND",
            ManagerError::RewindForbidden(_) => "REWIND_FORBIDDEN",
            ManagerError::ForkBoundary(_) => "FORK_BOUNDARY_UNAVAILABLE",
            ManagerError::Busy(_) => "AGENT_BUSY",
            ManagerError::CodexNotAllowed(_) => "CODEX_NOT_ALLOWED",
            ManagerError::JunoToolRefused(_) => "JUNO_TOOL_REFUSED",
            ManagerError::WorkNotFound(_) => "WORK_NOT_FOUND",
            ManagerError::IdNotFound(_) => "ID_NOT_FOUND",
            ManagerError::TooManyAgents(_) => "AGENT_LIMIT",
            ManagerError::ProviderUnavailable(_) => "PROVIDER_TRIPPED",
        }
    }
}

/// One message on the event bus (Paseo `agent_stream` / `agent_state`).
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum BusEvent {
    AgentStream {
        agent_id: String,
        event: Box<StreamEvent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        epoch: String,
        timestamp: String,
    },
    AgentState {
        agent: Box<AgentSnapshot>,
    },
    AgentDeleted {
        agent_id: String,
    },
    /// A provider subagent was created or changed, got a timeline item, or was removed
    /// (Paseo `agent.provider_subagents.update`).
    ProviderSubagent {
        event: Box<lucina_agents::subagent::SubagentUpdate>,
    },
    /// A rewind replaced the timeline (Paseo `agent.timeline.replacement`): clients drop
    /// what they hold for the agent and fetch it again under the new epoch.
    TimelineReplacement {
        agent_id: String,
        epoch: String,
    },
    /// Lucina only: a permission request (agent or external) was raised or changed.
    Permission {
        permission: Box<PermissionView>,
    },
    /// Lucina only: a workspace was created or changed (setup progress, title, archive).
    Workspace {
        workspace: Box<WorkspaceRecord>,
    },
    /// Lucina only: a command for an external agent's owner device. Streams send it
    /// to that device only.
    /// Lucina only: a terminal was created or changed (exit, title, size, activity, ports).
    Terminal {
        terminal: Box<crate::terminals::TerminalView>,
    },
    /// Lucina only: a terminal was killed and forgotten.
    TerminalRemoved {
        terminal_id: String,
        workspace_id: String,
    },
    /// Lucina only: a workspace's scripts changed (Paseo `script_status_update`).
    ScriptStatus {
        workspace_id: String,
        scripts: Vec<Value>,
    },
    /// Lucina only: an agent CLI in a terminal waits for input (Paseo `terminal_attention_required`).
    TerminalAttention {
        terminal_id: String,
        workspace_id: String,
        reason: String,
        title: String,
        body: String,
    },
    AgentCommand {
        agent_id: String,
        command_id: String,
        command: CommandBody,
        created_at: String,
        expires_at: String,
        #[serde(skip_serializing)]
        device_id: String,
    },
}

impl BusEvent {
    /// Whether a stream opened by `device` (`None`: not a device) may see this event.
    pub fn visible_to(&self, device: Option<&str>) -> bool {
        match self {
            BusEvent::AgentCommand { device_id, .. } => device == Some(device_id.as_str()),
            _ => true,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRequest {
    /// `provider` or `provider/model`.
    pub provider: String,
    #[serde(default)]
    pub model: Option<String>,
    pub cwd: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub mode_id: Option<String>,
    #[serde(default)]
    pub thinking_option_id: Option<String>,
    #[serde(default)]
    pub feature_values: Map<String, Value>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Run through a lane category (`deep`, `routine`, `bulk`) with quota
    /// fall-through instead of one fixed provider.
    #[serde(default)]
    pub chain: Option<String>,
    /// Paseo `toolPolicy` (`{preapproved: [{kind: "mcp", server, tool}]}`).
    #[serde(default)]
    pub tool_policy: Option<Value>,
    /// The finished-run gate's proof command (else `.lucina/agents.toml`).
    #[serde(default)]
    pub verify: Option<String>,
    #[serde(default)]
    pub verify_heavy: bool,
    /// Follow-up rounds after the gate refuses the run.
    #[serde(default)]
    pub auto_fix: u32,
    /// Run in this workspace (its cwd when `cwd` is empty).
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// The owner allows this run in a main checkout or a stray worktree. Honoured only
    /// from the owner (the API clears it for other callers).
    #[serde(default)]
    pub allow_main_checkout: bool,
    /// Hidden helper agents (title and branch generation); never from the API.
    #[serde(skip)]
    pub internal: bool,
    /// An agent another host runs (provider `secretary-code`); see [`crate::external`].
    #[serde(default)]
    pub external: bool,
    /// Where an external agent runs (`mac`).
    #[serde(default)]
    pub host: Option<String>,
    /// The device registering an external agent: set by the API from the caller, never
    /// from the body.
    #[serde(skip)]
    pub owner_device_id: Option<String>,
    /// A Claude session's agent (`manager/sessions.rs`); never from the API.
    #[serde(skip)]
    pub claude_session: bool,
    /// A fork's chat history, sent ahead of the first prompt (set by `fork`, never from
    /// the body).
    #[serde(skip)]
    pub seed_context: Option<String>,
    /// Who asked for the agent (the API's caller identity: `local`, `device:<id>`, `agent:<id>`):
    /// set by the create route, never from the body. Only a create carrying it is counted by the
    /// create caps (see [`breaker`]).
    #[serde(skip)]
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ActiveTurnBehavior {
    Steer,
    #[default]
    Interrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    TurnStarted,
    Steered,
    OutOfBand,
    /// An external agent: the prompt is queued for its owner device.
    Queued,
}

/// Paseo `waitForFinish`'s result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitResult {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    #[serde(rename = "final")]
    pub final_snapshot: AgentSnapshot,
}

struct Agent {
    record: AgentRecord,
    k: AgentState,
    session: Option<Arc<dyn Session>>,
    pending: Vec<PermissionRequest>,
    active_turn: Option<ActiveTurn>,
    rows: Vec<TimelineRow>,
    capabilities: Capabilities,
    modes: Vec<AgentMode>,
    echoes_user_message: bool,
    /// A lane chain is still deciding (between attempts the agent may briefly be
    /// in error): `wait` keeps waiting.
    chain_active: bool,
    /// Where the current (not yet gated) run started.
    turn_base: Option<gate::Base>,
    /// `wait --gate` waits: a finished run is being checked, or its auto-fix
    /// follow-up is running.
    gate_pending: bool,
    /// The gate task is running (one per agent).
    gate_running: bool,
    /// A turn completed while the gate task ran: check again.
    gate_again: bool,
    /// The children the provider runs inside this agent, each with its own timeline.
    subagents: Vec<provider_subagents::Sub>,
    /// A rewind is changing the agent: no new turn starts until it is done.
    rewinding: bool,
    /// The agent failed and gave up its session (and its provider processes); a new
    /// prompt starts a new one.
    released: bool,
}

impl Agent {
    fn next_seq(&self) -> u64 {
        self.rows.last().map(|r| r.seq + 1).unwrap_or(1)
    }
}

struct Inner {
    store: Arc<Store>,
    providers: HashMap<String, Arc<dyn Provider>>,
    lanes: LaneConfig,
    agents: Mutex<HashMap<String, Agent>>,
    bus: Journal,
    commands: Arc<Commands>,
    /// Set at shutdown: open streams end, so the server's graceful drain does not wait on them.
    closing: watch::Sender<bool>,
    generation: watch::Sender<u64>,
    book: Mutex<HashMap<String, permissions::Entry>>,
    options: Options,
    workspaces: Workspaces,
    terminals: crate::terminals::Hub,
    /// Schedules, subagents and agent messages (phase 6).
    orch: orch::Orch,
    pipeline: crate::pipeline::Pipeline,
    devices: crate::devices::Devices,
    work: work::Work,
    /// The identifiers of runs, changes, versions and findings.
    ids: ids::Ids,
    /// Providers whose last turn failed on authentication or a quota ([`breaker`]).
    breaker: Breaker,
    caps: breaker::Caps,
}

#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

fn split_provider(spec: &str, model: Option<String>) -> (String, Option<String>) {
    match spec.split_once('/') {
        Some((p, m)) if !m.is_empty() => (p.to_string(), model.or_else(|| Some(m.to_string()))),
        _ => (spec.to_string(), model),
    }
}

fn session_config(record: &AgentRecord, env: Vec<(String, String)>) -> SessionConfig {
    SessionConfig {
        cwd: record.cwd.clone(),
        model: record.config.model.clone(),
        mode_id: record.config.mode_id.clone(),
        thinking_option_id: record.config.thinking_option_id.clone(),
        feature_values: record.config.feature_values.clone().unwrap_or_default(),
        env,
        agent_id: Some(record.id.clone()),
        owner_device_id: record.owner_device_id.clone(),
        juno_tool: juno_tool(&record.labels),
    }
}

impl Manager {
    /// Open the store and bring every stored agent back as `closed` (Paseo's
    /// loader; lifecycle `RestartRepair`).
    pub fn open(
        store: Store,
        providers: Vec<Arc<dyn Provider>>,
        lanes: LaneConfig,
    ) -> Result<Self, ManagerError> {
        Self::open_with(store, providers, lanes, Options::default())
    }

    pub fn open_with(
        store: Store,
        providers: Vec<Arc<dyn Provider>>,
        lanes: LaneConfig,
        options: Options,
    ) -> Result<Self, ManagerError> {
        let mut providers: HashMap<String, Arc<dyn Provider>> =
            providers.into_iter().map(|p| (p.info().id, p)).collect();
        let bus = Journal::new();
        let (generation, _) = watch::channel(0);
        let store = Arc::new(store);
        let commands = Arc::new(Commands::new(store.clone(), bus.clone()));
        providers.insert(
            ext::PROVIDER.to_string(),
            Arc::new(ExternalProvider::new(commands.clone())),
        );
        providers.insert(
            ext::SESSION_PROVIDER.to_string(),
            Arc::new(ExternalProvider::claude_sessions(commands.clone())),
        );
        let term_bus = bus.clone();
        let terminals = crate::terminals::Hub::new(
            options.terminals.clone(),
            options.registry.clone(),
            Arc::new(move |e| {
                use crate::terminals::HubEvent;
                let _ = term_bus.send(match e {
                    HubEvent::Terminal(terminal) => BusEvent::Terminal { terminal },
                    HubEvent::Removed {
                        terminal_id,
                        workspace_id,
                    } => BusEvent::TerminalRemoved {
                        terminal_id,
                        workspace_id,
                    },
                    HubEvent::Scripts {
                        workspace_id,
                        scripts,
                    } => BusEvent::ScriptStatus {
                        workspace_id,
                        scripts,
                    },
                    HubEvent::Attention {
                        terminal_id,
                        workspace_id,
                        reason,
                        title,
                        body,
                    } => BusEvent::TerminalAttention {
                        terminal_id,
                        workspace_id,
                        reason,
                        title,
                        body,
                    },
                });
            }),
        );
        let ws_bus = bus.clone();
        let auto_terms = terminals.clone();
        let setup_seen: Mutex<HashMap<String, Option<String>>> = Mutex::new(HashMap::new());
        let publish: crate::workspaces::Publish = Arc::new(move |w: &WorkspaceRecord| {
            let _ = ws_bus.send(BusEvent::Workspace {
                workspace: Box::new(w.clone()),
            });
            // paseo.json `worktree.terminals` open once setup ends in this run (not on
            // later updates, nor for workspaces loaded at start).
            let now = w.setup.as_ref().map(|x| x.status.clone());
            let before = setup_seen
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(w.workspace_id.clone(), now.clone());
            let finished = matches!(now.as_deref(), Some("completed" | "none"));
            let was_pending = match &before {
                Some(None) => true,
                Some(Some(s)) => matches!(s.as_str(), "running" | "pending"),
                None => false,
            };
            if finished && was_pending && w.archived_at.is_none() && w.untrusted_source.is_none() {
                auto_terms.open_configured(&w.workspace_id, std::path::Path::new(&w.cwd));
            }
        });
        let workspaces = Workspaces::open(store.clone(), options.workspaces.clone(), publish)?;
        let orch = orch::Orch::open(&store)?;
        let pipeline = crate::pipeline::Pipeline::open(store.clone(), options.pipeline.clone())?;
        let devices = crate::devices::Devices::open(options.devices.clone());
        let work = work::Work::open(options.work.clone());
        let ids = ids::Ids::open(&options.work.dir);
        let breaker = Breaker::open(options.breaker_file.clone());
        let manager = Manager {
            inner: Arc::new(Inner {
                workspaces,
                terminals,
                orch,
                pipeline,
                devices,
                work,
                ids,
                breaker,
                caps: Default::default(),
                store,
                providers,
                lanes,
                agents: Mutex::new(HashMap::new()),
                bus,
                commands,
                closing: watch::channel(false).0,
                generation,
                book: Mutex::new(HashMap::new()),
                options,
            }),
        };
        for mut record in manager.inner.store.load_all()? {
            let k = AgentState {
                status: record.last_status,
                archived: record.archived_at.is_some(),
                deleted: false,
                pending_permissions: 0,
                // A Claude session pausing is nothing to push: it has its own window.
                internal: record.internal || ext::is_session(&record.provider),
                requires_attention: record.requires_attention,
                attention_reason: record.attention_reason,
            };
            let t = lifecycle::step(&k, Event::RestartRepair)?;
            let mut rows = manager.inner.store.load_rows(&record.id)?;
            if t.effects.contains(&Effect::RecordInterrupted) && !ext::is_session(&record.provider)
            {
                let row = TimelineRow {
                    seq: rows.last().map(|r| r.seq + 1).unwrap_or(1),
                    timestamp: now_iso(),
                    item: TimelineItem::Notification {
                        level: lucina_agents::timeline::NotificationLevel::Warning,
                        message: "The agent daemon restarted; this run was interrupted.".into(),
                    },
                    turn_id: None,
                };
                manager.inner.store.append_row(&record.id, &row)?;
                rows.push(row);
            }
            record.last_status = t.next.status;
            manager.inner.store.save(&record)?;
            let info = manager
                .inner
                .providers
                .get(&record.provider)
                .map(|p| p.info());
            let agent = Agent {
                subagents: manager.load_subagents(&record.id),
                capabilities: Capabilities::stored(),
                modes: info.as_ref().map(|i| i.modes.clone()).unwrap_or_default(),
                echoes_user_message: info.as_ref().is_some_and(|i| i.echoes_user_message),
                chain_active: false,
                record,
                k: t.next,
                session: None,
                pending: Vec::new(),
                active_turn: None,
                rows,
                turn_base: None,
                gate_pending: false,
                gate_running: false,
                gate_again: false,
                rewinding: false,
                released: false,
            };
            manager.lock().insert(agent.record.id.clone(), agent);
        }
        Ok(manager)
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Agent>> {
        self.inner.agents.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The numbered event bus (`GET /stream`).
    pub fn journal(&self) -> &Journal {
        &self.inner.bus
    }

    /// Ends every open stream (shutdown).
    pub fn close_streams(&self) {
        self.inner.closing.send_replace(true);
    }

    /// Becomes `true` at shutdown.
    pub fn closing(&self) -> watch::Receiver<bool> {
        self.inner.closing.subscribe()
    }

    pub fn providers(&self) -> Vec<ProviderInfo> {
        // Nobody can start a Claude session from here.
        let mut v: Vec<ProviderInfo> = self
            .inner
            .providers
            .values()
            .map(|p| p.info())
            .filter(|i| !ext::is_session(&i.id))
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    fn bump(&self) {
        self.inner.generation.send_modify(|g| *g += 1);
    }

    /// Resolve an id or a unique id prefix.
    pub fn resolve(&self, id_or_prefix: &str) -> Result<String, ManagerError> {
        let agents = self.lock();
        if agents.contains_key(id_or_prefix) {
            return Ok(id_or_prefix.to_string());
        }
        let matches: Vec<&String> = agents
            .keys()
            .filter(|k| k.starts_with(id_or_prefix))
            .collect();
        match matches.len() {
            0 => Err(ManagerError::NotFound(id_or_prefix.to_string())),
            1 => Ok(matches[0].clone()),
            _ => Err(ManagerError::Ambiguous(id_or_prefix.to_string())),
        }
    }

    // -----------------------------------------------------------------------
    // Snapshots

    fn snapshot_of(agent: &Agent) -> AgentSnapshot {
        let r = &agent.record;
        let runtime = agent
            .session
            .as_ref()
            .map(|s| s.runtime_info())
            .or_else(|| r.runtime_info.clone());
        let current_mode = agent
            .session
            .as_ref()
            .and_then(|s| s.current_mode())
            .or_else(|| r.last_mode_id.clone());
        AgentSnapshot {
            id: r.id.clone(),
            provider: r.provider.clone(),
            cwd: r.cwd.clone(),
            workspace_id: r.workspace_id.clone(),
            model: r.config.model.clone(),
            features: None,
            thinking_option_id: r.config.thinking_option_id.clone(),
            effective_thinking_option_id: r.config.thinking_option_id.clone().or_else(|| {
                runtime
                    .as_ref()
                    .and_then(|ri| ri.thinking_option_id.clone())
            }),
            created_at: r.created_at.clone(),
            updated_at: r.updated_at.clone(),
            last_user_message_at: r.last_user_message_at.clone(),
            status: agent.k.status,
            active_turn: agent.active_turn.clone(),
            capabilities: agent.capabilities.clone(),
            current_mode_id: current_mode,
            available_modes: agent.modes.clone(),
            pending_permissions: agent.pending.clone(),
            persistence: agent
                .session
                .as_ref()
                .map(|s| s.persistence())
                .or_else(|| r.persistence.clone()),
            runtime_info: runtime,
            last_usage: r.last_usage.clone(),
            last_error: r.last_error.clone(),
            title: r.title.clone(),
            labels: r.labels.clone(),
            requires_attention: Some(agent.k.requires_attention),
            attention_reason: agent.k.attention_reason,
            attention_timestamp: r.attention_timestamp.clone(),
            archived_at: r.archived_at.clone(),
            provider_unavailable: None,
            internal: r.internal.then_some(true),
            gate: r.gate.clone(),
            external: r.external.then_some(true),
            owner_device_id: r.owner_device_id.clone(),
            host: r.host.clone(),
        }
    }

    fn list_item_of(agent: &Agent) -> AgentListItem {
        let r = &agent.record;
        AgentListItem {
            id: r.id.clone(),
            short_id: r.short_id(),
            title: r.title.clone(),
            provider: r.provider.clone(),
            model: r.config.model.clone(),
            thinking_option_id: r.config.thinking_option_id.clone(),
            status: agent.k.status,
            cwd: r.cwd.clone(),
            created_at: r.created_at.clone(),
            updated_at: r.updated_at.clone(),
            last_user_message_at: r.last_user_message_at.clone(),
            archived_at: r.archived_at.clone(),
            requires_attention: Some(agent.k.requires_attention),
            attention_reason: agent.k.attention_reason,
            attention_timestamp: r.attention_timestamp.clone(),
            labels: r.labels.clone(),
            provider_unavailable: None,
            bucket: match (
                r.labels.contains_key(CONTINUED_ON_LABEL),
                r.labels.get(HANDOFF_LABEL).map(String::as_str),
            ) {
                // Handed on: another agent carries the run (manager/handoff.rs).
                (true, _) if agent.k.status == AgentStatus::Error => {
                    lucina_agents::bucket::Bucket::Done
                }
                (_, Some("pending")) if agent.k.status == AgentStatus::Error => {
                    lucina_agents::bucket::Bucket::Running
                }
                _ => state_bucket(BucketInput {
                    status: agent.k.status,
                    pending_permission_count: agent.pending.len(),
                    requires_attention: agent.k.requires_attention,
                    attention_reason: agent.k.attention_reason,
                }),
            },
            external: r.external.then_some(true),
            owner_device_id: r.owner_device_id.clone(),
            host: r.host.clone(),
        }
    }

    pub fn snapshot(&self, id: &str) -> Result<AgentSnapshot, ManagerError> {
        let agents = self.lock();
        agents
            .get(id)
            .map(Self::snapshot_of)
            .ok_or_else(|| ManagerError::NotFound(id.to_string()))
    }

    /// All agents as snapshots (archived ones only when asked).
    pub fn snapshots(&self, include_archived: bool) -> Vec<AgentSnapshot> {
        let agents = self.lock();
        let mut v: Vec<AgentSnapshot> = agents
            .values()
            .filter(|a| include_archived || a.record.archived_at.is_none())
            .filter(|a| !a.record.internal)
            .map(Self::snapshot_of)
            .collect();
        v.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        v
    }

    pub fn list(&self, include_archived: bool) -> Vec<AgentListItem> {
        let agents = self.lock();
        let mut v: Vec<AgentListItem> = agents
            .values()
            .filter(|a| include_archived || a.record.archived_at.is_none())
            .filter(|a| !a.record.internal)
            .map(Self::list_item_of)
            .collect();
        v.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        v
    }

    pub fn timeline(
        &self,
        id: &str,
        after_seq: u64,
        limit: usize,
    ) -> Result<(String, Vec<TimelineRow>), ManagerError> {
        let agents = self.lock();
        let a = agents
            .get(id)
            .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        let rows: Vec<TimelineRow> = a
            .rows
            .iter()
            .filter(|r| r.seq > after_seq)
            .take(if limit == 0 { usize::MAX } else { limit })
            .cloned()
            .collect();
        Ok((a.record.epoch.clone(), rows))
    }

    // -----------------------------------------------------------------------
    // State changes (all under the lock, no awaits)

    fn publish_state(&self, agent: &Agent) {
        let _ = self.inner.bus.send(BusEvent::AgentState {
            agent: Box::new(Self::snapshot_of(agent)),
        });
    }

    fn persist(&self, agent: &mut Agent) {
        agent.record.last_status = agent.k.status;
        agent.record.requires_attention = agent.k.requires_attention;
        agent.record.attention_reason = agent.k.attention_reason;
        agent.record.updated_at = now_iso();
        if let Some(s) = &agent.session {
            agent.record.runtime_info = Some(s.runtime_info());
            agent.record.persistence = Some(s.persistence());
            agent.record.last_mode_id = s.current_mode();
        }
        if let Err(e) = self.inner.store.save(&agent.record) {
            tracing::error!(agent = %agent.record.id, error = %e, "failed to save agent");
        }
    }

    /// Append to the timeline. A prompt that is a `<lucina-system>` envelope (an agent's
    /// message, a finish notice, a heartbeat) shows as what it is (Paseo
    /// `projectAgentMessages`).
    fn append(&self, agent: &mut Agent, item: TimelineItem, turn_id: Option<String>) {
        if let TimelineItem::UserMessage { text, .. } = &item {
            if text.starts_with(&format!("<{}", lucina_agents::envelope::TAG)) {
                for projected in lucina_agents::envelope::project_user_text(text) {
                    self.append_row(agent, projected, turn_id.clone());
                }
                return;
            }
        }
        self.append_row(agent, item, turn_id)
    }

    fn append_row(&self, agent: &mut Agent, item: TimelineItem, turn_id: Option<String>) {
        // Every user message has an id, so a rewind can name it (a provider that echoes
        // the message brings its own).
        let item = match item {
            TimelineItem::UserMessage {
                text,
                message_id: None,
                client_message_id,
            } => TimelineItem::UserMessage {
                text,
                message_id: Some(uuid::Uuid::new_v4().to_string()),
                client_message_id,
            },
            other => other,
        };
        let row = TimelineRow {
            seq: agent.next_seq(),
            timestamp: now_iso(),
            item,
            turn_id,
        };
        if let Err(e) = self.inner.store.append_row(&agent.record.id, &row) {
            tracing::error!(agent = %agent.record.id, error = %e, "failed to append timeline row");
        }
        if let TimelineItem::UserMessage { .. } = row.item {
            agent.record.last_user_message_at = Some(row.timestamp.clone());
        }
        agent.record.last_activity_at = Some(row.timestamp.clone());
        let _ = self.inner.bus.send(BusEvent::AgentStream {
            agent_id: agent.record.id.clone(),
            event: Box::new(StreamEvent::Timeline {
                provider: agent.record.provider.clone(),
                item: row.item.clone(),
                turn_id: row.turn_id.clone(),
            }),
            seq: Some(row.seq),
            epoch: agent.record.epoch.clone(),
            timestamp: row.timestamp.clone(),
        });
        agent.rows.push(row);
    }

    fn forward(&self, agent: &Agent, event: StreamEvent) {
        let _ = self.inner.bus.send(BusEvent::AgentStream {
            agent_id: agent.record.id.clone(),
            event: Box::new(event),
            seq: None,
            epoch: agent.record.epoch.clone(),
            timestamp: now_iso(),
        });
    }

    /// Run the kernel and apply the effects that need no provider call. Returns
    /// the effects for the caller to finish (Interrupt, CloseRuntime, ...).
    fn transition(
        &self,
        agent: &mut Agent,
        event: Event,
        error: Option<&str>,
    ) -> Result<Vec<Effect>, LifecycleError> {
        let t = lifecycle::step(&agent.k, event)?;
        agent.k = t.next;
        for effect in &t.effects {
            match effect {
                Effect::ClearLastError => agent.record.last_error = None,
                Effect::SetLastError => agent.record.last_error = error.map(str::to_string),
                Effect::AppendSystemError => {
                    if let Some(item) =
                        system_error_item(error.unwrap_or(""), agent.rows.last().map(|r| &r.item))
                    {
                        let turn = agent.active_turn.as_ref().map(|t| t.turn_id.clone());
                        self.append(agent, item, turn);
                    }
                }
                Effect::AutoDenyPending(message) => {
                    for request in std::mem::take(&mut agent.pending) {
                        self.withdraw_agent_request(
                            &agent.record.id,
                            &request.id,
                            &PermissionResponse::auto_deny(message),
                        );
                        self.forward(
                            agent,
                            StreamEvent::PermissionResolved {
                                provider: agent.record.provider.clone(),
                                request_id: request.id,
                                resolution: PermissionResponse::auto_deny(message),
                            },
                        );
                    }
                }
                Effect::SetAttention(reason) | Effect::NotifyAttention(reason) => {
                    let timestamp = now_iso();
                    if matches!(effect, Effect::SetAttention(_)) {
                        agent.record.attention_timestamp = Some(timestamp.clone());
                    }
                    let event = attention_event(agent, *reason, timestamp);
                    if let StreamEvent::AttentionRequired {
                        should_notify: true,
                        notification: Some(n),
                        ..
                    } = &event
                    {
                        self.push_attention(agent, *reason, &n.body);
                    }
                    self.forward(agent, event);
                }
                Effect::RecordInterrupted => {}
                Effect::Interrupt
                | Effect::CloseRuntime
                | Effect::CascadeArchiveChildren
                | Effect::RemoveRecord => {}
            }
        }
        Ok(t.effects)
    }

    /// Apply one provider event (called by the session pump, in order).
    fn on_event(&self, agent_id: &str, event: StreamEvent) {
        let mut agents = self.lock();
        let Some(agent) = agents.get_mut(agent_id) else {
            return;
        };
        let result = match event {
            StreamEvent::ThreadStarted { .. } => Ok(()),
            StreamEvent::TurnStarted { turn_id, .. } => {
                let r = self.transition(agent, Event::TurnStarted, None).map(|_| ());
                if agent.active_turn.is_none() {
                    agent.active_turn = turn_id.map(|t| ActiveTurn {
                        turn_id: t,
                        started_at: Some(now_iso()),
                    });
                }
                r
            }
            StreamEvent::Timeline { item, turn_id, .. } => {
                self.append(agent, item, turn_id);
                Ok(())
            }
            StreamEvent::UsageUpdated { usage, .. } => {
                agent.record.last_usage = Some(
                    agent
                        .record
                        .last_usage
                        .clone()
                        .unwrap_or_default()
                        .merged(&usage),
                );
                Ok(())
            }
            StreamEvent::TurnCompleted { usage, .. } => {
                if let Some(usage) = usage {
                    agent.record.last_usage = Some(
                        agent
                            .record
                            .last_usage
                            .clone()
                            .unwrap_or_default()
                            .merged(&usage),
                    );
                }
                let r = self
                    .transition(agent, Event::TurnCompleted, None)
                    .map(|_| ());
                agent.active_turn = None;
                if r.is_ok() {
                    self.breaker_observe_success(&agent.record.provider);
                }
                if r.is_ok() && agent.turn_base.is_some() && !agent.record.internal {
                    self.gate_turn_completed(agent);
                }
                r
            }
            StreamEvent::TurnFailed {
                error,
                code,
                diagnostic,
                ..
            } => {
                let message = format_turn_failed(&error, code.as_deref(), diagnostic.as_deref());
                let r = self
                    .transition(agent, Event::TurnFailed, Some(&message))
                    .map(|_| ());
                agent.record.last_error = Some(error);
                agent.active_turn = None;
                Self::gate_turn_ended(agent);
                if r.is_ok() {
                    if let Some((class, detector)) = self.handoff_check(agent) {
                        self.spawn_handoff(agent_id.to_string(), class, detector);
                    }
                    self.breaker_observe_failure(agent);
                    self.release_if_errored(agent);
                }
                r
            }
            StreamEvent::TurnCanceled { .. } => {
                let r = self
                    .transition(agent, Event::TurnCanceled, None)
                    .map(|_| ());
                agent.active_turn = None;
                Self::gate_turn_ended(agent);
                r
            }
            StreamEvent::PermissionRequested { request, provider } => {
                if self.register_agent_request(agent, &request) {
                    // Preapproved by the tool policy: allowed at once, never pending.
                    self.auto_allow(agent, &request.id);
                    Ok(())
                } else {
                    // Pending first, so the attention notification can preview it.
                    agent.pending.push((*request).clone());
                    let r = self
                        .transition(agent, Event::PermissionRequested, None)
                        .map(|_| ());
                    if r.is_ok() {
                        self.forward(
                            agent,
                            StreamEvent::PermissionRequested { provider, request },
                        );
                    } else {
                        agent.pending.pop();
                        self.withdraw_agent_request(
                            agent_id,
                            &request.id,
                            &PermissionResponse::auto_deny("Not accepted in this state"),
                        );
                    }
                    r
                }
            }
            StreamEvent::PermissionResolved {
                request_id,
                resolution,
                provider,
            } => match agent.pending.iter().position(|p| p.id == request_id) {
                Some(i) => {
                    agent.pending.remove(i);
                    self.withdraw_agent_request(agent_id, &request_id, &resolution);
                    let r = self
                        .transition(agent, Event::PermissionResolved, None)
                        .map(|_| ());
                    self.forward(
                        agent,
                        StreamEvent::PermissionResolved {
                            provider,
                            request_id,
                            resolution,
                        },
                    );
                    r
                }
                None => Ok(()),
            },
            StreamEvent::ProviderSubagent { provider, event } => {
                self.apply_subagent(agent, &provider, event);
                Ok(())
            }
            other @ StreamEvent::AttentionRequired { .. } => {
                self.forward(agent, other);
                Ok(())
            }
        };
        if let Err(e) = result {
            tracing::debug!(agent = agent_id, error = %e, "ignored provider event in this state");
        }
        self.persist(agent);
        self.publish_state(agent);
        drop(agents);
        self.bump();
    }

    fn spawn_pump(&self, agent_id: String, mut rx: mpsc::UnboundedReceiver<StreamEvent>) {
        let manager = self.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                manager.on_event(&agent_id, event);
            }
        });
    }

    // -----------------------------------------------------------------------
    // Operations

    pub async fn create(&self, mut req: CreateRequest) -> Result<AgentSnapshot, ManagerError> {
        if let Some(key) = req.idempotency_key.as_deref() {
            if let Some(id) = self.inner.store.find_by_idempotency_key(key)? {
                return self.snapshot(&id);
            }
        }
        if let Some(category) = req.chain.clone() {
            let mut lanes: Vec<(String, &'static str, Lane)> = self
                .inner
                .lanes
                .chain(&category)
                .into_iter()
                .filter(|(_, p, _)| self.inner.providers.contains_key(*p))
                .collect();
            let mut tripped = None;
            lanes.retain(|(_, p, _)| match self.check_breaker(p) {
                Ok(()) => true,
                Err(e) => {
                    tripped.get_or_insert(e);
                    false
                }
            });
            let chain_cwd = if req.cwd.trim().is_empty() {
                req.workspace_id
                    .as_deref()
                    .and_then(|wid| self.inner.workspaces.get(wid))
                    .filter(|w| w.archived_at.is_none())
                    .map(|w| w.cwd.clone())
                    .unwrap_or_default()
            } else {
                req.cwd.clone()
            };
            if crate::workspaces::is_lucina_project(
                &self.inner.options.workspaces,
                std::path::Path::new(&chain_cwd),
            ) {
                lanes.retain(|(_, p, _)| *p != "codex");
            }
            let Some((first_name, first_provider, first_lane)) = lanes.first().cloned() else {
                if let Some(e) = tripped {
                    return Err(e);
                }
                return Err(ManagerError::Invalid(format!(
                    "no lane of chain \"{category}\" can run here"
                )));
            };
            req.provider = first_provider.to_string();
            req.model = first_lane.model.clone();
            req.thinking_option_id = first_lane.effort.clone();
            req.labels.insert("lucina.chain".into(), category.clone());
            req.labels.insert("lucina.lane".into(), first_name);
            let prompt = req.prompt.take();
            if req.title.is_none() {
                req.title = resolve_titles(None, prompt.as_deref()).1;
            }
            let snap = Box::pin(self.create(CreateRequest { chain: None, ..req })).await?;
            if let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) {
                if let Some(a) = self.lock().get_mut(&snap.id) {
                    a.chain_active = true;
                }
                let manager = self.clone();
                let id = snap.id.clone();
                tokio::spawn(async move { manager.run_chain(id, prompt, lanes).await });
            }
            return self.snapshot(&snap.id);
        }
        let (provider_id, model) = split_provider(&req.provider, req.model.clone());
        if !req.claude_session
            && (req.external || provider_id == ext::PROVIDER || ext::is_session(&provider_id))
        {
            Self::check_external(&mut req, &provider_id)?;
        }
        let workspace = match &req.workspace_id {
            Some(wid) => Some(
                self.inner
                    .workspaces
                    .get(wid)
                    .filter(|w| w.archived_at.is_none())
                    .ok_or_else(|| WsError::NotFound(format!("Workspace not found: {wid}")))?,
            ),
            None => None,
        };
        if req.cwd.trim().is_empty() {
            match &workspace {
                Some(w) => req.cwd = w.cwd.clone(),
                None => return Err(ManagerError::Invalid("cwd is required".into())),
            }
        }
        if provider_id == "codex" {
            if let Some(repo) = crate::workspaces::lucina_repo_name(
                &self.inner.options.workspaces,
                std::path::Path::new(&req.cwd),
            ) {
                return Err(ManagerError::CodexNotAllowed(format!(
                    "Codex is not used on Lucina projects (owner rule). Pick Claude, Mistral or Qwen for {repo}."
                )));
            }
        }
        let provider = self
            .inner
            .providers
            .get(&provider_id)
            .cloned()
            .ok_or_else(|| ManagerError::UnknownProvider(provider_id.clone()))?;
        self.check_breaker(&provider_id)?;
        // Caps (see `breaker`): only a create from the create route, for an agent whose provider
        // process agentd spawns. The slot is held from here until the agent is in the table.
        req.labels.remove(CALLER_LABEL);
        let spawns = !req.claude_session
            && !req.external
            && !req.internal
            && provider_id != ext::PROVIDER
            && !ext::is_session(&provider_id);
        let mut slot = match (&req.created_by, spawns) {
            (Some(caller), true) => {
                req.labels.insert(CALLER_LABEL.into(), caller.clone());
                Some(self.reserve_slot(caller)?)
            }
            _ => None,
        };
        let info = provider.info();
        // The Juno shell tool only ever attaches to a Claude agent in a mode that
        // asks before each call, with no juno tool preapproved (fork lands here too).
        if juno_tool(&req.labels) {
            let mode = req.mode_id.clone().or_else(|| info.default_mode_id.clone());
            check_juno_tool(&provider_id, mode.as_deref(), req.tool_policy.as_ref())?;
        }
        if !req.internal && !req.external {
            let mode = req.mode_id.clone().or_else(|| info.default_mode_id.clone());
            self.guard_start(
                &req.cwd,
                &provider_id,
                mode.as_deref(),
                req.allow_main_checkout,
            )
            .await?;
        }
        let workspace_id = match req.external && !req.claude_session {
            // Its cwd is a path on another host.
            true => None,
            false => workspace.map(|w| w.workspace_id).or_else(|| {
                self.inner
                    .workspaces
                    .find_by_cwd(&req.cwd)
                    .map(|w| w.workspace_id)
            }),
        };
        let (_, provisional) = resolve_titles(req.title.as_deref(), req.prompt.as_deref());
        let now = now_iso();
        let record = AgentRecord {
            id: uuid::Uuid::new_v4().to_string(),
            provider: provider_id,
            cwd: req.cwd.clone(),
            workspace_id,
            created_at: now.clone(),
            updated_at: now,
            last_activity_at: None,
            last_user_message_at: None,
            title: provisional,
            labels: req.labels.clone(),
            last_status: AgentStatus::Initializing,
            last_mode_id: req.mode_id.clone().or_else(|| info.default_mode_id.clone()),
            config: AgentConfig {
                mode_id: req.mode_id.clone(),
                model,
                thinking_option_id: req.thinking_option_id.clone(),
                feature_values: (!req.feature_values.is_empty())
                    .then(|| req.feature_values.clone()),
                tool_policy: req.tool_policy.clone(),
                ..AgentConfig::default()
            },
            runtime_info: None,
            persistence: None,
            last_error: None,
            last_usage: None,
            requires_attention: false,
            attention_reason: None,
            attention_timestamp: None,
            internal: req.internal,
            archived_at: None,
            idempotency_key: req.idempotency_key.clone(),
            epoch: uuid::Uuid::new_v4().to_string(),
            gate: None,
            gate_request: (req.verify.is_some() || req.auto_fix > 0).then(|| GateRequest {
                verify: req.verify.clone().filter(|v| !v.trim().is_empty()),
                heavy: req.verify_heavy,
                auto_fix: req.auto_fix.min(5),
            }),
            external: req.external,
            owner_device_id: req.owner_device_id.clone(),
            host: req.host.clone(),
            seed_context: req.seed_context.clone().filter(|c| !c.trim().is_empty()),
        };
        self.inner.store.save(&record)?;
        let id = record.id.clone();
        let mut env: Vec<(String, String)> = req.env.clone().into_iter().collect();
        // A nested `lagent run` inside the agent reuses its workspace (Paseo: PASEO_WORKSPACE_ID).
        env.push(("LUCINA_AGENT_ID".into(), id.clone()));
        if let Some(w) = &record.workspace_id {
            env.push(("LUCINA_WORKSPACE_ID".into(), w.clone()));
            // Worktrees of a Rust repo share one target dir (one `target/` per worktree
            // filled nezopt's disk before).
            if let Some(root) = self
                .inner
                .workspaces
                .get(w)
                .and_then(|ws| ws.main_repo_root)
            {
                let root = std::path::Path::new(&root);
                if root.join("Cargo.toml").is_file() && !req.env.contains_key("CARGO_TARGET_DIR") {
                    let name = root
                        .file_name()
                        .map(|n| n.to_string_lossy().to_lowercase())
                        .unwrap_or_default();
                    let home = std::env::var("HOME").unwrap_or_default();
                    env.push((
                        "CARGO_TARGET_DIR".into(),
                        format!("{home}/.cache/cargo-target/{name}"),
                    ));
                }
            }
        }
        let config = session_config(&record, env);
        {
            let mut agents = self.lock();
            let mut agent = Agent {
                k: AgentState::new(ext::is_session(&record.provider)),
                record,
                session: None,
                pending: Vec::new(),
                active_turn: None,
                rows: Vec::new(),
                capabilities: info.capabilities.clone(),
                modes: info.modes.clone(),
                echoes_user_message: info.echoes_user_message,
                chain_active: false,
                turn_base: None,
                gate_pending: false,
                gate_running: false,
                gate_again: false,
                subagents: Vec::new(),
                rewinding: false,
                released: false,
            };
            self.persist(&mut agent);
            self.publish_state(&agent);
            agents.insert(id.clone(), agent);
            // In the table now: it counts as an agent, no longer as a reservation.
            if let Some(slot) = slot.as_mut() {
                slot.commit();
            }
        }
        let (tx, rx) = mpsc::unbounded_channel();
        match provider.create_session(config, tx).await {
            Ok(session) => {
                self.spawn_pump(id.clone(), rx);
                let mut agents = self.lock();
                if let Some(agent) = agents.get_mut(&id) {
                    agent.session = Some(session);
                    self.transition(agent, Event::HandshakeDone, None)?;
                    self.persist(agent);
                    self.publish_state(agent);
                }
            }
            Err(e) => {
                let mut agents = self.lock();
                if let Some(agent) = agents.get_mut(&id) {
                    self.transition(agent, Event::LoadFailed, Some(&e.0))?;
                    self.persist(agent);
                    self.publish_state(agent);
                }
                drop(agents);
                self.bump();
                return Err(ManagerError::Provider(e.0));
            }
        }
        self.bump();
        if let Some(prompt) = req.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
            self.start_turn(&id, prompt).await?;
        }
        self.snapshot(&id)
    }

    /// Change an agent's mode. An agent with the `codecor.juno` label only ever gets
    /// a mode that asks before each call (the Juno shell tool's rule): a forbidden
    /// mode is refused the same way as at create.
    pub fn set_mode(&self, id: &str, mode: Option<String>) -> Result<AgentSnapshot, ManagerError> {
        let snap = {
            let mut agents = self.lock();
            let agent = agents
                .get_mut(id)
                .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            if juno_tool(&agent.record.labels) {
                check_juno_tool(&agent.record.provider, mode.as_deref(), None)?;
            }
            agent.record.config.mode_id = mode.clone();
            agent.record.last_mode_id = mode;
            self.persist(agent);
            self.publish_state(agent);
            Self::snapshot_of(agent)
        };
        self.bump();
        Ok(snap)
    }

    /// Load a closed agent's session again (Paseo `ensureAgentLoaded`).
    async fn ensure_loaded(&self, id: &str) -> Result<Arc<dyn Session>, ManagerError> {
        let (record, released) = {
            let mut agents = self.lock();
            let agent = agents
                .get_mut(id)
                .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            if let Some(s) = &agent.session {
                return Ok(s.clone());
            }
            // An agent that failed and gave up its session stays in `error`; it gets a new
            // session here, and its next turn starts from `error`.
            let released = agent.released && agent.k.status == AgentStatus::Error;
            if !released {
                self.transition(agent, Event::Resume, None)?;
            }
            self.publish_state(agent);
            (agent.record.clone(), released)
        };
        let provider = self
            .inner
            .providers
            .get(&record.provider)
            .cloned()
            .ok_or_else(|| ManagerError::UnknownProvider(record.provider.clone()))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let config = session_config(&record, Vec::new());
        let result = match &record.persistence {
            Some(handle) => provider.resume_session(handle, config, tx).await,
            None => provider.create_session(config, tx).await,
        };
        let mut agents = self.lock();
        let agent = agents
            .get_mut(id)
            .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        match result {
            Ok(session) => {
                agent.session = Some(session.clone());
                if released {
                    agent.released = false;
                } else {
                    self.transition(agent, Event::HandshakeDone, None)?;
                }
                self.persist(agent);
                self.publish_state(agent);
                drop(agents);
                self.spawn_pump(id.to_string(), rx);
                self.bump();
                Ok(session)
            }
            Err(e) => {
                if !released {
                    self.transition(agent, Event::LoadFailed, Some(&e.0))?;
                }
                self.persist(agent);
                self.publish_state(agent);
                Err(ManagerError::Provider(e.0))
            }
        }
    }

    /// A foreground turn: running right away (Paseo `streamAgent`), the provider's
    /// own `turn_started` is then a no-op.
    async fn start_turn(&self, id: &str, prompt: &str) -> Result<(), ManagerError> {
        let session = self.ensure_loaded(id).await?;
        self.note_base(id).await;
        // A fork's chat history goes ahead of its first prompt, once; the timeline shows
        // the prompt as typed (a provider that echoes it shows what it was sent).
        let (sent, seed) = {
            let mut agents = self.lock();
            let agent = agents
                .get_mut(id)
                .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            self.transition(agent, Event::TurnStarted, None)?;
            if !agent.echoes_user_message {
                self.append(
                    agent,
                    TimelineItem::UserMessage {
                        text: prompt.to_string(),
                        message_id: None,
                        client_message_id: None,
                    },
                    None,
                );
            }
            agent.record.last_user_message_at = Some(now_iso());
            let seed = agent.record.seed_context.take();
            let sent = match &seed {
                Some(context) => format!("{context}\n\n{prompt}"),
                None => prompt.to_string(),
            };
            self.persist(agent);
            self.publish_state(agent);
            (sent, seed)
        };
        self.bump();
        match session.start_turn(&sent, None).await {
            Ok(turn_id) => {
                let mut agents = self.lock();
                if let Some(agent) = agents.get_mut(id) {
                    if agent.active_turn.is_none() && agent.k.status == AgentStatus::Running {
                        agent.active_turn = Some(ActiveTurn {
                            turn_id,
                            started_at: Some(now_iso()),
                        });
                    }
                }
                Ok(())
            }
            Err(e) => {
                let message = format_turn_failed(&e.0, None, None);
                let mut agents = self.lock();
                if let Some(agent) = agents.get_mut(id) {
                    let failed = self
                        .transition(agent, Event::TurnFailed, Some(&message))
                        .is_ok();
                    agent.record.last_error = Some(e.0.clone());
                    // The history wasn't delivered: the next prompt carries it.
                    agent.record.seed_context = seed;
                    if let Some((class, detector)) =
                        failed.then(|| self.handoff_check(agent)).flatten()
                    {
                        self.spawn_handoff(id.to_string(), class, detector);
                    }
                    if failed {
                        self.breaker_observe_failure(agent);
                        self.release_if_errored(agent);
                    }
                    self.persist(agent);
                    self.publish_state(agent);
                }
                drop(agents);
                self.bump();
                Err(ManagerError::Provider(e.0))
            }
        }
    }

    pub async fn send(
        &self,
        id: &str,
        prompt: &str,
        behavior: ActiveTurnBehavior,
    ) -> Result<Disposition, ManagerError> {
        if prompt.trim().is_empty() {
            return Err(ManagerError::Invalid("A prompt is required".into()));
        }
        if let Some((owner, running)) = self.external_of(id)? {
            let prompt = prompt.to_string();
            // A Claude session is only ever told something: it reads it at its next tool call or prompt.
            let bodies = match (running, behavior) {
                _ if self.is_claude_session(id) => vec![CommandBody::Send { prompt }],
                (true, ActiveTurnBehavior::Steer) => vec![CommandBody::Steer { prompt }],
                (true, ActiveTurnBehavior::Interrupt) => {
                    vec![CommandBody::Stop {}, CommandBody::Send { prompt }]
                }
                (false, _) => vec![CommandBody::Send { prompt }],
            };
            for body in bodies {
                self.inner.commands.enqueue(id, &owner, body)?;
            }
            return Ok(Disposition::Queued);
        }
        if self.lock().get(id).is_some_and(|a| a.rewinding) {
            return Err(ManagerError::Busy(
                "The agent is being rewound; send again when it is done".into(),
            ));
        }
        let session = self.ensure_loaded(id).await?;
        if let Some(events) = session.out_of_band(prompt) {
            for ev in events {
                self.on_event(id, ev);
            }
            return Ok(Disposition::OutOfBand);
        }
        let (running, turn) = {
            let agents = self.lock();
            let a = agents
                .get(id)
                .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            (
                a.k.status == AgentStatus::Running,
                a.active_turn.as_ref().map(|t| t.turn_id.clone()),
            )
        };
        if running {
            if behavior == ActiveTurnBehavior::Steer {
                if let Some(turn) = &turn {
                    if session
                        .steer(prompt, turn)
                        .await
                        .map_err(|e| ManagerError::Provider(e.0))?
                    {
                        let mut agents = self.lock();
                        if let Some(agent) = agents.get_mut(id) {
                            self.append(
                                agent,
                                TimelineItem::UserMessage {
                                    text: prompt.to_string(),
                                    message_id: None,
                                    client_message_id: None,
                                },
                                Some(turn.clone()),
                            );
                            self.persist(agent);
                        }
                        return Ok(Disposition::Steered);
                    }
                }
            }
            session
                .interrupt()
                .await
                .map_err(|e| ManagerError::Provider(e.0))?;
            self.wait_not_running(id, Duration::from_secs(10)).await;
        }
        self.start_turn(id, prompt).await?;
        Ok(Disposition::TurnStarted)
    }

    async fn wait_not_running(&self, id: &str, limit: Duration) {
        let mut rx = self.inner.generation.subscribe();
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let running = self
                .lock()
                .get(id)
                .is_some_and(|a| a.k.status == AgentStatus::Running);
            if !running {
                return;
            }
            if tokio::time::timeout_at(deadline, rx.changed())
                .await
                .is_err()
            {
                return;
            }
        }
    }

    /// Interrupt a running agent; `false` when it wasn't running.
    /// Interrupt the agent's turn, then kill its terminals (also when it was idle).
    pub async fn stop(&self, id: &str) -> Result<bool, ManagerError> {
        let stopped = self.stop_turn(id).await?;
        self.kill_agent_terminals(id).await;
        Ok(stopped)
    }

    async fn stop_turn(&self, id: &str) -> Result<bool, ManagerError> {
        self.refuse_for_session(id, "stopped")?;
        if let Some((owner, running)) = self.external_of(id)? {
            if running {
                self.inner
                    .commands
                    .enqueue(id, &owner, CommandBody::Stop {})?;
            }
            return Ok(running);
        }
        let session = {
            let agents = self.lock();
            let a = agents
                .get(id)
                .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            if a.k.status != AgentStatus::Running {
                return Ok(false);
            }
            a.session.clone()
        };
        match session {
            Some(s) => s
                .interrupt()
                .await
                .map_err(|e| ManagerError::Provider(e.0))?,
            None => {
                let mut agents = self.lock();
                if let Some(agent) = agents.get_mut(id) {
                    self.transition(agent, Event::TurnCanceled, None)?;
                    self.persist(agent);
                    self.publish_state(agent);
                }
            }
        }
        self.wait_not_running(id, Duration::from_secs(10)).await;
        Ok(true)
    }

    /// The agent's terminals and scripts go when it stops, is archived or deleted.
    async fn kill_agent_terminals(&self, id: &str) {
        let n = self
            .inner
            .terminals
            .kill_where(|t| t.agent_id.as_deref() == Some(id))
            .await;
        if n > 0 {
            tracing::info!(agent = %id, terminals = n, "killed the agent's terminals");
        }
    }

    pub fn devices(&self) -> &crate::devices::Devices {
        &self.inner.devices
    }

    pub fn terminals(&self) -> &crate::terminals::Hub {
        &self.inner.terminals
    }

    async fn finish_effects(&self, effects: &[Effect], session: Option<Arc<dyn Session>>) {
        if let Some(s) = session {
            if effects.contains(&Effect::Interrupt) {
                let _ = s.interrupt().await;
            }
            if effects.contains(&Effect::CloseRuntime) || effects.contains(&Effect::RemoveRecord) {
                s.close().await;
            }
        }
    }

    pub async fn archive(&self, id: &str) -> Result<String, ManagerError> {
        self.refuse_for_live_session(id, "archived")?;
        let (effects, session, children, archived_at) = {
            let mut agents = self.lock();
            let agent = agents
                .get_mut(id)
                .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            let effects = self.transition(agent, Event::Archive, None)?;
            let archived_at = now_iso();
            agent.record.archived_at = Some(archived_at.clone());
            agent.active_turn = None;
            let session = if effects.contains(&Effect::CloseRuntime) {
                agent.session.take()
            } else {
                None
            };
            if session.is_some() {
                self.cancel_running_subagents(agent);
            }
            self.persist(agent);
            self.publish_state(agent);
            let children: Vec<String> = agents
                .values()
                .filter(|a| {
                    a.record.archived_at.is_none()
                        && a.record.labels.get(PARENT_LABEL).map(String::as_str) == Some(id)
                })
                .map(|a| a.record.id.clone())
                .collect();
            (effects, session, children, archived_at)
        };
        self.finish_effects(&effects, session).await;
        self.kill_agent_terminals(id).await;
        if effects.contains(&Effect::CascadeArchiveChildren) {
            for child in children {
                if let Err(e) = Box::pin(self.archive(&child)).await {
                    tracing::warn!(child = %child, error = %e, "cascade archive failed");
                }
            }
        }
        self.bump();
        Ok(archived_at)
    }

    pub fn unarchive(&self, id: &str) -> Result<AgentSnapshot, ManagerError> {
        let mut agents = self.lock();
        let agent = agents
            .get_mut(id)
            .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        self.transition(agent, Event::Unarchive, None)?;
        agent.record.archived_at = None;
        self.persist(agent);
        self.publish_state(agent);
        let snap = Self::snapshot_of(agent);
        drop(agents);
        self.bump();
        Ok(snap)
    }

    pub async fn delete(&self, id: &str) -> Result<(), ManagerError> {
        self.refuse_for_live_session(id, "deleted")?;
        self.forget_session_of(id);
        let (effects, session) = {
            let mut agents = self.lock();
            let agent = agents
                .get_mut(id)
                .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            let effects = self.transition(agent, Event::Delete, None)?;
            let session = agent.session.take();
            self.remove_subagents(agent);
            agents.remove(id);
            (effects, session)
        };
        self.finish_effects(&effects, session).await;
        self.kill_agent_terminals(id).await;
        self.inner.store.delete(id)?;
        let _ = self.inner.bus.send(BusEvent::AgentDeleted {
            agent_id: id.to_string(),
        });
        self.bump();
        Ok(())
    }

    /// Paseo `waitForFinish`: until the agent stops running, needs permission,
    /// or `timeout` passes (`None` = no limit). With `gate`, also until the
    /// finished-run gate has checked the run (and any auto-fix rounds are done).
    /// A run handed on to another agent (manager/handoff.rs) is followed to that agent.
    pub async fn wait(
        &self,
        id: &str,
        timeout: Option<Duration>,
        gate: bool,
    ) -> Result<WaitResult, ManagerError> {
        let mut rx = self.inner.generation.subscribe();
        let deadline = timeout.map(|t| tokio::time::Instant::now() + t);
        let mut id = id.to_string();
        loop {
            {
                let agents = self.lock();
                let a = agents
                    .get(&id)
                    .ok_or_else(|| ManagerError::NotFound(id.clone()))?;
                let handing_off = a.k.status == AgentStatus::Error
                    && a.record
                        .labels
                        .get(HANDOFF_LABEL)
                        .is_some_and(|v| v == "pending");
                if a.k.status == AgentStatus::Error {
                    if let Some(next) = a
                        .record
                        .labels
                        .get(CONTINUED_ON_LABEL)
                        .filter(|n| agents.contains_key(n.as_str()))
                    {
                        id = next.clone();
                        continue;
                    }
                }
                let gating = gate && a.pending.is_empty() && a.gate_pending;
                let status = if gating || handing_off || (a.chain_active && a.pending.is_empty()) {
                    None
                } else if !a.pending.is_empty() {
                    Some("permission")
                } else {
                    match a.k.status {
                        AgentStatus::Error => Some("error"),
                        AgentStatus::Idle | AgentStatus::Closed => Some("idle"),
                        AgentStatus::Running | AgentStatus::Initializing => None,
                    }
                };
                if let Some(status) = status {
                    return Ok(WaitResult {
                        status,
                        error: (status == "error")
                            .then(|| a.record.last_error.clone())
                            .flatten(),
                        last_message: last_assistant_text(&a.rows),
                        final_snapshot: Self::snapshot_of(a),
                    });
                }
            }
            let changed = rx.changed();
            match deadline {
                Some(d) => {
                    if tokio::time::timeout_at(d, changed).await.is_err() {
                        let agents = self.lock();
                        let a = agents
                            .get(&id)
                            .ok_or_else(|| ManagerError::NotFound(id.clone()))?;
                        return Ok(WaitResult {
                            status: "timeout",
                            error: None,
                            last_message: last_assistant_text(&a.rows),
                            final_snapshot: Self::snapshot_of(a),
                        });
                    }
                }
                None => {
                    if changed.await.is_err() {
                        return Err(ManagerError::Provider("daemon shutting down".into()));
                    }
                }
            }
        }
    }

    /// Note something on an agent's timeline (lane decisions).
    fn note(&self, id: &str, level: lucina_agents::timeline::NotificationLevel, message: String) {
        let mut agents = self.lock();
        if let Some(agent) = agents.get_mut(id) {
            self.append(agent, TimelineItem::Notification { level, message }, None);
            self.persist(agent);
        }
    }

    /// Close the session and continue on another lane's provider (same agent).
    async fn switch_lane(
        &self,
        id: &str,
        lane_name: &str,
        provider: &str,
        lane: &Lane,
    ) -> Result<(), ManagerError> {
        let info = self
            .inner
            .providers
            .get(provider)
            .map(|p| p.info())
            .ok_or_else(|| ManagerError::UnknownProvider(provider.into()))?;
        let session = {
            let mut agents = self.lock();
            let agent = agents
                .get_mut(id)
                .ok_or_else(|| ManagerError::NotFound(id.into()))?;
            if agent.k.status != AgentStatus::Closed {
                self.transition(agent, Event::Close, None)?;
            }
            agent.session.take()
        };
        if let Some(s) = session {
            s.close().await;
        }
        let mut agents = self.lock();
        let agent = agents
            .get_mut(id)
            .ok_or_else(|| ManagerError::NotFound(id.into()))?;
        agent.record.provider = provider.to_string();
        agent.record.config.model = lane.model.clone();
        agent.record.config.thinking_option_id = lane.effort.clone();
        agent.record.config.mode_id = None;
        agent.record.persistence = None;
        agent.record.runtime_info = None;
        agent
            .record
            .labels
            .insert("lucina.lane".into(), lane_name.to_string());
        agent.capabilities = info.capabilities.clone();
        agent.modes = info.modes.clone();
        agent.echoes_user_message = info.echoes_user_message;
        self.persist(agent);
        self.publish_state(agent);
        Ok(())
    }

    /// The text a failed attempt is classified on: the error and the last system
    /// error item (which carries the provider's code, e.g. `code: quota`).
    fn failure_text(&self, id: &str) -> String {
        let agents = self.lock();
        let Some(a) = agents.get(id) else {
            return String::new();
        };
        let last_system = a.rows.iter().rev().find_map(|r| match &r.item {
            TimelineItem::AssistantMessage { text, .. }
                if text.starts_with(lucina_agents::events::SYSTEM_ERROR_PREFIX) =>
            {
                Some(text.clone())
            }
            _ => None,
        });
        format!(
            "{}\n{}",
            a.record.last_error.clone().unwrap_or_default(),
            last_system.unwrap_or_default()
        )
    }

    async fn worktree_status(cwd: &str) -> Option<String> {
        let out = tokio::process::Command::new("git")
            .args(["-C", cwd, "status", "--porcelain"])
            .output()
            .await
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// lane's rules: try each lane in order; fall through only on quota or
    /// unavailable, and never when the failed attempt left changes in the workspace.
    async fn run_chain(
        &self,
        id: String,
        prompt: String,
        lanes: Vec<(String, &'static str, Lane)>,
    ) {
        use lucina_agents::timeline::NotificationLevel::{Info, Warning};
        let cwd = self
            .lock()
            .get(&id)
            .map(|a| a.record.cwd.clone())
            .unwrap_or_default();
        let baseline = Self::worktree_status(&cwd).await;
        let lucina = crate::workspaces::is_lucina_project(
            &self.inner.options.workspaces,
            std::path::Path::new(&cwd),
        );
        for (i, (name, provider, lane)) in lanes.iter().enumerate() {
            if *provider == "codex" && lucina {
                continue;
            }
            if i > 0 {
                if let Err(e) = self.guard_start(&cwd, provider, None, false).await {
                    self.note(&id, Warning, format!("lane {name}: {e}; skipped"));
                    continue;
                }
                if let Err(e) = self.switch_lane(&id, name, provider, lane).await {
                    self.note(
                        &id,
                        Warning,
                        format!("lane {name}: cannot switch ({e}); the chain stops"),
                    );
                    break;
                }
            }
            let started = self.start_turn(&id, &prompt).await;
            if started.is_ok() {
                let mut rx = self.inner.generation.subscribe();
                loop {
                    let busy = self.lock().get(&id).is_some_and(|a| {
                        a.pending.is_empty()
                            && matches!(
                                a.k.status,
                                AgentStatus::Running | AgentStatus::Initializing
                            )
                    });
                    if !busy || rx.changed().await.is_err() {
                        break;
                    }
                }
            }
            let (status, waiting) = match self.lock().get(&id) {
                Some(a) => (a.k.status, !a.pending.is_empty()),
                None => return,
            };
            if waiting || (started.is_ok() && status != AgentStatus::Error) {
                break;
            }
            let text = match &started {
                Err(e) => format!("{e}\n{}", self.failure_text(&id)),
                Ok(()) => self.failure_text(&id),
            };
            let outcome = if text.contains("code: quota") {
                Outcome::Quota("provider-quota-reply".into())
            } else {
                self.inner.lanes.classify(name, &lane.kind, &text, None)
            };
            let (what, detector) = match &outcome {
                Outcome::Quota(d) => ("quota", d.clone()),
                Outcome::Unavailable(d) => ("unavailable", d.clone()),
                Outcome::Failed => {
                    self.note(
                        &id,
                        Warning,
                        format!("lane {name}: failed; the chain stops"),
                    );
                    break;
                }
            };
            if Self::worktree_status(&cwd).await != baseline {
                self.note(&id, Warning, format!("lane {name}: {what} ({detector}) after changing the workspace; the chain stops (dirty_after_quota)"));
                break;
            }
            match lanes.get(i + 1) {
                Some((next, _, _)) => self.note(
                    &id,
                    Info,
                    format!("lane {name}: {what} ({detector}); trying lane {next}"),
                ),
                None => self.note(
                    &id,
                    Warning,
                    format!("lane {name}: {what} ({detector}); no lanes left"),
                ),
            }
        }
        if let Some(a) = self.lock().get_mut(&id) {
            a.chain_active = false;
            self.release_if_errored(a);
        }
        self.bump();
    }

    /// Stop every running turn and close every session, all at once (shutdown).
    pub async fn shutdown(&self) {
        let sessions: Vec<Arc<dyn Session>> = self
            .lock()
            .values_mut()
            .filter_map(|a| a.session.take())
            .collect();
        let mut set = tokio::task::JoinSet::new();
        for s in sessions {
            set.spawn(async move { s.close().await });
        }
        while set.join_next().await.is_some() {}
        self.inner.terminals.shutdown().await;
    }
}

fn last_assistant_text(rows: &[TimelineRow]) -> Option<String> {
    let items: Vec<TimelineItem> = rows.iter().map(|r| r.item.clone()).collect();
    lucina_agents::projection::project_rows(&lucina_agents::projection::rows_from_items(&items))
        .into_iter()
        .rev()
        .find_map(|e| match e.item {
            TimelineItem::AssistantMessage { text, .. } => Some(text),
            _ => None,
        })
}

/// Paseo `agent-attention-notification.js` titles.
fn attention_event(agent: &Agent, reason: AttentionReason, timestamp: String) -> StreamEvent {
    let title = match reason {
        AttentionReason::Finished => "Agent finished",
        AttentionReason::Permission => "Agent needs permission",
        AttentionReason::Error => "Agent needs attention",
    };
    let assistant = match reason {
        AttentionReason::Finished => {
            let items: Vec<TimelineItem> = agent.rows.iter().map(|r| r.item.clone()).collect();
            lucina_agents::notification::latest_assistant_message(&items)
        }
        _ => None,
    };
    let body =
        lucina_agents::notification::body(reason, assistant.as_deref(), agent.pending.last());
    StreamEvent::AttentionRequired {
        provider: agent.record.provider.clone(),
        reason,
        timestamp,
        should_notify: reason != AttentionReason::Error,
        notification: Some(lucina_agents::events::AttentionNotification {
            title: title.to_string(),
            body,
            data: lucina_agents::events::AttentionNotificationData {
                server_id: "lucina-agentd".into(),
                workspace_id: agent.record.workspace_id.clone(),
                agent_id: agent.record.id.clone(),
                reason,
            },
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn labels(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn juno_tool_label_opt_in() {
        assert!(!juno_tool(&labels(&[])));
        assert!(juno_tool(&labels(&[(JUNO_LABEL, JUNO_LABEL_VALUE)])));
        assert!(!juno_tool(&labels(&[(JUNO_LABEL, "no")])));
        assert!(!juno_tool(&labels(&[("codecor.other", "bash")])));
    }

    #[test]
    fn juno_tool_create_refusals() {
        // Ask mode (2026-11-07 behaviour). Only a Claude agent.
        for p in ["mock", "codex", "secretary-code"] {
            assert!(
                matches!(
                    check_juno_tool_with(p, Some("default"), None, true),
                    Err(ManagerError::JunoToolRefused(_))
                ),
                "{p}"
            );
        }
        // Only modes that ask before each call (None = the provider default, claude's `default`).
        for m in [Some("default"), Some("plan"), Some("acceptEdits"), None] {
            assert!(
                check_juno_tool_with("claude", m, None, true).is_ok(),
                "{m:?}"
            );
        }
        for m in ["bypassPermissions", "dontAsk", "auto", "whatever"] {
            assert!(
                matches!(
                    check_juno_tool_with("claude", Some(m), None, true),
                    Err(ManagerError::JunoToolRefused(_))
                ),
                "{m}"
            );
        }
        // A tool policy that preapproves a `mcp__juno__*` tool is refused.
        let tp = json!({"preapproved": [{"kind": "mcp", "server": "juno", "tool": "juno_bash"}]});
        assert!(matches!(
            check_juno_tool_with("claude", Some("default"), Some(&tp), true),
            Err(ManagerError::JunoToolRefused(_))
        ));
        let tp = json!({"preapproved": [{"kind": "mcp", "server": "juno", "tool": "anything"}]});
        assert!(matches!(
            check_juno_tool_with("claude", None, Some(&tp), true),
            Err(ManagerError::JunoToolRefused(_))
        ));
        // Other preapprovals are fine.
        let tp = json!({"preapproved": [{"kind": "mcp", "server": "lucina-agents", "tool": "list_agents"}]});
        assert!(check_juno_tool_with("claude", Some("plan"), Some(&tp), true).is_ok());
        assert!(check_juno_tool_with("claude", Some("default"), None, true).is_ok());
        // The error's code and message.
        let e = ManagerError::JunoToolRefused(
            "mode \"bypassPermissions\" does not ask before each call".into(),
        );
        assert_eq!(e.code(), "JUNO_TOOL_REFUSED");
        assert!(e.to_string().contains(
            "The Juno shell tool needs a Claude agent in a mode that asks before each call"
        ));
    }

    #[test]
    fn juno_tool_allowed_without_ask_mode() {
        // Ask mode off (owner decision 2026-11-08): only the provider is checked.
        for m in [
            Some("default"),
            Some("plan"),
            Some("acceptEdits"),
            Some("bypassPermissions"),
            Some("dontAsk"),
            Some("whatever"),
            None,
        ] {
            assert!(
                check_juno_tool_with("claude", m, None, false).is_ok(),
                "{m:?}"
            );
        }
        // A tool policy that preapproves a `mcp__juno__*` tool is fine.
        let tp = json!({"preapproved": [{"kind": "mcp", "server": "juno", "tool": "juno_bash"}]});
        assert!(check_juno_tool_with("claude", Some("default"), Some(&tp), false).is_ok());
        // A non-claude provider is refused in both modes.
        for p in ["mock", "codex", "secretary-code"] {
            assert!(
                matches!(
                    check_juno_tool_with(p, Some("default"), None, false),
                    Err(ManagerError::JunoToolRefused(_))
                ),
                "{p}"
            );
            assert!(
                matches!(
                    check_juno_tool_with(p, Some("default"), None, true),
                    Err(ManagerError::JunoToolRefused(_))
                ),
                "{p}"
            );
        }
    }

    #[test]
    fn juno_tool_mode_change_refusal() {
        // The same check `set_mode` applies to a later mode change (ask mode).
        assert!(matches!(
            check_juno_tool_with("claude", Some("bypassPermissions"), None, true),
            Err(ManagerError::JunoToolRefused(_))
        ));
        assert!(check_juno_tool_with("claude", Some("acceptEdits"), None, true).is_ok());
        assert!(check_juno_tool_with("claude", None, None, true).is_ok());
    }
}
