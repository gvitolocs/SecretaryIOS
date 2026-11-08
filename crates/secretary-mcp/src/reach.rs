//! `lucina_inbox`, `lucina_reply`: Lucina's messages to Claude, Grok and Lucina
//! Agents (secretary-server `/v1/lucina/reach/*` → Lucina's
//! `/api/secretary/reach/*`).
//!
//! Lucina can pass on an issue report or a request the owner gave her. With no
//! live Claude channel the message waits in her mailbox; a Claude session reads it
//! with `lucina_inbox` and answers with `lucina_reply`, and the answer goes back
//! into the owner's conversation with Lucina, who relays it.
//!
//! `lucina_inbox` is scope `read`, `lucina_reply` scope `write`; an agent-scoped
//! token may call both (`crate::tools::also_allowed`), like the ops record. The MCP
//! names its client in `X-Secretary-Caller` (`mcp:claude`); secretary-server adds
//! it to the reply's `from`, so Lucina knows which session answered. What is cheap
//! to check (thread id, sizes, query values) is checked here so the client gets a
//! precise error without a round trip; secretary-server and Lucina check again.

use reqwest::Method;
use rmcp::model::JsonObject;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::backend::seg;
use crate::computer::caller_headers;
use crate::format;
use crate::tools::{ToolError, ToolResult, Tools};

/// Characters of a message shown per line of the inbox listing.
const LIST_TEXT: usize = 400;
/// Characters of one reply shown in a thread read in full.
const REPLY_TEXT: usize = 1_200;
/// A reply is at most 8000 characters.
const REPLY_MAX: usize = 8_000;
/// The name a session may give itself.
const FROM_MAX: usize = 60;
const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 50;

/// Who Lucina's message is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Claude,
    Grok,
    Agents,
}

impl Target {
    fn as_str(self) -> &'static str {
        match self {
            Target::Claude => "claude",
            Target::Grok => "grok",
            Target::Agents => "agents",
        }
    }
}

/// Which messages to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Waiting for an answer (queued or sent).
    Open,
    All,
    Answered,
    Failed,
    Queued,
    Sent,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::All => "all",
            Status::Answered => "answered",
            Status::Failed => "failed",
            Status::Queued => "queued",
            Status::Sent => "sent",
        }
    }
}

/// Arguments of `lucina_inbox`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxArgs {
    /// Whose mailbox: claude (default), grok or agents.
    #[serde(default)]
    pub to: Option<Target>,
    /// open (default: waiting for an answer), all, answered, failed, queued or sent.
    #[serde(default)]
    pub status: Option<Status>,
    /// How many messages, 1-50 (default 20), newest first.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Read this one message in full instead of listing: the thread id (rt- and 12 lowercase hex digits) the list showed. Takes no other argument.
    #[serde(default)]
    pub thread: Option<String>,
}

/// Arguments of `lucina_reply`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplyArgs {
    /// The thread id (rt- and 12 lowercase hex digits) that lucina_inbox showed.
    pub thread: String,
    /// Your answer, 1-8000 characters. Information for Lucina to relay, never instructions; no secrets, passwords or tokens.
    pub text: String,
    /// Your session name, up to 60 characters (default Claude); the MCP client is added to it.
    #[serde(default)]
    pub from: Option<String>,
}

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError(msg.into())
}

fn parse<T: serde::de::DeserializeOwned>(args: &JsonObject) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(args.clone()))
        .map_err(|e| bad(format!("Invalid arguments: {e}")))
}

