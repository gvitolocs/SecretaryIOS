//! A session's last tool loop, carried into its next one.
//!
//! Hermes Agent (MIT) replays a session's earlier tool calls and results on the
//! next turn; LucinaCode (our port of OpenCode) and OpenBot do the same. Our
//! loops started from `[system, user]`, so after "go ahead" Lucina found again
//! what the turn before had already found (2026-11-04, Juno stage C, turn 2).
//! [`remember`] keeps the owner's message, the assistant tool calls with their
//! results and the final reply of one loop; [`previous`] hands them to the next
//! loop of the same session within [`TTL`], clipped to [`BUDGET_CHARS`].
//! Tool results reach this point already secret-redacted (`agent::execute`).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use lucina_llm::{Message, Role};

/// A loop counts for the next turn this long.
pub const TTL: Duration = Duration::from_secs(15 * 60);
/// Characters of carried text (message contents and call arguments) at most.
pub const BUDGET_CHARS: usize = 6000;
/// One tool result, the owner's message or the final reply at most this long.
const PART_CHARS: usize = 1500;
/// Call arguments longer than this are replaced by a short JSON marker.
const ARGS_CHARS: usize = 1000;

struct Kept {
    at: Instant,
    messages: Vec<Message>,
}

static STORE: LazyLock<Mutex<HashMap<String, Kept>>> = LazyLock::new(Default::default);

fn store() -> std::sync::MutexGuard<'static, HashMap<String, Kept>> {
    STORE.lock().unwrap_or_else(|e| e.into_inner())
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…[clipped]", &s[..i]),
        None => s.to_owned(),
    }
}

fn size(m: &Message) -> usize {
    let content = m.content.as_deref().map_or(0, |c| c.chars().count());
    let args: usize = m
        .tool_calls
        .iter()
        .flatten()
        .map(|c| c.function.arguments.chars().count())
        .sum();
    content + args
}

/// One assistant message with its tool calls, and their results.
fn groups(loop_messages: &[Message]) -> (Vec<Vec<Message>>, Option<Message>) {
    let mut groups: Vec<Vec<Message>> = Vec::new();
    let mut last_reply = None;
    // Skip the loop's own system and user messages: only what the loop added.
    let start = loop_messages
        .iter()
        .position(|m| m.role == Role::User)
        .map_or(0, |i| i + 1);
    for m in &loop_messages[start..] {
        match m.role {
            Role::Assistant if m.tool_calls.as_ref().is_some_and(|c| !c.is_empty()) => {
                let mut a = m.clone();
                a.reasoning_content = None;
                for call in a.tool_calls.iter_mut().flatten() {
                    if call.function.arguments.chars().count() > ARGS_CHARS {
                        call.function.arguments = r#"{"_clipped":true}"#.to_owned();
                    }
                }
                groups.push(vec![a]);
            }
            Role::Tool => {
                if let Some(g) = groups.last_mut() {
                    let mut t = m.clone();
                    t.content = t.content.as_deref().map(|c| clip(c, PART_CHARS));
                    g.push(t);
                }
            }
            Role::Assistant => {
                let text = m.text().trim();
                if !text.is_empty() {
                    last_reply = Some(Message::assistant(clip(text, PART_CHARS)));
                }
            }
            _ => {}
        }
    }
    (groups, last_reply)
}

/// Keep this loop for the session's next one: the owner's `user_text`, the
/// tool calls with their results (oldest dropped first, whole, to fit
/// [`BUDGET_CHARS`]) and the final reply. A loop without tool calls clears
/// what was kept.
pub fn remember(session: &str, user_text: &str, loop_messages: &[Message]) {
    remember_at(session, user_text, loop_messages, Instant::now());
}

fn remember_at(session: &str, user_text: &str, loop_messages: &[Message], at: Instant) {
    let (mut groups, reply) = groups(loop_messages);
    if groups.is_empty() {
        store().remove(session);
        return;
    }
    let user = Message::user(clip(user_text.trim(), PART_CHARS));
    let fixed = size(&user) + reply.as_ref().map_or(0, size);
    let group_size = |g: &Vec<Message>| g.iter().map(size).sum::<usize>();
    while groups.len() > 1 && fixed + groups.iter().map(group_size).sum::<usize>() > BUDGET_CHARS {
        groups.remove(0);
    }
    let mut messages = vec![user];
    messages.extend(groups.into_iter().flatten());
    if let Some(r) = reply {
        messages.push(r);
    }
    store().insert(session.to_owned(), Kept { at, messages });
}

/// The session's last loop, when it ended less than [`TTL`] ago: messages to
/// put between the system message and the new user message.
pub fn previous(session: &str) -> Vec<Message> {
    previous_at(session, Instant::now())
}

