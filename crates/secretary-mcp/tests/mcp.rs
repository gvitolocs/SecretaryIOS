//! MCP over Streamable HTTP against a mock secretary-server.

mod common;

use common::*;
use serde_json::{json, Value};

const ALL_TOOLS: [&str; 11] = [
    "ask_lucina",
    "lucina_timeline",
    "shopping_list",
    "shopping_add",
    "shopping_mark_bought",
    "reminders_list",
    "reminder_create",
    "reminder_cancel",
    "gym_summary",
    "body_summary",
    "lucina_status",
];

#[tokio::test]
async fn initialize_and_tools_list() {
    let h = start().await;
    let t = h.issue("claude-code", "read,chat,write");
    let init = h
        .rpc(
            &t,
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "test", "version": "1"}}),
        )
        .await;
    assert_eq!(init["serverInfo"]["name"], "lucina-mcp");
    assert_eq!(init["protocolVersion"], "2025-11-25");
    assert!(init["capabilities"]["tools"].is_object());
    assert!(init["capabilities"]["resources"].is_object());
    assert!(init["capabilities"]["prompts"].is_object());
    assert!(init["instructions"]
        .as_str()
        .unwrap()
        .contains("ask_lucina"));

    let r = h.rpc(&t, "tools/list", json!({})).await;
    let tools = r["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ALL_TOOLS);
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert!(tool["description"].as_str().unwrap().len() > 40, "{tool}");
        assert!(tool["annotations"].is_object(), "{tool}");
    }
    let ask = &tools[0];
    assert_eq!(ask["inputSchema"]["required"], json!(["message"]));
    assert!(ask["inputSchema"]["properties"]["conversation"].is_object());
    let add = tools.iter().find(|t| t["name"] == "shopping_add").unwrap();
    assert_eq!(add["inputSchema"]["required"], json!(["items"]));
    let cancel = tools
        .iter()
        .find(|t| t["name"] == "reminder_cancel")
        .unwrap();
    assert_eq!(cancel["annotations"]["destructiveHint"], true);
    let tl = tools
        .iter()
        .find(|t| t["name"] == "lucina_timeline")
        .unwrap();
    assert_eq!(tl["annotations"]["readOnlyHint"], true);
}

#[tokio::test]
async fn stateless_2026_07_28_request() {
    let h = start().await;
    let t = h.issue("grok", "read");
    let resp = h
        .http
        .post(format!("{}/mcp", h.base))
        .bearer_auth(&t)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2026-07-28")
        .header("mcp-method", "tools/call")
        .header("mcp-name", "lucina_status")
        .json(
            &json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {
            "name": "lucina_status", "arguments": {},
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": {"name": "t", "version": "1"},
                "io.modelcontextprotocol/clientCapabilities": {}
            }}}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().get("mcp-session-id").is_none());
    let v: Value = resp.json().await.unwrap();
    let text = v["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("connected as grok"), "{text}");
    assert!(text.contains("Lucina: reachable (12 ms)"), "{text}");
    assert!(text.contains("Backend device token: accepted"), "{text}");
}

