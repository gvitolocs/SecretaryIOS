//! `GET /v1/voice/ws`: the LucinaVoice socket, with mock local speech-to-text,
//! Lucina (SSE and the plain JSON fallback) and Kokoro upstreams. No Gemini
//! key is set in this binary, so a failing local STT is `stt_unavailable`.
//!
//! The mock STT answers with whatever `Mock::transcript` holds (a recording of
//! all zeros, the warm-up, gets an empty text). The mock Lucina picks its
//! script from the transcript: see [`lucina`]. The mock Kokoro answers each
//! request with three samples whose value is the length of the input text, so
//! the order and the text of the segments can be read off the audio.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode as AxumStatus};
use axum::response::IntoResponse;
use axum::{Json, Router};
use futures_util::{stream, SinkExt, StreamExt};
use reqwest::Client;
use secretary_server::config::Config;
use secretary_server::devices::DeviceStore;
use secretary_server::voice_frames as frames;
use secretary_server::{app, serve_on, AppState};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{self, Message};

const LUCINA_TOKEN: &str = "lucina-server-secret";

// Transcripts, and with them the Lucina scripts.
const TWO: &str = "Ciao Lucina, come stai oggi?";
const JSON_REPLY: &str = "Dimmi una cosa con una risposta semplice";
const REPLACES_NOTHING_SPOKEN: &str = "Dammi un testo che verra sostituito";
const REPLACES_AFTER_SPEECH: &str = "Dammi un testo lungo che verra sostituito";
const SECRET: &str = "Read me my verification code";
const SLOW: &str = "Tell me a very slow story today";
const BROKEN: &str = "Please break the chat for me today";
const PIPELINE: &str = "Dimmi due frasi per favore";
const EMPTY_REPLY: &str = "Please say nothing to me today";
// Jev fast path (protocol section 6): route and ack events.
const ACK_SLOW: &str = "Cerca sul web le notizie di oggi per me";
const ACK_LATE: &str = "Cerca nel web le novita di oggi per me";
const ACK_TWICE: &str = "Cerca online le ultime notizie per me";
const FAST_TIME: &str = "Che ore sono adesso per favore";
// Layered Jev decisions and Voice Learn (protocol sections 8 and 9).
const LAYERED: &str = "Parlami della via Skibigaardsvej per favore";
const LAYERED_NO_DONE: &str = "Dimmi una cosa sulla mia via per favore";

fn pcm_of(len: usize) -> Vec<u8> {
    let s = (len as i16).to_le_bytes();
    [s, s, s].concat()
}

#[derive(Default)]
struct Mock {
    /// What the local STT answers.
    transcript: Mutex<String>,
    /// Make the local STT answer 500.
    stt_fails: Mutex<bool>,
    /// Extra keys of the local STT's JSON answer (`corrections`, `ambiguous`).
    stt_extra: Mutex<Option<Value>>,
    stt_calls: Mutex<Vec<SttCall>>,
    chats: Mutex<Vec<(Value, Option<String>)>>,
    /// (input text, when it arrived)
    speech: Mutex<Vec<(String, Instant)>>,
    /// How long Kokoro takes before answering.
    kokoro_delay: Mutex<Duration>,
    /// Kokoro answers 500.
    kokoro_fails: Mutex<bool>,
    /// The SLOW script's SSE body was dropped (the chat connection closed).
    slow_dropped: Mutex<bool>,
    /// Every `POST /v1/share` (voice notes).
    shares: Mutex<Vec<ShareCall>>,
    /// Every `GET /v1/share/{id}`: (id, authorization).
    share_polls: Mutex<Vec<(String, Option<String>)>>,
    /// How the mock share behaves.
    share: Mutex<ShareScript>,
    /// How the mock Lucina's timeline stream behaves.
    timeline: Mutex<TimelineScript>,
    /// Timeline streams opened so far, and open now.
    stream_opens: Mutex<usize>,
    streams_open: Mutex<usize>,
    /// Every request to any mock whose path names the memo API, other than the speech-to-text call `/v1/memos/stt`
    /// (chat voice is never a memo: no memo, summary or brief is made from it).
    memo_calls: Mutex<Vec<String>>,
}

/// The mock Lucina's `GET /api/secretary/timeline/stream`: off (404) unless `reply_after` is set.
#[derive(Debug, Clone, Default)]
struct TimelineScript {
    /// Emit the answer to the latest share this long after the stream opens (never if `None`).
    reply_after: Option<Duration>,
    /// The first stream closes at once (the second one is the normal one).
    drop_first: bool,
}

struct StreamGuard(Arc<Mock>);

impl Drop for StreamGuard {
    fn drop(&mut self) {
        *self.0.streams_open.lock().unwrap() -= 1;
    }
}

/// What the mock share endpoint does.
#[derive(Debug, Clone, Default)]
struct ShareScript {
    /// `GET` answers `processing` this many times, then the final status.
    processing: usize,
    /// The final status is `error`, not `done`.
    ends_in_error: bool,
    /// `POST` answers this status instead of 202.
    refuse: Option<u16>,
}

/// One voice-note upload as the mock share saw it.
#[derive(Debug, Clone, Default)]
struct ShareCall {
    auth: Option<String>,
    cookie: Option<String>,
    csrf: Option<String>,
    /// `X-Voice-Test`, the proof of a test device's note.
    voice_test: Option<String>,
    content_type: String,
    fields: Vec<(String, String)>,
    file_name: String,
    file_type: String,
    file: Vec<u8>,
}

impl ShareCall {
    fn field(&self, k: &str) -> Option<&str> {
        self.fields.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
}

fn find_at(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// The parts of a `multipart/form-data` body: text fields and the one file.
fn parse_multipart(content_type: &str, body: &[u8], call: &mut ShareCall) {
    let boundary = content_type.split("boundary=").nth(1).expect("a multipart boundary");
    let delim = format!("--{boundary}").into_bytes();
    let mut pos = find_at(body, &delim, 0).expect("first boundary");
    loop {
        let after = pos + delim.len();
        if body[after..].starts_with(b"--") {
            return;
        }
        let head_start = after + 2; // CRLF
        let head_end = find_at(body, b"\r\n\r\n", head_start).expect("part headers");
        let head = String::from_utf8_lossy(&body[head_start..head_end]).into_owned();
        let data_start = head_end + 4;
        let next = find_at(body, &delim, data_start).expect("next boundary");
        let data = &body[data_start..next - 2]; // CRLF before the boundary
        let name = head.split("name=\"").nth(1).and_then(|r| r.split('"').next()).unwrap_or("").to_owned();
        if head.contains("filename=\"") {
            call.file_name = head.split("filename=\"").nth(1).and_then(|r| r.split('"').next()).unwrap_or("").to_owned();
            call.file_type = head.split("Content-Type: ").nth(1).unwrap_or("").trim().to_owned();
            call.file = data.to_vec();
            assert_eq!(name, "file");
        } else {
            call.fields.push((name, String::from_utf8_lossy(data).into_owned()));
        }
        pos = next;
    }
}

async fn share(
    State(m): State<Arc<Mock>>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    m.note_memo_path(&format!("share {}", uri.path()), uri.path());
    let script = m.share.lock().unwrap().clone();
    let hdr = |k: &str| headers.get(k).map(|v| v.to_str().unwrap().to_owned());
    if method == axum::http::Method::POST && uri.path() == "/v1/share" {
        let mut call = ShareCall {
            auth: hdr("authorization"),
            cookie: hdr("cookie"),
            csrf: hdr("x-secretary-csrf"),
            voice_test: hdr("x-voice-test"),
            content_type: hdr("content-type").unwrap_or_default(),
            ..ShareCall::default()
        };
        parse_multipart(&call.content_type.clone(), &body, &mut call);
        let id = call.field("shareId").unwrap_or("").to_owned();
        m.shares.lock().unwrap().push(call);
        if let Some(code) = script.refuse {
            return (
                AxumStatus::from_u16(code).unwrap(),
                Json(json!({"ok": false, "error": "invalid shareId"})),
            )
                .into_response();
        }
        return (
            AxumStatus::ACCEPTED,
            Json(json!({"ok": true, "shareId": id, "duplicate": false, "status": "processing",
                        "userItemId": format!("usr_{}", &id[..8.min(id.len())]), "attachments": []})),
        )
            .into_response();
    }
    if method == axum::http::Method::GET {
        if let Some(id) = uri.path().strip_prefix("/v1/share/") {
            let n = {
                let mut polls = m.share_polls.lock().unwrap();
                polls.push((id.to_owned(), hdr("authorization")));
                polls.iter().filter(|(i, _)| i == id).count()
            };
            let v = if n <= script.processing {
                json!({"ok": true, "shareId": id, "status": "processing", "stage": "thinking"})
            } else if script.ends_in_error {
                json!({"ok": true, "shareId": id, "status": "error", "error": "model failed"})
            } else {
                json!({"ok": true, "shareId": id, "status": "done", "userItemId": "usr_x",
                       "lucinaItemId": "lci_42", "reply": "Fatto, ho aggiunto il latte."})
            };
            return Json(v).into_response();
        }
    }
    (AxumStatus::NOT_FOUND, "no").into_response()
}

#[derive(Debug, Clone)]
struct SttCall {
    auth: Option<String>,
    content_type: String,
    query: Option<String>,
    cookie: Option<String>,
    csrf: Option<String>,
    len: usize,
    silence: bool,
    /// The largest absolute sample of the body, read as s16le.
    peak: i32,
}

impl Mock {
    /// Remember a request that names the memo API, `/v1/memos/stt` aside.
    fn note_memo_path(&self, what: &str, path: &str) {
        if path.contains("memo") && path != "/v1/memos/stt" {
            self.memo_calls.lock().unwrap().push(what.to_owned());
        }
    }

    fn say(&self, text: &str) {
        *self.transcript.lock().unwrap() = text.to_owned();
    }

    fn real_stt_calls(&self) -> Vec<SttCall> {
        self.stt_calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| !c.silence)
            .cloned()
            .collect()
    }

    fn spoken(&self) -> Vec<String> {
        self.speech
            .lock()
            .unwrap()
            .iter()
            .map(|(t, _)| t.clone())
            .collect()
    }
}

async fn local_stt(
    State(m): State<Arc<Mock>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let silence = body.iter().all(|b| *b == 0);
    m.note_memo_path(&format!("{} {}", "STT", uri.path()), uri.path());
    let peak = body
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i32::from(i16::from_le_bytes(*b)).abs())
        .max()
        .unwrap_or(0);
    m.stt_calls.lock().unwrap().push(SttCall {
        auth: headers
            .get("authorization")
            .map(|v| v.to_str().unwrap().to_owned()),
        content_type: headers
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_owned())
            .unwrap_or_default(),
        query: uri.query().map(str::to_owned),
        cookie: headers
            .get("cookie")
            .map(|v| v.to_str().unwrap().to_owned()),
        csrf: headers
            .get("x-secretary-csrf")
            .map(|v| v.to_str().unwrap().to_owned()),
        len: body.len(),
        silence,
        peak,
    });
    if *m.stt_fails.lock().unwrap() {
        return (AxumStatus::INTERNAL_SERVER_ERROR, "boom").into_response();
    }
    let text = if silence {
        String::new()
    } else {
        m.transcript.lock().unwrap().clone()
    };
    let mut answer = json!({"text": text, "language": "it", "ms": 5, "mode": "gpu"});
    if let (Some(Value::Object(extra)), false) = (m.stt_extra.lock().unwrap().clone(), silence) {
        answer.as_object_mut().unwrap().extend(extra);
    }
    Json(answer).into_response()
}

