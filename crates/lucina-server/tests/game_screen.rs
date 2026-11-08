//! The apps' Pokémon screen (secretary-server /v1/game/* relays here). Own file: LUCINA_GAME_URL is process-wide.

mod common;

use common::{harness, script};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The apps' Pokémon screen: /api/secretary/game/{state,screenshot,control} relay the local game server.
#[tokio::test]
async fn game_screen_routes_relay_the_game_server() {
    let game = MockServer::start().await;
    Mock::given(method("GET")).and(path("/state"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"running": true, "map_name": "ROCKET_HIDEOUT_B4F", "badges": 4, "party": [{"name": "DUGTRIO", "level": 29, "hp": 69, "max_hp": 69}]})))
        .mount(&game).await;
    Mock::given(method("GET"))
        .and(path("/screenshot"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(vec![0x89, b'P', b'N', b'G', 1, 2, 3])
                .insert_header("content-type", "image/png"),
        )
        .mount(&game)
        .await;
    Mock::given(method("POST"))
        .and(path("/control"))
        .and(header("X-Lucina-Game", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"running": false})))
        .expect(1)
        .mount(&game)
        .await;
    let h = harness(script(json!({"rules": []})), |_| {}).await;
    std::env::set_var("LUCINA_GAME_URL", game.uri());

    let (s, b) = h.get("/game/state").await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["map_name"], "ROCKET_HIDEOUT_B4F");
    assert_eq!(b["data"]["party"][0]["name"], "DUGTRIO");

    let r = h
        .http
        .get(h.url("/game/screenshot"))
        .bearer_auth(common::TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");
    assert_eq!(&r.bytes().await.unwrap()[..4], &[0x89, b'P', b'N', b'G']);

    let (s, b) = h.post("/game/control", json!({"running": false})).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["data"]["running"], false);
    let (s, _) = h.post("/game/control", json!({"running": "yes"})).await;
    assert_eq!(s, 400);
    let (s, _, _) = h.call(reqwest::Method::POST, "/game/state", None).await;
    assert_eq!(s, 405);

    // without the token nothing is relayed
    let r = h.http.get(h.url("/game/state")).send().await.unwrap();
    assert_eq!(r.status(), 401);
}
