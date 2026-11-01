//! External agents: agents another host runs, mirrored in Lucina (Lucina only; design
//! "One protocol and one UI for agents and coding sessions").
//!
//! A Secretary Code session on the Mac registers itself with `POST /v1/agents`
//! (`provider: "secretary-code"`, `external: true`, `host: "mac"`). The device that
//! registers it becomes its owner:
//!
//! - only the owner posts its events (`POST /v1/agents/{id}/events`, [`EventsBody`]);
//! - sends, steers, stops and permission answers from any of the owner's devices (or
//!   `lagent`) become [`AgentCommand`]s, queued for the owner until it acknowledges
//!   them (`POST /v1/agents/{id}/commands/{commandId}/ack`);
//! - a command not acknowledged within [`COMMAND_TTL_MS`] expires; an expired
//!   permission answer counts as denied.
//!
//! The JSON matches Secretary Code's mirror (`packages/desktop/src/main/secretary/agents/wire.ts`).

use serde::{Deserialize, Serialize};

use crate::events::StreamEvent;
use crate::permission::PermissionResponse;

/// The provider id of Secretary Code sessions.
pub const PROVIDER: &str = "secretary-code";

/// Hosts an external agent may name.
pub const HOSTS: [&str; 1] = ["mac"];

/// Unacknowledged commands expire after 24 h.
pub const COMMAND_TTL_MS: u64 = 24 * 60 * 60 * 1000;

/// At most this many events in one post.
pub const MAX_EVENTS_PER_POST: usize = 200;

/// What the owner's other devices ask the device running the agent to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum CommandBody {
    Send { prompt: String },
    Steer { prompt: String },
    Stop {},
    PermissionResponse { request_id: String, response: PermissionResponse },
}

/// One queued command, as the stream (`type: "agent_command"`) and
/// `GET /v1/agents/commands` carry it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCommand {
    pub command_id: String,
    pub agent_id: String,
    pub command: CommandBody,
    /// ISO 8601.
    pub created_at: String,
    pub expires_at: String,
}

/// `POST /v1/agents/{id}/events`.
#[derive(Debug, Clone, Deserialize)]
pub struct EventsBody {
    pub events: Vec<StreamEvent>,
}

/// Why a device may not act on an external agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The agent is not external: its provider runs here.
    NotExternal,
    /// The caller is not a device (MCP, a service, `lagent`, an agent's process).
    NotADevice,
    /// Another device registered it.
    NotTheOwner,
}

/// Whether `caller_device` may post events for, or acknowledge commands of, an agent
/// (`external`, `owner`). Only the registering device may.
pub fn owner_check(external: bool, owner: Option<&str>, caller_device: Option<&str>) -> Result<(), Refusal> {
    if !external {
        return Err(Refusal::NotExternal);
    }
    let Some(caller) = caller_device.filter(|d| !d.is_empty()) else {
        return Err(Refusal::NotADevice);
    };
    match owner {
        Some(o) if o == caller => Ok(()),
        _ => Err(Refusal::NotTheOwner),
    }
}

/// The events an owner may post. `attention_required` is the daemon's own (it derives
/// it from the lifecycle), and `usage_updated` passes.
pub fn postable(event: &StreamEvent) -> bool {
    !matches!(event, StreamEvent::AttentionRequired { .. })
}

/// A valid device id in `X-Lucina-Origin: device:<id>` (what secretary-server sends:
/// its device ids are short ASCII tokens).
pub fn valid_device_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_json_matches_the_mirror() {
        let c = AgentCommand {
            command_id: "cmd_1".into(),
            agent_id: "a1".into(),
            command: CommandBody::PermissionResponse {
                request_id: "r1".into(),
                response: PermissionResponse::Deny { selected_action_id: None, message: Some("no".into()), interrupt: None },
            },
            created_at: "2026-10-28T00:00:00.000Z".into(),
            expires_at: "2026-10-29T00:00:00.000Z".into(),
        };
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!({"commandId": "cmd_1", "agentId": "a1",
                "command": {"kind": "permission_response", "requestId": "r1", "response": {"behavior": "deny", "message": "no"}},
                "createdAt": "2026-10-28T00:00:00.000Z", "expiresAt": "2026-10-29T00:00:00.000Z"})
        );
        for (body, v) in [
            (CommandBody::Send { prompt: "hi".into() }, json!({"kind": "send", "prompt": "hi"})),
            (CommandBody::Steer { prompt: "hi".into() }, json!({"kind": "steer", "prompt": "hi"})),
            (CommandBody::Stop {}, json!({"kind": "stop"})),
        ] {
            assert_eq!(serde_json::to_value(&body).unwrap(), v);
            assert_eq!(serde_json::from_value::<CommandBody>(v).unwrap(), body);
        }
    }

    #[test]
    fn only_the_registering_device_owns_it() {
        assert_eq!(owner_check(true, Some("dev_a"), Some("dev_a")), Ok(()));
        assert_eq!(owner_check(true, Some("dev_a"), Some("dev_b")), Err(Refusal::NotTheOwner));
        assert_eq!(owner_check(true, Some("dev_a"), None), Err(Refusal::NotADevice));
        assert_eq!(owner_check(true, Some("dev_a"), Some("")), Err(Refusal::NotADevice));
        assert_eq!(owner_check(true, None, Some("dev_a")), Err(Refusal::NotTheOwner));
        assert_eq!(owner_check(false, Some("dev_a"), Some("dev_a")), Err(Refusal::NotExternal));
    }

    #[test]
    fn events_the_owner_may_post() {
        let ev: StreamEvent = serde_json::from_value(json!({"type": "turn_started", "provider": PROVIDER})).unwrap();
        assert!(postable(&ev));
        let attention: StreamEvent = serde_json::from_value(json!({"type": "attention_required", "provider": PROVIDER,
            "reason": "finished", "timestamp": "t", "shouldNotify": true})).unwrap();
        assert!(!postable(&attention));
    }

    #[test]
    fn device_ids() {
        assert!(valid_device_id("dev_0f3a-9"));
        for bad in ["", "a b", "a/b", "a:b", &"x".repeat(65)] {
            assert!(!valid_device_id(bad), "{bad}");
        }
    }
}