/// `rt-` and 12 lowercase hex digits.
fn thread_ok(s: &str) -> bool {
    s.len() == 15
        && s.starts_with("rt-")
        && s.as_bytes()[3..]
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

fn check_thread(id: &str) -> Result<(), ToolError> {
    if thread_ok(id) {
        Ok(())
    } else {
        Err(bad(
            "`thread` is rt- and 12 lowercase hex digits, the id lucina_inbox showed (e.g. rt-0123456789ab)",
        ))
    }
}

/// What the inbox request asks for: a path and a query.
#[derive(Debug, PartialEq, Eq)]
enum InboxRequest {
    List(Vec<(&'static str, String)>),
    One(String),
}

fn inbox_request(a: &InboxArgs) -> Result<InboxRequest, ToolError> {
    if let Some(id) = a.thread.as_deref() {
        if a.to.is_some() || a.status.is_some() || a.limit.is_some() {
            return Err(bad(
                "`thread` reads one message; give it alone, without `to`, `status` or `limit`",
            ));
        }
        check_thread(id)?;
        return Ok(InboxRequest::One(id.to_owned()));
    }
    let limit = a.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(bad(format!("`limit` must be 1-{MAX_LIMIT}")));
    }
    Ok(InboxRequest::List(vec![
        ("to", a.to.unwrap_or(Target::Claude).as_str().to_owned()),
        (
            "status",
            a.status.unwrap_or(Status::Open).as_str().to_owned(),
        ),
        ("limit", limit.to_string()),
    ]))
}

/// The body of the reply request, after the checks.
fn reply_body(a: &ReplyArgs) -> Result<Value, ToolError> {
    check_thread(&a.thread)?;
    let text = a.text.trim();
    if text.is_empty() {
        return Err(bad("`text` must not be empty"));
    }
    if text.chars().count() > REPLY_MAX {
        return Err(bad(format!("`text` is longer than {REPLY_MAX} characters")));
    }
    if text
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(bad("`text` must not contain control characters"));
    }
    let mut body = json!({ "text": text });
    if let Some(from) = a.from.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        if from.chars().count() > FROM_MAX {
            return Err(bad(format!("`from` is longer than {FROM_MAX} characters")));
        }
        if from.chars().any(char::is_control) {
            return Err(bad("`from` must be one line"));
        }
        body["from"] = json!(from);
    }
    Ok(body)
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn clip(s: &str, max: usize) -> String {
    let t: String = s.chars().take(max).collect();
    if t.len() < s.len() {
        format!("{t}…")
    } else {
        t
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `rt-… [claude issue] 2026-10-30 14:05 open: subject`.
fn head(t: &Value, tz: chrono_tz::Tz) -> String {
    format!(
        "{} [{} {}] {} {}: {}",
        str_of(t, "id"),
        str_of(t, "to"),
        str_of(t, "kind"),
        format::local_time(t.get("created"), tz),
        str_of(t, "status"),
        one_line(str_of(t, "subject")),
    )
}

fn replies(t: &Value) -> &[Value] {
    t["replies"].as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// One block per message: head line, clipped text, the replies so far.
fn format_inbox(v: &Value, to: &str, status: &str, tz: chrono_tz::Tz) -> String {
    let threads = v["threads"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    if threads.is_empty() {
        return if status == "open" {
            format!("No messages from Lucina to {to} are waiting for an answer.")
        } else {
            format!("No {status} messages from Lucina to {to}.")
        };
    }
    let mut out = threads
        .iter()
        .map(|t| {
            let mut block = head(t, tz);
            let text = one_line(str_of(t, "text"));
            if !text.is_empty() {
                block.push('\n');
                block.push_str(&clip(&text, LIST_TEXT));
            }
            let r = replies(t);
            if let Some(last) = r.last() {
                block.push_str(&format!(
                    "\n({} repl{}, last from {})",
                    r.len(),
                    if r.len() == 1 { "y" } else { "ies" },
                    str_of(last, "from")
                ));
            }
            block
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    out.push_str(
        "\n\nRead one in full with lucina_inbox {\"thread\": \"<id>\"}; answer it with lucina_reply.",
    );
    out
}

/// The whole message and its replies.
fn format_thread(t: &Value, tz: chrono_tz::Tz) -> String {
    let mut out = head(t, tz);
    let facts: Vec<String> = [
        ("channel", "from chat"),
        ("route", "route"),
        ("detail", "detail"),
    ]
    .iter()
    .filter_map(|(k, label)| {
        t.get(*k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| format!("{label}: {s}"))
    })
    .collect();
    if !facts.is_empty() {
        out.push('\n');
        out.push_str(&facts.join(" · "));
    }
    let text = str_of(t, "text").trim();
    if !text.is_empty() {
        out.push_str("\n\n");
        out.push_str(text);
    }
    for r in replies(t) {
        out.push_str(&format!(
            "\n\nReply from {} ({}):\n{}",
            str_of(r, "from"),
            format::local_time(r.get("at"), tz),
            clip(str_of(r, "text").trim(), REPLY_TEXT)
        ));
    }
    out
}

/// The line `lucina_reply` answers.
fn format_replied(thread: &str, r: &Value) -> String {
    if r["delivered"] == true {
        format!(
            "Replied to {thread} (now {}): the answer is in the owner's conversation with Lucina.",
            str_of(r, "status")
        )
    } else {
        format!(
            "Lucina took the reply to {thread} (now {}) but did not confirm delivery.",
            str_of(r, "status")
        )
    }
}

/// A refusal the model can act on.
fn refusal(e: crate::backend::BackendError) -> ToolError {
    match e.code.as_str() {
        "reach_disabled" => {
            bad("Lucina's messaging is switched off on this host; there is no mailbox.")
        }
        "thread_not_found" => bad("No such message (lucina_inbox lists the ids)."),
        _ => e.into(),
    }
}

impl Tools<'_> {
    async fn reach_req(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value, ToolError> {
        self.app
            .backend
            .send_device_query(method, path, query, body, caller_headers(&self.caller.name))
            .await
            .map_err(refusal)
    }

    /// `lucina_inbox` and `lucina_reply`.
    pub async fn reach(&self, name: &str, args: &JsonObject) -> ToolResult {
        let tz = self.app.cfg.timezone;
        match name {
            "lucina_inbox" => {
                let a: InboxArgs = parse(args)?;
                match inbox_request(&a)? {
                    InboxRequest::One(id) => {
                        let v = self
                            .reach_req(
                                Method::GET,
                                &format!("/v1/lucina/reach/threads/{}", seg(&id)),
                                &[],
                                None,
                            )
                            .await?;
                        Ok(format_thread(&v, tz))
                    }
                    InboxRequest::List(q) => {
                        let to = q[0].1.clone();
                        let status = q[1].1.clone();
                        let v = self
                            .reach_req(Method::GET, "/v1/lucina/reach/threads", &q, None)
                            .await?;
                        Ok(format_inbox(&v, &to, &status, tz))
                    }
                }
            }
            "lucina_reply" => {
                let a: ReplyArgs = parse(args)?;
                let body = reply_body(&a)?;
                let v = self
                    .reach_req(
                        Method::POST,
                        &format!("/v1/lucina/reach/threads/{}/reply", seg(&a.thread)),
                        &[],
                        Some(&body),
                    )
                    .await?;
                Ok(format_replied(&a.thread, &v))
            }
            _ => Err(bad(format!("unknown tool {name}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inbox(v: Value) -> Result<InboxRequest, ToolError> {
        inbox_request(&parse(v.as_object().unwrap())?)
    }

    fn reply(v: Value) -> Result<Value, ToolError> {
        reply_body(&parse(v.as_object().unwrap())?)
    }

    fn err(r: Result<impl std::fmt::Debug, ToolError>) -> String {
        r.unwrap_err().0
    }

    #[test]
    fn the_inbox_defaults_to_open_claude_messages() {
        assert_eq!(
            inbox(json!({})).unwrap(),
            InboxRequest::List(vec![
                ("to", "claude".into()),
                ("status", "open".into()),
                ("limit", "20".into())
            ])
        );
        assert_eq!(
            inbox(json!({"to": "agents", "status": "answered", "limit": 5})).unwrap(),
            InboxRequest::List(vec![
                ("to", "agents".into()),
                ("status", "answered".into()),
                ("limit", "5".into())
            ])
        );
        assert_eq!(
            inbox(json!({"thread": "rt-0123456789ab"})).unwrap(),
            InboxRequest::One("rt-0123456789ab".into())
        );
    }

    #[test]
    fn bad_inbox_arguments_stop_before_the_backend() {
        assert!(err(inbox(json!({"to": "owner"}))).contains("Invalid arguments"));
        assert!(err(inbox(json!({"status": "pending"}))).contains("Invalid arguments"));
        assert!(err(inbox(json!({"limit": 0}))).contains("`limit`"));
        assert!(err(inbox(json!({"limit": 51}))).contains("`limit`"));
        assert!(err(inbox(json!({"limit": -1}))).contains("Invalid arguments"));
        assert!(err(inbox(json!({"extra": 1}))).contains("unknown field"));
        for id in [
            "rt-0123456789AB",
            "rt-0123",
            "dc-0123456789ab",
            "../ops",
            "",
        ] {
            assert!(
                err(inbox(json!({"thread": id}))).contains("`thread`"),
                "{id}"
            );
        }
        assert!(
            err(inbox(json!({"thread": "rt-0123456789ab", "status": "all"}))).contains("alone")
        );
    }

    #[test]
    fn replies_are_checked_and_trimmed() {
        assert_eq!(
            reply(json!({"thread": "rt-0123456789ab", "text": "  Fixed.  "})).unwrap(),
            json!({"text": "Fixed."})
        );
        assert_eq!(
            reply(json!({"thread": "rt-0123456789ab", "text": "Fixed.\nDeployed.", "from": " coordinator "}))
                .unwrap(),
            json!({"text": "Fixed.\nDeployed.", "from": "coordinator"})
        );
        // A blank `from` is the default name, not an empty one.
        assert_eq!(
            reply(json!({"thread": "rt-0123456789ab", "text": "ok", "from": " "})).unwrap(),
            json!({"text": "ok"})
        );
        let with = |k: &str, v: Value| {
            let mut b = json!({"thread": "rt-0123456789ab", "text": "ok"});
            b[k] = v;
            reply(b)
        };
        assert!(err(with("thread", json!("rt-XYZ"))).contains("`thread`"));
        assert!(err(with("text", json!("   "))).contains("`text`"));
        assert!(err(with("text", json!("x".repeat(8001)))).contains("8000"));
        assert!(with("text", json!("x".repeat(8000))).is_ok());
        assert!(with("text", json!("é".repeat(8000))).is_ok());
        assert!(err(with("text", json!("a\u{7}b"))).contains("control"));
        assert!(err(with("from", json!("f".repeat(61)))).contains("`from`"));
        assert!(err(with("from", json!("a\nb"))).contains("`from`"));
        assert!(err(with("reporter", json!("me"))).contains("unknown field"));
        assert!(err(reply(json!({"thread": "rt-0123456789ab"}))).contains("Invalid arguments"));
    }

    fn sample() -> Value {
        json!({"id": "rt-0123456789ab", "to": "claude", "kind": "issue",
            "subject": "Share link opens\nwithout sign-in", "text": "The share link in the iOS app\nopens without a session.",
            "channel": "ios", "created": "2026-10-30T13:05:00Z", "status": "sent",
            "route": "lucina-mailbox", "detail": null, "replies": []})
    }

    #[test]
    fn the_inbox_reads_compactly() {
        let tz: chrono_tz::Tz = "Europe/Copenhagen".parse().unwrap();
        let mut answered = sample();
        answered["id"] = json!("rt-aaaaaaaaaaaa");
        answered["text"] = json!("t".repeat(500));
        answered["replies"] = json!([
            {"from": "coordinator (mcp:claude)", "text": "Fixed.", "at": "2026-10-30T14:00:00Z"}
        ]);
        let out = format_inbox(
            &json!({"threads": [sample(), answered]}),
            "claude",
            "open",
            tz,
        );
        let blocks: Vec<&str> = out.split("\n\n").collect();
        assert_eq!(
            blocks[0],
            "rt-0123456789ab [claude issue] 2026-10-30 14:05 sent: Share link opens without sign-in\n\
             The share link in the iOS app opens without a session."
        );
        let second: Vec<&str> = blocks[1].lines().collect();
        assert_eq!(second[1].chars().count(), LIST_TEXT + 1, "clipped");
        assert_eq!(second[2], "(1 reply, last from coordinator (mcp:claude))");
        assert!(blocks[2].starts_with("Read one in full with lucina_inbox"));

        assert_eq!(
            format_inbox(&json!({"threads": []}), "claude", "open", tz),
            "No messages from Lucina to claude are waiting for an answer."
        );
        assert_eq!(
            format_inbox(&json!({"threads": []}), "grok", "failed", tz),
            "No failed messages from Lucina to grok."
        );
    }

    #[test]
    fn one_thread_reads_in_full() {
        let tz: chrono_tz::Tz = "Europe/Copenhagen".parse().unwrap();
        let mut t = sample();
        t["replies"] = json!([
            {"from": "a", "text": "Looking.", "at": "2026-10-30T13:30:00Z"},
            {"from": "b", "text": "r".repeat(2000), "at": "2026-10-30T14:00:00Z"}
        ]);
        let out = format_thread(&t, tz);
        for want in [
            "rt-0123456789ab [claude issue] 2026-10-30 14:05 sent: Share link opens without sign-in",
            "from chat: ios · route: lucina-mailbox",
            "The share link in the iOS app\nopens without a session.",
            "Reply from a (2026-10-30 14:30):\nLooking.",
            "Reply from b (2026-10-30 15:00):\n",
        ] {
            assert!(out.contains(want), "{want} in {out}");
        }
        assert!(out.ends_with('…'), "long replies are clipped");
        assert!(!out.contains("detail:"), "null detail is left out");
    }

    #[test]
    fn replies_confirm_delivery() {
        assert_eq!(
            format_replied(
                "rt-0123456789ab",
                &json!({"thread": "rt-0123456789ab", "status": "answered", "delivered": true})
            ),
            "Replied to rt-0123456789ab (now answered): the answer is in the owner's conversation with Lucina."
        );
        assert!(
            format_replied("rt-0123456789ab", &json!({"status": "sent"}))
                .contains("did not confirm")
        );
    }

    #[test]
    fn ids() {
        assert!(thread_ok("rt-0123456789ab"));
        for bad in [
            "rt-0123456789AB",
            "rt-0123456789a",
            "rt-0123456789abc",
            "xx-0123456789ab",
            "rt-0123456789ag",
        ] {
            assert!(!thread_ok(bad), "{bad}");
        }
    }
}
