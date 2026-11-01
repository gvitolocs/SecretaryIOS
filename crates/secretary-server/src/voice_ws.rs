//! `GET /v1/voice/ws`: the persistent WebSocket of the LucinaVoice speaker.
//! The contract is `docs/lucinavoice-protocol.md` section 2; the text here is
//! what this implementation does with it.
//!
//! **Connection.** Device auth on the upgrade request (`Authorization: Bearer`,
//! or the browser cookie; a cookie caller's `Origin`, if sent, must be this
//! server's own, else `403 forbidden_origin`), then one socket per device, kept open. The server
//! pings every [`PING_EVERY`]; a socket that sent nothing (pongs included) for
//! [`IDLE`] is closed. The socket also closes on server shutdown. One turn at
//! a time per connection, and per device across `POST /v1/voice` and sockets
//! (the [`crate::voice::Voice`] busy set); turns share `POST /v1/voice`'s rate
//! limit.
//!
//! **Client → server.** Text `start {mode: "ptt"|"vad", lang?: "it"|"en"}`,
//! binary s16le 16 kHz mono PCM, text `end`, text `cancel`. Frames or `end`
//! with no turn running are ignored (they may be late); a `start` while a turn
//! runs is answered with `busy` and changes nothing.
//!
//! The device's `hello` and `telemetry` text frames (protocol section 4) are
//! accepted and ignored: nothing is stored yet (section 5).
//!
//! **Turn.**
//! 1. `start` → `listening`; the local speech-to-text is warmed in the
//!    background ([`crate::voice::Voice::warm_stt`]).
//! 2. PCM is collected (at most [`crate::voice::MAX_BODY`] bytes, about 32 s)
//!    and watched by the energy VAD ([`crate::voice_vad`]). In `vad` mode, the
//!    end of speech sends `endpoint` and starts the turn; in `ptt` mode the VAD
//!    only trims trailing silence and `end` starts it. Reaching the size cap
//!    also sends `endpoint` and starts the turn, in either mode.
//! 3. The turn (t0 = that moment): speech-to-text ([`crate::voice::transcribe`],
//!    local first, Gemini as fallback) → `transcript` → the spoken pipeline
//!    ([`crate::voice_pipe`]) → `audio {rate}` before the first binary frame,
//!    then frames of at most 8 KiB → `reply {text}` (what was spoken) → `done
//!    {ms}`.
//! 4. A turn that ends without complete audio sends `error` instead of `done`:
//!    `no_speech` (under 0.3 s of audio, no speech in `vad` mode, or an empty
//!    transcript), `stt_unavailable`, `lucina_unavailable`, `tts_failed`. The
//!    connection stays open. `busy`, `bad_request` and `rate_limited` answer a
//!    bad or refused `start` (or a message that is not JSON of the contract).
//! 5. `cancel` aborts the turn wherever it is (collecting, speech-to-text,
//!    chat, speech); closing the socket does the same. A chat already sent to
//!    Lucina is not recalled.
//!
//! Timings are logged at info with the device id (never the transcript, the
//! audio or credentials).

use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Extension;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::auth::{Identity, Via};
use crate::error::ApiError;
use crate::tts;
use crate::voice::{self, Audio, Turn, MAX_BODY};
use crate::voice_pipe::{self, Shared, Spec};
use crate::voice_vad::Vad;
use crate::AppState;

/// Server ping interval.
pub const PING_EVERY: Duration = Duration::from_secs(20);
/// A socket silent for this long is dead (two and a half pings).
pub const IDLE: Duration = Duration::from_secs(50);
/// Largest message accepted (audio frames are at most 8 KiB).
const MAX_MESSAGE: usize = 64 * 1024;
/// Messages queued for the writer before senders wait.
const OUTBOX: usize = 64;
/// Less audio than this (after trimming) is not speech: [`MIN_SECONDS`] of 16 kHz s16le.
const MIN_BYTES: usize = 9_600;

