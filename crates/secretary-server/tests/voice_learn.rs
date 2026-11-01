//! Voice Learn (protocol section 8): `/v1/voice/learn[/{id}]`, `/suggest` and
//! `/sample`, with a mock Lucina (the four `voice-learn` routes) and a mock
//! local speech-to-text. Its own test binary (no Gemini key in the process).
//!
//! Who may do what: GET for every device and the owner's session; writes,
//! suggest and sample for the owner's browser session (cookie) only. A device token,
//! a speaker or a just-paired one that never connected, can only read.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode as AxumStatus, Uri};
use axum::response::IntoResponse;
use axum::{Json, Router};
use futures_util::SinkExt;
use reqwest::{Client, StatusCode};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const LUCINA_TOKEN: &str = "lucina-server-secret";

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    auth: Option<String>,
    body: Value,
}

#[derive(Default)]
struct Mock {
    lucina: Mutex<Vec<Seen>>,
    /// The local engine's answer.
    heard: Mutex<String>,
    corrections: Mutex<Value>,
    stt_calls: Mutex<Vec<(Option<String>, String, usize)>>,
    stt_fails: Mutex<bool>,
}

impl Mock {
    fn seen(&self) -> Vec<Seen> {
        self.lucina.lock().unwrap().clone()
    }
}

async fn lucina(
    State(m): State<Arc<Mock>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    m.lucina.lock().unwrap().push(Seen {
        method: method.to_string(),
        path: uri.path().to_owned(),
        auth: headers
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_owned()),
        body: body.clone(),
    });
    let entry = |id: &str| {
        json!({"id": id, "word": "Skibigårdsvej", "soundsLike": ["skibi gård vej"],
               "meaning": "home street", "lang": "da", "source": "app",
               "createdAt": "2026-11-01T10:00:00Z", "updatedAt": "2026-11-01T10:00:00Z"})
    };
    match (method.as_str(), uri.path()) {
        ("GET", "/api/secretary/voice-learn") => {
            Json(json!({"ok": true, "data": {"entries": [entry("vl_1")], "max": 500}}))
                .into_response()
        }
        ("POST", "/api/secretary/voice-learn") => {
            if body["word"].as_str().unwrap_or("").is_empty() {
                return (
                    AxumStatus::BAD_REQUEST,
                    Json(json!({"ok": false, "error": "invalid_word", "message": "word is required"})),
                )
                    .into_response();
            }
            (AxumStatus::CREATED, Json(json!({"ok": true, "data": entry("vl_2")}))).into_response()
        }
        ("POST", "/api/secretary/voice-learn/suggest") => Json(json!({"ok": true, "data": {
            "candidates": [{"word": "Skibigårdsvej", "soundsLike": ["skibi gard vai"], "lang": "da"}]
        }}))
        .into_response(),
        ("PATCH", "/api/secretary/voice-learn/vl_1") => {
            Json(json!({"ok": true, "data": entry("vl_1")})).into_response()
        }
        ("DELETE", "/api/secretary/voice-learn/vl_1") => {
            Json(json!({"ok": true, "data": {"deleted": true}})).into_response()
        }
        ("DELETE", "/api/secretary/voice-learn/vl_9") => (
            AxumStatus::NOT_FOUND,
            Json(json!({"ok": false, "error": "entry_not_found", "message": "no such entry"})),
        )
            .into_response(),
        _ => (AxumStatus::NOT_FOUND, "no").into_response(),
    }
}

async fn local_stt(
    State(m): State<Arc<Mock>>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    m.stt_calls.lock().unwrap().push((
        headers.get("cookie").map(|v| v.to_str().unwrap().to_owned()),
        headers
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default(),
        body.len(),
    ));
    if *m.stt_fails.lock().unwrap() {
        return (AxumStatus::SERVICE_UNAVAILABLE, "no engine").into_response();
    }
    Json(json!({
        "text": m.heard.lock().unwrap().clone(), "language": "da", "ms": 5, "mode": "gpu",
        "corrections": m.corrections.lock().unwrap().clone(),
    }))
    .into_response()
}

async fn spawn(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        serve_on(listener, router, std::future::pending())
            .await
            .unwrap()
    });
    format!("127.0.0.1:{}", addr.port())
}

