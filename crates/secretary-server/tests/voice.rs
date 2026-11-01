//! `POST /v1/voice`: audio in, Lucina's spoken answer out, with mock
//! Gemini, Lucina and Kokoro upstreams.
//!
//! The mock Gemini serves the whole test process (the server reads its
//! address from the environment on every call), so it picks the transcript
//! from the audio: the first sample of the recording is the script number.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI16, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode as AxumStatus, Uri};
use axum::response::IntoResponse;
use axum::{Json, Router};
use base64::Engine;
use futures_util::stream;
use reqwest::{Client, StatusCode};
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};

const GEMINI_KEY: &str = "test-gemini-key";
const LUCINA_TOKEN: &str = "lucina-server-secret";
/// What the mock Kokoro returns for each chunk of text.
const KOKORO_PCM: &[u8] = &[1, 0, 2, 0, 3, 0, 4, 0, 5, 0, 6, 0];

// Scripts: the first sample of the recording.
const HELLO: i16 = 1;
const SILENCE: i16 = 2;
const ENGLISH: i16 = 3;
const SECRET: i16 = 4;
const EMPTY_REPLY: i16 = 5;
const LONG_REPLY: i16 = 6;
const ITALIAN_SECRET: i16 = 7;
const SKIBI: i16 = 8;
const MIKEL: i16 = 9;

fn transcript(script: i16) -> &'static str {
    match script {
        HELLO => "Ciao Lucina, come stai oggi? Perché non rispondi?",
        SILENCE => "   ",
        ENGLISH => "What is the time today?",
        SECRET => "Read me my verification code",
        EMPTY_REPLY => "Please say nothing to me today",
        LONG_REPLY => "Tell me a very long story",
        ITALIAN_SECRET => "Leggimi il codice di verifica che è arrivato",
        SKIBI => "Skibi gaardsvej",
        MIKEL => "Ciao Mikel",
        _ => "unknown",
    }
}

// -- mock Gemini (one per process) --------------------------------------------

#[derive(Debug, Clone)]
struct GeminiCall {
    path: String,
    key: Option<String>,
    body: Value,
}

struct Gemini {
    /// By (script, nonce): the first two samples of the recording.
    calls: Mutex<HashMap<(i16, i16), Vec<GeminiCall>>>,
}

async fn gemini_handler(
    State(g): State<Arc<Gemini>>,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let data = body["contents"][0]["parts"][0]["inlineData"]["data"]
        .as_str()
        .unwrap_or("");
    let wav = base64::engine::general_purpose::STANDARD
        .decode(data)
        .unwrap_or_default();
    let sample = |i: usize| {
        wav.get(44 + 2 * i..46 + 2 * i)
            .map_or(0, |s| i16::from_le_bytes([s[0], s[1]]))
    };
    let (script, nonce) = (sample(0), sample(1));
    g.calls
        .lock()
        .unwrap()
        .entry((script, nonce))
        .or_default()
        .push(GeminiCall {
            path: uri.path().to_owned(),
            key: headers
                .get("x-goog-api-key")
                .map(|v| v.to_str().unwrap().to_owned()),
            body,
        });
    Json(json!({"candidates": [{
        "content": {"parts": [{"text": transcript(script)}]},
        "finishReason": "STOP"
    }]}))
}

/// Starts the shared mock Gemini and points the environment at it.
fn gemini() -> Arc<Gemini> {
    static G: OnceLock<Arc<Gemini>> = OnceLock::new();
    G.get_or_init(|| {
        let g = Arc::new(Gemini {
            calls: Mutex::new(HashMap::new()),
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let state = g.clone();
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    tx.send(listener.local_addr().unwrap()).unwrap();
                    let router = Router::new().fallback(gemini_handler).with_state(state);
                    axum::serve(listener, router).await.unwrap();
                });
        });
        let addr = rx.recv().unwrap();
        let dir = tempfile::tempdir().unwrap().keep();
        let key = dir.join("gemini.key");
        std::fs::write(&key, format!("{GEMINI_KEY}\n")).unwrap();
        std::env::set_var("SECRETARY_GEMINI_KEY_FILE", &key);
        std::env::set_var("SECRETARY_TRANSCRIPTION_URL", format!("http://{addr}"));
        std::env::set_var("SECRETARY_TRANSCRIPTION_MODEL", "gemini-test-model");
        g
    })
    .clone()
}

