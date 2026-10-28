//! Codex through `codex app-server` (JSON-RPC over stdio; the schema comes from
//! `codex app-server generate-json-schema`). Mirrors Paseo's
//! `codex-app-server-agent.js` for the core flow: `initialize` + `initialized`,
//! `thread/start` or `thread/resume`, `turn/start`, `turn/steer`, `turn/interrupt`;
//! item notifications become timeline items, and approval requests become
//! permission requests.
//!
//! Modes are Paseo's presets: `auto` (on-request, workspace-write), `auto-review`
//! (same, approvals routed to Codex's auto-reviewer), `read-only` and `full-access`
//! (never ask, danger-full-access).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lucina_agents::permission::{ActionVariant, Behavior, PermissionAction, PermissionKind, PermissionRequest};
use lucina_agents::timeline::{
    CompactionStatus, SearchDetail, SearchToolName, TodoEntry, TodoStatus, ToolCall, ToolCallDetail, ToolCallStatus,
};
use lucina_agents::types::{AgentMode, Capabilities, PersistenceHandle, RuntimeInfo};
use lucina_agents::{PermissionResponse, StreamEvent, TimelineItem, Usage};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::jsonrpc::{Incoming, Rpc};
use crate::process::{Proc, Registry, Spec};
use crate::provider::{EventSink, ModelInfo, Provider, ProviderError, ProviderInfo, Session, SessionConfig};

pub const ID: &str = "codex";
const RPC_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_MODE: &str = "auto";

fn mode(id: &str, label: &str, description: &str, icon: &str, tier: &str) -> AgentMode {
    AgentMode {
        id: id.into(),
        label: label.into(),
        description: Some(description.into()),
        icon: Some(icon.into()),
        color_tier: Some(tier.into()),
    }
}

/// `(approvalPolicy, sandbox, approvalsReviewer)` for a mode (Paseo `MODE_PRESETS`).
fn preset(mode: &str) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match mode {
        "read-only" => ("on-request", "read-only", "user"),
        "auto" => ("on-request", "workspace-write", "user"),
        "auto-review" => ("on-request", "workspace-write", "auto_review"),
        "full-access" => ("never", "danger-full-access", "user"),
        _ => return None,
    })
}

pub struct CodexProvider {
    pub bin: PathBuf,
    pub default_model: Option<String>,
    pub default_effort: Option<String>,
    pub registry: Option<Registry>,
}

impl CodexProvider {
    async fn start(&self, config: SessionConfig, events: EventSink, resume: Option<&str>) -> Result<Arc<dyn Session>, ProviderError> {
        let mode = config.mode_id.clone().unwrap_or_else(|| DEFAULT_MODE.into());
        let (approval, sandbox, reviewer) = preset(&mode).ok_or_else(|| {
            ProviderError::new(format!("Invalid Codex mode \"{mode}\". Valid modes are: read-only, auto, auto-review, full-access"))
        })?;
        let model = config.model.clone().or_else(|| self.default_model.clone());
        let effort = config.thinking_option_id.clone().or_else(|| self.default_effort.clone());
        let spec = Spec { program: self.bin.clone(), args: vec!["app-server".into()], cwd: config.cwd.clone(), env: config.env.clone(), env_remove: Vec::new() };
        let (proc_, stdin, stdout) = Proc::spawn(&spec, self.registry.as_ref()).map_err(|e| ProviderError::new(format!("cannot start codex app-server: {e}")))?;
        let (rpc, incoming) = Rpc::start(stdin, stdout);
        let proc_ = Arc::new(tokio::sync::Mutex::new(proc_));
        let fail = |what: &str, e: crate::jsonrpc::RpcError| ProviderError::new(format!("codex {what}: {e}"));
        rpc.request(
            "initialize",
            json!({"clientInfo": {"name": "lucina-agentd", "title": "Lucina Agents", "version": env!("CARGO_PKG_VERSION")}}),
            RPC_TIMEOUT,
        )
        .await
        .map_err(|e| fail("initialize", e))?;
        rpc.notify("initialized", json!({})).await.map_err(|e| fail("initialized", e))?;
        let mut params = json!({"cwd": config.cwd, "approvalPolicy": approval, "sandbox": sandbox, "approvalsReviewer": reviewer});
        if let Some(m) = &model {
            params["model"] = json!(m);
        }
        let (method, thread_id) = match resume {
            Some(id) => {
                params["threadId"] = json!(id);
                ("thread/resume", id.to_string())
            }
            None => ("thread/start", String::new()),
        };
        let resp = rpc.request(method, params, RPC_TIMEOUT).await.map_err(|e| fail(method, e))?;
        let thread_id = resp["thread"]["id"].as_str().map(str::to_string).filter(|s| !s.is_empty()).unwrap_or(thread_id);
        if thread_id.is_empty() {
            return Err(ProviderError::new("codex did not return a thread id"));
        }
        let model = resp["model"].as_str().map(str::to_string).or(model);
        let _ = events.send(StreamEvent::ThreadStarted { session_id: thread_id.clone(), provider: ID.into() });
        let session = Arc::new(CodexSession {
            rpc,
            proc_,
            events,
            state: Arc::new(Mutex::new(State {
                thread_id,
                model,
                effort,
                mode,
                turn_id: None,
                streamed: HashSet::new(),
                permissions: HashMap::new(),
                last_error: None,
            })),
        });
        session.clone().spawn_reader(incoming);
        Ok(session)
    }
}

