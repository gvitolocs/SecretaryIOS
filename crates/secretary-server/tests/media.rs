//! Private assets (`/v1/assets/{name}`) and text to speech (`/v1/tts`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode as AxumStatus;
use axum::response::IntoResponse;
use axum::Router;
use reqwest::{Client, StatusCode};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const MP3: &[u8] = b"ID3\x04\x00fake-mpeg-frames";

#[derive(Default)]
struct Mock {
    seen: Mutex<Vec<(String, Value)>>,
    mode: Mutex<&'static str>,
}

async fn kokoro(
    State(m): State<Arc<Mock>>,
    uri: axum::http::Uri,
    body: Bytes,
) -> axum::response::Response {
    m.seen.lock().unwrap().push((
        uri.path().to_owned(),
        serde_json::from_slice(&body).unwrap_or(Value::Null),
    ));
    let mode = *m.mode.lock().unwrap();
    match mode {
        "fail" => (AxumStatus::INTERNAL_SERVER_ERROR, "boom").into_response(),
        "slow" => {
            tokio::time::sleep(Duration::from_secs(3)).await;
            MP3.into_response()
        }
        _ => ([("content-type", "audio/mpeg")], MP3).into_response(),
    }
}

async fn spawn(router: Router, connect_info: bool) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        if connect_info {
            serve_on(listener, router, std::future::pending())
                .await
                .unwrap()
        } else {
            axum::serve(listener, router).await.unwrap()
        }
    });
    base
}

