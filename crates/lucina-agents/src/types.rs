//! Agents: status, the stored record, the snapshot and the list item (Paseo
//! `agent-lifecycle.js`, `STORED_AGENT_SCHEMA`, `AgentSnapshotPayloadSchema`,
//! `AgentListItemPayloadSchema`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::events::Usage;
use crate::permission::PermissionRequest;

/// Paseo `AGENT_LIFECYCLE_STATUSES`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Initializing,
    Idle,
    Running,
    Error,
    Closed,
}

impl AgentStatus {
    pub const ALL: [AgentStatus; 5] = [
        AgentStatus::Initializing,
        AgentStatus::Idle,
        AgentStatus::Running,
        AgentStatus::Error,
        AgentStatus::Closed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AgentStatus::Initializing => "initializing",
            AgentStatus::Idle => "idle",
            AgentStatus::Running => "running",
            AgentStatus::Error => "error",
            AgentStatus::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|st| st.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttentionReason {
    Finished,
    Error,
    Permission,
}

/// Lucina's finished-run gate (design: "Finished-run gate"). Paseo has no gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateResult {
    pub verdict: GateVerdict,
    /// Refusals, the proof's failing output, the judge's reasons.
    #[serde(default)]
    pub details: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diffgate: Option<crate::run_gate::Check>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<crate::run_gate::Check>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<crate::run_gate::Judge>,
    /// The run changed the workspace's gate config (`.lucina/agents.toml`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tampered: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof_command: Option<String>,
    /// The commit (or `git stash create` snapshot) the diff is taken from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// 0 for the run itself, then one more per auto-fix follow-up.
    #[serde(default)]
    pub round: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

/// What a run asks of the gate (`POST /agents`: `verify`, `verifyHeavy`, `autoFix`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateRequest {
    /// The proof command; overrides `verify` in the workspace's `.lucina/agents.toml`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
    /// Run the proof through `verify-gate` (one at a time, memory-capped).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub heavy: bool,
    /// Follow-up rounds after a refusal (0 = none).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub auto_fix: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GateVerdict {
    /// The checks are still running.
    Running,
    Verified,
    Refused,
    Suspicious,
    Unverified,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_option_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_values: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_options: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_policy: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    pub provider: String,
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_option_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistenceHandle {
    pub provider: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

/// One agent as lucina-agentd stores it (Paseo `STORED_AGENT_SCHEMA` plus
/// Lucina's `idempotencyKey`, `epoch` and `gate`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRecord {
    pub id: String,
    pub provider: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_user_message_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    pub last_status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_mode_id: Option<String>,
    #[serde(default)]
    pub config: AgentConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_info: Option<RuntimeInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistence: Option<PersistenceHandle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_usage: Option<Usage>,
    #[serde(default)]
    pub requires_attention: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_reason: Option<AttentionReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_timestamp: Option<String>,
    #[serde(default)]
    pub internal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// The timeline epoch. Paseo's changes on every daemon start; Lucina's stays.
    pub epoch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_request: Option<GateRequest>,
    /// Lucina only: an agent another host runs (a Secretary Code session on the Mac),
    /// mirrored here; see [`crate::external`].
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub external: bool,
    /// The device that registered an external agent: only it posts its events and
    /// receives its commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_device_id: Option<String>,
    /// Where an external agent runs (`mac`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

impl AgentRecord {
    /// Paseo `shortId`: the first 7 characters of the id.
    pub fn short_id(&self) -> String {
        self.id.chars().take(7).collect()
    }
}

/// Paseo `AgentCapabilityFlagsSchema` (extra provider flags in `extra`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub supports_streaming: bool,
    pub supports_session_persistence: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_session_listing: Option<bool>,
    pub supports_dynamic_modes: bool,
    pub supports_mcp_servers: bool,
    pub supports_reasoning_stream: bool,
    pub supports_tool_invocations: bool,
    #[serde(default)]
    pub supports_rewind_conversation: bool,
    #[serde(default)]
    pub supports_rewind_files: bool,
    #[serde(default)]
    pub supports_rewind_both: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, bool>,
}

impl Capabilities {
    /// Paseo `STORED_AGENT_CAPABILITIES`: what a closed agent (no provider
    /// runtime loaded) reports.
    pub fn stored() -> Self {
        Capabilities {
            supports_streaming: false,
            supports_session_persistence: true,
            supports_session_listing: None,
            supports_dynamic_modes: false,
            supports_mcp_servers: false,
            supports_reasoning_stream: false,
            supports_tool_invocations: true,
            supports_rewind_conversation: false,
            supports_rewind_files: false,
            supports_rewind_both: false,
            extra: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMode {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_tier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveTurn {
    pub turn_id: String,
    pub started_at: Option<String>,
}

/// Paseo `AgentSnapshotPayloadSchema`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSnapshot {
    pub id: String,
    pub provider: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_option_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_thinking_option_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_user_message_at: Option<String>,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn: Option<ActiveTurn>,
    pub capabilities: Capabilities,
    pub current_mode_id: Option<String>,
    pub available_modes: Vec<AgentMode>,
    pub pending_permissions: Vec<PermissionRequest>,
    pub persistence: Option<PersistenceHandle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_info: Option<RuntimeInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_attention: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_reason: Option<AttentionReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_timestamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_unavailable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internal: Option<bool>,
    /// Lucina only: the finished-run gate's result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateResult>,
    /// Lucina only: an external agent ([`crate::external`]), its owner device and host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// Paseo `AgentListItemPayloadSchema`, plus Lucina's state bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentListItem {
    pub id: String,
    pub short_id: String,
    pub title: Option<String>,
    pub provider: String,
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_option_id: Option<String>,
    pub status: AgentStatus,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: String,
    pub last_user_message_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_attention: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_reason: Option<AttentionReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_timestamp: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_unavailable: Option<bool>,
    pub bucket: crate::bucket::Bucket,
    /// Lucina only, as in [`AgentSnapshot`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_device_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn status_wire_names() {
        for st in AgentStatus::ALL {
            assert_eq!(serde_json::to_value(st).unwrap(), json!(st.as_str()));
            assert_eq!(AgentStatus::parse(st.as_str()), Some(st));
        }
        assert_eq!(AgentStatus::parse("busy"), None);
    }

    #[test]
    fn capabilities_flatten_extra_flags() {
        let v = json!({"supportsStreaming":true,"supportsSessionPersistence":true,"supportsDynamicModes":false,
            "supportsMcpServers":false,"supportsReasoningStream":true,"supportsToolInvocations":true,
            "supportsRewindConversation":true,"supportsRewindFiles":false,"supportsRewindBoth":false,"supportsSteer":true});
        let caps: Capabilities = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(caps.extra.get("supportsSteer"), Some(&true));
        assert_eq!(serde_json::to_value(&caps).unwrap(), v);
    }
}
