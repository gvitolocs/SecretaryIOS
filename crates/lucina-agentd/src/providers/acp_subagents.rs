//! Child sessions of an ACP agent (Paseo `opencode-agent.js`: `appendOpenCodeChildSessionDetected`,
//! `linkOpenCodeSubAgentChildSession`).
//!
//! OpenCode runs a `task` tool call as a child session of its own. Paseo drives OpenCode over its
//! HTTP API and sees every session; here the agent speaks ACP, where a child session's
//! `session/update` notifications carry the child's `sessionId` instead of the agent's. Those go to a
//! provider subagent keyed by that session id, as a Claude `Task` does:
//!
//! - **The call** is the agent's `task` tool call (its `rawInput` has `subagent_type`, `description`
//!   and `prompt`). It links to the child by `rawOutput.metadata.sessionId`, or, as Paseo does, when a
//!   child's first update arrives while exactly one such call is running and unlinked. The link gives
//!   the descriptor its title (the subagent type), description and `toolCallId`, and its timeline opens
//!   with the prompt.
//! - **The child's timeline** is its message and thought chunks (joined into one item until another
//!   kind of update arrives), tool calls (as the agent's own) and plan.
//! - **It ends** when the linked call completes or fails, or with the turn.
//!
//! Not checked against a live OpenCode: it may keep the child's updates to itself, in which case only
//! the agent's `task` card (a `sub_agent` with the child's session id) shows the work.

use std::collections::HashMap;

use lucina_agents::subagent::{SubagentEvent, SubagentStatus};
use lucina_agents::timeline::{TodoEntry, TodoStatus, ToolCall, ToolCallDetail, ToolCallStatus};
use lucina_agents::{StreamEvent, TimelineItem};
use serde_json::Value;

use super::acp::{acp_detail, content_text, s, tool_status};

/// How a turn ended, for the children it leaves running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ended {
    Completed,
    Failed,
    Canceled,
}

/// The agent's `task` call: OpenCode names the tool `task`, with the child's brief as input.
pub(super) fn is_task_call(update: &Value) -> bool {
    let raw = &update["rawInput"];
    update["title"].as_str().is_some_and(|t| t.eq_ignore_ascii_case("task")) || (raw["subagent_type"].is_string() && raw["prompt"].is_string())
}

struct Call {
    input: Value,
    child: Option<String>,
}

#[derive(Default)]
struct Child {
    status: Option<SubagentStatus>,
    /// Chunks of one kind not yet an item: `(is_thought, text)`.
    buffer: Option<(bool, String)>,
    tools: HashMap<String, (String, ToolCallDetail)>,
}

#[derive(Default)]
pub(super) struct Children {
    calls: HashMap<String, Call>,
    children: HashMap<String, Child>,
}

fn event(provider: &str, e: SubagentEvent) -> StreamEvent {
    StreamEvent::ProviderSubagent { provider: provider.into(), event: e }
}

impl Children {
    fn flush(&mut self, provider: &str, id: &str, out: &mut Vec<StreamEvent>) {
        let Some((thought, text)) = self.children.get_mut(id).and_then(|c| c.buffer.take()) else { return };
        let text = text.trim().to_string();
        if !text.is_empty() {
            let item = if thought { TimelineItem::Reasoning { text } } else { TimelineItem::AssistantMessage { text, message_id: None } };
            out.push(event(provider, SubagentEvent::Timeline { id: id.into(), item }));
        }
    }

    fn finish(&mut self, provider: &str, id: &str, status: SubagentStatus, out: &mut Vec<StreamEvent>) {
        self.flush(provider, id, out);
        if let Some(c) = self.children.get_mut(id) {
            if c.status != Some(status) {
                c.status = Some(status);
                out.push(event(provider, SubagentEvent::status(id, status)));
            }
        }
    }

    /// Declare child `id`, linked to `call_id` (its brief opens the timeline).
    fn declare(&mut self, provider: &str, id: &str, call_id: Option<&str>, out: &mut Vec<StreamEvent>) {
        let input = call_id.and_then(|c| self.calls.get(c)).map(|c| c.input.clone()).unwrap_or(Value::Null);
        let title = s(&input["subagent_type"]).filter(|t| !t.is_empty());
        let description = s(&input["description"]).filter(|t| !t.is_empty());
        let child = self.children.entry(id.to_string()).or_default();
        child.status = Some(SubagentStatus::Running);
        if let Some(c) = call_id.and_then(|c| self.calls.get_mut(c)) {
            c.child = Some(id.to_string());
        }
        out.push(event(
            provider,
            SubagentEvent::Upsert {
                id: id.into(),
                subtitle: title.clone(),
                title,
                description,
                status: Some(SubagentStatus::Running),
                tool_call_id: call_id.map(str::to_string),
                cwd: None,
                parent_subagent_id: None,
            },
        ));
        if let Some(prompt) = s(&input["prompt"]).filter(|p| !p.trim().is_empty()) {
            out.push(event(provider, SubagentEvent::Timeline { id: id.into(), item: TimelineItem::UserMessage { text: prompt, message_id: None, client_message_id: None } }));
        }
    }

