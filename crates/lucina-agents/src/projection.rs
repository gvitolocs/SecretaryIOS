//! Collapsing streamed timeline rows into what clients show (Paseo
//! `server/agent/timeline-projection.js`).
//!
//! Providers stream many rows per visible item: assistant text arrives in chunks,
//! and a tool call is updated as it runs. The projection merges them:
//!
//! 1. rows with the same identity (a tool call's `callId` in the same turn, a
//!    plugin item's `pluginId/id`) fold into the first one ([`merge_tool_calls`]);
//! 2. adjacent `assistant_message` chunks of one turn concatenate, unless a chunk
//!    names a different `messageId`;
//! 3. adjacent `reasoning` chunks of one turn concatenate.
//!
//! A projected entry keeps `seqStart..seqEnd` and the source ranges it covers.

use serde_json::{Map, Value};

use crate::timeline::{TimelineItem, TimelineRow, ToolCall, ToolCallDetail, ToolCallStatus};

/// One projected entry (Paseo's canonical/projected row shape).
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedEntry {
    pub item: TimelineItem,
    pub timestamp: String,
    pub turn_id: Option<String>,
    pub seq_start: u64,
    pub seq_end: u64,
    pub source_seq_ranges: Vec<(u64, u64)>,
    pub collapsed: Vec<&'static str>,
}

fn canonical(row: &TimelineRow) -> ProjectedEntry {
    ProjectedEntry {
        item: row.item.clone(),
        timestamp: row.timestamp.clone(),
        turn_id: row.turn_id.clone(),
        seq_start: row.seq,
        seq_end: row.seq,
        source_seq_ranges: vec![(row.seq, row.seq)],
        collapsed: Vec::new(),
    }
}

fn append_seq(ranges: &mut Vec<(u64, u64)>, seq: u64) {
    match ranges.last_mut() {
        Some(last) if seq <= last.1 + 1 => last.1 = last.1.max(seq),
        _ => ranges.push((seq, seq)),
    }
}

fn merge_seq_ranges(existing: &[(u64, u64)], incoming: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut merged = existing.to_vec();
    for &(start, end) in incoming {
        for seq in start..=end {
            append_seq(&mut merged, seq);
        }
    }
    merged
}

fn with_collapse(collapsed: &[&'static str], kind: &'static str) -> Vec<&'static str> {
    let mut out = collapsed.to_vec();
    if !out.contains(&kind) {
        out.push(kind);
    }
    out
}

/// Paseo `mergeToolCallItems`: a later update for the same call replaces the
/// earlier one, except that a known detail beats `unknown`, metadata merges, and
/// an absent `agentMessage` keeps the earlier one.
pub fn merge_tool_calls(existing: &ToolCall, incoming: &ToolCall) -> ToolCall {
    let detail = match (&existing.detail, &incoming.detail) {
        (ToolCallDetail::Unknown { .. }, d) if !matches!(d, ToolCallDetail::Unknown { .. }) => {
            d.clone()
        }
        (d, ToolCallDetail::Unknown { .. }) if !matches!(d, ToolCallDetail::Unknown { .. }) => {
            d.clone()
        }
        (_, d) => d.clone(),
    };
    let metadata = match (&existing.metadata, &incoming.metadata) {
        (None, None) => None,
        (old, new) => {
            let mut map: Map<String, Value> = old.clone().unwrap_or_default();
            for (k, v) in new.clone().unwrap_or_default() {
                map.insert(k, v);
            }
            Some(map)
        }
    };
    let error = match incoming.status {
        ToolCallStatus::Completed | ToolCallStatus::Canceled => Value::Null,
        _ => incoming.error.clone(),
    };
    ToolCall {
        agent_message: incoming
            .agent_message
            .clone()
            .or_else(|| existing.agent_message.clone()),
        call_id: incoming.call_id.clone(),
        name: incoming.name.clone(),
        detail,
        metadata,
        status: incoming.status,
        error,
    }
}