fn sse(
    events: Vec<(u64, &'static str, Value)>,
    m: Option<Arc<Mock>>,
    track: &'static str,
) -> axum::response::Response {
    // (delay before the event in ms, event, data)
    struct Dropped(Option<Arc<Mock>>, &'static str);
    impl Drop for Dropped {
        fn drop(&mut self) {
            if let (Some(m), "slow") = (&self.0, self.1) {
                *m.slow_dropped.lock().unwrap() = true;
            }
        }
    }
    let guard = Dropped(m.clone(), track);
    let s = stream::unfold((events.into_iter(), guard), |(mut it, guard)| async move {
        let (delay, ev, data) = it.next()?;
        tokio::time::sleep(Duration::from_millis(delay)).await;
        let block = format!("event: {ev}\ndata: {data}\n\n");
        Some((Ok::<_, std::io::Error>(Bytes::from(block)), (it, guard)))
    });
    (
        [("content-type", "text/event-stream")],
        Body::from_stream(s),
    )
        .into_response()
}

async fn lucina(
    State(m): State<Arc<Mock>>,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    m.note_memo_path(&format!("lucina {}", uri.path()), uri.path());
    if uri.path() == "/api/secretary/timeline/stream" {
        return timeline_stream(m).await;
    }
    if uri.path() != "/api/secretary/chat" {
        return (AxumStatus::NOT_FOUND, "no").into_response();
    }
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let accept = headers
        .get("accept")
        .map(|v| v.to_str().unwrap().to_owned());
    m.chats.lock().unwrap().push((body.clone(), accept));
    let msg = body["message"].as_str().unwrap_or("").to_owned();
    let d = |t: &str| json!({"text": t});
    let reply = |t: &str, sensitive: bool, replaces: bool| json!({"text": t, "sensitive": sensitive, "replaces": replaces});
    let done = || json!({"reply": "x", "action": "reply"});
    let route = |intent: &str, fast: bool| {
        json!({"intent": intent, "confidence": 0.93, "jev_ms": 212, "fast": fast,
               "source": "jev"})
    };
    match msg.as_str() {
        TWO => sse(
            vec![
                (0, "delta", d("Ciao a te. ")),
                (0, "delta", d("Come stai")),
                (0, "delta", d(" oggi?")),
                (
                    0,
                    "reply",
                    reply("Ciao a te. Come stai oggi?", false, false),
                ),
                (0, "done", done()),
            ],
            None,
            "",
        ),
        JSON_REPLY => {
            Json(json!({"ok": true, "data": {"reply": "Prima frase. Seconda frase lunga."}}))
                .into_response()
        }
        REPLACES_NOTHING_SPOKEN => sse(
            vec![
                (0, "delta", d("Testo vecchio senza fine")),
                (0, "reply", reply("Testo nuovo.", false, true)),
                (0, "done", done()),
            ],
            None,
            "",
        ),
        REPLACES_AFTER_SPEECH => sse(
            vec![
                (0, "delta", d("Prima frase detta. Resto che")),
                (0, "delta", d(" non sara detto")),
                (0, "reply", reply("Tutt'altro testo.", false, true)),
                (0, "done", done()),
            ],
            None,
            "",
        ),
        SECRET => sse(
            vec![
                (0, "delta", d("Il codice")),
                (0, "reply", reply("Your code is 482913.", true, true)),
                (0, "done", done()),
            ],
            None,
            "",
        ),
        SLOW => sse(
            vec![
                (0, "delta", d("Una frase iniziale. ")),
                (0, "delta", d("Un'altra frase. ")),
                (5000, "delta", d("Mai detta.")),
                (
                    0,
                    "reply",
                    reply(
                        "Una frase iniziale. Un'altra frase. Mai detta.",
                        false,
                        false,
                    ),
                ),
            ],
            Some(m.clone()),
            "slow",
        ),
        BROKEN => sse(
            vec![(0, "error", json!({"code": "llm_failed", "message": "boom"}))],
            None,
            "",
        ),
        ACK_SLOW => sse(
            vec![
                (0, "route", route("web_crawl", false)),
                (0, "ack", json!({"text": "One moment…"})),
                (250, "delta", d("Here is the news. ")),
                (0, "reply", reply("Here is the news.", false, false)),
                (0, "done", done()),
            ],
            None,
            "",
        ),
        ACK_LATE => sse(
            vec![
                (0, "route", route("web_crawl", false)),
                (0, "delta", d("Prima frase detta. ")),
                (0, "ack", json!({"text": "One moment…"})),
                (0, "delta", d("Seconda frase.")),
                (
                    0,
                    "reply",
                    reply("Prima frase detta. Seconda frase.", false, false),
                ),
                (0, "done", done()),
            ],
            None,
            "",
        ),
        ACK_TWICE => sse(
            vec![
                (0, "route", route("computer_task", false)),
                (0, "ack", json!({"text": "One moment…"})),
                (100, "ack", json!({"text": "Ancora un attimo…"})),
                (150, "delta", d("Done here. ")),
                (0, "reply", reply("Done here.", false, false)),
                (0, "done", done()),
            ],
            None,
            "",
        ),
        LAYERED => sse(
            vec![
                (
                    0,
                    "route",
                    json!({"intent": "chat", "confidence": 0.91, "jev_ms": 210, "fast": false,
                           "source": "jev", "layers": [
                        {"layer": "intent", "verdict": "chat", "confidence": 0.91, "ms": 210, "used": true}]}),
                ),
                (0, "delta", d("Una via di Aarhus. ")),
                (0, "reply", reply("Una via di Aarhus.", false, false)),
                (
                    0,
                    "done",
                    json!({"ok": true, "data": {"reply": "x", "action": "reply", "voice": {"layers": [
                        {"layer": "intent", "verdict": "chat", "confidence": 0.91, "ms": 215, "used": true},
                        {"layer": "vocab", "verdict": "Skibigårdsvej", "confidence": 0.77, "ms": 130, "used": true}]}}}),
                ),
            ],
            None,
            "",
        ),
        LAYERED_NO_DONE => sse(
            vec![
                (
                    0,
                    "route",
                    json!({"intent": "chat", "confidence": 0.8, "jev_ms": 190, "fast": false,
                           "source": "jev", "layers": [
                        {"layer": "intent", "verdict": "chat", "confidence": 0.8, "ms": 190, "used": true}]}),
                ),
                (0, "reply", reply("Una via.", false, true)),
                (0, "done", done()),
            ],
            None,
            "",
        ),
        FAST_TIME => sse(
            vec![
                (0, "route", route("temporal_query", true)),
                (0, "reply", reply("Sono le dieci e dieci.", false, true)),
                (0, "done", done()),
            ],
            None,
            "",
        ),
        PIPELINE => sse(
            vec![
                (0, "delta", d("Prima frase pipeline. ")),
                (0, "delta", d("Seconda frase pipeline.")),
                (
                    0,
                    "reply",
                    reply(
                        "Prima frase pipeline. Seconda frase pipeline.",
                        false,
                        false,
                    ),
                ),
                (0, "done", done()),
            ],
            None,
            "",
        ),
        EMPTY_REPLY => Json(json!({"ok": true, "data": {"reply": "  "}})).into_response(),
        _ => Json(json!({"ok": true, "data": {"reply": "Ok."}})).into_response(),
    }
}

/// Items for the voice notes: an unrelated one, then (after the delay) the reply to the latest note.
async fn timeline_stream(m: Arc<Mock>) -> axum::response::Response {
    let script = m.timeline.lock().unwrap().clone();
    let Some(delay) = script.reply_after else {
        return (AxumStatus::NOT_FOUND, "no").into_response();
    };
    let nth = {
        let mut n = m.stream_opens.lock().unwrap();
        *n += 1;
        *n
    };
    *m.streams_open.lock().unwrap() += 1;
    let guard = StreamGuard(m.clone());
    let item = |id: &str, role: &str, reply_to: &str, text: &str| {
        let data = json!({"id": id, "ts": "2026-11-08T10:00:00.000Z", "channel": "ios", "role": role,
                          "text": text, "replyTo": reply_to});
        format!("event: item\nid: {id}\ndata: {data}\n\n")
    };
    let mut chunks: Vec<(Duration, String)> = vec![(Duration::ZERO, ": ping\n\n".into())];
    if script.drop_first && nth == 1 {
        return (
            [("content-type", "text/event-stream")],
            Body::from_stream(stream::iter(chunks.into_iter().map(|(_, c)| Ok::<_, std::io::Error>(Bytes::from(c))))),
        )
            .into_response();
    }
    chunks.push((Duration::ZERO, item("lci_other", "lucina", "usr_somebody", "Non per te.")));
    chunks.push((Duration::ZERO, item("usr_echo", "user", "usr_x", "Eco.")));
    let uid = {
        let shares = m.shares.lock().unwrap();
        let id = shares.last().and_then(|c| c.field("shareId")).unwrap_or("").to_owned();
        format!("usr_{}", &id[..8.min(id.len())])
    };
    chunks.push((delay, item("lci_42", "lucina", &uid, "Fatto, ho aggiunto il latte.")));
    let s = stream::unfold((chunks.into_iter(), guard), |(mut it, guard)| async move {
        match it.next() {
            Some((d, c)) => {
                tokio::time::sleep(d).await;
                Some((Ok::<_, std::io::Error>(Bytes::from(c)), (it, guard)))
            }
            // Stay open, like Lucina's stream, until the client goes away.
            None => std::future::pending().await,
        }
    });
    ([("content-type", "text/event-stream")], Body::from_stream(s)).into_response()
}

async fn kokoro(State(m): State<Arc<Mock>>, body: Bytes) -> axum::response::Response {
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let input = v["input"].as_str().unwrap_or("").to_owned();
    if *m.kokoro_fails.lock().unwrap() {
        return (AxumStatus::INTERNAL_SERVER_ERROR, "boom").into_response();
    }
    m.speech
        .lock()
        .unwrap()
        .push((input.clone(), Instant::now()));
    let delay = *m.kokoro_delay.lock().unwrap();
    tokio::time::sleep(delay).await;
    // Two chunks, split inside a sample.
    let pcm = pcm_of(input.chars().count());
    let (a, b) = pcm.split_at(3);
    let s = stream::iter([
        Ok::<_, std::io::Error>(Bytes::copy_from_slice(a)),
        Ok(Bytes::copy_from_slice(b)),
    ]);
    ([("content-type", "audio/pcm")], Body::from_stream(s)).into_response()
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
    token: String,
    /// The speaker's device id.
    dev_id: String,
    data_dir: std::path::PathBuf,
    st: AppState,
    mock: Arc<Mock>,
    _dir: tempfile::TempDir,
}

async fn harness() -> H {
    harness_with(|_| {}).await
}

async fn harness_with(tweak: impl FnOnce(&mut Config)) -> H {
    harness_inner(tweak, false).await
}

/// The speaker's device is listed in `SECRETARY_VOICE_TEST_DEVICES`.
async fn harness_test_device() -> H {
    harness_inner(|_| {}, true).await
}

async fn harness_inner(tweak: impl FnOnce(&mut Config), test_device: bool) -> H {
    let mock = Arc::new(Mock::default());
    let share_url = spawn(
        Router::new()
            .fallback(share)
            .layer(axum::extract::DefaultBodyLimit::disable())
            .with_state(mock.clone()),
    )
    .await;
    let lucina_url = spawn(Router::new().fallback(lucina).with_state(mock.clone())).await;
    let tts = spawn(Router::new().fallback(kokoro).with_state(mock.clone())).await;
    let stt = spawn(Router::new().fallback(local_stt).with_state(mock.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(format!("http://{lucina_url}"), Some(LUCINA_TOKEN.into()));
    cfg.data_dir = dir.path().join("state");
    cfg.tts_url = format!("http://{tts}/");
    cfg.voice_stt_url = Some(format!("http://{stt}/v1/memos/stt"));
    cfg.voice_share_url = Some(format!("http://{share_url}/v1/share"));
    tweak(&mut cfg);
    let (dev, token) = DeviceStore::new(&cfg.data_dir).issue("Speaker").unwrap();
    if test_device {
        cfg.voice_test_devices = vec!["dev_somebody_else".into(), dev.id.clone()];
    }
    let st = AppState::new(&cfg).unwrap();
    let addr = spawn(app(st.clone())).await;
    H {
        addr,
        http: Client::new(),
        token,
        dev_id: dev.id,
        data_dir: cfg.data_dir.clone(),
        st,
        mock,
        _dir: dir,
    }
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

impl H {
    async fn try_connect(&self, bearer: Option<&str>) -> Result<Ws, u16> {
        let mut req = format!("ws://{}/v1/voice/ws", self.addr)
            .into_client_request()
            .unwrap();
        if let Some(t) = bearer {
            req.headers_mut()
                .insert("authorization", format!("Bearer {t}").parse().unwrap());
        }
        match tokio_tungstenite::connect_async(req).await {
            Ok((ws, _)) => Ok(ws),
            Err(tungstenite::Error::Http(r)) => Err(r.status().as_u16()),
            Err(e) => panic!("connect: {e}"),
        }
    }

    async fn connect(&self) -> Ws {
        self.try_connect(Some(&self.token)).await.unwrap()
    }

    /// Connect with extra upgrade headers (`X-Secretary-Client`).
    async fn connect_with(&self, extra: &[(&'static str, &'static str)]) -> Ws {
        let mut req = format!("ws://{}/v1/voice/ws", self.addr)
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("authorization", format!("Bearer {}", self.token).parse().unwrap());
        for (k, v) in extra {
            req.headers_mut().insert(*k, v.parse().unwrap());
        }
        tokio_tungstenite::connect_async(req).await.unwrap().0
    }
}

/// What the server sent, a binary frame or a JSON message.
#[derive(Debug)]
enum Got {
    Json(Value),
    Pcm(Vec<u8>),
}

async fn recv(ws: &mut Ws) -> Got {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("timed out waiting for the server")
            .expect("socket closed")
            .expect("socket error");
        match m {
            Message::Text(t) => return Got::Json(serde_json::from_str(t.as_str()).unwrap()),
            Message::Binary(b) => return Got::Pcm(b.to_vec()),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected {other:?}"),
        }
    }
}

async fn recv_json(ws: &mut Ws) -> Value {
    match recv(ws).await {
        Got::Json(v) => v,
        Got::Pcm(_) => panic!("unexpected audio"),
    }
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::text(v.to_string())).await.unwrap();
}

/// Everything up to and including `done` or `error`.
async fn turn_output(ws: &mut Ws) -> Vec<Got> {
    let mut out = vec![];
    loop {
        let g = recv(ws).await;
        let last = matches!(&g, Got::Json(v) if v["type"] == "done" || v["type"] == "error");
        out.push(g);
        if last {
            return out;
        }
    }
}

fn types(out: &[Got]) -> Vec<String> {
    let mut t: Vec<String> = vec![];
    for g in out {
        let s = match g {
            Got::Json(v) => v["type"].as_str().unwrap().to_owned(),
            Got::Pcm(_) => "pcm".to_owned(),
        };
        if s != "pcm" || t.last().map(String::as_str) != Some("pcm") {
            t.push(s);
        }
    }
    t
}

fn audio(out: &[Got]) -> Vec<u8> {
    out.iter()
        .filter_map(|g| match g {
            Got::Pcm(b) => Some(b.clone()),
            Got::Json(_) => None,
        })
        .flatten()
        .collect()
}

fn json_of<'a>(out: &'a [Got], ty: &str) -> &'a Value {
    out.iter()
        .find_map(|g| match g {
            Got::Json(v) if v["type"] == ty => Some(v),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {ty} message in {out:?}"))
}

/// s16le 16 kHz: `ms` of a 440 Hz tone.
fn tone(ms: usize) -> Vec<u8> {
    (0..16 * ms)
        .flat_map(|i| {
            let x = 8000.0 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin();
            (x as i16).to_le_bytes()
        })
        .collect()
}

fn silence(ms: usize) -> Vec<u8> {
    vec![0; 32 * ms]
}

/// s16le 16 kHz: `ms` of deterministic hiss, samples evenly in `-amp..=amp`.
fn hiss(ms: usize, amp: i16) -> Vec<u8> {
    let mut seed: u32 = 7;
    (0..16 * ms)
        .flat_map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let v = ((seed >> 16) as i32 % (2 * i32::from(amp) + 1)) - i32::from(amp);
            (v as i16).to_le_bytes()
        })
        .collect()
}

/// Speech-like and quiet (the DSi's): bursts of 400 ms of a 440 Hz tone of amplitude `amp`, 400 ms of room hiss (+-3) between.
fn quiet_speech(ms: usize, amp: f32) -> Vec<u8> {
    let room = hiss(ms, 3);
    (0..16 * ms)
        .flat_map(|i| {
            let burst = (i / 6400) % 2 == 0;
            let v = if burst {
                (amp * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16_000.0).sin()) as i16
            } else {
                i16::from_le_bytes([room[2 * i], room[2 * i + 1]])
            };
            v.to_le_bytes()
        })
        .collect()
}

/// The largest absolute sample of s16le bytes.
fn peak_of(pcm: &[u8]) -> i32 {
    pcm.as_chunks::<2>().0.iter().map(|b| i32::from(i16::from_le_bytes(*b)).abs()).max().unwrap_or(0)
}

/// Stream `audio` like the speaker: ~32 ms (1024-byte) frames.
async fn stream_audio(ws: &mut Ws, audio: &[u8]) {
    for f in audio.chunks(1024) {
        ws.send(Message::binary(f.to_vec())).await.unwrap();
    }
}

/// `listening`, with what the server understood of the `start` (protocol section 10.1).
async fn expect_listening(ws: &mut Ws) -> Value {
    let v = recv_json(ws).await;
    assert_eq!(v["type"], "listening", "{v}");
    v
}

async fn start_ptt(ws: &mut Ws) {
    send(ws, json!({"type": "start", "mode": "ptt"})).await;
    expect_listening(ws).await;
}

async fn say_ptt(ws: &mut Ws, ms: usize) -> Vec<Got> {
    start_ptt(ws).await;
    stream_audio(ws, &tone(ms)).await;
    send(ws, json!({"type": "end"})).await;
    turn_output(ws).await
}

