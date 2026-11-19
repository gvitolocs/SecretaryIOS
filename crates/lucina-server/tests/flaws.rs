//! The flaw tracker over `/api/secretary/flaws/*` and on a real chat turn (crate::flaws,
//! chat::flaw_turn). Starting tasks is covered in `src/flaws.rs` with fake hands.

mod common;

use common::{harness, script};
use lucina_server::Config;
use serde_json::{json, Value};

fn no_loop(c: &mut Config) {
    c.flaws.policy.auto_dispatch = false;
    c.flaws.policy.auto_triage = false;
}

async fn plain() -> common::H {
    harness(script(json!({"rules": []})), no_loop).await
}

#[tokio::test]
async fn report_read_matrix_and_the_evidence_rule_over_http() {
    let h = plain().await;
    let (s, b) = h.post("/flaws", json!({"words": "gli emoji sono quadratini", "affected": ["juno"], "severity": "medium"})).await;
    assert_eq!(s, 201, "{b}");
    let id = b["data"]["flaw"]["id"].as_str().unwrap().to_owned();
    assert_eq!(b["data"]["flaw"]["words"], "gli emoji sono quadratini");

    let (s, b) = h.get(&format!("/flaws/{id}")).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["targets"].as_array().unwrap().len(), 5);
    assert_eq!(b["data"]["events"][0]["kind"], "reported");

    let (s, b) = h.get("/flaws/matrix").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["rows"][0]["cells"]["juno"]["status"], "affected");

    let (s, b) = h.get("/flaws?q=quadratini&platform=juno&status=affected").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["items"].as_array().unwrap().len(), 1);

    // Closing without evidence, or on reading code, is refused and changes nothing.
    let url = format!("/flaws/{id}/targets/web");
    let (s, b, _) = h.call(reqwest::Method::PUT, &url, Some(json!({"status": "not_affected"}))).await;
    assert_eq!((s, b["error"].as_str()), (422, Some("evidence_required")), "{b}");
    let read = json!({"status": "not_affected", "evidence": {"kind": "code_reading", "reference": "rich.rs:1"}});
    let (s, _, _) = h.call(reqwest::Method::PUT, &url, Some(read)).await;
    assert_eq!(s, 422);
    let run = json!({"status": "not_affected", "evidence": {"kind": "harness_run", "reference": "logs/1.txt"}});
    let (s, b, _) = h.call(reqwest::Method::PUT, &url, Some(run)).await;
    assert_eq!(s, 422, "a harness run must name its build: {b}");
    let run = json!({"status": "not_affected", "evidence": {"kind": "harness_run", "reference": "logs/1.txt", "build": "deploy/web/x"}});
    let (s, b, _) = h.call(reqwest::Method::PUT, &url, Some(run)).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["status"], "not_affected");

    // A bad platform, id or transition.
    let (s, _, _) = h.call(reqwest::Method::PUT, &format!("/flaws/{id}/targets/nokia"), Some(json!({}))).await;
    assert_eq!(s, 400);
    assert_eq!(h.get("/flaws/NOPE").await.0, 404);
    let (s, b, _) = h.call(reqwest::Method::PUT, &format!("/flaws/{id}/targets/ios"), Some(json!({"status": "verified", "evidence": {"kind": "screenshot", "reference": "a.png"}}))).await;
    assert_eq!((s, b["error"].as_str()), (409, Some("bad_transition")), "{b}");
    // Dispatch needs a layer.
    let (s, b) = h.post(&format!("/flaws/{id}/dispatch"), json!({"platform": "juno"})).await;
    assert_eq!((s, b["error"].as_str()), (409, Some("not_claimable")), "{b}");
}

#[tokio::test]
async fn the_reporter_comes_from_the_caller_header_not_the_body() {
    let h = plain().await;
    let r = h
        .http
        .post(h.url("/flaws"))
        .bearer_auth(common::TOKEN)
        .header("x-lucina-caller", "mcp:claude")
        .json(&json!({"words": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), 201);
    let b: Value = r.json().await.unwrap();
    assert_eq!(b["data"]["flaw"]["reporter"], "mcp:claude");
    let (s, _) = h.post("/flaws", json!({"words": "x", "reporter": "someone"})).await;
    assert_eq!(s, 400, "unknown fields are refused");
}

const INTENT_CHAT: &str = "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}";

fn chatty() -> lucina_server::fake::Script {
    script(json!({"rules": [
        {"kind": "intent", "content": INTENT_CHAT},
        {"kind": "reply", "content": "Oh, mi dispiace."},
    ]}))
}

#[tokio::test]
async fn a_plain_report_without_a_route_is_only_a_chat() {
    // What happens when nothing routes it (Jev out): she chats and no flaw is recorded.
    let h = harness(chatty(), no_loop).await;
    let d = h.chat("gli emoji sono quadratini").await;
    assert_eq!(d["reply"], "Oh, mi dispiace.");
    let (_, b) = h.get("/flaws").await;
    assert!(b["data"]["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn an_explicit_report_needs_no_model() {
    let h = harness(chatty(), no_loop).await;
    let d = h.chat("flaw: gli a capo si perdono su Juno").await;
    let reply = d["reply"].as_str().unwrap();
    assert!(reply.starts_with("Noted (fl-"), "{reply}");
    assert_eq!(d["action"], "flaw_report");
    let (_, b) = h.get("/flaws").await;
    let f = &b["data"]["items"][0];
    assert_eq!(f["words"], "flaw: gli a capo si perdono su Juno");
    assert_eq!(f["reporter"], "owner");
    let (_, b) = h.get("/flaws/matrix").await;
    assert_eq!(b["data"]["rows"][0]["cells"]["juno"]["status"], "affected");
    assert_eq!(b["data"]["rows"][0]["cells"]["web"]["status"], "untriaged");
}

#[tokio::test]
async fn telling_claude_stays_a_message() {
    let h = harness(chatty(), no_loop).await;
    h.chat("tell Claude that emoji are boxes").await;
    let (_, b) = h.get("/flaws").await;
    assert!(b["data"]["items"].as_array().unwrap().is_empty(), "the chat turn did not take it: {b}");
}

#[tokio::test]
async fn off_means_off() {
    let h = harness(chatty(), |c: &mut Config| c.flaws.enabled = false).await;
    assert_eq!(h.get("/flaws").await.0, 404);
    let d = h.chat("flaw: x").await;
    assert_ne!(d["action"], "flaw_report");
}

#[tokio::test]
async fn the_lane_breakers_are_listed_and_cleared_over_http() {
    // LA-C035: a lead closes a lane by hand. Nothing has failed here, so the list is empty and a
    // clear says so; GET and POST are the only methods.
    let h = plain().await;
    let (s, b) = h.get("/flaws/lanes").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["lanes"].as_array().unwrap().len(), 0, "{b}");
    let (s, b) = h.post("/flaws/lanes/claude-sonnet/clear", json!({})).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["cleared"], false, "{b}");
    assert_eq!(b["data"]["lane"], "claude-sonnet");
    let (s, _, _) = h
        .call(reqwest::Method::PUT, "/flaws/lanes", Some(json!({})))
        .await;
    assert_eq!(s, 405);
}