pub async fn upgrade(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let Ok(ws) = ws else {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "a WebSocket upgrade is required",
        )
        .into_response();
    };
    // A browser (cookie) caller must come from this site: the cookie is
    // SameSite=Strict already; this is the second lock.
    if matches!(ident.via, Via::Cookie) {
        let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
        let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
        if let Some(origin) = origin {
            if !host.is_some_and(|h| origin_matches(origin, h)) {
                return ApiError::new(
                    StatusCode::FORBIDDEN,
                    "forbidden_origin",
                    "the WebSocket must be opened from this site",
                )
                .into_response();
            }
        }
    }
    // Only the caller's credentials are kept: they go to the local speech-to-text.
    let mut creds = HeaderMap::new();
    for name in [header::AUTHORIZATION, header::COOKIE] {
        if let Some(v) = headers.get(&name) {
            creds.insert(name, v.clone());
        }
    }
    ws.max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| connection(st, ident, creds, socket))
}

/// `Origin: https://host[:port]` is `Host: host[:port]`.
fn origin_matches(origin: &str, host: &str) -> bool {
    origin
        .split_once("://")
        .is_some_and(|(_, rest)| rest.trim_end_matches('/').eq_ignore_ascii_case(host))
}

#[derive(Deserialize)]
struct Command {
    #[serde(rename = "type")]
    kind: String,
    mode: Option<String>,
    lang: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Ptt,
    Vad,
}

/// The recording so far: whole samples only.
#[derive(Default)]
struct PcmBuf {
    pcm: Vec<u8>,
    /// The first byte of a sample split between two frames.
    odd: Option<u8>,
}

impl PcmBuf {
    fn add(&mut self, data: &[u8]) {
        self.pcm.extend(self.odd.take());
        self.pcm.extend_from_slice(data);
        if self.pcm.len() % 2 == 1 {
            self.odd = self.pcm.pop();
        }
    }
}

/// A turn that is listening.
struct Collect {
    mode: Mode,
    lang: Option<String>,
    buf: PcmBuf,
    vad: Vad,
    turn: Turn,
}

/// A task that is aborted when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

enum Phase {
    Idle,
    Collecting(Box<Collect>),
    /// The device's slot (`Turn`) is held here, not by the task, so a cancel
    /// frees it at once.
    Processing {
        task: AbortOnDrop,
        _turn: Turn,
    },
}

fn text(v: &Value) -> Message {
    Message::Text(v.to_string().into())
}

fn error_msg(code: &str, message: &str) -> Value {
    json!({"type": "error", "code": code, "message": message})
}

struct Conn {
    st: AppState,
    ident: Identity,
    creds: HeaderMap,
    tx: mpsc::Sender<Message>,
    phase: Phase,
}

async fn connection(st: AppState, ident: Identity, creds: HeaderMap, socket: WebSocket) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<Message>(OUTBOX);
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let closing = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || closing {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let device = ident.device_id.clone();
    let mut shutdown = st.shutdown.subscribe();
    let mut conn = Conn {
        st,
        ident,
        creds,
        tx,
        phase: Phase::Idle,
    };
    tracing::info!(device = %device, "voice socket open");

    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING_EVERY, PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_seen = Instant::now();
    loop {
        tokio::select! {
            msg = stream.next() => match msg {
                Some(Ok(m)) => {
                    last_seen = Instant::now();
                    if !conn.on_message(m) {
                        break;
                    }
                }
                _ => break,
            },
            _ = ping.tick() => {
                if last_seen.elapsed() > IDLE {
                    tracing::info!(device = %device, "voice socket idle; closing");
                    break;
                }
                if conn.tx.try_send(Message::Ping(Bytes::new())).is_err() {
                    break;
                }
            }
            () = conn.turn_ended() => conn.phase = Phase::Idle,
            _ = shutdown.changed() => break,
        }
    }
    // Dropping the connection aborts a turn in progress.
    let tx = conn.tx.clone();
    drop(conn);
    let _ = tx.send(Message::Close(None)).await;
    drop(tx);
    let _ = writer.await;
    tracing::info!(device = %device, "voice socket closed");
}

impl Conn {
    /// Resolves when the turn being processed has finished.
    async fn turn_ended(&mut self) {
        match &mut self.phase {
            Phase::Processing { task, .. } => {
                let _ = (&mut task.0).await;
            }
            _ => std::future::pending::<()>().await,
        }
    }

    fn send(&self, v: &Value) {
        let _ = self.tx.try_send(text(v));
    }

    fn send_error(&self, code: &str, message: &str) {
        self.send(&error_msg(code, message));
    }