pub fn modes() -> Vec<AgentMode> {
    vec![
        mode("auto", "Default Permissions", "Edit files and run commands with Codex's default approval flow.", "Shield", "moderate"),
        mode("auto-review", "Auto-review", "Same workspace-write permissions as Default, but eligible `on-request` approvals are routed through the auto-reviewer subagent.", "ShieldCheck", "moderate"),
        mode("read-only", "Read Only", "Read files and answer; every change needs approval.", "Eye", "safe"),
        mode("full-access", "Full Access", "Edit files, run commands, and access the network without additional prompts.", "ShieldOff", "dangerous"),
    ]
}

#[async_trait]
impl Provider for CodexProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: ID.into(),
            label: "Codex".into(),
            default_mode_id: Some(DEFAULT_MODE.into()),
            modes: modes(),
            models: self
                .default_model
                .iter()
                .map(|m| ModelInfo { id: m.clone(), label: m.clone(), description: None, is_default: true, is_selectable: true, default_thinking_option_id: self.default_effort.clone() })
                .collect(),
            capabilities: Capabilities {
                supports_streaming: true,
                supports_session_persistence: true,
                supports_session_listing: Some(true),
                supports_dynamic_modes: false,
                supports_mcp_servers: true,
                supports_reasoning_stream: true,
                supports_tool_invocations: true,
                supports_rewind_conversation: false,
                supports_rewind_files: false,
                supports_rewind_both: false,
                extra: [("supportsSteer".to_string(), true)].into_iter().collect(),
            },
            echoes_user_message: true,
        }
    }

    async fn create_session(&self, config: SessionConfig, events: EventSink) -> Result<Arc<dyn Session>, ProviderError> {
        self.start(config, events, None).await
    }

    async fn resume_session(&self, handle: &PersistenceHandle, config: SessionConfig, events: EventSink) -> Result<Arc<dyn Session>, ProviderError> {
        self.start(config, events, Some(&handle.session_id)).await
    }
}

/// A pending approval: the JSON-RPC id to answer, and how to word the answer.
#[derive(Debug, Clone)]
struct Pending {
    rpc_id: Value,
    method: String,
}

struct State {
    thread_id: String,
    model: Option<String>,
    effort: Option<String>,
    mode: String,
    turn_id: Option<String>,
    /// Agent-message items that streamed deltas (their completion is not re-emitted).
    streamed: HashSet<String>,
    permissions: HashMap<String, Pending>,
    last_error: Option<String>,
}

pub struct CodexSession {
    rpc: Arc<Rpc>,
    proc_: Arc<tokio::sync::Mutex<Proc>>,
    events: EventSink,
    state: Arc<Mutex<State>>,
}

fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

fn tool(call_id: &str, name: &str, detail: ToolCallDetail, status: ToolCallStatus, error: Value) -> TimelineItem {
    TimelineItem::ToolCall(Box::new(ToolCall { agent_message: None, call_id: call_id.into(), name: name.into(), detail, metadata: None, status, error }))
}

