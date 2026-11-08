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
//! **Client → server.** Text `start {mode: "ptt"|"vad", lang?: "it"|"en"}`
//! (plus the keys of section 10.1, below), binary s16le 16 kHz mono PCM (or
//! `ima-adpcm`), text `end`, text `cancel`. Frames or `end` with no turn
//! running are ignored (they may be late); a `start` while a turn runs is
//! answered with `busy` and changes nothing. The phase of the connection is
//! the pure machine of [`crate::voice_frames`] (`step`), which this file drives.
//!
//! **Streaming input** (protocol section 10). `start` also takes `purpose`
//! (`command`|`note`), `codec` (`pcm16`|`ima-adpcm`, with `rate` and `block`),
//! `thread` (`voice`|`main`, with `channel`, `sessionId`), `replyTo`, `source`,
//! `text` and `speak`; a bad value of any is `bad_request` and starts nothing.
//! `listening` echoes `purpose`, `codec`, `rate` and `turn` (a UUID v4: a note's
//! `shareId`). ADPCM frames are decoded and resampled to 16 kHz as they arrive,
//! so the VAD, the caps and the speech-to-text see 16 kHz PCM. A command with
//! `thread: "main"` is sent to Lucina under the channel of `/v1/chat`'s rule
//! (`routes::chat_channel`) and the client's session; `text: true` adds
//! `delta` / `answer` messages; `speak: false` makes no speech at all
//! ([`crate::voice_pipe`]). A note ([`crate::voice_note`]) is transcribed (up to
//! 120 s), posted as a share and answered by `note`; the phase goes back to Idle
//! at that moment, and a watcher owned by the connection (at most four) sends
//! `reply` and `done` with the `shareId` when Lucina has answered.
//!
//! The device's `hello` and `telemetry` text frames (protocol section 4) are
//! stored and never answered ([`crate::voice_telemetry`]); so is the server's
//! own record of every turn. Both are read through `GET /v1/voice/devices*`.
//!
//! **Turn.** The local speech-to-text is warmed in the background when the
//! socket opens and again on `start` ([`crate::voice::Voice::warm_stt`]; a
//! warm-up fresher than [`crate::voice::WARM_FRESH`] is not repeated).
//! 1. `start` → `listening`.
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
//!    When Lucina sends `route` / `ack` (protocol section 6), a cached filler
//!    phrase goes out first, `done.ms` gains `route` and `ack`, and the
//!    turn's record gets the route ([`crate::voice_pipe`]). The Jev layers of
//!    the turn (protocol section 9: `route.layers`, completed by the chat's
//!    `done` body) go in `done.layers` next to `ms` when there are any.
//!    Every chat turn is sent as channel `voice` (the "Lucina Voice" thread,
//!    protocol section 7; `thread: "main"` the `/v1/chat` channel instead) with
//!    `voiceMeta` (turn id, STT ms, Voice Learn facts).
//!    **Level** (protocol section 10.7): the recording is measured first ([`crate::voice_level`], rules in
//!    [`crate::voice_rules`]). A silent one is `error silent` and runs nothing; quiet speech is lifted on the copy the
//!    speech-to-text gets (a note's file stays as recorded). The four dBFS numbers are on the turn's log line and in its record.
//!    **Test devices** (section 10.8): a device flagged `test` in devices.json (`secretary-server token test <id> on`) runs in
//!    Lucina's self-test conversation, for chat turns and notes alike ([`crate::voice_pipe`], [`note_is_test`]).
//! 4. A turn that ends without complete audio sends `error` instead of `done`:
//!    `no_speech` (under 0.3 s of audio, no speech in `vad` mode, or an empty
//!    transcript), `silent` (no signal), `stt_unavailable`, `lucina_unavailable`, `tts_failed`, and
//!    for a note `note_failed` (the share was refused or unreachable). The
//!    connection stays open. `busy`, `bad_request` and `rate_limited` answer a
//!    bad or refused `start` (or a message that is not JSON of the contract).
//! 5. `cancel` aborts the turn wherever it is (collecting, speech-to-text,
//!    chat, speech); closing the socket does the same. A chat already sent to
//!    Lucina is not recalled.
//!
//! Timings are logged at info with the device id (never the transcript, the
//! audio or credentials).