#[tokio::test]
async fn ask_lucina_round_trip() {
    let h = start().await;
    let t = h.issue("claude-code", "read,chat");
    let (err, text) = h
        .call(
            &t,
            "ask_lucina",
            json!({"message": "Cosa ho in programma domani?"}),
        )
        .await;
    assert!(!err, "{text}");
    assert_eq!(
        text,
        "Ciao! You said: [via Lucina MCP, from claude-code] Cosa ho in programma domani?"
    );
    let reqs = h.mock.requests("/v1/chat");
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r.headers["x-secretary-client"], "web");
    assert_eq!(r.headers["authorization"], format!("Bearer {DEVICE_TOKEN}"));
    assert_eq!(r.body["sessionId"], "mcp-claude-code");
    assert_eq!(r.body["channel"], "web");
    assert_eq!(
        r.body["meta"],
        json!({"via": "mcp", "caller": "claude-code"})
    );
    assert!(r.body["clientMessageId"].as_str().unwrap().len() == 36);

    // a named conversation gets its own session
    let (err, _) = h
        .call(
            &t,
            "ask_lucina",
            json!({"message": "ciao", "conversation": "travel"}),
        )
        .await;
    assert!(!err);
    assert_eq!(
        h.mock.requests("/v1/chat")[1].body["sessionId"],
        "mcp-claude-code-travel"
    );

    // validation happens before Lucina is called
    let (err, text) = h.call(&t, "ask_lucina", json!({"message": "   "})).await;
    assert!(err && text.contains("must not be empty"), "{text}");
    let (err, text) = h
        .call(
            &t,
            "ask_lucina",
            json!({"message": "x", "conversation": "no spaces please"}),
        )
        .await;
    assert!(err && text.contains("conversation"), "{text}");
    let (err, text) = h
        .call(&t, "ask_lucina", json!({"text": "wrong field"}))
        .await;
    assert!(err && text.contains("Invalid arguments"), "{text}");
    assert_eq!(h.mock.requests("/v1/chat").len(), 2);
}

#[tokio::test]
async fn scopes_are_enforced() {
    let h = start().await;
    let read = h.issue("grok", "read");
    let names = h.tool_names(&read).await;
    assert!(!names.contains(&"ask_lucina".to_string()));
    assert!(!names
        .iter()
        .any(|n| n == "shopping_add" || n == "reminder_create" || n == "reminder_cancel"));
    assert!(names.contains(&"shopping_list".to_string()));

    let (err, text) = h
        .call(&read, "shopping_add", json!({"items": ["latte"]}))
        .await;
    assert!(err);
    assert!(text.contains("lacks the `write` scope"), "{text}");
    let (err, text) = h.call(&read, "ask_lucina", json!({"message": "hi"})).await;
    assert!(err && text.contains("`chat` scope"), "{text}");
    let (err, _) = h.call(&read, "reminder_cancel", json!({"id": "r1"})).await;
    assert!(err);
    // nothing reached the backend
    assert!(h.mock.requests("/v1/shopping/lists/default/ops").is_empty());
    assert!(h.mock.requests("/v1/chat").is_empty());
    assert!(h.mock.requests("/v1/reminders/r1").is_empty());

    let chat_only = h.issue("bot", "chat");
    assert_eq!(
        h.tool_names(&chat_only).await,
        vec!["ask_lucina".to_string()]
    );
    let r = h.rpc(&chat_only, "resources/list", json!({})).await;
    assert_eq!(r["resources"], json!([]));

    // unknown tool is a protocol error
    let (status, _, v) = h
        .rpc_raw(
            Some(&read),
            "tools/call",
            json!({"name": "rm_rf", "arguments": {}}),
        )
        .await;
    assert_eq!(status, 200);
    assert!(v["error"].is_object(), "{v}");
}

#[tokio::test]
async fn unauthenticated_requests_get_the_oauth_challenge() {
    let h = start().await;
    let (status, headers, _) = h.rpc_raw(None, "tools/list", json!({})).await;
    assert_eq!(status, 401);
    let www = headers["www-authenticate"].to_str().unwrap();
    assert!(www.starts_with("Bearer "), "{www}");
    assert!(
        www.contains("resource_metadata=\"https://getsecretary.tech/.well-known/oauth-protected-resource/mcp\""),
        "{www}"
    );
    assert!(www.contains("scope=\"read chat write\""));
    let (status, headers, _) = h
        .rpc_raw(Some("mcp_not_a_real_token"), "tools/list", json!({}))
        .await;
    assert_eq!(status, 401);
    assert!(headers["www-authenticate"]
        .to_str()
        .unwrap()
        .contains("error=\"invalid_token\""));
    let (status, _, _) = h
        .rpc_raw(
            Some("sec_device_tokens_do_not_work"),
            "tools/list",
            json!({}),
        )
        .await;
    assert_eq!(status, 401);
    // a raw token without "Bearer " is accepted (xAI-style header)
    let t = h.issue("grok", "read");
    let resp = h
        .http
        .post(format!("{}/mcp", h.base))
        .header("authorization", &t)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn wrong_host_is_refused() {
    let h = start().await;
    let t = h.issue("grok", "read");
    let resp = h
        .http
        .post(format!("{}/mcp", h.base))
        .bearer_auth(&t)
        .header("host", "evil.example")
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}))
        .send()
        .await
        .unwrap();
    assert_ne!(resp.status(), 200);
}

