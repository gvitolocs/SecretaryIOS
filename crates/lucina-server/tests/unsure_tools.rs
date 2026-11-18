//! CO-C005 (TURN-02): an owner turn Jev is unsure of goes to the planner with her tools, instead of
//! a plain chat. The same words from an MCP client, and the same words with `LUCINA_UNSURE_TOOLS`
//! off, are answered as before.
//!
//! A tool list is not observable here: this is the JSON planner, where no model request carries
//! `tools` (the native loop that does runs only with `LUCINA_PLANNER=tools`, and it never asks the
//! classifier). So the turn is checked twice: in the classification `POST /jev/route` returns, and
//! in the model call a real `/chat` turn makes, a plan or a plain chat answer.

mod common;

use common::{harness, script};
use serde_json::{json, Value};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A turn Jev cannot place: no tier-0 rule takes it, its words are not actionable on their own, and
/// Jev's answer is `unknown` at a confidence no threshold accepts.
const SAYING: &str = "move the seed trays from the porch into the greenhouse";

/// Jev, with no idea what the message is about. Its client reads its settings from the environment
/// once, so this is the whole binary's Jev.
async fn jev_has_no_idea() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model": "jev-1.14.0",
            "usage": {"input_tokens": 100, "output_tokens": 10},
            "answers": {
                "intent": {"type": "choice", "choice": "unknown", "confidence": 0.2,
                           "probabilities": {"unknown": 0.2, "chat": 0.2}},
                "scope": {"type": "choice", "choice": "all", "confidence": 0.5,
                          "probabilities": {"all": 0.5, "today": 0.5}},
                "needs_memory": {"type": "noul", "noul": 0.2},
                "needs_tools": {"type": "noul", "noul": 0.2},
            }
        })))
        .mount(&server)
        .await;
    std::env::set_var("JEV_API_KEY", "test-key");
    std::env::set_var("LUCINA_JEV", "on");
    std::env::set_var("LUCINA_JEV_URL", format!("{}/v1/systemone", server.uri()));
    std::env::set_var("LUCINA_JEV_FIRST", "1");
    assert!(
        lucina_jev::shared().available(),
        "the stub Jev is not in use"
    );
    server
}

/// The model calls a turn made, from request number `from` on.
fn kinds(h: &common::H, from: usize) -> Vec<&'static str> {
    h.fake
        .requests()
        .iter()
        .skip(from)
        .map(lucina_server::fake::kind_of)
        .collect()
}

/// What the classifier makes of [`SAYING`] for the owner (`mcp` false) or for an agent.
async fn routed(h: &common::H, mcp: bool) -> Value {
    let (s, b) = h
        .post("/jev/route", json!({"text": SAYING, "mcp": mcp}))
        .await;
    assert_eq!(s, 200, "{b}");
    b["data"]["routed"].clone()
}

#[tokio::test]
async fn an_unsure_owner_turn_goes_to_the_planner_with_the_tools() {
    // The shape the two turns below rest on: nothing else takes this sentence, so a plain chat
    // classification would answer it with words alone.
    assert!(
        lucina_intent::tier0::classify_tier0(
            SAYING,
            "",
            &lucina_intent::heuristics::Settings::default()
        )
        .is_none(),
        "a tier-0 rule takes the sentence"
    );
    assert!(
        !lucina_intent::heuristics::looks_like_actionable(SAYING),
        "the sentence reads as actionable on its own"
    );

    let _jev = jev_has_no_idea().await;
    let h = harness(
        script(json!({"rules": [
            {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"I will sort out the trays.\"}"},
            {"kind": "reply", "content": "Nice plan."}
        ]})),
        |_| {},
    )
    .await;

    // 1. The owner: her turn keeps the `chat` label, but takes the planner's path with her tools.
    let r = routed(&h, false).await;
    assert_eq!(r["intent"], "chat", "{r}");
    assert_eq!(r["replyMode"], "full_plan", "{r}");

    // 2. The same words from an agent: words, as before.
    let r = routed(&h, true).await;
    assert_eq!(r["replyMode"], "chat", "{r}");

    // 3. A real owner turn: the planner sees it; nobody answers it as a plain chat.
    let n = h.fake.requests().len();
    let d = h.chat(SAYING).await;
    let ks = kinds(&h, n);
    assert!(
        ks.contains(&"plan"),
        "the planner never saw the turn: {ks:?} {d}"
    );
    assert!(
        !ks.contains(&"reply"),
        "the turn was answered as a plain chat: {ks:?}"
    );

    // 4. The same turn from an MCP client: a plain chat answer, no planner.
    let n = h.fake.requests().len();
    let (s, b) = h
        .post(
            "/chat",
            json!({
                "message": format!("[via Lucina MCP, from grok] {SAYING}"),
                "sessionId": "mcp-grok",
                "channel": "ios"
            }),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    let ks = kinds(&h, n);
    assert!(
        !ks.contains(&"plan"),
        "an agent's turn went to the planner: {ks:?}"
    );
    assert!(
        ks.contains(&"reply"),
        "the agent's turn lost its plain chat answer: {ks:?}"
    );

    // 5. `LUCINA_UNSURE_TOOLS` off: the owner turn is answered as before.
    std::env::set_var("LUCINA_UNSURE_TOOLS", "off");
    let r = routed(&h, false).await;
    assert_eq!(r["replyMode"], "chat", "{r}");
    std::env::remove_var("LUCINA_UNSURE_TOOLS");
}