use std::collections::VecDeque;
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
use crate::voice_frames::{self as frames, Action, AdpcmDecoder, Event, Resampler, StartOpts, StartRaw};
use crate::voice_level;
use crate::voice_note;
use crate::voice_pipe::{self, Shared, Spec};
use crate::voice_telemetry::{self, ConnGuard, InputLevel, TurnInfo};
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
/// The `silent` error's message (protocol section 10.7).
pub const SILENT_MESSAGE: &str = "The recording was silent: check the microphone.";
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
    // The chat channel rule of `thread: "main"` and notes reads this header (`/v1/chat`'s).
    let mut client = HeaderMap::new();
    if let Some(v) = headers.get(crate::routes::CLIENT_HEADER) {
        client.insert(crate::routes::CLIENT_HEADER, v.clone());
    }
    ws.max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| connection(st, ident, creds, client, socket))
}

/// Text that starts like a `hello` or `telemetry` frame, valid JSON or not.
fn looks_like_telemetry(text: &str) -> bool {
    let t = text.trim_start();
    ["telemetry", "hello"]
        .iter()
        .any(|k| t.starts_with(&format!("{{\"type\":\"{k}\"")))
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
    lang: Option<String>,
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

/// `ima-adpcm` in, 16 kHz s16le out (protocol section 10.2).
struct AdpcmIn {
    dec: AdpcmDecoder,
    res: Resampler,
}

impl AdpcmIn {
    fn decode(&mut self, data: &[u8]) -> Vec<u8> {
        let mut samples = Vec::with_capacity(data.len() * 2);
        self.dec.push(data, &mut samples);
        self.resample(&samples)
    }

    /// The last partial block.
    fn finish(&mut self) -> Vec<u8> {
        let mut samples = Vec::new();
        self.dec.finish(&mut samples);
        self.resample(&samples)
    }

    fn resample(&mut self, samples: &[i16]) -> Vec<u8> {
        let mut out16 = Vec::with_capacity(samples.len() * 2);
        self.res.push(samples, &mut out16);
        let mut out = Vec::with_capacity(out16.len() * 2);
        for v in out16 {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }
}

/// A turn that is listening.
struct Collect {
    opts: StartOpts,
    lang: Option<String>,
    buf: PcmBuf,
    vad: Vad,
    turn: Turn,
    /// The turn id (a UUID v4: the note's `shareId`).
    id: String,
    /// `ima-adpcm`: the decoder and resampler.
    audio: Option<AdpcmIn>,
    /// A note's `ima-adpcm` blocks as the client sent them (the file keeps them).
    raw: Vec<u8>,
    /// Bytes received in binary frames.
    received: u64,
    /// The decoded 16 kHz audio kept at most this long (bytes).
    cap: usize,
    /// The chat's channel: `voice`, or what `/v1/chat` would use for `thread: "main"`.
    channel: String,
}

impl Collect {
    /// Take one binary frame. `true` when the turn must run now: the VAD found the end of speech
    /// (vad mode), or the cap was reached.
    fn feed(&mut self, data: &[u8]) -> bool {
        self.received += data.len() as u64;
        if self.opts.note && self.opts.adpcm {
            self.raw.extend_from_slice(data);
        }
        match &mut self.audio {
            None => self.push_pcm(data),
            Some(a) => {
                let pcm = a.decode(data);
                self.push_pcm(&pcm)
            }
        }
    }

    /// Decoded 16 kHz bytes into the recording, within the cap.
    fn push_pcm(&mut self, pcm: &[u8]) -> bool {
        let (take, full) = frames::audio_room(self.buf.pcm.len(), pcm.len(), self.cap);
        let pcm = &pcm[..take];
        self.buf.add(pcm);
        // A note is never trimmed or ended by a pause: no VAD.
        if self.opts.note {
            return full;
        }
        let up = self.vad.feed(pcm);
        (self.opts.vad && up.endpoint) || full
    }

    /// The end of the recording: flush the decoder's partial last block.
    fn finish(&mut self) {
        if let Some(a) = &mut self.audio {
            let tail = a.finish();
            self.push_pcm(&tail);
        }
    }
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
        id: String,
    },
}

impl Phase {
    /// Where the pure phase machine ([`frames::step`]) says this is.
    fn kind(&self) -> frames::Phase {
        match self {
            Phase::Idle => frames::Phase::Idle,
            Phase::Collecting(_) => frames::Phase::Collecting,
            Phase::Processing { .. } => frames::Phase::Processing,
        }
    }
}

fn text(v: &Value) -> Message {
    Message::Text(v.to_string().into())
}

fn error_msg(code: &str, message: &str) -> Value {
    json!({"type": "error", "code": code, "message": message})
}

/// What a turn's task tells the connection (which owns the phase and the watchers).
enum Ctl {
    /// A note's share was accepted: free the device, say `note`, follow the answer.
    NotePosted {
        id: String,
        user_item_id: String,
        stt_ms: Option<u64>,
        note_ms: u64,
        t0: Instant,
        /// A test note: it went to Lucina's self-test conversation, so its answer is followed by status reads.
        test: bool,
    },
}

struct Conn {
    st: AppState,
    ident: Identity,
    creds: HeaderMap,
    /// `X-Secretary-Client` of the upgrade, if it had one (the `/v1/chat` channel rule).
    client: HeaderMap,
    tx: mpsc::Sender<Message>,
    ctl: mpsc::UnboundedSender<Ctl>,
    phase: Phase,
    /// The watchers of notes whose answer is pending, oldest first (at most
    /// [`voice_note::MAX_WATCHED`]); aborted when the connection ends.
    notes: VecDeque<AbortOnDrop>,
}

async fn connection(
    st: AppState,
    ident: Identity,
    creds: HeaderMap,
    client: HeaderMap,
    socket: WebSocket,
) {
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
    let _online = ConnGuard::open(&st, &ident);
    // The notifier's "connected" (notify::reply_route): a reply to this device's turn reaches it here.
    let _link = st.notify.link(&ident.device_id);
    let mut shutdown = st.shutdown.subscribe();
    let (ctl, mut ctl_rx) = mpsc::unbounded_channel::<Ctl>();
    let mut conn = Conn {
        st,
        ident,
        creds,
        client,
        tx,
        ctl,
        phase: Phase::Idle,
        notes: VecDeque::new(),
    };
    tracing::info!(device = %device, "voice socket open");
    // Clients open the socket when their voice screen shows, seconds before the owner speaks: warm the
    // local speech-to-text now, so its cold load (about 3 s; it stops after 90 s idle) is not paid after
    // the end of a short recording. `start` warms again if this one has gone stale.
    {
        let (st, creds) = (conn.st.clone(), conn.creds.clone());
        tokio::spawn(async move { st.voice.warm_stt(&creds, None).await });
    }

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
            () = conn.turn_ended() => conn.finish_phase(),
            Some(c) = ctl_rx.recv() => conn.on_ctl(c),
            _ = shutdown.changed() => break,
        }
    }
    // Dropping the connection aborts a turn in progress and the note watchers.
    let tx = conn.tx.clone();
    drop(conn);
    let _ = tx.send(Message::Close(None)).await;
    drop(tx);
    let _ = writer.await;
    tracing::info!(device = %device, "voice socket closed");
}

