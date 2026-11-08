//! Claude `Task` subagents (Paseo `claude/sidechain-tracker.js`, `claude/subagents/live-source.js`).
//!
//! In stream-json, a subagent's work arrives on the same stream as the conversation, its
//! frames tagged `parent_tool_use_id` = the id of the `Task` tool call that started it
//! (the "sidechain"). They are not the parent's conversation, so they never go to the
//! parent's timeline. Each child gets a descriptor and a timeline of its own, keyed by
//! the `Task` call id:
//!
//! - **Identity and lifecycle** come from Claude Code's task protocol, which announces
//!   every task: `system` `task_started` (the task id, the call's `tool_use_id`, the
//!   type, the description and the prompt), `task_updated` and `task_notification`
//!   (status, `usage.total_tokens`), `task_progress`. Only `local_agent` and
//!   `local_workflow` tasks are subagents (a backgrounded shell is `local_bash`). Once
//!   a CLI announces tasks, a sidechain frame of a task it never declared is dropped.
//!   An older CLI announces nothing: the first sidechain frame declares the child and
//!   the `Task` result ends it.
//! - **The child's timeline** is built from its complete `assistant` messages (text,
//!   thinking, tool calls; the `SubagentHandback` report, which Claude Code 2.1.271+
//!   delivers instead of final text, is its last assistant message) and the `user`
//!   messages that answer its tool calls. Its prompt is `task_started.prompt`. Streamed
//!   deltas are not used.
//! - **The parent keeps one card**: the `Task` tool call as a `sub_agent` detail, whose
//!   `log` lists the child's tool calls (`[Bash] ls -la`, at most 200, like Paseo) and,
//!   once it has finished, its report. `lagent logs` of the parent prints that.
//!   A child started inside another child has its card in that child's timeline, and
//!   its `parentSubagentId` names it.

use std::collections::{HashMap, HashSet};

use lucina_agents::display::{tool_call_display, ToolCallDisplayInput};
use lucina_agents::jsstr;
use lucina_agents::subagent::{SubagentEvent, SubagentStatus};
use lucina_agents::timeline::{ToolCallDetail, ToolCallStatus};
use lucina_agents::{StreamEvent, TimelineItem};
use serde_json::{json, Value};

use super::claude::{detail_for, s, todo_items, tool_item, tool_result_text, ToolUse, ID};

/// Most tool calls a card's log lists (Paseo `MAX_SUB_AGENT_LOG_ENTRIES`).
const MAX_LOG_ENTRIES: usize = 200;
/// Longest task name, description or log summary (Paseo `MAX_SUB_AGENT_SUMMARY_CHARS`).
const MAX_TEXT: usize = 160;
/// Claude Code 2.1.271+ (subagent in auto mode): the report arrives as this call's `message`.
const HANDBACK: &str = "SubagentHandback";

/// How a turn ended, for the children it leaves running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ended {
    Completed,
    Failed,
    Canceled,
}

/// The parent's `Task` call, kept for the card.
struct Call {
    name: String,
    status: ToolCallStatus,
    error: Value,
    output: Option<Value>,
}

struct Child {
    title: Option<String>,
    subagent_type: Option<String>,
    description: Option<String>,
    /// The child that started this one; its timeline holds this child's card.
    owner: Option<String>,
    status: SubagentStatus,
    background: bool,
    call: Call,
    /// This child's own open tool calls.
    tools: HashMap<String, ToolUse>,
    /// `(tool call id, log line)`, oldest first.
    log: Vec<(String, String)>,
    /// Calls whose result must not become a card (the handback's acknowledgement).
    skip: HashSet<String>,
    model: Option<String>,
    tokens: Option<u64>,
    subtitle: Option<String>,
}

/// What is known of a child when it is declared.
struct Facts {
    title: Option<String>,
    description: Option<String>,
    subagent_type: Option<String>,
    /// The parent's tool call that started it (`Task`).
    call_name: String,
    /// The child that started it.
    owner: Option<String>,
}

#[derive(Default)]
pub(super) struct Subagents {
    /// `task_id` -> the child's id (its `Task` call id).
    by_task: HashMap<String, String>,
    children: HashMap<String, Child>,
    /// A tool call made inside a child -> that child.
    owner_of_call: HashMap<String, String>,
    /// The CLI announced a task: it announces all of them.
    announces: bool,
    /// At least one child was declared by the task protocol (which then owns completion).
    protocol: bool,
}

