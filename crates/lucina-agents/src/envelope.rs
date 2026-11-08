//! Messages between agents and the finish notices a parent gets about its child
//! (Paseo `server/agent/agent-messages/envelope.js` and
//! `finish-subscriptions/internal/notification.js`).
//!
//! Paseo wraps them in `<paseo-system …>`; Lucina writes `<lucina-system …>` with the
//! same attributes, escaping and body, so a receiving agent can tell an injected
//! message from its owner's prompt. [`PASEO_TAG`] exists only for the fixture test.

use crate::jsstr;
use crate::permission::PermissionRequest;
use crate::timeline::{AgentMessage, AgentMessageEvent, AgentMessageSender, ToolCall, ToolCallDetail, ToolCallStatus};
use crate::TimelineItem;

pub const TAG: &str = "lucina-system";
pub const PASEO_TAG: &str = "paseo-system";

/// Who sent a message: another agent, or the agent a finish notice is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    AgentMessage { agent_id: String, server_id: Option<String>, title: Option<String> },
    AgentNotification { agent_id: String, server_id: Option<String>, title: Option<String>, event: FinishEvent },
}

/// `event="…"` of a finish notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishEvent {
    Finished,
    Errored,
    PermissionRequired,
    Closed,
}

impl FinishEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            FinishEvent::Finished => "finished",
            FinishEvent::Errored => "errored",
            FinishEvent::PermissionRequired => "permission-required",
            FinishEvent::Closed => "closed",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "finished" => FinishEvent::Finished,
            "errored" => FinishEvent::Errored,
            "permission-required" => FinishEvent::PermissionRequired,
            "closed" => FinishEvent::Closed,
            _ => return None,
        })
    }
}

/// One message: `source: None` is the owner's own text (`kind="user-message"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub id: String,
    pub source: Option<Source>,
    pub text: String,
}