/// A string key of a `start`: missing or `null` is none.
fn str_key<'a>(v: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("{key}: a string")),
    }
}

fn int_key(v: &Value, key: &str) -> Result<Option<u32>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(n) => match n.as_u64() {
            Some(n) => Ok(Some(u32::try_from(n).unwrap_or(u32::MAX))),
            None => Err(format!("{key}: a whole number")),
        },
    }
}

fn bool_key(v: &Value, key: &str) -> Result<Option<bool>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{key}: true or false")),
    }
}

/// The keys of a `start` message (protocol section 10.1), checked.
fn parse_start(text: &str) -> Result<StartOpts, String> {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let raw = StartRaw {
        mode: str_key(&v, "mode")?,
        purpose: str_key(&v, "purpose")?,
        codec: str_key(&v, "codec")?,
        rate: int_key(&v, "rate")?,
        block: int_key(&v, "block")?,
        thread: str_key(&v, "thread")?,
        channel: str_key(&v, "channel")?,
        session_id: str_key(&v, "sessionId")?,
        reply_to: str_key(&v, "replyTo")?,
        source: str_key(&v, "source")?,
        text: bool_key(&v, "text")?,
        speak: bool_key(&v, "speak")?,
    };
    frames::validate_start(&raw).map_err(str::to_owned)
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

    /// The task of the turn ended (the phase machine's `TurnFinished`).
    fn finish_phase(&mut self) {
        if frames::step(self.phase.kind(), Event::TurnFinished).act == Action::Release {
            self.phase = Phase::Idle;
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
                self.st.voice_log.touch(&self.ident.device_id);
                match serde_json::from_str::<Command>(t.as_str()) {
                    // Device telemetry (protocol section 4): stored, never answered.
                    Ok(cmd) if matches!(cmd.kind.as_str(), "hello" | "telemetry") => {
                        self.st.voice_log.ingest(
                            &self.ident.device_id,
                            &self.ident.device_name,
                            &cmd.kind,
                            t.as_str(),
                        );
                    }
                    Ok(cmd) => self.on_command(cmd, t.as_str()),
                    // A broken telemetry frame is dropped without a word: an error
                    // frame in the middle of a turn would only confuse the device.
                    Err(_) if looks_like_telemetry(t.as_str()) => {}
                    Err(_) => self.send_error("bad_request", "not a JSON message of the protocol"),
                }
                true
            }
            Message::Binary(b) => {
                self.on_audio(&b);
                true
            }
            Message::Close(_) => false,
            Message::Ping(_) | Message::Pong(_) => {
                self.st.voice_log.touch(&self.ident.device_id);
                true
            }
        }
    }

    fn on_command(&mut self, cmd: Command, raw: &str) {
        match cmd.kind.as_str() {
            "start" => self.start(cmd, raw),
            "end" => {
                // With no turn collecting, `end` is late and ignored.
                if frames::step(self.phase.kind(), Event::End).act == Action::Process {
                    self.run_collected();
                }
            }
            "cancel" => {
                // Collecting or Processing: drop it (aborts the task).
                if frames::step(self.phase.kind(), Event::Cancel).act == Action::Abort {
                    self.phase = Phase::Idle;
                }
            }
            _ => self.send_error("bad_request", "unknown message type"),
        }
    }

    /// Take the collecting turn out of the phase and run it.
    fn run_collected(&mut self) {
        if let Phase::Collecting(c) = std::mem::replace(&mut self.phase, Phase::Idle) {
            self.process(*c);
        }
    }

    fn start(&mut self, cmd: Command, raw: &str) {
        // A finished turn whose task has not been reaped yet is not busy.
        if let Phase::Processing { task: t, .. } = &self.phase {
            if t.0.is_finished() {
                self.phase = Phase::Idle;
            }
        }
        if frames::step(self.phase.kind(), Event::Start).act != Action::Listen {
            self.send_error("busy", "a voice turn is already running");
            return;
        }
        let opts = match parse_start(raw) {
            Ok(o) => o,
            Err(m) => {
                self.send_error("bad_request", &m);
                return;
            }
        };
        if opts.note && self.st.voice.share_url().is_none() {
            self.send_error("bad_request", "voice notes are off");
            return;
        }
        // Validates `lang` (it, en, it-IT, ...).
        if let Err(m) = tts::pick_voice(&self.st.tts.voices, None, cmd.lang.as_deref(), "") {
            self.send_error("bad_request", &m);
            return;
        }
        // The chat channel of the main thread: the `/v1/chat` rule (protocol section 10.1).
        let channel = if opts.main {
            match crate::routes::chat_channel(&self.client, opts.channel.as_deref()) {
                Ok(c) => c,
                Err(e) => {
                    self.send_error("bad_request", &e.message);
                    return;
                }
            }
        } else {
            voice_pipe::CHANNEL.to_owned()
        };
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
        let Ok(id) = voice_note::new_turn_id() else {
            self.send_error("bad_request", "no turn id could be made");
            return;
        };
        self.send(&json!({
            "type": "listening",
            "purpose": if opts.note { "note" } else { "command" },
            "codec": if opts.adpcm { "ima-adpcm" } else { "pcm16" },
            "rate": opts.rate,
            "turn": id,
        }));
        tracing::info!(
            device = %self.ident.device_id,
            purpose = if opts.note { "note" } else { "command" },
            codec = if opts.adpcm { "ima-adpcm" } else { "pcm16" },
            rate = opts.rate,
            main = opts.main,
            text = opts.text,
            speak = opts.speak,
            "voice turn start"
        );
        let (st, creds, lang) = (self.st.clone(), self.creds.clone(), cmd.lang.clone());
        tokio::spawn(async move { st.voice.warm_stt(&creds, lang.as_deref()).await });
        let audio = opts.adpcm.then(|| AdpcmIn {
            dec: AdpcmDecoder::new(opts.block),
            res: Resampler::new(opts.rate),
        });
        let cap = if opts.note { frames::NOTE_CAP } else { MAX_BODY };
        self.phase = Phase::Collecting(Box::new(Collect {
            opts,
            lang: cmd.lang,
            buf: PcmBuf::default(),
            vad: Vad::new(),
            turn,
            id,
            audio,
            raw: Vec::new(),
            received: 0,
            cap,
            channel,
        }));
    }

    fn on_audio(&mut self, data: &[u8]) {
        if frames::step(self.phase.kind(), Event::Audio).act != Action::Accept {
            return; // late frames after an endpoint or a cancel
        }
        let Phase::Collecting(c) = &mut self.phase else {
            return;
        };
        if c.feed(data) && frames::step(frames::Phase::Collecting, Event::Endpoint).act == Action::Process {
            self.send(&json!({"type": "endpoint"}));
            self.run_collected();
        }
    }

    /// The recording is complete: run the turn (t0 is now).
    fn process(&mut self, mut c: Collect) {
        let t0 = Instant::now();
        c.finish();
        let Collect {
            opts,
            lang,
            buf,
            vad,
            turn,
            id,
            raw,
            received,
            channel,
            ..
        } = c;
        let mut pcm = buf.pcm;
        let up_bytes = if opts.adpcm { received } else { pcm.len() as u64 };
        if !opts.note {
            pcm.truncate(vad.trim_len(pcm.len()) & !1);
        }
        if pcm.len() < MIN_BYTES {
            self.send_error("no_speech", "no speech was heard");
            voice_telemetry::record_no_speech(&self.st, &self.ident, up_bytes);
            return; // `turn` is dropped: the device is free again
        }
        // The level of the recording (protocol section 10.7), before any speech-to-text: a silent one stops here, quiet speech is
        // lifted on the copy that goes to the speech-to-text (the note's file stays as recorded).
        let samples = voice_level::samples_of(&pcm);
        let assessed = voice_telemetry::assess(&samples);
        let level = assessed.level;
        if assessed.silent {
            tracing::info!(
                device = %self.ident.device_id,
                in_peak_dbfs = level.in_peak_dbfs,
                in_rms_dbfs = level.in_rms_dbfs,
                speech_rms_dbfs = level.speech_rms_dbfs,
                gain_db = level.gain_db,
                "voice turn: silent recording"
            );
            self.send_error("silent", SILENT_MESSAGE);
            voice_telemetry::record_silent(&self.st, &self.ident, up_bytes, level);
            return;
        }
        if opts.vad && !vad.heard_speech() {
            self.send_error("no_speech", "no speech was heard");
            voice_telemetry::record_no_speech(&self.st, &self.ident, up_bytes);
            return;
        }
        let stt_pcm = assessed.lifted.map_or_else(|| Bytes::from(pcm.clone()), |v| Bytes::from(voice_level::pcm_of(&v)));
        drop(samples);
        let task = if opts.note {
            tokio::spawn(run_note(NoteRun {
                st: self.st.clone(),
                ident: self.ident.clone(),
                creds: self.creds.clone(),
                tx: self.tx.clone(),
                ctl: self.ctl.clone(),
                id: id.clone(),
                pcm: Bytes::from(pcm),
                stt_pcm,
                level,
                raw,
                opts,
                channel,
                lang,
                t0,
                up_bytes,
            }))
        } else {
            tokio::spawn(run_turn(Run {
                st: self.st.clone(),
                ident: self.ident.clone(),
                creds: self.creds.clone(),
                tx: self.tx.clone(),
                pcm: Bytes::from(pcm),
                stt_pcm,
                level,
                lang,
                t0,
                up_bytes,
                channel,
                session_id: if opts.main {
                    opts.session_id
                } else {
                    Some(voice_pipe::SESSION_ID.to_owned())
                },
                speak: opts.speak,
                text: opts.text,
            }))
        };
        self.phase = Phase::Processing {
            task: AbortOnDrop(task),
            _turn: turn,
            id,
        };
    }

    /// A turn's task reports.
    fn on_ctl(&mut self, c: Ctl) {
        match c {
            Ctl::NotePosted {
                id,
                user_item_id,
                stt_ms,
                note_ms,
                t0,
                test,
            } => {
                // The device is free now (if the phase still is that note's turn).
                if matches!(&self.phase, Phase::Processing { id: p, .. } if *p == id)
                    && frames::step(self.phase.kind(), Event::NotePosted).act == Action::Release
                {
                    self.phase = Phase::Idle;
                }
                self.send(&json!({"type": "note", "shareId": id, "userItemId": user_item_id}));
                // Follow the answer; at most MAX_WATCHED, the oldest is dropped.
                self.notes.retain(|w| !w.0.is_finished());
                while self.notes.len() >= voice_note::MAX_WATCHED {
                    self.notes.pop_front();
                }
                let task = tokio::spawn(voice_note::watch(
                    self.st.clone(),
                    self.creds.clone(),
                    self.tx.clone(),
                    id,
                    // A test note's answer is not on the owner's timeline stream: an empty id makes the
                    // watcher read the share's status instead.
                    if test { String::new() } else { user_item_id },
                    t0,
                    stt_ms,
                    note_ms,
                    self.ident.device_id.clone(),
                ));
                self.notes.push_back(AbortOnDrop(task));
            }
        }
    }
}

