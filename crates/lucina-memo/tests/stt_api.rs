//! `POST /v1/memos/stt` with a fake whisper-server (an axum mock on the engine's
//! port) and a fake `gpu-lease` that just sleeps, so the real engine starts,
//! transcribes and is released.
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use http_body_util::BodyExt;
use lucina_memo::{api, App, Config};
use serde_json::Value;
use tower::ServiceExt;

const SVC: &str = "stt-api-service-token-0123456789abcdef";

#[derive(Clone, Default)]
struct Whisper {
    /// What `/inference` answers: (status, JSON body).
    reply: Arc<Mutex<(u16, String)>>,
    seen: Arc<Mutex<Vec<String>>>,
}

async fn inference(State(w): State<Whisper>, body: Bytes) -> axum::response::Response {
    w.seen
        .lock()
        .unwrap()
        .push(String::from_utf8_lossy(&body).into_owned());
    let (status, json) = w.reply.lock().unwrap().clone();
    (
        StatusCode::from_u16(status).unwrap(),
        [("content-type", "application/json")],
        json,
    )
        .into_response()
}

struct H {
    app: Arc<App>,
    whisper: Whisper,
    dir: tempfile::TempDir,
}

/// `lease_script` is the body of the fake gpu-lease.
async fn harness(lease_script: &str) -> H {
    harness_with(lease_script, true).await
}

/// With `serve: false` nothing answers on the engine's port (a server that never comes up).
async fn harness_with(lease_script: &str, serve: bool) -> H {
    harness_full(lease_script, serve, None).await
}

