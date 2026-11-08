//! Pi through `pi --mode rpc`: JSON-line commands on stdin (`prompt`, `steer`,
//! `abort`, `get_state`), and `response` objects plus session events on stdout
//! (Paseo `providers/pi/{runtime,rpc-types}.js`).
//!
//! A turn runs from `prompt` to `agent_end` (unless Pi will retry). Text and
//! thinking stream as `message_update` deltas, and tools as
//! `tool_execution_start` / `tool_execution_end`. Pi decides permissions itself.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lucina_agents::timeline::{CompactionStatus, ToolCall, ToolCallDetail, ToolCallStatus};
use lucina_agents::types::{Capabilities, PersistenceHandle, RuntimeInfo};
use lucina_agents::{PermissionResponse, StreamEvent, TimelineItem};
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::oneshot;

use crate::process::{Proc, Registry, Spec};
use crate::provider::{EventSink, Provider, ProviderError, ProviderInfo, Session, SessionConfig};

pub const ID: &str = "pi";
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

pub struct PiProvider {
    pub bin: PathBuf,
    pub registry: Option<Registry>,
}

impl PiProvider {
    async fn start(&self, config: SessionConfig, events: EventSink, resume: Option<&str>) -> Result<Arc<dyn Session>, ProviderError> {
        let mut args = vec!["--mode".to_string(), "rpc".to_string()];
        if let Some(m) = &config.model {
            args.extend(["--model".into(), m.clone()]);
        }
        if let Some(t) = &config.thinking_option_id {
            args.extend(["--thinking".into(), t.clone()]);
        }
        if let Some(r) = resume {
            args.extend(["--session".into(), r.to_string()]);
        }
        let spec = Spec { program: self.bin.clone(), args, cwd: config.cwd.clone(), env: config.env.clone(), env_remove: Vec::new() };
        let (proc_, stdin, stdout) = Proc::spawn(&spec, self.registry.as_ref()).map_err(|e| ProviderError::new(format!("cannot start pi: {e}")))?;
        let session = Arc::new(PiSession {
            stdin: tokio::sync::Mutex::new(stdin),
            proc_: Arc::new(tokio::sync::Mutex::new(proc_)),
            events: events.clone(),
            state: Arc::new(Mutex::new(State {
                session_id: resume.map(str::to_string).unwrap_or_default(),
                session_file: resume.map(str::to_string),
                model: config.model.clone(),
                thinking: config.thinking_option_id.clone(),
                turn_id: None,
                aborting: false,
                waiting: HashMap::new(),
                next_id: 1,
                tools: HashMap::new(),
                last_error: None,
            })),
        });
        session.clone().spawn_reader(stdout);
        let state = session.command("get_state", json!({})).await?;
        {
            let mut st = session.lock();
            if let Some(id) = state["sessionId"].as_str() {
                st.session_id = id.to_string();
            }
            if let Some(f) = state["sessionFile"].as_str() {
                st.session_file = Some(f.to_string());
            }
            if let Some(m) = state["model"]["id"].as_str() {
                st.model = Some(format!("{}/{}", state["model"]["provider"].as_str().unwrap_or(""), m).trim_start_matches('/').to_string());
            }
        }
        let id = session.lock().session_id.clone();
        let _ = events.send(StreamEvent::ThreadStarted { session_id: id, provider: ID.into() });
        Ok(session)
    }
}

#[async_trait]
impl Provider for PiProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: ID.into(),
            label: "Pi".into(),
            default_mode_id: Some("default".into()),
            modes: Vec::new(),
            models: Vec::new(),
            capabilities: Capabilities {
                supports_streaming: true,
                supports_session_persistence: true,
                supports_session_listing: Some(false),
                supports_dynamic_modes: false,
                supports_mcp_servers: true,
                supports_reasoning_stream: true,
                supports_tool_invocations: true,
                supports_rewind_conversation: false,
                supports_rewind_files: false,
                supports_rewind_both: false,
                extra: [("supportsSteer".to_string(), true)].into_iter().collect(),
            },
            echoes_user_message: false,
        }
    }

    async fn create_session(&self, config: SessionConfig, events: EventSink) -> Result<Arc<dyn Session>, ProviderError> {
        self.start(config, events, None).await
    }

    async fn resume_session(&self, handle: &PersistenceHandle, config: SessionConfig, events: EventSink) -> Result<Arc<dyn Session>, ProviderError> {
        let target = handle.native_handle.clone().unwrap_or_else(|| handle.session_id.clone());
        self.start(config, events, Some(&target)).await
    }
}

struct State {
    session_id: String,
    session_file: Option<String>,
    model: Option<String>,
    thinking: Option<String>,
    turn_id: Option<String>,
    aborting: bool,
    waiting: HashMap<String, oneshot::Sender<Value>>,
    next_id: u64,
    tools: HashMap<String, (String, Value)>,
    last_error: Option<String>,
}