fn ms_json(v: Option<u64>) -> Value {
    v.map_or(Value::Null, |v| json!(v))
}

/// A command turn after the recording.
struct Run {
    st: AppState,
    ident: Identity,
    creds: HeaderMap,
    tx: mpsc::Sender<Message>,
    pcm: Bytes,
    /// The recording as the speech-to-text gets it: quiet speech lifted (protocol section 10.7).
    stt_pcm: Bytes,
    level: InputLevel,
    lang: Option<String>,
    t0: Instant,
    up_bytes: u64,
    /// The chat's channel and session (protocol section 10.1).
    channel: String,
    session_id: Option<String>,
    speak: bool,
    text: bool,
}

/// One turn after the recording: speech-to-text, chat, speech. The server's
/// record of it goes to the telemetry store unless the socket went away.
async fn run_turn(run: Run) {
    let shared = std::sync::Arc::new(Shared::new(run.t0));
    let mut info = TurnInfo {
        err: None,
        transcript_chars: 0,
        speech_ms: run.pcm.len() as u64 / 32,
        up_bytes: run.up_bytes,
        level: Some(run.level),
        ..TurnInfo::default()
    };
    let (st, ident) = (run.st.clone(), run.ident.clone());
    turn_steps(run, &shared, &mut info).await;
    if info.err.as_deref() != Some("closed") {
        voice_telemetry::finish_turn(&st, &ident, &shared, info);
    }
}

