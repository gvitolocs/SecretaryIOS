//! Her Pokémon run on an owner (iOS) turn: the chat shortcut is skipped while the game is live, the planner sees
//! the live state and the override rule, and a game_hint plan reaches the game server's /hint.

mod common;

use common::{harness, script};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn game_is_in_the_system_part_on_every_path() {
    let game = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/state"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"running": true, "actions": 3100, "events": ["Walked to Route 5 gate"]}),
        ))
        .mount(&game)
        .await;
    Mock::given(method("GET")).and(path("/observe"))
        .respond_with(ResponseTemplate::new(200).set_body_string("MODE: Overworld\nLOCATION: ROUTE_5 (map 16) x=10 y=27\nSCREEN TEXT:\n  I'm on guard duty.\nLAST RESULT: The guard is thirsty"))
        .mount(&game).await;
    Mock::given(method("POST"))
        .and(path("/hint"))
        .and(header("X-Lucina-Game", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .expect(1)
        .mount(&game)
        .await;
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

    // 1. An MCP turn in a long conversation full of her old denials takes the casual chat shortcut (trimmed
    //    context): the live game and the PRIORITY rule are still in the system part, and no game action runs.
    let sid = "secretary_mcp-claude-code";
    let now = chrono::Utc::now();
    for i in 0..40 {
        h.state.recent.append(
            sid,
            "user",
            &format!(
                "({i}) stai giocando tu a Pokémon? {}",
                "dimmi tutto ".repeat(10)
            ),
            now,
        );
        h.state.recent.append(sid, "assistant", "No, non sono io: non ho nessun emulatore né controllo di gioco collegato qui, e non ho nessuno stato di gioco da leggere.", now);
    }
    let (s, b) = h
        .post("/chat", json!({"message": "[via Lucina MCP, from claude-code] Tu stai giocando? Dove sei adesso?", "sessionId": "mcp-claude-code", "channel": "web"}))
        .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["action"], "reply", "{b}");
    let reqs = h.fake.requests();
    let reply = reqs
        .iter()
        .find(|r| lucina_server::fake::kind_of(r) == "reply")
        .expect("casual reply ran");
    let system = reply["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("ROUTE_5"), "{system}");
    assert!(system.contains("This Pokémon Red run is YOU"));
    assert!(system.contains("PRIORITY: this live game block outranks Recent chat turns"));
    assert!(system.contains("This turn comes from an MCP client"));
    let user = reply["messages"][1]["content"].as_str().unwrap();
    assert!(
        user.contains("(tail)") || user.contains("…"),
        "the history was trimmed"
    );
    let before = reqs.len();

    // 2. The owner's iOS advice: full planner, live state in the system part, game_hint reaches /hint.
    let d = h.chat("Basta si ti sto guardando giocare sei andata per la 30th volta davanti al poliziotto che ha sete vedi online come si va alla prossima medaglia o come si prende la cosa bere come obiettivo").await;
    assert_eq!(d["action"], "game", "{d}");
    assert_eq!(d["reply"], "Ok, l'ho passato al mio io che gioca.");
    let reqs = h.fake.requests()[before..].to_vec();
    let plan = reqs
        .iter()
        .find(|r| lucina_server::fake::kind_of(r) == "plan")
        .expect("full planner ran");
    let body = plan.to_string();
    assert!(plan["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("ROUTE_5"));
    assert!(!plan["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("MCP client"));
    assert!(
        body.contains("This Pok\\u00e9mon Red run is YOU")
            || body.contains("This Pokémon Red run is YOU")
    );
    assert!(body.contains("PRIORITY: this live game block outranks Recent chat turns"));
    assert!(!reqs
        .iter()
        .any(|r| lucina_server::fake::kind_of(r) == "reply"));
    game.verify().await;
}