pub struct PiSession {
    stdin: tokio::sync::Mutex<ChildStdin>,
    proc_: Arc<tokio::sync::Mutex<Proc>>,
    events: EventSink,
    state: Arc<Mutex<State>>,
}

fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

fn result_text(v: &Value) -> Option<String> {
    match v {
        Value::String(t) => Some(t.clone()),
        Value::Object(_) => v["content"]
            .as_array()
            .map(|c| c.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"))
            .or_else(|| s(&v["output"])),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// A Pi tool (`bash`, `read`, `edit`, `write`, ...) as a detail.
pub fn pi_detail(name: &str, args: &Value, result: Option<&Value>) -> ToolCallDetail {
    let out = result.and_then(result_text);
    match name {
        "bash" => ToolCallDetail::Shell { command: s(&args["command"]).unwrap_or_default(), cwd: None, output: out, exit_code: None },
        "read" => ToolCallDetail::Read { file_path: s(&args["path"]).unwrap_or_default(), content: None, offset: None, limit: None },
        "edit" => ToolCallDetail::Edit { file_path: s(&args["path"]).unwrap_or_default(), old_string: s(&args["oldText"]), new_string: s(&args["newText"]), unified_diff: None },
        "write" => ToolCallDetail::Write { file_path: s(&args["path"]).unwrap_or_default(), content: s(&args["content"]) },
        _ => ToolCallDetail::Unknown { input: args.clone(), output: result.cloned().unwrap_or(Value::Null) },
    }
}

impl PiSession {
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

    /// Send a command and wait for its `response`.
    async fn command(&self, kind: &str, extra: Value) -> Result<Value, ProviderError> {
        let (id, rx) = {
            let mut st = self.lock();
            let id = format!("lucina-{}", st.next_id);
            st.next_id += 1;
            let (tx, rx) = oneshot::channel();
            st.waiting.insert(id.clone(), tx);
            (id, rx)
        };
        let mut msg = json!({"id": id, "type": kind});
        if let Value::Object(m) = extra {
            for (k, v) in m {
                msg[k] = v;
            }
        }
        let mut line = msg.to_string();
        line.push('\n');
        {
            let mut w = self.stdin.lock().await;
            w.write_all(line.as_bytes()).await.map_err(|e| ProviderError::new(format!("pi stdin: {e}")))?;
            w.flush().await.map_err(|e| ProviderError::new(format!("pi stdin: {e}")))?;
        }
        let resp = match tokio::time::timeout(RESPONSE_TIMEOUT, rx).await {
            Ok(Ok(v)) => v,
            _ => {
                let tail = self.proc_.lock().await.stderr_tail();
                return Err(ProviderError::new(format!("pi {kind}: no response\n{tail}")));
            }
        };
        if resp["success"] == false {
            return Err(ProviderError::new(format!("pi {kind}: {}", resp["error"].as_str().unwrap_or("failed"))));
        }
        Ok(resp["data"].clone())
    }

    fn spawn_reader(self: Arc<Self>, stdout: ChildStdout) {
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(ev) = serde_json::from_str::<Value>(line.trim()) else { continue };
                self.on_event(&ev);
            }
            let (turn, waiting) = {
                let mut st = self.lock();
                (st.turn_id.take(), std::mem::take(&mut st.waiting))
            };
            drop(waiting);
            if let Some(turn) = turn {
                let tail = self.proc_.lock().await.stderr_tail();
                self.emit(StreamEvent::TurnFailed { provider: ID.into(), turn_id: Some(turn), error: "pi exited".into(), code: None, diagnostic: (!tail.is_empty()).then_some(tail) });
            }
        });
    }

    fn on_event(&self, ev: &Value) {
        match ev["type"].as_str() {
            Some("response") => {
                let waiter = ev["id"].as_str().and_then(|id| self.lock().waiting.remove(id));
                if let Some(w) = waiter {
                    let _ = w.send(ev.clone());
                }
            }
            Some("message_update") => {
                let d = &ev["assistantMessageEvent"];
                match d["type"].as_str() {
                    Some("text_delta") => self.timeline(TimelineItem::AssistantMessage { text: s(&d["delta"]).unwrap_or_default(), message_id: None }),
                    Some("thinking_delta") => self.timeline(TimelineItem::Reasoning { text: s(&d["delta"]).unwrap_or_default() }),
                    _ => {}
                }
            }
            Some("message_end") => {
                let m = &ev["message"];
                if m["role"] == "assistant" {
                    if let Some(err) = s(&m["errorMessage"]).filter(|e| !e.is_empty()) {
                        self.lock().last_error = Some(err);
                    }
                }
            }
            Some("tool_execution_start") => {
                let id = s(&ev["toolCallId"]).unwrap_or_default();
                let name = s(&ev["toolName"]).unwrap_or_default();
                let args = ev["args"].clone();
                self.timeline(TimelineItem::ToolCall(Box::new(ToolCall { agent_message: None, call_id: id.clone(), name: name.clone(), detail: pi_detail(&name, &args, None), metadata: None, status: ToolCallStatus::Running, error: Value::Null })));
                self.lock().tools.insert(id, (name, args));
            }
            Some("tool_execution_end") => {
                let id = s(&ev["toolCallId"]).unwrap_or_default();
                let (name, args) = self.lock().tools.remove(&id).unwrap_or_else(|| (s(&ev["toolName"]).unwrap_or_default(), Value::Null));
                let failed = ev["isError"] == true;
                let error = if failed { json!(result_text(&ev["result"]).unwrap_or_else(|| "failed".into())) } else { Value::Null };
                self.timeline(TimelineItem::ToolCall(Box::new(ToolCall {
                    agent_message: None,
                    call_id: id,
                    name: name.clone(),
                    detail: pi_detail(&name, &args, Some(&ev["result"])),
                    metadata: None,
                    status: if failed { ToolCallStatus::Failed } else { ToolCallStatus::Completed },
                    error,
                })));
            }
            Some("compaction_start") => self.timeline(TimelineItem::Compaction { status: CompactionStatus::Loading, trigger: None, pre_tokens: None }),
            Some("compaction_end") => self.timeline(TimelineItem::Compaction { status: CompactionStatus::Completed, trigger: None, pre_tokens: None }),
            Some("agent_end") if ev["willRetry"] != true => {
                let (turn, aborted, error) = {
                    let mut st = self.lock();
                    (st.turn_id.take(), std::mem::take(&mut st.aborting), st.last_error.take())
                };
                let ev = if aborted {
                    StreamEvent::TurnCanceled { provider: ID.into(), turn_id: turn, reason: "Interrupted".into() }
                } else if let Some(error) = error {
                    StreamEvent::TurnFailed { provider: ID.into(), turn_id: turn, error, code: None, diagnostic: None }
                } else {
                    StreamEvent::TurnCompleted { provider: ID.into(), turn_id: turn, usage: None }
                };
                self.emit(ev);
            }
            _ => {}
        }
    }
}

