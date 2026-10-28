//! MCP over Streamable HTTP against a mock secretary-server.

mod common;

use common::*;
use serde_json::{json, Value};

const ALL_TOOLS: [&str; 35] = [
    "ask_lucina",
    "lucina_timeline",
    "shopping_list",
    "shopping_add",
    "shopping_mark_bought",
    "reminders_list",
    "reminder_create",
    "reminder_cancel",
    "calendar_list",
    "calendar_create",
    "gym_summary",
    "body_summary",
    "note_save",
    "note_append",
    "note_update",
    "note_get",
    "notes_search",
    "note_labels",
    "ops_record",
    "ops_search",
    "ops_get",
    "tasks_list",
    "task_add",
    "task_complete",
    "lucina_status",
    "library_list_books",
    "library_search",
    "library_get_book",
    "library_get_progress",
    "library_get_highlights",
    "library_get_recent_reading",
    "library_get_brief",
    "library_find_related_notes",
    "library_sync_status",
    "library_generate_brief",
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
    assert!(www.contains("scope=\"read chat write computer\""), "{www}");
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

#[tokio::test]
async fn calendar_tools_never_write_directly() {
    let h = start().await;
    let t = h.issue("claude-code", "read,write");
    let (err, text) = h
        .call(&t, "calendar_list", json!({"from": "2026-10-22"}))
        .await;
    assert!(!err, "{text}");
    assert!(
        text.contains("- 2026-10-22 09:30–10:00 Dentist (Google) @ Via Roma 1 [id google:abc]"),
        "{text}"
    );
    assert!(
        text.contains("- 2026-10-23 (all day) Birthday (Casa) [id iphone:EK1]"),
        "{text}"
    );
    assert!(text.contains("needs reconnecting"), "{text}");
    assert_eq!(
        h.mock.requests("/v1/calendar/events")[0].query,
        "from=2026-10-22&days=7"
    );
    let (err, _) = h.call(&t, "calendar_list", json!({"from": "x&y=1"})).await;
    assert!(err);
    let (err, text) = h
        .call(
            &t,
            "calendar_create",
            json!({"title": "Padel", "start": "2026-10-24T10:00:00+02:00", "location": "Club"}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.contains("needs owner confirmation in the app") && text.contains("prop_1"),
        "{text}"
    );
    let p = &h.mock.requests("/v1/calendar/proposals")[0];
    assert_eq!(
        p.body,
        json!({"title": "Padel", "start": "2026-10-24T10:00:00+02:00", "location": "Club", "from": "mcp:claude-code"})
    );
    let (err, text) = h
        .call(&t, "calendar_create", json!({"title": "x", "start": "bad"}))
        .await;
    assert!(err && text.contains("refused"), "{text}");
    // read-only tokens do not see calendar_create
    let ro = h.issue("grok", "read");
    let names = h.tool_names(&ro).await;
    assert!(
        names.contains(&"calendar_list".to_string())
            && !names.contains(&"calendar_create".to_string())
    );
}

#[tokio::test]
async fn internal_status_lists_clients_without_secrets() {
    let off = start().await;
    let r = off
        .http
        .get(format!("{}/internal/status", off.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 404);
    let h = common::start_with(|cfg, _| cfg.status_token = Some("st-123".into())).await;
    let tok = h.issue("claude-code", "read,chat");
    let rec = secretary_mcp::tokens::TokenStore::new(h.data_dir())
        .list()
        .unwrap();
    let r = h
        .http
        .get(format!("{}/internal/status", h.base))
        .bearer_auth("nope")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 401);
    let r = h
        .http
        .get(format!("{}/internal/status", h.base))
        .bearer_auth("st-123")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["clients"][0]["caller"], "claude-code");
    assert_eq!(v["clients"][0]["via"], "token");
    assert_eq!(v["clients"][0]["active"], true);
    assert_eq!(v["clients"][0]["scopes"], json!(["read", "chat"]));
    let text = v.to_string();
    assert!(!text.contains(&tok) && !text.contains(&rec[0].hash) && !text.contains(&rec[0].id));
}

#[tokio::test]
async fn notes_save_append_get_search() {
    let h = start().await;
    let t = h.issue("chatgpt", "read,chat,write");
    let briefing = "# CRAFT-IT4SD\n\nMarianne (Aarhus University) offers paid hours.";
    let (err, text) = h
        .call(
            &t,
            "note_save",
            json!({"title": "CRAFT-IT4SD — meeting with Marianne & Jonas", "slug": "craftit-meeting",
                   "content": briefing, "tags": ["work"]}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(text.contains("Saved note \"craftit-meeting\""), "{text}");
    assert!(text.contains("Secretary app under Notes"), "{text}");
    let reqs = h.mock.requests("/v1/notes");
    let post = reqs.iter().find(|r| r.method == "POST").unwrap();
    assert_eq!(post.body["source"], "chatgpt", "the caller is the source");
    assert_eq!(post.body["slug"], "craftit-meeting");
    assert_eq!(post.body["tags"], json!(["work"]));
    assert_eq!(
        post.headers["authorization"],
        format!("Bearer {DEVICE_TOKEN}")
    );

    // Part 2 of a long document, as a new paragraph; then a chunk glued on.
    let (err, text) = h
        .call(
            &t,
            "note_append",
            json!({"slug": "craftit-meeting", "content": "## Part 2\nQuestions"}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.starts_with("Appended to note \"craftit-meeting\""),
        "{text}"
    );
    let (err, _) = h
        .call(
            &t,
            "note_append",
            json!({"slug": "craftit-meeting", "content": " for Jonas.", "continuation": true}),
        )
        .await;
    assert!(!err);
    let appends = h.mock.requests("/v1/notes/craftit-meeting/append");
    assert_eq!(appends[0].body["separator"], "\n\n");
    assert!(appends[1].body.get("separator").is_none());

    let (err, text) = h
        .call(&t, "note_get", json!({"slug": "craftit-meeting"}))
        .await;
    assert!(!err, "{text}");
    assert!(
        text.contains("\"CRAFT-IT4SD — meeting with Marianne & Jonas\" (slug craftit-meeting"),
        "{text}"
    );
    assert!(text.contains("by chatgpt"), "{text}");
    assert!(text.ends_with("## Part 2\nQuestions for Jonas."), "{text}");

    // Paging through a long note.
    let (_, text) = h
        .call(
            &t,
            "note_get",
            json!({"slug": "craftit-meeting", "max_chars": 1000, "offset": 10}),
        )
        .await;
    assert!(text.contains("Showing characters 10-"), "{text}");

    let (err, text) = h.call(&t, "notes_search", json!({"q": "aarhus"})).await;
    assert!(!err, "{text}");
    assert!(text.starts_with("1 note(s) matching \"aarhus\""), "{text}");
    assert!(text.contains("craftit-meeting"), "{text}");
    let (_, text) = h
        .call(&t, "notes_search", json!({"q": "nothing like this"}))
        .await;
    assert_eq!(text, "No notes matching \"nothing like this\".");

    // Saving over an existing slug needs replace=true.
    let (err, text) = h
        .call(
            &t,
            "note_save",
            json!({"title": "Other", "slug": "craftit-meeting", "content": "x"}),
        )
        .await;
    assert!(err);
    assert!(
        text.contains("already exists") && text.contains("replace=true"),
        "{text}"
    );
    let (err, text) = h
        .call(
            &t,
            "note_save",
            json!({"title": "Other", "slug": "craftit-meeting", "content": "new", "replace": true}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.starts_with("Replaced note \"craftit-meeting\""),
        "{text}"
    );
    let patch = h
        .mock
        .requests("/v1/notes/craftit-meeting")
        .into_iter()
        .find(|r| r.method == "PATCH")
        .unwrap();
    assert_eq!(patch.body["content"], "new");

    // Errors the model can act on.
    let (err, text) = h.call(&t, "note_get", json!({"slug": "missing"})).await;
    assert!(err && text.contains("No note \"missing\""), "{text}");
    let (err, text) = h
        .call(
            &t,
            "note_append",
            json!({"slug": "missing", "content": "x"}),
        )
        .await;
    assert!(err && text.contains("note_save first"), "{text}");
    let (err, text) = h.call(&t, "note_get", json!({})).await;
    assert!(err && text.contains("slug"), "{text}");
    let (err, text) = h.call(&t, "note_get", json!({"slug": "../etc"})).await;
    assert!(err && text.contains("lower-case"), "{text}");
    let (err, text) = h
        .call(
            &t,
            "note_save",
            json!({"title": "big", "content": "x".repeat(256 * 1024 + 1)}),
        )
        .await;
    assert!(err && text.contains("note_append"), "{text}");
}

fn patches(h: &Harness, path: &str) -> Vec<Recorded> {
    h.mock
        .requests(path)
        .into_iter()
        .filter(|r| r.method == "PATCH")
        .collect()
}

async fn shopping_note(h: &Harness, t: &str) {
    let (err, text) = h
        .call(
            t,
            "note_save",
            json!({"title": "Shopping", "items": ["Milk", "Bread", "Eggs"], "labels": ["GodLevel", "Fisica 2"],
                   "color": "Blue", "pinned": true}),
        )
        .await;
    assert!(!err, "{text}");
}

#[tokio::test]
async fn keep_note_save_and_get() {
    let h = start().await;
    let t = h.issue("claude-code", "read,chat,write");
    let (err, text) = h
        .call(
            &t,
            "note_save",
            json!({"title": "Shopping", "items": ["Milk", {"text": "Bread", "checked": true}, {"text": "Eggs"}],
                   "labels": ["GodLevel", "Fisica 2"], "color": "Blue", "pinned": true, "archived": true}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.contains("Saved note \"shopping\" (Shopping) (checklist, 1/3 done)"),
        "{text}"
    );
    let post = h
        .mock
        .requests("/v1/notes")
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap();
    assert_eq!(post.body["color"], "blue", "colour names are lower-cased");
    assert_eq!(post.body["labels"], json!(["GodLevel", "Fisica 2"]));
    assert_eq!(post.body["pinned"], true);
    assert_eq!(post.body["archived"], true);
    assert_eq!(
        post.body["items"],
        json!([
            {"text": "Milk", "checked": false, "indent": 0},
            {"text": "Bread", "checked": true, "indent": 0},
            {"text": "Eggs", "checked": false, "indent": 0},
        ])
    );
    assert!(
        post.body.get("content").is_none(),
        "a checklist needs no content"
    );

    // note_get: labels by display name, colour, state, numbered items (no derived content).
    let (err, text) = h.call(&t, "note_get", json!({"slug": "shopping"})).await;
    assert!(!err, "{text}");
    assert!(text.contains("[pinned] [archived]"), "{text}");
    assert!(text.contains("colour blue"), "{text}");
    assert!(text.contains("labels: GodLevel, Fisica 2"), "{text}");
    assert!(
        text.ends_with("Checklist, 1/3 done:\n1. [ ] Milk\n2. [x] Bread\n3. [ ] Eggs"),
        "{text}"
    );
    assert!(!text.contains("- [ ]"), "{text}");

    // Attachments, links, collaborators and the reminder of a note.
    h.mock.state.lock().unwrap().notes.push(json!({
        "id": "n_rich", "slug": "rich", "title": "", "content": "Hello", "tags": [], "source": "google-keep",
        "createdAt": "2026-10-22T11:30:00.000Z", "updatedAt": "2026-10-22T11:30:00.000Z",
        "deletedAt": "2026-10-23T11:30:00.000Z",
        "attachments": [{"id": "a", "file": "a.jpg", "mimetype": "image/jpeg", "size": 2_500_000, "name": "photo.jpg"}],
        "annotations": [{"url": "https://example.org/x", "title": "Example"}],
        "collaborators": ["anna@example.com"],
        "reminder": {"at": "2026-11-03T09:00:00+01:00", "recurrence": "weekly", "lucinaId": "r9"},
    }));
    let (err, text) = h.call(&t, "note_get", json!({"slug": "rich"})).await;
    assert!(!err, "{text}");
    assert!(
        text.contains("\"untitled\"") && text.contains("[in the trash]"),
        "{text}"
    );
    assert!(
        text.contains("reminder 2026-11-03 09:00 (weekly)"),
        "{text}"
    );
    assert!(
        text.contains("Attachments: photo.jpg (image/jpeg, 2.4 MB)"),
        "{text}"
    );
    assert!(
        text.contains("Links: Example <https://example.org/x>"),
        "{text}"
    );
    assert!(text.contains("Shared with: anna@example.com"), "{text}");
    assert!(text.ends_with("5 characters.\n\nHello"), "{text}");

    // Mistakes the model can fix.
    let (err, text) = h
        .call(
            &t,
            "note_save",
            json!({"title": "x", "content": "y", "color": "mauve"}),
        )
        .await;
    assert!(err && text.contains("cerulean"), "{text}");
    let (err, text) = h.call(&t, "note_save", json!({"title": "x"})).await;
    assert!(err && text.contains("items"), "{text}");
    let (err, text) = h
        .call(
            &t,
            "note_save",
            json!({"title": "x", "labels": ["a,b"], "content": "y"}),
        )
        .await;
    assert!(err && text.contains("comma"), "{text}");

    // replace=true carries the Keep fields into the PATCH.
    let (err, text) = h
        .call(
            &t,
            "note_save",
            json!({"title": "Shopping", "slug": "shopping", "items": ["Tea"]}),
        )
        .await;
    assert!(err && text.contains("replace=true"), "{text}");
    let (err, text) = h
        .call(
            &t,
            "note_save",
            json!({"title": "Shopping", "slug": "shopping", "items": ["Tea"], "color": "red", "replace": true}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.starts_with("Replaced note \"shopping\" (Shopping) (checklist, 0/1 done)"),
        "{text}"
    );
    let p = &patches(&h, "/v1/notes/shopping")[0];
    assert_eq!(p.body["color"], "red");
    assert_eq!(p.body["items"][0]["text"], "Tea");
}

#[tokio::test]
async fn note_update_checks_items_by_number_and_by_text() {
    let h = start().await;
    let t = h.issue("claude-code", "read,chat,write");
    shopping_note(&h, &t).await;
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "check": [1, "bread"], "addItems": ["Butter", {"text": "Jam", "checked": true}],
                   "removeItems": [3]}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.starts_with(
            "Updated note \"shopping\" (Shopping): checked 2 items, removed 1 item, added 2 items."
        ),
        "{text}"
    );
    assert!(
        text.ends_with(
            "Checklist, 3/4 done:\n1. [x] Milk\n2. [x] Bread\n3. [ ] Butter\n4. [x] Jam"
        ),
        "{text}"
    );
    let p = &patches(&h, "/v1/notes/shopping")[0];
    assert!(p.body["baseUpdatedAt"].is_string(), "{}", p.body);
    assert!(p.body.get("content").is_none(), "{}", p.body);
    let items = p.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 4);
    assert!(
        items[0]["id"].as_str().unwrap().starts_with("i_"),
        "existing items keep their ids"
    );
    assert!(
        items[2].get("id").is_none(),
        "new items get theirs from the server"
    );

    // uncheck by number and by text, case-insensitive; the numbers are note_get's.
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"id": "n_00000000000000000000", "uncheck": [2, "MILK"]}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(text.contains("unchecked 2 items"), "{text}");
    assert!(text.contains("1. [ ] Milk\n2. [ ] Bread"), "{text}");

    // Errors say what the list holds.
    let (err, text) = h
        .call(&t, "note_update", json!({"slug": "shopping", "check": [9]}))
        .await;
    assert!(
        err && text.contains("No item number 9") && text.contains("1. Milk"),
        "{text}"
    );
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "check": ["nope"]}),
        )
        .await;
    assert!(
        err && text.contains("No item with the text \"nope\"") && text.contains("2. Bread"),
        "{text}"
    );
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "addItems": ["Milk"]}),
        )
        .await;
    assert!(!err, "{text}");
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "check": ["milk"]}),
        )
        .await;
    assert!(
        err && text.contains("2 items have the text") && text.contains("numbers 1, 5"),
        "{text}"
    );
    let (err, text) = h.call(&t, "note_update", json!({"slug": "shopping"})).await;
    assert!(err && text.contains("Nothing to change"), "{text}");
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "content": "x", "check": [1]}),
        )
        .await;
    assert!(err && text.contains("not both"), "{text}");
    let (err, text) = h
        .call(&t, "note_update", json!({"slug": "ghost", "pinned": true}))
        .await;
    assert!(err && text.contains("No note \"ghost\""), "{text}");
    // A text note is not a checklist.
    h.call(
        &t,
        "note_save",
        json!({"title": "Plain", "content": "a\nb"}),
    )
    .await;
    let (err, text) = h
        .call(&t, "note_update", json!({"slug": "plain", "check": [1]}))
        .await;
    assert!(err && text.contains("convert=\"list\""), "{text}");
}