#[tokio::test]
async fn a_push_to_talk_turn_streams_two_sentences_in_order() {
    let h = harness().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 1000).await;

    assert_eq!(
        types(&out),
        ["transcript", "audio", "pcm", "reply", "done"],
        "{out:?}"
    );
    assert_eq!(json_of(&out, "transcript")["text"], TWO);
    assert_eq!(json_of(&out, "audio")["rate"], 24000);
    assert_eq!(json_of(&out, "reply")["text"], "Ciao a te. Come stai oggi?");

    // Audio: the first sentence's three samples, then the second's.
    let expect = [pcm_of("Ciao a te.".len()), pcm_of("Come stai oggi?".len())].concat();
    assert_eq!(audio(&out), expect);
    for g in &out {
        if let Got::Pcm(b) = g {
            assert!(b.len() <= 8192 && b.len() % 2 == 0, "{}", b.len());
        }
    }
    assert_eq!(
        h.mock.spoken(),
        ["Ciao a te.", "Come stai oggi?"],
        "one Kokoro request per sentence"
    );

    // All five timings, in order of the pipeline.
    let ms = &json_of(&out, "done")["ms"];
    for k in ["stt", "lucina_first", "tts_first", "first_audio", "total"] {
        assert!(ms[k].is_u64(), "{k}: {ms}");
    }
    assert!(ms["stt"].as_u64() <= ms["lucina_first"].as_u64());
    assert!(ms["lucina_first"].as_u64() <= ms["tts_first"].as_u64());
    assert!(ms["tts_first"].as_u64() <= ms["first_audio"].as_u64());
    assert!(ms["first_audio"].as_u64() <= ms["total"].as_u64());
    // A Lucina without route/ack: no such keys, as before.
    assert!(ms.get("route").is_none() && ms.get("ack").is_none(), "{ms}");

    // Speech-to-text: the device's own bearer, raw PCM, trimmed of its
    // trailing silence (there was none: all of the tone).
    let calls = h.mock.real_stt_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].auth.as_deref(),
        Some(format!("Bearer {}", h.token).as_str())
    );
    assert_eq!(calls[0].content_type, "audio/x-s16le;rate=16000");
    assert_eq!(calls[0].len, 32_000);
    // Opening the socket (then `start`) warmed the engine with 0.5 s of silence, once in all.
    let warm: Vec<_> = h
        .mock
        .stt_calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.silence)
        .cloned()
        .collect();
    assert_eq!(warm.len(), 1);
    assert_eq!(warm[0].len, 16_000);
    assert_eq!(
        warm[0].auth.as_deref(),
        Some(format!("Bearer {}", h.token).as_str())
    );

    // Lucina got a streaming, spoken-style, session-scoped request.
    let chats = h.mock.chats.lock().unwrap().clone();
    assert_eq!(chats.len(), 1);
    let (body, accept) = &chats[0];
    assert_eq!(accept.as_deref(), Some("text/event-stream"));
    assert_eq!(body["message"], TWO);
    assert_eq!(body["voice"], true);
    assert_eq!(body["sessionId"], "lucinavoice");
    assert_eq!(body["deviceName"], "Speaker");
    assert!(body["clientMessageId"]
        .as_str()
        .unwrap()
        .starts_with("voice-"));
    assert!(body["device"].as_str().is_some());

    // The socket is ready for the next turn; the engine is warm, no new warm-up.
    start_ptt(&mut ws).await;
    send(&mut ws, json!({"type": "cancel"})).await;
    assert_eq!(
        h.mock
            .stt_calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.silence)
            .count(),
        1
    );
}

#[tokio::test]
async fn opening_the_socket_warms_speech_to_text() {
    let h = harness().await;
    let _ws = h.connect().await;
    // No `start`: the open alone warms the engine with the caller's own credentials.
    let deadline = Instant::now() + Duration::from_secs(5);
    let warm = loop {
        let warm: Vec<_> = h
            .mock
            .stt_calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.silence)
            .cloned()
            .collect();
        if !warm.is_empty() || Instant::now() > deadline {
            break warm;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(warm.len(), 1);
    assert_eq!(warm[0].len, 16_000);
    assert_eq!(
        warm[0].auth.as_deref(),
        Some(format!("Bearer {}", h.token).as_str())
    );
    assert!(h.mock.real_stt_calls().is_empty());
}

#[tokio::test]
async fn vad_mode_ends_the_turn_by_itself() {
    let h = harness().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    send(
        &mut ws,
        json!({"type": "start", "mode": "vad", "lang": "it"}),
    )
    .await;
    expect_listening(&mut ws).await;
    // 0.4 s of room noise (silence), 1 s of speech, then silence; no `end`.
    let mut sent = silence(400);
    sent.extend(tone(1000));
    sent.extend(silence(1500));
    stream_audio(&mut ws, &sent).await;

    assert_eq!(recv_json(&mut ws).await, json!({"type": "endpoint"}));
    let out = turn_output(&mut ws).await;
    assert_eq!(
        types(&out),
        ["transcript", "audio", "pcm", "reply", "done"],
        "{out:?}"
    );
    // The recording was cut at the endpoint, and its trailing silence trimmed
    // to 200 ms: 0.4 + 1.0 + 0.2 s.
    let calls = h.mock.real_stt_calls();
    assert_eq!(calls.len(), 1);
    let secs = calls[0].len as f64 / 32_000.0;
    assert!((1.55..=1.7).contains(&secs), "{secs}");
    assert_eq!(calls[0].query.as_deref(), Some("lang=it"));
    assert_eq!(h.mock.spoken().len(), 2);
}

#[tokio::test]
async fn a_second_start_is_busy_and_so_is_the_http_route() {
    let h = harness().await;
    h.mock.say(TWO);
    *h.mock.kokoro_delay.lock().unwrap() = Duration::from_millis(400);
    let mut ws = h.connect().await;
    start_ptt(&mut ws).await;
    // Another start while listening.
    send(&mut ws, json!({"type": "start"})).await;
    let e = recv_json(&mut ws).await;
    assert_eq!(
        (e["type"].as_str(), e["code"].as_str()),
        (Some("error"), Some("busy"))
    );
    // POST /v1/voice for the same device is busy too.
    let r = h
        .http
        .post(format!("http://{}/v1/voice", h.addr))
        .bearer_auth(&h.token)
        .header("content-type", "audio/x-s16le;rate=16000")
        .body(tone(500))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"]["code"],
        "voice_busy"
    );

    // The turn goes on as if nothing happened, and a start while it is
    // being processed is busy as well.
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    assert_eq!(recv_json(&mut ws).await["type"], "transcript");
    send(&mut ws, json!({"type": "start"})).await;
    let e = recv_json(&mut ws).await;
    assert_eq!(e["code"], "busy", "{e}");
    let out = turn_output(&mut ws).await;
    assert_eq!(json_of(&out, "done")["type"], "done");
    // Free again.
    start_ptt(&mut ws).await;
}

#[tokio::test]
async fn cancel_stops_the_turn_and_frees_the_device() {
    let h = harness().await;
    h.mock.say(SLOW);
    let mut ws = h.connect().await;
    start_ptt(&mut ws).await;
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    assert_eq!(recv_json(&mut ws).await["type"], "transcript");
    assert_eq!(recv_json(&mut ws).await["type"], "audio");
    assert!(matches!(recv(&mut ws).await, Got::Pcm(_)));

    send(&mut ws, json!({"type": "cancel"})).await;
    // At once a new turn can start.
    send(&mut ws, json!({"type": "start", "mode": "ptt"})).await;
    // Frames of the cancelled turn may still arrive before `listening`.
    loop {
        match recv(&mut ws).await {
            Got::Pcm(_) => continue,
            Got::Json(v) => {
                assert_eq!(v["type"], "listening", "{v}");
                break;
            }
        }
    }
    // Nothing more of the old turn: not the 5 s delta, no reply or done.
    let quiet = tokio::time::timeout(Duration::from_millis(400), ws.next()).await;
    assert!(quiet.is_err(), "{quiet:?}");
    // The chat connection to Lucina was dropped.
    for _ in 0..50 {
        if *h.mock.slow_dropped.lock().unwrap() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        *h.mock.slow_dropped.lock().unwrap(),
        "chat stream still open"
    );
}

#[tokio::test]
async fn closing_the_socket_aborts_the_turn() {
    let h = harness().await;
    h.mock.say(SLOW);
    let mut ws = h.connect().await;
    start_ptt(&mut ws).await;
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    assert_eq!(recv_json(&mut ws).await["type"], "transcript");
    drop(ws);
    for _ in 0..100 {
        if *h.mock.slow_dropped.lock().unwrap() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("chat stream still open after the socket closed");
}

#[tokio::test]
async fn a_plain_json_lucina_still_works() {
    let h = harness().await;
    h.mock.say(JSON_REPLY);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 600).await;
    assert_eq!(
        types(&out),
        ["transcript", "audio", "pcm", "reply", "done"],
        "{out:?}"
    );
    assert_eq!(h.mock.spoken(), ["Prima frase.", "Seconda frase lunga."]);
    assert_eq!(
        audio(&out),
        [
            pcm_of("Prima frase.".len()),
            pcm_of("Seconda frase lunga.".len())
        ]
        .concat()
    );
}

#[tokio::test]
async fn a_replacing_reply_is_spoken_when_nothing_was_said_yet() {
    let h = harness().await;
    h.mock.say(REPLACES_NOTHING_SPOKEN);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 600).await;
    assert_eq!(json_of(&out, "reply")["text"], "Testo nuovo.");
    assert_eq!(h.mock.spoken(), ["Testo nuovo."]);
    assert_eq!(audio(&out), pcm_of("Testo nuovo.".len()));
}

#[tokio::test]
async fn a_replacing_reply_after_speech_drops_the_rest() {
    let h = harness().await;
    h.mock.say(REPLACES_AFTER_SPEECH);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 600).await;
    assert_eq!(json_of(&out, "done")["type"], "done");
    assert_eq!(h.mock.spoken(), ["Prima frase detta."]);
    assert_eq!(json_of(&out, "reply")["text"], "Prima frase detta.");
}

#[tokio::test]
async fn a_sensitive_reply_is_never_spoken() {
    let h = harness().await;
    h.mock.say(SECRET);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 600).await;
    assert_eq!(
        json_of(&out, "reply")["text"],
        "I'll answer that in the app."
    );
    assert_eq!(h.mock.spoken(), ["I'll answer that in the app."]);
    let all = format!("{out:?}");
    assert!(!all.contains("482913"));
    // `lang` decides the phrase when given.
    send(
        &mut ws,
        json!({"type": "start", "mode": "ptt", "lang": "it"}),
    )
    .await;
    assert_eq!(recv_json(&mut ws).await["type"], "listening");
    stream_audio(&mut ws, &tone(600)).await;
    send(&mut ws, json!({"type": "end"})).await;
    let out = turn_output(&mut ws).await;
    assert_eq!(json_of(&out, "reply")["text"], "I'll answer that in the app.");
}

#[tokio::test]
async fn an_empty_reply_gets_the_short_apology() {
    let h = harness().await;
    h.mock.say(EMPTY_REPLY);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 600).await;
    assert_eq!(h.mock.spoken(), ["Sorry, I don't have an answer."]);
    assert_eq!(json_of(&out, "done")["type"], "done");
}

#[tokio::test]
async fn errors_end_the_turn_not_the_socket() {
    let h = harness().await;
    let mut ws = h.connect().await;

    // Under 0.3 s: no speech, and the engine is not asked.
    start_ptt(&mut ws).await;
    stream_audio(&mut ws, &tone(200)).await;
    send(&mut ws, json!({"type": "end"})).await;
    let e = recv_json(&mut ws).await;
    assert_eq!(
        (e["type"].as_str(), e["code"].as_str()),
        (Some("error"), Some("no_speech"))
    );
    assert!(h.mock.real_stt_calls().is_empty());

    // An empty transcript.
    h.mock.say("   ");
    let out = say_ptt(&mut ws, 600).await;
    assert_eq!(json_of(&out, "error")["code"], "no_speech");
    assert!(h.mock.chats.lock().unwrap().is_empty());

    // The engine down and no Gemini: stt_unavailable.
    *h.mock.stt_fails.lock().unwrap() = true;
    let out = say_ptt(&mut ws, 600).await;
    assert_eq!(json_of(&out, "error")["code"], "stt_unavailable");
    *h.mock.stt_fails.lock().unwrap() = false;

    // Lucina's `error` event.
    h.mock.say(BROKEN);
    let out = say_ptt(&mut ws, 600).await;
    assert_eq!(json_of(&out, "error")["code"], "lucina_unavailable");
    assert!(!out.iter().any(|g| matches!(g, Got::Pcm(_))));

    // Still usable: a good turn.
    h.mock.say(JSON_REPLY);
    let out = say_ptt(&mut ws, 600).await;
    assert_eq!(json_of(&out, "done")["type"], "done");
}

#[tokio::test]
async fn vad_mode_without_speech_is_no_speech() {
    let h = harness().await;
    let mut ws = h.connect().await;
    send(&mut ws, json!({"type": "start", "mode": "vad"})).await;
    assert_eq!(recv_json(&mut ws).await["type"], "listening");
    // A steady noise (-35 dBFS RMS) is neither silent (the level rules) nor speech (the VAD wants 3x its own floor).
    stream_audio(&mut ws, &hiss(1500, 1000)).await;
    send(&mut ws, json!({"type": "end"})).await;
    let e = recv_json(&mut ws).await;
    assert_eq!(e["code"], "no_speech");
    assert!(h.mock.real_stt_calls().is_empty());
}

#[tokio::test]
async fn bad_messages_are_answered_and_ignored() {
    let h = harness().await;
    let mut ws = h.connect().await;
    for msg in [
        json!({"type": "start", "mode": "shout"}),
        json!({"type": "start", "lang": "fr"}),
        json!({"type": "dance"}),
    ] {
        send(&mut ws, msg).await;
        let e = recv_json(&mut ws).await;
        assert_eq!(e["code"], "bad_request", "{e}");
    }
    ws.send(Message::text("not json")).await.unwrap();
    assert_eq!(recv_json(&mut ws).await["code"], "bad_request");
    // The device's hello and telemetry frames are accepted without an answer.
    send(
        &mut ws,
        json!({"type": "hello", "now": 1234, "specs": {"chip": "esp32"}}),
    )
    .await;
    send(
        &mut ws,
        json!({"type": "telemetry", "now": 5678, "dropped": 0, "events": []}),
    )
    .await;
    // Late frames, `end` and `cancel` with no turn are ignored.
    ws.send(Message::binary(vec![0u8; 640])).await.unwrap();
    send(&mut ws, json!({"type": "end"})).await;
    send(&mut ws, json!({"type": "cancel"})).await;
    start_ptt(&mut ws).await;
}

#[tokio::test]
async fn the_second_sentence_is_requested_while_the_first_is_made() {
    let h = harness().await;
    h.mock.say(PIPELINE);
    *h.mock.kokoro_delay.lock().unwrap() = Duration::from_millis(400);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 600).await;
    assert_eq!(json_of(&out, "done")["type"], "done");
    let speech = h.mock.speech.lock().unwrap().clone();
    assert_eq!(speech.len(), 2);
    let gap = speech[1].1.duration_since(speech[0].1);
    assert!(gap < Duration::from_millis(200), "{gap:?}: not pipelined");
    // And still strictly in order.
    assert_eq!(
        audio(&out),
        [
            pcm_of("Prima frase pipeline.".len()),
            pcm_of("Seconda frase pipeline.".len())
        ]
        .concat()
    );
}

#[tokio::test]
async fn the_upgrade_needs_a_device() {
    let h = harness().await;
    assert_eq!(h.try_connect(None).await.err(), Some(401));
    assert_eq!(
        h.try_connect(Some("sec_notarealtoken")).await.err(),
        Some(401)
    );
    // A plain GET with a good token is not an upgrade.
    let r = h
        .http
        .get(format!("http://{}/v1/voice/ws", h.addr))
        .bearer_auth(&h.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    assert_eq!(
        r.json::<Value>().await.unwrap()["error"]["code"],
        "bad_request"
    );
    assert!(h.mock.chats.lock().unwrap().is_empty());
}

#[tokio::test]
async fn post_voice_streams_the_pipeline_audio() {
    let h = harness().await;
    h.mock.say(PIPELINE);
    let r = h
        .http
        .post(format!("http://{}/v1/voice", h.addr))
        .bearer_auth(&h.token)
        .header("content-type", "audio/x-s16le;rate=16000")
        .body(tone(800))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "audio/x-s16le;rate=24000");
    assert!(r.headers().get("x-lucina-reply").is_none());
    let mut body = r.bytes_stream();
    let mut all = vec![];
    while let Some(c) = body.next().await {
        all.extend_from_slice(&c.unwrap());
    }
    assert_eq!(
        all,
        [
            pcm_of("Prima frase pipeline.".len()),
            pcm_of("Seconda frase pipeline.".len())
        ]
        .concat()
    );
}

