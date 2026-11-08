//! Agents that speak the Agent Client Protocol over stdio (JSON-RPC, one JSON object
//! per line): Cursor (`cursor-agent acp`), OpenCode (`opencode acp`), Copilot
//! (`copilot --acp`), and any agent listed in `~/.config/lucina-agentd/acp.toml`.
//! Paseo: `acp-agent.js`, `generic-acp-agent.js`, `cursor-acp-agent.js`,
//! `copilot-acp-agent.js` (Paseo drives OpenCode through `opencode serve`; its ACP
//! mode covers the same flow).
//!
//! - `initialize`, then `session/new` or `session/load`. Updates replayed during a
//!   load are dropped, because the timeline is already stored.
//! - `session/prompt` is answered when the turn ends, so it runs in the
//!   background. `session/cancel` interrupts it.
//! - `session/update` chunks become assistant, reasoning, tool-call and plan items.
//! - `session/request_permission` becomes a permission request whose actions are
//!   the agent's options.
//!
//! The client doesn't offer file system or terminal methods. Agents use their own
//! tools.
//!
//! **Turn watch** ([`lucina_agents::turn_watch`]): a turn the agent never ends is ended
//! as a quota stop when the agent's own error log reports a usage limit for the session
//! (OpenCode on Zen's free tier: `Free usage limit reached`, then silence), or when it
//! sends nothing for its stall time while no tool call and no permission request is open.
//! The manager then hands the run on like any quota stop (`manager/handoff.rs`).
//! `LUCINA_AGENTD_ACP_STALL_SECS` overrides every agent's stall time (0 = off).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use lucina_agents::permission::{ActionVariant, Behavior, PermissionAction, PermissionKind, PermissionRequest};
use lucina_agents::timeline::{SearchDetail, TodoEntry, TodoStatus, ToolCall, ToolCallDetail, ToolCallStatus};
use lucina_agents::turn_watch::{self, Verdict, WatchFacts};
use lucina_agents::types::{AgentMode, Capabilities, PersistenceHandle, RuntimeInfo};
use lucina_agents::{PermissionResponse, StreamEvent, TimelineItem};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc::UnboundedReceiver;

use super::acp_subagents::{is_task_call, Children, Ended};
use crate::jsonrpc::{Incoming, Rpc, RpcError};
use crate::process::{resolve_program, Proc, Registry, Spec};
use crate::provider::{EventSink, ModelInfo, Provider, ProviderError, ProviderInfo, Session, SessionConfig};

const RPC_TIMEOUT: Duration = Duration::from_secs(120);
const TURN_TIMEOUT: Duration = Duration::from_secs(24 * 3600);

/// One ACP agent: how to launch it and what to call it.
#[derive(Debug, Clone)]
pub struct AcpDef {
    pub id: &'static str,
    pub label: &'static str,
    pub bin: &'static str,
    /// Arguments; `{model}` is replaced (and the pair dropped when there's no model).
    pub args: &'static [&'static str],
    /// `clientCapabilities._meta` sent at `initialize`.
    pub client_meta: Option<&'static str>,
    /// Seconds without output (nothing open) after which a turn counts as stalled.
    pub stall_secs: Option<u64>,
    /// The agent's own log, where it reports errors it never sends over ACP (`~` = home).
    pub error_log: Option<&'static str>,
}

/// The ACP agents agentd knows (Paseo's built-in and plugin providers).
pub fn builtin() -> Vec<AcpDef> {
    vec![
        // The parameterized picker exposes Cursor's model parameters (context,
        // reasoning effort, fast) as separate config options (Paseo
        // CURSOR_CLIENT_CAPABILITY_META); without it, `grok-4.7` resolves to fast=true.
        AcpDef { id: "cursor", label: "Cursor", bin: "cursor-agent", args: &["--model", "{model}", "acp"], client_meta: Some(r#"{"parameterizedModelPicker":true}"#), stall_secs: Some(600), error_log: None },
        // OpenCode logs Zen's "Free usage limit reached" and then never answers the prompt.
        AcpDef { id: "opencode", label: "OpenCode", bin: "opencode", args: &["acp"], client_meta: None, stall_secs: Some(300), error_log: Some("~/.local/share/opencode/log/opencode.log") },
        AcpDef { id: "copilot", label: "Copilot", bin: "copilot", args: &["--acp"], client_meta: None, stall_secs: Some(600), error_log: None },
    ]
}

/// A custom agent from `~/.config/lucina-agentd/acp.toml`:
/// `[agents.kimi] label = "Kimi", command = ["kimi", "--acp"]`.
#[derive(Debug, Clone, Deserialize)]
pub struct CustomAcp {
    #[serde(default)]
    pub label: Option<String>,
    pub command: Vec<String>,
    /// The turn watch's stall time (none by default: a local model may queue for its GPU).
    #[serde(default)]
    pub stall_secs: Option<u64>,
    /// The agent's own error log, for usage limits it never reports over ACP.
    #[serde(default)]
    pub error_log: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct CustomFile {
    #[serde(default)]
    agents: std::collections::BTreeMap<String, CustomAcp>,
}

/// The custom ACP agents whose program exists, as providers.
pub fn custom(registry: &Registry) -> Vec<Arc<dyn Provider>> {
    let path = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/lucina-agentd/acp.toml");
    let Ok(text) = std::fs::read_to_string(&path) else { return Vec::new() };
    let file: CustomFile = match toml::from_str(&text) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "unreadable acp.toml");
            return Vec::new();
        }
    };
    file.agents
        .into_iter()
        .filter_map(|(id, agent)| {
            let (program, args) = agent.command.split_first()?;
            let bin = resolve_program(program)?;
            Some(Arc::new(AcpProvider {
                id: id.clone(),
                label: agent.label.unwrap_or_else(|| id.clone()),
                bin,
                args: args.to_vec(),
                default_model: None,
                client_meta: None,
                registry: Some(registry.clone()),
                stall: stall_override().unwrap_or(agent.stall_secs.map(Duration::from_secs)).filter(|d| !d.is_zero()),
                error_log: agent.error_log.as_deref().map(expand_home),
            }) as Arc<dyn Provider>)
        })
        .collect()
}