struct H {
    addr: String,
    http: Client,
    data_dir: std::path::PathBuf,
    mock: Arc<Mock>,
    /// A device token that never said hello (the phone app, or a speaker just paired).
    phone: String,
    /// The owner's browser session cookie.
    owner: String,
    /// A speaker that has said hello on the voice socket.
    speaker: String,
    _ws: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    _dir: tempfile::TempDir,
}

async fn harness() -> H {
    let mock = Arc::new(Mock::default());
    *mock.heard.lock().unwrap() = "skibi gard vej".into();
    *mock.corrections.lock().unwrap() = json!([]);
    let lucina_url = spawn(Router::new().fallback(lucina).with_state(mock.clone())).await;
    let stt = spawn(Router::new().fallback(local_stt).with_state(mock.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(format!("http://{lucina_url}"), Some(LUCINA_TOKEN.into()));
    cfg.data_dir = dir.path().join("state");
    cfg.voice_stt_url = Some(format!("http://{stt}/v1/memos/stt"));
    let store = DeviceStore::new(&cfg.data_dir);
    let (_, phone) = store.issue("Phone").unwrap();
    let (_, speaker) = store.issue("Speaker").unwrap();
    let st = AppState::new(&cfg).unwrap();
    let addr = spawn(app(st)).await;

    // The speaker connects and says hello: from then on it is known as one.
    let mut req = format!("ws://{addr}/v1/voice/ws").into_client_request().unwrap();
    req.headers_mut()
        .insert("authorization", format!("Bearer {speaker}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    ws.send(Message::text(
        json!({"type": "hello", "now": 1000, "specs": {"chip": "esp32", "fw": "0.4.1"}}).to_string(),
    ))
    .await
    .unwrap();
    let mut h = H {
        addr,
        http: Client::new(),
        data_dir: cfg.data_dir.clone(),
        mock,
        phone,
        owner: String::new(),
        speaker,
        _ws: ws,
        _dir: dir,
    };
    h.owner = h.cookie().await;
    // Telemetry is stored asynchronously: wait until the speaker is known.
    for _ in 0..150 {
        let r = h
            .http
            .get(format!("http://{}/v1/voice/devices", h.addr))
            .bearer_auth(&h.speaker)
            .send()
            .await
            .unwrap();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if v.as_array().is_some_and(|a| !a.is_empty()) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    h
}

impl H {
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    /// The owner's browser session: a cookie (from a device token) and the CSRF header on writes.
    async fn cookie(&self) -> String {
        let r = self
            .http
            .post(self.url("/v1/session"))
            .json(&json!({"token": self.phone}))
            .send()
            .await
            .unwrap();
        r.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }
}

async fn send(rb: reqwest::RequestBuilder) -> (u16, Value) {
    let r = rb.send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

/// `ms` of 16 kHz mono 16-bit audio (a 440 Hz tone) as a PCM WAV file.
fn wav(ms: usize) -> Vec<u8> {
    let pcm: Vec<u8> = (0..16 * ms)
        .flat_map(|i| {
            let x = 8000.0 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin();
            (x as i16).to_le_bytes()
        })
        .collect();
    let mut h = b"RIFF".to_vec();
    h.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    h.extend_from_slice(b"WAVEfmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&[1, 0, 1, 0]);
    h.extend_from_slice(&16_000u32.to_le_bytes());
    h.extend_from_slice(&32_000u32.to_le_bytes());
    h.extend_from_slice(&[2, 0, 16, 0]);
    h.extend_from_slice(b"data");
    h.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    h.extend_from_slice(&pcm);
    h
}

const ROUTES: [(&str, &str); 5] = [
    ("POST", "/v1/voice/learn"),
    ("PATCH", "/v1/voice/learn/vl_1"),
    ("DELETE", "/v1/voice/learn/vl_1"),
    ("POST", "/v1/voice/learn/suggest"),
    ("POST", "/v1/voice/learn/sample"),
];

/// The same request as the owner's browser session (cookie and the CSRF header).
fn as_owner(h: &H, method: &str, path: &str) -> reqwest::RequestBuilder {
    request(h, method, path, None)
        .header("cookie", &h.owner)
        .header("x-secretary-csrf", "1")
}

fn request(h: &H, method: &str, path: &str, token: Option<&str>) -> reqwest::RequestBuilder {
    let m = Method::from_bytes(method.as_bytes()).unwrap();
    let mut rb = h.http.request(m, h.url(path));
    if let Some(t) = token {
        rb = rb.bearer_auth(t);
    }
    match path {
        "/v1/voice/learn/sample" => rb.header("content-type", "audio/wav").body(wav(1000)),
        "/v1/voice/learn/suggest" => {
            rb.json(&json!({"transcript": "ciao skibi gard vej", "corrected": "ciao Skibigårdsvej"}))
        }
        _ if method == "DELETE" => rb,
        _ => rb.json(&json!({"word": "Skibigårdsvej", "soundsLike": ["skibi gård vej"], "lang": "da"})),
    }
}

// -- proxying and the auth matrix -----------------------------------------------------

#[tokio::test]
async fn the_owner_session_reads_and_writes_through_to_lucina() {
    let h = harness().await;
    let o = |m: &str, p: &str| as_owner(&h, m, p);

    let (s, b) = send(o("GET", "/v1/voice/learn")).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["entries"][0]["word"], "Skibigårdsvej");
    assert_eq!(b["max"], 500, "Lucina's data passes through unwrapped");

    let (s, b) = send(o("POST", "/v1/voice/learn")).await;
    assert_eq!((s, b["id"].as_str()), (201, Some("vl_2")), "{b}");

    let (s, b) = send(o("PATCH", "/v1/voice/learn/vl_1")).await;
    assert_eq!(s, 200, "{b}");

    let (s, b) = send(o("DELETE", "/v1/voice/learn/vl_1")).await;
    assert_eq!((s, &b["deleted"]), (200, &json!(true)), "{b}");

    let (s, b) = send(o("POST", "/v1/voice/learn/suggest")).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["candidates"][0]["word"], "Skibigårdsvej");

    let seen = h.mock.seen();
    let sig: Vec<String> = seen.iter().map(|s| format!("{} {}", s.method, s.path)).collect();
    assert_eq!(
        sig,
        [
            "GET /api/secretary/voice-learn",
            "POST /api/secretary/voice-learn",
            "PATCH /api/secretary/voice-learn/vl_1",
            "DELETE /api/secretary/voice-learn/vl_1",
            "POST /api/secretary/voice-learn/suggest",
        ]
    );
    // The server's own Lucina token, never the device's; the body as the app sent it.
    assert!(seen.iter().all(|s| s.auth.as_deref() == Some("Bearer lucina-server-secret")));
    assert_eq!(seen[1].body["word"], "Skibigårdsvej");
    assert_eq!(seen[1].body["lang"], "da");
    assert_eq!(seen[4].body, json!({"transcript": "ciao skibi gard vej", "corrected": "ciao Skibigårdsvej"}));
}

#[tokio::test]
async fn the_owner_session_may_write_with_the_csrf_header() {
    let h = harness().await;
    let cookie = h.cookie().await;
    let (s, b) = send(h.http.get(h.url("/v1/voice/learn")).header("cookie", &cookie)).await;
    assert_eq!(s, 200, "{b}");
    // A cookie write without the CSRF header is refused before it reaches Lucina.
    let (s, b) = send(request(&h, "POST", "/v1/voice/learn", None).header("cookie", &cookie)).await;
    assert_eq!((s, code(&b)), (403, "csrf"));
    let (s, b) = send(
        request(&h, "POST", "/v1/voice/learn", None)
            .header("cookie", &cookie)
            .header("x-secretary-csrf", "1"),
    )
    .await;
    assert_eq!((s, b["id"].as_str()), (201, Some("vl_2")), "{b}");
    let (s, _) = send(
        request(&h, "PATCH", "/v1/voice/learn/vl_1", None)
            .header("cookie", &cookie)
            .header("x-secretary-csrf", "1"),
    )
    .await;
    assert_eq!(s, 200);
}

#[tokio::test]
async fn a_device_token_that_never_connected_may_only_read() {
    // The phone app, or a speaker paired a moment ago that has not said hello: no write is possible
    // with a device token, whatever it is; only the owner's session does the owner things.
    let h = harness().await;
    let t = Some(h.phone.as_str());
    let (s, b) = send(request(&h, "GET", "/v1/voice/learn", t)).await;
    assert_eq!(s, 200, "{b}");
    for (method, path) in ROUTES {
        let (s, b) = send(request(&h, method, path, t)).await;
        assert_eq!((s, code(&b)), (403, "owner_only"), "{method} {path}: {b}");
        // The same request from the owner's session goes through.
        let (s, b) = send(as_owner(&h, method, path)).await;
        assert!(s == 200 || s == 201, "{method} {path}: {s} {b}");
    }
    // Lucina saw the read and the five owner writes; the device's five never reached it.
    assert_eq!(h.mock.seen().len(), 1 + 4);
    assert_eq!(h.mock.stt_calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_speaker_may_only_read() {
    let h = harness().await;
    let t = Some(h.speaker.as_str());
    let (s, b) = send(request(&h, "GET", "/v1/voice/learn", t)).await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(h.mock.seen().len(), 1);
    for (method, path) in ROUTES {
        let (s, b) = send(request(&h, method, path, t)).await;
        assert_eq!((s, code(&b)), (403, "owner_only"), "{method} {path}: {b}");
    }
    // Nothing but the read reached Lucina or the speech-to-text engine.
    assert_eq!(h.mock.seen().len(), 1);
    assert!(h.mock.stt_calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn no_credentials_is_401_everywhere() {
    let h = harness().await;
    for (method, path) in [("GET", "/v1/voice/learn")].into_iter().chain(ROUTES) {
        let (s, b) = send(request(&h, method, path, None)).await;
        assert_eq!((s, code(&b)), (401, "unauthorized"), "{method} {path}: {b}");
    }
    let (s, _) = send(request(&h, "GET", "/v1/voice/learn", Some("sct_notatoken"))).await;
    assert_eq!(s, 401);
    assert!(h.mock.seen().is_empty());
}

#[tokio::test]
async fn lucinas_validation_errors_pass_through_and_bad_input_stops_here() {
    let h = harness().await;
    // Lucina's own 400 reaches the app with its code and message (shown in the form).
    let (s, b) = send(
        h.http
            .post(h.url("/v1/voice/learn"))
            .header("cookie", &h.owner).header("x-secretary-csrf", "1")
            .json(&json!({"word": ""})),
    )
    .await;
    assert_eq!((s, code(&b)), (400, "invalid_word"), "{b}");
    assert_eq!(b["error"]["message"], "word is required");
    // An unknown id is Lucina's 404 (not the "route missing" 502).
    let (s, b) = send(as_owner(&h, "DELETE", "/v1/voice/learn/vl_9")).await;
    assert_eq!((s, code(&b)), (404, "entry_not_found"), "{b}");

    let before = h.mock.seen().len();
    // Not an object, a path-walking id, a bad suggest body: refused here.
    let (s, b) = send(h.http.post(h.url("/v1/voice/learn")).header("cookie", &h.owner).header("x-secretary-csrf", "1").json(&json!(["x"]))).await;
    assert_eq!((s, code(&b)), (400, "bad_request"), "{b}");
    let (s, _) = send(h.http.delete(h.url("/v1/voice/learn/a%2Fb")).header("cookie", &h.owner).header("x-secretary-csrf", "1")).await;
    assert_eq!(s, 400);
    for body in [
        json!({"transcript": "x"}),
        json!({"transcript": "x", "corrected": "y", "extra": 1}),
        json!({"transcript": "  ", "corrected": "y"}),
    ] {
        let (s, b) = send(h.http.post(h.url("/v1/voice/learn/suggest")).header("cookie", &h.owner).header("x-secretary-csrf", "1").json(&body)).await;
        assert_eq!(s, 400, "{body}: {b}");
    }
    assert_eq!(h.mock.seen().len(), before);
}

// -- /v1/voice/learn/sample -----------------------------------------------------------

#[tokio::test]
async fn a_sample_returns_what_the_engine_heard_and_stores_nothing() {
    let h = harness().await;
    *h.mock.corrections.lock().unwrap() = json!([{"from": "skibi gard", "to": "Skibigårdsvej"}]);
    let audio = wav(1500);
    let (s, b) = send(
        h.http
            .post(h.url("/v1/voice/learn/sample?lang=da"))
            .header("cookie", &h.owner).header("x-secretary-csrf", "1")
            .header("content-type", "audio/wav")
            .body(audio.clone()),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(b["heard"], "skibi gard vej");
    assert_eq!(b["corrections"], json!([{"from": "skibi gard", "to": "Skibigårdsvej"}]));
    // The engine got the clip with the owner session's own credentials, as WAV, whole.
    let calls = h.mock.stt_calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0.as_deref(), Some(h.owner.as_str()));
    assert_eq!(calls[0].1, "audio/wav");
    assert_eq!(calls[0].2, audio.len());
    // Nothing was sent to Lucina and nothing was written to the data directory.
    assert!(h.mock.seen().is_empty());
    let leftovers: Vec<_> = walk(&h.data_dir)
        .into_iter()
        .filter(|p| {
            let n = p.to_string_lossy().to_lowercase();
            n.contains("sample") || n.ends_with(".wav") || n.ends_with(".pcm")
        })
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // Raw 16 kHz PCM works too.
    let pcm = audio[44..].to_vec();
    let (s, b) = send(
        h.http
            .post(h.url("/v1/voice/learn/sample"))
            .header("cookie", &h.owner).header("x-secretary-csrf", "1")
            .header("content-type", "audio/x-s16le;rate=16000")
            .body(pcm),
    )
    .await;
    assert_eq!(s, 200, "{b}");
    assert_eq!(h.mock.stt_calls.lock().unwrap()[1].1, "audio/x-s16le;rate=16000");
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[tokio::test]
async fn a_sample_is_limited_and_validated() {
    let h = harness().await;
    let post = |ct: &'static str, body: Vec<u8>| {
        h.http
            .post(h.url("/v1/voice/learn/sample"))
            .header("cookie", &h.owner).header("x-secretary-csrf", "1")
            .header("content-type", ct)
            .body(body)
    };
    // 10 s is fine; a bit over is invalid_audio; a lot over is stopped by the size limit.
    let (s, b) = send(post("audio/wav", wav(10_000))).await;
    assert_eq!(s, 200, "{b}");
    let long_pcm = wav(10_300)[44..].to_vec();
    let (s, b) = send(post("audio/x-s16le;rate=16000", long_pcm)).await;
    assert_eq!((s, code(&b)), (400, "invalid_audio"), "{b}");
    let (s, b) = send(post("audio/wav", wav(20_000))).await;
    assert_eq!((s, code(&b)), (413, "payload_too_large"), "{b}");
    // Too short, empty, not audio we take, not a WAV.
    let (s, b) = send(post("audio/wav", wav(100))).await;
    assert_eq!((s, code(&b)), (422, "no_speech"), "{b}");
    let (s, b) = send(post("audio/wav", vec![])).await;
    assert_eq!((s, code(&b)), (422, "no_speech"), "{b}");
    let (s, b) = send(post("audio/webm", wav(1000))).await;
    assert_eq!((s, code(&b)), (415, "unsupported_media_type"), "{b}");
    let (s, b) = send(post("audio/wav", b"not a wav file at all".to_vec())).await;
    assert_eq!((s, code(&b)), (400, "invalid_audio"), "{b}");
    let (s, _) = send(
        h.http
            .post(h.url("/v1/voice/learn/sample?lang=fr"))
            .header("cookie", &h.owner).header("x-secretary-csrf", "1")
            .header("content-type", "audio/wav")
            .body(wav(1000)),
    )
    .await;
    assert_eq!(s, 400, "an unknown language");
}

#[tokio::test]
async fn silence_and_a_missing_engine_are_reported() {
    let h = harness().await;
    let post = || {
        h.http
            .post(h.url("/v1/voice/learn/sample"))
            .header("cookie", &h.owner).header("x-secretary-csrf", "1")
            .header("content-type", "audio/wav")
            .body(wav(1000))
    };
    *h.mock.heard.lock().unwrap() = "  ".into();
    let (s, b) = send(post()).await;
    assert_eq!((s, code(&b)), (422, "no_speech"), "{b}");
    // The local engine is down and Gemini is not set up: the app is told, nothing is invented.
    *h.mock.stt_fails.lock().unwrap() = true;
    let (s, b) = send(post()).await;
    assert_eq!((s, code(&b)), (503, "transcription_unavailable"), "{b}");
}

#[tokio::test]
async fn samples_are_rate_limited_per_device() {
    let h = harness().await;
    let mut limited = None;
    for i in 0..40 {
        let r = h
            .http
            .post(h.url("/v1/voice/learn/sample"))
            .header("cookie", &h.owner).header("x-secretary-csrf", "1")
            .header("content-type", "audio/wav")
            .body(wav(500))
            .send()
            .await
            .unwrap();
        if r.status() == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(i);
            break;
        }
    }
    assert!(limited.is_some_and(|i| i >= 15), "{limited:?}");
}