impl Gemini {
    fn calls(&self, script: i16, nonce: i16) -> Vec<GeminiCall> {
        self.calls
            .lock()
            .unwrap()
            .get(&(script, nonce))
            .cloned()
            .unwrap_or_default()
    }
}

// -- mock Lucina and Kokoro (one pair per harness) ------------------------------

#[derive(Default)]
struct Mock {
    chats: Mutex<Vec<Value>>,
    speech: Mutex<Vec<Value>>,
    stt_calls: Mutex<Vec<SttCall>>,
    stt_mode: Mutex<SttMode>,
    /// `GET /api/secretary/voice-learn` requests seen.
    learn_gets: Mutex<u32>,
    /// Lucina's Voice Learn list answers 500.
    learn_fails: Mutex<bool>,
    /// The list also holds Mikkel and Michel (two words close to "Mikel").
    learn_twins: Mutex<bool>,
}

/// What the mock local speech-to-text does.
#[derive(Default, Clone, Copy)]
enum SttMode {
    /// 200 with the script's transcript.
    #[default]
    Ok,
    /// 200 with empty text.
    Empty,
    /// 200 with the script's transcript and Voice Learn facts.
    Corrected,
    Fail500,
    /// lucina-memo's 503 `stt_unavailable`.
    Unavailable,
}

#[derive(Debug, Clone)]
struct SttCall {
    auth: Option<String>,
    cookie: Option<String>,
    csrf: Option<String>,
    content_type: String,
    query: Option<String>,
    len: usize,
}

async fn local_stt(
    State(m): State<Arc<Mock>>,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let h = |n: &str| headers.get(n).map(|v| v.to_str().unwrap().to_owned());
    let content_type = h("content-type").unwrap_or_default();
    m.stt_calls.lock().unwrap().push(SttCall {
        auth: h("authorization"),
        cookie: h("cookie"),
        csrf: h("x-secretary-csrf"),
        content_type: content_type.clone(),
        query: uri.query().map(str::to_owned),
        len: body.len(),
    });
    let at = if content_type.starts_with("audio/wav") {
        44
    } else {
        0
    };
    let script = body
        .get(at..at + 2)
        .map_or(0, |s| i16::from_le_bytes([s[0], s[1]]));
    let mode = *m.stt_mode.lock().unwrap();
    match mode {
        SttMode::Ok | SttMode::Empty => {
            let text = if matches!(mode, SttMode::Empty) {
                ""
            } else {
                transcript(script)
            };
            Json(json!({"text": text, "language": "it", "ms": 5, "mode": "gpu"})).into_response()
        }
        SttMode::Corrected => Json(json!({
            "text": transcript(script), "language": "it", "ms": 5, "mode": "gpu",
            "corrections": [{"from": "stai", "to": "Stay"}, {"from": "oggi", "to": "Oggi"}],
            "ambiguous": [{"heard": "come", "candidates": ["Kom", "Komme"]}]
        }))
        .into_response(),
        SttMode::Fail500 => (AxumStatus::INTERNAL_SERVER_ERROR, "boom").into_response(),
        SttMode::Unavailable => (
            AxumStatus::SERVICE_UNAVAILABLE,
            Json(json!({"error": {"code": "stt_unavailable", "message": "no engine"}})),
        )
            .into_response(),
    }
}