pub fn escape_xml(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// `None` when the text has a raw `<`, `>` or a bare `&` (Paseo `unescapeXml`).
fn unescape_xml(value: &str) -> Option<String> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(c) = rest.chars().next() {
        match c {
            '<' | '>' => return None,
            '&' => {
                let (entity, ch) = [("&amp;", '&'), ("&lt;", '<'), ("&gt;", '>'), ("&quot;", '"'), ("&apos;", '\'')]
                    .into_iter()
                    .find(|(e, _)| rest.starts_with(e))?;
                out.push(ch);
                rest = &rest[entity.len()..];
            }
            c => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    Some(out)
}

/// Paseo `formatAgentMessage`, with `tag` as the element name.
pub fn format_message_with(tag: &str, message: &Message) -> String {
    let (attributes, server, title, event) = match &message.source {
        None => ("kind=\"user-message\"".to_string(), None, None, None),
        Some(Source::AgentMessage { agent_id, server_id, title }) => {
            (format!("kind=\"agent-message\" source-agent-id=\"{}\"", escape_xml(agent_id)), server_id.as_deref(), title.as_deref(), None)
        }
        Some(Source::AgentNotification { agent_id, server_id, title, event }) => (
            format!("kind=\"agent-notification\" source-agent-id=\"{}\"", escape_xml(agent_id)),
            server_id.as_deref(),
            title.as_deref(),
            Some(*event),
        ),
    };
    let server = server.filter(|s| !s.is_empty()).map(|s| format!(" source-server-id=\"{}\"", escape_xml(s))).unwrap_or_default();
    let title = title.filter(|s| !s.is_empty()).map(|s| format!(" source-agent-title=\"{}\"", escape_xml(s))).unwrap_or_default();
    let event = event.map(|e| format!(" event=\"{}\"", e.as_str())).unwrap_or_default();
    format!(
        "<{tag} version=\"1\" {attributes} message-id=\"{}\"{server}{title}{event}>\n{}\n</{tag}>",
        escape_xml(&message.id),
        escape_xml(&message.text)
    )
}

pub fn format_message(message: &Message) -> String {
    format_message_with(TAG, message)
}

/// Paseo `parseAgentMessage`: the whole text must be one envelope.
pub fn parse_message_with(tag: &str, text: &str) -> Option<Message> {
    let open = format!("<{tag}");
    let close = format!("\n</{tag}>");
    let rest = text.strip_prefix(&open)?;
    let body_and_close = rest.split_once(">\n")?;
    let (attrs_src, after) = body_and_close;
    let body = after.strip_suffix(&close)?;
    // Attributes: one or more ` name="value"` with no `<>"` in values.
    let mut attrs: Vec<(String, String)> = Vec::new();
    let mut a = attrs_src;
    if a.is_empty() {
        return None;
    }
    while !a.is_empty() {
        let trimmed = a.trim_start_matches(|c: char| c.is_whitespace());
        if trimmed.len() == a.len() {
            return None;
        }
        let (name, rest) = trimmed.split_once("=\"")?;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-') {
            return None;
        }
        let end = rest.find('"')?;
        let raw = &rest[..end];
        if raw.contains(['<', '>']) {
            return None;
        }
        let value = unescape_xml(raw)?;
        if attrs.iter().any(|(n, _)| n == name) {
            return None;
        }
        attrs.push((name.to_string(), value));
        a = &rest[end + 1..];
    }
    let get = |k: &str| attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    if get("version") != Some("1") {
        return None;
    }
    let id = get("message-id").filter(|s| !s.is_empty())?.to_string();
    let text = unescape_xml(body)?;
    let opt = |k: &str| get(k).map(str::to_string);
    let nonempty = |k: &str| match get(k) {
        Some("") => Err(()),
        v => Ok(v.map(str::to_string)),
    };
    let source = match get("kind") {
        Some("user-message") => {
            if get("source-agent-id").is_some() || get("event").is_some() {
                return None;
            }
            None
        }
        Some(kind @ ("agent-message" | "agent-notification")) => {
            let agent_id = opt("source-agent-id").filter(|s| !s.is_empty())?;
            let server_id = nonempty("source-server-id").ok()?;
            let title = nonempty("source-agent-title").ok()?;
            if kind == "agent-message" {
                Some(Source::AgentMessage { agent_id, server_id, title })
            } else {
                let event = FinishEvent::parse(get("event")?)?;
                Some(Source::AgentNotification { agent_id, server_id, title, event })
            }
        }
        _ => return None,
    };
    Some(Message { id, source, text })
}

pub fn parse_message(text: &str) -> Option<Message> {
    parse_message_with(TAG, text)
}

/// Paseo `parseAgentMessages`: a complete sequence of envelopes, or `None`.
pub fn parse_messages_with(tag: &str, text: &str) -> Option<Vec<Message>> {
    let close = format!("</{tag}>");
    let open = format!("<{tag} ");
    let mut parts = Vec::new();
    let mut rest = text;
    loop {
        // Split after a closing tag that is followed by newlines and another envelope.
        let mut cut = None;
        let mut from = 0;
        while let Some(i) = rest[from..].find(&close) {
            let end = from + i + close.len();
            let after = &rest[end..];
            let nl = after.len() - after.trim_start_matches('\n').len();
            if nl > 0 && after[nl..].starts_with(&open) {
                cut = Some((end, end + nl));
                break;
            }
            from = end;
        }
        match cut {
            Some((end, next)) => {
                parts.push(&rest[..end]);
                rest = &rest[next..];
            }
            None => {
                parts.push(rest);
                break;
            }
        }
    }
    parts.into_iter().map(|p| parse_message_with(tag, p)).collect()
}

pub fn parse_messages(text: &str) -> Option<Vec<Message>> {
    parse_messages_with(TAG, text)
}

/// The owner's text that looks like an envelope is sent quoted as the owner's
/// (`kind="user-message"`; Paseo `prepareAgentMessage` on the human send path), so a
/// pasted envelope can't pose as another agent or a schedule. Other text is unchanged.
pub fn quote_reserved(text: &str, id: &str) -> String {
    if parse_messages(text).is_some() || is_system_injected(text) {
        format_message(&Message { id: id.to_string(), source: None, text: text.to_string() })
    } else {
        text.to_string()
    }
}

/// Paseo `formatSystemNotificationPrompt` (schedule fires).
pub fn format_system_notification(reason: &str) -> String {
    format!("<{TAG}>\n{reason}\n</{TAG}>")
}

pub fn is_system_injected(text: &str) -> bool {
    is_system_injected_with(TAG, text)
}

pub fn is_system_injected_with(tag: &str, text: &str) -> bool {
    let open = format!("<{tag}>\n");
    let close = format!("\n</{tag}>");
    text.len() >= open.len() + close.len() && text.starts_with(&open) && text.ends_with(&close)
}

/// How an envelope shows on the receiver's timeline (Paseo `projectAgentMessage`):
/// the owner's quoted text as a user message, another agent's message as an
/// `agent_message` tool call; `None` hides a plain system notice.
pub fn project_user_text(text: &str) -> Vec<TimelineItem> {
    project_user_text_with(TAG, "lucina-agent-message", text)
}

pub fn project_user_text_with(tag: &str, call_prefix: &str, text: &str) -> Vec<TimelineItem> {
    let user = |t: &str| TimelineItem::UserMessage { text: t.to_string(), message_id: None, client_message_id: None };
    let Some(messages) = parse_messages_with(tag, text) else {
        return if is_system_injected_with(tag, text) { Vec::new() } else { vec![user(text)] };
    };
    messages
        .into_iter()
        .map(|m| match m.source {
            None => TimelineItem::UserMessage { text: m.text, message_id: None, client_message_id: Some(m.id) },
            Some(source) => {
                let (event, id, server_id, title) = match source {
                    Source::AgentMessage { agent_id, server_id, title } => (AgentMessageEvent::Message, agent_id, server_id, title),
                    Source::AgentNotification { agent_id, server_id, title, event } => (
                        match event {
                            FinishEvent::Finished => AgentMessageEvent::Finished,
                            FinishEvent::Errored => AgentMessageEvent::Errored,
                            FinishEvent::PermissionRequired => AgentMessageEvent::PermissionRequired,
                            FinishEvent::Closed => AgentMessageEvent::Closed,
                        },
                        agent_id,
                        server_id,
                        title,
                    ),
                };
                TimelineItem::ToolCall(Box::new(ToolCall {
                    agent_message: Some(AgentMessage { event, sender: AgentMessageSender { id, server_id, title }, text: m.text.clone() }),
                    call_id: format!("{call_prefix}:{}", m.id),
                    name: "agent_message".into(),
                    detail: ToolCallDetail::PlainText { label: None, text: Some(m.text), icon: Some("bot".into()) },
                    metadata: None,
                    status: ToolCallStatus::Completed,
                    error: serde_json::Value::Null,
                }))
            }
        })
        .collect()
}

// -- finish notices ----------------------------------------------------------------

/// Why a parent hears about its child (Paseo `reasonFor`; "host …" reasons come with
/// connected hosts, phase 10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Finished,
    Errored,
    NeedsPermission,
    WasClosed,
    IsUnavailable,
    WasCanceled,
}

