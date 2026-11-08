//! ZCode (GLM) through its headless CLI: `node zcode.cjs --cwd <dir> --json
//! --no-color -p <prompt> [--resume sess_…]`, one process per turn (the same
//! invocation as lane's `zcode` lane). The CLI has no streaming JSON, so a turn's
//! answer arrives in one piece when the process ends. Its last JSON object holds
//! `response`, `sessionId` and `error`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use lucina_agents::types::{Capabilities, PersistenceHandle, RuntimeInfo};
use lucina_agents::{PermissionResponse, StreamEvent, TimelineItem};
use serde_json::Value;
use tokio::io::AsyncReadExt;

use crate::process::{Proc, Registry, Spec};
use crate::provider::{EventSink, Provider, ProviderError, ProviderInfo, Session, SessionConfig};

pub const ID: &str = "zcode";

/// The ZCode CLI bundle dir (the lane's `dir`, or the AppImage default).
pub fn find_dir(lane_dir: Option<&str>) -> Option<PathBuf> {
    let raw = lane_dir.unwrap_or("~/opt/zcode-appimage/resources/glm");
    let dir = match raw.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").ok()?).join(rest),
        None => PathBuf::from(raw),
    };
    dir.join("zcode.cjs").is_file().then_some(dir)
}

pub struct ZcodeProvider {
    pub node: PathBuf,
    pub dir: PathBuf,
    pub registry: Option<Registry>,
}

#[async_trait]
impl Provider for ZcodeProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: ID.into(),
            label: "ZCode".into(),
            default_mode_id: None,
            modes: Vec::new(),
            models: Vec::new(),
            capabilities: Capabilities {
                supports_streaming: false,
                supports_session_persistence: true,
                supports_session_listing: Some(false),
                supports_dynamic_modes: false,
                supports_mcp_servers: false,
                supports_reasoning_stream: false,
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
        Ok(Arc::new(ZcodeSession::new(self, config, events, None)))
    }

    async fn resume_session(&self, handle: &PersistenceHandle, config: SessionConfig, events: EventSink) -> Result<Arc<dyn Session>, ProviderError> {
        Ok(Arc::new(ZcodeSession::new(self, config, events, Some(handle.session_id.clone()))))
    }
}

struct State {
    session_id: Option<String>,
    turn: Option<(String, Arc<tokio::sync::Mutex<Option<Proc>>>)>,
    canceled: bool,
}

pub struct ZcodeSession {
    node: PathBuf,
    dir: PathBuf,
    cwd: String,
    env: Vec<(String, String)>,
    registry: Option<Registry>,
    events: EventSink,
    state: Arc<Mutex<State>>,
}

/// The last top-level JSON object in the CLI's stdout.
pub fn last_json(stdout: &str) -> Option<Value> {
    stdout
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .find(Value::is_object)
        .or_else(|| serde_json::from_str::<Value>(stdout.trim()).ok().filter(Value::is_object))
}

