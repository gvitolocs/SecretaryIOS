//! Parcel tracking on a real chat turn (chat::parcel_turn): a message Jev routes to `parcel_track`
//! is answered without a model call, against a stub of the local parcel scraper. Its own test
//! binary: everyday turns use the process-wide Jev client, which reads its settings once.

mod common;

use common::{harness, script, H};
use lucina_server::Config;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const NUMBER: &str = "3UW1S8IAAKA23";

struct JevByMessage;

impl Respond for JevByMessage {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let m = body["state"]["message"]
            .as_str()
            .unwrap_or("")
            .to_lowercase();
        let (intent, p) = if m.contains("track") || m.contains("parcels") {
            ("parcel_track", 0.92)
        } else {
            ("chat", 0.95)
        };
        let other = if intent == "chat" {
            "parcel_track"
        } else {
            "chat"
        };
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

struct DetectByText;

impl Respond for DetectByText {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let found = body["text"]
            .as_str()
            .unwrap_or("")
            .to_uppercase()
            .contains(NUMBER);
        ResponseTemplate::new(200).set_body_json(if found {
            json!({"trackingNumber": NUMBER, "source": "number", "carrier": "poste-italiane", "confidence": "high", "candidates": ["poste-italiane"], "preferred": []})
        } else {
            json!({"trackingNumber": null, "source": "none", "carrier": null, "confidence": "none", "candidates": [], "preferred": []})
        })
    }
}

async fn scraper_stub() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/detect"))
        .respond_with(DetectByText)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/track"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "carrier": "poste-italiane", "source": "poste-italiane", "attempts": [],
            "result": {"current_stage": "in_transit", "last_status_text": "Arrived at the sorting centre", "last_update": "2026-09-28T08:00:00Z",
                       "events": [{"time": "2026-09-28T08:00:00Z", "description": "Arrived", "location": "Milano", "stage": "in_transit", "instant": "2026-09-28T08:00:00Z"}]}
        })))
        .mount(&server)
        .await;
    server
}