pub struct AcpProvider {
    pub id: String,
    pub label: String,
    pub bin: PathBuf,
    pub args: Vec<String>,
    pub default_model: Option<String>,
    pub client_meta: Option<Value>,
    pub registry: Option<Registry>,
    /// The turn watch's stall time; `None` = no stall rule.
    pub stall: Option<Duration>,
    /// The agent's own error log, tailed during a turn for usage limits.
    pub error_log: Option<PathBuf>,
}

/// `LUCINA_AGENTD_ACP_STALL_SECS`: every agent's stall time (`Some(None)` when 0 = off).
fn stall_override() -> Option<Option<Duration>> {
    let secs: u64 = std::env::var("LUCINA_AGENTD_ACP_STALL_SECS").ok()?.trim().parse().ok()?;
    Some((secs > 0).then(|| Duration::from_secs(secs)))
}

fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(p),
    }
}

/// Owner rule: Fast is never turned on. Any ACP agent that offers a `fast` config
/// option gets it set to "false" for every session.
const NEVER_FAST: (&str, &str) = ("fast", "false");

impl AcpProvider {
    pub fn new(def: AcpDef, bin: PathBuf, default_model: Option<String>, registry: Option<Registry>) -> Self {
        AcpProvider {
            id: def.id.into(),
            label: def.label.into(),
            bin,
            args: def.args.iter().map(|a| a.to_string()).collect(),
            default_model,
            client_meta: def.client_meta.and_then(|m| serde_json::from_str(m).ok()),
            registry,
            stall: stall_override().unwrap_or(def.stall_secs.map(Duration::from_secs)).filter(|d| !d.is_zero()),
            error_log: def.error_log.map(expand_home),
        }
    }

    /// True when the model is passed on the command line (`--model {model}`), so it is
    /// already applied at launch and must not be set again over ACP.
    fn model_in_args(&self) -> bool {
        self.args.iter().any(|a| a == "{model}")
    }

    /// The model to set over ACP after `session/new`: the requested one, unless there is
    /// none or it was already passed on the command line.
    fn model_to_set_over_acp<'a>(&self, model: Option<&'a str>) -> Option<&'a str> {
        model.filter(|_| !self.model_in_args())
    }

    /// The launch arguments, with `--x {model}` dropped when there's no model.
    fn launch_args(&self, model: Option<&str>) -> Vec<String> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.args.len() {
            let a = &self.args[i];
            if self.args.get(i + 1).map(String::as_str) == Some("{model}") {
                if let Some(m) = model {
                    out.push(a.clone());
                    out.push(m.to_string());
                }
                i += 2;
                continue;
            }
            out.push(a.clone());
            i += 1;
        }
        out
    }

    async fn start(&self, config: SessionConfig, events: EventSink, resume: Option<&str>) -> Result<Arc<dyn Session>, ProviderError> {
        let model = config.model.clone().or_else(|| self.default_model.clone());
        let spec = Spec {
            program: self.bin.clone(),
            args: self.launch_args(model.as_deref()),
            cwd: config.cwd.clone(),
            env: config.env.clone(),
            env_remove: Vec::new(),
        };
        let (proc_, stdin, stdout) = Proc::spawn(&spec, self.registry.as_ref()).map_err(|e| ProviderError::new(format!("cannot start {}: {e}", self.id)))?;
        let proc_ = Arc::new(tokio::sync::Mutex::new(proc_));
        let (rpc, incoming) = Rpc::start(stdin, stdout);
        let id = self.id.clone();
        let fail = |what: &str, e: RpcError| ProviderError::new(format!("{id} {what}: {e}"));
        let mut caps = json!({"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false});
        if let Some(meta) = &self.client_meta {
            caps["_meta"] = meta.clone();
        }
        let init = rpc
            .request(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": caps,
                    "clientInfo": {"name": "lucina-agentd", "version": env!("CARGO_PKG_VERSION")},
                }),
                RPC_TIMEOUT,
            )
            .await;
        let init = match init {
            Ok(v) => v,
            Err(e) => {
                let tail = proc_.lock().await.stderr_tail();
                return Err(ProviderError::new(format!("{id} initialize: {e}\n{tail}")));
            }
        };
        let can_load = init["agentCapabilities"]["loadSession"] == true;
        let session = Arc::new(AcpSession {
            provider: self.id.clone(),
            rpc: rpc.clone(),
            proc_: proc_.clone(),
            events: events.clone(),
            stall: self.stall,
            error_log: self.error_log.clone(),
            state: Arc::new(Mutex::new(State {
                session_id: String::new(),
                model: model.clone(),
                mode: config.mode_id.clone(),
                turn_id: None,
                reply: String::new(),
                loading: resume.is_some(),
                permissions: HashMap::new(),
                tools: HashMap::new(),
                subs: Children::default(),
                last_activity: Instant::now(),
            })),
        });
        session.clone().spawn_reader(incoming);
        let (session_id, modes, config_options, models) = match resume {
            Some(sid) if can_load => {
                let resp = rpc
                    .request("session/load", json!({"sessionId": sid, "cwd": config.cwd, "mcpServers": crate::mcp::acp_servers(config.agent_id.is_some())}), RPC_TIMEOUT)
                    .await
                    .map_err(|e| fail("session/load", e))?;
                (sid.to_string(), resp["modes"].clone(), resp["configOptions"].clone(), resp["models"].clone())
            }
            _ => {
                let resp = rpc.request("session/new", json!({"cwd": config.cwd, "mcpServers": crate::mcp::acp_servers(config.agent_id.is_some())}), RPC_TIMEOUT).await;
                let resp = match resp {
                    Ok(v) => v,
                    Err(e) => {
                        let tail = proc_.lock().await.stderr_tail();
                        return Err(ProviderError::new(format!("{id} session/new: {e}\n{tail}")));
                    }
                };
                let sid = resp["sessionId"].as_str().unwrap_or_default().to_string();
                if let Some(m) = resp["models"]["currentModelId"].as_str() {
                    session.lock().model = Some(m.to_string());
                }
                (sid, resp["modes"].clone(), resp["configOptions"].clone(), resp["models"].clone())
            }
        };
        if session_id.is_empty() {
            return Err(ProviderError::new(format!("{id} returned no session id")));
        }
        {
            let mut st = session.lock();
            st.session_id = session_id.clone();
            st.loading = false;
            if st.mode.is_none() {
                st.mode = modes["currentModeId"].as_str().map(str::to_string);
            }
        }
        let mut options = config_options.as_array().cloned().unwrap_or_default();
        // A requested model that isn't already on the launch command line must be set
        // through ACP. Never fall back silently to the agent's own default model.
        if let Some(want) = self.model_to_set_over_acp(model.as_deref()) {
            let no_model = |why: String| ProviderError::new(format!("{id}: cannot use requested model \"{want}\": {why}"));
            match plan_model(&options, &models, want).map_err(no_model)? {
                ModelPlan::Keep(current) => session.lock().model = Some(current),
                ModelPlan::Config(value) => {
                    let resp = rpc
                        .request("session/set_config_option", json!({"sessionId": session_id, "configId": "model", "value": value}), RPC_TIMEOUT)
                        .await
                        .map_err(|e| no_model(e.to_string()))?;
                    // The reply carries the full, updated option list; otherwise patch our copy.
                    match resp["configOptions"].as_array() {
                        Some(updated) => options = updated.clone(),
                        None => {
                            for o in options.iter_mut().filter(|o| o["id"] == "model") {
                                o["currentValue"] = json!(value);
                            }
                        }
                    }
                    session.lock().model = Some(value);
                }
                ModelPlan::SetModel(model_id) => {
                    rpc.request("session/set_model", json!({"sessionId": session_id, "modelId": model_id}), RPC_TIMEOUT)
                        .await
                        .map_err(|e| no_model(e.to_string()))?;
                    session.lock().model = Some(model_id);
                }
            }
        }
        let has = |id: &str| options.iter().any(|o| o["id"] == id);
        if has(NEVER_FAST.0) {
            rpc.request("session/set_config_option", json!({"sessionId": session_id, "configId": NEVER_FAST.0, "value": NEVER_FAST.1}), RPC_TIMEOUT)
                .await
                .map_err(|e| ProviderError::new(format!("{id}: could not turn Fast off ({e}); refusing to run with Fast")))?;
        }
        // Report the model with its parameters (e.g. "grok-4.7 reasoning_effort=high fast=false").
        let model_desc = options.iter().find(|o| o["id"] == "model").and_then(|o| o["currentValue"].as_str()).map(|m| {
            let params: Vec<String> = options
                .iter()
                .filter(|o| o["category"] == "model_config" || o["category"] == "thought_level")
                .map(|o| {
                    let v = if o["id"] == NEVER_FAST.0 { NEVER_FAST.1.to_string() } else { o["currentValue"].as_str().unwrap_or("").to_string() };
                    format!("{}={v}", o["id"].as_str().unwrap_or(""))
                })
                .collect();
            if params.is_empty() { m.to_string() } else { format!("{m} {}", params.join(" ")) }
        });
        if let Some(desc) = model_desc {
            session.lock().model = Some(desc);
        }
        if let Some(mode) = config.mode_id.as_deref() {
            let _ = rpc.request("session/set_mode", json!({"sessionId": session_id, "modeId": mode}), RPC_TIMEOUT).await;
        }
        let _ = events.send(StreamEvent::ThreadStarted { session_id, provider: self.id.clone() });
        Ok(session)
    }
}

