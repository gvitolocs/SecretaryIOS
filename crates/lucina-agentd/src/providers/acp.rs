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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lucina_agents::permission::{ActionVariant, Behavior, PermissionAction, PermissionKind, PermissionRequest};
use lucina_agents::timeline::{SearchDetail, TodoEntry, TodoStatus, ToolCall, ToolCallDetail, ToolCallStatus};
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
}

/// The ACP agents agentd knows (Paseo's built-in and plugin providers).
pub fn builtin() -> Vec<AcpDef> {
    vec![
        // The parameterized picker exposes Cursor's model parameters (context,
        // reasoning effort, fast) as separate config options (Paseo
        // CURSOR_CLIENT_CAPABILITY_META); without it, `grok-4.7` resolves to fast=true.
        AcpDef { id: "cursor", label: "Cursor", bin: "cursor-agent", args: &["--model", "{model}", "acp"], client_meta: Some(r#"{"parameterizedModelPicker":true}"#) },
        AcpDef { id: "opencode", label: "OpenCode", bin: "opencode", args: &["acp"], client_meta: None },
        AcpDef { id: "copilot", label: "Copilot", bin: "copilot", args: &["--acp"], client_meta: None },
    ]
}

/// A custom agent from `~/.config/lucina-agentd/acp.toml`:
/// `[agents.kimi] label = "Kimi", command = ["kimi", "--acp"]`.
#[derive(Debug, Clone, Deserialize)]
pub struct CustomAcp {
    #[serde(default)]
    pub label: Option<String>,
    pub command: Vec<String>,
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
        }
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
            })),
        });
        session.clone().spawn_reader(incoming);
        let (session_id, modes, config_options) = match resume {
            Some(sid) if can_load => {
                let resp = rpc
                    .request("session/load", json!({"sessionId": sid, "cwd": config.cwd, "mcpServers": crate::mcp::acp_servers(config.agent_id.is_some())}), RPC_TIMEOUT)
                    .await
                    .map_err(|e| fail("session/load", e))?;
                (sid.to_string(), resp["modes"].clone(), resp["configOptions"].clone())
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
                (sid, resp["modes"].clone(), resp["configOptions"].clone())
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
        let options = config_options.as_array().cloned().unwrap_or_default();
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
}

pub struct AcpSession {
    provider: String,
    rpc: Arc<Rpc>,
    proc_: Arc<tokio::sync::Mutex<Proc>>,
    events: EventSink,
    state: Arc<Mutex<State>>,
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
                            let st = self.lock();
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
                    Incoming::Request { id, method, params } => self.on_request(id, &method, &params).await,
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
            st.session_id.clone()
        };
        self.emit(StreamEvent::TurnStarted { provider: self.provider.clone(), turn_id: Some(turn.clone()) });
        let rpc = self.rpc.clone();
        let events = self.events.clone();
        let state = self.state.clone();
        let provider = self.provider.clone();
        let proc_ = self.proc_.clone();
        let prompt = prompt.to_string();
        let turn_id = turn.clone();
        tokio::spawn(async move {
            let result = rpc
                .request("session/prompt", json!({"sessionId": session_id, "prompt": [{"type": "text", "text": prompt}]}), TURN_TIMEOUT)
                .await;
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
