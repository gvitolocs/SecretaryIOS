//! Her Pokémon run on an owner (iOS) turn: the chat shortcut is skipped while the game is live, the planner sees
//! the live state and the override rule, and a game_hint plan reaches the game server's /hint.

mod common;

use common::{harness, script};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn ios_game_advice_routes_to_game_hint() {
    let game = MockServer::start().await;
    Mock::given(method("GET")).and(path("/state"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"running": true, "actions": 3100, "events": ["Walked to Route 5 gate"]})))
        .mount(&game).await;
    Mock::given(method("GET")).and(path("/observe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("MODE: Overworld\nLOCATION: ROUTE_5 (map 16) x=10 y=27\nSCREEN TEXT:\n  I'm on guard duty.\nLAST RESULT: The guard is thirsty"))
        .mount(&game).await;
    Mock::given(method("POST")).and(path("/hint")).and(header("X-Lucina-Game", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&game).await;
    let h = harness(
        script(json!({"rules": [
            {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.95,\"reply_mode\":\"chat\",\"needs_tools\":false}"},
            {"kind": "plan", "content": "{\"action\":\"game_hint\",\"text\":\"Stop going to the thirsty guard; buy a drink at the Celadon Dept. Store rooftop first\",\"ack\":\"Ok, l'ho passato al mio io che gioca.\"}"},
            {"kind": "reply", "content": "WRONG: casual chat shortcut"}
        ]})),
        |_| {},
    )
    .await;
    std::env::set_var("LUCINA_GAME", "true");
    std::env::set_var("LUCINA_GAME_URL", game.uri());
    let d = h.chat("Basta si ti sto guardando giocare sei andata per la 30th volta davanti al poliziotto che ha sete vedi online come si va alla prossima medaglia o come si prende la cosa bere come obiettivo").await;
    assert_eq!(d["action"], "game", "{d}");
    assert_eq!(d["reply"], "Ok, l'ho passato al mio io che gioca.");
    let reqs = h.fake.requests();
    let plan = reqs.iter().find(|r| lucina_server::fake::kind_of(r) == "plan").expect("full planner ran");
    let body = plan.to_string();
    assert!(body.contains("ROUTE_5"));
    assert!(body.contains("This Pok\\u00e9mon Red run is YOU") || body.contains("This Pokémon Red run is YOU"));
    assert!(body.contains("PRIORITY: this live game block outranks Recent chat turns"));
    assert!(!reqs.iter().any(|r| lucina_server::fake::kind_of(r) == "reply"));
    game.verify().await;
}
