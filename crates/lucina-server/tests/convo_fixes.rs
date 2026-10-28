//! 2026-10-28 conversation fixes, end to end against the fake DeepSeek: a paused game run no longer forces
//! owner turns through the JSON planner, and a plan that is not JSON twice falls back to a reply about the
//! latest message only.

mod common;

use common::{harness, script};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn game_server(running: bool) -> MockServer {
    let game = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/state"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"running": running, "actions": 17274, "events": ["Healing the team"]}),
        ))
        .mount(&game)
        .await;
    Mock::given(method("GET"))
        .and(path("/observe"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("MODE: Dialog\nLOCATION: CHAMPIONS_ROOM (map 120) x=4 y=3"),
        )
        .mount(&game)
        .await;
    game
}

fn kinds(h: &common::H) -> Vec<&'static str> {
    h.fake
        .requests()
        .iter()
        .map(lucina_server::fake::kind_of)
        .collect()
}

fn system_of(req: &Value) -> &str {
    req["messages"][0]["content"].as_str().unwrap()
}

// One test: the game server address is process-wide (env), so the two scenarios run in sequence.
#[tokio::test]
async fn paused_game_and_failed_plans() {
    // 1. The run is paused (finished): an unrelated message takes the cheap router, not plan_response, and
    //    the paused block carries no rule that pushes game actions.
    let paused = game_server(false).await;
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
            {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"WRONG: the full planner ran\"}"},
            {"kind": "reply", "content": "Ciao!"}
        ]})),
        |_| {},
    )
    .await;
    std::env::set_var("LUCINA_GAME", "true"); // after the harness, which pins it off
    std::env::set_var("LUCINA_GAME_URL", paused.uri());
    let d = h.chat("che ne pensi dei gatti?").await;
    assert_eq!(d["reply"], "Ciao!", "{d}");
    assert!(
        !kinds(&h).contains(&"plan"),
        "a paused run must not force plan_response: {:?}",
        kinds(&h)
    );
    let reqs = h.fake.requests();
    let reply = reqs
        .iter()
        .find(|r| lucina_server::fake::kind_of(r) == "reply")
        .unwrap();
    let system = system_of(reply);
    assert!(system.contains("CHAMPIONS_ROOM"), "kept as information");
    assert!(system.contains("it is paused"));
    assert!(!system.contains("PRIORITY: this live game block outranks"));
    assert!(system.contains("Reply language: Italian"));

    // 2. A playing run does take the planner; its answer is not JSON twice, so she says so briefly in the
    //    language of the message instead of a tool-less guess (no reply call).
    let playing = game_server(true).await;
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
            {"kind": "plan", "content": "Sure, I will look at the calendar for you."},
            {"kind": "reply", "content": "It is at 12:00."}
        ]})),
        |_| {},
    )
    .await;
    std::env::set_var("LUCINA_GAME", "true");
    std::env::set_var("LUCINA_GAME_URL", playing.uri());
    let now = chrono::Utc::now();
    h.state.recent.append(
        "secretary_test",
        "user",
        "Ho fatto la lavanderia, poi vado alla posta per il pacco",
        now,
    );
    let d = h
        .chat("You know what time is the meeting therese è teresa")
        .await;
    assert_eq!(
        d["reply"],
        "Sorry, I got tangled up working out how to handle that message. Could you send it again?",
        "{d}"
    );
    let ks = kinds(&h);
    assert_eq!(
        ks.iter().filter(|k| **k == "plan").count(),
        2,
        "one plan call and one repair retry: {ks:?}"
    );
    let reqs = h.fake.requests();
    let plans: Vec<&Value> = reqs
        .iter()
        .filter(|r| lucina_server::fake::kind_of(r) == "plan")
        .collect();
    assert_eq!(plans[0]["response_format"], json!({"type": "json_object"}));
    assert!(plans[1]["messages"]
        .to_string()
        .contains("Return only the JSON object for this turn"));
    assert!(
        !ks.contains(&"reply"),
        "no tool-less guess after two failed plans: {ks:?}"
    );
}

/// 10:59: "Explode outlook in your sandbox" (speech-to-text for "Explore ...") is routed to her computer before
/// any planner or reply call; with the computer off the owner is told so.
#[tokio::test]
async fn the_computer_route_comes_before_the_planner() {
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
            {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"WRONG\"}"},
            {"kind": "reply", "content": "WRONG"}
        ]})),
        |_| {},
    )
    .await;
    let d = h.chat("Explode outlook in your sandbox").await;
    assert_eq!(d["action"], "computer_task", "{d}");
    assert!(d["reply"].as_str().unwrap().contains("switched off"), "{d}");
    let ks = kinds(&h);
    assert!(!ks.contains(&"plan") && !ks.contains(&"reply"), "{ks:?}");
}
