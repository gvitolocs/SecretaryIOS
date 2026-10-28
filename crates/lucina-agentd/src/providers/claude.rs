//! Claude through the Claude Code CLI in stream-json mode: one long-lived
//! `claude -p --input-format stream-json --output-format stream-json --verbose
//! --include-partial-messages --permission-prompts host --permission-prompt-tool stdio`
//! per session (Paseo uses the Agent SDK, which drives the same CLI protocol and
//! passes the same `--permission-prompt-tool stdio` when the host answers prompts;
//! without it the CLI denies every prompt itself as "non-interactive").
//!
//! - A turn is one user message on stdin; it ends with a `result` message.
//! - Text and thinking stream as `stream_event` deltas. The complete `assistant`
//!   message that follows adds only its tool calls, so text isn't duplicated.
//! - Permission prompts arrive as `control_request` / `can_use_tool` and are
//!   answered with a `control_response`. Interrupts are a `control_request` too.
//! - Modes are Paseo's: plan, default ("Always Ask", the CLI's `manual`),
//!   acceptEdits, auto, bypassPermissions.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lucina_agents::permission::{ActionVariant, Behavior, PermissionAction, PermissionKind, PermissionRequest};
use lucina_agents::timeline::{SearchDetail, SearchToolName, TodoEntry, TodoStatus, ToolCall, ToolCallDetail, ToolCallStatus};
use lucina_agents::types::{AgentMode, Capabilities, PersistenceHandle, RuntimeInfo};
use lucina_agents::{PermissionResponse, StreamEvent, TimelineItem, Usage};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};

use crate::process::{Proc, Registry, Spec};
use crate::provider::{EventSink, ModelInfo, Provider, ProviderError, ProviderInfo, Session, SessionConfig};

pub const ID: &str = "claude";
const DEFAULT_MODE: &str = "default";

pub struct ClaudeProvider {
    pub bin: PathBuf,
    pub default_model: Option<String>,
    pub default_effort: Option<String>,
    pub registry: Option<Registry>,
}

fn mode(id: &str, label: &str, description: &str, icon: &str, tier: &str) -> AgentMode {
    AgentMode { id: id.into(), label: label.into(), description: Some(description.into()), icon: Some(icon.into()), color_tier: Some(tier.into()) }
}

pub fn modes() -> Vec<AgentMode> {
    vec![
        mode("plan", "Plan Mode", "Plan first; nothing changes until the plan is approved.", "ClipboardList", "safe"),
        mode("default", "Always Ask", "Ask before every tool that changes something.", "Shield", "safe"),
        mode("acceptEdits", "Accept File Edits", "Edit files without asking; ask for commands.", "ShieldCheck", "moderate"),
        mode("auto", "Auto mode", "Claude decides which actions need approval.", "ShieldCheck", "moderate"),
        mode("bypassPermissions", "Bypass", "Never ask.", "ShieldOff", "dangerous"),
    ]
}

/// The CLI's `--permission-mode` for a Paseo mode id.
fn cli_mode(mode: &str) -> Option<&'static str> {
    Some(match mode {
        "plan" => "plan",
        "default" | "manual" => "manual",
        "acceptEdits" => "acceptEdits",
        "auto" => "auto",
        "bypassPermissions" => "bypassPermissions",
        _ => return None,
    })
}

