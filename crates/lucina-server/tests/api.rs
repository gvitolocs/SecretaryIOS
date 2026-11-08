//! `/api/secretary/*` end to end against the fake DeepSeek.

mod common;

use std::time::Duration;

use common::{harness, script, TOKEN};
use futures_util::StreamExt;
use lucina_server::PlannerMode;
use reqwest::Method;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn chat_script(reply: &str) -> lucina_server::fake::Script {
    script(json!({"rules": [
        {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
        {"kind": "reply", "content": reply}
    ]}))
}

#[tokio::test]
async fn envelope_auth_and_routing() {
    let h = harness(chat_script("ciao"), |_| {}).await;
    let raw = |rb: reqwest::RequestBuilder| async move {
        let r = rb.send().await.unwrap();
        let s = r.status().as_u16();
        let hd = r.headers().clone();
        (s, r.json::<Value>().await.unwrap_or(Value::Null), hd)
    };
    let (s, b, hd) = raw(h.http.get(h.url("/health"))).await;
    assert_eq!((s, b["error"].as_str()), (401, Some("unauthorized")));
    assert_eq!(hd["cache-control"], "no-store");
    let (s, _, _) = raw(h.http.get(h.url("/health")).bearer_auth("wrong")).await;
    assert_eq!(s, 401);
    let (s, b) = h.get("/health").await;
    assert_eq!(s, 200);
    assert_eq!(b["data"]["persona"], "Lucina");
    assert_eq!(b["data"]["apiVersion"], 1);
    assert_eq!(b["data"]["timeZone"], "Europe/Rome");
    assert!(b["data"]["now"].as_str().unwrap().ends_with('Z'));
    assert_eq!(h.get("/nope").await.1["error"], "not_found");
    assert_eq!(h.get("/health/x").await.1["error"], "not_found");
    let (s, b, hd) = h.call(Method::DELETE, "/chat", None).await;
    assert_eq!((s, b["error"].as_str()), (405, Some("method_not_allowed")));
    assert_eq!(hd["allow"], "POST");
    let (s, b, _) = raw(h
        .http
        .post(h.url("/chat"))
        .bearer_auth(TOKEN)
        .body("{not json"))
    .await;
    assert_eq!((s, b["error"].as_str()), (400, Some("invalid_json")));
    let (s, b, _) = raw(h.http.post(h.url("/chat")).bearer_auth(TOKEN).body("[1]")).await;
    assert_eq!((s, b["error"].as_str()), (400, Some("bad_request")));
    let (s, b, _) = raw(h
        .http
        .post(h.url("/chat"))
        .bearer_auth(TOKEN)
        .body(format!("{{\"message\":\"{}\"}}", "x".repeat(70_000))))
    .await;
    assert_eq!((s, b["error"].as_str()), (413, Some("payload_too_large")));
    for bad in [
        json!({}),
        json!({"message": "  "}),
        json!({"message": "x", "channel": "sms"}),
        json!({"message": "x", "sessionId": "a b"}),
        json!({"message": 5}),
    ] {
        assert_eq!(
            h.post("/chat", bad.clone()).await.1["error"],
            "bad_request",
            "{bad}"
        );
    }
    assert_eq!(h.get("/jobs/nope").await.1["error"], "job_not_found");
    // /health (no auth) reports shadow mode
    let v: Value = h
        .http
        .get(format!("{}/health", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["channels"], json!({"telegram": false, "discord": false}));
    assert_eq!(v["planner"], "json");
    // disabled API: 404 before auth
    let off = harness(chat_script("x"), |c| c.secretary_enabled = false).await;
    let (s, b, _) = raw(off.http.get(off.url("/health"))).await;
    assert_eq!((s, b["error"].as_str()), (404, Some("not_found")));
}

#[tokio::test]
async fn chat_reply_timeline_and_client_message_dedupe() {
    let h = harness(chat_script("Ciao Giuseppe!"), |_| {}).await;
    // Not settled by tier 0 (fast chat is off here), so the model classifies it.
    let body = json!({"message": "che ne pensi dei gatti?", "sessionId": "iphone", "clientMessageId": "c-1", "channel": "ios"});
    let (s, b) = h.post("/chat", body.clone()).await;
    assert_eq!(s, 200, "{b}");
    let d = &b["data"];
    assert_eq!(d["reply"], "Ciao Giuseppe!");
    assert_eq!(d["action"], "reply");
    assert_eq!(d["job"], Value::Null);
    assert_eq!(d["sessionId"], "secretary_iphone");
    assert_eq!(d["clientMessageId"], "c-1");
    let lines = h.timeline_lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["id"], d["userItemId"]);
    assert_eq!(lines[0]["clientMessageId"], "c-1");
    assert_eq!(lines[0]["channel"], "ios");
    assert_eq!(lines[1]["replyTo"], d["userItemId"]);
    assert_eq!(lines[1]["id"], d["lucinaItemId"]);
    let calls = h.fake.requests().len();
    let (_, again) = h.post("/chat", body).await;
    assert_eq!(again["data"]["duplicate"], true);
    assert_eq!(again["data"]["lucinaItemId"], d["lucinaItemId"]);
    assert_eq!(
        h.fake.requests().len(),
        calls,
        "a replay does not run the turn"
    );
    // the planner saw the verbatim classifier prompt and the Secretary platform
    let reqs = h.fake.requests();
    assert!(reqs[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .starts_with("You classify Lucina user messages before tool execution."));
    assert!(reqs[0]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .starts_with("Platform: secretary\n\nUser: Giuseppe (operator, Secretary app)"));
    assert!(reqs[1]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("Persona background:"));
}

#[tokio::test]
async fn chat_schedules_and_lists_reminders_in_json_mode() {
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "contains": "ricordami", "content": "{\"intent\":\"schedule_reminder\",\"confidence\":0.95,\"reply_mode\":\"direct\"}"},
            {"kind": "intent", "contains": "promemoria", "content": "{\"intent\":\"list_reminders\",\"confidence\":0.95,\"reply_mode\":\"direct\",\"scope\":\"all\"}"}
        ]})),
        |_| {},
    )
    .await;
    let d = h.chat("ricordami tra 2 ore di chiamare la mamma").await;
    assert_eq!(d["action"], "schedule_reminder");
    assert_eq!(d["reminder"]["text"], "chiamare la mamma");
    assert_eq!(d["reminder"]["source"], "secretary");
    assert!(d["reply"]
        .as_str()
        .unwrap()
        .starts_with("Reminder set for "));
    let d = h.chat("quali promemoria ho?").await;
    assert_eq!(d["action"], "list_reminders");
    assert!(
        d["reply"].as_str().unwrap().contains("chiamare la mamma"),
        "{d}"
    );
}

