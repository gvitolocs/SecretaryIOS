//! The spoken half of a LucinaVoice turn: transcript in, PCM out, shared by
//! `POST /v1/voice` ([`crate::voice`]) and the socket ([`crate::voice_ws`]).
//!
//! ```text
//! heard ─► Lucina streaming chat ─► sentence splitter ─► Kokoro (in order, one ahead) ─► PCM frames
//!            delta delta … reply          segments            segment 2 is requested
//!                                                              while segment 1 streams
//! ```
//!
//! [`speak_turn`] runs the whole thing and pushes ≤ 8 KiB, even-length PCM
//! frames (s16le mono at [`crate::tts::PCM_RATE`]) into a channel; the caller
//! forwards them. It returns what was spoken.
//!
//! - **Chat.** `secretary_core::LucinaClient::chat_stream` with `voice: true`.
//!   `delta`s feed a [`Splitter`]; each segment goes to the TTS worker at once.
//!   `reply` settles the turn: the rest of the text is flushed; with
//!   `replaces`, the reply text is spoken instead of the deltas if nothing was
//!   cut into a segment yet (else the remainder is dropped and logged); a
//!   `sensitive` reply is never spoken, a fixed phrase is ([`SENSITIVE_PHRASE`]),
//!   and segments queued but not started are dropped (what was already
//!   sent to the speaker cannot be taken back). A Lucina without streaming
//!   answers in JSON, which arrives as one `reply`.
//! - **TTS worker.** Segments are synthesized strictly in order, but up to
//!   [`IN_FLIGHT`] at a time: the next one is requested while the current one
//!   streams, and its PCM waits in memory until its turn.
//! - **Route and ack** (protocol section 6). A `route` event is recorded
//!   ([`RouteInfo`], and its ms in [`Shared::route_ms`]). An `ack` event, when
//!   nothing has been queued or sent for the turn yet and none came before,
//!   queues a filler segment first: the cached phrase PCM
//!   ([`crate::voice_filler`]), or, with the cache empty, the ack text
//!   synthesized like a sentence (and a cache rebuild starts). The answer's
//!   segments queue behind it in the same ordered stream, so they cannot
//!   overlap it. The filler is not part of the text returned as spoken.
//! - **Thread and layers** (protocol sections 7 and 9). The chat request is channel `voice`
//!   ([`CHANNEL`]) with `voiceMeta` ([`voice_meta`]: turn id, STT ms, the Voice Learn facts of
//!   the speech-to-text engine). The Jev layers of the turn are collected in [`Shared`]: those
//!   of the `route` event first, then those of the `done` body's `voice.layers` (a layer of the
//!   same name replaces the earlier one). A routed turn waits at most [`DONE_GRACE`] after its
//!   audio for the `done`, so the turn's record usually has the settled layers.
//! - **After `reply`** the connection to Lucina is read to its end by a
//!   background task, so Lucina finishes the turn (memory, rich reply) after
//!   the audio is done. Dropping [`speak_turn`] before it returns (a cancel,
//!   a closed socket or HTTP client) aborts everything, that task included.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use axum::body::Bytes;
use futures_util::StreamExt;
use secretary_core::models::ChatRequest;
use secretary_core::{ChatEvent, RouteLayer};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Semaphore};
use tokio::task::JoinHandle;

use crate::auth::Identity;
use crate::error::ApiError;
use crate::tts::{self, Format};
use crate::voice_split::Splitter;
use crate::AppState;

/// How long a routed turn waits for Lucina's `done` after its audio is made.
const DONE_GRACE: std::time::Duration = std::time::Duration::from_millis(250);
/// Largest PCM frame forwarded.
pub const FRAME_BYTES: usize = 8192;
/// Segments being synthesized at once: the one playing and the next.
pub const IN_FLIGHT: usize = 2;
/// Lucina's chat session for the speaker.
pub const SESSION_ID: &str = "lucinavoice";
/// Lucina's timeline channel for every voice turn: one shared "Lucina Voice" thread.
pub const CHANNEL: &str = "voice";