#[tokio::test]
async fn note_update_retries_once_on_conflict() {
    let h = start().await;
    let t = h.issue("claude-code", "read,chat,write");
    shopping_note(&h, &t).await;
    h.mock.state.lock().unwrap().race = 1;
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "check": ["Milk"]}),
        )
        .await;
    assert!(!err, "{text}");
    let ps = patches(&h, "/v1/notes/shopping");
    assert_eq!(ps.len(), 2, "the first PATCH hit the 409");
    assert_ne!(
        ps[0].body["baseUpdatedAt"], ps[1].body["baseUpdatedAt"],
        "the retry edits the new version"
    );
    assert!(
        text.ends_with(
            "Checklist, 1/4 done:\n1. [x] Milk\n2. [ ] Bread\n3. [ ] Eggs\n4. [ ] Added elsewhere"
        ),
        "the concurrent change is kept: {text}"
    );

    // A second conflict is reported, not retried forever.
    h.mock.state.lock().unwrap().race = 2;
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "check": ["Bread"]}),
        )
        .await;
    assert!(err && text.contains("changed by someone else"), "{text}");
    assert_eq!(patches(&h, "/v1/notes/shopping").len(), 4);
}

#[tokio::test]
async fn note_update_labels_colour_reminder_and_convert() {
    let h = start().await;
    let t = h.issue("claude-code", "read,chat,write");
    shopping_note(&h, &t).await;
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "addLabels": ["#Work", "godlevel"], "removeLabels": ["Fisica  2"],
                   "color": "teal", "pinned": false, "archived": true, "title": "Groceries"}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.contains(
            "title, colour teal, unpinned, archived, label +Work, +godlevel, label -Fisica 2"
        ),
        "{text}"
    );
    let p = &patches(&h, "/v1/notes/shopping")[0];
    assert_eq!(p.body["labels"], json!(["godlevel", "Work"]), "{}", p.body);
    assert_eq!(p.body["color"], "teal");
    assert_eq!(p.body["pinned"], false);
    assert_eq!(p.body["archived"], true);
    assert_eq!(p.body["title"], "Groceries");
    assert!(p.body.get("items").is_none());

    // Reminder: set, keep the repeat when the time moves, clear.
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "reminderAt": "2026-11-03T09:00:00+01:00", "reminderRepeat": "weekly"}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(text.contains("reminder 2026-11-03 09:00"), "{text}");
    let (err, _) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "reminderAt": "2026-11-10T09:00:00+01:00"}),
        )
        .await;
    assert!(!err);
    let ps = patches(&h, "/v1/notes/shopping");
    assert_eq!(
        ps[1].body["reminder"],
        json!({"at": "2026-11-03T09:00:00+01:00", "recurrence": "weekly"})
    );
    assert_eq!(
        ps[2].body["reminder"],
        json!({"at": "2026-11-10T09:00:00+01:00", "recurrence": "weekly"})
    );
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "reminderAt": null}),
        )
        .await;
    assert!(!err && text.contains("reminder cleared"), "{text}");
    assert!(patches(&h, "/v1/notes/shopping")[3].body["reminder"].is_null());
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "reminderRepeat": "daily"}),
        )
        .await;
    assert!(err && text.contains("no reminder yet"), "{text}");
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "reminderAt": "tomorrow 9"}),
        )
        .await;
    assert!(err && text.contains("RFC 3339"), "{text}");
    // Lucina unreachable: the server answers 502 and the note is unchanged.
    h.call(
        &t,
        "note_save",
        json!({"title": "No Lucina", "content": "x", "slug": "no-lucina"}),
    )
    .await;
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "no-lucina", "reminderAt": "2026-11-03T09:00:00+01:00"}),
        )
        .await;
    assert!(
        err && text.contains("Lucina could not be reached"),
        "{text}"
    );

    // convert: list -> text -> list.
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "convert": "text"}),
        )
        .await;
    assert!(!err && text.contains("converted to text"), "{text}");
    assert!(!text.contains("Checklist"), "{text}");
    let (err, text) = h
        .call(
            &t,
            "note_update",
            json!({"slug": "shopping", "convert": "list", "addItems": ["Tea"]}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(text.contains("converted to list, added 1 item"), "{text}");
    assert!(
        text.ends_with("Checklist, 0/4 done:\n1. [ ] Milk\n2. [ ] Bread\n3. [ ] Eggs\n4. [ ] Tea"),
        "{text}"
    );
}

#[tokio::test]
async fn notes_search_filters_and_hits_show_keep_fields() {
    let h = start().await;
    let t = h.issue("claude-code", "read,chat,write");
    shopping_note(&h, &t).await;
    h.call(&t, "note_update", json!({"slug": "shopping", "check": [1], "reminderAt": "2026-11-03T09:00:00+01:00", "archived": true})).await;
    h.call(
        &t,
        "note_save",
        json!({"title": "Plain", "content": "milk is white"}),
    )
    .await;

    let (err, text) = h
        .call(
            &t,
            "notes_search",
            json!({"label": "#GodLevel", "color": "Blue", "type": "list", "archived": "only"}),
        )
        .await;
    assert!(!err, "{text}");
    let q = &h
        .mock
        .requests("/v1/notes")
        .into_iter()
        .filter(|r| r.method == "GET")
        .last()
        .unwrap()
        .query;
    for part in ["tag=godlevel", "color=blue", "type=list", "archived=only"] {
        assert!(q.contains(part), "{q}");
    }
    assert!(
        text.starts_with(
            "1 note(s) with label \"GodLevel\" in colour blue of type list (archived only):"
        ),
        "{text}"
    );
    assert!(text.contains("[pinned] [archived]"), "{text}");
    assert!(text.contains("colour blue · labels: GodLevel, Fisica 2 · checklist 1/3 done · reminder 2026-11-03 09:00"), "{text}");
    assert!(
        text.contains("- [x] Milk"),
        "the excerpt is the derived checklist: {text}"
    );

    // The old `tag` argument still works, and archived=exclude hides the archived one.
    let (_, text) = h
        .call(
            &t,
            "notes_search",
            json!({"tag": "godlevel", "archived": "exclude"}),
        )
        .await;
    assert_eq!(text, "No notes with label \"godlevel\" (archive excluded).");
    let (_, text) = h.call(&t, "notes_search", json!({"tag": "godlevel"})).await;
    assert!(
        text.starts_with("1 note(s) with label \"godlevel\":"),
        "{text}"
    );
    let (_, text) = h
        .call(&t, "notes_search", json!({"q": "milk", "type": "text"}))
        .await;
    assert!(
        text.contains("\"Plain\"") && !text.contains("\"Shopping\""),
        "{text}"
    );
    let (err, text) = h.call(&t, "notes_search", json!({"type": "video"})).await;
    assert!(err && text.contains("Invalid arguments"), "{text}");
}

#[tokio::test]
async fn note_labels_lists_display_names_with_counts() {
    let h = start().await;
    let t = h.issue("claude-code", "read");
    let (_, text) = h.call(&t, "note_labels", json!({})).await;
    assert_eq!(text, "No labels yet.");
    let w = h.issue("claude-code", "read,write");
    shopping_note(&h, &w).await;
    h.call(
        &w,
        "note_save",
        json!({"title": "Two", "content": "x", "labels": ["GodLevel"]}),
    )
    .await;
    let (err, text) = h.call(&t, "note_labels", json!({})).await;
    assert!(!err, "{text}");
    assert!(
        text.starts_with("2 label(s):\n- GodLevel (2 notes)\n- Fisica 2 (1 note)\n"),
        "{text}"
    );
    assert_eq!(h.mock.requests("/v1/notes/labels")[1].method, "GET");
}

#[tokio::test]
async fn notes_need_the_right_scopes() {
    let h = start().await;
    let reader = h.issue("grok", "read");
    let names = h.tool_names(&reader).await;
    assert!(names.contains(&"note_get".to_owned()) && names.contains(&"notes_search".to_owned()));
    assert!(!names.contains(&"note_save".to_owned()) && !names.contains(&"note_append".to_owned()));
    assert!(
        names.contains(&"note_labels".to_owned()) && !names.contains(&"note_update".to_owned())
    );
    let (err, text) = h
        .call(&reader, "note_update", json!({"slug": "x", "pinned": true}))
        .await;
    assert!(err && text.contains("lacks the `write` scope"), "{text}");
    let (err, text) = h
        .call(&reader, "note_save", json!({"title": "x", "content": "y"}))
        .await;
    assert!(err && text.contains("lacks the `write` scope"), "{text}");
    assert!(h.mock.requests("/v1/notes").is_empty());
}

#[tokio::test]
async fn ask_lucina_takes_12000_characters_and_points_to_notes() {
    let h = start().await;
    let t = h.issue("chatgpt", "read,chat");
    let (err, text) = h
        .call(&t, "ask_lucina", json!({"message": "x".repeat(12_000)}))
        .await;
    assert!(!err, "{text}");
    let (err, text) = h
        .call(&t, "ask_lucina", json!({"message": "x".repeat(12_001)}))
        .await;
    assert!(err && text.contains("12000"), "{text}");
    let r = h.rpc(&t, "tools/list", json!({})).await;
    let ask = r["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "ask_lucina")
        .unwrap()
        .clone();
    assert!(ask["description"].as_str().unwrap().contains("note_save"));
}

#[tokio::test]
async fn tasks_list_add_complete() {
    let h = start().await;
    let t = h.issue("claude-code", "read,chat,write");
    let (err, text) = h.call(&t, "tasks_list", json!({})).await;
    assert!(!err, "{text}");
    assert!(
        text.starts_with("Today (today is 2026-10-22; done today 1/5):"),
        "{text}"
    );
    assert!(text.contains("Overdue:\n- 10 min anki/memrise · Inbox · overdue since 2026-10-20 (2 d) ↻ every day · @lingue [id a2]"), "{text}");
    assert!(
        text.contains("- 20min speaking · Lingue · today · p1 [id a5]"),
        "{text}"
    );
    let q = &h.mock.requests("/v1/tasks/state")[0].query;
    assert_eq!(q, "view=today");
    // a project is a Todoist filter
    let _ = h.call(&t, "tasks_list", json!({"project": "Lingue"})).await;
    assert_eq!(
        h.mock.requests("/v1/tasks/state")[1].query,
        "view=filter&query=%23Lingue"
    );
    let (err, _) = h.call(&t, "tasks_list", json!({"view": "weird"})).await;
    assert!(err);

    let (err, text) = h
        .call(
            &t,
            "task_add",
            json!({"text": "chiamare Marco domani", "due": "today"}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.starts_with("Added \"chiamare Marco domani\", due \"domani\" (2026-10-23)"),
        "{text}"
    );
    let post = h.mock.requests("/v1/tasks/command").pop().unwrap();
    assert!(post.body["op_id"].as_str().unwrap().starts_with("mcp-"));
    assert_eq!(
        post.body["command"],
        json!({"type": "quick_add", "text": "chiamare Marco domani", "default_due": "today"})
    );
    assert!(
        post.body.get("timezone").is_none(),
        "the server fills in the owner's zone"
    );

    // by title: one match among today's tasks
    let (err, text) = h
        .call(&t, "task_complete", json!({"title": "shadowing"}))
        .await;
    assert!(!err, "{text}");
    let post = h.mock.requests("/v1/tasks/command").pop().unwrap();
    assert_eq!(
        post.body["command"],
        json!({"type": "complete", "task_id": "a1"})
    );
    // several matches: the ids come back
    let (err, text) = h.call(&t, "task_complete", json!({"title": "min"})).await;
    assert!(
        err && text.contains("Several tasks match")
            && text.contains("[id a5]")
            && text.contains("[id a2]"),
        "{text}"
    );
    // falls back to a search over all open tasks
    let (err, text) = h.call(&t, "task_complete", json!({"title": "club"})).await;
    assert!(!err, "{text}");
    let (err, text) = h.call(&t, "task_complete", json!({"id": "nope"})).await;
    assert!(err && text.contains("Task not found"), "{text}");
    let (err, text) = h
        .call(&t, "task_complete", json!({"title": "piano lessons"}))
        .await;
    assert!(err && text.contains("No open task matches"), "{text}");
}

#[tokio::test]
async fn tasks_need_the_right_scopes() {
    let h = start().await;
    let reader = h.issue("grok", "read");
    let names = h.tool_names(&reader).await;
    assert!(names.contains(&"tasks_list".to_owned()));
    assert!(
        !names.contains(&"task_add".to_owned()) && !names.contains(&"task_complete".to_owned())
    );
    assert!(
        !names.iter().any(|n| n.contains("delete")),
        "no delete tool"
    );
    let (err, text) = h.call(&reader, "task_add", json!({"text": "x"})).await;
    assert!(err && text.contains("lacks the `write` scope"), "{text}");
    assert!(h.mock.requests("/v1/tasks/command").is_empty());
}

#[tokio::test]
async fn library_tools_refs_scopes_and_backend_calls() {
    let h = start().await;
    let read = h.issue("reader", "read");
    for (name, args, expected) in [
        (
            "library_list_books",
            json!({"q":"book","onKindle":true,"kind":"dictionary"}),
            "[bk_0123456789abcdef, library]",
        ),
        (
            "library_search",
            json!({"q":"thought"}),
            "[bk_0123456789abcdef p.12 loc 170-172, kindle-clippings]",
        ),
        (
            "library_get_book",
            json!({"id":"bk_0123456789abcdef"}),
            "[bk_0123456789abcdef, library]",
        ),
        (
            "library_get_progress",
            json!({"id":"bk_0123456789abcdef"}),
            "progress unknown",
        ),
        (
            "library_get_highlights",
            json!({"id":"bk_0123456789abcdef"}),
            "[bk_0123456789abcdef p.12 loc 170-172, kindle-clippings]",
        ),
        (
            "library_get_recent_reading",
            json!({"days":7}),
            "sessions unavailable",
        ),
        (
            "library_get_brief",
            json!({"id":"bk_0123456789abcdef"}),
            "Generated extractive brief",
        ),
        (
            "library_find_related_notes",
            json!({"id":"bk_0123456789abcdef"}),
            "[obsidian:Vault/Reading/Book.md#L2]",
        ),
        (
            "library_find_related_notes",
            json!({"id":"bk_fedcba9876543210"}),
            "no Obsidian vault has been indexed",
        ),
        ("library_sync_status", json!({}), "imports log"),
    ] {
        let (err, text) = h.call(&read, name, args).await;
        assert!(!err, "{name}: {text}");
        assert!(text.contains(expected), "{name}: {text}");
    }
    assert!(h
        .mock
        .requests("/v1/library/books")
        .iter()
        .any(|r| r.query.contains("kind=dictionary")));
    let notes = h
        .mock
        .requests("/v1/library/books/bk_0123456789abcdef/notes");
    assert!(
        !notes.is_empty()
            && notes
                .iter()
                .all(|r| r.method == "GET" && r.query == "limit=20")
    );
    let args = json!({"id":"bk_0123456789abcdef"});
    assert!(
        h.call(&read, "library_generate_brief", args.clone())
            .await
            .0
    );
    let write = h.issue("writer", "write");
    let (err, text) = h.call(&write, "library_generate_brief", args).await;
    assert!(!err, "{text}");
    assert!(text.contains("not the owner's words"));
    assert!(text.contains("kindle-clippings]"));
    let requests = h
        .mock
        .requests("/v1/library/books/bk_0123456789abcdef/brief");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(
        requests[0].headers["authorization"],
        format!("Bearer {DEVICE_TOKEN}")
    );
    for (name, args) in [
        ("library_get_book", json!({"id":"../escape"})),
        ("library_list_books", json!({"limit":0})),
        ("library_get_recent_reading", json!({"days":0})),
        ("library_list_books", json!({"status":"owned"})),
        ("library_get_book", json!({})),
    ] {
        assert!(h.call(&read, name, args).await.0);
    }
}

#[tokio::test]
async fn computer_tools_need_the_computer_scope() {
    let h = start().await;
    let full = h.issue("grok", "read,chat,write");
    let names = h.tool_names(&full).await;
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("computer_") || n.starts_with("android_")),
        "{names:?}"
    );
    let (err, text) = h
        .call(&full, "computer_bash", json!({"command": "id"}))
        .await;
    assert!(err && text.contains("`computer` scope"), "{text}");
    assert!(h.mock.requests("/v1/computer/call").is_empty());

    let owner = h.issue("claude", "read,computer");
    let r = h.rpc(&owner, "tools/list", json!({})).await;
    let tools: Vec<&Value> = r["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| {
            t["name"].as_str().unwrap().contains("computer_")
                || t["name"].as_str().unwrap().starts_with("android_")
        })
        .collect();
    assert_eq!(tools.len(), 11);
    for t in &tools {
        assert_eq!(t["annotations"]["destructiveHint"], false, "{t}");
    }

    let r = h
        .rpc(
            &owner,
            "tools/call",
            json!({"name": "computer_screenshot", "arguments": {}}),
        )
        .await;
    assert_eq!(r["content"][0]["type"], "image");
    assert_eq!(r["content"][0]["mimeType"], "image/png");
    assert!(r["content"][1]["text"]
        .as_str()
        .unwrap()
        .contains("window_id"));

    let (err, text) = h
        .call(&owner, "computer_bash", json!({"command": "echo hi"}))
        .await;
    assert!(!err && text == "hi\n", "{text}");
    let sent = h.mock.requests("/v1/computer/call");
    assert_eq!(sent.last().unwrap().body["sandbox"], "workspace");
    assert_eq!(
        sent.last().unwrap().headers["x-secretary-caller"],
        "mcp:claude"
    );

    let (err, text) = h
        .call(&owner, "android_bash", json!({"command": "id"}))
        .await;
    assert!(err && text.contains("driving"), "{text}");

    let (err, text) = h
        .call(
            &owner,
            "computer_task",
            json!({"task": "open example.com", "wait_seconds": 10}),
        )
        .await;
    assert!(!err, "{text}");
    assert!(
        text.contains("completed in 12 s") && text.ends_with("Example Domain."),
        "{text}"
    );
    assert_eq!(
        h.mock.requests("/v1/computer/task")[0].body["task"],
        "open example.com"
    );
}
