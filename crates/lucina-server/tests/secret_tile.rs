//! "Let me give you the maps key": Lucina opens the Add secret card in his chat at once, with a name
//! and a place she picks, and an open card for the same secret comes down instead of a new one. A
//! self-test turn gets a card of its own and never touches his chat or his cards.

mod common;

use common::{harness, script};
use serde_json::{json, Value};

const OFFER: &str = "let me gibe you the maps apikey";

fn rules(name: &str) -> Value {
    json!({"rules": [
        {"kind": "agent", "contains": "maps", "afterTool": false, "toolCalls": [{"name": "secret_tile", "arguments": {"name": name}}]},
        {"kind": "agent", "contains": "maps", "afterTool": true, "content": "The card is open below: paste it there."},
        {"kind": "any", "content": "{\"action\":\"reply\",\"text\":\"Ok.\"}"}
    ]})
}

async fn setup(name: &str) -> common::H {
    harness(script(rules(name)), |_| {}).await
}

fn agent_request(name: &str) -> Value {
    json!({"name": name, "project": "lucina", "environment": "prod", "reason": "secretary-maps asks Google for bike routes",
           "asking_client": "claude-code", "from": "gmaps"})
}

async fn chat(h: &common::H, session: &str, message: &str) -> Value {
    let (s, b) = h
        .post(
            "/chat",
            json!({"message": message, "sessionId": session, "channel": "web"}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    b["data"].clone()
}

async fn queue(h: &common::H) -> Value {
    let (s, b) = h.get("/secrets/requests").await;
    assert_eq!(s, 200, "{b}");
    b["data"].clone()
}

fn cards(h: &common::H) -> Vec<Value> {
    h.timeline_lines()
        .into_iter()
        .filter(|l| l["meta"]["kind"] == "secret_request")
        .collect()
}

fn entry<'a>(q: &'a Value, name: &str) -> &'a Value {
    q["requests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in {q}"))
}

#[tokio::test]
async fn an_open_card_for_the_same_secret_comes_down_with_its_place() {
    let h = setup("GOOGLE_MAPS_SERVER_KEY").await;
    let (s, _) = h
        .post("/secrets/request", agent_request("GOOGLE_MAPS_SERVER_KEY"))
        .await;
    assert_eq!(s, 200);
    h.post("/secrets/request", agent_request("GOOGLE_MAPS_WEB_KEY"))
        .await;
    assert_eq!(cards(&h).len(), 1);

    let d = chat(&h, "test", OFFER).await;
    assert_eq!(d["reply"], "The card is open below: paste it there.", "{d}");
    // The same request, brought to the bottom: a second card item for it, in the place it named.
    let all = cards(&h);
    assert_eq!(all.len(), 2, "{all:?}");
    let q = queue(&h).await;
    let e = entry(&q, "GOOGLE_MAPS_SERVER_KEY");
    assert_eq!(
        (e["status"].as_str(), e["project"].as_str()),
        (Some("active"), Some("lucina"))
    );
    assert_eq!(e["itemId"], all[1]["id"]);
    assert_eq!(entry(&q, "GOOGLE_MAPS_WEB_KEY")["status"], "queued");
    assert_eq!(q["requests"].as_array().unwrap().len(), 2, "no new request");
    // She was told which cards are open, by name, and nothing else of them.
    let asked = serde_json::to_string(&h.fake.requests()).unwrap();
    assert!(
        asked.contains("GOOGLE_MAPS_WEB_KEY in lucina/prod (queued"),
        "{asked}"
    );
    assert!(asked.contains("Secrets only ever go to Infisical"));
}

#[tokio::test]
async fn his_card_goes_ahead_of_an_agents_and_the_agents_follows() {
    let h = setup("STRIPE_API_KEY").await;
    h.post("/secrets/request", agent_request("GOOGLE_MAPS_SERVER_KEY"))
        .await;
    chat(&h, "test", OFFER).await;
    let q = queue(&h).await;
    let mine = entry(&q, "STRIPE_API_KEY");
    // Nothing named a place: his own secrets' place.
    assert_eq!(
        (
            mine["status"].as_str(),
            mine["project"].as_str(),
            mine["environment"].as_str()
        ),
        (Some("active"), Some("personal"), Some("prod"))
    );
    assert_eq!(
        mine["who"], "",
        "his own request: no push to his phone, no 'who asks'"
    );
    assert_eq!(entry(&q, "GOOGLE_MAPS_SERVER_KEY")["status"], "queued");
    let last = cards(&h).pop().unwrap();
    assert!(
        last["text"]
            .as_str()
            .unwrap()
            .contains("Paste `STRIPE_API_KEY` here"),
        "{last}"
    );
    // "Not now" on his: the agent's card comes back at the bottom.
    let id = mine["id"].as_str().unwrap().to_owned();
    let (s, b) = h
        .post(&format!("/secrets/requests/{id}/dismiss"), json!({}))
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(
        entry(&queue(&h).await, "GOOGLE_MAPS_SERVER_KEY")["status"],
        "active"
    );
}

#[tokio::test]
async fn a_self_test_never_reaches_his_chat_or_his_cards() {
    let h = setup("CANARY_CARD_TEST").await;
    h.post("/secrets/request", agent_request("GOOGLE_MAPS_SERVER_KEY"))
        .await;
    let before = h.timeline_lines().len();
    let d = chat(&h, "selftest-card", OFFER).await;
    assert_eq!(d["reply"], "The card is open below: paste it there.", "{d}");
    assert_eq!(h.timeline_lines().len(), before, "his chat got something");
    let q = queue(&h).await;
    assert_eq!(q["requests"].as_array().unwrap().len(), 1);
    assert_eq!(entry(&q, "GOOGLE_MAPS_SERVER_KEY")["status"], "active");
    let selftest = h.state.secret_cards.selftest.list();
    assert_eq!(entry(&selftest, "CANARY_CARD_TEST")["status"], "active");
    let lines = h.state.selftest_timeline.read_all().unwrap();
    assert!(
        lines.iter().any(|l| l.raw.contains("CANARY_CARD_TEST")),
        "the card went to the self-test conversation"
    );
}

#[tokio::test]
async fn an_agent_turn_cannot_open_his_card() {
    let h = setup("X_KEY").await;
    let ctx = lucina_tools::Ctx::new(h.state.tool_env.clone(), "web", "test")
        .with_origin(lucina_tools::Origin::Mcp);
    let tool = lucina_tools::tools::secret_tile::SecretCardTool {
        opener: h.state.secret_cards.clone(),
    };
    let e = lucina_tools::Tool::call(&tool, json!({"name": "X_KEY"}), &ctx)
        .await
        .unwrap_err();
    assert!(e.0.contains("owner"), "{}", e.0);
    assert!(cards(&h).is_empty());
}
