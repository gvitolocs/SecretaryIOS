//! The provider interface. A provider starts or resumes sessions; a session runs
//! turns and reports what happens as [`StreamEvent`]s on the channel it was given.
//! (Paseo: `AgentClient` / `AgentSession` in `server/agent/agent-sdk-types`.)

use std::sync::Arc;

use async_trait::async_trait;
use lucina_agents::types::{AgentMode, Capabilities, PersistenceHandle, RuntimeInfo};
use lucina_agents::{PermissionResponse, StreamEvent};
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::sync::mpsc::UnboundedSender;

pub type EventSink = UnboundedSender<StreamEvent>;

#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct ProviderError(pub String);

impl ProviderError {
    pub fn new(msg: impl Into<String>) -> Self {
        ProviderError(msg.into())
    }
}

/// What a session is started with.
#[derive(Debug, Clone, Default)]
pub struct SessionConfig {
    pub cwd: String,
    pub model: Option<String>,
    pub mode_id: Option<String>,
    pub thinking_option_id: Option<String>,
    pub feature_values: Map<String, Value>,
    pub env: Vec<(String, String)>,
}

/// One model a provider offers (Paseo `AgentModelDefinitionSchema`, the fields
/// lucina-agentd uses).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub is_default: bool,
    pub is_selectable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_thinking_option_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
    pub default_mode_id: Option<String>,
    pub modes: Vec<AgentMode>,
    pub models: Vec<ModelInfo>,
    pub capabilities: Capabilities,
    /// The provider puts the user's prompt on the timeline itself (Paseo's mock
    /// does); otherwise the manager records it.
    pub echoes_user_message: bool,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn info(&self) -> ProviderInfo;
    async fn create_session(&self, config: SessionConfig, events: EventSink) -> Result<Arc<dyn Session>, ProviderError>;
    async fn resume_session(
        &self,
        handle: &PersistenceHandle,
        config: SessionConfig,
        events: EventSink,
    ) -> Result<Arc<dyn Session>, ProviderError>;
}

#[async_trait]
pub trait Session: Send + Sync {
    fn session_id(&self) -> String;
    /// Start a turn; returns its id. Events follow on the sink.
    async fn start_turn(&self, prompt: &str, client_message_id: Option<String>) -> Result<String, ProviderError>;
    /// Steer the running turn; `false` when there's no such turn.
    async fn steer(&self, prompt: &str, expected_turn_id: &str) -> Result<bool, ProviderError>;
    /// Cancel the running turn (emits `turn_canceled`); a no-op when idle.
    async fn interrupt(&self) -> Result<(), ProviderError>;
    async fn respond_permission(&self, request_id: &str, response: PermissionResponse) -> Result<(), ProviderError>;
    /// A prompt the provider answers without a turn (Paseo `tryHandleOutOfBand`).
    fn out_of_band(&self, _prompt: &str) -> Option<Vec<StreamEvent>> {
        None
    }
    async fn close(&self);
    fn runtime_info(&self) -> RuntimeInfo;
    fn persistence(&self) -> PersistenceHandle;
    fn current_mode(&self) -> Option<String>;
}