/// `vocab`: the Voice Learn file's JSON (none = no file).
async fn harness_full(lease_script: &str, serve: bool, vocab: Option<&str>) -> H {
    let whisper = Whisper::default();
    *whisper.reply.lock().unwrap() = (
        200,
        r#"{"text":" Ciao Lucina, che ore sono? ","language":"italian"}"#.into(),
    );
    let router = Router::new()
        .route("/", get(|| async { "ok" }))
        .route("/inference", post(inference))
        .with_state(whisper.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    if serve {
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    }

    let dir = tempfile::tempdir().unwrap();
    let lease = dir.path().join("gpu-lease");
    std::fs::write(&lease, format!("#!/bin/sh\n{lease_script}\n")).unwrap();
    std::fs::set_permissions(&lease, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (data, lease) = (
        dir.path().join("data").display().to_string(),
        lease.display().to_string(),
    );
    let vocab_file = dir.path().join("voice-learn.json");
    if let Some(v) = vocab {
        std::fs::write(&vocab_file, v).unwrap();
    }
    let vocab_path = vocab_file.display().to_string();
    let cfg = Config::from_lookup(|k| match k {
        "LUCINA_MEMO_DIR" => Some(data.clone()),
        "LUCINA_MEMO_OBSIDIAN_VAULT" => Some("off".into()),
        "LUCINA_MEMO_GPU_LEASE" => Some(lease.clone()),
        "LUCINA_VOICE_LEARN_FILE" => Some(vocab_path.clone()),
        "LUCINA_PROMPTER_STT_PORT" => Some(port.clone()),
        // No CPU fallback in tests: systemd-run must not start anything.
        "LUCINA_PROMPTER_WHISPER_SERVER" => Some("/nonexistent/whisper-server".into()),
        _ => None,
    });
    let app = App::new(
        cfg,
        Arc::new(lucina_llm::ScriptedModel::new()),
        "test".into(),
    )
    .unwrap()
    .with_tokens(Some("gateway-key-0123456789abcdef"), Some(SVC));
    H { app, whisper, dir }
}

fn pcm(samples: usize) -> Vec<u8> {
    (0..samples)
        .flat_map(|i| (((i % 200) as i16) * 50).to_le_bytes())
        .collect()
}

fn wav(rate: u32, pcm: &[u8]) -> Vec<u8> {
    let mut w = b"RIFF".to_vec();
    w.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&[1, 0, 1, 0]);
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    w.extend_from_slice(&[2, 0, 16, 0]);
    w.extend_from_slice(b"data");
    w.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    w.extend_from_slice(pcm);
    w
}

impl H {
    async fn post(
        &self,
        query: &str,
        content_type: Option<&str>,
        auth: bool,
        body: Vec<u8>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method("POST")
            .uri(format!("/v1/memos/stt{query}"));
        if let Some(ct) = content_type {
            req = req.header("content-type", ct);
        }
        if auth {
            req = req.header("authorization", format!("Bearer {SVC}"));
        }
        let res = api::router(self.app.clone())
            .oneshot(req.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn raw(&self, query: &str, samples: usize) -> (StatusCode, Value) {
        self.post(query, Some("audio/x-s16le;rate=16000"), true, pcm(samples))
            .await
    }

    /// Engine sessions, once the (spawned) release has run.
    async fn sessions(&self) -> u64 {
        for _ in 0..100 {
            let s = self.app.prompter.engine.status().await["sessions"]
                .as_u64()
                .unwrap();
            if s == 0 {
                return 0;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        self.app.prompter.engine.status().await["sessions"]
            .as_u64()
            .unwrap()
    }
}

#[tokio::test]
async fn raw_pcm_is_transcribed_and_the_engine_released() {
    let h = harness("exec sleep 600").await;
    let (s, v) = h.raw("?lang=it", 16_000).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["text"], "Ciao Lucina, che ore sono?");
    assert_eq!(v["language"], "it");
    assert_eq!(v["mode"], "gpu");
    assert!(v["ms"].is_u64());
    assert_eq!(h.sessions().await, 0);

    let seen = h.whisper.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].contains("name=\"language\"\r\n\r\nit\r\n"),
        "{}",
        &seen[0][..300]
    );
    assert!(seen[0].contains("name=\"prompt\"\r\n\r\nLucina\r\n"));
    // The server is still warm (the reaper stops it later) and was reused.
    let (s, v) = h.raw("", 8_000).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let seen = h.whisper.seen.lock().unwrap().clone();
    assert!(seen[1].contains("name=\"language\"\r\n\r\nauto\r\n"));
    assert_eq!(h.sessions().await, 0);
    h.app.prompter.engine.shutdown().await;
}

#[tokio::test]
async fn a_wav_body_and_the_gateway_key_work_too() {
    let h = harness("exec sleep 600").await;
    let (s, v) = h
        .post("", Some("audio/wav"), true, wav(16_000, &pcm(9_600)))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["language"], "it");
    // Through Caddy: the gateway key instead of the service token.
    let req = Request::builder()
        .method("POST")
        .uri("/v1/memos/stt")
        .header("content-type", "audio/x-s16le;rate=16000")
        .header("x-memo-gateway", "gateway-key-0123456789abcdef")
        .body(Body::from(pcm(9_600)))
        .unwrap();
    let res = api::router(h.app.clone()).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    h.app.prompter.engine.shutdown().await;
}

#[tokio::test]
async fn silence_is_a_200_with_empty_text() {
    let h = harness("exec sleep 600").await;
    *h.whisper.reply.lock().unwrap() =
        (200, r#"{"text":" [BLANK_AUDIO] ","language":"en"}"#.into());
    let (s, v) = h.raw("", 16_000).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["text"], "");
    assert_eq!(v["language"], "en");
    h.app.prompter.engine.shutdown().await;
}

#[tokio::test]
async fn bad_requests_never_reach_the_engine() {
    let h = harness("exec sleep 600").await;
    let ok = pcm(16_000);
    for (query, ct, body, want) in [
        ("", Some("application/json"), ok.clone(), 415),
        ("", None, ok.clone(), 415),
        ("", Some("audio/x-s16le;rate=44100"), ok.clone(), 415),
        ("", Some("audio/wav"), wav(44_100, &ok), 415),
        ("", Some("audio/wav"), ok.clone(), 400),
        ("", Some("audio/x-s16le;rate=16000"), vec![], 422),
        ("", Some("audio/x-s16le;rate=16000"), pcm(4_799), 422),
        (
            "?lang=fr",
            Some("audio/x-s16le;rate=16000"),
            ok.clone(),
            400,
        ),
        (
            "?lang=it&x=1",
            Some("audio/x-s16le;rate=16000"),
            ok.clone(),
            400,
        ),
        (
            "",
            Some("audio/x-s16le;rate=16000"),
            vec![0; 2 * 1024 * 1024 + 2],
            413,
        ),
    ] {
        let (s, v) = h.post(query, ct, true, body).await;
        assert_eq!(s.as_u16(), want, "{query} {ct:?}: {v}");
    }
    let (s, _) = h
        .post("", Some("audio/x-s16le;rate=16000"), false, ok)
        .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert!(h.whisper.seen.lock().unwrap().is_empty());
    assert_eq!(h.app.prompter.engine.status().await["running"], Value::Null);
}

#[tokio::test]
async fn an_engine_that_does_not_start_is_503_and_leaves_no_session() {
    // gpu-lease says "no VRAM now" (75); the CPU fallback's server does not exist.
    let h = harness_with("exit 75", false).await;
    let (s, v) = h.raw("", 16_000).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["error"]["code"], "stt_unavailable");
    assert_eq!(h.sessions().await, 0);
}