#[tokio::test]
async fn a_cookie_caller_must_come_from_this_site() {
    let h = harness().await;
    h.mock.say(TWO);
    let login = h
        .http
        .post(format!("http://{}/v1/session", h.addr))
        .json(&json!({"token": h.token}))
        .send()
        .await
        .unwrap();
    let cookie = login.headers()["set-cookie"].to_str().unwrap().to_owned();
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let connect = |origin: Option<String>| {
        let mut req = format!("ws://{}/v1/voice/ws", h.addr)
            .into_client_request()
            .unwrap();
        req.headers_mut().insert("cookie", cookie.parse().unwrap());
        if let Some(o) = origin {
            req.headers_mut().insert("origin", o.parse().unwrap());
        }
        async move {
            match tokio_tungstenite::connect_async(req).await {
                Ok((ws, _)) => Ok(ws),
                Err(tungstenite::Error::Http(r)) => Err(r.status().as_u16()),
                Err(e) => panic!("connect: {e}"),
            }
        }
    };
    assert_eq!(
        connect(Some("https://evil.example".into())).await.err(),
        Some(403)
    );
    assert_eq!(connect(Some("null".into())).await.err(), Some(403));
    // The site's own origin, and no Origin at all (not a browser), get in.
    let mut ws = connect(Some(format!("http://{}", h.addr))).await.unwrap();
    let out = say_ptt(&mut ws, 600).await;
    assert_eq!(json_of(&out, "done")["type"], "done");
    drop(ws);
    assert!(connect(None).await.is_ok());
    // The engine was reached with the cookie and the CSRF header, no bearer.
    let calls = h.mock.real_stt_calls();
    assert_eq!(calls[0].cookie.as_deref(), Some(cookie.as_str()));
    assert_eq!(calls[0].csrf.as_deref(), Some("1"));
    assert!(calls[0].auth.is_none());
}

// -- telemetry (protocol sections 4 and 5) -----------------------------------------

impl H {
    /// A second device token (never sends a `hello`: an owner's app).
    fn issue(&self, name: &str) -> (String, String) {
        let (d, t) = DeviceStore::new(&self.data_dir).issue(name).unwrap();
        (d.id, t)
    }

    async fn get(&self, token: Option<&str>, path: &str) -> (u16, Value) {
        let mut r = self.http.get(format!("http://{}{path}", self.addr));
        if let Some(t) = token {
            r = r.bearer_auth(t);
        }
        let r = r.send().await.unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    /// Poll `path` until `ok` accepts the answer (telemetry is handled asynchronously).
    async fn wait_for(&self, token: &str, path: &str, ok: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..150 {
            let (code, v) = self.get(Some(token), path).await;
            if code == 200 && ok(&v) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("{path} never matched");
    }
}

fn hello_frame() -> Value {
    json!({"type": "hello", "now": 4000, "specs": {
        "chip": "esp32", "rev": "v3.1", "cores": 2, "cpu_mhz": 240, "flash_bytes": 4194304,
        "psram_bytes": 0, "fw": "0.4.1", "commit": "abc1234", "mac": "da:b8",
        "heap_total": 300000, "ina": "ina219", "temp_sensor": false},
        "boot": {"reset": "poweron", "seq": 3}})
}

fn stats_event(up: u64, down: u64) -> Value {
    json!({"t": 60_000, "k": "stats", "uptime_ms": 60_000, "heap_free": 120000,
        "heap_min_free": 90000, "rssi": -58, "up_bytes": up, "down_bytes": down,
        "tx_air_ms": 900, "cpu_mhz": 240,
        "power": {"est_mw": 395, "rail_mv": 4980, "rail_ma": 79, "energy_mwh": 6.5}})
}

#[tokio::test]
async fn hello_telemetry_and_a_turn_show_up_in_the_status_api() {
    let h = harness().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    send(&mut ws, hello_frame()).await;
    let out = say_ptt(&mut ws, 1000).await;
    assert_eq!(json_of(&out, "done")["type"], "done");
    // The device reports the turn and its stats; nothing is answered.
    send(
        &mut ws,
        json!({"type": "telemetry", "now": 61_000, "dropped": 0, "events": [
            {"t": 60_500, "k": "turn", "ok": true, "mode": "link", "speech_ms": 1000,
             "first_audio_ms": 1300, "total_ms": 3000, "up_bytes": 33000, "down_bytes": 400,
             "server_ms": json_of(&out, "done")["ms"].clone()},
            stats_event(4000, 9000),
            {"t": 1000, "k": "wifi", "up": true, "rssi": -57}]}),
    )
    .await;
    let path = format!("/v1/voice/devices/{}/status", h.dev_id);
    let v = h
        .wait_for(&h.token, &path, |v| {
            v["live"]["up_bytes"] == 4000 && v["turns"][0]["device_first_audio_ms"] == 1300
        })
        .await;
    assert_eq!(v["id"], h.dev_id.as_str());
    assert_eq!(v["name"], "Speaker");
    assert_eq!(v["online"], true);
    assert!(v["connected_since"].is_string());
    assert_eq!(v["specs"]["fw"], "0.4.1");
    assert_eq!(v["specs"]["ina"], "ina219");
    assert_eq!(v["boot"]["reset"], "poweron");
    assert_eq!(v["live"]["rssi"], -58);
    assert_eq!(v["power"]["measured"], true);
    assert_eq!(v["power"]["est_mw"], 395);
    // The device booted 60 s before: its totals count in full.
    assert_eq!(v["network"]["today_up_bytes"], 4000);
    assert_eq!(v["network"]["total_down_bytes"], 9000);
    // One merged turn: the server's spans and bytes, the device's own measure.
    let turns = v["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    let t = &turns[0];
    assert_eq!(t["ok"], true);
    assert_eq!(t["transcript_chars"], TWO.chars().count());
    assert_eq!(t["speech_ms"], 1000);
    assert_eq!(t["up_bytes"], 32_000);
    assert_eq!(t["down_bytes"], 12);
    assert_eq!(t["device_first_audio_ms"], 1300);
    assert!(t.get("intent").is_none() && t.get("fast").is_none(), "{t}");
    assert!(t["spans"].get("route").is_none() && t["spans"].get("ack").is_none());
    let done_ms = &json_of(&out, "done")["ms"];
    for k in ["stt", "lucina_first", "tts_first", "first_audio"] {
        assert_eq!(t["spans"][k], done_ms[k], "{k}");
    }
    assert!(t["spans"]["total"].as_u64() >= done_ms["total"].as_u64());
    // The transcript text is nowhere in the stored data.
    let dir = h.data_dir.join("voice").join(&h.dev_id);
    for f in ["status.json", "events.jsonl"] {
        let text = std::fs::read_to_string(dir.join(f)).unwrap_or_default();
        assert!(!text.contains("Ciao Lucina"), "{f}");
    }

    // The list, and the events (newest first, the server's turn included).
    let (code, list) = h.get(Some(&h.token), "/v1/voice/devices").await;
    assert_eq!(code, 200);
    assert_eq!(list[0]["id"], h.dev_id.as_str());
    assert_eq!(list[0]["online"], true);
    assert_eq!(list[0]["fw"], "0.4.1");
    assert_eq!(list[0]["est_mw"], 395);
    assert_eq!(list[0]["rssi"], -58);
    let (_, ev) = h
        .get(
            Some(&h.token),
            &format!("/v1/voice/devices/{}/events?limit=50", h.dev_id),
        )
        .await;
    let kinds: Vec<&str> = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["k"].as_str().unwrap())
        .collect();
    for k in ["server_turn", "turn", "stats", "wifi"] {
        assert!(kinds.contains(&k), "{k} in {kinds:?}");
    }

    // Closing the socket takes the device offline.
    drop(ws);
    h.wait_for(&h.token, &path, |v| {
        v["online"] == false && v.get("connected_since").is_none()
    })
    .await;
}

#[tokio::test]
async fn bad_telemetry_is_ignored_and_the_socket_and_turns_keep_working() {
    let h = harness().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    send(&mut ws, hello_frame()).await;
    // Oversized (over 16 KiB, under the socket's 64 KiB), not JSON, wrong shapes.
    send(
        &mut ws,
        json!({"type": "telemetry", "now": 1, "events": [], "pad": "p".repeat(20_000)}),
    )
    .await;
    ws.send(Message::text("{\"type\":\"telemetry\","))
        .await
        .unwrap();
    send(&mut ws, json!({"type": "telemetry", "events": "nope"})).await;
    send(&mut ws, json!({"type": "telemetry", "now": "x", "events": [1, null, "a", {"k": 5}, {"k": "stats", "up_bytes": "lots"}]})).await;
    send(
        &mut ws,
        json!({"type": "hello", "specs": "nope", "boot": 5}),
    )
    .await;
    send(
        &mut ws,
        json!({"type": "telemetry", "now": 100, "events": [
        {"t": 100, "k": "error", "what": "<script>alert(1)</script>", "detail": "x".repeat(900)},
        {"t": 100, "k": "nope"}]}),
    )
    .await;
    // A turn still works, with no error frames in between.
    let out = say_ptt(&mut ws, 600).await;
    assert_eq!(
        types(&out),
        ["transcript", "audio", "pcm", "reply", "done"],
        "{out:?}"
    );

    let path = format!("/v1/voice/devices/{}/events?kind=error", h.dev_id);
    let v = h
        .wait_for(&h.token, &path, |v| {
            v["events"].as_array().is_some_and(|a| !a.is_empty())
        })
        .await;
    let e = &v["events"][0];
    assert_eq!(e["what"], "scriptalert(1)/script");
    assert_eq!(e["detail"].as_str().unwrap().len(), 160);
    assert_eq!(v["events"].as_array().unwrap().len(), 1);
    let (_, st) = h
        .get(
            Some(&h.token),
            &format!("/v1/voice/devices/{}/status", h.dev_id),
        )
        .await;
    assert_eq!(st["specs"]["chip"], "esp32");
    assert_eq!(st["network"]["total_up_bytes"], 0);
}

#[tokio::test]
async fn events_filter_page_and_validate() {
    let h = harness().await;
    let mut ws = h.connect().await;
    send(&mut ws, hello_frame()).await;
    for i in 0..6u64 {
        let k = if i % 2 == 0 { "wifi" } else { "link" };
        send(&mut ws, json!({"type": "telemetry", "now": 1000, "events": [{"t": 1000, "k": k, "up": true, "ms": i}]})).await;
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    let base = format!("/v1/voice/devices/{}/events", h.dev_id);
    let v = h
        .wait_for(&h.token, &format!("{base}?limit=100"), |v| {
            v["events"].as_array().is_some_and(|a| a.len() == 6)
        })
        .await;
    assert!(v.get("next").is_none());
    let (_, p1) = h.get(Some(&h.token), &format!("{base}?limit=4")).await;
    assert_eq!(p1["events"].as_array().unwrap().len(), 4);
    assert_eq!(p1["events"][0]["ms"], 5);
    let next = p1["next"].as_str().unwrap().to_owned();
    assert_eq!(next, p1["events"][3]["at"].as_str().unwrap());
    // `before` is exclusive; `+` in an offset must be encoded by the client.
    let (_, p2) = h
        .get(Some(&h.token), &format!("{base}?limit=4&before={next}"))
        .await;
    assert!(p2["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["at"].as_str().unwrap() < next.as_str()));
    let (_, w) = h.get(Some(&h.token), &format!("{base}?kind=wifi")).await;
    assert_eq!(w["events"].as_array().unwrap().len(), 3);
    assert!(w["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["k"] == "wifi"));
    let (_, s) = h
        .get(Some(&h.token), &format!("{base}?since={next}&limit=100"))
        .await;
    assert!(s["events"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["at"].as_str().unwrap() >= next.as_str()));
    // Bad parameters.
    for q in [
        "since=yesterday",
        "before=1",
        "kind=Wi-Fi",
        "kind=",
        "limit=abc",
    ] {
        let (code, v) = h.get(Some(&h.token), &format!("{base}?{q}")).await;
        assert_eq!(code, 400, "{q}");
        assert_eq!(v["error"]["code"], "bad_request", "{q}");
    }
}

#[tokio::test]
async fn device_tokens_see_only_themselves_and_the_owner_session_sees_all() {
    let h = harness().await;
    // Speaker A (the harness's device) and speaker B say hello.
    let mut a = h.connect().await;
    send(&mut a, hello_frame()).await;
    let (b_id, b_token) = h.issue("Kitchen");
    let mut b = h.try_connect(Some(&b_token)).await.unwrap();
    send(&mut b, hello_frame()).await;
    // A speaker paired a moment ago that has not connected yet, and an owner's phone app.
    let (fresh_id, fresh) = h.issue("LucinaVoice");
    let (_, phone) = h.issue("Phone");
    let login = h
        .http
        .post(format!("http://{}/v1/session", h.addr))
        .json(&json!({"token": h.token}))
        .send()
        .await
        .unwrap();
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let owner = |path: String| {
        let req = h
            .http
            .get(format!("http://{}{path}", h.addr))
            .header("cookie", &cookie);
        async move {
            let r = req.send().await.unwrap();
            let code = r.status().as_u16();
            (code, r.json::<Value>().await.unwrap_or(Value::Null))
        }
    };

    let a_path = format!("/v1/voice/devices/{}/status", h.dev_id);
    let b_path = format!("/v1/voice/devices/{b_id}/status");
    h.wait_for(&h.token, &a_path, |v| v["specs"]["chip"] == "esp32")
        .await;
    h.wait_for(&b_token, &b_path, |v| v["specs"]["chip"] == "esp32")
        .await;

    // The owner's session sees both, a name per device, and 404 for unknown ids.
    let (code, list) = owner("/v1/voice/devices".into()).await;
    assert_eq!(code, 200);
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&"Speaker") && names.contains(&"Kitchen"));
    assert_eq!(owner(a_path.clone()).await.0, 200);
    assert_eq!(owner(b_path.clone()).await.0, 200);
    assert_eq!(
        owner(format!("/v1/voice/devices/{b_id}/events")).await.0,
        200
    );
    let (code, v) = owner("/v1/voice/devices/dev_nobody/status".into()).await;
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (404, Some("device_not_found"))
    );
    assert_eq!(
        owner("/v1/voice/devices/..%2Fetc/status".into()).await.0,
        404
    );

    // Every device token is confined to its own id: a speaker, one that never connected
    // (just paired, no hello yet) and any other device such as the phone app alike.
    for (who, token, own) in [
        ("speaker A", h.token.as_str(), Some(h.dev_id.as_str())),
        ("fresh speaker", fresh.as_str(), None),
        ("phone", phone.as_str(), None),
    ] {
        let (code, list) = h.get(Some(token), "/v1/voice/devices").await;
        assert_eq!(code, 200, "{who}");
        let ids: Vec<&str> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, own.into_iter().collect::<Vec<_>>(), "{who}");
        for path in [
            b_path.clone(),
            format!("/v1/voice/devices/{b_id}/events"),
            "/v1/voice/devices/dev_nobody/status".to_owned(),
        ] {
            let (code, v) = h.get(Some(token), &path).await;
            assert_eq!(
                (code, v["error"]["code"].as_str()),
                (403, Some("owner_only")),
                "{who} {path}"
            );
        }
    }
    // The fresh speaker reading speaker A: refused before it has ever said hello.
    let (code, _) = h.get(Some(&fresh), &a_path).await;
    assert_eq!(code, 403);
    // Its own id with no data yet is a 404, not someone else's data.
    let (code, v) = h
        .get(
            Some(&fresh),
            &format!("/v1/voice/devices/{fresh_id}/status"),
        )
        .await;
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (404, Some("device_not_found"))
    );
    assert_eq!(h.get(Some(&h.token), &a_path).await.0, 200);

    // No credentials: 401 on all three.
    for p in [
        "/v1/voice/devices",
        a_path.as_str(),
        &format!("/v1/voice/devices/{}/events", h.dev_id),
    ] {
        assert_eq!(h.get(None, p).await.0, 401, "{p}");
    }
    drop((a, b));
}
#[tokio::test]
async fn post_voice_turns_are_recorded_too() {
    let h = harness().await;
    h.mock.say(PIPELINE);
    let audio = tone(800);
    let r = h
        .http
        .post(format!("http://{}/v1/voice", h.addr))
        .bearer_auth(&h.token)
        .header("content-type", "audio/x-s16le;rate=16000")
        .body(audio.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body = r.bytes().await.unwrap();
    let path = format!("/v1/voice/devices/{}/status", h.dev_id);
    let v = h
        .wait_for(&h.token, &path, |v| {
            v["turns"][0]["down_bytes"].as_u64() == Some(body.len() as u64)
        })
        .await;
    let t = &v["turns"][0];
    assert_eq!(t["ok"], true);
    assert_eq!(t["transcript_chars"], PIPELINE.chars().count());
    assert_eq!(t["up_bytes"], audio.len());
    assert_eq!(t["speech_ms"], 800);
    assert!(
        t["spans"]["stt"].is_u64()
            && t["spans"]["tts_first"].is_u64()
            && t["spans"]["total"].is_u64()
    );
    assert_eq!(v["online"], false);
    // A failed one (nothing said) is recorded as such.
    h.mock.say("  ");
    let r = h
        .http
        .post(format!("http://{}/v1/voice", h.addr))
        .bearer_auth(&h.token)
        .header("content-type", "audio/x-s16le;rate=16000")
        .body(audio)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
    let v = h
        .wait_for(&h.token, &path, |v| {
            v["turns"].as_array().is_some_and(|t| t.len() == 2)
        })
        .await;
    assert_eq!(v["turns"][0]["ok"], false);
    assert_eq!(v["turns"][0]["err"], "no_speech");
}

// -- Jev fast path: route and ack (protocol section 6) ---------------------------

/// The cache holds both languages' phrases (`Un attimo…`, `Vediamo…`, `One moment…`, `Let me see…`; only the English ones are played).
async fn fillers_ready(h: &H) {
    for _ in 0..100 {
        if h.st.fillers.count(true) == 2 && h.st.fillers.count(false) == 2 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "filler cache not ready: it {} en {}",
        h.st.fillers.count(true),
        h.st.fillers.count(false)
    );
}

/// The first sample value of the audio (the mock encodes the input length in it).
fn first_len(pcm: &[u8]) -> i16 {
    i16::from_le_bytes([pcm[0], pcm[1]])
}

#[tokio::test]
async fn the_filler_cache_is_built_at_startup() {
    let h = harness().await;
    assert_eq!(h.st.fillers.count(true) + h.st.fillers.count(false), 0);
    secretary_server::voice_filler::spawn_prewarm(&h.st);
    fillers_ready(&h).await;
    let mut spoken = h.mock.spoken();
    spoken.sort();
    assert_eq!(
        spoken,
        ["Let me see…", "One moment…", "Un attimo…", "Vediamo…"]
    );
    // A few hundred KB at most (the mock makes 6 bytes each).
    assert!(h.st.fillers.bytes() <= 1024 * 1024);
}

#[tokio::test]
async fn an_ack_plays_the_filler_first_then_the_answer_in_order() {
    let h = harness().await;
    secretary_server::voice_filler::spawn_prewarm(&h.st);
    fillers_ready(&h).await;
    h.mock.speech.lock().unwrap().clear();
    h.mock.say(ACK_SLOW);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 900).await;

    assert_eq!(
        types(&out),
        ["transcript", "audio", "pcm", "reply", "done"],
        "one audio header, then filler and answer as one stream: {out:?}"
    );
    let pcm = audio(&out);
    let answer = pcm_of("Here is the news.".len());
    assert_eq!(pcm.len(), 6 + answer.len(), "filler + answer");
    assert!(
        first_len(&pcm) == 11,
        "an English filler comes first: {}",
        first_len(&pcm)
    );
    assert_eq!(pcm[6..], answer[..], "the answer follows, whole");
    // The filler came from memory: Kokoro only heard the answer.
    assert_eq!(h.mock.spoken(), ["Here is the news."]);
    // The filler is not part of the spoken reply.
    assert_eq!(json_of(&out, "reply")["text"], "Here is the news.");

    let ms = &json_of(&out, "done")["ms"];
    let n = |k: &str| ms[k].as_u64().unwrap_or_else(|| panic!("{k}: {ms}"));
    assert!(n("route") <= n("ack"), "{ms}");
    assert!(n("ack") <= n("first_audio"), "{ms}");
    assert!(
        n("first_audio") < n("tts_first"),
        "the filler plays before the answer is synthesized: {ms}"
    );
    assert!(n("tts_first") <= n("total"));

    // The route reaches the status API and the stored events, with no text.
    let path = format!("/v1/voice/devices/{}/status", h.dev_id);
    let v = h
        .wait_for(&h.token, &path, |v| v["turns"][0]["intent"] == "web_crawl")
        .await;
    let t = &v["turns"][0];
    assert_eq!(t["fast"], false);
    assert_eq!(t["source"], "jev");
    assert_eq!(t["jev_ms"], 212);
    assert!((t["confidence"].as_f64().unwrap() - 0.93).abs() < 1e-9);
    assert_eq!(t["spans"]["route"], ms["route"]);
    assert_eq!(t["spans"]["ack"], ms["ack"]);
    let (_, ev) = h
        .get(
            Some(&h.token),
            &format!("/v1/voice/devices/{}/events?kind=server_turn", h.dev_id),
        )
        .await;
    let e = &ev["events"][0];
    assert_eq!(e["intent"], "web_crawl");
    assert_eq!(e["fast"], false);
    let dir = h.data_dir.join("voice").join(&h.dev_id);
    for f in ["status.json", "events.jsonl"] {
        let text = std::fs::read_to_string(dir.join(f)).unwrap_or_default();
        assert!(!text.contains("Cerca sul web"), "{f}");
    }
}

#[tokio::test]
async fn the_filler_follows_the_turns_language() {
    let h = harness().await;
    secretary_server::voice_filler::spawn_prewarm(&h.st);
    fillers_ready(&h).await;
    h.mock.say(ACK_SLOW);
    let mut ws = h.connect().await;
    send(
        &mut ws,
        json!({"type": "start", "mode": "ptt", "lang": "en"}),
    )
    .await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(900)).await;
    send(&mut ws, json!({"type": "end"})).await;
    let out = turn_output(&mut ws).await;
    // "One moment…" and "Let me see…" are both 11 characters.
    assert_eq!(first_len(&audio(&out)), 11, "{out:?}");
}

#[tokio::test]
async fn a_second_ack_is_ignored() {
    let h = harness().await;
    secretary_server::voice_filler::spawn_prewarm(&h.st);
    fillers_ready(&h).await;
    h.mock.say(ACK_TWICE);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 900).await;
    let pcm = audio(&out);
    assert_eq!(
        pcm.len(),
        6 + pcm_of("Done here.".len()).len(),
        "one filler"
    );
    assert_eq!(
        h.mock.spoken().last().map(String::as_str),
        Some("Done here.")
    );
}