async fn turn_steps(run: Run, shared: &std::sync::Arc<Shared>, info: &mut TurnInfo) {
    let Run {
        st,
        ident,
        creds,
        tx,
        stt_pcm,
        level,
        lang,
        channel,
        session_id,
        speak,
        text: text_out,
        ..
    } = run;
    let (st, ident, creds, tx) = (&st, &ident, &creds, &tx);
    let send = |v: Value| {
        let tx = tx.clone();
        async move { tx.send(text(&v)).await.is_ok() }
    };
    let device = ident.device_id.clone();

    let voice::Heard { text: heard, facts } = match voice::transcribe(st, creds, &Audio::Pcm(stt_pcm), lang.as_deref()).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(device = %device, code = %e.code, "voice turn: speech-to-text failed");
            info.err = Some("stt_unavailable".into());
            send(error_msg("stt_unavailable", &e.message)).await;
            return;
        }
    };
    let stt_ms = shared.elapsed_ms();
    shared.set_stt(stt_ms);
    info.stt_corrections = facts.corrections.len() as u32;
    info.stt_ambiguous = facts.ambiguous.len() as u32;
    if heard.is_empty() {
        info.err = Some("no_speech".into());
        send(error_msg("no_speech", "no speech was heard")).await;
        return;
    }
    info.transcript_chars = heard.chars().count() as u64;
    if !send(json!({"type": "transcript", "text": heard})).await {
        info.err = Some("closed".into());
        return;
    }

    let (out_tx, mut out_rx) = mpsc::channel::<Bytes>(16);
    let pipe = voice_pipe::speak_turn(
        st.clone(),
        Spec {
            heard,
            lang,
            ident: ident.clone(),
            facts,
            channel,
            session_id,
            speak,
            text_out: text_out.then(|| tx.clone()),
        },
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
                route_ms = ?shared.route_ms(),
                ack_ms = ?shared.ack_ms(),
                total_ms = total,
                speak,
                in_peak_dbfs = level.in_peak_dbfs,
                in_rms_dbfs = level.in_rms_dbfs,
                speech_rms_dbfs = level.speech_rms_dbfs,
                gain_db = level.gain_db,
                "voice turn"
            );
            send(json!({"type": "reply", "text": spoken})).await;
            let mut ms = if speak {
                json!({
                    "stt": stt_ms,
                    "lucina_first": ms_json(shared.lucina_first()),
                    "tts_first": ms_json(shared.tts_first()),
                    "first_audio": ms_json(shared.first_audio()),
                    "total": total,
                })
            } else {
                // No speech: no `tts_first` or `first_audio` (protocol section 10.3).
                json!({
                    "stt": stt_ms,
                    "lucina_first": ms_json(shared.lucina_first()),
                    "total": total,
                })
            };
            // Only when Lucina sent `route` / `ack` (protocol section 6).
            if let Some(v) = shared.route_ms() {
                ms["route"] = json!(v);
            }
            if let Some(v) = shared.ack_ms() {
                ms["ack"] = json!(v);
            }
            let mut done = json!({"type": "done", "ms": ms});
            // The Jev layers that decided the turn (protocol section 9), next to `ms`.
            let layers = shared.layers();
            if !layers.is_empty() {
                done["layers"] = json!(layers);
            }
            send(done).await;
        }
        Err(voice_pipe::PipeError::Closed) => info.err = Some("closed".into()),
        Err(e) => {
            tracing::warn!(device = %device, code = e.code(), "voice turn failed");
            info.err = Some(e.code().into());
            send(error_msg(e.code(), &e.message())).await;
        }
    }
}