fn merge_identity(existing: &ProjectedEntry, entry: &ProjectedEntry) -> Option<ProjectedEntry> {
    match (&existing.item, &entry.item) {
        (TimelineItem::ToolCall(old), TimelineItem::ToolCall(new)) => {
            if existing.turn_id != entry.turn_id {
                return None;
            }
            Some(ProjectedEntry {
                item: TimelineItem::ToolCall(Box::new(merge_tool_calls(old, new))),
                timestamp: entry.timestamp.clone(),
                turn_id: existing.turn_id.clone(),
                seq_start: existing.seq_start,
                seq_end: existing.seq_end.max(entry.seq_end),
                source_seq_ranges: merge_seq_ranges(
                    &existing.source_seq_ranges,
                    &entry.source_seq_ranges,
                ),
                collapsed: with_collapse(&existing.collapsed, "tool_lifecycle"),
            })
        }
        (_, TimelineItem::ToolCall(_)) => None,
        (TimelineItem::Plugin { .. }, TimelineItem::Plugin { .. }) => Some(ProjectedEntry {
            seq_start: existing.seq_start,
            source_seq_ranges: merge_seq_ranges(&existing.source_seq_ranges, &entry.source_seq_ranges),
            collapsed: with_collapse(&existing.collapsed, "identity"),
            ..entry.clone()
        }),
        _ => None,
    }
}

fn collapse_by_identity(entries: Vec<ProjectedEntry>) -> Vec<ProjectedEntry> {
    let mut output: Vec<ProjectedEntry> = Vec::new();
    let mut index_by_identity: std::collections::HashMap<String, usize> = Default::default();
    for entry in entries {
        let Some(identity) = entry.item.identity() else {
            output.push(entry);
            continue;
        };
        match index_by_identity.get(&identity).copied() {
            None => {
                index_by_identity.insert(identity, output.len());
                output.push(entry);
            }
            Some(i) => match merge_identity(&output[i], &entry) {
                Some(merged) => output[i] = merged,
                None => {
                    index_by_identity.insert(identity, output.len());
                    output.push(entry);
                }
            },
        }
    }
    output
}

fn merged_kinds(a: &[&'static str], b: &[&'static str], kind: &'static str) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for k in a.iter().chain(b.iter()).copied().chain(std::iter::once(kind)) {
        if !out.contains(&k) {
            out.push(k);
        }
    }
    out
}

fn merge_assistant_chunks(entries: Vec<ProjectedEntry>) -> Vec<ProjectedEntry> {
    let mut output: Vec<ProjectedEntry> = Vec::new();
    for entry in entries {
        if let Some(previous) = output.last_mut() {
            if let (
                TimelineItem::AssistantMessage { text: prev_text, message_id: prev_id },
                TimelineItem::AssistantMessage { text, message_id },
            ) = (&previous.item, &entry.item)
            {
                let adjacent = previous.seq_end + 1 == entry.seq_start && previous.turn_id == entry.turn_id;
                let other_message = message_id.is_some() && prev_id != message_id;
                if adjacent && !other_message {
                    let item = TimelineItem::AssistantMessage {
                        text: format!("{prev_text}{text}"),
                        message_id: prev_id.clone().filter(|id| !id.is_empty()),
                    };
                    *previous = ProjectedEntry {
                        item,
                        timestamp: entry.timestamp.clone(),
                        seq_end: entry.seq_end,
                        source_seq_ranges: merge_seq_ranges(&previous.source_seq_ranges, &entry.source_seq_ranges),
                        collapsed: merged_kinds(&previous.collapsed, &entry.collapsed, "assistant_merge"),
                        ..previous.clone()
                    };
                    continue;
                }
            }
        }
        output.push(entry);
    }
    output
}