#[tokio::test]
async fn an_ack_after_audio_has_started_is_ignored() {
    let h = harness().await;
    secretary_server::voice_filler::spawn_prewarm(&h.st);
    fillers_ready(&h).await;
    h.mock.speech.lock().unwrap().clear();
    h.mock.say(ACK_LATE);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 900).await;
    assert_eq!(
        audio(&out),
        [
            pcm_of("Prima frase detta.".len()),
            pcm_of("Seconda frase.".len())
        ]
        .concat(),
        "the answer only: {out:?}"
    );
    assert_eq!(h.mock.spoken(), ["Prima frase detta.", "Seconda frase."]);
    let ms = &json_of(&out, "done")["ms"];
    assert!(ms["route"].is_u64() && ms.get("ack").is_none(), "{ms}");
}

#[tokio::test]
async fn a_fast_route_gets_no_filler_and_its_route_is_recorded() {
    let h = harness().await;
    secretary_server::voice_filler::spawn_prewarm(&h.st);
    fillers_ready(&h).await;
    h.mock.speech.lock().unwrap().clear();
    h.mock.say(FAST_TIME);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 900).await;
    assert_eq!(audio(&out), pcm_of("Sono le dieci e dieci.".len()));
    assert_eq!(h.mock.spoken(), ["Sono le dieci e dieci."]);
    let ms = &json_of(&out, "done")["ms"];
    assert!(ms["route"].is_u64() && ms.get("ack").is_none(), "{ms}");
    let path = format!("/v1/voice/devices/{}/status", h.dev_id);
    let v = h
        .wait_for(&h.token, &path, |v| {
            v["turns"][0]["intent"] == "temporal_query"
        })
        .await;
    assert_eq!(v["turns"][0]["fast"], true);
    assert!(v["turns"][0]["spans"].get("ack").is_none());
}

#[tokio::test]
async fn voice_turns_are_the_voice_channel_and_carry_stt_facts_to_lucina() {
    let h = harness().await;
    *h.mock.stt_extra.lock().unwrap() = Some(json!({
        "corrections": [{"from": "skibi gard", "to": "Skibigårdsvej"}, {"from": "Mikel", "to": "Mikkel"}],
        "ambiguous": [{"heard": "via", "candidates": ["Skibigårdsvej", "Skibbyvej"]}]
    }));
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 800).await;
    assert_eq!(json_of(&out, "done")["type"], "done");
    let chats = h.mock.chats.lock().unwrap().clone();
    let chat = &chats[0].0;
    assert_eq!(chat["channel"], "voice");
    assert_eq!(chat["sessionId"], "lucinavoice");
    assert_eq!(chat["voiceMeta"]["turnId"], chat["clientMessageId"]);
    assert!(chat["voiceMeta"]["ms"]["stt"].is_u64());
    // Lucina's vocab layer gets the doubtful spans; the replaced words are counted here, not sent.
    assert_eq!(
        chat["voiceMeta"]["ambiguous"],
        json!([{"heard": "via", "candidates": ["Skibigårdsvej", "Skibbyvej"]}])
    );
    assert!(chat["voiceMeta"].get("corrections").is_none());

    // The telemetry keeps counts only: no words, no transcript text.
    let path = format!("/v1/voice/devices/{}/status", h.dev_id);
    let v = h
        .wait_for(&h.token, &path, |v| v["turns"][0]["stt_corrections"] == 2)
        .await;
    assert_eq!(v["turns"][0]["stt_ambiguous"], 1);
    assert_eq!(v["turns"][0]["transcript_chars"], TWO.chars().count());
    let dir = h.data_dir.join("voice").join(&h.dev_id);
    for f in ["status.json", "events.jsonl"] {
        let text = std::fs::read_to_string(dir.join(f)).unwrap_or_default();
        for word in ["Skibigårdsvej", "Mikkel", "skibi gard", "Ciao Lucina"] {
            assert!(!text.contains(word), "{f} holds {word:?}");
        }
    }
    // A turn with nothing to report has no such keys.
    h.mock.say(PIPELINE);
    *h.mock.stt_extra.lock().unwrap() = None;
    let out = say_ptt(&mut ws, 800).await;
    assert_eq!(json_of(&out, "done")["type"], "done");
    let v = h
        .wait_for(&h.token, &path, |v| v["turns"].as_array().is_some_and(|t| t.len() == 2))
        .await;
    assert!(v["turns"][0].get("stt_corrections").is_none() && v["turns"][0].get("layers").is_none());
    let chats = h.mock.chats.lock().unwrap().clone();
    assert!(chats[1].0["voiceMeta"].get("ambiguous").is_none());
}

#[tokio::test]
async fn jev_layers_reach_the_done_message_the_status_api_and_the_event_log() {
    let h = harness().await;
    h.mock.say(LAYERED);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 900).await;
    // `route.layers` completed by the `done` body (a layer of the same name is replaced).
    let layers = json!([
        {"layer": "intent", "verdict": "chat", "confidence": 0.91, "ms": 215, "used": true},
        {"layer": "vocab", "verdict": "Skibigårdsvej", "confidence": 0.77, "ms": 130, "used": true}
    ]);
    assert_eq!(json_of(&out, "done")["layers"], layers);
    assert!(json_of(&out, "done")["ms"].get("layers").is_none(), "ms stays numbers only");

    let path = format!("/v1/voice/devices/{}/status", h.dev_id);
    let v = h
        .wait_for(&h.token, &path, |v| v["turns"][0]["layers"].is_array())
        .await;
    assert_eq!(v["turns"][0]["layers"], layers);
    assert_eq!(v["turns"][0]["intent"], "chat");
    let (_, ev) = h
        .get(
            Some(&h.token),
            &format!("/v1/voice/devices/{}/events?kind=server_turn", h.dev_id),
        )
        .await;
    assert_eq!(ev["events"][0]["layers"], layers);
    // Facts only: the transcript and the reply never reach the stored files.
    let dir = h.data_dir.join("voice").join(&h.dev_id);
    for f in ["status.json", "events.jsonl"] {
        let text = std::fs::read_to_string(dir.join(f)).unwrap_or_default();
        assert!(!text.contains("Skibigaardsvej") && !text.contains("Aarhus"), "{f}");
    }
}

#[tokio::test]
async fn a_done_without_layers_keeps_the_routes_layers() {
    let h = harness().await;
    h.mock.say(LAYERED_NO_DONE);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 900).await;
    assert_eq!(
        json_of(&out, "done")["layers"],
        json!([{"layer": "intent", "verdict": "chat", "confidence": 0.8, "ms": 190, "used": true}])
    );
    // And a Lucina that sends no layers at all leaves the message as it was.
    h.mock.say(TWO);
    let out = say_ptt(&mut ws, 900).await;
    assert!(json_of(&out, "done").get("layers").is_none());
}

#[tokio::test]
async fn the_cache_is_rebuilt_lazily_after_a_kokoro_failure() {
    let h = harness().await;
    // Kokoro is down at startup: nothing cached.
    *h.mock.kokoro_fails.lock().unwrap() = true;
    assert_eq!(h.st.fillers.prewarm(&h.st.tts).await, 0);
    *h.mock.kokoro_fails.lock().unwrap() = false;

    // The first ack finds the cache empty: the ack text is spoken like a
    // sentence, first, and a rebuild starts.
    h.mock.say(ACK_SLOW);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 900).await;
    assert_eq!(
        audio(&out),
        [
            pcm_of("One moment…".chars().count()),
            pcm_of("Here is the news.".len())
        ]
        .concat(),
        "{out:?}"
    );
    assert_eq!(json_of(&out, "reply")["text"], "Here is the news.");
    assert!(json_of(&out, "done")["ms"]["ack"].is_u64());
    fillers_ready(&h).await;

    // The next one plays from memory: Kokoro hears only the answer.
    h.mock.speech.lock().unwrap().clear();
    let out = say_ptt(&mut ws, 900).await;
    let pcm = audio(&out);
    assert!(first_len(&pcm) == 11, "{}", first_len(&pcm));
    assert_eq!(pcm.len(), 6 + pcm_of("Here is the news.".len()).len());
    assert_eq!(h.mock.spoken(), ["Here is the news."]);
}