/// A note after the recording (protocol section 10.4).
struct NoteRun {
    st: AppState,
    ident: Identity,
    creds: HeaderMap,
    tx: mpsc::Sender<Message>,
    ctl: mpsc::UnboundedSender<Ctl>,
    id: String,
    /// The decoded 16 kHz recording (the file keeps it as recorded).
    pcm: Bytes,
    /// The recording as the speech-to-text gets it: quiet speech lifted (protocol section 10.7).
    stt_pcm: Bytes,
    level: InputLevel,
    /// `ima-adpcm`: the client's blocks.
    raw: Vec<u8>,
    opts: StartOpts,
    channel: String,
    lang: Option<String>,
    t0: Instant,
    up_bytes: u64,
}

/// The note's file: a 16 kHz PCM WAV, or the client's own ADPCM blocks behind a 60-byte header.
fn note_wav(opts: &StartOpts, pcm: &[u8], raw: &[u8]) -> Vec<u8> {
    let clip = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    if opts.adpcm {
        let mut f = frames::wav_adpcm_header(opts.rate, opts.block, clip(raw.len()));
        f.extend_from_slice(raw);
        if raw.len() % 2 == 1 {
            f.push(0);
        }
        f
    } else {
        let mut f = frames::wav_pcm16_header(clip(pcm.len()), frames::OUT_RATE);
        f.extend_from_slice(pcm);
        f
    }
}