fn merge_reasoning_chunks(entries: Vec<ProjectedEntry>) -> Vec<ProjectedEntry> {
    let mut output: Vec<ProjectedEntry> = Vec::new();
    for entry in entries {
        if let Some(previous) = output.last_mut() {
            if let (TimelineItem::Reasoning { text: prev_text }, TimelineItem::Reasoning { text }) =
                (&previous.item, &entry.item)
            {
                if previous.seq_end + 1 == entry.seq_start && previous.turn_id == entry.turn_id {
                    let item = TimelineItem::Reasoning { text: format!("{prev_text}{text}") };
                    *previous = ProjectedEntry {
                        item,
                        timestamp: entry.timestamp.clone(),
                        seq_end: entry.seq_end,
                        source_seq_ranges: merge_seq_ranges(&previous.source_seq_ranges, &entry.source_seq_ranges),
                        collapsed: merged_kinds(&previous.collapsed, &entry.collapsed, "reasoning_merge"),
                        ..previous.clone()
                    };
                    continue;
                }
            }
        }
        output.push(entry);
    }
    output
}

/// Paseo `projectTimelineRows({mode: "projected"})`.
pub fn project_rows(rows: &[TimelineRow]) -> Vec<ProjectedEntry> {
    let canonical: Vec<ProjectedEntry> = rows.iter().map(canonical).collect();
    merge_reasoning_chunks(merge_assistant_chunks(collapse_by_identity(canonical)))
}

/// Rows numbered 1.. from bare items, as Paseo's curator does.
pub fn rows_from_items(items: &[TimelineItem]) -> Vec<TimelineRow> {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| TimelineRow {
            seq: i as u64 + 1,
            timestamp: String::new(),
            item: item.clone(),
            turn_id: None,
        })
        .collect()
}

/// Which end of the timeline a window is taken from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    After,
    Tail,
}

/// Paseo `selectTimelineWindowByProjectedLimit`: pick `limit` projected entries
/// (0 = all), widen the window to every projected entry overlapping it, and map
/// it back to contiguous canonical rows.
pub fn select_window(
    rows: &[TimelineRow],
    direction: Direction,
    limit: usize,
) -> (Vec<ProjectedEntry>, Vec<TimelineRow>) {
    let projected_all = project_rows(rows);
    if projected_all.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let picked: Vec<ProjectedEntry> = if limit == 0 || limit >= projected_all.len() {
        projected_all.clone()
    } else if direction == Direction::After {
        projected_all[..limit].to_vec()
    } else {
        projected_all[projected_all.len() - limit..].to_vec()
    };
    let bounds = |entries: &[ProjectedEntry]| {
        let min = entries.iter().map(|e| e.seq_start).min().unwrap_or(u64::MAX);
        let max = entries.iter().map(|e| e.seq_end).max().unwrap_or(0);
        (min, max)
    };
    let (mut min_seq, mut max_seq) = bounds(&picked);
    let mut expanded = picked;
    for _ in 0..=projected_all.len() {
        let overlapping: Vec<ProjectedEntry> = projected_all
            .iter()
            .filter(|e| e.seq_start <= max_seq && e.seq_end >= min_seq)
            .cloned()
            .collect();
        let (next_min, next_max) = bounds(&overlapping);
        let stable = overlapping.len() == expanded.len() && next_min == min_seq && next_max == max_seq;
        expanded = overlapping;
        min_seq = next_min;
        max_seq = next_max;
        if stable {
            break;
        }
    }
    let selected = rows
        .iter()
        .filter(|r| r.seq >= min_seq && r.seq <= max_seq)
        .cloned()
        .collect();
    (expanded, selected)
}

/// Paseo `selectItemsByProjectedLimit`: `(items, totalProjected, shownProjected)`.
pub fn select_items_by_projected_limit(
    items: &[TimelineItem],
    direction: Direction,
    limit: usize,
) -> (Vec<TimelineItem>, usize, usize) {
    let rows = rows_from_items(items);
    let total = project_rows(&rows).len();
    let (entries, selected) = select_window(&rows, direction, limit);
    (selected.into_iter().map(|r| r.item).collect(), total, entries.len())
}

/// Paseo `TimelineProjection`: the incremental projection the daemon keeps per
/// agent while rows stream in.
#[derive(Debug, Clone, Default)]
pub struct Projection {
    rows: Vec<ProjectedEntry>,
    identities: std::collections::HashMap<String, usize>,
}