#[tokio::test]
async fn post_voice_plays_the_filler_first_too() {
    let h = harness().await;
    secretary_server::voice_filler::spawn_prewarm(&h.st);
    fillers_ready(&h).await;
    h.mock.say(ACK_SLOW);
    let r = h
        .http
        .post(format!("http://{}/v1/voice", h.addr))
        .bearer_auth(&h.token)
        .header("content-type", "audio/x-s16le;rate=16000")
        .body(tone(800))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["x-tts-voice"], "af_heart");
    let all = r.bytes().await.unwrap();
    assert!(first_len(&all) == 11);
    assert_eq!(all[6..], pcm_of("Here is the news.".len())[..]);
    let path = format!("/v1/voice/devices/{}/status", h.dev_id);
    let v = h
        .wait_for(&h.token, &path, |v| v["turns"][0]["intent"] == "web_crawl")
        .await;
    assert!(v["turns"][0]["spans"]["ack"].is_u64());
}


// -- Streaming input: codecs, notes, text (protocol section 10) ------------------------------------

/// s16 samples of a 440 Hz tone at `rate`.
fn tone_samples(rate: u32, ms: usize) -> Vec<i16> {
    (0..rate as usize * ms / 1000)
        .map(|i| (8000.0 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32).sin()) as i16)
        .collect()
}

/// IMA-ADPCM (mono, `adpcm_ima_wav` blocks of `block` bytes) of `pcm`; the last block is padded
/// with its last sample, like Fortuna's encoder does.
fn ima_encode(pcm: &[i16], block: usize) -> Vec<u8> {
    let spb = frames::samples_per_block(block);
    let mut index = 0i32;
    let mut out = Vec::new();
    for chunk in pcm.chunks(spb) {
        let mut pred = chunk[0] as i32;
        out.extend_from_slice(&chunk[0].to_le_bytes());
        out.push(index as u8);
        out.push(0);
        let last = *chunk.last().unwrap();
        let mut codes = Vec::with_capacity(spb - 1);
        for k in 1..spb {
            let s = *chunk.get(k).unwrap_or(&last) as i32;
            let mut best = (i32::MAX, 0u8, pred, index);
            for code in 0..16u8 {
                let (p, i) = frames::expand(pred, index, code);
                if (p - s).abs() < best.0 {
                    best = ((p - s).abs(), code, p, i);
                }
            }
            codes.push(best.1);
            pred = best.2;
            index = best.3;
        }
        for pair in codes.chunks(2) {
            out.push(pair[0] | (pair.get(1).copied().unwrap_or(0) << 4));
        }
        assert_eq!(out.len() % block, 0);
    }
    out
}

/// Frames of `size` bytes, the way a device cuts a stream (not at block boundaries).
async fn stream_bytes(ws: &mut Ws, data: &[u8], size: usize) {
    for f in data.chunks(size) {
        ws.send(Message::binary(f.to_vec())).await.unwrap();
    }
}

fn is_uuid_v4(s: &str) -> bool {
    s.len() == 36
        && s.as_bytes()[14] == b'4'
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit() && !b.is_ascii_uppercase(),
        })
}

#[tokio::test]
async fn listening_echoes_what_the_server_understood_for_every_start() {
    let h = harness().await;
    let mut ws = h.connect().await;
    // An old-style start: today's turn, with the echo of its defaults.
    send(&mut ws, json!({"type": "start", "mode": "ptt"})).await;
    let a = expect_listening(&mut ws).await;
    assert_eq!(a["purpose"], "command");
    assert_eq!(a["codec"], "pcm16");
    assert_eq!(a["rate"], 16000);
    let id = a["turn"].as_str().unwrap();
    assert!(is_uuid_v4(id), "{id}");
    send(&mut ws, json!({"type": "cancel"})).await;
    // A new-style one: the explicit values come back; every turn has its own id.
    send(
        &mut ws,
        json!({"type": "start", "purpose": "note", "codec": "ima-adpcm", "rate": 10909, "block": 256}),
    )
    .await;
    let b = expect_listening(&mut ws).await;
    assert_eq!(
        (b["purpose"].as_str(), b["codec"].as_str(), b["rate"].as_u64()),
        (Some("note"), Some("ima-adpcm"), Some(10909))
    );
    assert!(is_uuid_v4(b["turn"].as_str().unwrap()));
    assert_ne!(b["turn"], a["turn"]);
}

#[tokio::test]
async fn bad_start_options_are_bad_requests_and_start_no_turn() {
    let h = harness().await;
    let mut ws = h.connect().await;
    let adpcm = json!({"codec": "ima-adpcm", "rate": 10909, "block": 256});
    let with = |extra: Value| {
        let mut v = json!({"type": "start"});
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    let mut bad = vec![
        json!({"type": "start", "codec": "opus"}),
        json!({"type": "start", "rate": 8000}),
        json!({"type": "start", "codec": "ima-adpcm", "rate": 10909}),
        json!({"type": "start", "codec": "ima-adpcm", "rate": 10909, "block": 4}),
        json!({"type": "start", "codec": "ima-adpcm", "rate": 10909, "block": 5000}),
        json!({"type": "start", "codec": "ima-adpcm", "rate": 30000, "block": 256}),
        json!({"type": "start", "codec": "ima-adpcm", "rate": 7000, "block": 256}),
        json!({"type": "start", "purpose": "memo"}),
        json!({"type": "start", "thread": "side"}),
        json!({"type": "start", "purpose": "note", "thread": "voice"}),
        json!({"type": "start", "purpose": "note", "mode": "vad"}),
        json!({"type": "start", "mode": "walkie"}),
        json!({"type": "start", "channel": "voice"}),
        json!({"type": "start", "channel": "android"}),
        json!({"type": "start", "sessionId": "has space"}),
        json!({"type": "start", "sessionId": "x".repeat(65)}),
        json!({"type": "start", "replyTo": ""}),
        json!({"type": "start", "replyTo": "x".repeat(65)}),
        json!({"type": "start", "source": "iOS-chat"}),
        json!({"type": "start", "source": "a".repeat(33)}),
        json!({"type": "start", "rate": "16000"}),
        json!({"type": "start", "text": "yes"}),
        json!({"type": "start", "speak": 0}),
        json!({"type": "start", "lang": "fr"}),
    ];
    bad.push(with(json!({"codec": "pcm16", "block": "x"})));
    bad.push({
        let mut v = with(adpcm.clone());
        v["rate"] = json!(10909.5);
        v
    });
    for msg in bad {
        send(&mut ws, msg.clone()).await;
        let e = recv_json(&mut ws).await;
        assert_eq!(
            (e["type"].as_str(), e["code"].as_str()),
            (Some("error"), Some("bad_request")),
            "{msg} -> {e}"
        );
    }
    // None of them started a turn: no busy, no listening, nothing in the way.
    let quiet = tokio::time::timeout(Duration::from_millis(200), ws.next()).await;
    assert!(quiet.is_err(), "{quiet:?}");
    send(&mut ws, with(adpcm)).await;
    expect_listening(&mut ws).await;
    // Unknown keys are ignored, as before.
    send(&mut ws, json!({"type": "cancel"})).await;
    send(&mut ws, json!({"type": "start", "future": [1, 2], "hello": true})).await;
    expect_listening(&mut ws).await;
}

#[tokio::test]
async fn an_ima_adpcm_command_turn_reaches_the_stt_as_16k_pcm() {
    let h = harness().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    let tone = tone_samples(10_909, 1200);
    let data = ima_encode(&tone, 256);
    send(
        &mut ws,
        json!({"type": "start", "mode": "ptt", "codec": "ima-adpcm", "rate": 10909, "block": 256}),
    )
    .await;
    let l = expect_listening(&mut ws).await;
    assert_eq!((l["codec"].as_str(), l["rate"].as_u64()), (Some("ima-adpcm"), Some(10909)));
    // 700-byte frames: they cut the blocks anywhere.
    stream_bytes(&mut ws, &data, 700).await;
    send(&mut ws, json!({"type": "end"})).await;
    let out = turn_output(&mut ws).await;
    assert_eq!(types(&out), ["transcript", "audio", "pcm", "reply", "done"], "{out:?}");
    assert_eq!(json_of(&out, "transcript")["text"], TWO);

    let calls = h.mock.real_stt_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].content_type, "audio/x-s16le;rate=16000");
    // 1200 ms of tone are 13 090 samples at 10 909 Hz (26 blocks decode to 13 130): about 19 200
    // to 19 260 samples at 16 kHz, whole samples.
    let samples = calls[0].len / 2;
    assert_eq!(calls[0].len % 2, 0);
    assert!((19_190..=19_262).contains(&samples), "{samples}");
    // The turn is the usual one for Lucina.
    let chats = h.mock.chats.lock().unwrap().clone();
    assert_eq!(chats[0].0["channel"], "voice");
    assert_eq!(chats[0].0["sessionId"], "lucinavoice");
}

#[tokio::test]
async fn an_ima_adpcm_vad_turn_ends_by_itself() {
    let h = harness().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    send(
        &mut ws,
        json!({"type": "start", "mode": "vad", "codec": "ima-adpcm", "rate": 8000, "block": 128}),
    )
    .await;
    expect_listening(&mut ws).await;
    let mut pcm = vec![0i16; 8000 * 4 / 10];
    pcm.extend(tone_samples(8000, 1000));
    pcm.extend(vec![0i16; 8000 * 3 / 2]);
    stream_bytes(&mut ws, &ima_encode(&pcm, 128), 512).await;
    assert_eq!(recv_json(&mut ws).await, json!({"type": "endpoint"}));
    let out = turn_output(&mut ws).await;
    assert_eq!(json_of(&out, "done")["type"], "done", "{out:?}");
}