/// `it-IT` is `it`.
fn primary_lang(lang: &Option<String>) -> Option<String> {
    lang.as_deref()
        .and_then(|l| l.split(['-', '_']).next())
        .filter(|l| !l.is_empty())
        .map(str::to_ascii_lowercase)
}

async fn run_note(n: NoteRun) {
    let shared = std::sync::Arc::new(Shared::new(n.t0));
    let mut info = TurnInfo {
        err: None,
        transcript_chars: 0,
        speech_ms: n.pcm.len() as u64 / 32,
        up_bytes: n.up_bytes,
        level: Some(n.level),
        ..TurnInfo::default()
    };
    let (st, ident) = (n.st.clone(), n.ident.clone());
    let posted = note_steps(&n, &shared, &mut info).await;
    if info.err.as_deref() != Some("closed") {
        voice_telemetry::finish_turn(&st, &ident, &shared, info);
    }
    // Last, after the record: the connection frees the device and aborts this task.
    if let Some((user_item_id, stt_ms, note_ms)) = posted {
        let _ = n.ctl.send(Ctl::NotePosted {
            id: n.id.clone(),
            user_item_id,
            stt_ms,
            note_ms,
            t0: n.t0,
            test: note_is_test(&n.st, &n.ident.device_id, &n.opts.source),
        });
    }
}

