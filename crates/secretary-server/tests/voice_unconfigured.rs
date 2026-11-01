//! `POST /v1/voice` without a transcription key: 503, as `/v1/calls/transcribe`.
//! Its own test binary: the other voice tests set the process environment.

use reqwest::StatusCode;
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::Value;

#[tokio::test]
async fn voice_is_unavailable_without_a_transcription_key() {
    assert!(std::env::var_os("SECRETARY_GEMINI_KEY_FILE").is_none());
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new("http://127.0.0.1:9", Some("lucina-server-secret".into()));
    cfg.data_dir = dir.path().join("state");
    let (_, token) = DeviceStore::new(&cfg.data_dir).issue("Speaker").unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = AppState::new(&cfg).unwrap();
    tokio::spawn(async move {
        serve_on(listener, app(state), std::future::pending())
            .await
            .unwrap()
    });
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/voice"))
        .bearer_auth(&token)
        .header("content-type", "audio/x-s16le;rate=16000")
        .body(vec![0u8; 32_000])
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "transcription_unavailable");
}
