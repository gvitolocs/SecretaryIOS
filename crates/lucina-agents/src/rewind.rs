//! Rewind: an agent goes back to an earlier user message (Paseo
//! `server/agent/rewind/rewind.js` and `AgentManager.rewind`).
//!
//! Three modes, each a capability a provider has or lacks (`supportsRewindConversation`,
//! `supportsRewindFiles`, `supportsRewindBoth`):
//!
//! - **conversation:** the provider's own conversation state goes back to just before the
//!   target message, and so does the timeline. Files stay as they are.
//! - **files:** the files the agent changed since that message go back to how they were
//!   when it was sent. The conversation stays.
//! - **both:** both, as one operation.
//!
//! The target is a user message of the agent's timeline, named by its `messageId` (or
//! the `clientMessageId` the sender gave it). Rewinding to a message removes that message
//! and everything after it, so the owner can send it again, edited.

use serde::{Deserialize, Serialize};

use crate::timeline::{TimelineItem, TimelineRow};
use crate::types::Capabilities;

/// Paseo `AgentRewindModeSchema`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RewindMode {
    Conversation,
    Files,
    Both,
}

impl RewindMode {
    pub const ALL: [RewindMode; 3] = [RewindMode::Conversation, RewindMode::Files, RewindMode::Both];

    pub fn as_str(self) -> &'static str {
        match self {
            RewindMode::Conversation => "conversation",
            RewindMode::Files => "files",
            RewindMode::Both => "both",
        }
    }

    pub fn parse(s: &str) -> Option<RewindMode> {
        RewindMode::ALL.into_iter().find(|m| m.as_str() == s)
    }

    /// The conversation (and so the timeline) goes back.
    pub fn changes_conversation(self) -> bool {
        self != RewindMode::Files
    }

    /// Files on disk change.
    pub fn changes_files(self) -> bool {
        self != RewindMode::Conversation
    }
}

/// Whether a provider's capability flags allow `mode` (Paseo `invokeRewindCapability`).
pub fn supports(capabilities: &Capabilities, mode: RewindMode) -> bool {
    match mode {
        RewindMode::Conversation => capabilities.supports_rewind_conversation,
        RewindMode::Files => capabilities.supports_rewind_files,
        RewindMode::Both => capabilities.supports_rewind_both,
    }
}

/// Paseo `RewindCapabilityError`'s message.
pub fn unsupported_message(mode: RewindMode) -> String {
    format!("Provider does not support rewinding {}", mode.as_str())
}

/// The user message a rewind goes back to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// Its row's sequence number.
    pub seq: u64,
    /// How many user messages come before it.
    pub user_turn: usize,
    /// How many user messages the timeline has in all.
    pub user_turns: usize,
}

impl Target {
    /// User messages from the target on (the turns a conversation rewind drops).
    pub fn dropped_turns(&self) -> usize {
        self.user_turns - self.user_turn
    }
}

/// The user message `message_id` names: its `messageId` or its `clientMessageId`.
pub fn find_target(rows: &[TimelineRow], message_id: &str) -> Option<Target> {
    let mut found: Option<(u64, usize)> = None;
    let mut turns = 0usize;
    for row in rows {
        if let TimelineItem::UserMessage { message_id: id, client_message_id, .. } = &row.item {
            if found.is_none() && (id.as_deref() == Some(message_id) || client_message_id.as_deref() == Some(message_id)) {
                found = Some((row.seq, turns));
            }
            turns += 1;
        }
    }
    found.map(|(seq, user_turn)| Target { seq, user_turn, user_turns: turns })
}

/// A conversation rewind's timeline: the rows before the target.
pub fn keep_before(rows: &mut Vec<TimelineRow>, target: &Target) {
    rows.retain(|r| r.seq < target.seq);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(seq: u64, id: Option<&str>, client: Option<&str>) -> TimelineRow {
        TimelineRow {
            seq,
            timestamp: String::new(),
            item: TimelineItem::UserMessage { text: format!("q{seq}"), message_id: id.map(Into::into), client_message_id: client.map(Into::into) },
            turn_id: None,
        }
    }

    fn assistant(seq: u64) -> TimelineRow {
        TimelineRow { seq, timestamp: String::new(), item: TimelineItem::AssistantMessage { text: "a".into(), message_id: Some("m".into()) }, turn_id: None }
    }

    #[test]
    fn modes_parse_and_say_what_they_change() {
        for m in RewindMode::ALL {
            assert_eq!(RewindMode::parse(m.as_str()), Some(m));
        }
        assert_eq!(RewindMode::parse("everything"), None);
        assert_eq!(serde_json::to_string(&RewindMode::Both).unwrap(), "\"both\"");
        assert!(RewindMode::Conversation.changes_conversation() && !RewindMode::Conversation.changes_files());
        assert!(!RewindMode::Files.changes_conversation() && RewindMode::Files.changes_files());
        assert!(RewindMode::Both.changes_conversation() && RewindMode::Both.changes_files());
    }

    #[test]
    fn capability_flags_gate_each_mode() {
        let none = Capabilities::stored();
        let only_files = Capabilities { supports_rewind_files: true, ..Capabilities::stored() };
        for m in RewindMode::ALL {
            assert!(!supports(&none, m));
            assert_eq!(supports(&only_files, m), m == RewindMode::Files);
        }
        assert_eq!(unsupported_message(RewindMode::Both), "Provider does not support rewinding both");
    }

    #[test]
    fn finds_user_messages_by_either_id() {
        let rows = vec![user(1, Some("u1"), None), assistant(2), user(3, Some("u2"), Some("c2")), assistant(4), user(5, None, None)];
        assert_eq!(find_target(&rows, "u1"), Some(Target { seq: 1, user_turn: 0, user_turns: 3 }));
        assert_eq!(find_target(&rows, "u2"), Some(Target { seq: 3, user_turn: 1, user_turns: 3 }));
        assert_eq!(find_target(&rows, "c2"), find_target(&rows, "u2"));
        assert_eq!(find_target(&rows, "m"), None, "an assistant message id is not a target");
        assert_eq!(find_target(&rows, "nope"), None);
        assert_eq!(find_target(&rows, "u2").unwrap().dropped_turns(), 2);
    }

    #[test]
    fn a_conversation_rewind_drops_the_target_and_what_follows() {
        let mut rows = vec![user(1, Some("u1"), None), assistant(2), user(3, Some("u2"), None), assistant(4)];
        let target = find_target(&rows, "u2").unwrap();
        keep_before(&mut rows, &target);
        assert_eq!(rows.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![1, 2]);
    }
}