#[tokio::test]
async fn shopping_v2_tools() {
    let h = start().await;
    let t = h.issue("claude-code", "read,write");
    let (err, text) = h.call(&t, "shopping_list", json!({})).await;
    assert!(!err, "{text}");
    assert!(
        text.starts_with("Shopping list \"Casa\" (id default): 2 needed:"),
        "{text}"
    );
    assert!(text.contains("- Banane (6, mature) [id i2]"), "{text}");
    assert!(text.contains("Recently bought: Pane"), "{text}");
    assert!(
        text.contains("Other lists: Casa (id default), Ufficio (id work)"),
        "{text}"
    );

    let (err, text) = h
        .call(
            &t,
            "shopping_add",
            json!({"items": ["uova", {"name": "Caffè", "spec": "macinato", "quantity": 2, "unit": "pz"}]}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.contains("Added on list default: uova, Caffè (macinato)"),
        "{text}"
    );
    let ops = h.mock.requests("/v1/shopping/lists/default/ops");
    assert_eq!(ops.len(), 1);
    let sent = ops[0].body["ops"].as_array().unwrap();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["type"], "add");
    assert_eq!(sent[0]["name"], "uova");
    assert_eq!(sent[1]["spec"], "macinato");
    assert_eq!(sent[1]["quantity"], 2.0);
    assert_eq!(sent[1]["unit"], "pz");
    assert_eq!(sent[0]["opId"].as_str().unwrap().len(), 36);

    // by name, accent- and case-insensitive
    let (err, text) = h
        .call(&t, "shopping_mark_bought", json!({"item": "caffe"}))
        .await;
    assert!(!err, "{text}");
    assert!(
        text.contains("Marked as bought on list default: Caffè"),
        "{text}"
    );
    let ops = h.mock.requests("/v1/shopping/lists/default/ops");
    assert_eq!(ops[1].body["ops"][0]["type"], "purchase");
    assert_eq!(ops[1].body["ops"][0]["itemId"], "item4");
    let (err, text) = h
        .call(&t, "shopping_mark_bought", json!({"item": "pane"}))
        .await;
    assert!(err && text.contains("already marked as bought"), "{text}");
    let (err, text) = h
        .call(&t, "shopping_mark_bought", json!({"item": "kiwi"}))
        .await;
    assert!(err && text.contains("No item matching"), "{text}");

    // named lists resolve by name
    let (err, text) = h
        .call(&t, "shopping_list", json!({"list": "ufficio"}))
        .await;
    assert!(
        !err && text.contains("\"Ufficio\" (id work): 0 needed."),
        "{text}"
    );
    let (err, text) = h.call(&t, "shopping_list", json!({"list": "garage"})).await;
    assert!(err && text.contains("No shopping list called"), "{text}");

    let (err, text) = h.call(&t, "shopping_add", json!({"items": []})).await;
    assert!(err && text.contains("1-50"), "{text}");
}

#[tokio::test]
async fn shopping_falls_back_to_legacy() {
    let h = start().await;
    h.mock.set_v2(false);
    let t = h.issue("claude-code", "read,write");
    let (err, text) = h.call(&t, "shopping_list", json!({})).await;
    assert!(!err, "{text}");
    assert!(text.contains("- latte (milk) [id l0]"), "{text}");
    let (err, text) = h.call(&t, "shopping_add", json!({"items": ["pane"]})).await;
    assert!(!err && text.contains("pane"), "{text}");
    assert_eq!(
        h.mock.requests("/v1/shopping")[1].body,
        json!({"rawName": "pane"})
    );
    let (err, text) = h
        .call(&t, "shopping_mark_bought", json!({"item": "milk"}))
        .await;
    assert!(!err, "{text}");
    let patch = h.mock.requests("/v1/shopping/l0");
    assert_eq!(patch[0].method, "PATCH");
    assert_eq!(patch[0].body, json!({"status": "purchased"}));
    // named lists need v2
    let (err, text) = h.call(&t, "shopping_list", json!({"list": "work"})).await;
    assert!(err && text.contains("v2"), "{text}");
}