struct H {
    base: String,
    http: Client,
    token: String,
    mock: Arc<Mock>,
    assets: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

async fn harness(tweak: impl FnOnce(&mut Config)) -> H {
    let mock = Arc::new(Mock::default());
    let tts = spawn(
        Router::new().fallback(kokoro).with_state(mock.clone()),
        false,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new("http://127.0.0.1:9", None);
    cfg.data_dir = dir.path().join("state");
    cfg.private_assets_dir = dir.path().join("private-assets");
    cfg.tts_url = format!("{tts}/");
    cfg.tts_timeout = Duration::from_millis(800);
    tweak(&mut cfg);
    std::fs::create_dir_all(&cfg.private_assets_dir).unwrap();
    let (_, token) = DeviceStore::new(&cfg.data_dir).issue("iPhone").unwrap();
    let base = spawn(app(AppState::new(&cfg).unwrap()), true).await;
    H {
        base,
        http: Client::new(),
        token,
        mock,
        assets: cfg.private_assets_dir.clone(),
        _dir: dir,
    }
}

impl H {
    fn url(&self, p: &str) -> String {
        format!("{}{p}", self.base)
    }
    async fn tts(&self, body: Value) -> reqwest::Response {
        self.http
            .post(self.url("/v1/tts"))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await
            .unwrap()
    }
    async fn login(&self) -> String {
        let r = self
            .http
            .post(self.url("/v1/session"))
            .json(&json!({"token": self.token}))
            .send()
            .await
            .unwrap();
        let c = r.headers()["set-cookie"].to_str().unwrap().to_owned();
        c.split(';').next().unwrap().to_owned()
    }
}

#[tokio::test]
async fn private_assets_need_auth_and_carry_cache_headers() {
    let h = harness(|_| {}).await;
    let model: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    std::fs::write(h.assets.join("lucina.glb"), &model).unwrap();
    std::fs::write(h.assets.join("notes.txt"), "secret").unwrap();

    // never without auth
    let r = h
        .http
        .get(h.url("/v1/assets/lucina.glb"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let body = r.bytes().await.unwrap();
    assert!(!body.windows(16).any(|w| w == &model[..16]));
    let r = h
        .http
        .get(h.url("/v1/assets/lucina.glb"))
        .bearer_auth("sec_not-a-real-token")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    let r = h
        .http
        .get(h.url("/v1/assets/lucina.glb"))
        .bearer_auth(&h.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["content-type"], "model/gltf-binary");
    assert_eq!(r.headers()["cache-control"], "private, max-age=86400");
    let etag = r.headers()["etag"].to_str().unwrap().to_owned();
    assert!(etag.starts_with('"') && etag.ends_with('"'));
    assert_eq!(r.bytes().await.unwrap().as_ref(), &model[..]);

    let r = h
        .http
        .get(h.url("/v1/assets/lucina.glb"))
        .bearer_auth(&h.token)
        .header("if-none-match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(r.headers()["etag"], etag.as_str());

    let r = h
        .http
        .get(h.url("/v1/assets/lucina.glb"))
        .bearer_auth(&h.token)
        .header("range", "bytes=100-199")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.bytes().await.unwrap().as_ref(), &model[100..200]);

    // browsers: the cookie is enough for a GET
    let cookie = h.login().await;
    let r = h
        .http
        .get(h.url("/v1/assets/lucina.glb"))
        .header("cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);

    for name in [
        "missing.glb",
        "notes.txt",
        "..%2Fstate%2Fdevices.json",
        ".hidden.png",
        "Lucina.glb",
    ] {
        let r = h
            .http
            .get(h.url(&format!("/v1/assets/{name}")))
            .bearer_auth(&h.token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "{name}");
    }
}

#[tokio::test]
async fn tts_proxies_to_kokoro_and_streams_mp3() {
    let h = harness(|_| {}).await;
    let r = h
        .tts(json!({"text": "Ciao Giuseppe, come stai oggi?"}))
        .await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["content-type"], "audio/mpeg");
    assert_eq!(r.headers()["x-tts-voice"], "if_sara");
    assert_eq!(r.bytes().await.unwrap().as_ref(), MP3);
    let (path, body) = h.mock.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(path, "/v1/audio/speech");
    assert_eq!(
        body,
        json!({"model": "kokoro", "input": "Ciao Giuseppe, come stai oggi?", "voice": "if_sara",
               "response_format": "mp3", "speed": 1.0})
    );

    let r = h.tts(json!({"text": "Good morning", "speed": 1.25})).await;
    assert_eq!(r.headers()["x-tts-voice"], "af_heart");
    let r = h.tts(json!({"text": "Hello", "lang": "it"})).await;
    assert_eq!(r.headers()["x-tts-voice"], "if_sara");
    let r = h
        .tts(json!({"text": "Ciao", "voice": "bf_emma", "lang": "it"}))
        .await;
    assert_eq!(r.headers()["x-tts-voice"], "bf_emma");
    let (_, body) = h.mock.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(body["voice"], "bf_emma");

    for bad in [
        json!({"text": ""}),
        json!({"text": "x".repeat(1201)}),
        json!({"text": "hi", "voice": "zz_unknown"}),
        json!({"text": "hi", "lang": "fr"}),
        json!({"text": "hi", "speed": 3.0}),
    ] {
        let r = h.tts(bad.clone()).await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    let r = h.tts(json!({"text": "x".repeat(1200)})).await;
    assert_eq!(r.status(), StatusCode::OK);
}

#[tokio::test]
async fn tts_auth_csrf_and_rate_limit() {
    let h = harness(|cfg| cfg.rate_limit_per_minute = 1000).await;
    let r = h
        .http
        .post(h.url("/v1/tts"))
        .json(&json!({"text": "hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let cookie = h.login().await;
    let r = h
        .http
        .post(h.url("/v1/tts"))
        .header("cookie", &cookie)
        .json(&json!({"text": "hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = h
        .http
        .post(h.url("/v1/tts"))
        .header("cookie", &cookie)
        .header("x-secretary-csrf", "1")
        .json(&json!({"text": "hi"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    // 30 per minute per device (one used above)
    for _ in 0..29 {
        assert_eq!(h.tts(json!({"text": "hi"})).await.status(), StatusCode::OK);
    }
    let r = h.tts(json!({"text": "hi"})).await;
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(r.headers().contains_key("retry-after"));
}

#[tokio::test]
async fn tts_upstream_failures_are_tts_unavailable() {
    let h = harness(|_| {}).await;
    *h.mock.mode.lock().unwrap() = "fail";
    let r = h.tts(json!({"text": "hi"})).await;
    assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"]["code"],
        "tts_unavailable"
    );
    *h.mock.mode.lock().unwrap() = "slow";
    let r = h.tts(json!({"text": "hi"})).await;
    assert_eq!(r.status(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"]["code"],
        "tts_unavailable"
    );

    let down = harness(|cfg| cfg.tts_url = "http://127.0.0.1:9".into()).await;
    let r = down.tts(json!({"text": "hi"})).await;
    assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"]["code"],
        "tts_unavailable"
    );
}

#[tokio::test]
async fn long_replies_are_synthesized_sentence_by_sentence_into_one_stream() {
    let h = harness(|_| {}).await;
    let sentence = "Oggi hai fatto un ottimo allenamento, brava davvero. ";
    let text = sentence.repeat(15);
    let r = h.tts(json!({ "text": text })).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["x-tts-voice"], "if_sara");
    let body = r.bytes().await.unwrap();
    let seen = h.mock.seen.lock().unwrap().clone();
    assert!(seen.len() >= 3, "{}", seen.len());
    assert_eq!(body.len(), MP3.len() * seen.len());
    let inputs: Vec<String> = seen
        .iter()
        .map(|(_, b)| {
            assert_eq!(b["voice"], "if_sara");
            b["input"].as_str().unwrap().to_owned()
        })
        .collect();
    assert!(inputs
        .iter()
        .all(|i| i.chars().count() <= 300 && i.ends_with('.')));
    assert_eq!(inputs.join(" "), text.trim());
}
