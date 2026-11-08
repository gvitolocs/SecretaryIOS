//! Fork context: a new agent seeded with an agent's chat history (Paseo
//! `buildAgentForkContextAttachment` in `server/agent/activity-curator.js`, served as
//! `agent.fork_context.request`).
//!
//! The history is the source agent's projected timeline up to a boundary, cut down to
//! user messages, assistant messages and tool calls (reasoning, notifications and
//! plugin items stay out), written as the curator's plain-text log with assistant
//! messages labelled, and wrapped in `<chat-history-summary>`. The whole attachment is
//! at most [`MAX_CHARS`] characters (UTF-16 code units, as Paseo counts); when the log
//! doesn't fit, the oldest entries go first and `[Earlier chat history omitted]` marks
//! the cut.
//!
//! The boundary picks where the fork starts from, by one of:
//!
//! - a **cursor** `{epoch, seq}`: the projected entry that ends at `seq`. A cursor from
//!   another epoch is stale (the timeline was replaced since);
//! - an **assistant message id**: the last projected assistant message with that id;
//! - nothing: the whole timeline.
//!
//! A boundary inside an entry that has kept growing since the cursor was made (a
//! streamed answer, a tool call still updating) is refused: fork from a completed
//! response.

use serde::Serialize;

use crate::curator::{curate_projected, CurateOptions};
use crate::jsstr;
use crate::projection::project_rows;
use crate::timeline::{Cursor, TimelineItem, TimelineRow};

/// Paseo `MAX_FORK_CONTEXT_CHARS`.
pub const MAX_CHARS: usize = 100_000;
/// Paseo `MAX_FORK_METADATA_CHARS`: the title and directory in the header.
const MAX_METADATA_CHARS: usize = 1000;
const HISTORY_OMITTED: &str = "[Earlier chat history omitted]\n";
const TRUNCATED_MARKER: &str = "\n[Message truncated]\n";
const NO_HISTORY: &str = "No chat history to display.";

/// Why a boundary can't be used (Paseo's messages).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForkError {
    #[error("Selected timeline position is no longer available.")]
    PositionGone,
    #[error("Selected assistant message is no longer available.")]
    MessageGone,
    #[error("This checkpoint changed after it was created. Fork from a later completed response instead.")]
    Changed,
}

/// Paseo's `TextAttachment` with `contextKind: "chat_history"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub mime_type: &'static str,
    pub context_kind: &'static str,
    pub title: &'static str,
    pub text: String,
}

/// Paseo's `agent.fork_context.response` payload (without the request id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkContext {
    pub attachment: Attachment,
    /// Projected entries before the boundary (all kinds, not only the ones written).
    pub item_count: usize,
    pub boundary_cursor: Option<Cursor>,
    pub boundary_message_id: Option<String>,
}

fn metadata(value: Option<&str>) -> Option<String> {
    let trimmed = jsstr::trim(value?);
    (!trimmed.is_empty()).then(|| jsstr::slice(trimmed, 0, MAX_METADATA_CHARS))
}

/// Paseo `buildForkContextText`.
fn wrap(body: &str, title: Option<&str>, cwd: Option<&str>) -> String {
    let mut header = vec!["Chat history from a previous Paseo agent.".to_string()];
    if let Some(t) = metadata(title) {
        header.push(format!("Source agent: {t}"));
    }
    if let Some(c) = metadata(cwd) {
        header.push(format!("Source directory: {c}"));
    }
    format!("<chat-history-summary>\n{}\n\n{body}\n</chat-history-summary>", header.join("\n"))
}

/// Paseo `fitForkContextEntries`: the newest entries that fit `budget` characters.
fn fit(entries: &[String], budget: usize) -> String {
    let length: usize = entries.iter().map(|e| jsstr::len(e) + 1).sum::<usize>().saturating_sub(1);
    if length <= budget {
        return entries.join("\n");
    }
    let mut selected: Vec<String> = Vec::new();
    let mut remaining = budget.saturating_sub(jsstr::len(HISTORY_OMITTED));
    for text in entries.iter().rev() {
        let size = jsstr::len(text) + usize::from(!selected.is_empty());
        if size > remaining {
            if selected.is_empty() {
                // One entry bigger than everything: keep its head and its tail.
                let available = remaining.saturating_sub(jsstr::len(TRUNCATED_MARKER));
                let head = available.div_ceil(2);
                let tail = available - head;
                // JS `text.slice(-0)` is the whole text.
                let tail_text = if tail == 0 { text.clone() } else { jsstr::slice_from(text, jsstr::len(text).saturating_sub(tail)) };
                selected.push(format!("{}{TRUNCATED_MARKER}{tail_text}", jsstr::slice(text, 0, head)));
            }
            break;
        }
        selected.push(text.clone());
        remaining -= size;
    }
    selected.reverse();
    format!("{HISTORY_OMITTED}{}", selected.join("\n"))
}

/// Where the fork starts (see the module docs). A cursor wins over a message id.
#[derive(Debug, Clone, Copy, Default)]
pub struct Boundary<'a> {
    pub cursor: Option<&'a Cursor>,
    pub message_id: Option<&'a str>,
}

