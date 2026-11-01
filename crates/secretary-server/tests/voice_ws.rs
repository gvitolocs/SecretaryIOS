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
}

impl Mock {
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
                (0, "ack", json!({"text": "Un attimo…"})),
                (250, "delta", d("Ecco le notizie. ")),
                (0, "reply", reply("Ecco le notizie.", false, false)),
                (0, "done", done()),
            ],
            None,
            "",
        ),
        ACK_LATE => sse(
            vec![
                (0, "route", route("web_crawl", false)),
                (0, "delta", d("Prima frase detta. ")),
                (0, "ack", json!({"text": "Un attimo…"})),
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
                (0, "ack", json!({"text": "Un attimo…"})),
                (100, "ack", json!({"text": "Ancora un attimo…"})),
                (150, "delta", d("Fatto qui. ")),
                (0, "reply", reply("Fatto qui.", false, false)),
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
    let mock = Arc::new(Mock::default());
    let lucina_url = spawn(Router::new().fallback(lucina).with_state(mock.clone())).await;
    let tts = spawn(Router::new().fallback(kokoro).with_state(mock.clone())).await;
    let stt = spawn(Router::new().fallback(local_stt).with_state(mock.clone())).await;
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::new(format!("http://{lucina_url}"), Some(LUCINA_TOKEN.into()));
    cfg.data_dir = dir.path().join("state");
    cfg.tts_url = format!("http://{tts}/");
    cfg.voice_stt_url = Some(format!("http://{stt}/v1/memos/stt"));
    let (dev, token) = DeviceStore::new(&cfg.data_dir).issue("Speaker").unwrap();
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

/// Stream `audio` like the speaker: ~32 ms (1024-byte) frames.
async fn stream_audio(ws: &mut Ws, audio: &[u8]) {
    for f in audio.chunks(1024) {
        ws.send(Message::binary(f.to_vec())).await.unwrap();
    }
}

async fn start_ptt(ws: &mut Ws) {
    send(ws, json!({"type": "start", "mode": "ptt"})).await;
    assert_eq!(recv_json(ws).await, json!({"type": "listening"}));
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
    // `start` warmed the engine with 0.5 s of silence, once.
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
async fn vad_mode_ends_the_turn_by_itself() {
    let h = harness().await;
    h.mock.say(TWO);
    let mut ws = h.connect().await;
    send(
        &mut ws,
        json!({"type": "start", "mode": "vad", "lang": "it"}),
    )
    .await;
    assert_eq!(recv_json(&mut ws).await, json!({"type": "listening"}));
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
                assert_eq!(v, json!({"type": "listening"}), "{v}");
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
    assert_eq!(json_of(&out, "reply")["text"], "Ti rispondo nell'app.");
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
    stream_audio(&mut ws, &silence(1500)).await;
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

/// The cache holds both languages' phrases (`Un attimo…`, `Vediamo…`, `One moment…`, `Let me see…`).
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
    let answer = pcm_of("Ecco le notizie.".len());
    assert_eq!(pcm.len(), 6 + answer.len(), "filler + answer");
    assert!(
        matches!(first_len(&pcm), 10 | 8),
        "an Italian filler comes first: {}",
        first_len(&pcm)
    );
    assert_eq!(pcm[6..], answer[..], "the answer follows, whole");
    // The filler came from memory: Kokoro only heard the answer.
    assert_eq!(h.mock.spoken(), ["Ecco le notizie."]);
    // The filler is not part of the spoken reply.
    assert_eq!(json_of(&out, "reply")["text"], "Ecco le notizie.");

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
    assert_eq!(recv_json(&mut ws).await, json!({"type": "listening"}));
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
        6 + pcm_of("Fatto qui.".len()).len(),
        "one filler"
    );
    assert_eq!(
        h.mock.spoken().last().map(String::as_str),
        Some("Fatto qui.")
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
            pcm_of("Un attimo…".chars().count()),
            pcm_of("Ecco le notizie.".len())
        ]
        .concat(),
        "{out:?}"
    );
    assert_eq!(json_of(&out, "reply")["text"], "Ecco le notizie.");
    assert!(json_of(&out, "done")["ms"]["ack"].is_u64());
    fillers_ready(&h).await;

    // The next one plays from memory: Kokoro hears only the answer.
    h.mock.speech.lock().unwrap().clear();
    let out = say_ptt(&mut ws, 900).await;
    let pcm = audio(&out);
    assert!(matches!(first_len(&pcm), 10 | 8), "{}", first_len(&pcm));
    assert_eq!(pcm.len(), 6 + pcm_of("Ecco le notizie.".len()).len());
    assert_eq!(h.mock.spoken(), ["Ecco le notizie."]);
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
    assert_eq!(r.headers()["x-tts-voice"], "if_sara");
    let all = r.bytes().await.unwrap();
    assert!(matches!(first_len(&all), 10 | 8));
    assert_eq!(all[6..], pcm_of("Ecco le notizie.".len())[..]);
    let path = format!("/v1/voice/devices/{}/status", h.dev_id);
    let v = h
        .wait_for(&h.token, &path, |v| v["turns"][0]["intent"] == "web_crawl")
        .await;
    assert!(v["turns"][0]["spans"]["ack"].is_u64());
}