impl FinishReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FinishReason::Finished => "finished",
            FinishReason::Errored => "errored",
            FinishReason::NeedsPermission => "needs permission",
            FinishReason::WasClosed => "was closed",
            FinishReason::IsUnavailable => "is unavailable",
            FinishReason::WasCanceled => "was canceled",
        }
    }

    /// Paseo `finishNotificationEvents`.
    pub fn event(self) -> FinishEvent {
        match self {
            FinishReason::Finished => FinishEvent::Finished,
            FinishReason::Errored | FinishReason::WasCanceled => FinishEvent::Errored,
            FinishReason::NeedsPermission => FinishEvent::PermissionRequired,
            FinishReason::WasClosed | FinishReason::IsUnavailable => FinishEvent::Closed,
        }
    }
}

pub const FINISH_MESSAGE_LIMIT: usize = 4000;

/// Lucina: the parent can't answer its child's permission request (only the owner
/// does), so the notice says so instead of Paseo's `respond_to_permission` hint.
pub const PERMISSION_HINT: &str =
    "The owner answers this permission request in the Secretary app; agents can't. Wait for the child, or stop it with `cancel_agent`.";

/// Paseo `formatFinishNotificationBody`; `permission_hint` replaces Paseo's sentence.
pub fn finish_body(
    child_agent_id: &str,
    server_id: Option<&str>,
    title: &str,
    reason: FinishReason,
    last_assistant_message: Option<&str>,
    permission_request: Option<&PermissionRequest>,
    permission_hint: &str,
) -> String {
    let mut sections = vec![format!("Agent {child_agent_id} ({title}) {}.", reason.as_str())];
    if let Some(s) = server_id.filter(|s| !s.is_empty()) {
        sections.push(format!("Server: {s}"));
    }
    if let (FinishReason::NeedsPermission, Some(request)) = (reason, permission_request) {
        // A struct keeps Paseo's key order (a `Value` object would sort the keys).
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Block<'a> {
            agent_id: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            server_id: Option<&'a str>,
            request_id: &'a str,
            request: &'a PermissionRequest,
        }
        let block = Block { agent_id: child_agent_id, server_id: server_id.filter(|s| !s.is_empty()), request_id: &request.id, request };
        let pretty = serde_json::to_string_pretty(&block).unwrap_or_default();
        sections.push(permission_hint.to_string());
        sections.push(format!("<permission-request>\n{pretty}\n</permission-request>"));
    }
    if let Some(msg) = last_assistant_message.map(jsstr::trim).filter(|m| !m.is_empty()) {
        let n = jsstr::len(msg);
        let msg = if n > FINISH_MESSAGE_LIMIT {
            format!(
                "{}\n[truncated {} chars; use get_agent_activity for the full response]",
                jsstr::slice(msg, 0, FINISH_MESSAGE_LIMIT),
                n - FINISH_MESSAGE_LIMIT
            )
        } else {
            msg.to_string()
        };
        sections.push(format!("<agent-response>\n{msg}\n</agent-response>"));
    }
    sections.join("\n\n")
}

