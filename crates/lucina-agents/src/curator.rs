//! The plain-text activity log that `lagent logs`, MCP `agent_logs` and fork
//! context read (Paseo `server/agent/activity-curator.js`).

use serde::Serialize;
use serde_json::Value;

use crate::display::{agent_message_display, is_likely_external_tool_name, tool_call_display, ToolCallDisplayInput};
use crate::jsstr;
use crate::projection::{project_rows, rows_from_items, select_items_by_projected_limit, Direction};
use crate::timeline::{AgentMessage, TimelineItem, ToolCallDetail};

const MAX_TOOL_INPUT_CHARS: usize = 400;
const MAX_TOOL_SUMMARY_CHARS: usize = 200;

/// Curator options (Paseo's `options` object).
#[derive(Debug, Clone, Default)]
pub struct CurateOptions {
    /// 0 = all items.
    pub max_items: usize,
    pub label_assistant_messages: bool,
    /// Only these item types (`user_message`, `tool_call`, ...), when set.
    pub include_kinds: Option<Vec<&'static str>>,
    /// Paseo's default is true.
    pub include_external_tool_input: Option<bool>,
}

/// Paseo `summarizeAgentActivity`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySummary {
    pub update_count: usize,
    pub content: String,
}

fn format_tool_input_json(input: &Value) -> String {
    let encoded = serde_json::to_string(input).unwrap_or_else(|_| "null".to_string());
    if jsstr::len(&encoded) <= MAX_TOOL_INPUT_CHARS {
        encoded
    } else {
        format!("{}...", jsstr::slice(&encoded, 0, MAX_TOOL_INPUT_CHARS))
    }
}

fn format_tool_summary(summary: Option<&str>) -> Option<String> {
    let collapsed = jsstr::collapse_spaces(summary?);
    let normalized = jsstr::trim(&collapsed);
    if normalized.is_empty() {
        return None;
    }
    if jsstr::len(normalized) <= MAX_TOOL_SUMMARY_CHARS {
        return Some(normalized.to_string());
    }
    Some(format!("{}...", jsstr::slice(normalized, 0, MAX_TOOL_SUMMARY_CHARS - 3)))
}

fn push_text(entries: &mut Vec<String>, item: &TimelineItem, text: &str, options: &CurateOptions) {
    let text = jsstr::trim(text);
    if text.is_empty() {
        return;
    }
    let prefix = match item {
        TimelineItem::Reasoning { .. } => "[Thought] ",
        _ if options.label_assistant_messages => "[Assistant] ",
        _ => "",
    };
    entries.push(format!("{prefix}{text}"));
}

fn push_agent_message(entries: &mut Vec<String>, message: &AgentMessage) {
    let display = agent_message_display(message);
    entries.push(match display.summary {
        Some(s) => format!("[{}] {s}", display.display_name),
        None => format!("[{}]", display.display_name),
    });
    let body = jsstr::trim(&message.text);
    if !body.is_empty() {
        entries.push(body.to_string());
    }
}

/// Paseo `curateProjectedActivityEntries`, on items that are already projected.
pub fn curate_projected(items: &[TimelineItem], options: &CurateOptions) -> Vec<String> {
    let recent = if options.max_items > 0 && items.len() > options.max_items {
        &items[items.len() - options.max_items..]
    } else {
        items
    };
    let mut entries = Vec::new();
    for item in recent {
        if let Some(kinds) = &options.include_kinds {
            if !kinds.contains(&item.kind()) {
                continue;
            }
        }
        match item {
            TimelineItem::UserMessage { text, .. } => {
                entries.push(format!("[User] {}", jsstr::trim(text)));
            }
            TimelineItem::AssistantMessage { text, .. } | TimelineItem::Reasoning { text } => {
                push_text(&mut entries, item, text, options);
            }
            TimelineItem::ToolCall(call) => {
                if let Some(message) = &call.agent_message {
                    push_agent_message(&mut entries, message);
                    continue;
                }
                let input = match &call.detail {
                    ToolCallDetail::Unknown { input, .. } => input.clone(),
                    _ => Value::Null,
                };
                let input_json = format_tool_input_json(&input);
                let display = tool_call_display(&ToolCallDisplayInput {
                    name: &call.name,
                    detail: &call.detail,
                    metadata: call.metadata.as_ref(),
                    agent_message: None,
                    cwd: None,
                    target_agent_title: None,
                });
                let summary = format_tool_summary(display.summary.as_deref());
                if options.include_external_tool_input.unwrap_or(true) && is_likely_external_tool_name(&call.name) {
                    entries.push(format!("[{}] {input_json}", display.display_name));
                } else {
                    entries.push(match summary {
                        Some(s) => format!("[{}] {s}", display.display_name),
                        None => format!("[{}]", display.display_name),
                    });
                }
                if let ToolCallDetail::SubAgent { log, .. } = &call.detail {
                    let log = jsstr::trim(log);
                    if !log.is_empty() {
                        entries.push(log.to_string());
                    }
                }
            }
            TimelineItem::Todo { items } => {
                entries.push("[Tasks]".to_string());
                for entry in items {
                    let checkbox = if entry.completed { "[x]" } else { "[ ]" };
                    entries.push(format!("- {checkbox} {}", entry.text));
                }
            }
            TimelineItem::Error { message } => entries.push(format!("[Error] {message}")),
            TimelineItem::Compaction { .. } => entries.push("[Compacted]".to_string()),
            TimelineItem::Notification { .. } | TimelineItem::Plugin { .. } => {}
        }
    }
    entries
}

/// Paseo `curateAgentActivity`.
pub fn curate_activity(timeline: &[TimelineItem], options: &CurateOptions) -> String {
    let projected: Vec<TimelineItem> = project_rows(&rows_from_items(timeline))
        .into_iter()
        .map(|e| e.item)
        .collect();
    let entries = curate_projected(&projected, options);
    if entries.is_empty() {
        "No activity to display.".to_string()
    } else {
        entries.join("\n")
    }
}

/// Paseo `summarizeAgentActivity` (`limit` 0 or `None` = everything).
pub fn summarize_activity(timeline: &[TimelineItem], limit: Option<usize>) -> ActivitySummary {
    let limit = limit.unwrap_or(0);
    let (items, total, shown) = select_items_by_projected_limit(timeline, Direction::Tail, limit);
    let noun = if total == 1 { "activity" } else { "activities" };
    let header = if limit > 0 && shown < total {
        format!("Showing {shown} of {total} {noun} (limited to {limit})")
    } else {
        format!("Showing all {total} {noun}")
    };
    ActivitySummary {
        update_count: timeline.len(),
        content: format!("{header}\n\n{}", curate_activity(&items, &CurateOptions::default())),
    }
}