/// The projected items before the boundary, and the boundary as Paseo echoes it.
struct Selected {
    items: Vec<TimelineItem>,
    cursor: Option<Cursor>,
    message_id: Option<String>,
}

/// Paseo `selectForkContextRows`.
fn select(rows: &[TimelineRow], epoch: &str, boundary: Boundary<'_>) -> Result<Selected, ForkError> {
    let projected = project_rows(rows);
    let message_id = boundary.message_id.map(jsstr::trim).filter(|m| !m.is_empty());
    if boundary.cursor.is_none() && message_id.is_none() {
        return Ok(Selected { items: projected.into_iter().map(|e| e.item).collect(), cursor: None, message_id: None });
    }
    if boundary.cursor.is_some_and(|c| c.epoch != epoch) {
        return Err(ForkError::PositionGone);
    }
    let index = match boundary.cursor {
        Some(c) => projected.iter().position(|r| r.seq_end == c.seq).ok_or(ForkError::PositionGone)?,
        None => projected
            .iter()
            .rposition(|r| matches!(&r.item, TimelineItem::AssistantMessage { message_id: Some(id), .. } if Some(id.as_str()) == message_id))
            .ok_or(ForkError::MessageGone)?,
    };
    let seq = projected[index].seq_end;
    if projected.iter().any(|r| r.seq_start <= seq && r.seq_end > seq) {
        return Err(ForkError::Changed);
    }
    let items = projected.into_iter().filter(|r| r.seq_end <= seq).map(|e| e.item).collect();
    Ok(Selected { items, cursor: boundary.cursor.cloned(), message_id: message_id.map(str::to_string) })
}