/// What a voice turn tells Lucina besides the words (`voiceMeta`, read by lucina-server's chat
/// route): `{turnId, ms: {stt}, ambiguous?: [{heard, candidates}]}`. `turnId` and `ms` end up in
/// the thread item's `meta.voice` (the later stages are Lucina's own: the route and its layers);
/// `ambiguous` feeds Lucina's `vocab` layer. Never audio; no transcript beyond the spans the
/// speech-to-text engine could not settle.
pub fn voice_meta(turn_id: &str, stt_ms: Option<u64>, facts: &crate::voice::SttFacts) -> serde_json::Value {
    let mut m = serde_json::json!({"turnId": turn_id});
    if let Some(ms) = stt_ms {
        m["ms"] = serde_json::json!({"stt": ms});
    }
    if !facts.ambiguous.is_empty() {
        m["ambiguous"] = serde_json::Value::Array(facts.ambiguous_json());
    }
    m
}

/// Spoken instead of a sensitive reply: (Italian, English). Only the English one is ever spoken.
pub const SENSITIVE_PHRASE: (&str, &str) =
    ("Ti rispondo nell'app.", "I'll answer that in the app.");
/// Spoken when Lucina's reply is empty: (Italian, English). Only the English one is ever spoken.
pub const EMPTY_PHRASE: (&str, &str) = (
    "Scusa, non ho una risposta.",
    "Sorry, I don't have an answer.",
);

/// What a turn needs to know.
pub struct Spec {
    /// The transcript.
    pub heard: String,
    /// `it`, `en` (or `it-IT` …) when the client said; else the voice follows the text.
    pub lang: Option<String>,
    pub ident: Identity,
    /// What the speech-to-text engine said about Voice Learn for this turn.
    pub facts: crate::voice::SttFacts,
}

/// Why a turn did not end in complete audio.
#[derive(Debug)]
pub enum PipeError {
    /// Lucina failed (or could not be reached).
    Lucina(ApiError),
    /// Kokoro failed.
    Tts(ApiError),
    /// The consumer of the frames went away.
    Closed,
}

impl PipeError {
    /// The code a socket client sees.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Lucina(_) => "lucina_unavailable",
            Self::Tts(_) => "tts_failed",
            Self::Closed => "closed",
        }
    }

    /// The error as an HTTP answer.
    pub fn into_api(self) -> ApiError {
        match self {
            Self::Lucina(e) | Self::Tts(e) => e,
            Self::Closed => ApiError::internal("the client went away"),
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Lucina(e) | Self::Tts(e) => e.message.clone(),
            Self::Closed => "closed".into(),
        }
    }
}

/// How Lucina routed a voice turn (`route` event). No transcript text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RouteInfo {
    pub intent: String,
    pub confidence: Option<f64>,
    pub jev_ms: Option<u64>,
    pub fast: bool,
    pub source: String,
}

impl RouteInfo {
    /// Cleaned for the logs and the API: strings cut and without control
    /// characters or `<` `>`, confidence in 0..=1.
    fn clean(self) -> Self {
        let s = |t: String, n: usize| -> String {
            t.chars()
                .filter(|c| !c.is_control() && !matches!(c, '<' | '>'))
                .take(n)
                .collect()
        };
        Self {
            intent: s(self.intent, 64),
            confidence: self.confidence.map(|c| c.clamp(0.0, 1.0)),
            jev_ms: self.jev_ms.map(|m| m.min(10_000_000)),
            fast: self.fast,
            source: s(self.source, 32),
        }
    }
}