/// The mock share's POSTs, once `n` have arrived.
async fn shares(h: &H, n: usize) -> Vec<ShareCall> {
    for _ in 0..200 {
        let v = h.mock.shares.lock().unwrap().clone();
        if v.len() >= n {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("no {n}th share arrived");
}

#[tokio::test]
async fn a_pcm16_note_is_transcribed_posted_and_followed() {
    let h = harness().await;
    // The answer comes on Lucina's timeline stream; the status says "processing" throughout.
    h.mock.timeline.lock().unwrap().reply_after = Some(Duration::from_millis(300));
    h.mock.share.lock().unwrap().processing = 1000;
    h.mock.say("Ricordami di comprare il latte");
    let mut ws = h.connect().await;
    send(
        &mut ws,
        json!({"type": "start", "purpose": "note", "lang": "it", "channel": "ios",
               "sessionId": "ios_chat1", "replyTo": "itm_9", "source": "ios-chat"}),
    )
    .await;
    let l = expect_listening(&mut ws).await;
    assert_eq!((l["purpose"].as_str(), l["codec"].as_str()), (Some("note"), Some("pcm16")));
    let turn = l["turn"].as_str().unwrap().to_owned();
    // 3 s of speech and a second of silence at the end: a note is kept whole.
    let mut audio = tone(3000);
    audio.extend(silence(1000));
    stream_audio(&mut ws, &audio).await;
    send(&mut ws, json!({"type": "end"})).await;

    // transcript, then note (the device is free), then reply and done from the watcher.
    let t = recv_json(&mut ws).await;
    assert_eq!((t["type"].as_str(), t["text"].as_str()), (Some("transcript"), Some("Ricordami di comprare il latte")));
    let n = recv_json(&mut ws).await;
    assert_eq!(n["type"], "note");
    assert_eq!(n["shareId"], turn.as_str());
    assert!(n["userItemId"].as_str().unwrap().starts_with("usr_"));
    let r = recv_json(&mut ws).await;
    assert_eq!(
        r,
        json!({"type": "reply", "shareId": turn, "text": "Fatto, ho aggiunto il latte.", "itemId": "lci_42"})
    );
    let d = recv_json(&mut ws).await;
    assert_eq!((d["type"].as_str(), d["shareId"].as_str()), (Some("done"), Some(turn.as_str())));
    let ms = &d["ms"];
    for k in ["stt", "note", "reply", "total"] {
        assert!(ms[k].is_u64(), "{k}: {ms}");
    }
    assert!(ms["stt"].as_u64() <= ms["note"].as_u64() && ms["note"].as_u64() <= ms["reply"].as_u64());
    assert!(ms["reply"].as_u64() <= ms["total"].as_u64());

    // The share request.
    let calls = shares(&h, 1).await;
    assert_eq!(calls.len(), 1);
    let c = &calls[0];
    assert_eq!(c.auth.as_deref(), Some(format!("Bearer {}", h.token).as_str()));
    assert!(c.cookie.is_none() && c.csrf.is_none());
    assert!(c.content_type.starts_with("multipart/form-data; boundary="));
    for (k, v) in [
        ("shareId", turn.as_str()),
        ("intent", "ask"),
        ("lang", "it"),
        ("sessionId", "ios_chat1"),
        ("channel", "ios"),
        ("source", "ios-chat"),
        ("replyTo", "itm_9"),
    ] {
        assert_eq!(c.field(k), Some(v), "{k}: {:?}", c.fields);
    }
    assert!(c.field("note").is_none());
    let meta: Value = serde_json::from_str(c.field("meta").unwrap()).unwrap();
    let f = &meta["files"][0];
    assert_eq!((f["index"].as_u64(), f["voice"].as_bool()), (Some(0), Some(true)));
    assert_eq!(f["durationMs"], 4000);
    assert_eq!(f["transcript"], "Ricordami di comprare il latte");
    // The file: a 44-byte PCM header and the whole recording (silence included).
    assert_eq!(c.file_type, "audio/wav");
    assert!(c.file_name.starts_with("voice-") && c.file_name.ends_with(".wav") && c.file_name.len() == 6 + 15 + 4, "{}", c.file_name);
    assert_eq!(c.file.len(), 44 + audio.len());
    assert_eq!(c.file[..44], frames::wav_pcm16_header(audio.len() as u32, 16000)[..]);
    assert_eq!(c.file[44..], audio[..]);
    // The recording went to the speech-to-text as it was; Lucina's chat and Kokoro were not asked.
    let stt = h.mock.real_stt_calls();
    assert_eq!((stt.len(), stt[0].len), (1, audio.len()));
    assert!(h.mock.chats.lock().unwrap().is_empty() && h.mock.spoken().is_empty());
    // The status was read with the caller's credentials.
    // Followed by event: one status read when the stream was open, not a poll every 250 ms.
    let polls = h.mock.share_polls.lock().unwrap().clone();
    assert!((1..=2).contains(&polls.len()), "{polls:?}");
    assert_eq!(*h.mock.stream_opens.lock().unwrap(), 1);
    assert!(polls.iter().all(|(id, a)| *id == turn && a.as_deref() == Some(format!("Bearer {}", h.token).as_str())));
    // The server keeps a record of the note's turn too.
    let path = format!("/v1/voice/devices/{}/status", h.dev_id);
    let v = h.wait_for(&h.token, &path, |v| v["turns"][0]["ok"] == true).await;
    assert_eq!(v["turns"][0]["up_bytes"], audio.len());
    assert!(v["turns"][0]["spans"]["stt"].is_u64());
}

#[tokio::test]
async fn an_ima_adpcm_note_keeps_the_clients_blocks() {
    let h = harness().await;
    h.mock.say("Nota con blocchi");
    let mut ws = h.connect().await;
    send(
        &mut ws,
        json!({"type": "start", "purpose": "note", "codec": "ima-adpcm", "rate": 10909, "block": 256,
               "channel": "web", "source": "fortuna-chat"}),
    )
    .await;
    let l = expect_listening(&mut ws).await;
    let turn = l["turn"].as_str().unwrap().to_owned();
    let data = ima_encode(&tone_samples(10_909, 1500), 256);
    assert_eq!(data.len() % 256, 0);
    // A partial last block of 40 bytes: 4 header + 36 of codes -> 73 samples, kept in the file.
    let mut sent = data.clone();
    sent.extend_from_slice(&data[..40]);
    stream_bytes(&mut ws, &sent, 1000).await;
    send(&mut ws, json!({"type": "end"})).await;
    let out = {
        let mut v = vec![];
        loop {
            let g = recv(&mut ws).await;
            let last = matches!(&g, Got::Json(j) if j["type"] == "done");
            v.push(g);
            if last {
                break v;
            }
        }
    };
    assert_eq!(types(&out), ["transcript", "note", "reply", "done"], "{out:?}");
    assert_eq!(json_of(&out, "note")["shareId"], turn.as_str());

    let calls = shares(&h, 1).await;
    let c = &calls[0];
    assert_eq!(c.field("channel"), Some("web"));
    assert_eq!(c.field("source"), Some("fortuna-chat"));
    assert!(c.field("replyTo").is_none() && c.field("sessionId").is_none() && c.field("lang").is_none());
    // The client's bytes after a 60-byte IMA-ADPCM header.
    assert_eq!(c.file.len(), 60 + sent.len());
    assert_eq!(c.file[..60], frames::wav_adpcm_header(10909, 256, sent.len() as u32)[..]);
    assert_eq!(c.file[60..], sent[..]);
    assert_eq!(&c.file[0..4], b"RIFF");
    assert_eq!(u16::from_le_bytes([c.file[20], c.file[21]]), 0x0011);
    let want = frames::total_samples(sent.len(), 256);
    assert_eq!(u32::from_le_bytes(c.file[48..52].try_into().unwrap()) as usize, want);
    // The speech-to-text heard the decoded, resampled audio.
    let stt = h.mock.real_stt_calls();
    let samples = stt[0].len / 2;
    let expect = want * 16_000 / 10_909;
    assert!(samples.abs_diff(expect) <= 2, "{samples} vs {expect}");
    // Duration in the meta is the decoded audio's.
    let meta: Value = serde_json::from_str(c.field("meta").unwrap()).unwrap();
    let ms = meta["files"][0]["durationMs"].as_u64().unwrap();
    assert!(ms.abs_diff(want as u64 * 1000 / 10_909) <= 2, "{ms}");
}

#[tokio::test]
async fn a_new_start_is_accepted_while_a_notes_reply_is_pending() {
    let h = harness().await;
    h.mock.share.lock().unwrap().processing = 1000;
    h.mock.timeline.lock().unwrap().reply_after = Some(Duration::from_millis(1500));
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    let turn = expect_listening(&mut ws).await["turn"].as_str().unwrap().to_owned();
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    assert_eq!(recv_json(&mut ws).await["type"], "transcript");
    assert_eq!(recv_json(&mut ws).await["type"], "note");
    // The note's answer is pending; the device is free: a spoken turn goes through.
    let t0 = Instant::now();
    send(&mut ws, json!({"type": "start", "mode": "ptt"})).await;
    let l = expect_listening(&mut ws).await;
    assert_eq!(l["purpose"], "command");
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    // Both turns' messages arrive, in whatever order; the note's carry its shareId.
    let mut note_msgs = vec![];
    let mut turn_msgs = vec![];
    let mut audio_frames = 0;
    while note_msgs.len() < 2 || !turn_msgs.iter().any(|v: &Value| v["type"] == "done") {
        match recv(&mut ws).await {
            Got::Pcm(_) => audio_frames += 1,
            Got::Json(v) if v.get("shareId").is_some() => note_msgs.push(v),
            Got::Json(v) => turn_msgs.push(v),
        }
    }
    assert!(audio_frames > 0);
    let kinds: Vec<_> = turn_msgs.iter().map(|v| v["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["transcript", "audio", "reply", "done"]);
    assert_eq!(note_msgs[0]["type"], "reply");
    assert_eq!(note_msgs[1]["type"], "done");
    assert!(note_msgs.iter().all(|v| v["shareId"] == turn.as_str()));
    // The command turn did not wait for the note.
    assert!(t0.elapsed() < Duration::from_secs(5));
    // And the command was the voice thread's, the note a share.
    assert_eq!(h.mock.chats.lock().unwrap().len(), 1);
    assert_eq!(shares(&h, 1).await.len(), 1);
}

#[tokio::test]
async fn cancel_before_the_note_posts_nothing() {
    let h = harness().await;
    h.mock.say("Una nota");
    let mut ws = h.connect().await;
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(1000)).await;
    send(&mut ws, json!({"type": "cancel"})).await;
    // `end` after a cancel is late and ignored; the device is free for the next turn.
    send(&mut ws, json!({"type": "end"})).await;
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    expect_listening(&mut ws).await;
    let quiet = tokio::time::timeout(Duration::from_millis(500), ws.next()).await;
    assert!(quiet.is_err(), "{quiet:?}");
    assert!(h.mock.shares.lock().unwrap().is_empty());
    assert!(h.mock.real_stt_calls().is_empty());
}

#[tokio::test]
async fn notes_are_off_when_the_share_url_is_off() {
    let h = harness_with(|cfg| cfg.voice_share_url = None).await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    let e = recv_json(&mut ws).await;
    assert_eq!((e["code"].as_str(), e["message"].as_str()), (Some("bad_request"), Some("voice notes are off")));
    // Commands are not affected.
    let out = say_ptt(&mut ws, 600).await;
    assert_eq!(json_of(&out, "done")["type"], "done");
}

#[tokio::test]
async fn a_refused_share_is_note_failed_and_the_device_is_free() {
    let h = harness().await;
    h.mock.share.lock().unwrap().refuse = Some(400);
    h.mock.say("Una nota");
    let mut ws = h.connect().await;
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    assert_eq!(recv_json(&mut ws).await["type"], "transcript");
    let e = recv_json(&mut ws).await;
    assert_eq!(e["type"], "error");
    assert_eq!(e["code"], "note_failed");
    assert!(e["message"].as_str().unwrap().contains("400"), "{e}");
    assert!(e.get("shareId").is_none());
    // No watcher, no note; the next turn starts at once.
    h.mock.share.lock().unwrap().refuse = None;
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    expect_listening(&mut ws).await;
    assert!(h.mock.share_polls.lock().unwrap().is_empty());
    // The failed turn left a record with its error.
    let path = format!("/v1/voice/devices/{}/status", h.dev_id);
    let v = h.wait_for(&h.token, &path, |v| v["turns"][0]["err"] == "note_failed").await;
    assert_eq!(v["turns"][0]["ok"], false);
}

#[tokio::test]
async fn a_share_that_ends_in_error_is_lucina_unavailable_with_its_share_id() {
    let h = harness().await;
    h.mock.share.lock().unwrap().ends_in_error = true;
    h.mock.say("Una nota");
    let mut ws = h.connect().await;
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    let turn = expect_listening(&mut ws).await["turn"].as_str().unwrap().to_owned();
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    assert_eq!(recv_json(&mut ws).await["type"], "transcript");
    assert_eq!(recv_json(&mut ws).await["type"], "note");
    let e = recv_json(&mut ws).await;
    assert_eq!(
        (e["type"].as_str(), e["code"].as_str(), e["shareId"].as_str()),
        (Some("error"), Some("lucina_unavailable"), Some(turn.as_str())),
        "{e}"
    );
}

#[tokio::test]
async fn a_failed_local_stt_still_posts_the_note_without_a_transcript() {
    let h = harness().await;
    *h.mock.stt_fails.lock().unwrap() = true;
    let mut ws = h.connect().await;
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    // No `transcript`: the first message is the note; lucina-rs transcribes the file itself.
    assert_eq!(recv_json(&mut ws).await["type"], "note");
    let c = &shares(&h, 1).await[0];
    let meta: Value = serde_json::from_str(c.field("meta").unwrap()).unwrap();
    assert!(meta["files"][0].get("transcript").is_none());
    assert_eq!(meta["files"][0]["voice"], true);
    // A note too short to be speech is no_speech and posts nothing more.
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(100)).await;
    send(&mut ws, json!({"type": "end"})).await;
    loop {
        let v = recv_json(&mut ws).await;
        if v["type"] == "error" {
            assert_eq!(v["code"], "no_speech");
            break;
        }
    }
    assert_eq!(h.mock.shares.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn at_most_four_notes_are_followed_and_the_oldest_is_dropped() {
    let h = harness().await;
    // Never answers within this test.
    h.mock.share.lock().unwrap().processing = usize::MAX;
    h.mock.timeline.lock().unwrap().reply_after = Some(Duration::from_secs(3600));
    h.mock.say("Una nota");
    let mut ws = h.connect().await;
    for _ in 0..5 {
        send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
        expect_listening(&mut ws).await;
        stream_audio(&mut ws, &tone(500)).await;
        send(&mut ws, json!({"type": "end"})).await;
        assert_eq!(recv_json(&mut ws).await["type"], "transcript");
        assert_eq!(recv_json(&mut ws).await["type"], "note");
    }
    // Each watcher holds a timeline stream: five were opened, the oldest watcher was dropped, so
    // four are open now. No fast polling: one status read per note.
    for _ in 0..100 {
        if *h.mock.streams_open.lock().unwrap() == 4 && *h.mock.stream_opens.lock().unwrap() == 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(*h.mock.stream_opens.lock().unwrap(), 5);
    assert_eq!(*h.mock.streams_open.lock().unwrap(), 4);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(h.mock.share_polls.lock().unwrap().len(), 5);
    drop(ws);
    for _ in 0..100 {
        if *h.mock.streams_open.lock().unwrap() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("watchers outlived the connection");
}

#[tokio::test]
async fn a_dropped_timeline_stream_is_reopened_once() {
    let h = harness().await;
    {
        let mut t = h.mock.timeline.lock().unwrap();
        t.reply_after = Some(Duration::from_millis(200));
        t.drop_first = true;
    }
    h.mock.share.lock().unwrap().processing = 1000;
    h.mock.say("Una nota");
    let mut ws = h.connect().await;
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(500)).await;
    send(&mut ws, json!({"type": "end"})).await;
    assert_eq!(recv_json(&mut ws).await["type"], "transcript");
    assert_eq!(recv_json(&mut ws).await["type"], "note");
    let r = recv_json(&mut ws).await;
    assert_eq!((r["type"].as_str(), r["itemId"].as_str()), (Some("reply"), Some("lci_42")), "{r}");
    assert_eq!(recv_json(&mut ws).await["type"], "done");
    assert_eq!(*h.mock.stream_opens.lock().unwrap(), 2);
    assert!(h.mock.share_polls.lock().unwrap().len() <= 2);
}

#[tokio::test]
async fn thread_main_sends_the_chat_with_the_clients_channel_and_session() {
    let h = harness().await;
    h.mock.say(TWO);
    // The upgrade's X-Secretary-Client wins over the start's channel.
    let mut ws = h.connect_with(&[("x-secretary-client", "ios")]).await;
    send(&mut ws, json!({"type": "start", "thread": "main", "sessionId": "sess_7", "channel": "web"})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    let out = turn_output(&mut ws).await;
    assert_eq!(json_of(&out, "done")["type"], "done");
    // Without the header: the start's channel, else web.
    let mut ws2 = h.connect().await;
    send(&mut ws2, json!({"type": "start", "thread": "main", "channel": "ios"})).await;
    expect_listening(&mut ws2).await;
    stream_audio(&mut ws2, &tone(800)).await;
    send(&mut ws2, json!({"type": "end"})).await;
    turn_output(&mut ws2).await;
    send(&mut ws2, json!({"type": "start", "thread": "main"})).await;
    expect_listening(&mut ws2).await;
    stream_audio(&mut ws2, &tone(800)).await;
    send(&mut ws2, json!({"type": "end"})).await;
    turn_output(&mut ws2).await;
    let chats = h.mock.chats.lock().unwrap().clone();
    let seen: Vec<_> = chats
        .iter()
        .map(|(b, _)| (b["channel"].as_str().unwrap().to_owned(), b["sessionId"].as_str().map(str::to_owned)))
        .collect();
    assert_eq!(seen[0], ("ios".to_owned(), Some("sess_7".to_owned())));
    assert_eq!(seen[1].0, "ios");
    assert_eq!(seen[2].0, "web");
    // Still a streaming, spoken-style, device-scoped request.
    assert_eq!(chats[0].1.as_deref(), Some("text/event-stream"));
    assert_eq!(chats[0].0["voice"], true);
    assert_eq!(chats[0].0["deviceName"], "Speaker");
    // A bad X-Secretary-Client is refused at `start` for the main thread only.
    let mut ws3 = h.connect_with(&[("x-secretary-client", "voice")]).await;
    send(&mut ws3, json!({"type": "start", "thread": "main"})).await;
    assert_eq!(recv_json(&mut ws3).await["code"], "bad_request");
    send(&mut ws3, json!({"type": "start"})).await;
    expect_listening(&mut ws3).await;
}

#[tokio::test]
async fn text_true_sends_deltas_then_the_answer_beside_the_audio() {
    let h = harness().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    send(&mut ws, json!({"type": "start", "mode": "ptt", "text": true})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    let out = turn_output(&mut ws).await;
    let deltas: Vec<_> = out
        .iter()
        .filter_map(|g| match g {
            Got::Json(v) if v["type"] == "delta" => Some(v["text"].as_str().unwrap().to_owned()),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, ["Ciao a te. ", "Come stai", " oggi?"]);
    let answer = json_of(&out, "answer");
    assert_eq!(answer["text"], "Ciao a te. Come stai oggi?");
    assert_eq!(answer["replaces"], false);
    // Order: transcript, the deltas, then the answer; the audio and the rest as before.
    let order: Vec<_> = types(&out);
    let pos = |t: &str| order.iter().position(|x| x == t).unwrap();
    assert!(pos("transcript") < pos("delta") && pos("delta") < pos("answer"));
    assert!(order.iter().filter(|t| *t == "delta").count() >= 1);
    assert!(pos("answer") < pos("reply") && pos("reply") < pos("done"));
    assert_eq!(audio(&out), [pcm_of("Ciao a te.".len()), pcm_of("Come stai oggi?".len())].concat());
    assert_eq!(json_of(&out, "reply")["text"], "Ciao a te. Come stai oggi?");

    // A sensitive reply: the fixed phrase replaces what was shown.
    h.mock.say(SECRET);
    send(&mut ws, json!({"type": "start", "mode": "ptt", "text": true})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    let out = turn_output(&mut ws).await;
    let answer = json_of(&out, "answer");
    assert_eq!(answer["text"], "I'll answer that in the app.");
    assert_eq!(answer["replaces"], true);
    assert!(!out.iter().any(|g| matches!(g, Got::Json(v) if v["text"].as_str().is_some_and(|t| t.contains("482913")) && v["type"] == "answer")));

    // Without `text`, none of these messages exist.
    h.mock.say(TWO);
    let out = say_ptt(&mut ws, 800).await;
    assert!(!types(&out).iter().any(|t| t == "delta" || t == "answer"), "{:?}", types(&out));
}

#[tokio::test]
async fn speak_false_makes_no_kokoro_call_and_sends_no_audio() {
    let h = harness().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    send(&mut ws, json!({"type": "start", "mode": "ptt", "speak": false})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    let out = turn_output(&mut ws).await;
    assert!(!out.iter().any(|g| matches!(g, Got::Pcm(_))), "{out:?}");
    let t = types(&out);
    assert_eq!(t.first().map(String::as_str), Some("transcript"));
    assert!(!t.iter().any(|x| x == "audio"));
    // `text` is implied: deltas, the answer, then reply and done.
    assert_eq!(t.iter().filter(|x| *x == "delta").count(), 3, "{t:?}");
    assert_eq!(&t[t.len() - 3..], ["answer", "reply", "done"]);
    assert_eq!(json_of(&out, "answer")["text"], "Ciao a te. Come stai oggi?");
    assert_eq!(json_of(&out, "reply")["text"], "Ciao a te. Come stai oggi?");
    let ms = &json_of(&out, "done")["ms"];
    assert!(ms["stt"].is_u64() && ms["lucina_first"].is_u64() && ms["total"].is_u64(), "{ms}");
    assert!(ms.get("tts_first").is_none() && ms.get("first_audio").is_none(), "{ms}");
    assert!(h.mock.spoken().is_empty(), "Kokoro was asked: {:?}", h.mock.spoken());

    // No filler either, on a turn that would have one.
    h.mock.say(ACK_SLOW);
    send(&mut ws, json!({"type": "start", "mode": "ptt", "speak": false})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    let out = turn_output(&mut ws).await;
    assert!(!out.iter().any(|g| matches!(g, Got::Pcm(_))));
    let ms = &json_of(&out, "done")["ms"];
    assert!(ms["route"].is_u64() && ms.get("ack").is_none() && ms.get("first_audio").is_none(), "{ms}");
    assert!(h.mock.spoken().is_empty());

    // A sensitive reply, and the JSON-only Lucina, work without speech too.
    h.mock.say(SECRET);
    send(&mut ws, json!({"type": "start", "mode": "ptt", "speak": false})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    let out = turn_output(&mut ws).await;
    assert_eq!(json_of(&out, "answer")["text"], "I'll answer that in the app.");
    h.mock.say(JSON_REPLY);
    send(&mut ws, json!({"type": "start", "mode": "ptt", "speak": false})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    let out = turn_output(&mut ws).await;
    assert_eq!(json_of(&out, "answer")["text"], "Prima frase. Seconda frase lunga.");
    assert_eq!(json_of(&out, "answer")["replaces"], true);
    assert!(h.mock.spoken().is_empty());
}

#[tokio::test]
async fn a_cookie_callers_note_reaches_the_share_with_the_cookie_and_csrf() {
    let h = harness().await;
    h.mock.say("Una nota");
    let login = h
        .http
        .post(format!("http://{}/v1/session", h.addr))
        .json(&json!({"token": h.token}))
        .send()
        .await
        .unwrap();
    let cookie = login.headers()["set-cookie"].to_str().unwrap().to_owned();
    let cookie = cookie.split(';').next().unwrap().to_owned();
    let mut req = format!("ws://{}/v1/voice/ws", h.addr).into_client_request().unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    loop {
        if recv_json(&mut ws).await["type"] == "done" {
            break;
        }
    }
    let c = &shares(&h, 1).await[0];
    assert_eq!(c.cookie.as_deref(), Some(cookie.as_str()));
    assert_eq!(c.csrf.as_deref(), Some("1"));
    assert!(c.auth.is_none());
    // No X-Secretary-Client and no channel: the `/v1/chat` default.
    assert_eq!(c.field("channel"), Some("web"));
    assert!(h.mock.share_polls.lock().unwrap().iter().all(|(_, a)| a.is_none()));
}

#[tokio::test]
async fn a_long_note_is_not_transcribed_by_the_server() {
    let h = harness().await;
    h.mock.say("Non sentita");
    let mut ws = h.connect().await;
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    expect_listening(&mut ws).await;
    // 125 s: past the 120 s the local engine is asked for.
    let audio = tone(125_000);
    stream_audio(&mut ws, &audio).await;
    send(&mut ws, json!({"type": "end"})).await;
    let n = recv_json(&mut ws).await;
    assert_eq!(n["type"], "note", "no transcript expected: {n}");
    let d = loop {
        let v = recv_json(&mut ws).await;
        if v["type"] == "done" {
            break v;
        }
    };
    assert!(d["ms"].get("stt").is_none(), "{d}");
    assert!(h.mock.real_stt_calls().is_empty());
    let c = &shares(&h, 1).await[0];
    let meta: Value = serde_json::from_str(c.field("meta").unwrap()).unwrap();
    assert!(meta["files"][0].get("transcript").is_none());
    assert_eq!(meta["files"][0]["durationMs"], 125_000);
    assert_eq!(c.file.len(), 44 + audio.len());
}

#[tokio::test]
async fn the_command_cap_counts_decoded_audio() {
    let h = harness().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    send(
        &mut ws,
        json!({"type": "start", "mode": "ptt", "codec": "ima-adpcm", "rate": 8000, "block": 256}),
    )
    .await;
    expect_listening(&mut ws).await;
    // 40 s of tone at 8 kHz is about 160 KB of ADPCM and 1.25 MB of 16 kHz PCM: over the 1 MiB cap.
    let data = ima_encode(&tone_samples(8000, 40_000), 256);
    assert!(data.len() < 200_000);
    stream_bytes(&mut ws, &data, 4096).await;
    // The server stops listening by itself (no `end`) and runs the turn.
    assert_eq!(recv_json(&mut ws).await, json!({"type": "endpoint"}));
    let out = turn_output(&mut ws).await;
    assert_eq!(json_of(&out, "done")["type"], "done", "{out:?}");
    let calls = h.mock.real_stt_calls();
    assert_eq!(calls[0].len, 1024 * 1024);
}

// -- quiet input (protocol section 10.7) -----------------------------------------------------------------------------------

const SILENT_MESSAGE: &str = "The recording was silent: check the microphone.";

#[tokio::test]
async fn a_silent_command_turn_is_error_silent_and_runs_no_stt_or_chat() {
    let h = harness().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    // Digital silence, and a hiss at -70 dBFS (peak 8): both are silent. The hiss is not all zeros, so a speech-to-text call
    // would show up in `real_stt_calls`.
    for audio in [silence(1500), hiss(1500, 8)] {
        start_ptt(&mut ws).await;
        stream_audio(&mut ws, &audio).await;
        send(&mut ws, json!({"type": "end"})).await;
        let e = recv_json(&mut ws).await;
        assert_eq!(e, json!({"type": "error", "code": "silent", "message": SILENT_MESSAGE}));
    }
    assert!(h.mock.real_stt_calls().is_empty());
    assert!(h.mock.chats.lock().unwrap().is_empty() && h.mock.spoken().is_empty());
    // The socket and the device are free: a real turn runs next.
    let out = say_ptt(&mut ws, 800).await;
    assert_eq!(json_of(&out, "done")["type"], "done", "{out:?}");
    // The server's record of the silent turn: the error and the level, numbers only.
    let path = format!("/v1/voice/devices/{}/status", h.dev_id);
    let v = h.wait_for(&h.token, &path, |v| v["turns"].as_array().is_some_and(|t| t.len() == 3)).await;
    let silent = &v["turns"][2];
    assert_eq!(silent["err"], "silent");
    for k in ["in_peak_dbfs", "in_rms_dbfs", "speech_rms_dbfs", "gain_db"] {
        assert!(silent[k].is_f64(), "{k}: {silent}");
    }
    assert_eq!(silent["gain_db"], 0.0);
}

#[tokio::test]
async fn a_silent_note_is_not_posted() {
    let h = harness().await;
    h.mock.say("Una nota");
    let mut ws = h.connect().await;
    for audio in [silence(2000), hiss(2000, 8)] {
        send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
        expect_listening(&mut ws).await;
        stream_audio(&mut ws, &audio).await;
        send(&mut ws, json!({"type": "end"})).await;
        let e = recv_json(&mut ws).await;
        assert_eq!(e, json!({"type": "error", "code": "silent", "message": SILENT_MESSAGE}));
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(h.mock.shares.lock().unwrap().is_empty());
    assert!(h.mock.real_stt_calls().is_empty());
}

#[tokio::test]
async fn quiet_speech_is_lifted_for_the_stt_and_the_note_file_stays_as_recorded() {
    let h = harness().await;
    h.mock.share.lock().unwrap().processing = 1000;
    h.mock.say("Ricordami di comprare il latte");
    let mut ws = h.connect().await;
    // About -33 dBFS speech and a -30 dBFS peak: the DSi's note, a little louder.
    let quiet = quiet_speech(4000, 1000.0);
    let before = peak_of(&quiet);
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &quiet).await;
    send(&mut ws, json!({"type": "end"})).await;
    assert_eq!(recv_json(&mut ws).await["type"], "transcript");
    assert_eq!(recv_json(&mut ws).await["type"], "note");

    // The speech-to-text got more than 3x the peak, never past -1 dBFS; the same length.
    let stt = h.mock.real_stt_calls();
    assert_eq!(stt.len(), 1);
    assert_eq!(stt[0].len, quiet.len());
    assert!(stt[0].peak > 3 * before, "{} vs {before}", stt[0].peak);
    assert!(stt[0].peak <= 29_205, "{}", stt[0].peak);
    // The file posted is the recording exactly as it was.
    let c = &shares(&h, 1).await[0];
    assert_eq!(c.file[44..], quiet[..]);
    // The record says what was measured and applied.
    let path = format!("/v1/voice/devices/{}/status", h.dev_id);
    let v = h.wait_for(&h.token, &path, |v| v["turns"][0]["ok"] == true).await;
    let t = &v["turns"][0];
    assert!((t["in_peak_dbfs"].as_f64().unwrap() + 30.0).abs() < 1.0, "{t}");
    assert!(t["in_rms_dbfs"].as_f64().unwrap() < -35.0, "{t}");
    let speech = t["speech_rms_dbfs"].as_f64().unwrap();
    assert!(speech > -35.0 && speech < -30.0, "{t}");
    let gain = t["gain_db"].as_f64().unwrap();
    assert!(gain > 10.0 && gain <= 30.0, "{t}");
}

#[tokio::test]
async fn a_quiet_command_turn_is_lifted_too_and_loud_speech_is_left_alone() {
    let h = harness().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    start_ptt(&mut ws).await;
    let quiet = quiet_speech(3000, 1000.0);
    stream_audio(&mut ws, &quiet).await;
    send(&mut ws, json!({"type": "end"})).await;
    let out = turn_output(&mut ws).await;
    assert_eq!(json_of(&out, "done")["type"], "done", "{out:?}");
    let stt = h.mock.real_stt_calls();
    assert!(stt[0].peak > 3 * peak_of(&quiet) && stt[0].peak <= 29_205, "{}", stt[0].peak);

    // A tone at -12 dBFS is already above the target: not touched.
    let out = say_ptt(&mut ws, 800).await;
    assert_eq!(json_of(&out, "done")["type"], "done");
    let stt = h.mock.real_stt_calls();
    assert_eq!(stt[1].peak, peak_of(&tone(800)));
}

// -- chat voice is never a memo (owner rule) ---------------------------------------------------------------------------------

#[tokio::test]
async fn command_and_note_turns_make_no_memo_request_but_the_stt() {
    let h = harness().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 800).await;
    assert_eq!(json_of(&out, "done")["type"], "done");
    h.mock.say("Ricordami di comprare il latte");
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(1200)).await;
    send(&mut ws, json!({"type": "end"})).await;
    assert_eq!(recv_json(&mut ws).await["type"], "transcript");
    assert_eq!(recv_json(&mut ws).await["type"], "note");
    shares(&h, 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Every call to the memo API was the speech-to-text; the mock records any other one.
    assert!(h.mock.memo_calls.lock().unwrap().is_empty(), "{:?}", h.mock.memo_calls.lock().unwrap());
    assert!(!h.mock.stt_calls.lock().unwrap().is_empty());
    // And the only thing posted to the share is the note itself (no memo, no summary, no brief).
    assert_eq!(h.mock.shares.lock().unwrap().len(), 1);
}

// -- test devices (protocol section 10.8) ----------------------------------------------------------------------------------

#[tokio::test]
async fn a_command_turn_of_a_test_device_is_flagged_and_in_the_test_session() {
    let h = harness_test_device().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    let out = say_ptt(&mut ws, 800).await;
    assert_eq!(json_of(&out, "done")["type"], "done", "{out:?}");
    // With thread "main" and a session of its own, the test session still wins: the owner's session is never used.
    send(
        &mut ws,
        json!({"type": "start", "mode": "ptt", "thread": "main", "channel": "ios", "sessionId": "ios_chat1", "speak": false}),
    )
    .await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(800)).await;
    send(&mut ws, json!({"type": "end"})).await;
    turn_output(&mut ws).await;
    let chats = h.mock.chats.lock().unwrap().clone();
    assert_eq!(chats.len(), 2);
    for (body, _) in &chats {
        assert_eq!(body["test"], true, "{body}");
        assert_eq!(body["sessionId"], "voice-test", "{body}");
    }
    assert_eq!(chats[0].0["channel"], "voice");
    assert_eq!(chats[1].0["channel"], "ios");
}

#[tokio::test]
async fn a_command_turn_of_any_other_device_is_not_flagged() {
    let h = harness().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    say_ptt(&mut ws, 800).await;
    let chats = h.mock.chats.lock().unwrap().clone();
    assert!(chats[0].0.get("test").is_none(), "{}", chats[0].0);
    assert_eq!(chats[0].0["sessionId"], "lucinavoice");
}

#[tokio::test]
async fn a_note_of_a_test_device_posts_the_share_with_test_1() {
    let h = harness_test_device().await;
    h.mock.share.lock().unwrap().processing = 1000;
    h.mock.say("Una nota");
    let mut ws = h.connect().await;
    send(&mut ws, json!({"type": "start", "purpose": "note", "sessionId": "ios_chat1"})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(1000)).await;
    send(&mut ws, json!({"type": "end"})).await;
    assert_eq!(recv_json(&mut ws).await["type"], "transcript");
    assert_eq!(recv_json(&mut ws).await["type"], "note");
    let c = &shares(&h, 1).await[0];
    assert_eq!(c.field("test"), Some("1"));
    // The proof for this very share, under the Lucina token; the token itself is not in any header.
    let marker = c.voice_test.as_deref().expect("X-Voice-Test on a test device's note");
    let turn = c.field("shareId").unwrap();
    assert!(secretary_server::voice_marker::verify(LUCINA_TOKEN.as_bytes(), turn, marker), "{marker}");
    assert!(!secretary_server::voice_marker::verify(LUCINA_TOKEN.as_bytes(), "another-share-id", marker));
    assert!(!marker.contains(LUCINA_TOKEN));

    // Another device's note has no such field.
    let h = harness().await;
    h.mock.share.lock().unwrap().processing = 1000;
    h.mock.say("Una nota");
    let mut ws = h.connect().await;
    send(&mut ws, json!({"type": "start", "purpose": "note"})).await;
    expect_listening(&mut ws).await;
    stream_audio(&mut ws, &tone(1000)).await;
    send(&mut ws, json!({"type": "end"})).await;
    assert_eq!(recv_json(&mut ws).await["type"], "transcript");
    assert_eq!(recv_json(&mut ws).await["type"], "note");
    let c = &shares(&h, 1).await[0];
    assert_eq!(c.field("test"), None);
    assert!(c.voice_test.is_none());
}

#[tokio::test]
async fn a_client_chat_cannot_flag_itself_as_a_test() {
    // `/v1/chat` clears the field: only the voice socket's own pipeline sets it.
    let h = harness().await;
    let r = h
        .http
        .post(format!("http://{}/v1/chat", h.addr))
        .bearer_auth(&h.token)
        .json(&json!({"message": "ciao", "test": true, "sessionId": "voice-test"}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success() || r.status().is_server_error(), "{}", r.status());
    let chats = h.mock.chats.lock().unwrap().clone();
    assert_eq!(chats.len(), 1);
    assert!(chats[0].0.get("test").is_none(), "{}", chats[0].0);
}