impl Projection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn append(&mut self, row: &TimelineRow) {
        let entry = canonical(row);
        let identity = row.item.identity();
        if let Some(i) = identity.as_ref().and_then(|id| self.identities.get(id)).copied() {
            if let Some(merged) = merge_identity(&self.rows[i], &entry) {
                self.rows[i] = merged;
                return;
            }
        }
        if let Some(previous) = self.rows.last().cloned() {
            let adjacent = merge_reasoning_chunks(merge_assistant_chunks(vec![previous, entry.clone()]));
            if adjacent.len() == 1 {
                let last = self.rows.len() - 1;
                self.rows[last] = adjacent.into_iter().next().expect("one entry");
                return;
            }
        }
        if let Some(id) = identity {
            self.identities.insert(id, self.rows.len());
        }
        self.rows.push(entry);
    }

    pub fn rows(&self) -> &[ProjectedEntry] {
        &self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(v: Value) -> TimelineItem {
        serde_json::from_value(v).unwrap()
    }

    fn row(seq: u64, v: Value, turn: Option<&str>) -> TimelineRow {
        TimelineRow { seq, timestamp: format!("t{seq}"), item: item(v), turn_id: turn.map(str::to_string) }
    }

    #[test]
    fn chunks_and_tool_lifecycle_collapse() {
        let rows = vec![
            row(1, json!({"type":"user_message","text":"go"}), None),
            row(2, json!({"type":"assistant_message","text":"Hel"}), Some("t")),
            row(3, json!({"type":"assistant_message","text":"lo"}), Some("t")),
            row(4, json!({"type":"tool_call","callId":"c","name":"Bash","detail":{"type":"unknown","input":{"cmd":"ls"},"output":null},"status":"running","error":null}), Some("t")),
            row(5, json!({"type":"reasoning","text":"a"}), Some("t")),
            row(6, json!({"type":"reasoning","text":"b"}), Some("t")),
            row(7, json!({"type":"tool_call","callId":"c","name":"Bash","detail":{"type":"shell","command":"ls"},"status":"completed","error":null}), Some("t")),
        ];
        let p = project_rows(&rows);
        let kinds: Vec<_> = p.iter().map(|e| e.item.kind()).collect();
        assert_eq!(kinds, ["user_message", "assistant_message", "tool_call", "reasoning"]);
        assert_eq!(p[1].item, item(json!({"type":"assistant_message","text":"Hello"})));
        assert_eq!((p[2].seq_start, p[2].seq_end), (4, 7));
        match &p[2].item {
            TimelineItem::ToolCall(c) => {
                assert_eq!(c.status, ToolCallStatus::Completed);
                assert_eq!(c.detail.kind(), "shell");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(p[3].item, item(json!({"type":"reasoning","text":"ab"})));

        let mut inc = Projection::new();
        for r in &rows {
            inc.append(r);
        }
        let inc_kinds: Vec<_> = inc.rows().iter().map(|e| e.item.kind()).collect();
        assert_eq!(inc_kinds, kinds);
    }

    #[test]
    fn different_message_ids_do_not_merge() {
        let rows = vec![
            row(1, json!({"type":"assistant_message","text":"a","messageId":"m1"}), None),
            row(2, json!({"type":"assistant_message","text":"b","messageId":"m2"}), None),
            row(3, json!({"type":"assistant_message","text":"c"}), None),
        ];
        let p = project_rows(&rows);
        assert_eq!(p.len(), 2);
        assert_eq!(p[1].item, item(json!({"type":"assistant_message","text":"bc","messageId":"m2"})));
    }

    #[test]
    fn window_widens_over_tool_updates() {
        let rows = vec![
            row(1, json!({"type":"tool_call","callId":"c","name":"x","detail":{"type":"plan","text":"p"},"status":"running","error":null}), None),
            row(2, json!({"type":"user_message","text":"u"}), None),
            row(3, json!({"type":"tool_call","callId":"c","name":"x","detail":{"type":"plan","text":"p"},"status":"completed","error":null}), None),
        ];
        let (entries, selected) = select_window(&rows, Direction::Tail, 1);
        assert_eq!(entries.len(), 2);
        assert_eq!(selected.len(), 3);
    }
}