/// Timings of one turn, in ms since its start (`t0`), and the voice used.
/// Each mark is set once, by whoever sees the moment first.
#[derive(Debug)]
pub struct Shared {
    t0: Instant,
    lucina_first: AtomicU64,
    tts_first: AtomicU64,
    first_audio: AtomicU64,
    stt: AtomicU64,
    /// The `route` event arrived.
    route_ms: AtomicU64,
    /// The filler (or synthesized ack) audio was handed to the client.
    ack_ms: AtomicU64,
    route: OnceLock<RouteInfo>,
    /// The Jev layers of the turn (protocol section 9): from the `route` event, then
    /// completed by the `done` body. Facts only, never text.
    layers: Mutex<Vec<RouteLayer>>,
    /// PCM bytes handed to the client.
    down_bytes: AtomicU64,
    /// The Kokoro voice, set when the first segment is known.
    pub voice: OnceLock<String>,
}

const UNSET: u64 = u64::MAX;

impl Shared {
    pub fn new(t0: Instant) -> Self {
        Self {
            t0,
            lucina_first: AtomicU64::new(UNSET),
            tts_first: AtomicU64::new(UNSET),
            first_audio: AtomicU64::new(UNSET),
            stt: AtomicU64::new(UNSET),
            route_ms: AtomicU64::new(UNSET),
            ack_ms: AtomicU64::new(UNSET),
            route: OnceLock::new(),
            layers: Mutex::new(Vec::new()),
            down_bytes: AtomicU64::new(0),
            voice: OnceLock::new(),
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.t0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn mark(&self, slot: &AtomicU64) {
        let _ = slot.compare_exchange(UNSET, self.elapsed_ms(), Ordering::SeqCst, Ordering::SeqCst);
    }

    fn get(slot: &AtomicU64) -> Option<u64> {
        Some(slot.load(Ordering::SeqCst)).filter(|v| *v != UNSET)
    }

    /// First delta or reply from Lucina.
    pub fn lucina_first(&self) -> Option<u64> {
        Self::get(&self.lucina_first)
    }

    /// First PCM byte from Kokoro.
    pub fn tts_first(&self) -> Option<u64> {
        Self::get(&self.tts_first)
    }

    /// The first audio frame handed to the client (set by the consumer).
    pub fn first_audio(&self) -> Option<u64> {
        Self::get(&self.first_audio)
    }

    /// The transcript is ready (ms since `t0`).
    pub fn set_stt(&self, ms: u64) {
        let _ = self
            .stt
            .compare_exchange(UNSET, ms, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub fn stt(&self) -> Option<u64> {
        Self::get(&self.stt)
    }

    /// Ms from end of speech to Lucina's `route` event, if it sent one.
    pub fn route_ms(&self) -> Option<u64> {
        Self::get(&self.route_ms)
    }

    /// Ms from end of speech to the filler audio, if one was played.
    pub fn ack_ms(&self) -> Option<u64> {
        Self::get(&self.ack_ms)
    }

    /// What the `route` event said, if there was one.
    pub fn route(&self) -> Option<&RouteInfo> {
        self.route.get()
    }

    /// The Jev layers reported so far (`route` event, then `done`).
    pub fn layers(&self) -> Vec<RouteLayer> {
        self.layers.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Add reported layers: one with the name of an earlier one replaces it
    /// (the `done` body is the settled version), the rest are appended, up to
    /// [`secretary_core::chat_stream::MAX_LAYERS`].
    pub fn merge_layers(&self, more: Vec<RouteLayer>) {
        let mut mine = self.layers.lock().unwrap_or_else(|p| p.into_inner());
        for l in more {
            if let Some(old) = mine.iter_mut().find(|o| o.layer == l.layer) {
                *old = l;
            } else if mine.len() < secretary_core::chat_stream::MAX_LAYERS {
                mine.push(l);
            }
        }
    }

    /// PCM bytes handed to the client so far.
    pub fn down_bytes(&self) -> u64 {
        self.down_bytes.load(Ordering::SeqCst)
    }

    pub fn mark_first_audio(&self) {
        self.mark(&self.first_audio);
    }
}

/// Aborts its task when dropped.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Like [`AbortOnDrop`] but can be defused.
struct Guard(Option<tokio::task::AbortHandle>);

impl Guard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            h.abort();
        }
    }
}

/// Cuts PCM chunks (any size, maybe an odd byte count) into even-length
/// frames of at most [`FRAME_BYTES`].
#[derive(Debug, Default)]
pub struct Framer {
    carry: Option<u8>,
}

impl Framer {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Bytes> {
        let mut data = Vec::with_capacity(chunk.len() + 1);
        data.extend(self.carry.take());
        data.extend_from_slice(chunk);
        if data.len() % 2 == 1 {
            self.carry = data.pop();
        }
        data.chunks(FRAME_BYTES)
            .map(Bytes::copy_from_slice)
            .collect()
    }
}

/// One piece of text for Kokoro.
struct Seg {
    text: String,
    /// The fixed phrase that replaces a sensitive reply (or the filler): never muted.
    safe: bool,
    /// Already-made PCM (a cached filler): Kokoro is not asked.
    pcm: Option<Bytes>,
    /// The ack filler, cached or synthesized.
    ack: bool,
}

/// The audio of one segment, as Kokoro streams it.
struct SegAudio {
    rx: mpsc::UnboundedReceiver<Result<Bytes, ApiError>>,
    safe: bool,
    ack: bool,
}

fn tts_error(e: &ApiError) -> PipeError {
    PipeError::Tts(e.clone())
}

/// The text of an ack that has to be synthesized: Lucina's own when it is a
/// short phrase, else the first filler phrase of the turn's language.
fn ack_text(text: &str, italian: bool) -> String {
    let t = text.trim();
    if t.is_empty() || t.chars().count() > 60 {
        (if italian {
            crate::voice_filler::PHRASES_IT[0]
        } else {
            crate::voice_filler::PHRASES_EN[0]
        })
        .to_owned()
    } else {
        t.to_owned()
    }
}

/// Hands segments to the TTS worker, within the [`tts::MAX_CHARS`] budget.
struct Emitter {
    seg_tx: mpsc::UnboundedSender<Seg>,
    /// The segments handed over, for the log.
    spoken: Vec<String>,
    chars: usize,
    /// Text was left out for the budget.
    capped: bool,
}

impl Emitter {
    fn emit(&mut self, text: String, safe: bool) {
        let n = text.chars().count();
        if self.chars + n > tts::MAX_CHARS {
            self.capped = true;
            return;
        }
        self.chars += n;
        self.spoken.push(text.clone());
        let _ = self.seg_tx.send(Seg {
            text,
            safe,
            pcm: None,
            ack: false,
        });
    }

    /// The ack filler, first in line: not part of `spoken` and outside the
    /// character budget.
    fn emit_ack(&self, text: String, pcm: Option<Bytes>) {
        let _ = self.seg_tx.send(Seg {
            text,
            safe: true,
            pcm,
            ack: true,
        });
    }
}

/// Synthesizes `seg_rx`'s segments in order and sends their PCM frames to `out`.
async fn tts_worker(
    st: AppState,
    _heard: String,
    _lang: Option<String>,
    mut seg_rx: mpsc::UnboundedReceiver<Seg>,
    shared: Arc<Shared>,
    muted: Arc<AtomicBool>,
    out: mpsc::Sender<Bytes>,
) -> Result<(), PipeError> {
    let (order_tx, mut order_rx) = mpsc::unbounded_channel::<SegAudio>();
    let requester = {
        let shared = shared.clone();
        let muted = muted.clone();
        tokio::spawn(async move {
            let permits = Arc::new(Semaphore::new(IN_FLIGHT));
            let mut synths = tokio::task::JoinSet::new();
            while let Some(seg) = seg_rx.recv().await {
                if muted.load(Ordering::SeqCst) && !seg.safe {
                    continue;
                }
                let voice = shared
                    .voice
                    .get_or_init(|| {
                        // The voice follows what she says, not what he said: an Italian question gets an
                        // English answer (owner rule 2026-11-02), read in the English voice.
                        tts::pick_voice(&st.tts.voices, None, None, &seg.text)
                            .unwrap_or_else(|_| tts::ENGLISH_VOICE.to_owned())
                    })
                    .clone();
                let (tx, rx) = mpsc::unbounded_channel();
                if let Some(pcm) = seg.pcm {
                    // Cached audio: ready now, no Kokoro call, no permit.
                    let _ = tx.send(Ok(pcm));
                    if order_tx
                        .send(SegAudio {
                            rx,
                            safe: seg.safe,
                            ack: seg.ack,
                        })
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                let Ok(permit) = permits.clone().acquire_owned().await else {
                    return;
                };
                if order_tx
                    .send(SegAudio {
                        rx,
                        safe: seg.safe,
                        ack: seg.ack,
                    })
                    .is_err()
                {
                    return;
                }
                let (st, shared) = (st.clone(), shared.clone());
                synths.spawn(async move {
                    let _permit = permit;
                    let stream = match st.tts.synthesize(&seg.text, &voice, 1.0, Format::Pcm).await
                    {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = tx.send(Err(e));
                            return;
                        }
                    };
                    let mut stream = stream;
                    while let Some(item) = stream.next().await {
                        match item {
                            Ok(chunk) => {
                                shared.mark(&shared.tts_first);
                                if tx.send(Ok(chunk)).is_err() {
                                    return;
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(Err(ApiError::new(
                                    axum::http::StatusCode::BAD_GATEWAY,
                                    "tts_unavailable",
                                    if e.is_timeout() {
                                        "the speech service timed out"
                                    } else {
                                        "the speech service failed"
                                    },
                                )));
                                return;
                            }
                        }
                    }
                });
            }
            // seg_rx closed: let the synths finish (they are aborted with this task).
            drop(order_tx);
            while synths.join_next().await.is_some() {}
        })
    };
    let _requester = AbortOnDrop(requester);

    let mut framer;
    while let Some(mut seg) = order_rx.recv().await {
        framer = Framer::default();
        while let Some(item) = seg.rx.recv().await {
            if muted.load(Ordering::SeqCst) && !seg.safe {
                break; // a sensitive reply: the rest of this segment is not sent
            }
            let chunk = item.map_err(|e| tts_error(&e))?;
            for frame in framer.push(&chunk) {
                if seg.ack {
                    shared.mark(&shared.ack_ms);
                }
                shared
                    .down_bytes
                    .fetch_add(frame.len() as u64, Ordering::SeqCst);
                out.send(frame).await.map_err(|_| PipeError::Closed)?;
            }
        }
    }
    Ok(())
}

/// Says `spec.heard`'s answer. Pushes PCM frames to `out`; returns the text
/// that was spoken (for logs and the `reply` message). See the module doc.
pub async fn speak_turn(
    st: AppState,
    spec: Spec,
    shared: Arc<Shared>,
    out: mpsc::Sender<Bytes>,
) -> Result<String, PipeError> {
    let mut drain_guard = Guard(None);
    let result = speak_inner(st, spec, shared, out, &mut drain_guard).await;
    // Finished (well or badly): Lucina's turn may run on.
    drain_guard.disarm();
    result
}

async fn speak_inner(
    st: AppState,
    spec: Spec,
    shared: Arc<Shared>,
    out: mpsc::Sender<Bytes>,
    drain_guard: &mut Guard,
) -> Result<String, PipeError> {
    let Spec {
        heard,
        lang,
        ident,
        facts,
    } = spec;
    let turn_id = crate::voice::client_message_id().map_err(PipeError::Lucina)?;
    let chat = ChatRequest {
        message: heard.clone(),
        session_id: Some(SESSION_ID.into()),
        client_message_id: Some(turn_id.clone()),
        // The shared "Lucina Voice" thread (protocol section 7).
        channel: Some(CHANNEL.into()),
        device: Some(ident.device_id.clone()),
        device_name: Some(ident.device_name.clone()),
        reply_to: None,
        voice_meta: Some(voice_meta(&turn_id, shared.stt(), &facts)),
    };
    let lucina = st.lucina().map_err(PipeError::Lucina)?;
    let mut events = lucina
        .chat_stream(&chat, true)
        .await
        .map_err(|e| PipeError::Lucina(e.into()))?;

    // Her fixed phrases and fillers are never Italian (owner rule 2026-11-02), whatever he spoke or the
    // client's `lang` says; the Italian variants stay for the transcription hint only.
    let italian = false;
    let phrase = |p: (&'static str, &'static str)| (if italian { p.0 } else { p.1 }).to_owned();

    let (seg_tx, seg_rx) = mpsc::unbounded_channel::<Seg>();
    let muted = Arc::new(AtomicBool::new(false));
    let mut worker = AbortOnDrop(tokio::spawn(tts_worker(
        st.clone(),
        heard.clone(),
        lang.clone(),
        seg_rx,
        shared.clone(),
        muted.clone(),
        out,
    )));

    let mut splitter = Splitter::new();
    let mut say = Emitter {
        seg_tx,
        spoken: Vec::new(),
        chars: 0,
        capped: false,
    };
    let mut deltas = false;
    let mut acked = false;

    // The reply, once settled: (text, sensitive, replaces).
    let mut reply: Option<(String, bool, bool)> = None;
    loop {
        let ev = tokio::select! {
            biased;
            done = &mut worker.0 => {
                // The worker only ends early on a failure.
                return Err(match done {
                    Ok(Err(e)) => e,
                    Ok(Ok(())) => PipeError::Tts(ApiError::internal("speech worker stopped")),
                    Err(_) => PipeError::Tts(ApiError::internal("speech worker failed")),
                });
            }
            ev = events.next() => ev.map_err(|e| PipeError::Lucina(e.into()))?,
        };
        match ev {
            None => break,
            Some(ChatEvent::Route {
                intent,
                confidence,
                jev_ms,
                fast,
                source,
                layers,
            }) => {
                shared.mark(&shared.route_ms);
                shared.merge_layers(layers);
                let info = RouteInfo {
                    intent,
                    confidence,
                    jev_ms,
                    fast,
                    source,
                }
                .clean();
                tracing::info!(
                    device = %ident.device_id,
                    intent = %info.intent,
                    confidence = ?info.confidence,
                    jev_ms = ?info.jev_ms,
                    fast = info.fast,
                    source = %info.source,
                    "voice route"
                );
                let _ = shared.route.set(info);
            }
            Some(ChatEvent::Ack { text }) => {
                // Only before anything of the answer is queued or sent, once,
                // and never for a turn Lucina answered fast.
                let fast = shared.route().is_some_and(|r| r.fast);
                if !acked && !fast && say.spoken.is_empty() && shared.down_bytes() == 0 {
                    acked = true;
                    let voice = crate::voice_filler::voice_for(italian);
                    // The voice of the whole turn, filler included.
                    shared.voice.get_or_init(|| voice.to_owned());
                    if let Some(f) = st.fillers.pick(italian) {
                        say.emit_ack(f.phrase.to_owned(), Some(f.pcm));
                    } else {
                        // Nothing cached (Kokoro was down): say the ack text
                        // like a sentence, and build the cache for next time.
                        say.emit_ack(ack_text(&text, italian), None);
                        let st = st.clone();
                        tokio::spawn(async move {
                            st.fillers.prewarm(&st.tts).await;
                        });
                    }
                }
            }
            Some(ChatEvent::Delta(t)) => {
                shared.mark(&shared.lucina_first);
                deltas = true;
                for seg in splitter.push(&t) {
                    say.emit(seg, false);
                }
            }
            Some(ChatEvent::Reply {
                text,
                sensitive,
                replaces,
            }) => {
                shared.mark(&shared.lucina_first);
                reply = Some((text, sensitive, replaces));
                break;
            }
            Some(ChatEvent::Done(v)) => {
                // No `reply` event: take the answer from the body (the SSE
                // `done` carries `{ok, data}`, a plain JSON answer just `data`).
                shared.mark(&shared.lucina_first);
                shared.merge_layers(RouteLayer::from_done(&v));
                let data = if v["data"].is_object() {
                    &v["data"]
                } else {
                    &v
                };
                reply = Some((
                    data["reply"].as_str().unwrap_or("").to_owned(),
                    data["sensitive"].as_bool() == Some(true),
                    false,
                ));
                break;
            }
        }
    }
    let (text, sensitive, replaces) = reply.unwrap_or_default();

    if sensitive {
        muted.store(true, Ordering::SeqCst);
        splitter.clear();
        say.spoken.clear();
        say.emit(phrase(SENSITIVE_PHRASE), true);
    } else if replaces && splitter.emitted() > 0 {
        tracing::warn!("lucina's reply replaces text that was already spoken; the rest is dropped");
        splitter.clear();
    } else {
        if replaces || !deltas {
            splitter.clear();
            for seg in splitter.push(&text) {
                say.emit(seg, false);
            }
        }
        if let Some(seg) = splitter.finish() {
            say.emit(seg, false);
        }
    }
    if say.spoken.is_empty() && !say.capped {
        say.emit(phrase(EMPTY_PHRASE), true);
    }
    let spoken = say.spoken;
    drop(say.seg_tx);

    // Lucina goes on after `reply` (memory, rich reply): keep reading so it
    // does not see a closed connection, but never wait for it. Its `done` body
    // may complete the Jev layers; the turn's record picks them up if it
    // arrives while the audio is still being made (see below).
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    let drain = tokio::spawn({
        let shared = shared.clone();
        async move {
            let mut done_tx = Some(done_tx);
            while let Ok(Some(ev)) = events.next().await {
                if let ChatEvent::Done(v) = ev {
                    shared.merge_layers(RouteLayer::from_done(&v));
                    if let Some(tx) = done_tx.take() {
                        let _ = tx.send(());
                    }
                }
            }
        }
    });
    *drain_guard = Guard(Some(drain.abort_handle()));

    let outcome = match (&mut worker.0).await {
        Ok(Ok(())) => Ok(spoken.join(" ")),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(PipeError::Tts(ApiError::internal("speech worker failed"))),
    };
    // A Lucina that routes turns (it sent `route`) also sends layers in `done`:
    // give it a moment after the audio, no more.
    if outcome.is_ok() && shared.route().is_some() {
        let _ = tokio::time::timeout(DONE_GRACE, done_rx).await;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_even_and_bounded() {
        let mut f = Framer::default();
        // 5 bytes: 4 go out, 1 waits.
        let out = f.push(&[1, 2, 3, 4, 5]);
        assert_eq!(out.concat(), [1, 2, 3, 4]);
        // The waiting byte leads the next chunk.
        let out = f.push(&[6, 7, 8]);
        assert_eq!(out.concat(), [5, 6, 7, 8]);
        // Big chunk: frames of at most 8 KiB, all even.
        let big: Vec<u8> = (0..20_001u32).map(|i| (i % 251) as u8).collect();
        let mut f = Framer::default();
        let out = f.push(&big);
        assert!(out
            .iter()
            .all(|b| b.len() <= FRAME_BYTES && b.len() % 2 == 0));
        assert_eq!(out.len(), 3);
        assert_eq!(out.concat(), big[..20_000]);
        assert!(f.push(&[]).is_empty());
    }

    #[test]
    fn timings_are_set_once() {
        let s = Shared::new(Instant::now());
        assert_eq!(s.lucina_first(), None);
        s.mark(&s.lucina_first);
        let first = s.lucina_first().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.mark(&s.lucina_first);
        assert_eq!(s.lucina_first(), Some(first));
    }
}
