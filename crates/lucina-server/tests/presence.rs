//! `GET /api/secretary/presence`, the `presence` tool and the context line,
//! with presence files written by the real watcher engine.

mod common;

use std::time::Duration;

use chrono::Utc;
use common::{harness, script};
use lucina_discord::engine::Engine;
use lucina_discord::store::{day_start, PresenceStore};
use lucina_server::PlannerMode;
use reqwest::Method;
use serde_json::json;

const OWNER: &str = "649020113026023475";

/// Owner online on desktop + Spotify since 30 minutes ago; someone else in
/// the same guild with a game.
fn seed(data_dir: &std::path::Path) {
    let start = Utc::now() - chrono::Duration::minutes(30);
    let mut e = Engine::start(
        OWNER,
        PresenceStore::for_data_dir(data_dir, 90),
        start,
        Duration::from_secs(600),
        Duration::from_secs(60),
    )
    .unwrap();
    let frame = |user: &str, acts: serde_json::Value| {
        json!({"op": 0, "t": "PRESENCE_UPDATE", "d": {"user": {"id": user}, "guild_id": "1", "status": "online",
              "client_status": {"desktop": "online"}, "activities": acts}})
        .to_string()
    };
    e.handle_text(
        &frame(
            OWNER,
            json!([{"type": 2, "name": "Spotify", "details": "Song X", "state": "Band Y"}]),
        ),
        start,
    );
    e.handle_text(
        &frame("42", json!([{"type": 0, "name": "SomeoneElsesGame"}])),
        start,
    );
    e.tick(Utc::now());
}

#[tokio::test]
async fn presence_route() {
    let h = harness(script(json!({"rules": []})), |_| {}).await;
    let (s, b) = h.get("/presence").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["available"], false);
    assert_eq!(b["data"]["fresh"], false);
    assert_eq!(
        b["data"]["summary"],
        "Discord: presence unknown (watcher not running)"
    );

    seed(h.dir.path());
    let (s, b) = h.get("/presence").await;
    assert_eq!(s, 200, "{b}");
    let d = &b["data"];
    assert_eq!(d["available"], true);
    assert_eq!(d["fresh"], true);
    assert_eq!(d["status"], "online");
    assert_eq!(
        d["clientStatus"],
        json!({"desktop": "online", "mobile": null, "web": null})
    );
    assert_eq!(
        d["activities"][0],
        json!({"type": "listening", "name": "Spotify", "details": "Song X", "state": "Band Y", "startedAt": null})
    );
    assert_eq!(d["timeZone"], "Europe/Rome");
    let now = Utc::now();
    let from =
        (now - chrono::Duration::minutes(30)).max(day_start(now, "Europe/Rome".parse().unwrap()));
    let expected = (now - from).num_minutes();
    let got = d["todayOnlineMinutes"].as_i64().unwrap();
    assert!((got - expected).abs() <= 1, "{got} vs {expected}");
    assert!(d["summary"]
        .as_str()
        .unwrap()
        .starts_with("Discord: online on desktop since "));
    assert!(!b.to_string().contains("SomeoneElsesGame"));

    let (s, b, headers) = h.call(Method::POST, "/presence", Some(json!({}))).await;
    assert_eq!(s, 405, "{b}");
    assert_eq!(headers["allow"], "GET");
    let unauth = h.http.get(h.url("/presence")).send().await.unwrap();
    assert_eq!(unauth.status().as_u16(), 401);
    let wrong = h
        .http
        .get(h.url("/presence"))
        .bearer_auth("nope")
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status().as_u16(), 401);
}

#[tokio::test]
async fn presence_tool_and_context_line() {
    let h = harness(
        script(json!({"rules": [
            {"kind": "agent", "contains": "online", "afterTool": false, "toolCalls": [{"name": "presence", "arguments": {"history": true}}]},
            {"kind": "agent", "contains": "online", "afterTool": true, "content": "Sì, sei online dal desktop e ascolti Song X."}
        ]})),
        |c| c.planner = PlannerMode::Tools,
    )
    .await;
    seed(h.dir.path());
    let d = h.chat("sono online su discord?").await;
    assert_eq!(d["reply"], "Sì, sei online dal desktop e ascolti Song X.");
    let reqs = h.fake.requests();
    let first = reqs[0].to_string();
    assert!(
        first.contains("Discord: online on desktop since "),
        "context line missing: {first}"
    );
    assert!(first.contains("listening to \\\"Song X\\\" by Band Y on Spotify"));
    let tool_msg = reqs[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .to_owned();
    let out: serde_json::Value = serde_json::from_str(&tool_msg).unwrap();
    assert_eq!(out["status"], "online");
    assert_eq!(out["fresh"], true);
    assert!(out["todayOnlineMinutes"].as_i64().is_some());
    let hist = out["todayHistory"].as_array().unwrap();
    assert!(hist.iter().any(|e| e["event"] == "activity_start"));
    assert!(!tool_msg.contains("SomeoneElsesGame"));
    let lines = h.timeline_lines();
    assert_eq!(lines[1]["meta"]["tool"], "presence");
}

#[tokio::test]
async fn context_line_can_be_turned_off() {
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
            {"kind": "reply", "content": "ok"}
        ]})),
        |c| c.presence_context = false,
    )
    .await;
    seed(h.dir.path());
    h.chat("ciao").await;
    let all = serde_json::to_string(&h.fake.requests()).unwrap();
    assert!(!all.contains("Discord: online"));
}
