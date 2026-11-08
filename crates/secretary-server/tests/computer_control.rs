//! `/v1/computer/control` against a mock Lucina: what reaches her is the action, the device the
//! login names, and (for a heartbeat) whether the owner gave real input; nothing else from the body.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

#[derive(Default)]
struct Mock {
    seen: Mutex<Vec<Value>>,
}

async fn control(State(m): State<Arc<Mock>>, Json(body): Json<Value>) -> Json<Value> {
    m.seen.lock().unwrap().push(body);
    Json(json!({"ok": true, "data": {"state": {"driver": "owner"}}}))
}

#[tokio::test]
async fn heartbeats_carry_whether_the_owner_was_active() {
    let mock = Arc::new(Mock::default());
    let lucina = Router::new()
        .route("/api/secretary/computer/control", post(control))
        .with_state(mock.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lucina_url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, lucina).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina_url, Some("lucina-secret".into()));
    cfg.data_dir = dir.path().join("state");
    let (_, token) = DeviceStore::new(&cfg.data_dir)
        .issue("Secretary Code Mac")
        .unwrap();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let state = AppState::new(&cfg).unwrap();
    tokio::spawn(async move {
        serve_on(l, app(state), std::future::pending())
            .await
            .unwrap()
    });
    let http = reqwest::Client::new();
    let send = |body: Value| {
        http.post(format!("{base}/v1/computer/control"))
            .bearer_auth(&token)
            .json(&body)
            .send()
    };

    for (body, expect) in [
        (
            json!({"sandbox": "workspace", "action": "heartbeat", "active": false, "device": "forged", "extra": 1}),
            json!({"sandbox": "workspace", "action": "heartbeat", "active": false, "device": "Secretary Code Mac"}),
        ),
        (
            json!({"sandbox": "workspace", "action": "heartbeat", "active": true}),
            json!({"sandbox": "workspace", "action": "heartbeat", "active": true, "device": "Secretary Code Mac"}),
        ),
        // An older app says nothing (Lucina then counts the heartbeat as input, as before).
        (
            json!({"sandbox": "workspace", "action": "heartbeat"}),
            json!({"sandbox": "workspace", "action": "heartbeat", "device": "Secretary Code Mac"}),
        ),
        // Only a boolean passes.
        (
            json!({"sandbox": "workspace", "action": "heartbeat", "active": "yes"}),
            json!({"sandbox": "workspace", "action": "heartbeat", "device": "Secretary Code Mac"}),
        ),
    ] {
        let r = send(body).await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(mock.seen.lock().unwrap().last().cloned().unwrap(), expect);
    }
}