/// The iPhone voice note of 2026-10-27: a relative day plus a part of the day and no
/// time. The planner gives no dueDate; the parser alone must schedule it.
#[tokio::test]
async fn chat_schedules_a_relative_day_reminder_without_a_time() {
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "contains": "ricordami", "content": "{\"intent\":\"schedule_reminder\",\"confidence\":0.95,\"reply_mode\":\"direct\"}"}
        ]})),
        |_| {},
    )
    .await;
    let d = h
        .chat("Ciao Lucina, ricordami di comprare il latte domani mattina.")
        .await;
    assert_eq!(d["action"], "schedule_reminder", "{d}");
    assert_eq!(d["reminder"]["text"], "comprare il latte", "{d}");
    let reply = d["reply"].as_str().unwrap();
    assert!(reply.starts_with("Reminder set for "), "{reply}");
    assert!(!reply.starts_with("Lucina: "), "{reply}");
    let due = chrono::DateTime::parse_from_rfc3339(d["reminder"]["dueAt"].as_str().unwrap())
        .unwrap()
        .with_timezone(&chrono_tz::Europe::Rome);
    let tomorrow = (chrono::Utc::now().with_timezone(&chrono_tz::Europe::Rome)
        + chrono::Duration::days(1))
    .date_naive();
    assert_eq!(due.date_naive(), tomorrow);
    assert_eq!(due.format("%H:%M").to_string(), "09:00");
}

async fn weather_mock() -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [{"name": "Copenhagen", "latitude": 55.676, "longitude": 12.566, "country": "Denmark", "country_code": "DK"}]})))
        .mount(&s).await;
    Mock::given(method("GET")).and(path("/v1/forecast"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "current": {"time": "2026-10-21T17:00", "temperature_2m": 9.0, "weather_code": 3},
            "daily": {"time": ["2026-10-21", "2026-10-22"], "weather_code": [3, 61], "temperature_2m_max": [11.0, 10.0], "temperature_2m_min": [6.0, 5.0],
                      "precipitation_sum": [0, 4.2], "precipitation_probability_max": [10, 85], "wind_speed_10m_max": [20, 30], "sunrise": ["", ""], "sunset": ["", ""]}
        })))
        .mount(&s).await;
    s
}

