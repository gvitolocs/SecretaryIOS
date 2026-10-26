//! Lucina's Pokémon run for the apps: `/v1/game/state`, `/v1/game/screenshot` (PNG) and `/v1/game/control`
//! forward Lucina's `game/*` with the server's Lucina token, behind the device login (the owner's devices).

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

#[derive(Default)]
struct Mock {
    /// (method, path, Authorization, body)
    seen: Mutex<Vec<(String, String, String, String)>>,
}

async fn lucina(
    State(m): State<Arc<Mock>>,
    method: Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    m.seen.lock().unwrap().push((
        method.to_string(),
        uri.path().to_owned(),
        auth,
        String::from_utf8_lossy(&body).into(),
    ));
    match uri.path() {
        "/api/secretary/game/state" => Json(json!({"ok": true, "data": {"running": true, "map_name": "SILPH_CO_11F", "badges": 5,
            "party": [{"name": "SHELLY", "species": "BLASTOISE", "level": 40, "hp": 133, "max_hp": 133}]}})).into_response(),
        "/api/secretary/game/screenshot" => ([(header::CONTENT_TYPE, "image/png")], PNG.to_vec()).into_response(),
        "/api/secretary/game/control" => {
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            Json(json!({"ok": true, "data": {"running": v["running"]}})).into_response()
        }
        _ => (StatusCode::NOT_FOUND, Json(json!({"ok": false, "error": "not_found"}))).into_response(),
    }
}

#[tokio::test]
async fn game_routes_forward_to_lucina_for_the_owner() {
    let mock = Arc::new(Mock::default());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lucina_url = format!("http://{}", l.local_addr().unwrap());
    let router = Router::new().fallback(lucina).with_state(mock.clone());
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina_url, Some("lucina-secret".into()));
    cfg.data_dir = dir.path().join("state");
    let (_, token) = DeviceStore::new(&cfg.data_dir).issue("test").unwrap();
    let state = AppState::new(&cfg).unwrap();
    let s = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", s.local_addr().unwrap());
    tokio::spawn(async move {
        serve_on(s, app(state), std::future::pending())
            .await
            .unwrap()
    });
    let http = reqwest::Client::new();

    let r = http
        .get(format!("{base}/v1/game/state"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["map_name"], "SILPH_CO_11F", "{v}");
    assert_eq!(v["party"][0]["species"], "BLASTOISE");

    let r = http
        .get(format!("{base}/v1/game/screenshot"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/png");
    assert_eq!(r.headers()["cache-control"], "no-store");
    assert_eq!(r.bytes().await.unwrap().as_ref(), &PNG);

    let r = http
        .post(format!("{base}/v1/game/control"))
        .bearer_auth(&token)
        .json(&json!({"running": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["running"], false);
    let r = http
        .post(format!("{base}/v1/game/control"))
        .bearer_auth(&token)
        .json(&json!({"running": "maybe"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // no device login: nothing reaches Lucina
    let before = mock.seen.lock().unwrap().len();
    for p in ["/v1/game/state", "/v1/game/screenshot"] {
        assert_eq!(
            http.get(format!("{base}{p}"))
                .send()
                .await
                .unwrap()
                .status(),
            401,
            "{p}"
        );
    }
    assert_eq!(mock.seen.lock().unwrap().len(), before);

    // Lucina sees the server's token, never the device token
    let seen = mock.seen.lock().unwrap();
    assert!(seen
        .iter()
        .all(|(_, _, auth, _)| auth == "Bearer lucina-secret"));
    let control = seen
        .iter()
        .find(|(m, p, _, _)| m == "POST" && p == "/api/secretary/game/control")
        .unwrap();
    assert!(
        control.3.contains("\"running\":false")
            && control.3.contains("Paused by the owner from the app"),
        "{}",
        control.3
    );
}