fn event(e: SubagentEvent) -> StreamEvent {
    StreamEvent::ProviderSubagent { provider: ID.into(), event: e }
}

fn status_event(id: &str, status: SubagentStatus) -> StreamEvent {
    event(SubagentEvent::status(id, status))
}

fn text(v: &Value) -> Option<String> {
    let t = jsstr::trim(v.as_str()?);
    (!t.is_empty()).then(|| t.to_string())
}

/// One line of text: spaces collapsed, cut at [`MAX_TEXT`] (Paseo `normalizeSubAgentText`).
fn one_line(v: &Value) -> Option<String> {
    let collapsed = jsstr::collapse_spaces(v.as_str()?);
    let t = jsstr::trim(&collapsed);
    if t.is_empty() {
        None
    } else if jsstr::len(t) <= MAX_TEXT {
        Some(t.to_string())
    } else {
        Some(format!("{}...", jsstr::slice(t, 0, MAX_TEXT)))
    }
}

/// Paseo `mapTaskStatus`: paused and pending are still running; killed is a cancellation.
fn task_status(status: &Value) -> Option<SubagentStatus> {
    Some(match status.as_str()? {
        "pending" | "running" | "paused" => SubagentStatus::Running,
        "completed" => SubagentStatus::Completed,
        "failed" => SubagentStatus::Failed,
        "killed" | "stopped" => SubagentStatus::Canceled,
        _ => return None,
    })
}

fn is_subagent_task(msg: &Value) -> bool {
    match msg["task_type"].as_str() {
        Some(t) => t == "local_agent" || t == "local_workflow",
        None => text(&msg["subagent_type"]).is_some(),
    }
}

/// Paseo `formatTokens`.
fn tokens_text(total: u64) -> String {
    if total < 1000 {
        format!("{total} tokens")
    } else {
        format!("{}k tokens", (total as f64 / 100.0).round() / 10.0)
    }
}

impl Child {
    /// `Explore · claude-opus-5-5 · 1.2k tokens` (Paseo `buildClaudeSubagentSubtitle`, without the effort).
    fn make_subtitle(&self) -> Option<String> {
        let parts: Vec<String> = [self.title.clone(), self.model.clone(), self.tokens.filter(|t| *t > 0).map(tokens_text)].into_iter().flatten().collect();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }

    /// The `log` of the card: what the child did, then (once there is one) its report.
    fn card_log(&self) -> String {
        let mut lines: Vec<String> = self.log.iter().map(|(_, l)| l.clone()).collect();
        if let Some(report) = self.call.output.as_ref().map(tool_result_text) {
            let report = jsstr::trim(&report);
            if !report.is_empty() {
                lines.push(report.to_string());
            }
        }
        lines.join("\n")
    }
}

impl Subagents {
    fn find_call(&self, tools: &HashMap<String, ToolUse>, call_id: &str) -> Option<ToolUse> {
        tools.get(call_id).cloned().or_else(|| self.owner_of_call.get(call_id).and_then(|o| self.children.get(o)).and_then(|c| c.tools.get(call_id).cloned()))
    }

    /// Where a child's card goes: the parent's timeline, or its owner's.
    fn card_event(&self, id: &str) -> Option<StreamEvent> {
        let c = self.children.get(id)?;
        let detail = ToolCallDetail::SubAgent {
            sub_agent_type: c.subagent_type.clone().or_else(|| c.title.clone()),
            description: c.description.clone(),
            child_session_id: None,
            log: c.card_log(),
            actions: Some(Vec::new()),
        };
        let item = tool_item(id, &c.call.name, detail, c.call.status, c.call.error.clone());
        Some(match &c.owner {
            Some(owner) => event(SubagentEvent::Timeline { id: owner.clone(), item }),
            None => StreamEvent::Timeline { provider: ID.into(), item, turn_id: None },
        })
    }

    fn timeline(id: &str, item: TimelineItem) -> StreamEvent {
        event(SubagentEvent::Timeline { id: id.into(), item })
    }

    fn set_status(&mut self, id: &str, status: SubagentStatus, out: &mut Vec<StreamEvent>) {
        if let Some(c) = self.children.get_mut(id) {
            if c.status != status {
                c.status = status;
                out.push(status_event(id, status));
            }
        }
    }

    fn refresh_subtitle(&mut self, id: &str, out: &mut Vec<StreamEvent>) {
        let Some(c) = self.children.get_mut(id) else { return };
        let subtitle = c.make_subtitle();
        if subtitle.is_some() && subtitle != c.subtitle {
            c.subtitle = subtitle.clone();
            out.push(event(SubagentEvent::Upsert {
                id: id.into(),
                title: None,
                description: None,
                status: None,
                tool_call_id: None,
                cwd: None,
                subtitle,
                parent_subagent_id: None,
            }));
        }
    }