async fn lucina(
    State(m): State<Arc<Mock>>,
    method: axum::http::Method,
    uri: Uri,
    body: Bytes,
) -> axum::response::Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    if uri.path() == "/api/secretary/voice-learn" && method == axum::http::Method::GET {
        *m.learn_gets.lock().unwrap() += 1;
        if *m.learn_fails.lock().unwrap() {
            return (AxumStatus::INTERNAL_SERVER_ERROR, "boom").into_response();
        }
        let mut entries = vec![
            json!({"id": "vl_1", "word": "Lucina", "updatedAt": "2026-11-01T10:00:00Z"}),
            json!({"id": "vl_2", "word": "Skibigårdsvej", "soundsLike": ["skibi gård vej"], "updatedAt": "2026-11-01T11:00:00Z"}),
        ];
        if *m.learn_twins.lock().unwrap() {
            entries.push(json!({"id": "vl_3", "word": "Mikkel", "updatedAt": "2026-11-01T09:00:00Z"}));
            entries.push(json!({"id": "vl_4", "word": "Michel", "updatedAt": "2026-11-01T08:00:00Z"}));
        }
        return Json(json!({"ok": true, "data": {"entries": entries}})).into_response();
    }
    if uri.path() != "/api/secretary/chat" {
        return (AxumStatus::NOT_FOUND, "no").into_response();
    }
    m.chats.lock().unwrap().push(body.clone());
    let msg = body["message"].as_str().unwrap_or("");
    let data = if msg == transcript(SECRET) || msg == transcript(ITALIAN_SECRET) {
        json!({"reply": "Your code is 482913.", "sensitive": true})
    } else if msg == transcript(EMPTY_REPLY) {
        json!({"reply": "  "})
    } else if msg == transcript(LONG_REPLY) {
        json!({"reply": "È una lunga storia che continua senza fine. ".repeat(80)})
    } else if msg == transcript(ENGLISH) {
        json!({"reply": "It is **noon**, sir."})
    } else {
        json!({"reply": "Sto bene, grazie! E tu?"})
    };
    Json(json!({"ok": true, "data": data})).into_response()
}

async fn kokoro(State(m): State<Arc<Mock>>, body: Bytes) -> axum::response::Response {
    m.speech
        .lock()
        .unwrap()
        .push(serde_json::from_slice(&body).unwrap_or(Value::Null));
    // Two chunks, to prove the response is a stream of pieces.
    let (a, b) = KOKORO_PCM.split_at(5);
    let s = stream::iter([
        Ok::<_, std::io::Error>(Bytes::copy_from_slice(a)),
        Ok(Bytes::copy_from_slice(b)),
    ]);
    (
        [("content-type", "audio/pcm")],
        axum::body::Body::from_stream(s),
    )
        .into_response()
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
    device_id: String,
    /// Marks this test's recordings in the shared mock Gemini.
    nonce: i16,
    mock: Arc<Mock>,
    gemini: Arc<Gemini>,
    _dir: tempfile::TempDir,
}

/// Where the server looks for local speech-to-text.
#[derive(Clone, Copy, PartialEq)]
enum Local {
    /// Not configured: Gemini only.
    Off,
    /// The mock above.
    Mock,
    /// A port nothing listens on.
    Dead,
}

async fn harness() -> H {
    harness_with(Local::Off).await
}

async fn harness_with(local: Local) -> H {
    let gemini = gemini();
    let mock = Arc::new(Mock::default());
    let lucina_url = spawn(
        Router::new().fallback(lucina).with_state(mock.clone()),
        false,
    )
    .await;
    let tts = spawn(
        Router::new().fallback(kokoro).with_state(mock.clone()),
        false,
    )
    .await;
    let stt = spawn(
        Router::new().fallback(local_stt).with_state(mock.clone()),
        false,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(lucina_url, Some(LUCINA_TOKEN.into()));
    cfg.data_dir = dir.path().join("state");
    cfg.tts_url = format!("{tts}/");
    cfg.voice_stt_url = match local {
        Local::Off => None,
        Local::Mock => Some(format!("{stt}/v1/memos/stt")),
        Local::Dead => Some("http://127.0.0.1:9/v1/memos/stt".into()),
    };
    let (dev, token) = DeviceStore::new(&cfg.data_dir).issue("Speaker").unwrap();
    let base = spawn(app(AppState::new(&cfg).unwrap()), true).await;
    H {
        base,
        http: Client::new(),
        token,
        device_id: dev.id,
        nonce: NONCE.fetch_add(1, Ordering::SeqCst),
        mock,
        gemini,
        _dir: dir,
    }
}

/// `seconds` of 16 kHz mono 16-bit audio: samples alternate `script`, `nonce`.
fn pcm(seconds: f64, script: i16, nonce: i16) -> Vec<u8> {
    let n = (seconds * 16_000.0) as usize / 2;
    [script.to_le_bytes(), nonce.to_le_bytes()]
        .concat()
        .repeat(n)
}

/// The same audio as a WAV file with the canonical 44-byte header.
fn wav(pcm: &[u8]) -> Vec<u8> {
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
    h.extend_from_slice(pcm);
    h
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

static NONCE: AtomicI16 = AtomicI16::new(100);

const RAW: &str = "audio/x-s16le;rate=16000";

impl H {
    fn req(&self, query: &str, content_type: &str, body: reqwest::Body) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}/v1/voice{query}", self.base))
            .bearer_auth(&self.token)
            .header("content-type", content_type)
            .body(body)
    }

    /// The recording, sent in 4 KiB pieces with `Transfer-Encoding: chunked`.
    fn chunked(&self, query: &str, audio: Vec<u8>) -> reqwest::RequestBuilder {
        let pieces: Vec<_> = audio
            .chunks(4096)
            .map(|c| Ok::<_, std::io::Error>(Bytes::copy_from_slice(c)))
            .collect();
        self.req(query, RAW, reqwest::Body::wrap_stream(stream::iter(pieces)))
    }

    async fn say(&self, query: &str, script: i16) -> reqwest::Response {
        self.chunked(query, pcm(1.0, script, self.nonce))
            .send()
            .await
            .unwrap()
    }
}