#[tokio::test]
async fn tools_mode_runs_tools_and_records_them() {
    let meteo = weather_mock().await;
    let uri = meteo.uri();
    let h = harness(
        script(json!({"rules": [
            {"kind": "agent", "contains": "tempo", "afterTool": false, "toolCalls": [{"name": "weather", "arguments": {"location": "Copenhagen", "days": 2}}]},
            {"kind": "agent", "contains": "tempo", "afterTool": true, "content": "Domani a Copenaghen: pioggia leggera, 5-10 °C."},
            {"kind": "agent", "contains": "17%", "afterTool": false, "toolCalls": [{"name": "calculator", "arguments": {"expression": "17% * 230"}}]},
            {"kind": "agent", "contains": "17%", "afterTool": true, "content": "Il 17% di 230 è 39,1."}
        ]})),
        move |c| {
            c.planner = PlannerMode::Tools;
            c.endpoints.open_meteo_forecast = uri.clone();
            c.endpoints.open_meteo_geocoding = uri;
        },
    )
    .await;
    let d = h.chat("che tempo fa domani a Copenhagen?").await;
    assert_eq!(d["reply"], "Domani a Copenaghen: pioggia leggera, 5-10 °C.");
    assert_eq!(d["action"], "reply");
    let reqs = h.fake.requests();
    let tool_msg = reqs[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    assert!(tool_msg["content"]
        .as_str()
        .unwrap()
        .contains("\"light rain\""));
    let lines = h.timeline_lines();
    assert_eq!(lines.len(), 3, "user, tool, reply");
    assert_eq!(lines[1]["meta"]["kind"], "tool");
    assert_eq!(lines[1]["meta"]["tool"], "weather");
    assert_eq!(lines[1]["replyTo"], lines[0]["id"]);
    let d = h.chat("quanto fa 17% di 230?").await;
    assert_eq!(d["reply"], "Il 17% di 230 è 39,1.");
}

#[tokio::test]
async fn confirmation_flow_across_turns() {
    let h = harness(
        script(json!({"rules": [
            {"kind": "agent", "contains": "cancella", "afterTool": false, "toolCalls": [{"name": "reminders", "arguments": {"action": "cancel", "id": "rem-1"}}]},
            {"kind": "agent", "contains": "cancella", "afterTool": true, "content": "Vuoi davvero cancellarlo? Rispondi sì."},
            {"kind": "reply", "content": "Fatto, l'ho cancellato."}
        ]})),
        |c| {
            c.planner = PlannerMode::Tools;
            std::fs::write(&c.reminders_path, serde_json::to_string_pretty(&json!({"reminders": [{
                "id": "rem-1", "dedupeKey": "k", "text": "dentista", "dueAt": "2099-10-22T07:30:00.000Z", "timeZone": "Europe/Rome",
                "chatId": "42", "userPeerId": "giuseppe", "userLabel": "", "persona": "Lucina", "source": "secretary",
                "status": "pending", "createdAt": "2026-10-21T10:00:00.000Z", "sentAt": null, "lastError": null
            }], "updatedAt": "x"})).unwrap()).unwrap();
        },
    )
    .await;
    let d = h.chat("cancella il promemoria del dentista").await;
    assert_eq!(d["reply"], "Vuoi davvero cancellarlo? Rispondi sì.");
    let (_, list) = h.get("/reminders").await;
    assert_eq!(
        list["data"]["reminders"].as_array().unwrap().len(),
        1,
        "not cancelled in the same turn"
    );
    assert!(
        h.timeline_lines()
            .iter()
            .all(|l| l["meta"]["kind"] != "tool"),
        "a deferred call is not an executed tool"
    );
    let d = h.chat("sì").await;
    assert_eq!(d["reply"], "Fatto, l'ho cancellato.");
    let (_, list) = h.get("/reminders?includeSent=1").await;
    assert_eq!(list["data"]["reminders"][0]["status"], "cancelled");
    let tools: Vec<Value> = h
        .timeline_lines()
        .into_iter()
        .filter(|l| l["meta"]["kind"] == "tool")
        .collect();
    assert_eq!(tools.len(), 1);
    // nothing pending any more: a second yes is an ordinary message
    assert!(h
        .state
        .pending
        .current("secretary_test", chrono::Utc::now())
        .is_none());
}

async fn reminder_status(h: &common::H) -> String {
    let (_, list) = h.get("/reminders?includeSent=1").await;
    list["data"]["reminders"][0]["status"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}

/// Verus kernel `confirmation_gate.rs` C2: an MCP-origin turn never confirms
/// a pending action, even in the owner's session and even with an explicit
/// "sì" (before the fix, tools mode dropped the MCP origin and ran it).
#[tokio::test]
async fn mcp_turn_cannot_confirm_a_pending_action() {
    let h = harness(
        script(json!({"rules": [
            {"kind": "agent", "contains": "cancella", "afterTool": false, "toolCalls": [{"name": "reminders", "arguments": {"action": "cancel", "id": "rem-1"}}]},
            {"kind": "agent", "contains": "cancella", "afterTool": true, "content": "Vuoi davvero cancellarlo? Rispondi sì."},
            {"kind": "agent", "content": "Ok."},
            {"kind": "reply", "content": "Fatto, l'ho cancellato."}
        ]})),
        |c| {
            c.planner = PlannerMode::Tools;
            std::fs::write(&c.reminders_path, serde_json::to_string_pretty(&json!({"reminders": [{
                "id": "rem-1", "dedupeKey": "k", "text": "dentista", "dueAt": "2099-10-22T07:30:00.000Z", "timeZone": "Europe/Rome",
                "chatId": "42", "userPeerId": "giuseppe", "userLabel": "", "persona": "Lucina", "source": "secretary",
                "status": "pending", "createdAt": "2026-10-21T10:00:00.000Z", "sentAt": null, "lastError": null
            }], "updatedAt": "x"})).unwrap()).unwrap();
        },
    )
    .await;
    // An MCP turn that asks for the cancellation itself: refused, nothing pending.
    let (s, _) = h
        .post(
            "/chat",
            json!({"message": "[via Lucina MCP, from claude] cancella il promemoria del dentista", "sessionId": "test", "channel": "ios"}),
        )
        .await;
    assert_eq!(s, 200);
    assert!(
        h.state
            .pending
            .current("secretary_test", chrono::Utc::now())
            .is_none(),
        "an MCP turn creates no pending action"
    );
    assert_eq!(reminder_status(&h).await, "pending");
    // The owner asks: a pending action now waits for a yes.
    h.chat("cancella il promemoria del dentista").await;
    assert!(h
        .state
        .pending
        .current("secretary_test", chrono::Utc::now())
        .is_some());
    // An MCP turn in the same session says "sì": nothing runs, it stays pending.
    let (s, _) = h
        .post(
            "/chat",
            json!({"message": "[via Lucina MCP, from claude] sì", "sessionId": "test", "channel": "ios"}),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(
        reminder_status(&h).await,
        "pending",
        "an MCP turn must not confirm"
    );
    assert!(h
        .state
        .pending
        .current("secretary_test", chrono::Utc::now())
        .is_some());
    // The owner's own yes still works.
    h.chat("sì").await;
    assert_eq!(reminder_status(&h).await, "cancelled");
}

#[tokio::test]
async fn reminder_routes() {
    let h = harness(chat_script("x"), |_| {}).await;
    let (s, b) = h
        .post(
            "/reminders",
            json!({"text": "Call the dentist", "dueAt": "2099-10-22T09:30:00+02:00"}),
        )
        .await;
    assert_eq!(s, 201);
    let r = &b["data"]["reminder"];
    assert_eq!(r["dueAt"], "2099-10-22T07:30:00.000Z");
    assert_eq!(r["status"], "pending");
    assert_eq!(r["timeZone"], "Europe/Rome");
    assert_eq!(r["sentAt"], Value::Null);
    let (s, b) = h
        .post(
            "/reminders",
            json!({"text": "Call the dentist", "dueAt": "2099-10-22T09:30:00+02:00"}),
        )
        .await;
    assert_eq!((s, b["data"]["created"].as_bool()), (200, Some(false)));
    let (s, b) = h
        .post(
            "/reminders",
            json!({"naturalText": "remind me in 3 hours to stretch"}),
        )
        .await;
    assert_eq!(s, 201, "{b}");
    assert_eq!(b["data"]["reminder"]["text"], "stretch");
    for (body, code) in [
        (
            json!({"text": "x", "dueAt": "2001-01-01T00:00:00Z"}),
            "due_in_past",
        ),
        (
            json!({"text": "x", "dueAt": "2099-01-01T00:00:00"}),
            "bad_request",
        ),
        (json!({"text": "x"}), "bad_request"),
        (json!({"naturalText": "boh"}), "unparseable_reminder"),
        (json!({"naturalText": "28 August 2001 — x"}), "due_in_past"),
        (
            json!({"naturalText": "in 2 hours x", "text": "y"}),
            "bad_request",
        ),
        (json!({"text": 5, "dueAt": "x"}), "bad_request"),
    ] {
        assert_eq!(
            h.post("/reminders", body.clone()).await.1["error"],
            code,
            "{body}"
        );
    }
    let (_, list) = h.get("/reminders?scope=upcoming").await;
    assert_eq!(list["data"]["reminders"].as_array().unwrap().len(), 2);
    assert_eq!(list["data"]["includeSent"], false);
    assert_eq!(
        h.get("/reminders?scope=week").await.1["error"],
        "bad_request"
    );
    assert_eq!(
        h.get("/reminders?includeSent=2").await.1["error"],
        "bad_request"
    );
    let id = list["data"]["reminders"][1]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (s, b, _) = h
        .call(Method::DELETE, &format!("/reminders/{id}"), None)
        .await;
    assert_eq!((s, b["data"]["cancelled"].as_bool()), (200, Some(true)));
    assert_eq!(b["data"]["reminder"]["status"], "cancelled");
    let (_, b, _) = h
        .call(Method::DELETE, &format!("/reminders/{id}"), None)
        .await;
    assert_eq!(b["data"]["cancelled"], false);
    let (s, b, _) = h.call(Method::DELETE, "/reminders/unknown-id", None).await;
    assert_eq!((s, b["error"].as_str()), (404, Some("reminder_not_found")));
    let (s, _, _) = h.call(Method::DELETE, "/reminders/bad!id", None).await;
    assert_eq!(s, 404);
    let (_, all) = h.get("/reminders?scope=all&includeSent=1").await;
    assert_eq!(all["data"]["reminders"].as_array().unwrap().len(), 2);
    // reminders disabled / no owner chat
    let off = harness(chat_script("x"), |c| c.reminders_enabled = false).await;
    assert_eq!(
        off.post(
            "/reminders",
            json!({"text": "x", "dueAt": "2099-01-01T00:00:00Z"})
        )
        .await
        .1["error"],
        "reminders_disabled"
    );
    let nochat = harness(chat_script("x"), |c| c.owner_chat_id = None).await;
    assert_eq!(
        nochat
            .post(
                "/reminders",
                json!({"text": "x", "dueAt": "2099-01-01T00:00:00Z"})
            )
            .await
            .1["error"],
        "owner_chat_unconfigured"
    );
}

#[tokio::test]
async fn reminder_writes_forward_to_node() {
    let node = MockServer::start().await;
    Mock::given(method("POST")).and(path("/api/secretary/reminders"))
        .and(wiremock::matchers::header("authorization", "Bearer node-token"))
        .and(wiremock::matchers::body_json(json!({"naturalText": "remind me in 3 hours to stretch"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"ok": true, "data": {"created": true, "reminder": {"id": "n1", "text": "stretch", "dueAt": "2099-01-01T00:00:00.000Z", "timeZone": "Europe/Rome", "status": "pending", "source": "secretary", "createdAt": "x", "sentAt": null, "cancelledAt": null}}})))
        .expect(1).mount(&node).await;
    let token_file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(token_file.path(), "node-token\n").unwrap();
    let uri = node.uri();
    let h = harness(chat_script("x"), move |c| {
        c.reminders_writer = lucina_server::WriterKind::Node;
        c.node_url = uri;
        c.node_token = Some("node-token".into());
    })
    .await;
    let (s, b) = h
        .post(
            "/reminders",
            json!({"naturalText": "remind me in 3 hours to stretch"}),
        )
        .await;
    assert_eq!((s, b["data"]["reminder"]["id"].as_str()), (201, Some("n1")));
    assert!(
        !h.dir.path().join("reminders.json").exists(),
        "Rust never wrote the file"
    );
    let none = harness(chat_script("x"), |c| {
        c.reminders_writer = lucina_server::WriterKind::Node;
        c.node_token = None;
    })
    .await;
    let (s, b) = none
        .post(
            "/reminders",
            json!({"text": "x", "dueAt": "2099-01-01T00:00:00Z"}),
        )
        .await;
    assert_eq!((s, b["error"].as_str()), (503, Some("reminders_disabled")));
}

#[tokio::test]
async fn agenda_briefing_cache() {
    let h = harness(script(json!({"rules": [{"kind": "reply", "content": "Giornata libera. Nessun impegno. Riposa. Uno. Due. Tre. Sette."}]})), |_| {}).await;
    let (s, b) = h.get("/agenda?date=2099-10-22").await;
    assert_eq!(s, 200, "{b}");
    let d = &b["data"];
    assert_eq!(d["date"], "2099-10-22");
    assert_eq!(
        d["briefing"],
        "Giornata libera. Nessun impegno. Riposa. Uno. Due. Tre."
    );
    assert_eq!(d["briefingCached"], false);
    assert!(d["briefingGeneratedAt"].is_string());
    let n = h.fake.requests().len();
    let (_, b) = h.get("/agenda?date=2099-10-22").await;
    assert_eq!(b["data"]["briefingCached"], true);
    assert_eq!(h.fake.requests().len(), n);
    let (_, b) = h.get("/agenda?date=2099-10-22&refresh=1").await;
    assert_eq!(b["data"]["briefingCached"], false);
    // a new reminder that day invalidates the cache
    h.post(
        "/reminders",
        json!({"text": "dentista", "dueAt": "2099-10-22T09:30:00+02:00"}),
    )
    .await;
    let (_, b) = h.get("/agenda?date=2099-10-22").await;
    assert_eq!(b["data"]["briefingCached"], false);
    assert_eq!(b["data"]["reminders"][0]["text"], "dentista");
    let prompt = h.fake.requests().last().unwrap()["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(prompt.contains("- 09:30 dentista"), "{prompt}");
    assert_eq!(
        h.get("/agenda?date=2099-02-30").await.1["error"],
        "bad_request"
    );
    assert_eq!(h.get("/agenda?refresh=x").await.1["error"], "bad_request");
}

#[tokio::test]
async fn timeline_pages_and_sse() {
    let h = harness(chat_script("ok"), |c| c.timeline_max_streams = 2).await;
    for i in 0..3 {
        h.chat(&format!("m{i}")).await;
    }
    let (_, b) = h.get("/timeline?limit=4").await;
    let items = b["data"]["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 4);
    assert!(
        items[0]["id"].as_str().unwrap() > items[1]["id"].as_str().unwrap(),
        "newest first"
    );
    let next = b["data"]["nextBefore"].as_str().unwrap().to_owned();
    let (_, b) = h.get(&format!("/timeline?before={next}&limit=200")).await;
    assert_eq!(b["data"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(b["data"]["nextBefore"], Value::Null);
    assert_eq!(h.get("/timeline?limit=0").await.1["error"], "bad_request");
    assert_eq!(
        h.get("/timeline?before=nope").await.1["error"],
        "bad_request"
    );

    // SSE: replay after Last-Event-ID, then live items and pings
    let oldest = b["data"]["items"][1]["id"].as_str().unwrap().to_owned();
    let resp = h
        .http
        .get(h.url("/timeline/stream"))
        .bearer_auth(TOKEN)
        .header("last-event-id", &oldest)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.headers()["content-type"],
        "text/event-stream; charset=utf-8"
    );
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while buf.matches("event: item").count() < 5 && tokio::time::Instant::now() < deadline {
        if let Ok(Some(Ok(chunk))) =
            tokio::time::timeout(Duration::from_millis(500), stream.next()).await
        {
            buf.push_str(&String::from_utf8_lossy(&chunk));
        }
    }
    assert!(buf.starts_with("retry: 5000\n: connected\n\n"), "{buf}");
    assert_eq!(buf.matches("event: item").count(), 5, "{buf}");
    // a live item from this process, and a line appended by another writer
    h.chat("live").await;
    let foreign = lucina_store::timeline::normalize(
        lucina_store::NewItem {
            channel: "telegram".into(),
            role: "user".into(),
            text: "from node".into(),
            ..Default::default()
        },
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap();
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(h.dir.path().join("timeline.jsonl"))
            .unwrap();
        writeln!(f, "{}", serde_json::to_string(&foreign).unwrap()).unwrap();
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !(buf.contains("from node") && buf.contains(": ping"))
        && tokio::time::Instant::now() < deadline
    {
        if let Ok(Some(Ok(chunk))) =
            tokio::time::timeout(Duration::from_millis(500), stream.next()).await
        {
            buf.push_str(&String::from_utf8_lossy(&chunk));
        }
    }
    assert!(buf.contains("\"text\":\"live\""), "{buf}");
    assert!(buf.contains("from node"), "tail-follow: {buf}");
    assert!(buf.contains(": ping\n\n"));
    // stream limit
    let _second = h
        .http
        .get(h.url("/timeline/stream"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    let third = h
        .http
        .get(h.url("/timeline/stream"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(third.status().as_u16(), 429);
    let b: Value = third.json().await.unwrap();
    assert_eq!(b["error"], "too_many_streams");
    let bad = h
        .http
        .get(h.url("/timeline/stream"))
        .bearer_auth(TOKEN)
        .header("last-event-id", "x")
        .send()
        .await
        .unwrap();
    assert!(bad.status().as_u16() == 400 || bad.status().as_u16() == 429);
}

/// Read SSE text until `done(buf)` or 5 s.
async fn read_sse<B: AsRef<[u8]>>(
    stream: &mut (impl futures_util::Stream<Item = reqwest::Result<B>> + Unpin),
    buf: &mut String,
    done: impl Fn(&str) -> bool,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !done(buf) && tokio::time::Instant::now() < deadline {
        if let Ok(Some(Ok(chunk))) =
            tokio::time::timeout(Duration::from_millis(300), stream.next()).await
        {
            buf.push_str(&String::from_utf8_lossy(chunk.as_ref()));
        }
    }
}

/// T1 (docs/verification.md): Node took N2's id, Rust then wrote R (a newer
/// id), the app saw R and reconnects with `Last-Event-ID: R`, and only then
/// does N2 land in the file. Resuming by id skipped N2 forever; by arrival it
/// is replayed (already landed) or sent live (lands later), exactly once.
#[tokio::test]
async fn resume_by_last_event_id_replays_a_late_line() {
    let h = harness(chat_script("ok"), |_| {}).await;
    let path = h.dir.path().join("timeline.jsonl");
    let late_line = |text: &str| {
        lucina_store::timeline::normalize(
            lucina_store::NewItem {
                channel: "telegram".into(),
                role: "user".into(),
                text: text.into(),
                ..Default::default()
            },
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap()
    };
    let write_line = |item: &lucina_store::timeline::TimelineItem| {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .unwrap();
        writeln!(f, "{}", serde_json::to_string(item).unwrap()).unwrap();
    };
    let n2 = late_line("late node line");
    let n3 = late_line("later node line");
    tokio::time::sleep(Duration::from_millis(5)).await;
    h.chat("rust line").await;
    let (_, b) = h.get("/timeline?limit=1").await;
    let r = b["data"]["items"][0]["id"].as_str().unwrap().to_owned();
    assert!(n2.id < r && n3.id < r, "the late lines have smaller ids");
    // N2 lands, then the app resumes from R: N2 comes in the replay.
    write_line(&n2);
    let resp = h
        .http
        .get(h.url("/timeline/stream"))
        .bearer_auth(TOKEN)
        .header("last-event-id", &r)
        .send()
        .await
        .unwrap();
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    read_sse(&mut stream, &mut buf, |b| b.contains("late node line")).await;
    assert!(buf.contains(&format!("id: {}\n", n2.id)), "replayed: {buf}");
    // N3 lands while the stream is open: the follow tick sends it (its id is
    // smaller than the cursor's), once.
    write_line(&n3);
    read_sse(&mut stream, &mut buf, |b| b.contains("later node line")).await;
    // Two more follow ticks: nothing is sent again.
    let until = tokio::time::Instant::now() + Duration::from_millis(2500);
    while tokio::time::Instant::now() < until {
        if let Ok(Some(Ok(chunk))) =
            tokio::time::timeout(Duration::from_millis(200), stream.next()).await
        {
            buf.push_str(&String::from_utf8_lossy(chunk.as_ref()));
        }
    }
    assert_eq!(buf.matches(&format!("id: {}\n", n3.id)).count(), 1, "{buf}");
    assert_eq!(buf.matches(&format!("id: {}\n", n2.id)).count(), 1, "{buf}");
    assert_eq!(buf.matches(&format!("id: {r}\n")).count(), 0, "{buf}");
    // A fresh stream (no id) starts after the line that arrived last (N3),
    // not after the newest id (R).
    drop(stream);
    let resp = h
        .http
        .get(h.url("/timeline/stream"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    read_sse(&mut stream, &mut buf, |b| b.contains(": connected")).await;
    let n4 = late_line("fresh-stream line");
    write_line(&n4);
    read_sse(&mut stream, &mut buf, |b| b.contains("fresh-stream line")).await;
    assert!(buf.contains("fresh-stream line"), "{buf}");
    assert!(!buf.contains("late node line"), "{buf}");
}

#[tokio::test]
async fn device_bridge_routes() {
    let h = harness(chat_script("ok"), |_| {}).await;
    let ts = lucina_store::timeutil::now_iso();
    let (s, b) = h.post("/device/events", json!({"events": [
        {"id": "e1", "ts": ts, "type": "location_visit", "source": "ios", "data": {"name": "Gym"}},
        {"id": "x", "ts": ts, "type": "nope", "source": "ios"}
    ]})).await;
    assert_eq!(s, 200);
    assert_eq!(b["data"]["accepted"], json!(["e1"]));
    assert_eq!(
        b["data"]["rejected"][0],
        json!({"index": 1, "id": "x", "error": "invalid_type"})
    );
    assert_eq!(
        h.post("/device/events", json!({"events": []})).await.1["error"],
        "bad_request"
    );
    let (s, b) = h
        .post(
            "/device/state",
            json!({"alarms": [{"id": "A1", "time": "7:00"}], "focus": {"mode": "Work"}}),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(
        b["data"]["counts"],
        json!({"alarms": 1, "timers": 0, "reminders": 0, "events": 0})
    );
    assert_eq!(
        h.get("/device/state").await.1["data"]["state"]["alarms"][0]["time"],
        "07:00"
    );
    assert_eq!(
        h.get("/device/commands").await.1["data"]["commands"],
        json!([])
    );
    let c = h
        .state
        .device
        .create_command(
            "alarm.delete",
            json!({"id": "A1", "time": "07:00"}),
            "ios",
            "en",
            chrono::Utc::now(),
        )
        .unwrap();
    let id = c["id"].as_str().unwrap();
    assert_eq!(
        h.get("/device/commands").await.1["data"]["commands"][0]["requiresConfirmation"],
        true
    );
    let (s, b) = h
        .post(
            &format!("/device/commands/{id}/result"),
            json!({"ok": true, "deviceObjectId": "A1"}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["duplicate"], false);
    assert_eq!(b["data"]["command"]["status"], "done");
    let confirmation = b["data"]["confirmationItemId"].as_str().unwrap().to_owned();
    let line = h
        .timeline_lines()
        .into_iter()
        .find(|l| l["id"] == json!(confirmation))
        .unwrap();
    assert_eq!(line["text"], "Done: your 07:00 alarm is deleted.");
    assert_eq!(
        line["meta"],
        json!({"kind": "device_command_result", "commandId": id, "ok": true})
    );
    assert_eq!(
        h.post(
            &format!("/device/commands/{id}/result"),
            json!({"ok": true})
        )
        .await
        .1["data"]["duplicate"],
        true
    );
    assert_eq!(
        h.post(
            &format!("/device/commands/{id}/result"),
            json!({"ok": false})
        )
        .await
        .1["error"],
        "already_resolved"
    );
    assert_eq!(
        h.post(
            &format!("/device/commands/{id}/result"),
            json!({"ok": "yes"})
        )
        .await
        .1["error"],
        "bad_request"
    );
    assert_eq!(
        h.get("/device/commands/00000000-0000-0000-0000-000000000000")
            .await
            .1["error"],
        "command_not_found"
    );
    assert_eq!(h.get("/device/x").await.1["error"], "not_found");
}

#[tokio::test]
async fn shopping_routes() {
    let h = harness(chat_script("ok"), |_| {}).await;
    if h.state.grocery.is_none() {
        assert_eq!(h.get("/shopping").await.1["error"], "grocery_unavailable");
        assert_eq!(
            h.get("/shopping/lists").await.1["error"],
            "grocery_unavailable"
        );
        let (s, _, hd) = h.call(Method::PUT, "/shopping", None).await;
        assert_eq!((s, hd["allow"].to_str().unwrap()), (405, "GET, POST"));
        return eprintln!("LUCINA_TEST_DATABASE_URL unset: only the unavailable path was tested");
    }
    let user = format!("api-{}", uuid::Uuid::new_v4());
    let h = harness(chat_script("ok"), move |c| c.grocery_user_id = user).await;
    let (s, b) = h
        .post("/shopping", json!({"rawName": "latte", "channel": "ios"}))
        .await;
    assert_eq!(s, 201, "{b}");
    let item = b["data"]["item"].clone();
    // Item objects (Node 15d9629): `product` is the object's name in the
    // owner's language (default it), `itemId` the catalog id.
    assert_eq!(
        (
            item["product"].as_str(),
            item["itemId"].as_str(),
            item["status"].as_str()
        ),
        (Some("Latte"), Some("milk"), Some("needed"))
    );
    let id = item["id"].as_str().unwrap().to_owned();
    let (s, b, _) = h
        .call(
            Method::PATCH,
            &format!("/shopping/{id}"),
            Some(json!({"status": "purchased"})),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(
        b["data"]["item"],
        json!({"id": id, "product": "Latte", "name": "Latte", "itemId": "milk", "status": "purchased"})
    );
    assert_eq!(
        h.call(
            Method::PATCH,
            &format!("/shopping/{id}"),
            Some(json!({"status": "eaten"}))
        )
        .await
        .1["error"],
        "invalid_status"
    );
    assert_eq!(
        h.call(
            Method::PATCH,
            "/shopping/zz",
            Some(json!({"status": "needed"}))
        )
        .await
        .1["error"],
        "item_not_found"
    );
    assert_eq!(
        h.get("/shopping").await.1["data"]["items"][0]["status"],
        "purchased"
    );
    let (_, lists) = h.get("/shopping/lists").await;
    assert_eq!(lists["data"]["lists"][0]["isDefault"], true);
    let (s, b) = h.post("/shopping/lists/default/ops", json!({"expectedRevision": 0, "ops": [{"opId": uuid::Uuid::new_v4().to_string(), "type": "need", "itemId": id}]})).await;
    assert_eq!((s, b["error"].as_str()), (409, Some("revision_conflict")));
    assert!(b["revision"].is_number() && b["conflicts"].is_array());
    let (s, b) = h.post("/shopping/lists/default/ops", json!({"ops": [{"opId": uuid::Uuid::new_v4().to_string(), "type": "need", "itemId": id}], "channel": "ios"})).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["items"][0]["updatedBy"], "ios");
    assert_eq!(
        h.post(
            "/shopping/lists/default/ops",
            json!({"expectedRevision": 1.5, "ops": []})
        )
        .await
        .1["error"],
        "bad_request"
    );
    assert_eq!(
        h.get("/shopping/lists/not-a-list!").await.1["error"],
        "list_not_found"
    );
    let (_, b, _) = h
        .call(Method::DELETE, "/shopping/lists/default", None)
        .await;
    assert_eq!(b["error"], "default_list");
    // A sentence is one row per item, never one row (docs/design/shopping-labels.md).
    let (s, many) = h
        .post("/shopping", json!({"rawName": "aggiungi alla lista uova e 2 kg di farina", "channel": "ios"}))
        .await;
    assert_eq!(s, 201, "{many}");
    let rows = many["data"]["items"].as_array().unwrap();
    let typed: Vec<&str> = rows.iter().map(|r| r["rawName"].as_str().unwrap_or("")).collect();
    assert_eq!(typed, ["uova", "farina"], "{many}");
    assert_eq!(many["data"]["item"]["id"], rows[0]["id"]);
}

#[tokio::test]
async fn scheduler_health_reports_failures_without_failing_health() {
    let h = harness(chat_script("x"), |c| c.scheduler = true).await;
    let tick_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    {
        let mut health = h.state.scheduler_health.lock().unwrap();
        // A recent tick: a stalled scheduler (no tick for 10 polls) is a liveness failure.
        health.last_tick_at = Some(tick_at.clone());
        health.last_error = Some("delivery uncertain; not resent: read timeout".into());
        health.delivered = 2;
        health.not_delivered = 3;
        health.uncertain = 1;
    }
    let response = h
        .http
        .get(format!("{}/health", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body["scheduler"],
        json!({"enabled":true,"lastTickAt":tick_at,"lastError":"delivery uncertain; not resent: read timeout","delivered":2,"notDelivered":3,"uncertain":1})
    );
}

#[tokio::test]
async fn device_chat_and_conversation_sync_contract() {
    use lucina_store::timeline::{Meta, NewItem};
    for planner in [PlannerMode::Json, PlannerMode::Tools] {
        let fake = if planner == PlannerMode::Tools {
            script(json!({"rules": [{"kind": "agent", "content": "hello"}]}))
        } else {
            chat_script("hello")
        };
        let h = harness(fake, |c| c.planner = planner).await;
        let (status, _, _) = h.call(Method::POST, "/chat", Some(json!({
            "message": "hi", "device": "phone-A", "deviceName": " My phone ", "channel": "ios", "source": "ios-chat"
        }))).await;
        assert_eq!(status, 200);
        let lines = h.timeline_lines();
        assert_eq!(lines.len(), 2);
        for item in &lines {
            assert_eq!(item["meta"]["device"], "phone-A");
            assert_eq!(item["meta"]["deviceName"], "My phone");
            // Who asked, on his item and her reply (secretary-server's notifier routes by it).
            assert_eq!(item["meta"]["source"], "ios-chat");
            assert_eq!(item["meta"]["fromDevice"], "phone-A");
        }
        assert_eq!(lines[1]["replyTo"], lines[0]["id"]);
        let (status, b) = h
            .post(
                "/chat",
                json!({
                    "message": "[via Lucina MCP, from grok] what do you think?",
                    "sessionId": "mcp-grok", "device": "desktop", "deviceName": "Desktop"
                }),
            )
            .await;
        assert_eq!(status, 200, "{b}");
        for item in h.timeline_lines().iter().skip(2) {
            assert_eq!(item["meta"]["device"], "desktop");
            assert_eq!(item["meta"]["deviceName"], "Desktop");
            assert_eq!(item["meta"]["via"], "mcp");
            assert_eq!(item["meta"]["caller"], "grok");
        }
    }
    let h = harness(chat_script("hello"), |_| {}).await;
    for bad in [
        "scope=unknown",
        "scope=mine",
        "scope=mine&device=A",
        "scope=mine&channel=ios",
        "scope=mine&device=A&channel=other",
        "scope=mine&device=bad.id&channel=web",
        "conversation=device:A&scope=mine&device=A&channel=web",
        "conversation=Device:A",
        "conversation=device:",
        "conversation=device:bad.id",
        "conversation=a:b:c",
        "conversation=",
        "after=bad",
        "before=01K83ZK4Q6X1V9W2D3E4F5G6H7&after=01K83ZK4Q6X1V9W2D3E4F5G6H7",
    ] {
        assert_eq!(h.get(&format!("/timeline?{bad}")).await.0, 400, "{bad}");
    }
    let (_, empty) = h.get("/timeline").await;
    assert!(empty["data"]["cursor"].is_null());
    assert_eq!(empty["data"]["hasMore"], false);
    let mut ids = Vec::new();
    for (channel, device, text) in [
        ("ios", Some("A"), "a"),
        ("web", Some("B"), "b"),
        ("ios", None, "legacy"),
        ("other", None, "lucina"),
    ] {
        let i = h
            .state
            .timeline
            .append(NewItem {
                channel: channel.into(),
                role: "user".into(),
                text: text.into(),
                meta: device.map(|d| Meta {
                    device: Some(d.into()),
                    ..Meta::default()
                }),
                ..NewItem::default()
            })
            .unwrap();
        ids.push(i.id);
    }
    let late = "00000000000000000000000001";
    h.state
        .timeline
        .append(NewItem {
            id: Some(late.into()),
            channel: "ios".into(),
            role: "lucina".into(),
            text: "late".into(),
            meta: Some(Meta {
                device: Some("A".into()),
                ..Meta::default()
            }),
            ..NewItem::default()
        })
        .unwrap();
    let (_, b) = h
        .get("/timeline?scope=mine&device=A&channel=ios&limit=2")
        .await;
    assert_eq!(b["data"]["items"][0]["text"], "lucina");
    assert_eq!(b["data"]["items"][1]["text"], "legacy");
    assert_eq!(b["data"]["cursor"], late);
    let next = b["data"]["nextBefore"].as_str().unwrap();
    let (_, b) = h
        .get(&format!(
            "/timeline?scope=mine&device=A&channel=ios&before={next}"
        ))
        .await;
    assert_eq!(b["data"]["items"].as_array().unwrap().len(), 2);
    assert!(b["data"]["nextBefore"].is_null());
    let (_, b) = h.get("/timeline?conversation=device:A").await;
    assert_eq!(b["data"]["items"].as_array().unwrap().len(), 2);
    let (_, b) = h
        .get(&format!(
            "/timeline?after={}&scope=mine&device=A&channel=ios&limit=1",
            ids[0]
        ))
        .await;
    assert_eq!(b["data"]["items"][0]["text"], "legacy");
    assert_eq!(b["data"]["cursor"], ids[2]);
    assert_eq!(b["data"]["hasMore"], true);
    let (_, b) = h.get(&format!("/timeline?after={}&limit=1", ids[2])).await;
    assert_eq!(b["data"]["cursor"], ids[3]);
    assert_eq!(b["data"]["hasMore"], true);
    let (_, b) = h.get(&format!("/timeline?after={}&limit=1", ids[3])).await;
    assert_eq!(b["data"]["items"][0]["text"], "late");
    assert_eq!(b["data"]["cursor"], late);
    assert_eq!(b["data"]["hasMore"], false);
    let (status, b) = h.get("/timeline?after=00000000000000000000000002").await;
    assert_eq!(status, 410);
    assert_eq!(b["error"], "cursor_expired");
    let (status, b, headers) = h.call(Method::GET, "/timeline/conversations", None).await;
    assert_eq!(status, 200);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(b["ok"], true);
    let items = b["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 4);
    let device = items.iter().find(|i| i["key"] == "device:A").unwrap();
    assert_eq!(device["kind"], "device");
    assert_eq!(device["title"], "A");
    assert_eq!(device["count"], 2);
    assert_eq!(device["lastId"], ids[0]);
}

#[tokio::test]
async fn device_context_reaches_later_job_timeline_item() {
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"unknown\",\"confidence\":0.2,\"reply_mode\":\"full_plan\"}"},
            {"kind": "plan", "content": "{\"action\":\"generate_image\",\"prompt\":\"a cat\"}"}
        ]})),
        |c| c.image_jobs = true,
    ).await;
    let (status, b) = h
        .post(
            "/chat",
            json!({
                "message": "draw a cat", "device": "phone", "deviceName": "Phone", "channel": "ios"
            }),
        )
        .await;
    assert_eq!(status, 200, "{b}");
    assert!(b["data"]["job"]["id"].is_string(), "{b}");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if h.timeline_lines().len() >= 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let lines = h.timeline_lines();
    assert_eq!(lines.len(), 3);
    for item in lines {
        assert_eq!(item["meta"]["device"], "phone");
        assert_eq!(item["meta"]["deviceName"], "Phone");
    }
}