/// Paseo `buildAgentForkContextAttachment`. `epoch` is the timeline's current epoch;
/// `title` and `cwd` are the source agent's.
pub fn build(rows: &[TimelineRow], epoch: &str, boundary: Boundary<'_>, title: Option<&str>, cwd: Option<&str>) -> Result<ForkContext, ForkError> {
    let Selected { items, cursor: boundary_cursor, message_id: boundary_message_id } = select(rows, epoch, boundary)?;
    let entries = curate_projected(
        &items,
        &CurateOptions {
            max_items: 0,
            label_assistant_messages: true,
            include_kinds: Some(vec!["user_message", "assistant_message", "tool_call"]),
            include_external_tool_input: Some(false),
        },
    );
    let budget = MAX_CHARS.saturating_sub(jsstr::len(&wrap("", title, cwd)));
    let body = if entries.is_empty() { NO_HISTORY.to_string() } else { fit(&entries, budget) };
    Ok(ForkContext {
        attachment: Attachment { kind: "text", mime_type: "text/plain", context_kind: "chat_history", title: "Chat history", text: wrap(&body, title, cwd) },
        item_count: items.len(),
        boundary_cursor,
        boundary_message_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(seq: u64, item: TimelineItem) -> TimelineRow {
        TimelineRow { seq, timestamp: String::new(), item, turn_id: Some("t1".into()) }
    }

    fn user(seq: u64, text: &str) -> TimelineRow {
        row(seq, TimelineItem::UserMessage { text: text.into(), message_id: None, client_message_id: None })
    }

    fn assistant(seq: u64, text: &str, id: &str) -> TimelineRow {
        row(seq, TimelineItem::AssistantMessage { text: text.into(), message_id: Some(id.into()) })
    }

    fn history() -> Vec<TimelineRow> {
        vec![user(1, "fix the bug"), assistant(2, "Looking", "m1"), assistant(3, " at it.", "m1"), user(4, "and the test"), assistant(5, "Done.", "m2")]
    }

    #[test]
    fn wraps_the_log_with_the_source() {
        let c = build(&history(), "e1", Boundary::default(), Some("  Scroll bug "), Some("/w/x")).unwrap();
        assert_eq!(
            c.attachment.text,
            "<chat-history-summary>\nChat history from a previous Paseo agent.\nSource agent: Scroll bug\nSource directory: /w/x\n\n[User] fix the bug\n[Assistant] Looking at it.\n[User] and the test\n[Assistant] Done.\n</chat-history-summary>"
        );
        assert_eq!((c.item_count, c.boundary_cursor, c.boundary_message_id), (4, None, None));
        let json = serde_json::to_value(&c.attachment).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["contextKind"], "chat_history");
        assert_eq!(json["mimeType"], "text/plain");
        assert_eq!(json["title"], "Chat history");
    }

    #[test]
    fn an_empty_history_says_so() {
        let c = build(&[], "e1", Boundary::default(), None, None).unwrap();
        assert!(c.attachment.text.contains("\n\nNo chat history to display.\n"));
        assert_eq!(c.item_count, 0);
    }

    #[test]
    fn boundary_by_cursor() {
        let cursor = Cursor { epoch: "e1".into(), seq: 3 };
        let c = build(&history(), "e1", Boundary { cursor: Some(&cursor), message_id: None }, None, None).unwrap();
        assert!(c.attachment.text.ends_with("[Assistant] Looking at it.\n</chat-history-summary>"));
        assert_eq!((c.item_count, c.boundary_cursor), (2, Some(cursor.clone())));
        // A cursor into the middle of a merged answer is no entry's end.
        let mid = Cursor { epoch: "e1".into(), seq: 2 };
        assert_eq!(build(&history(), "e1", Boundary { cursor: Some(&mid), message_id: None }, None, None), Err(ForkError::PositionGone));
        let gone = Cursor { epoch: "e1".into(), seq: 99 };
        assert_eq!(build(&history(), "e1", Boundary { cursor: Some(&gone), message_id: None }, None, None), Err(ForkError::PositionGone));
        let stale = Cursor { epoch: "old".into(), seq: 3 };
        assert_eq!(build(&history(), "e1", Boundary { cursor: Some(&stale), message_id: None }, None, None), Err(ForkError::PositionGone));
    }

    #[test]
    fn a_checkpoint_inside_a_tool_call_that_kept_updating_is_refused() {
        use crate::timeline::{ToolCall, ToolCallDetail, ToolCallStatus};
        let call = |status| {
            TimelineItem::ToolCall(Box::new(ToolCall {
                agent_message: None,
                call_id: "c1".into(),
                name: "bash".into(),
                detail: ToolCallDetail::Shell { command: "ls".into(), cwd: None, output: None, exit_code: None },
                metadata: None,
                status,
                error: serde_json::Value::Null,
            }))
        };
        // The call starts at seq 2 and completes at seq 4, around a message at seq 3.
        let rows = vec![user(1, "go"), row(2, call(ToolCallStatus::Running)), assistant(3, "meanwhile", "m1"), row(4, call(ToolCallStatus::Completed))];
        let cursor = Cursor { epoch: "e1".into(), seq: 3 };
        assert_eq!(build(&rows, "e1", Boundary { cursor: Some(&cursor), message_id: None }, None, None), Err(ForkError::Changed));
        assert_eq!(build(&rows, "e1", Boundary { cursor: None, message_id: Some("m1") }, None, None), Err(ForkError::Changed));
    }

    #[test]
    fn boundary_by_assistant_message_id() {
        let c = build(&history(), "e1", Boundary { cursor: None, message_id: Some(" m1 ") }, None, None).unwrap();
        assert!(c.attachment.text.ends_with("[Assistant] Looking at it.\n</chat-history-summary>"));
        assert_eq!(c.boundary_message_id.as_deref(), Some("m1"));
        assert_eq!(build(&history(), "e1", Boundary { cursor: None, message_id: Some("nope") }, None, None), Err(ForkError::MessageGone));
        // A blank id is no boundary.
        assert_eq!(build(&history(), "e1", Boundary { cursor: None, message_id: Some("  ") }, None, None).unwrap().item_count, 4);
    }

    #[test]
    fn a_long_history_keeps_the_newest_entries() {
        let mut rows = Vec::new();
        for i in 0..60u64 {
            rows.push(user(i * 2 + 1, &format!("question {i}: {}", "x".repeat(2000))));
            rows.push(assistant(i * 2 + 2, &format!("answer {i}"), &format!("m{i}")));
        }
        let c = build(&rows, "e1", Boundary::default(), Some("t"), Some("/w")).unwrap();
        assert!(jsstr::len(&c.attachment.text) <= MAX_CHARS);
        assert!(c.attachment.text.contains("\n\n[Earlier chat history omitted]\n["), "{}", &c.attachment.text[..200]);
        assert!(c.attachment.text.contains("[Assistant] answer 59\n</chat-history-summary>"));
        assert!(!c.attachment.text.contains("question 0:"));
        assert_eq!(c.item_count, 120);
    }

    #[test]
    fn one_giant_entry_keeps_its_head_and_tail() {
        let text = format!("{}{}", "a".repeat(150_000), "z".repeat(10));
        let c = build(&[user(1, &text)], "e1", Boundary::default(), None, None).unwrap();
        let body = &c.attachment.text;
        assert!(jsstr::len(body) <= MAX_CHARS);
        assert!(body.contains("[Earlier chat history omitted]\n[User] aaaa"));
        assert!(body.contains("\n[Message truncated]\n"));
        assert!(body.ends_with("zzzzzzzzzz\n</chat-history-summary>"));
    }

    #[test]
    fn tool_calls_and_reasoning() {
        use crate::timeline::{ToolCall, ToolCallDetail, ToolCallStatus};
        let call = TimelineItem::ToolCall(Box::new(ToolCall {
            agent_message: None,
            call_id: "c1".into(),
            name: "bash".into(),
            detail: ToolCallDetail::Shell { command: "ls".into(), cwd: None, output: None, exit_code: None },
            metadata: None,
            status: ToolCallStatus::Completed,
            error: serde_json::Value::Null,
        }));
        let rows = vec![user(1, "go"), row(2, TimelineItem::Reasoning { text: "hmm".into() }), row(3, call)];
        let c = build(&rows, "e1", Boundary::default(), None, None).unwrap();
        assert!(c.attachment.text.contains("[User] go\n[Shell] ls\n"), "{}", c.attachment.text);
        assert!(!c.attachment.text.contains("hmm"));
        assert_eq!(c.item_count, 3);
    }
}