    fn declare(&mut self, id: &str, facts: Facts, out: &mut Vec<StreamEvent>) {
        let Facts { title, description, subagent_type, call_name, owner } = facts;
        let child = Child {
            subtitle: title.clone(),
            title: title.clone(),
            subagent_type,
            description: description.clone(),
            owner: owner.clone(),
            status: SubagentStatus::Running,
            background: false,
            call: Call { name: call_name, status: ToolCallStatus::Running, error: Value::Null, output: None },
            tools: HashMap::new(),
            log: Vec::new(),
            skip: HashSet::new(),
            model: None,
            tokens: None,
        };
        self.children.insert(id.into(), child);
        out.push(event(SubagentEvent::Upsert {
            id: id.into(),
            title: title.clone(),
            description,
            status: Some(SubagentStatus::Running),
            tool_call_id: Some(id.into()),
            cwd: None,
            subtitle: title,
            parent_subagent_id: owner,
        }));
    }

    /// A `system` message. `tools` is the parent's open tool calls (where the `Task` call's input is).
    pub(super) fn observe_system(&mut self, msg: &Value, tools: &HashMap<String, ToolUse>) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        match msg["subtype"].as_str() {
            Some("task_started") => self.task_started(msg, tools, &mut out),
            Some("task_updated") => {
                let Some(id) = msg["task_id"].as_str().and_then(|t| self.by_task.get(t)).cloned() else { return out };
                if let (Some(c), Some(bg)) = (self.children.get_mut(&id), msg["patch"]["is_backgrounded"].as_bool()) {
                    c.background = bg;
                }
                if let Some(status) = task_status(&msg["patch"]["status"]) {
                    self.set_status(&id, status, &mut out);
                }
            }
            Some("task_notification") | Some("task_progress") => {
                let Some(id) = msg["task_id"].as_str().and_then(|t| self.by_task.get(t)).cloned() else { return out };
                if let (Some(c), Some(total)) = (self.children.get_mut(&id), msg["usage"]["total_tokens"].as_u64()) {
                    c.tokens = Some(total);
                }
                self.refresh_subtitle(&id, &mut out);
                if msg["subtype"] == "task_notification" {
                    if let Some(status) = task_status(&msg["status"]) {
                        self.set_status(&id, status, &mut out);
                    }
                }
            }
            _ => {}
        }
        out
    }

    fn task_started(&mut self, msg: &Value, tools: &HashMap<String, ToolUse>, out: &mut Vec<StreamEvent>) {
        // Recorded before the filter: this CLI announces its tasks.
        self.announces = true;
        let (Some(task_id), Some(id)) = (s(&msg["task_id"]), text(&msg["tool_use_id"])) else { return };
        if msg["skip_transcript"] == true || !is_subagent_task(msg) {
            return;
        }
        self.protocol = true;
        let workflow = msg["task_type"] == "local_workflow";
        let prompt = if workflow { text(&msg["description"]) } else { text(&msg["prompt"]) };
        if let Some(existing) = self.by_task.get(&task_id).cloned() {
            // The same task asked again: running once more, with a new prompt.
            self.set_status(&existing, SubagentStatus::Running, out);
            if let Some(p) = prompt {
                out.push(Self::timeline(&existing, TimelineItem::UserMessage { text: p, message_id: None, client_message_id: None }));
            }
            return;
        }
        let call = self.find_call(tools, &id);
        let input = call.as_ref().map(|c| c.input.clone()).unwrap_or(Value::Null);
        // An explicit `name` on the call wins over the type: five Explores must not read as five identical rows.
        let title = if workflow { Some("Workflow".to_string()) } else { one_line(&input["name"]).or_else(|| text(&msg["subagent_type"])) };
        let owner = self.owner_of_call.get(&id).cloned();
        let name = call.map(|c| c.name).unwrap_or_else(|| "Task".into());
        self.by_task.insert(task_id, id.clone());
        let facts = Facts { title, description: one_line(&msg["description"]), subagent_type: text(&msg["subagent_type"]), call_name: name, owner };
        self.declare(&id, facts, out);
        if msg["is_backgrounded"] == true {
            if let Some(c) = self.children.get_mut(&id) {
                c.background = true;
            }
        }
        // The pane opens with the task it was given, not mid-conversation.
        if let Some(p) = prompt {
            out.push(Self::timeline(&id, TimelineItem::UserMessage { text: p, message_id: None, client_message_id: None }));
        }
    }

    /// A frame from inside the subagent started by `Task` call `parent_id`.
    pub(super) fn sidechain(&mut self, msg: &Value, parent_id: &str, tools: &HashMap<String, ToolUse>) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        if !self.children.contains_key(parent_id) {
            // Once a CLI announces its tasks, a frame for one it never declared is work the
            // filter rejected (a workflow's child, housekeeping): attribute it to nobody.
            if self.announces {
                return out;
            }
            // An older CLI: the first frame of the sidechain declares the child.
            let call = self.find_call(tools, parent_id);
            let input = call.as_ref().map(|c| c.input.clone()).unwrap_or(Value::Null);
            let title = one_line(&input["name"]).or_else(|| text(&input["subagent_type"]));
            let owner = self.owner_of_call.get(parent_id).cloned();
            let name = call.map(|c| c.name).unwrap_or_else(|| "Task".into());
            let facts = Facts { title, description: one_line(&input["description"]), subagent_type: text(&input["subagent_type"]), call_name: name, owner };
            self.declare(parent_id, facts, &mut out);
        }
        match msg["type"].as_str() {
            Some("assistant") => self.sidechain_assistant(msg, parent_id, &mut out),
            Some("user") => self.sidechain_user(msg, parent_id, &mut out),
            _ => {}
        }
        out
    }

    fn sidechain_assistant(&mut self, msg: &Value, id: &str, out: &mut Vec<StreamEvent>) {
        let message_id = s(&msg["message"]["id"]);
        let mut log_changed = false;
        for block in msg["message"]["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") => {
                    if let Some(t) = text(&block["text"]) {
                        out.push(Self::timeline(id, TimelineItem::AssistantMessage { text: t, message_id: message_id.clone() }));
                    }
                }
                Some("thinking") => {
                    if let Some(t) = text(&block["thinking"]) {
                        out.push(Self::timeline(id, TimelineItem::Reasoning { text: t }));
                    }
                }
                Some("tool_use") | Some("mcp_tool_use") | Some("server_tool_use") => {
                    let call_id = s(&block["id"]).unwrap_or_default();
                    let name = s(&block["name"]).unwrap_or_default();
                    let input = block["input"].clone();
                    let Some(c) = self.children.get_mut(id) else { return };
                    if name == HANDBACK {
                        // The report is the child's last message; the call and its acknowledgement are not cards.
                        c.skip.insert(call_id);
                        if let Some(report) = text(&input["message"]) {
                            out.push(Self::timeline(id, TimelineItem::AssistantMessage { text: report, message_id: message_id.clone() }));
                        }
                        continue;
                    }
                    if name == "TodoWrite" {
                        out.push(Self::timeline(id, TimelineItem::Todo { items: todo_items(&input) }));
                    }
                    let detail = detail_for(&name, &input, None);
                    let display = tool_call_display(&ToolCallDisplayInput { name: &name, detail: &detail, metadata: None, agent_message: None, cwd: None, target_agent_title: None });
                    let summary = display.summary.as_deref().and_then(|t| one_line(&json!(t)));
                    let line = match summary {
                        Some(sum) => format!("[{name}] {sum}"),
                        None => format!("[{name}]"),
                    };
                    if c.log.iter().all(|(k, _)| *k != call_id) {
                        c.log.push((call_id.clone(), line));
                        if c.log.len() > MAX_LOG_ENTRIES {
                            c.log.remove(0);
                        }
                        log_changed = true;
                    }
                    out.push(Self::timeline(id, tool_item(&call_id, &name, detail, ToolCallStatus::Running, Value::Null)));
                    c.tools.insert(call_id.clone(), ToolUse { name, input });
                    self.owner_of_call.insert(call_id, id.to_string());
                }
                _ => {}
            }
        }
        // The child's own model, read per frame (a provider can swap it mid-flight).
        if let Some(model) = text(&msg["message"]["model"]).filter(|m| !m.starts_with('<')) {
            if let Some(c) = self.children.get_mut(id) {
                c.model = Some(model);
            }
            self.refresh_subtitle(id, out);
        }
        if log_changed {
            out.extend(self.card_event(id));
        }
    }

    fn sidechain_user(&mut self, msg: &Value, id: &str, out: &mut Vec<StreamEvent>) {
        for block in msg["message"]["content"].as_array().into_iter().flatten() {
            if block["type"].as_str().is_none_or(|t| !t.ends_with("tool_result")) {
                // The prompt: task_started carried it. Without that protocol, it opens the timeline here.
                if !self.announces && block["type"] == "text" {
                    if let Some(t) = text(&block["text"]) {
                        out.push(Self::timeline(id, TimelineItem::UserMessage { text: t, message_id: None, client_message_id: None }));
                    }
                }
                continue;
            }
            let call_id = s(&block["tool_use_id"]).unwrap_or_default();
            let failed = block["is_error"] == true;
            let output = &block["content"];
            let (status, error) = if failed { (ToolCallStatus::Failed, json!(tool_result_text(output))) } else { (ToolCallStatus::Completed, Value::Null) };
            // A call that started a child of its own: that child's card.
            if let Some(events) = self.settle(&call_id, status, &error, Some(output)) {
                out.extend(events);
                continue;
            }
            let Some(c) = self.children.get_mut(id) else { return };
            if c.skip.remove(&call_id) {
                continue;
            }
            let Some(tool) = c.tools.remove(&call_id) else { continue };
            self.owner_of_call.remove(&call_id);
            out.push(Self::timeline(id, tool_item(&call_id, &tool.name, detail_for(&tool.name, &tool.input, Some(output)), status, error)));
        }
    }

    /// The `Task` call `call_id` got its result (or was left open at the end of the turn).
    /// `None` when it is not a subagent's call: the caller records it as any tool call.
    pub(super) fn settle(&mut self, call_id: &str, status: ToolCallStatus, error: &Value, output: Option<&Value>) -> Option<Vec<StreamEvent>> {
        let c = self.children.get_mut(call_id)?;
        c.call.status = status;
        c.call.error = error.clone();
        if output.is_some() {
            c.call.output = output.cloned();
        }
        let mut out: Vec<StreamEvent> = self.card_event(call_id).into_iter().collect();
        // A foreground call returns when its child is done; the task protocol says so too,
        // but its notice may come later (or, from an old CLI, not at all). A background
        // call returns at once and its child keeps going.
        let done = match status {
            ToolCallStatus::Failed => Some(SubagentStatus::Failed),
            ToolCallStatus::Canceled => Some(SubagentStatus::Canceled),
            ToolCallStatus::Completed => Some(SubagentStatus::Completed),
            ToolCallStatus::Running => None,
        };
        let background = self.children.get(call_id).is_some_and(|c| c.background);
        if let (Some(done), false) = (done, background) {
            self.set_status(call_id, done, &mut out);
        }
        Some(out)
    }

    /// The turn ended. A child still running with no protocol to say otherwise ends with it; with the
    /// protocol, only an interrupt ends the foreground ones (a background one outlives the turn).
    pub(super) fn turn_ended(&mut self, ended: Ended) -> Vec<StreamEvent> {
        let status = match ended {
            Ended::Completed => SubagentStatus::Completed,
            Ended::Failed => SubagentStatus::Failed,
            Ended::Canceled => SubagentStatus::Canceled,
        };
        if self.protocol && ended != Ended::Canceled {
            return Vec::new();
        }
        let ids: Vec<String> = self.children.iter().filter(|(_, c)| c.status == SubagentStatus::Running && !(self.protocol && c.background)).map(|(i, _)| i.clone()).collect();
        let mut out = Vec::new();
        for id in ids {
            self.set_status(&id, status, &mut out);
        }
        out
    }

    /// The Claude process went away: nothing it ran is still running.
    pub(super) fn lost(&mut self) -> Vec<StreamEvent> {
        let ids: Vec<String> = self.children.iter().filter(|(_, c)| c.status == SubagentStatus::Running).map(|(i, _)| i.clone()).collect();
        let mut out = Vec::new();
        for id in ids {
            self.set_status(&id, SubagentStatus::Failed, &mut out);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::super::claude::sidechain_parent;
    use super::*;

    const FIXTURE: &str = include_str!("../../tests/fixtures/claude-task-subagent.jsonl");

    /// What the Claude session does with each frame, around the tracker.
    struct Driver {
        subs: Subagents,
        tools: HashMap<String, ToolUse>,
        events: Vec<StreamEvent>,
    }

    impl Driver {
        fn new() -> Self {
            Driver { subs: Subagents::default(), tools: HashMap::new(), events: Vec::new() }
        }

        fn feed(&mut self, msg: &Value) {
            if let Some(parent) = sidechain_parent(msg) {
                self.events.extend(self.subs.sidechain(msg, &parent, &self.tools));
                return;
            }
            match msg["type"].as_str() {
                Some("system") => self.events.extend(self.subs.observe_system(msg, &self.tools)),
                Some("assistant") => {
                    for b in msg["message"]["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "tool_use") {
                        self.tools.insert(s(&b["id"]).unwrap(), ToolUse { name: s(&b["name"]).unwrap(), input: b["input"].clone() });
                    }
                }
                Some("user") => {
                    for b in msg["message"]["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "tool_result") {
                        let id = s(&b["tool_use_id"]).unwrap();
                        self.tools.remove(&id);
                        let (status, error) = if b["is_error"] == true { (ToolCallStatus::Failed, json!("boom")) } else { (ToolCallStatus::Completed, Value::Null) };
                        self.events.extend(self.subs.settle(&id, status, &error, Some(&b["content"])).unwrap_or_default());
                    }
                }
                _ => {}
            }
        }

        fn feed_lines(&mut self, lines: &str) {
            for l in lines.lines().filter(|l| !l.trim().is_empty()) {
                self.feed(&serde_json::from_str(l).unwrap());
            }
        }

        fn take(&mut self) -> Vec<StreamEvent> {
            std::mem::take(&mut self.events)
        }
    }

    /// Subagent timeline items as `kind: text` strings, one per item of child `id`.
    fn child_items(events: &[StreamEvent], id: &str) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ProviderSubagent { event: SubagentEvent::Timeline { id: i, item }, .. } if i == id => Some(match item {
                    TimelineItem::UserMessage { text, .. } => format!("user: {text}"),
                    TimelineItem::AssistantMessage { text, .. } => format!("assistant: {text}"),
                    TimelineItem::Reasoning { text } => format!("reasoning: {text}"),
                    TimelineItem::ToolCall(c) => format!("tool {} {} {}", c.call_id, c.name, c.status.as_str()),
                    other => other.kind().to_string(),
                }),
                _ => None,
            })
            .collect()
    }

    fn upserts(events: &[StreamEvent]) -> Vec<Value> {
        events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ProviderSubagent { event: ev @ SubagentEvent::Upsert { .. }, .. } => Some(serde_json::to_value(ev).unwrap()),
                _ => None,
            })
            .collect()
    }

    /// The parent-timeline cards of call `id`: `(status, log)`.
    fn cards(events: &[StreamEvent], id: &str) -> Vec<(String, String)> {
        events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::Timeline { item: TimelineItem::ToolCall(c), .. } if c.call_id == id => match &c.detail {
                    ToolCallDetail::SubAgent { log, .. } => Some((c.status.as_str().to_string(), log.clone())),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_task_run_builds_a_timeline_and_a_card() {
        let mut d = Driver::new();
        d.feed_lines(FIXTURE);
        let events = d.take();
        let id = "toolu_01TaskListFiles";
        assert_eq!(
            child_items(&events, id),
            [
                "user: Run `ls` in the current directory with Bash and report the file names in one short sentence.",
                "assistant: I'll list the directory.",
                "tool toolu_01SubBash Bash running",
                "tool toolu_01SubBash Bash completed",
                "assistant: The directory has a.txt and b.txt.",
            ]
        );
        // The sidechain's streamed deltas and prompt frame are in no timeline: only the card is the parent's.
        assert_eq!(events.iter().filter(|e| matches!(e, StreamEvent::Timeline { .. })).count(), 2, "the parent's card, twice");
        assert_eq!(
            cards(&events, id),
            [
                ("running".to_string(), "[Bash] ls".to_string()),
                ("completed".to_string(), "[Bash] ls\nThe directory has a.txt and b.txt.".to_string()),
            ]
        );
        let ups = upserts(&events);
        assert_eq!(
            ups[0],
            json!({"type":"upsert","id":id,"title":"general-purpose","description":"List files","status":"running","toolCallId":id,"subtitle":"general-purpose"})
        );
        // Subtitle: the model from the child's first frame, then its token total from the protocol.
        let subtitles: Vec<&str> = ups.iter().filter_map(|u| u["subtitle"].as_str()).collect();
        assert_eq!(
            subtitles,
            ["general-purpose", "general-purpose · claude-opus-5-5", "general-purpose · claude-opus-5-5 · 4.2k tokens", "general-purpose · claude-opus-5-5 · 5.1k tokens"]
        );
        // One completion, from the protocol; the Task result does not repeat it.
        let completions = ups.iter().filter(|u| u["status"] == "completed").count();
        assert_eq!(completions, 1, "{ups:?}");
    }

    #[test]
    fn a_task_name_titles_the_child_and_other_tasks_are_not_subagents() {
        let mut d = Driver::new();
        d.feed(&json!({"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"t1","name":"Task","input":{"name":"scout","subagent_type":"Explore","description":"look   around","prompt":"p"}}]},"parent_tool_use_id":null}));
        d.feed(&json!({"type":"system","subtype":"task_started","task_id":"k1","tool_use_id":"t1","task_type":"local_agent","subagent_type":"Explore","description":"look   around","prompt":"p"}));
        // A backgrounded shell and housekeeping announce tasks too.
        d.feed(&json!({"type":"system","subtype":"task_started","task_id":"k2","tool_use_id":"b1","task_type":"local_bash","description":"sleep 20"}));
        d.feed(&json!({"type":"system","subtype":"task_started","task_id":"k3","tool_use_id":"h1","task_type":"local_agent","subagent_type":"x","skip_transcript":true}));
        let ups = upserts(&d.take());
        assert_eq!(ups.len(), 1);
        assert_eq!((ups[0]["title"].as_str(), ups[0]["description"].as_str()), (Some("scout"), Some("look around")));
        // Once tasks are announced, a frame for one that was never declared is dropped.
        d.feed(&json!({"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"stray"}]},"parent_tool_use_id":"b1"}));
        assert!(d.take().is_empty());
    }

    #[test]
    fn an_older_cli_declares_on_the_first_frame_and_the_result_ends_it() {
        let mut d = Driver::new();
        d.feed(&json!({"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"t1","name":"Task","input":{"subagent_type":"Explore","description":"look","prompt":"find it"}}]},"parent_tool_use_id":null}));
        d.feed(&json!({"type":"user","message":{"content":[{"type":"text","text":"find it"}]},"parent_tool_use_id":"t1"}));
        d.feed(&json!({"type":"assistant","message":{"id":"s1","content":[{"type":"thinking","thinking":"hm"},{"type":"tool_use","id":"u1","name":"Grep","input":{"pattern":"foo"}}]},"parent_tool_use_id":"t1"}));
        d.feed(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"u1","content":"x.rs:1:foo","is_error":false}]},"parent_tool_use_id":"t1"}));
        d.feed(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"found it"}]},"parent_tool_use_id":null}));
        let events = d.take();
        assert_eq!(child_items(&events, "t1"), ["user: find it", "reasoning: hm", "tool u1 Grep running", "tool u1 Grep completed"]);
        let ups = upserts(&events);
        assert_eq!(ups[0]["title"], "Explore");
        assert_eq!(ups.last().unwrap()["status"], "completed");
        assert_eq!(cards(&events, "t1").last().unwrap(), &("completed".to_string(), "[Grep] foo\nfound it".to_string()));
    }

    #[test]
    fn a_handback_is_the_last_message_and_its_acknowledgement_is_no_card() {
        let mut d = Driver::new();
        d.feed(&json!({"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"t1","name":"Agent","input":{"description":"d","subagent_type":"general-purpose"}}]},"parent_tool_use_id":null}));
        d.feed(&json!({"type":"system","subtype":"task_started","task_id":"k1","tool_use_id":"t1","task_type":"local_agent","subagent_type":"general-purpose","description":"d","prompt":"go"}));
        d.feed(&json!({"type":"assistant","message":{"id":"s1","content":[{"type":"tool_use","id":"h1","name":"SubagentHandback","input":{"message":"All done: 3 files."}}]},"parent_tool_use_id":"t1"}));
        d.feed(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"h1","content":"ok"}]},"parent_tool_use_id":"t1"}));
        let events = d.take();
        assert_eq!(child_items(&events, "t1"), ["user: go", "assistant: All done: 3 files."]);
        assert!(cards(&events, "t1").is_empty(), "the handback is not a tool call of the child's log");
    }

    #[test]
    fn a_child_started_inside_a_child_keeps_its_card_in_the_parent_childs_timeline() {
        let mut d = Driver::new();
        d.feed(&json!({"type":"assistant","message":{"id":"m","content":[{"type":"tool_use","id":"A","name":"Task","input":{"description":"outer","subagent_type":"general-purpose"}}]},"parent_tool_use_id":null}));
        d.feed(&json!({"type":"system","subtype":"task_started","task_id":"kA","tool_use_id":"A","task_type":"local_agent","subagent_type":"general-purpose","description":"outer","prompt":"go"}));
        d.feed(&json!({"type":"assistant","message":{"id":"s1","content":[{"type":"tool_use","id":"B","name":"Task","input":{"description":"inner","subagent_type":"Explore"}}]},"parent_tool_use_id":"A"}));
        d.feed(&json!({"type":"system","subtype":"task_started","task_id":"kB","tool_use_id":"B","task_type":"local_agent","subagent_type":"Explore","description":"inner","prompt":"dig"}));
        d.feed(&json!({"type":"assistant","message":{"id":"s2","content":[{"type":"tool_use","id":"C","name":"Bash","input":{"command":"pwd"}}]},"parent_tool_use_id":"B"}));
        d.feed(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"C","content":"/w"}]},"parent_tool_use_id":"B"}));
        d.feed(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"B","content":"inner done"}]},"parent_tool_use_id":"A"}));
        let events = d.take();
        let ups = upserts(&events);
        let inner = ups.iter().find(|u| u["id"] == "B").expect("inner declared");
        assert_eq!(inner["parentSubagentId"], "A");
        assert_eq!(child_items(&events, "B"), ["user: dig", "tool C Bash running", "tool C Bash completed"]);
        // A's timeline holds the Task call that started B (its card as B works, and when B returns).
        let in_a = child_items(&events, "A");
        assert_eq!(in_a.iter().filter(|i| i.starts_with("tool B Task")).count(), 3, "the call, the card with B's log, the final card: {in_a:?}");
        assert!(cards(&events, "B").is_empty(), "B's card is not in the parent's timeline");
    }

    #[test]
    fn a_turn_end_leaves_background_children_and_an_interrupt_cancels_the_rest() {
        let mut d = Driver::new();
        for (call, task, bg) in [("fg", "k1", false), ("bg", "k2", true)] {
            d.feed(&json!({"type":"assistant","message":{"id":call,"content":[{"type":"tool_use","id":call,"name":"Task","input":{"description":"d","subagent_type":"general-purpose"}}]},"parent_tool_use_id":null}));
            d.feed(&json!({"type":"system","subtype":"task_started","task_id":task,"tool_use_id":call,"task_type":"local_agent","subagent_type":"general-purpose","description":"d","prompt":"p","is_backgrounded":bg}));
        }
        d.take();
        assert!(d.subs.turn_ended(Ended::Completed).is_empty(), "the protocol owns completion");
        let canceled = d.subs.turn_ended(Ended::Canceled);
        let ids: Vec<&str> = canceled.iter().map(|e| if let StreamEvent::ProviderSubagent { event, .. } = e { event.id() } else { "" }).collect();
        assert_eq!(ids, ["fg"]);
        // The process dying takes the background child too.
        assert_eq!(d.subs.lost().len(), 1);
        // A foreground call's result completes it even if its notice never comes; a background call's does not.
        let mut d = Driver::new();
        for (call, task, bg) in [("fg", "k1", false), ("bg", "k2", true)] {
            d.feed(&json!({"type":"assistant","message":{"id":call,"content":[{"type":"tool_use","id":call,"name":"Task","input":{}}]},"parent_tool_use_id":null}));
            d.feed(&json!({"type":"system","subtype":"task_started","task_id":task,"tool_use_id":call,"task_type":"local_agent","subagent_type":"x","description":"d","prompt":"p","is_backgrounded":bg}));
            d.feed(&json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":call,"content":"r"}]},"parent_tool_use_id":null}));
        }
        let statuses: Vec<(String, String)> = upserts(&d.take()).iter().filter(|u| u["status"] != "running").map(|u| (u["id"].as_str().unwrap().into(), u["status"].as_str().unwrap().into())).collect();
        assert_eq!(statuses, [("fg".to_string(), "completed".to_string())]);
    }

    #[test]
    fn heartbeats_and_other_frames_are_not_sidechains() {
        assert_eq!(sidechain_parent(&json!({"type":"tool_progress","parent_tool_use_id":"t1","heartbeat":true})), None);
        assert_eq!(sidechain_parent(&json!({"type":"system","subtype":"task_started","parent_tool_use_id":"t1"})), None);
        assert_eq!(sidechain_parent(&json!({"type":"assistant","parent_tool_use_id":null})), None);
        assert_eq!(sidechain_parent(&json!({"type":"assistant","parent_tool_use_id":""})), None);
        assert_eq!(sidechain_parent(&json!({"type":"stream_event","parent_tool_use_id":"t1"})).as_deref(), Some("t1"));
    }
}