/// Paseo `buildScheduleFireBody`.
pub fn schedule_fire_body(name: Option<&str>, schedule_id: &str, run_id: &str, prompt: &str) -> String {
    let heading = match name {
        Some(n) if !n.is_empty() => format!("Schedule \"{n}\" fired (id={schedule_id}, run={run_id})."),
        _ => format!("Schedule fired (id={schedule_id}, run={run_id})."),
    };
    format!("{heading}\n{prompt}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_refusals() {
        let m = Message {
            id: "m1".into(),
            source: Some(Source::AgentNotification {
                agent_id: "a<1>".into(),
                server_id: None,
                title: Some("Fix \"it\" & go".into()),
                event: FinishEvent::Finished,
            }),
            text: "done <ok> & 'yes'".into(),
        };
        let s = format_message(&m);
        assert!(s.starts_with("<lucina-system version=\"1\" kind=\"agent-notification\" source-agent-id=\"a&lt;1&gt;\""));
        assert_eq!(parse_message(&s), Some(m.clone()));
        let two = format!("{s}\n\n{s}");
        assert_eq!(parse_messages_with(TAG, &two).map(|v| v.len()), Some(2));
        assert_eq!(parse_message("hello"), None);
        assert_eq!(parse_message(&s.replace("version=\"1\"", "version=\"2\"")), None);
        assert_eq!(parse_message(&s.replace("done", "<b>")), None);
        // An owner's prompt that merely mentions the tag stays the owner's text.
        assert_eq!(project_user_text("see <lucina-system> docs").len(), 1);
        assert!(project_user_text(&format_system_notification("Schedule fired.")).is_empty());
        match &project_user_text(&s)[0] {
            TimelineItem::ToolCall(t) => assert_eq!(t.name, "agent_message"),
            other => panic!("{other:?}"),
        }
    }
}