    /// Handle one incoming message; `false` closes the connection.
    fn on_message(&mut self, msg: Message) -> bool {
        match msg {
            Message::Text(t) => {
                match serde_json::from_str::<Command>(t.as_str()) {
                    Ok(cmd) => self.on_command(cmd),
                    Err(_) => self.send_error("bad_request", "not a JSON message of the protocol"),
                }
                true
            }
            Message::Binary(b) => {
                self.on_audio(&b);
                true
            }
            Message::Close(_) => false,
            Message::Ping(_) | Message::Pong(_) => true,
        }
    }

    fn on_command(&mut self, cmd: Command) {
        match cmd.kind.as_str() {
            "start" => self.start(cmd),
            "end" => {
                // With no turn collecting, `end` is late and ignored.
                if matches!(self.phase, Phase::Collecting(_)) {
                    if let Phase::Collecting(c) = std::mem::replace(&mut self.phase, Phase::Idle) {
                        self.process(*c);
                    }
                }
            }
            "cancel" => {
                // Collecting or Processing: drop it (aborts the task).
                self.phase = Phase::Idle;
            }
            // Device telemetry (protocol section 4): accepted, not stored.
            "hello" | "telemetry" => {}
            _ => self.send_error("bad_request", "unknown message type"),
        }
    }

    fn start(&mut self, cmd: Command) {
        // A finished turn whose task has not been reaped yet is not busy.
        if let Phase::Processing { task: t, .. } = &self.phase {
            if t.0.is_finished() {
                self.phase = Phase::Idle;
            }
        }
        if !matches!(self.phase, Phase::Idle) {
            self.send_error("busy", "a voice turn is already running");
            return;
        }
        let mode = match cmd.mode.as_deref() {
            None | Some("ptt") => Mode::Ptt,
            Some("vad") => Mode::Vad,
            Some(_) => {
                self.send_error("bad_request", "mode: ptt or vad");
                return;
            }
        };
        // Validates `lang` (it, en, it-IT, ...).
        if let Err(m) = tts::pick_voice(&self.st.tts.voices, None, cmd.lang.as_deref(), "") {
            self.send_error("bad_request", &m);
            return;
        }
        if let Err(wait) = self.st.voice.limiter.check(&self.ident.device_id) {
            self.send_error(
                "rate_limited",
                &format!("too many voice turns; retry in {} s", wait.as_secs().max(1)),
            );
            return;
        }
        let Some(turn) = Turn::begin(&self.st, &self.ident.device_id) else {
            self.send_error("busy", "the device has a voice turn running");
            return;
        };
        self.send(&json!({"type": "listening"}));
        let (st, creds, lang) = (self.st.clone(), self.creds.clone(), cmd.lang.clone());
        tokio::spawn(async move { st.voice.warm_stt(&creds, lang.as_deref()).await });
        self.phase = Phase::Collecting(Box::new(Collect {
            mode,
            lang: cmd.lang,
            buf: PcmBuf::default(),
            vad: Vad::new(),
            turn,
        }));
    }

    fn on_audio(&mut self, data: &[u8]) {
        let Phase::Collecting(c) = &mut self.phase else {
            return; // late frames after an endpoint or a cancel
        };
        let room = MAX_BODY.saturating_sub(c.buf.pcm.len());
        let full = data.len() >= room;
        let data = &data[..data.len().min(room)];
        c.buf.add(data);
        let up = c.vad.feed(data);
        let endpoint = (c.mode == Mode::Vad && up.endpoint) || full;
        if endpoint {
            self.send(&json!({"type": "endpoint"}));
            let Phase::Collecting(c) = std::mem::replace(&mut self.phase, Phase::Idle) else {
                return;
            };
            self.process(*c);
        }
    }

    /// The recording is complete: run the turn (t0 is now).
    fn process(&mut self, c: Collect) {
        let t0 = Instant::now();
        let Collect {
            mode,
            lang,
            buf,
            vad,
            turn,
        } = c;
        let mut pcm = buf.pcm;
        pcm.truncate(vad.trim_len(pcm.len()) & !1);
        if pcm.len() < MIN_BYTES || (mode == Mode::Vad && !vad.heard_speech()) {
            self.send_error("no_speech", "no speech was heard");
            return; // `turn` is dropped: the device is free again
        }
        let task = tokio::spawn(run_turn(
            self.st.clone(),
            self.ident.clone(),
            self.creds.clone(),
            self.tx.clone(),
            Bytes::from(pcm),
            lang,
            t0,
        ));
        self.phase = Phase::Processing {
            task: AbortOnDrop(task),
            _turn: turn,
        };
    }
}

