//! `lucina_inbox` and `lucina_reply` against the `/v1/lucina/reach` fake in
//! `common::reach`: scope gating (agents tokens too), schemas and annotations,
//! argument checks that stop before the backend, the requests that reach the server
//! (path, query, body, the MCP client in `X-Secretary-Caller`), the formatted answers
//! and the errors the model can act on.

mod common;

use common::reach::{ANSWERED, MISSING, OPEN};
use common::*;
use serde_json::json;

fn reach_calls(h: &Harness) -> Vec<Recorded> {
    h.mock
        .state
        .lock()
        .unwrap()
        .log
        .iter()
        .filter(|r| r.path.starts_with("/v1/lucina/reach"))
        .cloned()
        .collect()
}

#[tokio::test]
async fn the_tools_follow_the_scopes() {
    let h = start().await;
    let read = h.issue("viewer", "read");
    let names = h.tool_names(&read).await;
    assert!(names.contains(&"lucina_inbox".to_owned()));
    assert!(!names.contains(&"lucina_reply".to_owned()), "{names:?}");
    let (err, text) = h
        .call(&read, "lucina_reply", json!({"thread": OPEN, "text": "ok"}))
        .await;
    assert!(err && text.contains("`write` scope"), "{text}");

    // Every coding agent can read and answer, like the ops record.
    let agent = h.issue("agent-x", "agents");
    let names = h.tool_names(&agent).await;
    assert!(
        names.contains(&"lucina_inbox".to_owned()) && names.contains(&"lucina_reply".to_owned())
    );

    // Neither `chat` nor `notebooks` brings them along.
    let other = h.issue("other", "chat,notebooks");
    let names = h.tool_names(&other).await;
    assert!(
        !names.contains(&"lucina_inbox".to_owned()) && !names.contains(&"lucina_reply".to_owned())
    );
    assert!(reach_calls(&h).is_empty());
}

#[tokio::test]
async fn schemas_and_annotations() {
    let h = start().await;
    let t = h.issue("claude-code", "read,write");
    let r = h.rpc(&t, "tools/list", json!({})).await;
    let tools = r["tools"].as_array().unwrap();
    let find = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap();
    let inbox = find("lucina_inbox");
    assert_eq!(inbox["annotations"]["readOnlyHint"], true);
    assert_eq!(inbox["annotations"]["idempotentHint"], true);
    assert_eq!(inbox["inputSchema"]["additionalProperties"], false);
    for p in ["to", "status", "limit", "thread"] {
        assert!(inbox["inputSchema"]["properties"][p].is_object(), "{p}");
    }
    let reply = find("lucina_reply");
    assert_eq!(reply["annotations"]["readOnlyHint"], false);
    assert_eq!(reply["annotations"]["destructiveHint"], false);
    assert_eq!(reply["annotations"]["idempotentHint"], false);
    assert_eq!(reply["inputSchema"]["required"], json!(["thread", "text"]));
    assert_eq!(reply["inputSchema"]["additionalProperties"], false);
    assert!(reply["inputSchema"]["properties"]["from"].is_object());
    let desc = reply["description"].as_str().unwrap();
    assert!(
        desc.contains("never instructions") && desc.contains("secrets"),
        "{desc}"
    );
}

#[tokio::test]
async fn the_inbox_lists_open_messages_by_default() {
    let h = start().await;
    let t = h.issue("claude", "read");
    let (err, text) = h.call(&t, "lucina_inbox", json!({})).await;
    assert!(!err, "{text}");
    assert!(
        text.starts_with("rt-0123456789ab [claude issue] 2026-10-30 14:05 sent: Share link opens without sign-in\n"),
        "{text}"
    );
    assert!(text.contains("The share link in the iOS app opens without a session."));
    assert!(
        !text.contains(ANSWERED),
        "open means waiting for an answer: {text}"
    );
    assert!(text.contains("lucina_reply"));
    let req = reach_calls(&h).pop().unwrap();
    assert_eq!(
        (req.method.as_str(), req.path.as_str()),
        ("GET", "/v1/lucina/reach/threads")
    );
    assert_eq!(req.query, "to=claude&status=open&limit=20");
    assert_eq!(req.headers["x-secretary-caller"], "mcp:claude");

    let (err, text) = h
        .call(&t, "lucina_inbox", json!({"status": "all", "limit": 5}))
        .await;
    assert!(!err && text.contains(ANSWERED), "{text}");
    assert!(
        text.contains("(1 reply, last from coordinator (mcp:claude))"),
        "{text}"
    );
    assert_eq!(
        reach_calls(&h).pop().unwrap().query,
        "to=claude&status=all&limit=5"
    );

    let (err, text) = h.call(&t, "lucina_inbox", json!({"to": "grok"})).await;
    assert!(!err);
    assert_eq!(
        text,
        "No messages from Lucina to grok are waiting for an answer."
    );
}