fn chatty() -> lucina_server::fake::Script {
    script(json!({"rules": [
        {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
        {"kind": "reply", "content": "Oh, ok."},
    ]}))
}

async fn with_scraper(url: String) -> H {
    harness(chatty(), move |c: &mut Config| {
        c.parcels.enabled = true;
        c.parcels.scraper_url = url;
    })
    .await
}

fn stored(h: &H) -> Vec<lucina_parcels::Parcel> {
    h.state
        .tool_env
        .parcels
        .as_ref()
        .map(|s| s.store.list())
        .unwrap_or_default()
}

async fn chat_as(h: &H, session: &str, message: &str) -> Value {
    let (s, b) = h
        .post(
            "/chat",
            json!({"message": message, "sessionId": session, "channel": "ios"}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    b["data"].clone()
}

#[test]
fn the_intent_is_one_jev_can_pick_and_lucina_knows() {
    let (_, text) = lucina_server::jev_route::INTENT_OPTIONS
        .iter()
        .find(|(n, _)| *n == "parcel_track")
        .expect("in INTENT_OPTIONS");
    assert!(text.contains("tracking number"));
    assert!(lucina_intent::tier0::INTENTS.contains(&"parcel_track"));
    assert!(lucina_server::jev_route::SIDE_EFFECT_INTENTS.contains(&"parcel_track"));
    assert_eq!(
        lucina_intent::tier0::default_reply_mode("parcel_track"),
        "chat"
    );
    // No regex rule: only the router sends a message here.
    let rule = lucina_intent::tier0::classify_tier0(
        "track this 3UW1S8IAAKA23",
        "",
        &lucina_intent::heuristics::Settings::default(),
    );
    assert!(rule.is_none_or(|c| c.intent != "parcel_track"));
}

#[tokio::test]
async fn jev_routes_parcel_turns() {
    let jev = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(JevByMessage)
        .mount(&jev)
        .await;
    std::env::set_var("JEV_API_KEY", "test-key");
    std::env::set_var("LUCINA_JEV", "on");
    std::env::set_var("LUCINA_JEV_URL", format!("{}/v1/systemone", jev.uri()));
    assert!(
        lucina_jev::shared().available(),
        "the first use of the shared client in this binary"
    );
    let scraper = scraper_stub().await;

    // A number: carrier detected, saved, tracked once, answered with the status.
    let h = with_scraper(scraper.uri()).await;
    let d = chat_as(&h, "test", &format!("track this {NUMBER}")).await;
    let reply = d["reply"].as_str().unwrap();
    assert!(
        reply.starts_with(&format!(
            "Tracking Poste Italiane {NUMBER}: Arrived at the sorting centre"
        )),
        "{reply}"
    );
    assert!(
        reply.contains("every hour once it's close to you"),
        "{reply}"
    );
    assert_eq!(d["action"], "parcel_track");
    let parcels = stored(&h);
    assert_eq!(parcels.len(), 1);
    assert_eq!(
        (parcels[0].number.as_str(), parcels[0].carrier.as_deref()),
        (NUMBER, Some("poste-italiane"))
    );
    assert!(parcels[0].status.is_some());

    // The same number again is the same parcel.
    let d = chat_as(&h, "test", &format!("track this {NUMBER}")).await;
    assert!(
        d["reply"]
            .as_str()
            .unwrap()
            .starts_with("I'm already tracking"),
        "{d}"
    );
    assert_eq!(stored(&h).len(), 1);

    // No number: the list.
    let d = chat_as(&h, "test", "where are my parcels").await;
    let reply = d["reply"].as_str().unwrap();
    assert!(
        reply.starts_with("I'm tracking 1 parcel:")
            && reply.contains(NUMBER)
            && reply.contains("Arrived at the sorting centre"),
        "{reply}"
    );

    // Nothing tracked: an invitation.
    let empty = with_scraper(scraper.uri()).await;
    let d = chat_as(&empty, "test", "where are my parcels").await;
    assert_eq!(
        d["reply"],
        "I'm not tracking any parcel. Send me a tracking number and I'll follow it."
    );

    // A self-test session looks it up and writes nothing.
    let st = with_scraper(scraper.uri()).await;
    let d = chat_as(&st, "selftest-parcels", &format!("track this {NUMBER}")).await;
    let reply = d["reply"].as_str().unwrap();
    assert!(
        reply.starts_with("Tracking Poste Italiane") && reply.ends_with(" (self-test: not saved)"),
        "{reply}"
    );
    assert!(stored(&st).is_empty());
    assert!(!st.dir.path().join("parcels").join("parcels.json").exists());

    // An MCP caller's turn is not handled here.
    let mcp = with_scraper(scraper.uri()).await;
    let d = chat_as(
        &mcp,
        "test",
        &format!("[via Lucina MCP, from claude] track this {NUMBER}"),
    )
    .await;
    assert_ne!(d["action"], "parcel_track", "{d}");
    assert!(stored(&mcp).is_empty());

    // The scraper down: the normal turn answers, nothing is saved.
    let down = with_scraper("http://127.0.0.1:1".into()).await;
    let d = chat_as(&down, "test", &format!("track this {NUMBER}")).await;
    assert_ne!(d["action"], "parcel_track", "{d}");
    assert!(stored(&down).is_empty());

    // Off means off: no service, no turn.
    let off = harness(chatty(), |c: &mut Config| c.parcels.enabled = false).await;
    assert!(off.state.tool_env.parcels.is_none());
    let d = chat_as(&off, "test", &format!("track this {NUMBER}")).await;
    assert_ne!(d["action"], "parcel_track", "{d}");

    // Nothing in the stub was asked for a parcel that was not saved or looked up.
    let tracked = scraper
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/v1/track")
        .count();
    assert!(tracked >= 3, "{tracked}");
}

#[tokio::test]
async fn a_carrier_that_needs_more_or_has_no_record_is_said_plainly() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/detect"))
        .respond_with(DetectByText)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/track"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "needs postcode", "field": "postcode", "hint": {"kind": "input_required"}})))
        .mount(&server)
        .await;
    let h = with_scraper(server.uri()).await;
    let svc = h.state.tool_env.parcels.clone().unwrap();
    let added = svc.add(NUMBER, None, None, chrono::Utc::now(), true).await;
    assert!(added.saved);
    assert_eq!(
        added.lookup,
        Err(lucina_parcels::TrackError::InputRequired("postcode".into()))
    );
    assert_eq!(stored(&h).len(), 1, "the parcel is kept");
}