fn header(r: &reqwest::Response, name: &str) -> String {
    percent_decode(r.headers()[name].to_str().unwrap())
}

async fn error_code(r: reqwest::Response) -> String {
    let v: Value = r.json().await.unwrap();
    v["error"]["code"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_chunked_pcm_turn_returns_the_spoken_reply() {
    let h = harness().await;
    let audio = pcm(1.0, HELLO, h.nonce);
    let r = h.chunked("", audio.clone()).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["content-type"], "audio/x-s16le;rate=24000");
    assert_eq!(r.headers()["x-tts-voice"], "if_sara");
    assert_eq!(header(&r, "x-lucina-transcript"), transcript(HELLO));
    // The reply is not known when the headers go out: no X-Lucina-Reply.
    assert!(!r.headers().contains_key("x-lucina-reply"));
    // One Kokoro request per sentence, in order; their PCM, back to back.
    assert_eq!(r.bytes().await.unwrap().as_ref(), KOKORO_PCM.repeat(2));

    // Gemini: the PCM was wrapped into a WAV, plain transcript, the key.
    let calls = h.gemini.calls(HELLO, h.nonce);
    assert_eq!(calls.len(), 1);
    let c = &calls[0];
    assert_eq!(c.path, "/v1beta/models/gemini-test-model:generateContent");
    assert_eq!(c.key.as_deref(), Some(GEMINI_KEY));
    let part = &c.body["contents"][0]["parts"][0]["inlineData"];
    assert_eq!(part["mimeType"], "audio/wav");
    let sent = base64::engine::general_purpose::STANDARD
        .decode(part["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(sent, wav(&audio));
    assert_eq!(
        c.body["generationConfig"]["audioTranscriptionConfig"]["diarization"],
        false
    );

    // Lucina: the same request /v1/chat forwards.
    let chats = h.mock.chats.lock().unwrap().clone();
    assert_eq!(chats.len(), 1);
    let chat = &chats[0];
    assert_eq!(chat["message"], transcript(HELLO));
    assert_eq!(chat["sessionId"], "lucinavoice");
    // Every voice turn is the shared "Lucina Voice" thread (protocol section 7), with
    // the speech-to-text facts for its `meta.voice`: the turn id is the client message id.
    assert_eq!(chat["channel"], "voice");
    assert_eq!(chat["voiceMeta"]["turnId"], chat["clientMessageId"]);
    assert!(chat["voiceMeta"]["ms"]["stt"].is_u64(), "{}", chat["voiceMeta"]);
    assert!(chat["voiceMeta"].get("ambiguous").is_none());
    assert_eq!(chat["device"], h.device_id.as_str());
    assert_eq!(chat["deviceName"], "Speaker");
    // A streaming, spoken-style turn.
    assert_eq!(chat["voice"], true);
    let id = chat["clientMessageId"].as_str().unwrap();
    assert!(
        id.starts_with("voice-") && id.split('-').count() == 3,
        "{id}"
    );

    // Kokoro: raw PCM, Italian voice, one sentence per request.
    let speech = h.mock.speech.lock().unwrap().clone();
    assert_eq!(speech.len(), 2);
    assert!(speech
        .iter()
        .all(|s| s["response_format"] == "pcm" && s["voice"] == "if_sara"));
    assert_eq!(speech[0]["input"], "Sto bene, grazie!");
    assert_eq!(speech[1]["input"], "E tu?");
}

#[tokio::test]
async fn the_local_engines_voice_learn_facts_reach_lucina_as_voice_meta() {
    let h = harness_with(Local::Mock).await;
    *h.mock.stt_mode.lock().unwrap() = SttMode::Corrected;
    let r = h.say("", HELLO).await;
    assert_eq!(r.status(), StatusCode::OK);
    let _ = r.bytes().await.unwrap();
    let chats = h.mock.chats.lock().unwrap().clone();
    let meta = &chats[0]["voiceMeta"];
    // The doubtful spans feed Lucina's vocab layer; the replaced words stay here.
    assert_eq!(meta["ambiguous"], json!([{"heard": "come", "candidates": ["Kom", "Komme"]}]));
    assert!(meta.get("corrections").is_none());
    assert_eq!(chats[0]["channel"], "voice");
    // The local engine answered: Gemini and the vocabulary list were never asked.
    assert_eq!(*h.mock.learn_gets.lock().unwrap(), 0);
    assert!(h.gemini.calls(HELLO, h.nonce).is_empty());
}

#[tokio::test]
async fn the_gemini_fallback_gets_a_vocabulary_line_cached_for_a_minute() {
    let h = harness().await; // no local engine: Gemini
    for _ in 0..2 {
        let r = h.say("", HELLO).await;
        assert_eq!(r.status(), StatusCode::OK);
        let _ = r.bytes().await.unwrap();
    }
    let calls = h.gemini.calls(HELLO, h.nonce);
    assert_eq!(calls.len(), 2);
    for c in &calls {
        let parts = c.body["contents"][0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(parts[0]["inlineData"].is_object());
        // newest entry first, with what it sounds like
        assert_eq!(
            parts[1]["text"],
            "Vocabulary: Skibigårdsvej (sounds like skibi gård vej); Lucina"
        );
    }
    // Fetched once for both turns (60 s cache), with the server's token.
    assert_eq!(*h.mock.learn_gets.lock().unwrap(), 1);
}

#[tokio::test]
async fn the_gemini_transcript_is_post_corrected_with_the_vocabulary() {
    let h = harness().await;
    let r = h.say("", SKIBI).await;
    assert_eq!(r.status(), StatusCode::OK);
    // Gemini wrote "Skibi gaardsvej"; Lucina and the device hear the entry's spelling.
    assert_eq!(header(&r, "x-lucina-transcript"), "Skibigårdsvej");
    let _ = r.bytes().await.unwrap();
    assert_eq!(h.gemini.calls(SKIBI, h.nonce).len(), 1);
    let chats = h.mock.chats.lock().unwrap().clone();
    assert_eq!(chats[0]["message"], "Skibigårdsvej");
    // Nothing doubtful: no ambiguous spans in voiceMeta (and the corrections stay here).
    assert!(chats[0]["voiceMeta"].get("ambiguous").is_none());
    assert!(chats[0]["voiceMeta"].get("corrections").is_none());
}

#[tokio::test]
async fn ambiguous_spans_of_the_gemini_transcript_go_to_lucina_like_the_local_ones() {
    let h = harness().await;
    *h.mock.learn_twins.lock().unwrap() = true;
    let r = h.say("", MIKEL).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(header(&r, "x-lucina-transcript"), "Ciao Mikel", "left as Gemini wrote it");
    let _ = r.bytes().await.unwrap();
    let chats = h.mock.chats.lock().unwrap().clone();
    assert_eq!(chats[0]["message"], "Ciao Mikel");
    assert_eq!(
        chats[0]["voiceMeta"]["ambiguous"],
        json!([{"heard": "Mikel", "candidates": ["Mikkel", "Michel"]}])
    );
}

#[tokio::test]
async fn the_gemini_fallback_fails_open_when_the_vocabulary_is_unavailable() {
    let h = harness().await;
    *h.mock.learn_fails.lock().unwrap() = true;
    let r = h.say("", SKIBI).await;
    assert_eq!(r.status(), StatusCode::OK);
    // No vocabulary: no prompt line and the transcript is exactly what Gemini wrote.
    assert_eq!(header(&r, "x-lucina-transcript"), transcript(SKIBI));
    let _ = r.bytes().await.unwrap();
    let calls = h.gemini.calls(SKIBI, h.nonce);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].body["contents"][0]["parts"].as_array().unwrap().len(), 1);
    assert_eq!(*h.mock.learn_gets.lock().unwrap(), 1);
}

#[tokio::test]
async fn a_wav_body_is_accepted_and_sent_as_is() {
    let h = harness().await;
    let file = wav(&pcm(0.8, ENGLISH, h.nonce));
    let r = h
        .req("", "audio/wav", file.clone().into())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(header(&r, "x-lucina-transcript"), transcript(ENGLISH));
    // Markdown marks are not read aloud, and the voice is English.
    assert_eq!(r.headers()["x-tts-voice"], "af_heart");
    assert_eq!(r.bytes().await.unwrap().as_ref(), KOKORO_PCM);
    let calls = h.gemini.calls(ENGLISH, h.nonce);
    let data = calls[0].body["contents"][0]["parts"][0]["inlineData"]["data"]
        .as_str()
        .unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap(),
        file
    );
    let speech = h.mock.speech.lock().unwrap().clone();
    assert_eq!(speech[0]["input"], "It is noon, sir.");
}

#[tokio::test]
async fn lang_picks_the_voice() {
    let h = harness().await;
    let r = h.say("?lang=en", HELLO).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["x-tts-voice"], "af_heart");
    let r = h.say("?lang=it-IT", ENGLISH).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["x-tts-voice"], "if_sara");
    let r = h.say("?lang=fr", HELLO).await;
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(r).await, "bad_request");
}