#[async_trait]
impl Provider for AcpProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            label: self.label.clone(),
            default_mode_id: None,
            modes: Vec::<AgentMode>::new(),
            models: self
                .default_model
                .iter()
                .map(|m| ModelInfo { id: m.clone(), label: m.clone(), description: None, is_default: true, is_selectable: true, default_thinking_option_id: None })
                .collect(),
            capabilities: Capabilities {
                supports_streaming: true,
                supports_session_persistence: true,
                supports_session_listing: Some(false),
                supports_dynamic_modes: true,
                supports_mcp_servers: true,
                supports_reasoning_stream: true,
                supports_tool_invocations: true,
                supports_rewind_conversation: false,
                supports_rewind_files: false,
                supports_rewind_both: false,
                extra: Default::default(),
            },
            echoes_user_message: false,
        }
    }

    async fn create_session(&self, config: SessionConfig, events: EventSink) -> Result<Arc<dyn Session>, ProviderError> {
        self.start(config, events, None).await
    }

    async fn resume_session(&self, handle: &PersistenceHandle, config: SessionConfig, events: EventSink) -> Result<Arc<dyn Session>, ProviderError> {
        self.start(config, events, Some(&handle.session_id)).await
    }
}

/// How to put a requested model into effect on a fresh ACP session.
#[derive(Debug, PartialEq)]
enum ModelPlan {
    /// The session already runs it (carries the model id).
    Keep(String),
    /// `session/set_config_option` with `configId: "model"` and this value.
    Config(String),
    /// Legacy `session/set_model` with this model id.
    SetModel(String),
}

/// The values a select config option offers; options may be flat or grouped.
fn option_values(opt: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for o in opt["options"].as_array().into_iter().flatten() {
        if let Some(v) = o["value"].as_str() {
            out.push(v.to_string());
        }
        for g in o["options"].as_array().into_iter().flatten() {
            if let Some(v) = g["value"].as_str() {
                out.push(v.to_string());
            }
        }
    }
    out
}

/// Match `want` against the offered ids: exactly, or as the part after the provider
/// prefix (`deepseek-v4.1-flash-free` matches `opencode/deepseek-v4.1-flash-free`) when
/// that is unambiguous.
fn match_model(offered: &[String], want: &str) -> Result<String, String> {
    if let Some(v) = offered.iter().find(|v| v.as_str() == want) {
        return Ok(v.clone());
    }
    let suffix = format!("/{want}");
    let hits: Vec<&String> = offered.iter().filter(|v| v.ends_with(&suffix)).collect();
    match hits.as_slice() {
        [one] => Ok((*one).clone()),
        [] => {
            let shown: Vec<&str> = offered.iter().take(8).map(String::as_str).collect();
            let more = if offered.len() > shown.len() { ", ..." } else { "" };
            Err(format!("the agent does not offer it (offered: {}{more})", shown.join(", ")))
        }
        many => Err(format!("ambiguous, matches {}", many.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(", "))),
    }
}