impl ClaudeProvider {
    async fn start(&self, config: SessionConfig, events: EventSink, resume: Option<&str>) -> Result<Arc<dyn Session>, ProviderError> {
        let mode = config.mode_id.clone().unwrap_or_else(|| DEFAULT_MODE.into());
        let cli = cli_mode(&mode).ok_or_else(|| ProviderError::new(format!("Invalid Claude mode \"{mode}\"")))?;
        let model = config.model.clone().or_else(|| self.default_model.clone());
        let effort = config.thinking_option_id.clone().or_else(|| self.default_effort.clone());
        let mut args: Vec<String> = [
            "-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
            "--include-partial-messages", "--permission-prompts", "host", "--permission-prompt-tool", "stdio",
            "--permission-mode", cli,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        if let Some(m) = &model {
            args.extend(["--model".into(), m.clone()]);
        }
        if let Some(e) = &effort {
            args.extend(["--effort".into(), e.clone()]);
        }
        if let Some(r) = resume {
            args.extend(["--resume".into(), r.to_string()]);
        }
        // The agents' own tools (subagents, heartbeats, search): `lagent mcp`.
        if config.agent_id.is_some() {
            if let Some(server) = crate::mcp::attach() {
                args.extend(["--mcp-config".into(), serde_json::json!({"mcpServers": {crate::mcp::SERVER: server}}).to_string()]);
            }
        }
        let spec = Spec { program: self.bin.clone(), args, cwd: config.cwd.clone(), env: config.env.clone(), env_remove: Vec::new() };
        let (proc_, stdin, stdout) = Proc::spawn(&spec, self.registry.as_ref()).map_err(|e| ProviderError::new(format!("cannot start claude: {e}")))?;
        let session = Arc::new(ClaudeSession {
            stdin: tokio::sync::Mutex::new(stdin),
            proc_: Arc::new(tokio::sync::Mutex::new(proc_)),
            events,
            state: Arc::new(Mutex::new(State {
                session_id: resume.map(str::to_string).unwrap_or_default(),
                model,
                effort,
                mode,
                turn_id: None,
                interrupting: false,
                streamed_messages: HashSet::new(),
                current_message: None,
                tools: HashMap::new(),
                permissions: HashMap::new(),
                request_seq: 0,
            })),
        });
        session.clone().spawn_reader(stdout);
        Ok(session)
    }
}

#[async_trait]
impl Provider for ClaudeProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: ID.into(),
            label: "Claude".into(),
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

struct ToolUse {
    name: String,
    input: Value,
}

struct State {
    session_id: String,
    model: Option<String>,
    effort: Option<String>,
    mode: String,
    turn_id: Option<String>,
    interrupting: bool,
    /// Assistant message ids whose text arrived as deltas.
    streamed_messages: HashSet<String>,
    current_message: Option<String>,
    tools: HashMap<String, ToolUse>,
    /// Our permission id -> the CLI's control request id.
    permissions: HashMap<String, String>,
    request_seq: u64,
}

pub struct ClaudeSession {
    stdin: tokio::sync::Mutex<ChildStdin>,
    proc_: Arc<tokio::sync::Mutex<Proc>>,
    events: EventSink,
    state: Arc<Mutex<State>>,
}

fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

fn search(query: Option<String>, tool: SearchToolName) -> ToolCallDetail {
    ToolCallDetail::Search(SearchDetail {
        query: query.unwrap_or_default(),
        tool_name: Some(tool),
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
    })
}

/// A Claude tool call's detail (Paseo `claude/tool-call-detail-parser.js`, the
/// common tools).
pub fn detail_for(name: &str, input: &Value, output: Option<&Value>) -> ToolCallDetail {
    let text_output = output.map(tool_result_text);
    match name {
        "Bash" => ToolCallDetail::Shell { command: s(&input["command"]).unwrap_or_default(), cwd: None, output: text_output, exit_code: None },
        "Read" => ToolCallDetail::Read {
            file_path: s(&input["file_path"]).unwrap_or_default(),
            content: text_output,
            offset: input["offset"].as_i64().map(Into::into),
            limit: input["limit"].as_i64().map(Into::into),
        },
        "Edit" | "MultiEdit" => ToolCallDetail::Edit {
            file_path: s(&input["file_path"]).unwrap_or_default(),
            old_string: s(&input["old_string"]),
            new_string: s(&input["new_string"]),
            unified_diff: None,
        },
        "Write" => ToolCallDetail::Write { file_path: s(&input["file_path"]).unwrap_or_default(), content: s(&input["content"]) },
        "Grep" => search(s(&input["pattern"]), SearchToolName::Grep),
        "Glob" => search(s(&input["pattern"]), SearchToolName::Glob),
        "WebSearch" => search(s(&input["query"]), SearchToolName::WebSearch),
        "WebFetch" => ToolCallDetail::Fetch { url: s(&input["url"]).unwrap_or_default(), prompt: s(&input["prompt"]), result: text_output, code: None, code_text: None, bytes: None, duration_ms: None },
        "Task" | "Agent" => ToolCallDetail::SubAgent {
            sub_agent_type: s(&input["subagent_type"]),
            description: s(&input["description"]),
            child_session_id: None,
            log: text_output.unwrap_or_default(),
            actions: Some(Vec::new()),
        },
        "ExitPlanMode" => ToolCallDetail::Plan { text: s(&input["plan"]).unwrap_or_default() },
        _ => ToolCallDetail::Unknown { input: input.clone(), output: output.cloned().unwrap_or(Value::Null) },
    }
}

fn tool_result_text(v: &Value) -> String {
    match v {
        Value::String(t) => t.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
        other => other.to_string(),
    }
}

fn todo_items(input: &Value) -> Vec<TodoEntry> {
    input["todos"]
        .as_array()
        .map(|todos| {
            todos
                .iter()
                .map(|t| {
                    let status = match t["status"].as_str() {
                        Some("completed") => TodoStatus::Completed,
                        Some("in_progress") => TodoStatus::InProgress,
                        _ => TodoStatus::Pending,
                    };
                    TodoEntry {
                        text: s(&t["content"]).unwrap_or_default(),
                        completed: status == TodoStatus::Completed,
                        id: s(&t["id"]),
                        status: Some(status),
                        active_form: s(&t["activeForm"]),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn tool_item(call_id: &str, name: &str, detail: ToolCallDetail, status: ToolCallStatus, error: Value) -> TimelineItem {
    TimelineItem::ToolCall(Box::new(ToolCall { agent_message: None, call_id: call_id.into(), name: name.into(), detail, metadata: None, status, error }))
}

impl ClaudeSession {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn emit(&self, ev: StreamEvent) {
        let _ = self.events.send(ev);
    }

    fn timeline(&self, item: TimelineItem) {
        let turn = self.lock().turn_id.clone();
        self.emit(StreamEvent::Timeline { provider: ID.into(), item, turn_id: turn });
    }

    async fn write(&self, msg: Value) -> Result<(), ProviderError> {
        let mut line = msg.to_string();
        line.push('\n');
        let mut w = self.stdin.lock().await;
        w.write_all(line.as_bytes()).await.map_err(|e| ProviderError::new(format!("claude stdin: {e}")))?;
        w.flush().await.map_err(|e| ProviderError::new(format!("claude stdin: {e}")))
    }

    fn spawn_reader(self: Arc<Self>, stdout: ChildStdout) {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else { continue };
                self.on_message(&msg).await;
            }
            let turn = self.lock().turn_id.take();
            if let Some(turn) = turn {
                let tail = self.proc_.lock().await.stderr_tail();
                self.emit(StreamEvent::TurnFailed {
                    provider: ID.into(),
                    turn_id: Some(turn),
                    error: "Claude Code exited".into(),
                    code: None,
                    diagnostic: (!tail.is_empty()).then_some(tail),
                });
            }
        });
    }

    async fn on_message(&self, msg: &Value) {
        match msg["type"].as_str() {
            Some("system") if msg["subtype"] == "init" => {
                let mut st = self.lock();
                if let Some(id) = s(&msg["session_id"]) {
                    st.session_id = id;
                }
                if let Some(m) = s(&msg["model"]) {
                    st.model = Some(m);
                }
                let id = st.session_id.clone();
                drop(st);
                self.emit(StreamEvent::ThreadStarted { session_id: id, provider: ID.into() });
            }
            Some("stream_event") => self.on_stream_event(&msg["event"]),
            Some("assistant") => self.on_assistant(&msg["message"]),
            Some("user") => self.on_user(&msg["message"]),
            Some("result") => self.on_result(msg),
            Some("control_request") => self.on_control_request(msg).await,
            _ => {}
        }
    }

    fn on_stream_event(&self, ev: &Value) {
        match ev["type"].as_str() {
            Some("message_start") => {
                let id = s(&ev["message"]["id"]);
                self.lock().current_message = id;
            }
            Some("content_block_delta") => {
                let delta = &ev["delta"];
                let message_id = self.lock().current_message.clone();
                if let Some(id) = &message_id {
                    self.lock().streamed_messages.insert(id.clone());
                }
                match delta["type"].as_str() {
                    Some("text_delta") => self.timeline(TimelineItem::AssistantMessage { text: s(&delta["text"]).unwrap_or_default(), message_id }),
                    Some("thinking_delta") => self.timeline(TimelineItem::Reasoning { text: s(&delta["thinking"]).unwrap_or_default() }),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn on_assistant(&self, message: &Value) {
        let id = s(&message["id"]);
        let streamed = id.as_ref().is_some_and(|i| self.lock().streamed_messages.contains(i));
        for block in message["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") if !streamed => self.timeline(TimelineItem::AssistantMessage { text: s(&block["text"]).unwrap_or_default(), message_id: id.clone() }),
                Some("thinking") if !streamed => self.timeline(TimelineItem::Reasoning { text: s(&block["thinking"]).unwrap_or_default() }),
                Some("tool_use") => {
                    let call_id = s(&block["id"]).unwrap_or_default();
                    let name = s(&block["name"]).unwrap_or_default();
                    let input = block["input"].clone();
                    if name == "TodoWrite" {
                        self.timeline(TimelineItem::Todo { items: todo_items(&input) });
                    }
                    self.timeline(tool_item(&call_id, &name, detail_for(&name, &input, None), ToolCallStatus::Running, Value::Null));
                    self.lock().tools.insert(call_id, ToolUse { name, input });
                }
                _ => {}
            }
        }
    }

    fn on_user(&self, message: &Value) {
        for block in message["content"].as_array().into_iter().flatten() {
            if block["type"] != "tool_result" {
                continue;
            }
            let call_id = s(&block["tool_use_id"]).unwrap_or_default();
            let Some(tool) = self.lock().tools.remove(&call_id) else { continue };
            let failed = block["is_error"] == true;
            let output = &block["content"];
            let (status, error) = if failed { (ToolCallStatus::Failed, json!(tool_result_text(output))) } else { (ToolCallStatus::Completed, Value::Null) };
            self.timeline(tool_item(&call_id, &tool.name, detail_for(&tool.name, &tool.input, Some(output)), status, error));
        }
    }

    fn on_result(&self, msg: &Value) {
        let (turn, interrupted) = {
            let mut st = self.lock();
            st.streamed_messages.clear();
            st.current_message = None;
            (st.turn_id.take(), std::mem::take(&mut st.interrupting))
        };
        let u = &msg["usage"];
        let usage = Usage {
            input_tokens: u["input_tokens"].as_u64().map(Into::into),
            cached_input_tokens: u["cache_read_input_tokens"].as_u64().map(Into::into),
            output_tokens: u["output_tokens"].as_u64().map(Into::into),
            total_cost_usd: msg["total_cost_usd"].as_f64().and_then(serde_json::Number::from_f64),
            context_window_max_tokens: None,
            context_window_used_tokens: None,
        };
        // Tools still open when the turn ends never got a result.
        let open: Vec<(String, ToolUse)> = self.lock().tools.drain().collect();
        for (call_id, tool) in open {
            let status = if interrupted { ToolCallStatus::Canceled } else { ToolCallStatus::Completed };
            self.emit(StreamEvent::Timeline {
                provider: ID.into(),
                item: tool_item(&call_id, &tool.name, detail_for(&tool.name, &tool.input, None), status, Value::Null),
                turn_id: turn.clone(),
            });
        }
        if interrupted {
            self.emit(StreamEvent::TurnCanceled { provider: ID.into(), turn_id: turn, reason: "Interrupted".into() });
        } else if msg["is_error"] == true || msg["subtype"].as_str().is_some_and(|st| st.starts_with("error")) {
            let error = s(&msg["result"]).filter(|r| !r.is_empty()).or_else(|| s(&msg["subtype"])).unwrap_or_else(|| "Claude turn failed".into());
            // `api_error` (e.g. usage_limit_reached) or the terminal reason; the
            // subtype says "success" even for API errors.
            let code = s(&msg["api_error"]).or_else(|| s(&msg["terminal_reason"])).or_else(|| s(&msg["subtype"]));
            let diagnostic = msg["api_error_status"].as_u64().map(|st| format!("HTTP {st}"));
            self.emit(StreamEvent::TurnFailed { provider: ID.into(), turn_id: turn, error, code, diagnostic });
        } else {
            self.emit(StreamEvent::TurnCompleted { provider: ID.into(), turn_id: turn, usage: Some(usage) });
        }
    }

    async fn on_control_request(&self, msg: &Value) {
        let request = &msg["request"];
        let cli_id = s(&msg["request_id"]).unwrap_or_default();
        if request["subtype"] != "can_use_tool" {
            let _ = self
                .write(json!({"type": "control_response", "response": {"subtype": "error", "request_id": cli_id, "error": "unsupported request"}}))
                .await;
            return;
        }
        let name = s(&request["tool_name"]).unwrap_or_default();
        let input = request["input"].clone();
        let kind = match name.as_str() {
            "ExitPlanMode" => PermissionKind::Plan,
            "AskUserQuestion" => PermissionKind::Question,
            _ => PermissionKind::Tool,
        };
        let id = {
            let mut st = self.lock();
            st.request_seq += 1;
            let id = format!("claude-perm-{}", st.request_seq);
            st.permissions.insert(id.clone(), cli_id);
            id
        };
        let request = PermissionRequest {
            id,
            provider: ID.into(),
            name: name.clone(),
            kind,
            title: Some(name.clone()),
            description: s(&request["decision_reason"]),
            input: input.as_object().cloned(),
            detail: Some(detail_for(&name, &input, None)),
            suggestions: request["permission_suggestions"].as_array().map(|a| a.iter().filter_map(|v| v.as_object().cloned()).collect()),
            actions: (kind != PermissionKind::Question).then(|| {
                vec![
                    PermissionAction { id: "allow".into(), label: "Allow".into(), behavior: Behavior::Allow, variant: Some(ActionVariant::Primary), intent: None },
                    PermissionAction { id: "deny".into(), label: "Deny".into(), behavior: Behavior::Deny, variant: Some(ActionVariant::Danger), intent: None },
                ]
            }),
            metadata: None,
        };
        self.emit(StreamEvent::PermissionRequested { provider: ID.into(), request: Box::new(request) });
    }
}

#[async_trait]
impl Session for ClaudeSession {
    fn session_id(&self) -> String {
        self.lock().session_id.clone()
    }

    async fn start_turn(&self, prompt: &str, _client_message_id: Option<String>) -> Result<String, ProviderError> {
        let turn = uuid::Uuid::new_v4().to_string();
        {
            let mut st = self.lock();
            if st.turn_id.is_some() {
                return Err(ProviderError::new("Claude already has a running turn"));
            }
            st.turn_id = Some(turn.clone());
        }
        let session_id = self.lock().session_id.clone();
        self.write(json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": prompt}]},
            "parent_tool_use_id": null,
            "session_id": session_id,
        }))
        .await?;
        self.emit(StreamEvent::TurnStarted { provider: ID.into(), turn_id: Some(turn.clone()) });
        Ok(turn)
    }

    async fn steer(&self, prompt: &str, expected_turn_id: &str) -> Result<bool, ProviderError> {
        // A user message sent while a turn runs is queued into it by Claude Code.
        if self.lock().turn_id.as_deref() != Some(expected_turn_id) {
            return Ok(false);
        }
        let session_id = self.lock().session_id.clone();
        self.write(json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": prompt}]},
            "parent_tool_use_id": null,
            "session_id": session_id,
        }))
        .await?;
        Ok(true)
    }

    async fn interrupt(&self) -> Result<(), ProviderError> {
        let id = {
            let mut st = self.lock();
            if st.turn_id.is_none() {
                return Ok(());
            }
            st.interrupting = true;
            st.request_seq += 1;
            format!("lucina-interrupt-{}", st.request_seq)
        };
        self.write(json!({"type": "control_request", "request_id": id, "request": {"subtype": "interrupt"}})).await
    }

    async fn respond_permission(&self, request_id: &str, response: PermissionResponse) -> Result<(), ProviderError> {
        let Some(cli_id) = self.lock().permissions.remove(request_id) else { return Ok(()) };
        let body = match &response {
            PermissionResponse::Allow { updated_input, updated_permissions, .. } => {
                let mut r = json!({"behavior": "allow", "updatedInput": updated_input.clone().map(Value::Object).unwrap_or(json!({}))});
                if let Some(p) = updated_permissions {
                    r["updatedPermissions"] = json!(p);
                }
                r
            }
            PermissionResponse::Deny { message, interrupt, .. } => {
                json!({"behavior": "deny", "message": message.clone().unwrap_or_else(|| "Denied by the owner".into()), "interrupt": interrupt.unwrap_or(false)})
            }
        };
        self.write(json!({"type": "control_response", "response": {"subtype": "success", "request_id": cli_id, "response": body}})).await?;
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
            session_id: (!st.session_id.is_empty()).then(|| st.session_id.clone()),
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
        PersistenceHandle { provider: ID.into(), session_id: st.session_id.clone(), native_handle: None, metadata: Some(meta) }
    }

    fn current_mode(&self) -> Option<String> {
        Some(self.lock().mode.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_details() {
        let d = detail_for("Bash", &json!({"command": "ls -la"}), Some(&json!("a\nb")));
        assert_eq!(serde_json::to_value(d).unwrap(), json!({"type":"shell","command":"ls -la","output":"a\nb"}));
        let d = detail_for("Read", &json!({"file_path": "/w/a.rs", "offset": 3}), None);
        assert_eq!(serde_json::to_value(d).unwrap(), json!({"type":"read","filePath":"/w/a.rs","offset":3}));
        let d = detail_for("Grep", &json!({"pattern": "TODO"}), None);
        assert_eq!(d.kind(), "search");
        let d = detail_for("mcp__x__y", &json!({"a": 1}), None);
        assert_eq!(d.kind(), "unknown");
        assert!(todo_items(&json!({"todos": [{"content":"a","status":"completed","activeForm":"Doing a"}]}))[0].completed);
        assert_eq!(cli_mode("default"), Some("manual"));
        assert_eq!(cli_mode("nope"), None);
    }
}