fn previous_at(session: &str, now: Instant) -> Vec<Message> {
    let mut map = store();
    match map.get(session) {
        Some(k) if now.saturating_duration_since(k.at) <= TTL => k.messages.clone(),
        Some(_) => {
            map.remove(session);
            Vec::new()
        }
        None => Vec::new(),
    }
}

/// Forget the session's carried loop.
pub fn forget(session: &str) {
    store().remove(session);
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_llm::{FunctionCall, ToolCall};

    fn call(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            kind: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
        }
    }

    fn assistant_calls(calls: Vec<ToolCall>) -> Message {
        let mut m = Message::assistant("");
        m.content = None;
        m.tool_calls = Some(calls);
        m.reasoning_content = Some("thinking".into());
        m
    }

    fn a_loop() -> Vec<Message> {
        vec![
            Message::system("sys"),
            Message::user("make a note"),
            assistant_calls(vec![call("c1", "juno_call", r#"{"tool":"intents"}"#)]),
            Message::tool_result("c1", "notes.create(body*, title)"),
            assistant_calls(vec![
                call("c2", "juno_call", r#"{"tool":"intent"}"#),
                call("c3", "time", "{}"),
            ]),
            Message::tool_result("c2", "ok act_1"),
            Message::tool_result("c3", "12:00"),
            Message::assistant("Done."),
        ]
    }

    #[test]
    fn a_loop_is_carried_whole_and_in_order() {
        let s = "carry-1";
        remember(s, "make a note", &a_loop());
        let p = previous(s);
        let roles: Vec<Role> = p.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                Role::User,
                Role::Assistant,
                Role::Tool,
                Role::Assistant,
                Role::Tool,
                Role::Tool,
                Role::Assistant
            ]
        );
        assert_eq!(p[0].text(), "make a note");
        assert_eq!(p.last().unwrap().text(), "Done.");
        assert!(p.iter().all(|m| m.reasoning_content.is_none()));
        assert_eq!(p[2].tool_call_id.as_deref(), Some("c1"));
        assert!(!p.iter().any(|m| m.role == Role::System));
    }

    #[test]
    fn old_loops_expire_and_a_tool_less_loop_clears() {
        let s = "carry-2";
        let t0 = Instant::now();
        remember_at(s, "x", &a_loop(), t0);
        assert!(!previous_at(s, t0 + TTL).is_empty());
        assert!(previous_at(s, t0 + TTL + Duration::from_secs(1)).is_empty());
        assert!(previous_at(s, t0).is_empty(), "expired entries are dropped");
        remember(s, "x", &a_loop());
        remember(
            s,
            "hi",
            &[
                Message::system("s"),
                Message::user("hi"),
                Message::assistant("Hey"),
            ],
        );
        assert!(previous(s).is_empty());
    }

    #[test]
    fn budget_drops_whole_oldest_groups_and_clips_parts() {
        let s = "carry-3";
        let big = "r".repeat(5000);
        let mut l = vec![Message::system("s"), Message::user("u")];
        for i in 0..6 {
            l.push(assistant_calls(vec![call(&format!("c{i}"), "t", "{}")]));
            l.push(Message::tool_result(format!("c{i}"), big.clone()));
        }
        l.push(Message::assistant("end"));
        remember(s, "u", &l);
        let p = previous(s);
        let total: usize = p.iter().map(size).sum();
        assert!(total <= BUDGET_CHARS, "{total}");
        // Every tool result follows its own assistant call: pairs never split.
        for (i, m) in p.iter().enumerate() {
            if m.role == Role::Tool {
                let id = m.tool_call_id.clone().unwrap();
                let prev = &p[i - 1];
                assert!(prev.tool_calls.iter().flatten().any(|c| c.id == id), "{id}");
            }
        }
        // The newest group survives.
        assert!(p.iter().any(|m| m.tool_call_id.as_deref() == Some("c5")));
        assert!(!p.iter().any(|m| m.tool_call_id.as_deref() == Some("c0")));
        assert!(p
            .iter()
            .filter(|m| m.role == Role::Tool)
            .all(|m| m.text().contains("[clipped]")));
    }

    #[test]
    fn long_arguments_become_a_json_marker() {
        let s = "carry-4";
        let long = format!(r#"{{"text":"{}"}}"#, "a".repeat(2000));
        let l = vec![
            Message::system("s"),
            Message::user("u"),
            assistant_calls(vec![call("c1", "write", &long)]),
            Message::tool_result("c1", "ok"),
        ];
        remember(s, "u", &l);
        let p = previous(s);
        let args = &p[1].tool_calls.as_ref().unwrap()[0].function.arguments;
        assert!(serde_json::from_str::<serde_json::Value>(args).is_ok());
        assert!(args.contains("_clipped"));
        forget(s);
        assert!(previous(s).is_empty());
    }
}