/// Decide how to apply `want` from a session's `configOptions` and legacy `models` block.
fn plan_model(options: &[Value], models: &Value, want: &str) -> Result<ModelPlan, String> {
    if let Some(opt) = options.iter().find(|o| o["id"] == "model") {
        let offered = option_values(opt);
        // An option that lists no values: pass the request through and let the agent judge.
        let value = if offered.is_empty() { want.to_string() } else { match_model(&offered, want)? };
        return Ok(if opt["currentValue"].as_str() == Some(value.as_str()) { ModelPlan::Keep(value) } else { ModelPlan::Config(value) });
    }
    if let Some(list) = models["availableModels"].as_array() {
        let offered: Vec<String> = list.iter().filter_map(|m| m["modelId"].as_str().map(str::to_string)).collect();
        let value = match_model(&offered, want)?;
        return Ok(if models["currentModelId"].as_str() == Some(value.as_str()) { ModelPlan::Keep(value) } else { ModelPlan::SetModel(value) });
    }
    Err("the agent exposes no model selection".into())
}

struct OpenTool {
    name: String,
    detail: ToolCallDetail,
}

struct State {
    session_id: String,
    model: Option<String>,
    mode: Option<String>,
    turn_id: Option<String>,
    /// The turn's assistant text so far (to spot quota replies).
    reply: String,
    loading: bool,
    /// Our permission id -> (JSON-RPC id, the agent's options as (optionId, kind)).
    permissions: HashMap<String, (Value, Vec<(String, String)>)>,
    tools: HashMap<String, OpenTool>,
    /// Child sessions (OpenCode's `task` tool), each a provider subagent.
    subs: Children,
    /// When the agent last sent anything (an update or a request), for the turn watch.
    last_activity: Instant,
}

pub struct AcpSession {
    provider: String,
    rpc: Arc<Rpc>,
    proc_: Arc<tokio::sync::Mutex<Proc>>,
    events: EventSink,
    state: Arc<Mutex<State>>,
    stall: Option<Duration>,
    error_log: Option<PathBuf>,
}

/// The new lines of an agent's error log since a turn started.
struct LogTail {
    path: PathBuf,
    offset: u64,
    partial: String,
}

/// At most this much of the log is read per look.
const LOG_READ_MAX: u64 = 4 << 20;

impl LogTail {
    fn start(path: &std::path::Path) -> LogTail {
        let offset = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        LogTail { path: path.to_path_buf(), offset, partial: String::new() }
    }

    /// A usage limit for `session_id` among the lines written since the last look.
    fn scan(&mut self, session_id: &str) -> Option<String> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&self.path).ok()?;
        let len = f.metadata().ok()?.len();
        if len < self.offset {
            // Rotated or truncated: start over.
            self.offset = 0;
            self.partial.clear();
        }
        f.seek(SeekFrom::Start(self.offset)).ok()?;
        let mut buf = Vec::new();
        f.take(LOG_READ_MAX).read_to_end(&mut buf).ok()?;
        self.offset += buf.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&buf));
        let mut hit = None;
        let complete = match self.partial.rfind('\n') {
            Some(i) => {
                let rest = self.partial.split_off(i + 1);
                std::mem::replace(&mut self.partial, rest)
            }
            None => return None,
        };
        for line in complete.lines() {
            if hit.is_none() {
                hit = turn_watch::limit_in_log_line(line, session_id);
            }
        }
        hit
    }
}

/// Watch a running turn until it is dead: `(verdict, error text)`. Never returns while
/// the turn is alive or once it is no longer `turn_id` (the prompt's answer decides then).
async fn watch_turn(
    state: Arc<Mutex<State>>,
    provider: String,
    turn_id: String,
    session_id: String,
    stall: Option<Duration>,
    mut log: Option<LogTail>,
) -> (Verdict, String) {
    if stall.is_none() && log.is_none() {
        return std::future::pending().await;
    }
    let tick = stall.map(|s| (s / 4).clamp(Duration::from_millis(50), Duration::from_secs(2))).unwrap_or(Duration::from_secs(2));
    let mut limit: Option<String> = None;
    loop {
        tokio::time::sleep(tick).await;
        if limit.is_none() {
            limit = log.as_mut().and_then(|t| t.scan(&session_id));
        }
        let facts = {
            let st = state.lock().unwrap_or_else(|p| p.into_inner());
            if st.turn_id.as_deref() != Some(turn_id.as_str()) {
                None
            } else {
                Some(WatchFacts {
                idle_ms: st.last_activity.elapsed().as_millis().min(u64::MAX as u128) as u64,
                stall_ms: stall.map(|d| d.as_millis().min(u64::MAX as u128) as u64),
                open_tools: st.tools.len().min(u32::MAX as usize) as u32,
                open_permissions: st.permissions.len().min(u32::MAX as usize) as u32,
                log_limit: limit.is_some(),
                })
            }
        };
        let Some(facts) = facts else {
            // The turn ended or was replaced: its prompt's answer decides.
            return std::future::pending().await;
        };
        match turn_watch::verdict(&facts) {
            Verdict::Wait => {}
            Verdict::Limit => return (Verdict::Limit, limit.unwrap_or_default()),
            Verdict::Stalled => {
                let secs = facts.idle_ms / 1000;
                return (
                    Verdict::Stalled,
                    format!("{provider} stalled: no output for {secs} s with no tool call or permission open; ended as a usage-limit stop"),
                );
            }
        }
    }
}

pub(super) fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

