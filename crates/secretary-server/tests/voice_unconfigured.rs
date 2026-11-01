//! `POST /v1/voice` without a Gemini key (p1 today): local speech-to-text alone
//! must carry a turn, and its failure is a 503, as `/v1/calls/transcribe`'s.
//! Its own test binary: the other voice tests set the process environment.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode as AxumStatus;
use axum::response::IntoResponse;
use axum::{Json, Router};
use reqwest::StatusCode;
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

async fn spawn(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        serve_on(listener, router, std::future::pending())
            .await
            .unwrap()
    });
    base
}

#[derive(Default)]
struct Mock {
    fail_stt: Mutex<bool>,
    stt_calls: Mutex<usize>,
    chats: Mutex<Vec<Value>>,
}

async fn stt(State(m): State<Arc<Mock>>) -> axum::response::Response {
    *m.stt_calls.lock().unwrap() += 1;
    if *m.fail_stt.lock().unwrap() {
        return (AxumStatus::SERVICE_UNAVAILABLE, "no engine").into_response();
    }
    Json(json!({"text": "Ciao Lucina", "language": "it", "ms": 3, "mode": "cpu"})).into_response()
}

async fn lucina(State(m): State<Arc<Mock>>, body: Bytes) -> axum::response::Response {
    m.chats
        .lock()
        .unwrap()
        .push(serde_json::from_slice(&body).unwrap());
    Json(json!({"ok": true, "data": {"reply": "Ciao!"}})).into_response()
}

async fn kokoro() -> axum::response::Response {
    ([("content-type", "audio/pcm")], vec![1u8, 0, 2, 0]).into_response()
}

struct H {
    base: String,
    token: String,
    mock: Arc<Mock>,
    _dir: tempfile::TempDir,
}

async fn harness() -> H {
    harness_with(true).await
}

/// `local: false` leaves local speech-to-text unconfigured.
async fn harness_with(local: bool) -> H {
    assert!(std::env::var_os("SECRETARY_GEMINI_KEY_FILE").is_none());
    let mock = Arc::new(Mock::default());
    let stt_url = spawn(
        Router::new()
            .route("/v1/memos/stt", axum::routing::post(stt))
            .with_state(mock.clone()),
    )
    .await;
    let lucina_url = spawn(Router::new().fallback(lucina).with_state(mock.clone())).await;
    let tts = spawn(Router::new().fallback(kokoro)).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina_url, Some("lucina-server-secret".into()));
    cfg.data_dir = dir.path().join("state");
    cfg.tts_url = tts;
    cfg.voice_stt_url = local.then(|| format!("{stt_url}/v1/memos/stt"));
    let (_, token) = DeviceStore::new(&cfg.data_dir).issue("Speaker").unwrap();
    let base = spawn(app(AppState::new(&cfg).unwrap())).await;
    H {
        base,
        token,
        mock,
        _dir: dir,
    }
}

impl H {
    async fn say(&self) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!("{}/v1/voice", self.base))
            .bearer_auth(&self.token)
            .header("content-type", "audio/x-s16le;rate=16000")
            .body(vec![0u8; 32_000])
            .send()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn local_speech_to_text_alone_carries_a_turn() {
    let h = harness().await;
    let r = h.say().await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["x-lucina-transcript"], "Ciao%20Lucina");
    assert_eq!(h.mock.chats.lock().unwrap()[0]["message"], "Ciao Lucina");
}

#[tokio::test]
async fn a_failing_local_engine_without_gemini_is_503() {
    let h = harness().await;
    *h.mock.fail_stt.lock().unwrap() = true;
    let r = h.say().await;
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "transcription_unavailable");
    assert_eq!(*h.mock.stt_calls.lock().unwrap(), 1);
    assert!(h.mock.chats.lock().unwrap().is_empty());
}

#[tokio::test]
async fn voice_is_unavailable_without_a_transcription_key() {
    // Neither local speech-to-text nor Gemini is set up.
    let h = harness_with(false).await;
    let r = h.say().await;
    assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "transcription_unavailable");
    assert_eq!(*h.mock.stt_calls.lock().unwrap(), 0);
}