#[async_trait]
impl Session for PiSession {
    fn session_id(&self) -> String {
        self.lock().session_id.clone()
    }

    async fn start_turn(&self, prompt: &str, _client_message_id: Option<String>) -> Result<String, ProviderError> {
        let turn = uuid::Uuid::new_v4().to_string();
        {
            let mut st = self.lock();
            if st.turn_id.is_some() {
                return Err(ProviderError::new("pi already has a running turn"));
            }
            st.turn_id = Some(turn.clone());
            st.last_error = None;
        }
        self.emit(StreamEvent::TurnStarted { provider: ID.into(), turn_id: Some(turn.clone()) });
        if let Err(e) = self.command("prompt", json!({"message": prompt})).await {
            self.lock().turn_id = None;
            return Err(e);
        }
        Ok(turn)
    }

    async fn steer(&self, prompt: &str, expected_turn_id: &str) -> Result<bool, ProviderError> {
        if self.lock().turn_id.as_deref() != Some(expected_turn_id) {
            return Ok(false);
        }
        self.command("steer", json!({"message": prompt})).await.map(|_| true)
    }

    async fn interrupt(&self) -> Result<(), ProviderError> {
        if self.lock().turn_id.is_none() {
            return Ok(());
        }
        self.lock().aborting = true;
        self.command("abort", json!({})).await.map(|_| ())
    }

    async fn respond_permission(&self, _request_id: &str, _response: PermissionResponse) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn close(&self) {
        let _ = self.interrupt().await;
        self.proc_.lock().await.shutdown(Duration::from_secs(3)).await;
    }

    fn runtime_info(&self) -> RuntimeInfo {
        let st = self.lock();
        RuntimeInfo { provider: ID.into(), session_id: Some(st.session_id.clone()), model: st.model.clone(), thinking_option_id: st.thinking.clone(), mode_id: None, extra: None }
    }

    fn persistence(&self) -> PersistenceHandle {
        let st = self.lock();
        let mut meta = Map::new();
        if let Some(m) = &st.model {
            meta.insert("model".into(), json!(m));
        }
        PersistenceHandle { provider: ID.into(), session_id: st.session_id.clone(), native_handle: st.session_file.clone(), metadata: Some(meta) }
    }

    fn current_mode(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_details() {
        let d = pi_detail("bash", &json!({"command":"ls"}), Some(&json!({"content":[{"type":"text","text":"a"}]})));
        assert_eq!(serde_json::to_value(d).unwrap(), json!({"type":"shell","command":"ls","output":"a"}));
        assert_eq!(pi_detail("edit", &json!({"path":"/a","oldText":"x","newText":"y"}), None).kind(), "edit");
        assert_eq!(pi_detail("custom", &json!({}), None).kind(), "unknown");
    }
}