#[tokio::test]
async fn a_failing_transcription_is_502_and_still_releases() {
    let h = harness("exec sleep 600").await;
    *h.whisper.reply.lock().unwrap() = (500, "{}".into());
    let (s, v) = h.raw("", 16_000).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY, "{v}");
    assert_eq!(v["error"]["code"], "stt_failed");
    assert_eq!(h.sessions().await, 0);
    h.app.prompter.engine.shutdown().await;
}

const VOCAB: &str = r#"{"entries":[
  {"id":"1","word":"Skibigårdsvej","updatedAt":"2026-11-01T10:00:00Z"},
  {"id":"2","word":"Horus","soundsLike":["orus"],"updatedAt":"2026-11-01T11:00:00Z"}]}"#;

#[tokio::test]
async fn the_vocabulary_biases_the_prompt_and_corrects_the_text() {
    let h = harness_full("exec sleep 600", true, Some(VOCAB)).await;
    *h.whisper.reply.lock().unwrap() = (
        200,
        r#"{"text":" Torno a skibi gaardsvej, poi guardo orus. ","language":"italian"}"#.into(),
    );
    let (s, v) = h.raw("?lang=it", 16_000).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["text"], "Torno a Skibigårdsvej, poi guardo Horus.");
    assert_eq!(
        v["corrections"],
        serde_json::json!([
            {"from": "skibi gaardsvej", "to": "Skibigårdsvej"},
            {"from": "orus", "to": "Horus"}
        ])
    );
    assert_eq!(v["ambiguous"], serde_json::json!([]));
    let seen = h.whisper.seen.lock().unwrap().clone();
    assert!(
        seen[0].contains("name=\"prompt\"\r\n\r\nLucina, Horus, Skibigårdsvej\r\n"),
        "{}",
        &seen[0][..400.min(seen[0].len())]
    );

    // The file is re-read when it changes: a new word shows up in the next prompt.
    std::fs::write(
        h.dir.path().join("voice-learn.json"),
        r#"{"entries":[{"id":"3","word":"Kokoro","updatedAt":"2026-11-02T00:00:00Z"}]}"#,
    )
    .unwrap();
    let (s, v) = h.raw("?lang=it", 16_000).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["text"], "Torno a skibi gaardsvej, poi guardo orus.");
    let seen = h.whisper.seen.lock().unwrap().clone();
    assert!(seen[1].contains("\r\n\r\nLucina, Kokoro\r\n"));
    h.app.prompter.engine.shutdown().await;
}

#[tokio::test]
async fn a_borderline_match_is_reported_not_applied() {
    let h = harness_full("exec sleep 600", true, Some(VOCAB)).await;
    *h.whisper.reply.lock().unwrap() = (
        200,
        r#"{"text":" Come sta Horace? ","language":"italian"}"#.into(),
    );
    let (s, v) = h.raw("?lang=it", 16_000).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["text"], "Come sta Horace?");
    assert_eq!(v["corrections"], serde_json::json!([]));
    assert_eq!(
        v["ambiguous"],
        serde_json::json!([{"heard": "Horace", "candidates": ["Horus"]}])
    );
    h.app.prompter.engine.shutdown().await;
}

#[tokio::test]
async fn without_a_vocabulary_the_answer_has_empty_lists() {
    let h = harness("exec sleep 600").await;
    let (s, v) = h.raw("?lang=it", 16_000).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["corrections"], serde_json::json!([]));
    assert_eq!(v["ambiguous"], serde_json::json!([]));
    h.app.prompter.engine.shutdown().await;
}
