//! Provider stream events (Paseo `AgentStreamEventPayloadSchema`). A provider adapter
//! turns its native protocol into these; the daemon records `timeline` events and
//! drives the [`crate::lifecycle`] kernel with the others.

use serde::{Deserialize, Serialize};
use serde_json::Number;

use crate::permission::{PermissionRequest, PermissionResponse};
use crate::timeline::TimelineItem;
use crate::types::AttentionReason;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_max_tokens: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_used_tokens: Option<Number>,
}

impl Usage {
    /// `{ ...lastUsage, ...usage }`: fields present in `newer` win.
    pub fn merged(&self, newer: &Usage) -> Usage {
        fn pick(old: &Option<Number>, new: &Option<Number>) -> Option<Number> {
            new.clone().or_else(|| old.clone())
        }
        Usage {
            input_tokens: pick(&self.input_tokens, &newer.input_tokens),
            cached_input_tokens: pick(&self.cached_input_tokens, &newer.cached_input_tokens),
            output_tokens: pick(&self.output_tokens, &newer.output_tokens),
            total_cost_usd: pick(&self.total_cost_usd, &newer.total_cost_usd),
            context_window_max_tokens: pick(
                &self.context_window_max_tokens,
                &newer.context_window_max_tokens,
            ),
            context_window_used_tokens: pick(
                &self.context_window_used_tokens,
                &newer.context_window_used_tokens,
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum StreamEvent {
    ThreadStarted {
        session_id: String,
        provider: String,
    },
    TurnStarted {
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
    },
    TurnCompleted {
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
    },
    TurnFailed {
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diagnostic: Option<String>,
    },
    TurnCanceled {
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        reason: String,
    },
    Timeline {
        provider: String,
        item: TimelineItem,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
    },
    PermissionRequested {
        provider: String,
        request: Box<PermissionRequest>,
    },
    PermissionResolved {
        provider: String,
        request_id: String,
        resolution: PermissionResponse,
    },
    /// Provider-internal (Paseo's mock and adapters emit it; the manager merges it
    /// into `lastUsage` and does not forward it to clients).
    UsageUpdated {
        provider: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        usage: Usage,
    },
    AttentionRequired {
        provider: String,
        reason: AttentionReason,
        timestamp: String,
        should_notify: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notification: Option<AttentionNotification>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AttentionNotification {
    pub title: String,
    pub body: String,
    pub data: AttentionNotificationData,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttentionNotificationData {
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub agent_id: String,
    pub reason: AttentionReason,
}

/// Paseo `formatTurnFailedMessage`: the error (or "Provider run failed"), then
/// `code: <code>`, then the diagnostic when it differs from the error, separated
/// by blank lines.
pub fn format_turn_failed(error: &str, code: Option<&str>, diagnostic: Option<&str>) -> String {
    let base = crate::jsstr::trim(error);
    let mut parts = vec![if base.is_empty() {
        "Provider run failed".to_string()
    } else {
        base.to_string()
    }];
    if let Some(code) = code.map(crate::jsstr::trim).filter(|c| !c.is_empty()) {
        parts.push(format!("code: {code}"));
    }
    if let Some(diag) = diagnostic.map(crate::jsstr::trim).filter(|d| !d.is_empty()) {
        if diag != base {
            parts.push(diag.to_string());
        }
    }
    parts.join("\n\n")
}

/// Paseo `SYSTEM_ERROR_PREFIX`.
pub const SYSTEM_ERROR_PREFIX: &str = "[System Error]";

/// Paseo `appendSystemErrorTimelineMessage`: a failed turn shows up on the
/// timeline as an assistant message `[System Error] <message>`, not as an
/// `error` item. `None` when the message is blank, or when `last` is already
/// the same text (Paseo skips the duplicate).
pub fn system_error_item(message: &str, last: Option<&TimelineItem>) -> Option<TimelineItem> {
    let normalized = crate::jsstr::trim(message);
    if normalized.is_empty() {
        return None;
    }
    let text = format!("{SYSTEM_ERROR_PREFIX} {normalized}");
    if let Some(TimelineItem::AssistantMessage { text: last_text, .. }) = last {
        if *last_text == text {
            return None;
        }
    }
    Some(TimelineItem::AssistantMessage { text, message_id: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_round_trip() {
        for ev in [
            json!({"type":"thread_started","sessionId":"s","provider":"mock"}),
            json!({"type":"turn_started","provider":"mock","turnId":"t1"}),
            json!({"type":"turn_completed","provider":"mock","usage":{"inputTokens":32,"outputTokens":100}}),
            json!({"type":"turn_failed","provider":"mock","error":"boom","code":"E1"}),
            json!({"type":"turn_canceled","provider":"mock","reason":"interrupted"}),
            json!({"type":"timeline","provider":"mock","item":{"type":"reasoning","text":"r"},"turnId":"t1"}),
            json!({"type":"permission_resolved","provider":"mock","requestId":"p","resolution":{"behavior":"deny"}}),
            json!({"type":"attention_required","provider":"mock","reason":"finished","timestamp":"2026-10-27T00:00:00.000Z","shouldNotify":true,
                   "notification":{"title":"Agent finished","body":"b","data":{"serverId":"srv","agentId":"a","reason":"finished"}}}),
        ] {
            let parsed: StreamEvent = serde_json::from_value(ev.clone()).unwrap();
            assert_eq!(serde_json::to_value(&parsed).unwrap(), ev);
        }
    }

    #[test]
    fn turn_failed_text() {
        assert_eq!(format_turn_failed("  ", None, None), "Provider run failed");
        assert_eq!(format_turn_failed("boom", Some(" E1 "), Some("boom")), "boom\n\ncode: E1");
        assert_eq!(format_turn_failed("boom", None, Some("stack")), "boom\n\nstack");
        let item = system_error_item("boom", None).unwrap();
        assert_eq!(serde_json::to_value(&item).unwrap(), json!({"type":"assistant_message","text":"[System Error] boom"}));
        assert_eq!(system_error_item("boom", Some(&item)), None);
        assert_eq!(system_error_item(" ", None), None);
    }

    #[test]
    fn usage_merge_keeps_old_fields() {
        let old = Usage { input_tokens: Some(1.into()), output_tokens: Some(2.into()), ..Usage::default() };
        let new = Usage { output_tokens: Some(5.into()), ..Usage::default() };
        let merged = old.merged(&new);
        assert_eq!(merged.input_tokens, Some(1.into()));
        assert_eq!(merged.output_tokens, Some(5.into()));
    }
}