fn ms_json(v: Option<u64>) -> Value {
    v.map_or(Value::Null, |v| json!(v))
}

/// One turn after the recording: speech-to-text, chat, speech.
async fn run_turn(
    st: AppState,
    ident: Identity,
    creds: HeaderMap,
    tx: mpsc::Sender<Message>,
    pcm: Bytes,
    lang: Option<String>,
    t0: Instant,
) {
    let send = |v: Value| {
        let tx = tx.clone();
        async move { tx.send(text(&v)).await.is_ok() }
    };
    let shared = std::sync::Arc::new(Shared::new(t0));
    let device = ident.device_id.clone();

    let heard = match voice::transcribe(&st, &creds, &Audio::Pcm(pcm), lang.as_deref()).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(device = %device, code = %e.code, "voice turn: speech-to-text failed");
            send(error_msg("stt_unavailable", &e.message)).await;
            return;
        }
    };
    let stt_ms = shared.elapsed_ms();
    if heard.is_empty() {
        send(error_msg("no_speech", "no speech was heard")).await;
        return;
    }
    if !send(json!({"type": "transcript", "text": heard})).await {
        return;
    }

    let (out_tx, mut out_rx) = mpsc::channel::<Bytes>(16);
    let pipe = voice_pipe::speak_turn(
        st.clone(),
        Spec { heard, lang, ident },
        shared.clone(),
        out_tx,
    );
    let pump = async {
        let mut first = true;
        while let Some(frame) = out_rx.recv().await {
            if first {
                first = false;
                if !send(json!({"type": "audio", "rate": tts::PCM_RATE})).await {
                    return;
                }
            }
            if tx.send(Message::Binary(frame)).await.is_err() {
                return;
            }
            shared.mark_first_audio();
        }
    };
    let (result, ()) = tokio::join!(pipe, pump);

    match result {
        Ok(spoken) => {
            let total = shared.elapsed_ms();
            tracing::info!(
                device = %device,
                stt_ms,
                lucina_first_ms = ?shared.lucina_first(),
                tts_first_ms = ?shared.tts_first(),
                first_audio_ms = ?shared.first_audio(),
                total_ms = total,
                "voice turn"
            );
            send(json!({"type": "reply", "text": spoken})).await;
            send(json!({"type": "done", "ms": {
                "stt": stt_ms,
                "lucina_first": ms_json(shared.lucina_first()),
                "tts_first": ms_json(shared.tts_first()),
                "first_audio": ms_json(shared.first_audio()),
                "total": total,
            }}))
            .await;
        }
        Err(voice_pipe::PipeError::Closed) => {}
        Err(e) => {
            tracing::warn!(device = %device, code = e.code(), "voice turn failed");
            send(error_msg(e.code(), &e.message())).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_must_match_the_host() {
        assert!(origin_matches(
            "https://getsecretary.tech",
            "getsecretary.tech"
        ));
        assert!(origin_matches("http://127.0.0.1:8080/", "127.0.0.1:8080"));
        assert!(origin_matches(
            "https://GetSecretary.tech",
            "getsecretary.tech"
        ));
        assert!(!origin_matches("https://evil.example", "getsecretary.tech"));
        assert!(!origin_matches(
            "https://getsecretary.tech.evil.example",
            "getsecretary.tech"
        ));
        assert!(!origin_matches(
            "https://getsecretary.tech:8443",
            "getsecretary.tech"
        ));
        assert!(!origin_matches("null", "getsecretary.tech"));
    }

    #[test]
    fn min_bytes_is_three_tenths_of_a_second() {
        assert_eq!(MIN_BYTES, 9600);
    }

    #[test]
    fn odd_frames_keep_sample_alignment() {
        // Two frames of 3 bytes: the split sample is stitched back together.
        let mut b = PcmBuf::default();
        b.add(&[1, 2, 3]);
        assert_eq!(b.pcm, [1, 2]);
        b.add(&[4, 5, 6]);
        assert_eq!(b.pcm, [1, 2, 3, 4, 5, 6]);
        assert_eq!(b.odd, None);
        b.add(&[7]);
        assert_eq!(b.pcm.len() % 2, 0);
    }
}