    /// The agent's own `task` call changed (`update` is its `tool_call` / `tool_call_update`).
    pub(super) fn note_call(&mut self, provider: &str, call_id: &str, update: &Value) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        let entry = self.calls.entry(call_id.to_string()).or_insert(Call { input: Value::Null, child: None });
        if update["rawInput"].is_object() {
            entry.input = update["rawInput"].clone();
        }
        let linked = entry.child.clone();
        if linked.is_none() {
            if let Some(id) = s(&update["rawOutput"]["metadata"]["sessionId"]).or_else(|| s(&update["_meta"]["sessionId"])).filter(|i| !i.is_empty()) {
                self.declare(provider, &id, Some(call_id), &mut out);
            }
        }
        let child = self.calls.get(call_id).and_then(|c| c.child.clone());
        match (child, tool_status(&update["status"])) {
            (Some(id), Some(ToolCallStatus::Completed)) => self.finish(provider, &id, SubagentStatus::Completed, &mut out),
            (Some(id), Some(ToolCallStatus::Failed)) => self.finish(provider, &id, SubagentStatus::Failed, &mut out),
            _ => {}
        }
        out
    }

    /// A `session/update` from child session `id`.
    pub(super) fn child_update(&mut self, provider: &str, id: &str, update: &Value, running_calls: &[String]) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        if !self.children.contains_key(id) {
            // Paseo: a child that appears while exactly one task call waits for one belongs to it.
            let waiting: Vec<&String> = running_calls.iter().filter(|c| self.calls.get(*c).is_some_and(|k| k.child.is_none())).collect();
            let call = (waiting.len() == 1).then(|| waiting[0].clone());
            self.declare(provider, id, call.as_deref(), &mut out);
        }
        match update["sessionUpdate"].as_str() {
            Some(kind @ ("agent_message_chunk" | "agent_thought_chunk")) => {
                let thought = kind == "agent_thought_chunk";
                let text = content_text(&update["content"]);
                let child = self.children.entry(id.to_string()).or_default();
                if child.buffer.as_ref().is_some_and(|(t, _)| *t != thought) {
                    self.flush(provider, id, &mut out);
                }
                let child = self.children.entry(id.to_string()).or_default();
                child.buffer.get_or_insert((thought, String::new())).1.push_str(&text);
            }
            Some("tool_call") | Some("tool_call_update") => {
                self.flush(provider, id, &mut out);
                let call_id = s(&update["toolCallId"]).unwrap_or_default();
                let child = self.children.entry(id.to_string()).or_default();
                let prev = child.tools.get(&call_id);
                let name = s(&update["title"]).or_else(|| prev.map(|p| p.0.clone())).unwrap_or_else(|| "tool".into());
                let detail = acp_detail(update, prev.map(|p| &p.1));
                let status = tool_status(&update["status"]).unwrap_or(ToolCallStatus::Running);
                let error = if status == ToolCallStatus::Failed {
                    let t = content_text(&update["content"]);
                    serde_json::json!(if t.is_empty() { "failed".to_string() } else { t })
                } else {
                    Value::Null
                };
                if matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed) {
                    child.tools.remove(&call_id);
                } else {
                    child.tools.insert(call_id.clone(), (name.clone(), detail.clone()));
                }
                let item = TimelineItem::ToolCall(Box::new(ToolCall { agent_message: None, call_id, name, detail, metadata: None, status, error }));
                out.push(event(provider, SubagentEvent::Timeline { id: id.into(), item }));
            }
            Some("plan") => {
                self.flush(provider, id, &mut out);
                let items = update["entries"]
                    .as_array()
                    .map(|e| {
                        e.iter()
                            .map(|x| {
                                let status = match x["status"].as_str() {
                                    Some("completed") => TodoStatus::Completed,
                                    Some("in_progress") => TodoStatus::InProgress,
                                    _ => TodoStatus::Pending,
                                };
                                TodoEntry { text: s(&x["content"]).unwrap_or_default(), completed: status == TodoStatus::Completed, id: None, status: Some(status), active_form: None }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(event(provider, SubagentEvent::Timeline { id: id.into(), item: TimelineItem::Todo { items } }));
            }
            _ => {}
        }
        out
    }

    /// The turn ended, or the process went away: children still running end with it.
    pub(super) fn turn_ended(&mut self, provider: &str, ended: Ended) -> Vec<StreamEvent> {
        let status = match ended {
            Ended::Completed => SubagentStatus::Completed,
            Ended::Failed => SubagentStatus::Failed,
            Ended::Canceled => SubagentStatus::Canceled,
        };
        let ids: Vec<String> = self.children.iter().filter(|(_, c)| c.status == Some(SubagentStatus::Running)).map(|(i, _)| i.clone()).collect();
        let mut out = Vec::new();
        for id in ids {
            self.finish(provider, &id, status, &mut out);
        }
        // Chunks of a child that had already ended.
        let rest: Vec<String> = self.children.iter().filter(|(_, c)| c.buffer.is_some()).map(|(i, _)| i.clone()).collect();
        for id in rest {
            self.flush(provider, &id, &mut out);
        }
        self.calls.clear();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn items(events: &[StreamEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ProviderSubagent { event: SubagentEvent::Timeline { id, item }, .. } => Some(match item {
                    TimelineItem::UserMessage { text, .. } => format!("{id} user: {text}"),
                    TimelineItem::AssistantMessage { text, .. } => format!("{id} assistant: {text}"),
                    TimelineItem::Reasoning { text } => format!("{id} reasoning: {text}"),
                    TimelineItem::ToolCall(c) => format!("{id} tool {} {}", c.name, c.status.as_str()),
                    other => format!("{id} {}", other.kind()),
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

    fn chunk(kind: &str, text: &str) -> Value {
        json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}})
    }

    #[test]
    fn a_child_session_links_to_the_only_running_task_call() {
        let mut c = Children::default();
        let task = json!({"toolCallId":"c1","title":"task","kind":"other","status":"in_progress","rawInput":{"subagent_type":"explore","description":"find it","prompt":"look in src"}});
        assert!(is_task_call(&task));
        assert!(c.note_call("opencode", "c1", &task).is_empty());
        let mut ev = c.child_update("opencode", "ses_child", &chunk("agent_thought_chunk", "hmm "), &["c1".into()]);
        ev.extend(c.child_update("opencode", "ses_child", &chunk("agent_thought_chunk", "ok"), &["c1".into()]));
        ev.extend(c.child_update("opencode", "ses_child", &chunk("agent_message_chunk", "Searching "), &["c1".into()]));
        ev.extend(c.child_update("opencode", "ses_child", &chunk("agent_message_chunk", "now."), &["c1".into()]));
        ev.extend(c.child_update("opencode", "ses_child", &json!({"sessionUpdate":"tool_call","toolCallId":"g1","title":"grep","kind":"search","status":"in_progress","rawInput":{"pattern":"foo"}}), &["c1".into()]));
        ev.extend(c.child_update("opencode", "ses_child", &json!({"sessionUpdate":"tool_call_update","toolCallId":"g1","status":"completed"}), &["c1".into()]));
        ev.extend(c.child_update("opencode", "ses_child", &chunk("agent_message_chunk", "Found it."), &["c1".into()]));
        let up = &upserts(&ev)[0];
        assert_eq!(up["id"], "ses_child");
        assert_eq!((up["title"].as_str(), up["description"].as_str(), up["toolCallId"].as_str()), (Some("explore"), Some("find it"), Some("c1")));
        // The call completes: the child is done, its last chunks are an item.
        let done = c.note_call("opencode", "c1", &json!({"toolCallId":"c1","status":"completed","rawOutput":{"output":"Found it.","metadata":{"sessionId":"ses_child"}}}));
        ev.extend(done);
        assert_eq!(
            items(&ev),
            [
                "ses_child user: look in src",
                "ses_child reasoning: hmm ok",
                "ses_child assistant: Searching now.",
                "ses_child tool grep running",
                "ses_child tool grep completed",
                "ses_child assistant: Found it.",
            ]
        );
        assert_eq!(upserts(&ev).last().unwrap()["status"], "completed");
        // Nothing left to end with the turn.
        assert!(c.turn_ended("opencode", Ended::Completed).is_empty());
    }

    #[test]
    fn two_waiting_calls_leave_a_child_unlinked_and_the_metadata_links_it() {
        let mut c = Children::default();
        for id in ["c1", "c2"] {
            c.note_call("opencode", id, &json!({"toolCallId":id,"title":"task","status":"in_progress","rawInput":{"subagent_type":"explore","description":id,"prompt":"p"}}));
        }
        let ev = c.child_update("opencode", "s1", &chunk("agent_message_chunk", "x"), &["c1".into(), "c2".into()]);
        assert_eq!(upserts(&ev)[0]["toolCallId"], Value::Null);
        // The result names its child: c2 declares s2 with its brief.
        let ev = c.note_call("opencode", "c2", &json!({"toolCallId":"c2","status":"completed","rawOutput":{"output":"r","metadata":{"sessionId":"s2"}}}));
        let ups = upserts(&ev);
        assert_eq!((ups[0]["id"].as_str(), ups[0]["toolCallId"].as_str(), ups[0]["description"].as_str()), (Some("s2"), Some("c2"), Some("c2")));
        assert_eq!(ups.last().unwrap()["status"], "completed");
        // The unlinked child ends with the turn, flushing what it said.
        let end = c.turn_ended("opencode", Ended::Canceled);
        assert_eq!(items(&end), ["s1 assistant: x"]);
        assert_eq!(upserts(&end)[0]["status"], "canceled");
    }
}
