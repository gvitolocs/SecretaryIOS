//! A plain report ("gli emoji sono quadratini", no "flaw:") on the real chat path, routed by a
//! fake Jev. Its own test binary: everyday turns use the process-wide Jev client, which reads
//! its settings from the environment once.

mod common;

use common::{harness, script};
use lucina_server::Config;
use serde_json::{json, Value};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

struct JevByMessage;

impl Respond for JevByMessage {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let (intent, p) = match body["state"]["message"].as_str().unwrap_or("") {
            "gli emoji sono quadratini" => ("flaw_report", 0.92),
            "le icone sono storte" => ("flaw_report", 0.61),
            "le icone forse" => ("flaw_report", 0.4),
            _ => ("chat", 0.95),
        };
        let other = if intent == "chat" { "flaw_report" } else { "chat" };
        ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.14.0",
            "usage": {"input_tokens": 100, "output_tokens": 10},
            "answers": {
                "intent": {"type": "choice", "choice": intent, "confidence": p, "probabilities": {intent: p, other: 1.0 - p}},
                "scope": {"type": "choice", "choice": "all", "confidence": 0.9, "probabilities": {"all": 0.9, "today": 0.1}},
                "needs_memory": {"type": "noul", "noul": 0.1},
                "needs_tools": {"type": "noul", "noul": 0.1},
            }
        }))
    }
}

#[tokio::test]
async fn jev_routes_a_plain_report_into_the_tracker() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any()).respond_with(JevByMessage).mount(&server).await;
    std::env::set_var("JEV_API_KEY", "test-key");
    std::env::set_var("LUCINA_JEV", "on");
    std::env::set_var("LUCINA_JEV_URL", format!("{}/v1/systemone", server.uri()));
    assert!(lucina_jev::shared().available(), "the first use of the shared client in this binary");

    let rules = json!({"rules": [
        {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
        {"kind": "reply", "content": "Oh, mi dispiace."},
    ]});
    let h = harness(script(rules), |c: &mut Config| {
        c.flaws.policy.auto_dispatch = false;
        c.flaws.policy.auto_triage = false;
    })
    .await;

    // The owner's sentence, no keyword: it lands in the tracker in his words.
    let d = h.chat("gli emoji sono quadratini").await;
    assert!(d["reply"].as_str().unwrap().starts_with("Segnato (fl-"), "{d}");
    let (_, b) = h.get("/flaws").await;
    assert_eq!(b["data"]["items"][0]["words"], "gli emoji sono quadratini");
    assert_eq!(b["data"]["items"].as_array().unwrap().len(), 1);

    // Facts only in what Jev saw: the one template, nothing guessed.
    let seen = server.received_requests().await.unwrap();
    let last: Value = serde_json::from_slice(&seen.last().unwrap().body).unwrap();
    let keys: Vec<String> = last["state"].as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["message", "platform", "recent_turns", "user"], "{}", last["state"]);

    // A sure answer about something else stays a chat, and so does a route below the bar.
    let d = h.chat("che tempo fa domani").await;
    assert_eq!(d["reply"], "Oh, mi dispiace.");
    let d = h.chat("le icone forse").await;
    assert!(!d["reply"].as_str().unwrap().contains("fl-"), "{d}");
    let (_, b) = h.get("/flaws").await;
    assert_eq!(b["data"]["items"].as_array().unwrap().len(), 1, "{b}");
}