#[tokio::test]
async fn too_little_audio_is_no_speech() {
    let h = harness().await;
    // Empty, and 0.25 s (under the 0.3 s minimum), raw and WAV.
    for (ct, body) in [
        (RAW, vec![]),
        (RAW, pcm(0.25, HELLO, h.nonce)),
        (RAW, vec![1]),
        ("audio/wav", vec![]),
        ("audio/wav", wav(&pcm(0.25, HELLO, h.nonce))),
    ] {
        let r = h.req("", ct, body.clone().into()).send().await.unwrap();
        assert_eq!(
            r.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{ct} {}",
            body.len()
        );
        assert_eq!(error_code(r).await, "no_speech");
    }
    // Just over the minimum goes through.
    let r = h
        .chunked("", pcm(0.31, HELLO, h.nonce))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert!(h.mock.chats.lock().unwrap().len() == 1);
}

#[tokio::test]
async fn an_empty_transcript_is_no_speech_and_never_reaches_lucina() {
    let h = harness().await;
    let r = h.say("", SILENCE).await;
    assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error_code(r).await, "no_speech");
    assert_eq!(h.gemini.calls(SILENCE, h.nonce).len(), 1);
    assert!(h.mock.chats.lock().unwrap().is_empty());
    assert!(h.mock.speech.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_sensitive_reply_is_replaced_by_a_fixed_phrase() {
    let h = harness().await;
    // English transcript: English phrase and voice.
    let r = h.say("", SECRET).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["x-tts-voice"], "af_heart");
    assert_eq!(r.bytes().await.unwrap().as_ref(), KOKORO_PCM);
    // Italian transcript, and `lang=it` for an English one.
    let r = h.say("", ITALIAN_SECRET).await;
    assert_eq!(r.headers()["x-tts-voice"], "if_sara");
    let _ = r.bytes().await.unwrap();
    let r = h.say("?lang=it", SECRET).await;
    let _ = r.bytes().await.unwrap();

    let spoken: Vec<String> = h
        .mock
        .speech
        .lock()
        .unwrap()
        .iter()
        .map(|s| s["input"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        spoken,
        [
            "I'll answer that in the app.",
            "Ti rispondo nell'app.",
            "Ti rispondo nell'app."
        ]
    );
    assert!(spoken.iter().all(|s| !s.contains("482913")));
}

#[tokio::test]
async fn an_empty_reply_gets_a_short_fallback() {
    let h = harness().await;
    let r = h.say("", EMPTY_REPLY).await;
    assert_eq!(r.status(), StatusCode::OK);
    let _ = r.bytes().await.unwrap();
    let r = h.say("?lang=it", EMPTY_REPLY).await;
    let _ = r.bytes().await.unwrap();
    let spoken: Vec<String> = h
        .mock
        .speech
        .lock()
        .unwrap()
        .iter()
        .map(|s| s["input"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        spoken,
        [
            "Sorry, I don't have an answer.",
            "Scusa, non ho una risposta."
        ]
    );
}

#[tokio::test]
async fn long_replies_are_cut_for_speech() {
    let h = harness().await;
    let r = h.say("", LONG_REPLY).await;
    assert_eq!(r.status(), StatusCode::OK);
    // Kokoro gets one sentence per request, 1200 characters in all at most,
    // made as the stream is read and streamed back to back.
    let body = r.bytes().await.unwrap();
    let speech = h.mock.speech.lock().unwrap().clone();
    assert!(speech.len() >= 4, "{}", speech.len());
    let total: usize = speech
        .iter()
        .map(|s| s["input"].as_str().unwrap().chars().count())
        .sum();
    assert!(total <= 1200 && total > 1100, "{total}");
    assert!(speech
        .iter()
        .all(|s| s["response_format"] == "pcm" && s["input"].as_str().unwrap().ends_with('.')));
    assert_eq!(body.as_ref(), KOKORO_PCM.repeat(speech.len()));
}

#[tokio::test]
async fn it_needs_a_device_and_the_right_media_type() {
    let h = harness().await;
    let url = format!("{}/v1/voice", h.base);
    let audio = pcm(1.0, HELLO, h.nonce);

    let r = h
        .http
        .post(&url)
        .header("content-type", RAW)
        .body(audio.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let r = h
        .http
        .post(&url)
        .bearer_auth("sec_notarealtoken")
        .header("content-type", RAW)
        .body(audio.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    assert!(h.gemini.calls(HELLO, h.nonce).is_empty());

    for ct in ["application/json", "audio/mpeg", "audio/x-s16le;rate=44100"] {
        let r = h.req("", ct, audio.clone().into()).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE, "{ct}");
        assert_eq!(error_code(r).await, "unsupported_media_type");
    }
    // Not a WAV although it says so.
    let r = h
        .req("", "audio/wav", audio.clone().into())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(r).await, "invalid_audio");
    assert!(h.gemini.calls(HELLO, h.nonce).is_empty());
}

#[tokio::test]
async fn bodies_over_one_mib_are_refused() {
    let h = harness().await;
    // Known length, and chunked (no Content-Length to check up front).
    let big = pcm(32.8, HELLO, h.nonce)
        .into_iter()
        .chain(std::iter::repeat_n(0u8, 2))
        .collect::<Vec<u8>>();
    assert!(big.len() > 1024 * 1024 && big.len() < 1024 * 1024 + 4096);
    let r = h.req("", RAW, big.clone().into()).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let r = h.chunked("", big).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error_code(r).await, "payload_too_large");
    // 32 s (just under 1 MiB) is still fine.
    let r = h
        .chunked("", pcm(32.0, HELLO, h.nonce))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
}

#[tokio::test]
async fn voice_turns_have_their_own_rate_limit() {
    let h = harness().await;
    for i in 0..20 {
        let r = h
            .req("", RAW, Vec::<u8>::new().into())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY, "turn {i}");
    }
    let r = h
        .req("", RAW, Vec::<u8>::new().into())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(r.headers().contains_key("retry-after"));
    assert_eq!(error_code(r).await, "rate_limited");
    // /v1/tts has its own bucket and is unaffected.
    let r = h
        .http
        .post(format!("{}/v1/tts", h.base))
        .bearer_auth(&h.token)
        .json(&json!({"text": "Ciao"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
}

#[tokio::test]
async fn tts_still_asks_for_mp3() {
    let h = harness().await;
    let r = h
        .http
        .post(format!("{}/v1/tts", h.base))
        .bearer_auth(&h.token)
        .json(&json!({"text": "Ciao"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(r.headers()["content-type"], "audio/mpeg");
    let speech = h.mock.speech.lock().unwrap().clone();
    assert_eq!(speech[0]["response_format"], "mp3");
}

// -- local speech-to-text first --------------------------------------------------

#[tokio::test]
async fn local_stt_is_used_first_with_the_callers_bearer() {
    let h = harness_with(Local::Mock).await;
    let audio = pcm(1.0, HELLO, h.nonce);
    let r = h
        .chunked("?lang=it-IT", audio.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(header(&r, "x-lucina-transcript"), transcript(HELLO));
    assert_eq!(r.bytes().await.unwrap().as_ref(), KOKORO_PCM.repeat(2));

    let calls = h.mock.stt_calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    let c = &calls[0];
    // The device's own token, verbatim; no cookie.
    assert_eq!(
        c.auth.as_deref(),
        Some(format!("Bearer {}", h.token).as_str())
    );
    assert!(c.cookie.is_none() && c.csrf.is_none());
    assert_eq!(c.content_type, "audio/x-s16le;rate=16000");
    assert_eq!(c.query.as_deref(), Some("lang=it"));
    assert_eq!(c.len, audio.len());
    // No Gemini call, and the transcript reached Lucina.
    assert!(h.gemini.calls(HELLO, h.nonce).is_empty());
    assert_eq!(
        h.mock.chats.lock().unwrap()[0]["message"],
        transcript(HELLO)
    );
}

#[tokio::test]
async fn local_stt_without_lang_has_no_query_and_wav_goes_as_wav() {
    let h = harness_with(Local::Mock).await;
    let file = wav(&pcm(0.8, ENGLISH, h.nonce));
    let r = h
        .req("", "audio/wav", file.clone().into())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(header(&r, "x-lucina-transcript"), transcript(ENGLISH));
    let c = h.mock.stt_calls.lock().unwrap()[0].clone();
    assert_eq!(c.content_type, "audio/wav");
    assert_eq!(c.query, None);
    assert_eq!(c.len, file.len());
}

#[tokio::test]
async fn a_cookie_caller_is_forwarded_with_its_cookie_and_csrf() {
    let h = harness_with(Local::Mock).await;
    let login = h
        .http
        .post(format!("{}/v1/session", h.base))
        .json(&json!({"token": h.token}))
        .send()
        .await
        .unwrap();
    let cookie = login.headers()["set-cookie"].to_str().unwrap().to_owned();
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let r = h
        .http
        .post(format!("{}/v1/voice", h.base))
        .header("cookie", &cookie)
        .header("x-secretary-csrf", "1")
        .header("content-type", RAW)
        .body(pcm(1.0, HELLO, h.nonce))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let c = h.mock.stt_calls.lock().unwrap()[0].clone();
    assert_eq!(c.cookie.as_deref(), Some(cookie.as_str()));
    assert_eq!(c.csrf.as_deref(), Some("1"));
    assert!(c.auth.is_none());
}

#[tokio::test]
async fn a_failing_local_stt_falls_back_to_gemini() {
    for mode in [SttMode::Fail500, SttMode::Unavailable] {
        let h = harness_with(Local::Mock).await;
        *h.mock.stt_mode.lock().unwrap() = mode;
        let r = h.say("", HELLO).await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(header(&r, "x-lucina-transcript"), transcript(HELLO));
        assert_eq!(h.mock.stt_calls.lock().unwrap().len(), 1);
        assert_eq!(h.gemini.calls(HELLO, h.nonce).len(), 1);
        assert_eq!(h.mock.chats.lock().unwrap().len(), 1);
    }
    // Nothing listening at all.
    let h = harness_with(Local::Dead).await;
    let r = h.say("", HELLO).await;
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(h.gemini.calls(HELLO, h.nonce).len(), 1);
}

#[tokio::test]
async fn an_empty_local_transcript_is_no_speech_without_a_gemini_retry() {
    let h = harness_with(Local::Mock).await;
    *h.mock.stt_mode.lock().unwrap() = SttMode::Empty;
    let r = h.say("", HELLO).await;
    assert_eq!(r.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error_code(r).await, "no_speech");
    assert_eq!(h.mock.stt_calls.lock().unwrap().len(), 1);
    assert!(h.gemini.calls(HELLO, h.nonce).is_empty());
    assert!(h.mock.chats.lock().unwrap().is_empty());
}