#[tokio::test]
async fn reminders_tools() {
    let h = start().await;
    let t = h.issue("chatgpt", "read,write");
    let (err, text) = h
        .call(
            &t,
            "reminders_list",
            json!({"scope": "today", "include_sent": true}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.contains("- 2026-10-22 09:30 — Call the dentist [id r1] (pending)"),
        "{text}"
    );
    assert_eq!(
        h.mock.requests("/v1/reminders")[0].query,
        "scope=today&includeSent=1"
    );

    let (err, text) = h
        .call(
            &t,
            "reminder_create",
            json!({"text": "Buy flowers", "when": "2099-01-01T10:00:00+01:00"}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.starts_with("Created reminder \"Buy flowers\""),
        "{text}"
    );
    let posts: Vec<_> = h
        .mock
        .requests("/v1/reminders")
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert_eq!(
        posts[0].body,
        json!({"text": "Buy flowers", "dueAt": "2099-01-01T10:00:00+01:00"})
    );

    let (err, _) = h
        .call(
            &t,
            "reminder_create",
            json!({"text": "chiamare la mamma", "when": "domani alle 18"}),
        )
        .await;
    assert!(!err);
    let posts: Vec<_> = h
        .mock
        .requests("/v1/reminders")
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert_eq!(
        posts[1].body,
        json!({"naturalText": "domani alle 18 — chiamare la mamma"})
    );

    let (err, text) = h
        .call(
            &t,
            "reminder_create",
            json!({"text": "x", "when": "never ever"}),
        )
        .await;
    assert!(
        err && text.contains("ISO 8601") && text.contains("Now is"),
        "{text}"
    );
    let (err, text) = h
        .call(
            &t,
            "reminder_create",
            json!({"text": "x", "when": "2001-01-01T10:00:00Z"}),
        )
        .await;
    assert!(err && text.contains("in the past"), "{text}");

    let (err, text) = h.call(&t, "reminder_cancel", json!({"id": "r1"})).await;
    assert!(
        !err && text.contains("Cancelled the reminder \"Call the dentist\""),
        "{text}"
    );
    assert_eq!(h.mock.requests("/v1/reminders/r1")[0].method, "DELETE");
    let (err, text) = h.call(&t, "reminder_cancel", json!({"id": "nope"})).await;
    assert!(err && text.contains("reminder_not_found"), "{text}");
    let (err, _) = h.call(&t, "reminder_cancel", json!({"id": "../x"})).await;
    assert!(err);
}

#[tokio::test]
async fn read_tools_resources_and_prompt() {
    let h = start().await;
    let t = h.issue("claude-code", "read");
    let (err, text) = h.call(&t, "lucina_timeline", json!({"limit": 2})).await;
    assert!(!err, "{text}");
    assert!(
        text.contains("telegram] Lucina: Fatto, latte aggiunto."),
        "{text}"
    );
    assert!(
        text.contains("telegram] owner: Aggiungi il latte"),
        "{text}"
    );
    assert!(text.ends_with("next_before: tl_2"), "{text}");
    let (_, text) = h
        .call(&t, "lucina_timeline", json!({"channel": "web"}))
        .await;
    assert!(
        text.contains("web] owner: Ciao") && !text.contains("telegram"),
        "{text}"
    );
    let (err, _) = h
        .call(&t, "lucina_timeline", json!({"before": "bad id!"}))
        .await;
    assert!(err);

    let (err, text) = h.call(&t, "gym_summary", json!({})).await;
    assert!(!err, "{text}");
    assert!(text.contains("Workouts this week: 2 of 3"), "{text}");
    assert!(text.contains("PR: Bench Press 82.5 kg × 5"), "{text}");
    let ins = h.mock.requests("/v1/insights/summary");
    assert_eq!(
        ins[0].headers["authorization"],
        format!("Bearer {SERVICE_TOKEN}")
    );
    assert_eq!(ins[0].query, "timezone=Europe%2FRome&workouts=5");

    let (err, text) = h.call(&t, "body_summary", json!({})).await;
    assert!(!err, "{text}");
    assert!(
        text.contains("current 80.5 kg, 7-day average 80.6 kg"),
        "{text}"
    );
    assert!(text.contains("Goal 78.0 kg: +2.6 kg to go."), "{text}");
    assert!(text.contains("80.5 kg — after run"), "{text}");

    let r = h.rpc(&t, "resources/list", json!({})).await;
    let uris: Vec<&str> = r["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert_eq!(
        uris,
        ["lucina://timeline/recent", "lucina://shopping/default"]
    );
    let r = h
        .rpc(
            &t,
            "resources/read",
            json!({"uri": "lucina://shopping/default"}),
        )
        .await;
    assert_eq!(r["contents"][0]["uri"], "lucina://shopping/default");
    assert!(r["contents"][0]["text"].as_str().unwrap().contains("Latte"));
    let r = h
        .rpc(
            &t,
            "resources/read",
            json!({"uri": "lucina://timeline/recent"}),
        )
        .await;
    assert!(r["contents"][0]["text"].as_str().unwrap().contains("Fatto"));
    let (_, _, v) = h
        .rpc_raw(Some(&t), "resources/read", json!({"uri": "lucina://nope"}))
        .await;
    assert!(v["error"].is_object());

    let r = h.rpc(&t, "prompts/list", json!({})).await;
    assert_eq!(r["prompts"][0]["name"], "daily_briefing");
    let r = h
        .rpc(
            &t,
            "prompts/get",
            json!({"name": "daily_briefing", "arguments": {"date": "2026-10-22"}}),
        )
        .await;
    let text = r["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(
        text.contains("Tomorrow starts at 9:30 with the dentist call."),
        "{text}"
    );
    assert!(text.contains("Call the dentist"), "{text}");
    assert_eq!(h.mock.requests("/v1/agenda")[0].query, "date=2026-10-22");
}

#[tokio::test]
async fn gym_without_service_token_uses_state() {
    let h = start_with(|c, _| c.service_token = None).await;
    let t = h.issue("claude-code", "read");
    let (err, text) = h.call(&t, "gym_summary", json!({})).await;
    assert!(!err, "{text}");
    assert!(text.contains("Workouts this week: 1 of 3"), "{text}");
    assert!(text.contains("Squat: 100 kg × 5 (e1RM 116.7 kg)"), "{text}");
    assert!(h.mock.requests("/v1/insights/summary").is_empty());
}

#[tokio::test]
async fn backend_failures_are_tool_errors() {
    let h = start_with(|c, _| c.backend_url = "http://127.0.0.1:9".into()).await;
    let t = h.issue("claude-code", "read,chat");
    let (err, text) = h.call(&t, "ask_lucina", json!({"message": "hi"})).await;
    assert!(err && text.contains("not reachable"), "{text}");
    let (err, text) = h.call(&t, "lucina_status", json!({})).await;
    assert!(
        !err && text.contains("Secretary server: unreachable"),
        "{text}"
    );
}

#[tokio::test]
async fn origins() {
    let h = start().await;
    let t = h.issue("grok", "read");
    for (origin, ok) in [
        ("https://chatgpt.com", true),
        ("https://getsecretary.tech", true),
        ("https://evil.example", false),
    ] {
        let resp = h
            .http
            .post(format!("{}/mcp", h.base))
            .bearer_auth(&t)
            .header("origin", origin)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status() == 200, ok, "{origin}: {}", resp.status());
    }
}