impl ZcodeSession {
    fn new(p: &ZcodeProvider, config: SessionConfig, events: EventSink, session_id: Option<String>) -> Self {
        ZcodeSession {
            node: p.node.clone(),
            dir: p.dir.clone(),
            cwd: config.cwd,
            env: config.env,
            registry: p.registry.clone(),
            events,
            state: Arc::new(Mutex::new(State { session_id, turn: None, canceled: false })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[async_trait]
impl Session for ZcodeSession {
    fn session_id(&self) -> String {
        self.lock().session_id.clone().unwrap_or_default()
    }

    async fn start_turn(&self, prompt: &str, _client_message_id: Option<String>) -> Result<String, ProviderError> {
        let mut args = vec!["zcode.cjs".to_string(), "--cwd".into(), self.cwd.clone(), "--json".into(), "--no-color".into(), "-p".into(), prompt.to_string()];
        if let Some(sid) = self.lock().session_id.clone() {
            args.extend(["--resume".into(), sid]);
        }
        let spec = Spec { program: self.node.clone(), args, cwd: self.dir.display().to_string(), env: self.env.clone(), env_remove: Vec::new() };
        let (proc_, stdin, mut stdout) = Proc::spawn(&spec, self.registry.as_ref()).map_err(|e| ProviderError::new(format!("cannot start zcode: {e}")))?;
        drop(stdin);
        let turn = uuid::Uuid::new_v4().to_string();
        let slot = Arc::new(tokio::sync::Mutex::new(Some(proc_)));
        {
            let mut st = self.lock();
            if st.turn.is_some() {
                return Err(ProviderError::new("zcode already has a running turn"));
            }
            st.turn = Some((turn.clone(), slot.clone()));
            st.canceled = false;
        }
        let _ = self.events.send(StreamEvent::TurnStarted { provider: ID.into(), turn_id: Some(turn.clone()) });
        let events = self.events.clone();
        let state = self.state.clone();
        let turn_id = turn.clone();
        tokio::spawn(async move {
            let mut out = String::new();
            let _ = stdout.read_to_string(&mut out).await;
            let (code, tail) = {
                let mut guard = slot.lock().await;
                let mut code = None;
                let mut tail = String::new();
                if let Some(p) = guard.as_mut() {
                    for _ in 0..50 {
                        if let Some(c) = p.try_exit() {
                            code = c;
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    tail = p.stderr_tail();
                    p.shutdown(Duration::from_secs(1)).await;
                }
                (code, tail)
            };
            let canceled = {
                let mut st = state.lock().unwrap_or_else(|p| p.into_inner());
                st.turn = None;
                std::mem::take(&mut st.canceled)
            };
            let v = last_json(&out).unwrap_or(Value::Null);
            if let Some(sid) = v["sessionId"].as_str() {
                state.lock().unwrap_or_else(|p| p.into_inner()).session_id = Some(sid.to_string());
                let _ = events.send(StreamEvent::ThreadStarted { session_id: sid.to_string(), provider: ID.into() });
            }
            let response = v["response"].as_str().map(str::to_string);
            let ev = if canceled {
                StreamEvent::TurnCanceled { provider: ID.into(), turn_id: Some(turn_id), reason: "Interrupted".into() }
            } else if let (Some(text), true) = (response, v["error"].is_null()) {
                let _ = events.send(StreamEvent::Timeline { provider: ID.into(), item: TimelineItem::AssistantMessage { text, message_id: None }, turn_id: Some(turn_id.clone()) });
                StreamEvent::TurnCompleted { provider: ID.into(), turn_id: Some(turn_id), usage: None }
            } else {
                let error = v["error"].as_str().map(str::to_string).or_else(|| (!v["error"].is_null()).then(|| v["error"].to_string())).unwrap_or_else(|| format!("zcode exited with {code:?}"));
                StreamEvent::TurnFailed { provider: ID.into(), turn_id: Some(turn_id), error, code: code.map(|c| c.to_string()), diagnostic: (!tail.is_empty()).then_some(tail) }
            };
            let _ = events.send(ev);
        });
        Ok(turn)
    }

    async fn steer(&self, _prompt: &str, _expected_turn_id: &str) -> Result<bool, ProviderError> {
        Ok(false)
    }

    async fn interrupt(&self) -> Result<(), ProviderError> {
        let slot = {
            let mut st = self.lock();
            st.canceled = st.turn.is_some();
            st.turn.as_ref().map(|(_, s)| s.clone())
        };
        if let Some(slot) = slot {
            if let Some(p) = slot.lock().await.as_mut() {
                p.shutdown(Duration::from_secs(2)).await;
            }
        }
        Ok(())
    }

    async fn respond_permission(&self, _request_id: &str, _response: PermissionResponse) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn close(&self) {
        let _ = self.interrupt().await;
    }

    fn runtime_info(&self) -> RuntimeInfo {
        RuntimeInfo { provider: ID.into(), session_id: self.lock().session_id.clone(), model: Some("glm-5.3-flash".into()), thinking_option_id: None, mode_id: None, extra: None }
    }

    fn persistence(&self) -> PersistenceHandle {
        PersistenceHandle { provider: ID.into(), session_id: self.lock().session_id.clone().unwrap_or_default(), native_handle: None, metadata: None }
    }

    fn current_mode(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_json_object() {
        let out = "progress...\n{\"response\":\"ok\",\"sessionId\":\"sess_1\",\"error\":null}\n";
        let v = last_json(out).unwrap();
        assert_eq!(v["sessionId"], "sess_1");
        assert!(last_json("no json").is_none());
    }
}