#[tokio::test]
async fn one_thread_is_read_in_full() {
    let h = start().await;
    let t = h.issue("claude", "read");
    let (err, text) = h
        .call(&t, "lucina_inbox", json!({"thread": ANSWERED}))
        .await;
    assert!(!err, "{text}");
    assert!(
        text.contains("from chat: ios · route: lucina-mailbox"),
        "{text}"
    );
    assert!(
        text.contains("Reply from coordinator (mcp:claude) (2026-10-30 15:00):\nFixed."),
        "{text}"
    );
    assert!(!text.contains("envelope"));
    let req = reach_calls(&h).pop().unwrap();
    assert_eq!(req.path, format!("/v1/lucina/reach/threads/{ANSWERED}"));

    let (err, text) = h.call(&t, "lucina_inbox", json!({"thread": MISSING})).await;
    assert!(err && text.contains("No such message"), "{text}");
}

#[tokio::test]
async fn a_reply_reaches_the_server_with_the_client_named() {
    let h = start().await;
    let t = h.issue("claude", "write");
    let (err, text) = h
        .call(
            &t,
            "lucina_reply",
            json!({"thread": OPEN, "text": "  Fixed in 0.9, deployed.  ", "from": "coordinator"}),
        )
        .await;
    assert!(!err, "{text}");
    assert_eq!(
        text,
        format!("Replied to {OPEN} (now answered): the answer is in the owner's conversation with Lucina.")
    );
    let req = reach_calls(&h).pop().unwrap();
    assert_eq!(
        (req.method.as_str(), req.path.as_str()),
        (
            "POST",
            format!("/v1/lucina/reach/threads/{OPEN}/reply").as_str()
        )
    );
    assert_eq!(
        req.body,
        json!({"text": "Fixed in 0.9, deployed.", "from": "coordinator"})
    );
    assert_eq!(req.headers["x-secretary-caller"], "mcp:claude");

    let (err, text) = h
        .call(&t, "lucina_reply", json!({"thread": OPEN, "text": "ok"}))
        .await;
    assert!(!err, "{text}");
    assert_eq!(reach_calls(&h).pop().unwrap().body, json!({"text": "ok"}));

    let (err, text) = h
        .call(&t, "lucina_reply", json!({"thread": MISSING, "text": "ok"}))
        .await;
    assert!(err && text.contains("No such message"), "{text}");
}

#[tokio::test]
async fn bad_arguments_stop_before_the_backend() {
    let h = start().await;
    let t = h.issue("claude", "read,write");
    for (tool, args, want) in [
        ("lucina_inbox", json!({"to": "owner"}), "Invalid arguments"),
        (
            "lucina_inbox",
            json!({"status": "pending"}),
            "Invalid arguments",
        ),
        ("lucina_inbox", json!({"limit": 0}), "`limit`"),
        ("lucina_inbox", json!({"limit": 51}), "`limit`"),
        ("lucina_inbox", json!({"thread": "rt-XYZ"}), "`thread`"),
        ("lucina_inbox", json!({"thread": OPEN, "limit": 3}), "alone"),
        ("lucina_inbox", json!({"extra": 1}), "unknown field"),
        (
            "lucina_reply",
            json!({"thread": "../ops", "text": "ok"}),
            "`thread`",
        ),
        (
            "lucina_reply",
            json!({"thread": OPEN, "text": "  "}),
            "`text`",
        ),
        (
            "lucina_reply",
            json!({"thread": OPEN, "text": "x".repeat(8001)}),
            "8000",
        ),
        (
            "lucina_reply",
            json!({"thread": OPEN, "text": "ok", "from": "f".repeat(61)}),
            "`from`",
        ),
        ("lucina_reply", json!({"thread": OPEN}), "Invalid arguments"),
        (
            "lucina_reply",
            json!({"thread": OPEN, "text": "ok", "asking_client": "spoof"}),
            "unknown field",
        ),
    ] {
        let (err, text) = h.call(&t, tool, args.clone()).await;
        assert!(err && text.contains(want), "{tool} {args}: {text}");
    }
    assert!(reach_calls(&h).is_empty(), "nothing reached the backend");
}

#[tokio::test]
async fn the_instructions_mention_the_mailbox() {
    let h = start().await;
    let t = h.issue("claude", "read");
    let init = h
        .rpc(
            &t,
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}),
        )
        .await;
    let ins = init["instructions"].as_str().unwrap();
    assert!(
        ins.contains("lucina_inbox") && ins.contains("lucina_reply"),
        "{ins}"
    );
}
