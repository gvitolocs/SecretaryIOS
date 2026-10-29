//! Provider subagents: the children a provider runs inside an agent (a Claude `Task`,
//! an OpenCode child session), each with a timeline of its own (Paseo
//! `server/agent/provider-subagents/store.js`, `@getpaseo/protocol`
//! `ProviderSubagent*` messages).
//!
//! A provider reports a child with three events: an `upsert` of its descriptor, items
//! for its timeline, and a `remove`. The parent's own timeline keeps only the
//! provider's card for the call (a `sub_agent` tool call whose `log` lists what the child
//! did), so `lagent logs` of the parent stays short. The child's items are read through
//! the subagent routes.
//!
//! Descriptors follow Paseo's sticky rule: a field an `upsert` omits keeps its stored
//! value. (Paseo also lets an explicit `null` clear a field; no provider here needs it,
//! so a field, once set, is only replaced.)

use serde::{Deserialize, Serialize};

use crate::timeline::TimelineItem;

/// Most rows one child's timeline keeps; later items are dropped.
pub const MAX_ROWS: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentStatus {
    Running,
    Completed,
    Failed,
    Canceled,
}

impl SubagentStatus {
    pub fn is_terminal(self) -> bool {
        self != SubagentStatus::Running
    }
}

/// One child (Paseo `ProviderSubagentDescriptorPayloadSchema`). `id` is the child's own
/// id: the `Task` tool call id for Claude, the child session id for OpenCode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subagent {
    pub id: String,
    pub parent_agent_id: String,
    /// The child that started this one, when it is not a direct child of the agent.
    #[serde(default)]
    pub parent_subagent_id: Option<String>,
    pub provider: String,
    /// The subagent's name or type (`Explore`).
    pub title: Option<String>,
    /// What it was asked to do.
    pub description: Option<String>,
    pub status: SubagentStatus,
    pub created_at: String,
    pub updated_at: String,
    /// The parent timeline's tool call that started it.
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    /// Provider-formatted context (`Explore · Opus 5.5 · 1.2k tokens`).
    #[serde(default)]
    pub subtitle: Option<String>,
}

/// What a provider reports about a child (Paseo's provider-subagent event).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SubagentEvent {
    Upsert {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<SubagentStatus>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subtitle: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_subagent_id: Option<String>,
    },
    Timeline {
        id: String,
        item: TimelineItem,
    },
    Remove {
        id: String,
    },
}

impl SubagentEvent {
    pub fn id(&self) -> &str {
        match self {
            SubagentEvent::Upsert { id, .. } | SubagentEvent::Timeline { id, .. } | SubagentEvent::Remove { id } => id,
        }
    }

    /// An `upsert` that changes only the status.
    pub fn status(id: &str, status: SubagentStatus) -> Self {
        SubagentEvent::Upsert {
            id: id.into(),
            title: None,
            description: None,
            status: Some(status),
            tool_call_id: None,
            cwd: None,
            subtitle: None,
            parent_subagent_id: None,
        }
    }
}

/// Paseo `ProviderSubagentStore.apply` for an `upsert`: the descriptor after the event.
/// `None` for the other event kinds.
pub fn apply_upsert(previous: Option<&Subagent>, parent_agent_id: &str, provider: &str, event: &SubagentEvent, now: &str) -> Option<Subagent> {
    let SubagentEvent::Upsert { id, title, description, status, tool_call_id, cwd, subtitle, parent_subagent_id } = event else { return None };
    let sticky = |next: &Option<String>, old: Option<&Option<String>>| next.clone().or_else(|| old.and_then(Clone::clone));
    Some(Subagent {
        id: id.clone(),
        parent_agent_id: parent_agent_id.into(),
        parent_subagent_id: sticky(parent_subagent_id, previous.map(|p| &p.parent_subagent_id)),
        provider: provider.into(),
        title: sticky(title, previous.map(|p| &p.title)),
        description: sticky(description, previous.map(|p| &p.description)),
        status: status.or(previous.map(|p| p.status)).unwrap_or(SubagentStatus::Running),
        created_at: previous.map(|p| p.created_at.clone()).unwrap_or_else(|| now.into()),
        updated_at: now.into(),
        tool_call_id: sticky(tool_call_id, previous.map(|p| &p.tool_call_id)),
        cwd: sticky(cwd, previous.map(|p| &p.cwd)),
        subtitle: sticky(subtitle, previous.map(|p| &p.subtitle)),
    })
}

/// What the event bus carries for a subagent (Paseo `agent.provider_subagents.update`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum SubagentUpdate {
    Upsert {
        subagent: Subagent,
    },
    Timeline {
        parent_agent_id: String,
        subagent_id: String,
        provider: String,
        item: TimelineItem,
        timestamp: String,
        seq: u64,
        epoch: String,
    },
    Remove {
        parent_agent_id: String,
        subagent_id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn upsert(v: serde_json::Value) -> SubagentEvent {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn events_round_trip() {
        for v in [
            json!({"type":"upsert","id":"t1","title":"Explore","description":"look","status":"running","toolCallId":"t1","parentSubagentId":"t0"}),
            json!({"type":"upsert","id":"t1","status":"canceled"}),
            json!({"type":"timeline","id":"t1","item":{"type":"assistant_message","text":"hi"}}),
            json!({"type":"remove","id":"t1"}),
        ] {
            let e: SubagentEvent = serde_json::from_value(v.clone()).unwrap();
            assert_eq!(serde_json::to_value(&e).unwrap(), v);
        }
    }

    #[test]
    fn upsert_is_sticky_and_keeps_created_at() {
        let first = apply_upsert(None, "p", "claude", &upsert(json!({"type":"upsert","id":"t1","title":"Explore","description":"look","toolCallId":"t1"})), "T0").unwrap();
        assert_eq!(first.status, SubagentStatus::Running);
        assert_eq!((first.created_at.as_str(), first.updated_at.as_str()), ("T0", "T0"));
        let done = apply_upsert(Some(&first), "p", "claude", &SubagentEvent::status("t1", SubagentStatus::Completed), "T1").unwrap();
        assert_eq!(done.status, SubagentStatus::Completed);
        assert_eq!(done.title.as_deref(), Some("Explore"));
        assert_eq!(done.description.as_deref(), Some("look"));
        assert_eq!(done.tool_call_id.as_deref(), Some("t1"));
        assert_eq!((done.created_at.as_str(), done.updated_at.as_str()), ("T0", "T1"));
        let renamed = apply_upsert(Some(&done), "p", "claude", &upsert(json!({"type":"upsert","id":"t1","title":"Plan","subtitle":"Plan · 1.2k tokens"})), "T2").unwrap();
        assert_eq!(renamed.title.as_deref(), Some("Plan"));
        assert_eq!(renamed.status, SubagentStatus::Completed);
        assert_eq!(renamed.subtitle.as_deref(), Some("Plan · 1.2k tokens"));
    }

    #[test]
    fn descriptor_wire_shape() {
        let s = apply_upsert(None, "p", "claude", &upsert(json!({"type":"upsert","id":"t1","title":"Explore"})), "T0").unwrap();
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            json!({"id":"t1","parentAgentId":"p","parentSubagentId":null,"provider":"claude","title":"Explore","description":null,
                   "status":"running","createdAt":"T0","updatedAt":"T0","toolCallId":null,"cwd":null,"subtitle":null})
        );
        assert!(apply_upsert(None, "p", "claude", &SubagentEvent::Remove { id: "t1".into() }, "T0").is_none());
        assert!(SubagentStatus::Failed.is_terminal() && !SubagentStatus::Running.is_terminal());
    }
}
