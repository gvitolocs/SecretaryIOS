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

// -- Claude sessions ---------------------------------------------------------------------

/// The provider id of Claude Code sessions working in a Lucina workspace: Claude desktop chats
/// on the Mac and Claude CLI runs on nezopt. agentd runs nothing for them: a hook reports what
/// they do ([`SessionBeat`], design `docs/design/agents-pipeline.md` "Claude sessions").
pub const SESSION_PROVIDER: &str = "claude-session";

/// Where a session may run.
pub const SESSION_HOSTS: [&str; 2] = ["mac", "nezopt"];

/// A session silent for this long has ended (`LUCINA_AGENTD_SESSION_TTL_SECS` changes it). The
/// hook beats every 30 s to 2 min while the session is idle, so this means the session's process
/// or its host is gone. The next beat brings it back.
pub const SESSION_TTL_SECS: u64 = 30 * 60;

/// An ended session leaves the lists (archived) after this long.
pub const SESSION_ARCHIVE_AFTER_SECS: u64 = 24 * 60 * 60;

/// A session's text kept on its timeline, in characters.
pub const SESSION_TEXT_MAX: usize = 8000;

pub fn is_session(provider: &str) -> bool {
    provider == SESSION_PROVIDER
}

/// The queue's owner for a session's commands. `:` can't be in a device id
/// ([`valid_device_id`]), so no device can read or acknowledge them.
pub fn session_owner(session_id: &str) -> String {
    format!("session:{session_id}")
}

/// Which session a beat is for: a Claude id (`local_…` or a UUID). `lagent` falls back to
/// `user@host` when it can't name the session, which is no session at all.
pub fn valid_session_id(id: &str) -> bool {
    valid_device_id(id)
}

/// The host a session runs on when nothing said so: desktop sessions (`local_…`) run on the Mac.
pub fn default_session_host(session_id: &str) -> &'static str {
    if session_id.starts_with("local_") {
        "mac"
    } else {
        "nezopt"
    }
}

/// What a session just did, from its hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionEvent {
    /// It started or resumed (`SessionStart`).
    Start,
    /// The user sent a prompt (`UserPromptSubmit`).
    Prompt,
    /// A tool finished (`PostToolUse`).
    Tool,
    /// The turn ended (`Stop`).
    Stop,
    /// It is waiting for its next turn, and alive (the `Stop` hook's loop).
    Poll,
    /// It ended (`SessionEnd`).
    End,
    /// `lagent session register`, `lagent task start`: it took on a workspace.
    Link,
}

/// `POST /sessions/beat`: one hook call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBeat {
    pub session_id: String,
    pub event: SessionEvent,
    /// `mac` or `nezopt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The session's name in its app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Paths on this host that show which worktree it works in (its cwd, files it edited).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Workspace ids it named (`lagent task start`'s answer).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<String>,
    /// [`SessionEvent::Prompt`]: what the user wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// [`SessionEvent::Stop`]: the turn's last message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Report only: a subagent's hook has nowhere to deliver a message to.
    #[serde(default)]
    pub no_commands: bool,
    /// Track it even without a workspace (`lagent session register`).
    #[serde(default)]
    pub register: bool,
}

/// What a beat answers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeatReply {
    /// `false`: the session works in no workspace, so agentd keeps nothing about it.
    pub tracked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspaces: Vec<String>,
    /// Messages waiting for the session. The hook delivers them, then acknowledges them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<AgentCommand>,
}

/// `POST /sessions/{id}/ack`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAck {
    pub command_ids: Vec<String>,
}

/// Whether a session last heard from `last_seen_ms` has gone silent.
pub fn session_silent(now_ms: u64, last_seen_ms: u64, ttl_secs: u64) -> bool {
    now_ms.saturating_sub(last_seen_ms) >= ttl_secs.saturating_mul(1000)
}

/// One line of up to [`SESSION_TEXT_MAX`] characters per message, cut on a character boundary.
pub fn clip_session_text(text: &str) -> String {
    let text = text.trim();
    match text.char_indices().nth(SESSION_TEXT_MAX) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_session_owner_can_never_be_a_device() {
        let owner = session_owner("local_eef97e35-6581-4588-9b25-56c3b4c9c936");
        assert!(!valid_device_id(&owner));
        assert!(valid_session_id("local_eef97e35-6581-4588-9b25-56c3b4c9c936"));
        assert!(valid_session_id("fe5b42cf-182a-4727-a036-91538adafec7"));
        assert!(!valid_session_id("nez@nez_opt"), "lagent's fallback names no session");
        assert_eq!(default_session_host("local_x"), "mac");
        assert_eq!(default_session_host("fe5b42cf"), "nezopt");
    }

    #[test]
    fn beats_read_what_the_hook_writes() {
        let b: SessionBeat = serde_json::from_value(json!({"sessionId": "s1", "event": "tool", "paths": ["/x"], "noCommands": true})).unwrap();
        assert_eq!(b.event, SessionEvent::Tool);
        assert_eq!(b.paths, ["/x"]);
        assert!(b.no_commands && !b.register);
        assert_eq!(serde_json::to_value(BeatReply::default()).unwrap(), json!({"tracked": false}));
    }

    #[test]
    fn silence_and_clipping() {
        assert!(!session_silent(1_000, 500, 1));
        assert!(session_silent(1_500, 500, 1));
        assert!(!session_silent(0, 500, 1), "a clock going back is not silence");
        assert_eq!(clip_session_text("  hi \n"), "hi");
        let long = "é".repeat(SESSION_TEXT_MAX + 5);
        assert_eq!(clip_session_text(&long).chars().count(), SESSION_TEXT_MAX + 1);
    }

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