fn item_status(status: &Value) -> ToolCallStatus {
    match status.as_str() {
        Some("completed") => ToolCallStatus::Completed,
        Some("failed") => ToolCallStatus::Failed,
        Some("declined") => ToolCallStatus::Canceled,
        _ => ToolCallStatus::Running,
    }
}

/// A Codex `ThreadItem` as a timeline item (`None` for items that stream
/// separately or have no timeline form).
fn map_item(item: &Value, completed: bool) -> Option<TimelineItem> {
    let id = item["id"].as_str().unwrap_or_default();
    let status = if completed { item_status(&item["status"]) } else { ToolCallStatus::Running };
    let status = if completed && item["status"].is_null() { ToolCallStatus::Completed } else { status };
    let error = |st: ToolCallStatus| if st == ToolCallStatus::Failed { json!("failed") } else { Value::Null };
    match item["type"].as_str()? {
        "commandExecution" => Some(tool(
            id,
            "shell",
            ToolCallDetail::Shell {
                command: s(&item["command"]).unwrap_or_default(),
                cwd: s(&item["cwd"]),
                output: s(&item["aggregatedOutput"]),
                exit_code: item.get("exitCode").filter(|v| !v.is_null()).map(|v| v.as_i64().map(Into::into)),
            },
            status,
            error(status),
        )),
        "fileChange" => {
            let changes = item["changes"].as_array().cloned().unwrap_or_default();
            let first = changes.first().cloned().unwrap_or(Value::Null);
            let path = s(&first["path"]).unwrap_or_default();
            let diff = changes.iter().filter_map(|c| c["diff"].as_str()).collect::<Vec<_>>().join("\n");
            let detail = if first["kind"]["type"] == "add" || first["kind"] == "add" {
                ToolCallDetail::Write { file_path: path, content: None }
            } else {
                ToolCallDetail::Edit { file_path: path, old_string: None, new_string: None, unified_diff: (!diff.is_empty()).then_some(diff) }
            };
            Some(tool(id, "apply_patch", detail, status, error(status)))
        }
        "mcpToolCall" => {
            let name = format!("mcp__{}__{}", item["server"].as_str().unwrap_or("mcp"), item["tool"].as_str().unwrap_or("tool"));
            let err = if status == ToolCallStatus::Failed { item["error"].clone() } else { Value::Null };
            let err = if status == ToolCallStatus::Failed && err.is_null() { json!("failed") } else { err };
            Some(tool(id, &name, ToolCallDetail::Unknown { input: item["arguments"].clone(), output: item["result"].clone() }, status, err))
        }
        "dynamicToolCall" => Some(tool(
            id,
            item["tool"].as_str().unwrap_or("tool"),
            ToolCallDetail::Unknown { input: item["arguments"].clone(), output: item["contentItems"].clone() },
            status,
            error(status),
        )),
        "webSearch" => Some(tool(
            id,
            "web_search",
            ToolCallDetail::Search(SearchDetail {
                query: s(&item["query"]).unwrap_or_default(),
                tool_name: Some(SearchToolName::WebSearch),
                content: None,
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
            if completed { ToolCallStatus::Completed } else { ToolCallStatus::Running },
            Value::Null,
        )),
        "plan" if completed => Some(tool(id, "plan", ToolCallDetail::Plan { text: s(&item["text"]).unwrap_or_default() }, ToolCallStatus::Completed, Value::Null)),
        "contextCompaction" => Some(TimelineItem::Compaction {
            status: if completed { CompactionStatus::Completed } else { CompactionStatus::Loading },
            trigger: None,
            pre_tokens: None,
        }),
        "userMessage" if !completed => {
            let text = item["content"]
                .as_array()
                .map(|c| c.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"))
                .unwrap_or_default();
            Some(TimelineItem::UserMessage { text, message_id: Some(id.to_string()), client_message_id: s(&item["clientId"]) })
        }
        _ => None,
    }
}

impl CodexSession {
    fn emit(&self, ev: StreamEvent) {
        let _ = self.events.send(ev);
    }

    fn timeline(&self, item: TimelineItem, turn: Option<String>) {
        self.emit(StreamEvent::Timeline { provider: ID.into(), item, turn_id: turn });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn spawn_reader(self: Arc<Self>, mut incoming: UnboundedReceiver<Incoming>) {
        tokio::spawn(async move {
            while let Some(msg) = incoming.recv().await {
                match msg {
                    Incoming::Notification { method, params } => self.on_notification(&method, &params),
                    Incoming::Request { id, method, params } => self.on_request(id, &method, &params).await,
                    Incoming::Closed => {
                        let turn = self.lock().turn_id.take();
                        if let Some(turn) = turn {
                            let tail = self.proc_.lock().await.stderr_tail();
                            self.emit(StreamEvent::TurnFailed {
                                provider: ID.into(),
                                turn_id: Some(turn),
                                error: "Codex app-server exited".into(),
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

    fn on_notification(&self, method: &str, p: &Value) {
        let turn = s(&p["turnId"]).or_else(|| s(&p["turn"]["id"]));
        match method {
            "turn/started" => {
                self.lock().turn_id = turn.clone();
                self.emit(StreamEvent::TurnStarted { provider: ID.into(), turn_id: turn });
            }
            "item/agentMessage/delta" => {
                let item_id = s(&p["itemId"]).unwrap_or_default();
                self.lock().streamed.insert(item_id.clone());
                self.timeline(TimelineItem::AssistantMessage { text: s(&p["delta"]).unwrap_or_default(), message_id: Some(item_id) }, turn);
            }
            "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                self.timeline(TimelineItem::Reasoning { text: s(&p["delta"]).unwrap_or_default() }, turn);
            }
            "item/started" => {
                if let Some(item) = map_item(&p["item"], false) {
                    self.timeline(item, turn);
                }
            }
            "item/completed" => {
                let item = &p["item"];
                if item["type"] == "agentMessage" {
                    let id = s(&item["id"]).unwrap_or_default();
                    if !self.lock().streamed.remove(&id) {
                        self.timeline(TimelineItem::AssistantMessage { text: s(&item["text"]).unwrap_or_default(), message_id: Some(id) }, turn);
                    }
                } else if let Some(mapped) = map_item(item, true) {
                    self.timeline(mapped, turn);
                }
            }
            "turn/plan/updated" => {
                let items = p["plan"]
                    .as_array()
                    .map(|steps| {
                        steps
                            .iter()
                            .map(|st| {
                                let status = match st["status"].as_str() {
                                    Some("completed") => TodoStatus::Completed,
                                    Some("inProgress") | Some("in_progress") => TodoStatus::InProgress,
                                    _ => TodoStatus::Pending,
                                };
                                TodoEntry {
                                    text: s(&st["step"]).unwrap_or_default(),
                                    completed: status == TodoStatus::Completed,
                                    id: None,
                                    status: Some(status),
                                    active_form: None,
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                self.timeline(TimelineItem::Todo { items }, turn);
            }
            "thread/tokenUsage/updated" => {
                let u = &p["tokenUsage"];
                let usage = Usage {
                    input_tokens: u["last"]["inputTokens"].as_u64().map(Into::into),
                    cached_input_tokens: u["last"]["cachedInputTokens"].as_u64().map(Into::into),
                    output_tokens: u["last"]["outputTokens"].as_u64().map(Into::into),
                    total_cost_usd: None,
                    context_window_max_tokens: u["modelContextWindow"].as_u64().map(Into::into),
                    context_window_used_tokens: u["total"]["totalTokens"].as_u64().map(Into::into),
                };
                self.emit(StreamEvent::UsageUpdated { provider: ID.into(), turn_id: turn, usage });
            }
            "error" => {
                if p["willRetry"] != true {
                    let message = s(&p["error"]["message"]).or_else(|| s(&p["error"])).unwrap_or_else(|| p["error"].to_string());
                    self.lock().last_error = Some(message);
                }
            }
            "turn/completed" => {
                let t = &p["turn"];
                self.lock().turn_id = None;
                match t["status"].as_str() {
                    Some("interrupted") => self.emit(StreamEvent::TurnCanceled { provider: ID.into(), turn_id: turn, reason: "Interrupted".into() }),
                    Some("failed") => {
                        let message = s(&t["error"]["message"]).or_else(|| self.lock().last_error.take()).unwrap_or_else(|| "Codex turn failed".into());
                        self.emit(StreamEvent::TurnFailed { provider: ID.into(), turn_id: turn, error: message, code: None, diagnostic: None });
                    }
                    _ => self.emit(StreamEvent::TurnCompleted { provider: ID.into(), turn_id: turn, usage: None }),
                }
            }
            _ => {}
        }
    }

    async fn on_request(&self, id: Value, method: &str, p: &Value) {
        let (name, kind, title, detail, input) = match method {
            "item/commandExecution/requestApproval" | "execCommandApproval" => {
                let command = match &p["command"] {
                    Value::Array(parts) => parts.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "),
                    other => s(other).unwrap_or_default(),
                };
                let detail = ToolCallDetail::Shell { command: command.clone(), cwd: s(&p["cwd"]), output: None, exit_code: None };
                ("Bash", PermissionKind::Tool, command, Some(detail), p.clone())
            }
            "item/fileChange/requestApproval" | "applyPatchApproval" => ("Edit", PermissionKind::Tool, "Apply file changes".to_string(), None, p.clone()),
            "item/permissions/requestApproval" => ("Permissions", PermissionKind::Other, "Grant extra permissions".to_string(), None, p.clone()),
            "item/tool/requestUserInput" => ("AskUserQuestion", PermissionKind::Question, "Questions".to_string(), None, p.clone()),
            _ => {
                let _ = self.rpc.respond_error(id, -32601, &format!("lucina-agentd does not handle {method}")).await;
                return;
            }
        };
        let request_id = format!("codex-{}", id);
        let request = PermissionRequest {
            id: request_id.clone(),
            provider: ID.into(),
            name: name.into(),
            kind,
            title: Some(title),
            description: s(&p["reason"]),
            input: input.as_object().cloned(),
            detail,
            suggestions: None,
            actions: (kind != PermissionKind::Question).then(|| {
                vec![
                    PermissionAction { id: "accept".into(), label: "Allow".into(), behavior: Behavior::Allow, variant: Some(ActionVariant::Primary), intent: None },
                    PermissionAction { id: "acceptForSession".into(), label: "Allow for this session".into(), behavior: Behavior::Allow, variant: Some(ActionVariant::Secondary), intent: None },
                    PermissionAction { id: "decline".into(), label: "Deny".into(), behavior: Behavior::Deny, variant: Some(ActionVariant::Danger), intent: None },
                ]
            }),
            metadata: None,
        };
        self.lock().permissions.insert(request_id, Pending { rpc_id: id, method: method.to_string() });
        self.emit(StreamEvent::PermissionRequested { provider: ID.into(), request: Box::new(request) });
    }

    fn text_input(text: &str) -> Value {
        json!([{"type": "text", "text": text, "text_elements": []}])
    }
}

#[async_trait]
impl Session for CodexSession {
    fn session_id(&self) -> String {
        self.lock().thread_id.clone()
    }

    async fn start_turn(&self, prompt: &str, client_message_id: Option<String>) -> Result<String, ProviderError> {
        let (thread, effort) = {
            let st = self.lock();
            (st.thread_id.clone(), st.effort.clone())
        };
        let mut params = json!({"threadId": thread, "input": Self::text_input(prompt)});
        if let Some(e) = effort {
            params["effort"] = json!(e);
        }
        if let Some(c) = client_message_id {
            params["clientUserMessageId"] = json!(c);
        }
        let resp = self.rpc.request("turn/start", params, RPC_TIMEOUT).await.map_err(|e| ProviderError::new(e.to_string()))?;
        let turn = s(&resp["turn"]["id"]).unwrap_or_default();
        self.lock().turn_id = Some(turn.clone());
        Ok(turn)
    }

    async fn steer(&self, prompt: &str, expected_turn_id: &str) -> Result<bool, ProviderError> {
        let thread = self.lock().thread_id.clone();
        let params = json!({"threadId": thread, "expectedTurnId": expected_turn_id, "input": Self::text_input(prompt)});
        match self.rpc.request("turn/steer", params, RPC_TIMEOUT).await {
            Ok(_) => Ok(true),
            Err(crate::jsonrpc::RpcError::Remote { .. }) => Ok(false),
            Err(e) => Err(ProviderError::new(e.to_string())),
        }
    }

    async fn interrupt(&self) -> Result<(), ProviderError> {
        let (thread, turn) = {
            let st = self.lock();
            (st.thread_id.clone(), st.turn_id.clone())
        };
        let Some(turn) = turn else { return Ok(()) };
        self.rpc
            .request("turn/interrupt", json!({"threadId": thread, "turnId": turn}), RPC_TIMEOUT)
            .await
            .map(|_| ())
            .map_err(|e| ProviderError::new(e.to_string()))
    }

    async fn respond_permission(&self, request_id: &str, response: PermissionResponse) -> Result<(), ProviderError> {
        let Some(pending) = self.lock().permissions.remove(request_id) else {
            return Ok(());
        };
        let result = match (&response, pending.method.as_str()) {
            (PermissionResponse::Allow { updated_input, .. }, "item/tool/requestUserInput") => {
                json!({"answers": updated_input.as_ref().and_then(|m| m.get("answers").cloned()).unwrap_or(json!({}))})
            }
            (PermissionResponse::Deny { .. }, "item/tool/requestUserInput") => json!({"answers": {}}),
            (PermissionResponse::Allow { selected_action_id, .. }, _) => {
                let decision = match selected_action_id.as_deref() {
                    Some("acceptForSession") => "acceptForSession",
                    _ => "accept",
                };
                json!({"decision": decision})
            }
            (PermissionResponse::Deny { interrupt, .. }, _) => json!({"decision": if *interrupt == Some(true) { "cancel" } else { "decline" }}),
        };
        self.rpc.respond(pending.rpc_id, result).await.map_err(|e| ProviderError::new(e.to_string()))?;
        self.emit(StreamEvent::PermissionResolved { provider: ID.into(), request_id: request_id.into(), resolution: response });
        Ok(())
    }

    async fn close(&self) {
        let _ = self.interrupt().await;
        self.proc_.lock().await.shutdown(Duration::from_secs(3)).await;
    }

    fn runtime_info(&self) -> RuntimeInfo {
        let st = self.lock();
        RuntimeInfo {
            provider: ID.into(),
            session_id: Some(st.thread_id.clone()),
            model: st.model.clone(),
            thinking_option_id: st.effort.clone(),
            mode_id: Some(st.mode.clone()),
            extra: None,
        }
    }

    fn persistence(&self) -> PersistenceHandle {
        let st = self.lock();
        let mut meta = Map::new();
        if let Some(m) = &st.model {
            meta.insert("model".into(), json!(m));
        }
        meta.insert("modeId".into(), json!(st.mode));
        PersistenceHandle { provider: ID.into(), session_id: st.thread_id.clone(), native_handle: None, metadata: Some(meta) }
    }

    fn current_mode(&self) -> Option<String> {
        Some(self.lock().mode.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_command_and_file_items() {
        let started = map_item(&json!({"type":"commandExecution","id":"c1","command":"ls","cwd":"/w","commandActions":[],"status":"inProgress"}), false).unwrap();
        let done = map_item(&json!({"type":"commandExecution","id":"c1","command":"ls","cwd":"/w","commandActions":[],"status":"completed","aggregatedOutput":"a\n","exitCode":0}), true).unwrap();
        let (TimelineItem::ToolCall(a), TimelineItem::ToolCall(b)) = (started, done) else { panic!("tool calls") };
        assert_eq!((a.status, b.status), (ToolCallStatus::Running, ToolCallStatus::Completed));
        assert_eq!(serde_json::to_value(&b.detail).unwrap(), json!({"type":"shell","command":"ls","cwd":"/w","output":"a\n","exitCode":0}));
        let failed = map_item(&json!({"type":"commandExecution","id":"c2","command":"false","cwd":"/w","commandActions":[],"status":"failed","exitCode":1}), true).unwrap();
        let TimelineItem::ToolCall(f) = failed else { panic!() };
        assert_eq!(f.status, ToolCallStatus::Failed);
        assert!(!f.error.is_null());
        let patch = map_item(&json!({"type":"fileChange","id":"f","status":"completed","changes":[{"path":"/w/a.rs","kind":{"type":"update"},"diff":"@@ -1 +1 @@"}]}), true).unwrap();
        assert_eq!(patch.kind(), "tool_call");
        assert!(map_item(&json!({"type":"reasoning","id":"r"}), true).is_none());
        assert_eq!(preset("auto"), Some(("on-request", "workspace-write", "user")));
        assert_eq!(preset("bogus"), None);
    }
}
