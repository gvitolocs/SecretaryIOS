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
    stt_calls: Mutex<Vec<SttCall>>,
    chats: Mutex<Vec<(Value, Option<String>)>>,
    /// (input text, when it arrived)
    speech: Mutex<Vec<(String, Instant)>>,
    /// How long Kokoro takes before answering.
    kokoro_delay: Mutex<Duration>,
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
    Json(json!({"text": text, "language": "it", "ms": 5, "mode": "gpu"})).into_response()
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
    let (_, token) = DeviceStore::new(&cfg.data_dir).issue("Speaker").unwrap();
    let addr = spawn(app(AppState::new(&cfg).unwrap())).await;
    H {
        addr,
        http: Client::new(),
        token,
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