/// A test note (a test device, or a `probe…`/`test…` source): it goes to Lucina's self-test
/// conversation, never the owner's chat, and never notifies (docs/notifications.md).
fn note_is_test(st: &AppState, device: &str, source: &str) -> bool {
    st.devices.is_test(device) || crate::notify::reply_route::is_test_source(source)
}

/// Transcribe (up to 120 s), post the share. `Some((userItemId, stt ms, note ms))` once posted.
async fn note_steps(
    n: &NoteRun,
    shared: &std::sync::Arc<Shared>,
    info: &mut TurnInfo,
) -> Option<(String, Option<u64>, u64)> {
    let send = |v: Value| {
        let tx = n.tx.clone();
        async move { tx.send(text(&v)).await.is_ok() }
    };
    let device = n.ident.device_id.clone();
    let mut transcript: Option<String> = None;
    let mut stt_ms = None;
    if n.pcm.len() <= frames::NOTE_STT_MAX {
        match voice::transcribe(&n.st, &n.creds, &Audio::Pcm(n.stt_pcm.clone()), n.lang.as_deref()).await {
            Ok(h) if !h.text.is_empty() => {
                let ms = shared.elapsed_ms();
                shared.set_stt(ms);
                stt_ms = Some(ms);
                info.transcript_chars = h.text.chars().count() as u64;
                info.stt_corrections = h.facts.corrections.len() as u32;
                info.stt_ambiguous = h.facts.ambiguous.len() as u32;
                if !send(json!({"type": "transcript", "text": h.text})).await {
                    info.err = Some("closed".into());
                    return None;
                }
                transcript = Some(h.text);
            }
            // Empty or failed: lucina-rs transcribes the file itself.
            Ok(_) => tracing::debug!(device = %device, "voice note: empty local transcript"),
            Err(e) => tracing::warn!(device = %device, code = %e.code, "voice note: speech-to-text failed"),
        }
    }

    let wav = note_wav(&n.opts, &n.pcm, &n.raw);
    let name = voice_note::file_name(chrono::Utc::now());
    let lang = primary_lang(&n.lang);
    let (content_type, body) = voice_note::share_body(
        voice_note::new_boundary(),
        &voice_note::NoteForm {
            share_id: &n.id,
            lang: lang.as_deref(),
            session_id: n.opts.session_id.as_deref(),
            channel: &n.channel,
            source: &n.opts.source,
            conversation: note_is_test(&n.st, &n.ident.device_id, &n.opts.source).then_some("selftest"),
            reply_to: n.opts.reply_to.as_deref(),
            duration_ms: n.pcm.len() as u64 / 32,
            transcript: transcript.as_deref(),
            file_name: &name,
            wav: &wav,
        },
    );
    match voice_note::post_share(&n.st, &n.creds, content_type, body).await {
        Ok(a) => {
            let note_ms = shared.elapsed_ms();
            tracing::info!(
                device = %device,
                codec = if n.opts.adpcm { "ima-adpcm" } else { "pcm16" },
                audio_ms = n.pcm.len() as u64 / 32,
                file_bytes = wav.len(),
                stt_ms = ?stt_ms,
                note_ms,
                in_peak_dbfs = n.level.in_peak_dbfs,
                in_rms_dbfs = n.level.in_rms_dbfs,
                speech_rms_dbfs = n.level.speech_rms_dbfs,
                gain_db = n.level.gain_db,
                "voice note posted"
            );
            Some((a.user_item_id, stt_ms, note_ms))
        }
        Err(e) => {
            tracing::warn!(device = %device, reason = %e.message(), "voice note: share failed");
            info.err = Some("note_failed".into());
            send(error_msg("note_failed", &e.message())).await;
            None
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