pub(super) fn content_text(content: &Value) -> String {
    match content {
        Value::Array(items) => items.iter().map(content_text).collect::<Vec<_>>().join(""),
        Value::Object(_) => match content["type"].as_str() {
            Some("text") => s(&content["text"]).unwrap_or_default(),
            Some("content") => content_text(&content["content"]),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// An ACP tool call (`kind`, `title`, `rawInput`, `locations`, `content`) as a detail.
pub fn acp_detail(update: &Value, previous: Option<&ToolCallDetail>) -> ToolCallDetail {
    let raw = &update["rawInput"];
    let title = s(&update["title"]).unwrap_or_default();
    let path = update["locations"][0]["path"].as_str().map(str::to_string);
    let output = {
        let t = content_text(&update["content"]);
        (!t.is_empty()).then_some(t).or_else(|| s(&update["rawOutput"]["output"]))
    };
    let diff = update["content"].as_array().and_then(|c| c.iter().find(|i| i["type"] == "diff").cloned());
    // A `task` call keeps its `sub_agent` shape through later updates, whatever `kind` they carry.
    let kind = if matches!(previous, Some(ToolCallDetail::SubAgent { .. })) { None } else { update["kind"].as_str() };
    if is_task_call(update) && !matches!(previous, Some(ToolCallDetail::SubAgent { .. })) {
        return ToolCallDetail::SubAgent {
            sub_agent_type: s(&raw["subagent_type"]),
            description: s(&raw["description"]),
            child_session_id: s(&update["rawOutput"]["metadata"]["sessionId"]),
            log: output.unwrap_or_default(),
            actions: Some(Vec::new()),
        };
    }
    match (kind, previous) {
        (None, Some(ToolCallDetail::SubAgent { sub_agent_type, description, child_session_id, log, actions })) => ToolCallDetail::SubAgent {
            sub_agent_type: sub_agent_type.clone(),
            description: description.clone(),
            child_session_id: s(&update["rawOutput"]["metadata"]["sessionId"]).or_else(|| child_session_id.clone()),
            log: output.unwrap_or_else(|| log.clone()),
            actions: actions.clone(),
        },
        (None, Some(prev)) => match (prev, output) {
            (ToolCallDetail::Shell { command, cwd, exit_code, .. }, Some(out)) => {
                ToolCallDetail::Shell { command: command.clone(), cwd: cwd.clone(), output: Some(out), exit_code: exit_code.clone() }
            }
            _ => prev.clone(),
        },
        (Some("execute"), _) => ToolCallDetail::Shell {
            command: s(&raw["command"]).or_else(|| raw["command"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "))).unwrap_or(title),
            cwd: s(&raw["cwd"]),
            output,
            exit_code: None,
        },
        (Some("read"), _) => ToolCallDetail::Read { file_path: path.or_else(|| s(&raw["path"])).unwrap_or(title), content: None, offset: None, limit: None },
        (Some("edit") | Some("delete") | Some("move"), _) => match diff {
            Some(d) => ToolCallDetail::Edit {
                file_path: s(&d["path"]).or(path).unwrap_or(title),
                old_string: s(&d["oldText"]),
                new_string: s(&d["newText"]),
                unified_diff: None,
            },
            None => ToolCallDetail::Edit { file_path: path.unwrap_or(title), old_string: None, new_string: None, unified_diff: None },
        },
        (Some("search"), _) => ToolCallDetail::Search(SearchDetail {
            query: s(&raw["pattern"]).or_else(|| s(&raw["query"])).unwrap_or(title),
            tool_name: None,
            content: output,
            file_paths: None,
            web_results: None,
            annotations: None,
            num_files: None,
            num_matches: None,
            duration_ms: None,
            duration_seconds: None,
            truncated: None,
            mode: None,
        }),
        (Some("fetch"), _) => ToolCallDetail::Fetch { url: s(&raw["url"]).unwrap_or(title), prompt: None, result: output, code: None, code_text: None, bytes: None, duration_ms: None },
        (Some("think"), _) => ToolCallDetail::PlainText { label: Some(title), text: output, icon: None },
        _ => ToolCallDetail::Unknown { input: raw.clone(), output: update["rawOutput"].clone() },
    }
}

/// Cursor and Copilot end a turn normally when the account is out of quota, with
/// the notice as the whole reply ("Upgrade your plan to continue", "Error: You have
/// exceeded your monthly quota ..."). Such a turn did no work: it's a quota failure.
pub fn is_quota_reply(reply: &str) -> bool {
    let r = reply.trim().to_lowercase();
    let r = r.strip_prefix("error:").map(str::trim).unwrap_or(&r);
    r.len() < 400
        && (r.starts_with("upgrade your plan to continue")
            || r.starts_with("you have exceeded your monthly quota")
            || r.starts_with("you've hit your usage limit")
            || r.starts_with("you have hit your usage limit")
            || r.starts_with("usage limit reached"))
}

pub(super) fn tool_status(v: &Value) -> Option<ToolCallStatus> {
    Some(match v.as_str()? {
        "pending" | "in_progress" => ToolCallStatus::Running,
        "completed" => ToolCallStatus::Completed,
        "failed" => ToolCallStatus::Failed,
        _ => return None,
    })
}

impl AcpSession {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn emit(&self, ev: StreamEvent) {
        let _ = self.events.send(ev);
    }

    fn timeline(&self, item: TimelineItem) {
        let turn = self.lock().turn_id.clone();
        self.emit(StreamEvent::Timeline { provider: self.provider.clone(), item, turn_id: turn });
    }

    fn spawn_reader(self: Arc<Self>, mut incoming: UnboundedReceiver<Incoming>) {
        tokio::spawn(async move {
            while let Some(msg) = incoming.recv().await {
                match msg {
                    Incoming::Notification { method, params } if method == "session/update" => {
                        let (loading, own) = {
                            let mut st = self.lock();
                            st.last_activity = Instant::now();
                            (st.loading, st.session_id.clone())
                        };
                        let from = params["sessionId"].as_str().unwrap_or_default();
                        if loading {
                        } else if !from.is_empty() && !own.is_empty() && from != own {
                            // A child session's update: not the agent's conversation.
                            let events = {
                                let mut st = self.lock();
                                let st = &mut *st;
                                let running: Vec<String> = st.tools.iter().filter(|(_, t)| matches!(t.detail, ToolCallDetail::SubAgent { .. })).map(|(c, _)| c.clone()).collect();
                                st.subs.child_update(&self.provider, from, &params["update"], &running)
                            };
                            for e in events {
                                self.emit(e);
                            }
                        } else {
                            self.on_update(&params["update"]);
                        }
                    }
                    Incoming::Notification { .. } => {}
                    Incoming::Request { id, method, params } => {
                        self.lock().last_activity = Instant::now();
                        self.on_request(id, &method, &params).await
                    }
                    Incoming::Closed => {
                        let lost = self.lock().subs.turn_ended(&self.provider, Ended::Failed);
                        for e in lost {
                            self.emit(e);
                        }
                        let turn = self.lock().turn_id.take();
                        if let Some(turn) = turn {
                            let tail = self.proc_.lock().await.stderr_tail();
                            self.emit(StreamEvent::TurnFailed {
                                provider: self.provider.clone(),
                                turn_id: Some(turn),
                                error: format!("{} exited", self.provider),
                                code: None,
                                diagnostic: (!tail.is_empty()).then_some(tail),
                            });
                        }
                        return;
                    }
                }
            }
        });
    }

    fn on_update(&self, u: &Value) {
        match u["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => {
                let text = content_text(&u["content"]);
                self.lock().reply.push_str(&text);
                self.timeline(TimelineItem::AssistantMessage { text, message_id: None });
            }
            Some("agent_thought_chunk") => self.timeline(TimelineItem::Reasoning { text: content_text(&u["content"]) }),
            Some("tool_call") | Some("tool_call_update") => {
                let call_id = s(&u["toolCallId"]).unwrap_or_default();
                let (name, detail) = {
                    let st = self.lock();
                    let prev = st.tools.get(&call_id);
                    let name = s(&u["title"]).or_else(|| prev.map(|p| p.name.clone())).unwrap_or_else(|| "tool".into());
                    let detail = acp_detail(u, prev.map(|p| &p.detail));
                    (name, detail)
                };
                let status = tool_status(&u["status"]).unwrap_or(ToolCallStatus::Running);
                let error = if status == ToolCallStatus::Failed {
                    let t = content_text(&u["content"]);
                    json!(if t.is_empty() { "failed".to_string() } else { t })
                } else {
                    Value::Null
                };
                {
                    let mut st = self.lock();
                    if matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed) {
                        st.tools.remove(&call_id);
                    } else {
                        st.tools.insert(call_id.clone(), OpenTool { name: name.clone(), detail: detail.clone() });
                    }
                }
                let task = matches!(detail, ToolCallDetail::SubAgent { .. }) && is_task_call(u);
                let linked = if task { self.lock().subs.note_call(&self.provider, &call_id, u) } else { Vec::new() };
                self.timeline(TimelineItem::ToolCall(Box::new(ToolCall { agent_message: None, call_id, name, detail, metadata: None, status, error })));
                for e in linked {
                    self.emit(e);
                }
            }
            Some("plan") => {
                let items = u["entries"]
                    .as_array()
                    .map(|e| {
                        e.iter()
                            .map(|x| {
                                let status = match x["status"].as_str() {
                                    Some("completed") => TodoStatus::Completed,
                                    Some("in_progress") => TodoStatus::InProgress,
                                    _ => TodoStatus::Pending,
                                };
                                TodoEntry { text: s(&x["content"]).unwrap_or_default(), completed: status == TodoStatus::Completed, id: None, status: Some(status), active_form: None }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.timeline(TimelineItem::Todo { items });
            }
            Some("current_mode_update") => {
                self.lock().mode = s(&u["currentModeId"]);
            }
            _ => {}
        }
    }

    async fn on_request(&self, id: Value, method: &str, p: &Value) {
        if method != "session/request_permission" {
            let _ = self.rpc.respond_error(id, -32601, &format!("lucina-agentd does not provide {method}")).await;
            return;
        }
        let tool = &p["toolCall"];
        let options: Vec<(String, String, String)> = p["options"]
            .as_array()
            .map(|o| o.iter().map(|x| (s(&x["optionId"]).unwrap_or_default(), s(&x["name"]).unwrap_or_default(), s(&x["kind"]).unwrap_or_default())).collect())
            .unwrap_or_default();
        let request_id = format!("{}-perm-{}", self.provider, id);
        let prev = self.lock().tools.get(s(&tool["toolCallId"]).as_deref().unwrap_or("")).map(|t| t.detail.clone());
        let request = PermissionRequest {
            id: request_id.clone(),
            provider: self.provider.clone(),
            name: s(&tool["title"]).unwrap_or_else(|| "tool".into()),
            kind: if tool["kind"] == "switch_mode" { PermissionKind::Mode } else { PermissionKind::Tool },
            title: s(&tool["title"]),
            description: None,
            input: tool["rawInput"].as_object().cloned(),
            detail: Some(acp_detail(tool, prev.as_ref())),
            suggestions: None,
            actions: Some(
                options
                    .iter()
                    .map(|(oid, name, kind)| {
                        let allow = kind.starts_with("allow");
                        PermissionAction {
                            id: oid.clone(),
                            label: name.clone(),
                            behavior: if allow { Behavior::Allow } else { Behavior::Deny },
                            variant: Some(if allow { ActionVariant::Primary } else { ActionVariant::Danger }),
                            intent: None,
                        }
                    })
                    .collect(),
            ),
            metadata: None,
        };
        self.lock().permissions.insert(request_id, (id, options.into_iter().map(|(o, _, k)| (o, k)).collect()));
        self.emit(StreamEvent::PermissionRequested { provider: self.provider.clone(), request: Box::new(request) });
    }
}

#[async_trait]
impl Session for AcpSession {
    fn session_id(&self) -> String {
        self.lock().session_id.clone()
    }

    async fn start_turn(&self, prompt: &str, _client_message_id: Option<String>) -> Result<String, ProviderError> {
        let turn = uuid::Uuid::new_v4().to_string();
        let session_id = {
            let mut st = self.lock();
            if st.turn_id.is_some() {
                return Err(ProviderError::new(format!("{} already has a running turn", self.provider)));
            }
            st.turn_id = Some(turn.clone());
            st.reply.clear();
            st.last_activity = Instant::now();
            st.session_id.clone()
        };
        let log = self.error_log.as_deref().map(LogTail::start);
        let stall = self.stall;
        self.emit(StreamEvent::TurnStarted { provider: self.provider.clone(), turn_id: Some(turn.clone()) });
        let rpc = self.rpc.clone();
        let events = self.events.clone();
        let state = self.state.clone();
        let provider = self.provider.clone();
        let proc_ = self.proc_.clone();
        let prompt = prompt.to_string();
        let turn_id = turn.clone();
        tokio::spawn(async move {
            let watch = watch_turn(state.clone(), provider.clone(), turn_id.clone(), session_id.clone(), stall, log);
            let request = rpc.request("session/prompt", json!({"sessionId": session_id, "prompt": [{"type": "text", "text": prompt}]}), TURN_TIMEOUT);
            let result = tokio::select! {
                r = request => r,
                (verdict, why) = watch => {
                    // The agent never ends this turn: stop it and report a quota stop.
                    let ours = {
                        let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
                        let ours = st.turn_id.as_deref() == Some(turn_id.as_str());
                        if ours {
                            st.turn_id = None;
                            st.reply.clear();
                        }
                        ours
                    };
                    if !ours {
                        return;
                    }
                    let _ = rpc.notify("session/cancel", json!({"sessionId": session_id})).await;
                    let children = state.lock().unwrap_or_else(|p| p.into_inner()).subs.turn_ended(&provider, Ended::Failed);
                    for e in children {
                        let _ = events.send(e);
                    }
                    let what = if verdict == Verdict::Limit { "usage limit in the agent's log" } else { "stalled turn" };
                    let _ = events.send(StreamEvent::TurnFailed {
                        provider: provider.clone(),
                        turn_id: Some(turn_id),
                        error: why,
                        code: Some("quota".into()),
                        diagnostic: Some(format!("lucina-agentd turn watch: {what}")),
                    });
                    return;
                }
            };
            let (ours, reply) = {
                let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
                if st.turn_id.as_deref() == Some(turn_id.as_str()) {
                    st.turn_id = None;
                    (true, std::mem::take(&mut st.reply))
                } else {
                    (false, String::new())
                }
            };
            if !ours {
                return;
            }
            let ended = match &result {
                Ok(v) if v["stopReason"] == "cancelled" => Ended::Canceled,
                Ok(v) if v["stopReason"] == "refusal" => Ended::Failed,
                Ok(_) if is_quota_reply(&reply) => Ended::Failed,
                Ok(_) => Ended::Completed,
                Err(_) => Ended::Failed,
            };
            let children = state.lock().unwrap_or_else(|p| p.into_inner()).subs.turn_ended(&provider, ended);
            for e in children {
                let _ = events.send(e);
            }
            let ev = match result {
                Ok(_) if is_quota_reply(&reply) => StreamEvent::TurnFailed {
                    provider: provider.clone(),
                    turn_id: Some(turn_id),
                    error: reply.trim().to_string(),
                    code: Some("quota".into()),
                    diagnostic: None,
                },
                Ok(v) if v["stopReason"] == "cancelled" => StreamEvent::TurnCanceled { provider: provider.clone(), turn_id: Some(turn_id), reason: "Interrupted".into() },
                Ok(v) if v["stopReason"] == "refusal" => StreamEvent::TurnFailed { provider: provider.clone(), turn_id: Some(turn_id), error: "The agent refused the request".into(), code: Some("refusal".into()), diagnostic: None },
                Ok(_) => StreamEvent::TurnCompleted { provider: provider.clone(), turn_id: Some(turn_id), usage: None },
                Err(e) => {
                    let tail = proc_.lock().await.stderr_tail();
                    StreamEvent::TurnFailed { provider: provider.clone(), turn_id: Some(turn_id), error: e.to_string(), code: None, diagnostic: (!tail.is_empty()).then_some(tail) }
                }
            };
            let _ = events.send(ev);
        });
        Ok(turn)
    }

    async fn steer(&self, _prompt: &str, _expected_turn_id: &str) -> Result<bool, ProviderError> {
        // ACP has no steering: the manager interrupts and starts a new turn.
        Ok(false)
    }

    async fn interrupt(&self) -> Result<(), ProviderError> {
        let (session_id, running) = {
            let st = self.lock();
            (st.session_id.clone(), st.turn_id.is_some())
        };
        if !running {
            return Ok(());
        }
        self.rpc.notify("session/cancel", json!({"sessionId": session_id})).await.map_err(|e| ProviderError::new(e.to_string()))
    }

    async fn respond_permission(&self, request_id: &str, response: PermissionResponse) -> Result<(), ProviderError> {
        let Some((rpc_id, options)) = self.lock().permissions.remove(request_id) else { return Ok(()) };
        let pick = |selected: &Option<String>, allow: bool| {
            selected
                .clone()
                .filter(|s| options.iter().any(|(o, _)| o == s))
                .or_else(|| options.iter().find(|(_, k)| if allow { k == "allow_once" } else { k == "reject_once" }).map(|(o, _)| o.clone()))
                .or_else(|| options.iter().find(|(_, k)| k.starts_with(if allow { "allow" } else { "reject" })).map(|(o, _)| o.clone()))
        };
        let outcome = match &response {
            PermissionResponse::Allow { selected_action_id, .. } => pick(selected_action_id, true),
            PermissionResponse::Deny { selected_action_id, .. } => pick(selected_action_id, false),
        };
        let result = match outcome {
            Some(option) => json!({"outcome": {"outcome": "selected", "optionId": option}}),
            None => json!({"outcome": {"outcome": "cancelled"}}),
        };
        self.rpc.respond(rpc_id, result).await.map_err(|e| ProviderError::new(e.to_string()))?;
        self.emit(StreamEvent::PermissionResolved { provider: self.provider.clone(), request_id: request_id.into(), resolution: response });
        Ok(())
    }

    async fn close(&self) {
        let _ = self.interrupt().await;
        self.proc_.lock().await.shutdown(Duration::from_secs(3)).await;
    }

    fn runtime_info(&self) -> RuntimeInfo {
        let st = self.lock();
        RuntimeInfo {
            provider: self.provider.clone(),
            session_id: Some(st.session_id.clone()),
            model: st.model.clone(),
            thinking_option_id: None,
            mode_id: st.mode.clone(),
            extra: None,
        }
    }

    fn persistence(&self) -> PersistenceHandle {
        let st = self.lock();
        let mut meta = Map::new();
        if let Some(m) = &st.model {
            meta.insert("model".into(), json!(m));
        }
        PersistenceHandle { provider: self.provider.clone(), session_id: st.session_id.clone(), native_handle: None, metadata: Some(meta) }
    }

    fn current_mode(&self) -> Option<String> {
        self.lock().mode.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_args_drop_missing_model() {
        let p = AcpProvider::new(builtin()[0].clone(), PathBuf::from("/x"), None, None);
        assert_eq!(p.client_meta, Some(json!({"parameterizedModelPicker": true})));
        assert_eq!(p.launch_args(None), ["acp"]);
        assert_eq!(p.launch_args(Some("composer-2.5")), ["--model", "composer-2.5", "acp"]);
    }

    fn opencode_options(current: &str) -> Vec<Value> {
        vec![
            json!({"id":"model","category":"model","type":"select","currentValue":current,"options":[
                {"value":"opencode/big-pickle","name":"Big Pickle"},
                {"value":"opencode/deepseek-v4.1-flash-free","name":"DeepSeek"},
                {"value":"openrouter/x/deepseek-v4.1-flash-free","name":"Other"}]}),
            json!({"id":"mode","type":"select","currentValue":"build","options":[]}),
        ]
    }

    #[test]
    fn model_applied_via_config_option() {
        let opts = opencode_options("opencode/big-pickle");
        // The full id is set as given.
        assert_eq!(plan_model(&opts, &Value::Null, "opencode/deepseek-v4.1-flash-free"), Ok(ModelPlan::Config("opencode/deepseek-v4.1-flash-free".into())));
        // Already current: nothing to send.
        assert_eq!(plan_model(&opts, &Value::Null, "opencode/big-pickle"), Ok(ModelPlan::Keep("opencode/big-pickle".into())));
        // A bare id resolves to the one provider-prefixed value that ends with it.
        let opts2 = vec![json!({"id":"model","currentValue":"a/m","options":[{"value":"a/m"},{"value":"opencode/deepseek-v4.1-flash-free"}]})];
        assert_eq!(plan_model(&opts2, &Value::Null, "deepseek-v4.1-flash-free"), Ok(ModelPlan::Config("opencode/deepseek-v4.1-flash-free".into())));
        // Grouped option lists are read too.
        let grouped = vec![json!({"id":"model","currentValue":"a","options":[{"group":"g","name":"G","options":[{"value":"a"},{"value":"b"}]}]})];
        assert_eq!(plan_model(&grouped, &Value::Null, "b"), Ok(ModelPlan::Config("b".into())));
        // Legacy `models` block uses session/set_model.
        let legacy = json!({"currentModelId":"m1","availableModels":[{"modelId":"m1"},{"modelId":"m2"}]});
        assert_eq!(plan_model(&[], &legacy, "m2"), Ok(ModelPlan::SetModel("m2".into())));
        assert_eq!(plan_model(&[], &legacy, "m1"), Ok(ModelPlan::Keep("m1".into())));
    }

    #[test]
    fn model_not_offered_is_an_error() {
        let opts = opencode_options("opencode/big-pickle");
        let e = plan_model(&opts, &Value::Null, "gpt-nope").unwrap_err();
        assert!(e.contains("does not offer it") && e.contains("opencode/big-pickle"), "{e}");
        // Ambiguous bare id: refuse rather than guess.
        let amb = vec![json!({"id":"model","currentValue":"a/m","options":[{"value":"a/m"},{"value":"b/z/m"},{"value":"c/m"}]})];
        assert!(plan_model(&amb, &Value::Null, "m").unwrap_err().contains("ambiguous"));
        // No model option and no models block at all.
        assert!(plan_model(&[json!({"id":"mode"})], &Value::Null, "x").unwrap_err().contains("no model selection"));
        let legacy = json!({"currentModelId":"m1","availableModels":[{"modelId":"m1"}]});
        assert!(plan_model(&[], &legacy, "m9").is_err());
    }

    #[test]
    fn model_in_args_skips_acp_set() {
        // Cursor takes `--model {model}` at launch; OpenCode and Copilot do not.
        let defs = builtin();
        let by = |id: &str| AcpProvider::new(defs.iter().find(|d| d.id == id).unwrap().clone(), PathBuf::from("/x"), None, None);
        assert!(by("cursor").model_in_args());
        assert!(!by("opencode").model_in_args());
        assert!(!by("copilot").model_in_args());
        assert_eq!(by("cursor").model_to_set_over_acp(Some("composer-2.5")), None);
        assert_eq!(by("opencode").model_to_set_over_acp(Some("opencode/x")), Some("opencode/x"));
        // No model requested: nothing is set, whatever the provider.
        assert_eq!(by("opencode").model_to_set_over_acp(None), None);
        assert_eq!(by("copilot").model_to_set_over_acp(None), None);
    }

    #[test]
    fn maps_tool_calls() {
        let d = acp_detail(&json!({"kind":"execute","title":"Run ls","rawInput":{"command":"ls -la"}}), None);
        assert_eq!(serde_json::to_value(&d).unwrap(), json!({"type":"shell","command":"ls -la"}));
        let d2 = acp_detail(&json!({"toolCallId":"t","status":"completed","content":[{"type":"content","content":{"type":"text","text":"out"}}]}), Some(&d));
        assert_eq!(serde_json::to_value(&d2).unwrap(), json!({"type":"shell","command":"ls -la","output":"out"}));
        let e = acp_detail(&json!({"kind":"edit","title":"Edit","content":[{"type":"diff","path":"/w/a","oldText":"a","newText":"b"}]}), None);
        assert_eq!(serde_json::to_value(&e).unwrap(), json!({"type":"edit","filePath":"/w/a","oldString":"a","newString":"b"}));
        let r = acp_detail(&json!({"kind":"read","title":"Read","locations":[{"path":"/w/b"}]}), None);
        assert_eq!(r.kind(), "read");
        // OpenCode's `task` call is a sub_agent card, with the child's session once the result names it.
        let t = acp_detail(&json!({"kind":"other","title":"task","rawInput":{"subagent_type":"explore","description":"find it","prompt":"p"}}), None);
        assert_eq!(serde_json::to_value(&t).unwrap(), json!({"type":"sub_agent","subAgentType":"explore","description":"find it","log":"","actions":[]}));
        let t2 = acp_detail(&json!({"toolCallId":"t","status":"completed","kind":"other","rawOutput":{"output":"found","metadata":{"sessionId":"ses_1"}}}), Some(&t));
        assert_eq!(serde_json::to_value(&t2).unwrap(), json!({"type":"sub_agent","subAgentType":"explore","description":"find it","childSessionId":"ses_1","log":"found","actions":[]}));
        assert_eq!(tool_status(&json!("in_progress")), Some(ToolCallStatus::Running));
        assert!(is_quota_reply("Upgrade your plan to continue"));
        assert!(is_quota_reply("Error: You have exceeded your monthly quota (Request ID: F361)"));
        assert!(!is_quota_reply("ok"));
        assert!(!is_quota_reply("Here is how to upgrade your plan to continue using the API: ..."));
    }
}
