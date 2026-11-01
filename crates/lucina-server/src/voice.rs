//! The voice stream of `POST /api/secretary/chat` (LucinaVoice, docs/lucinavoice-protocol.md section 1).
//!
//! A voice device wants to start speaking on the first sentence, not after the whole turn (the `reply`
//! model call, then the rich-reply composer, then Honcho and the timeline). Two things make that so:
//!
//! - `"voice": true` in the body: a spoken-style rule joins the turn's system text ([`SPOKEN_RULE`]).
//!   It works with or without the stream.
//! - `Accept: text/event-stream`: the route answers at once with SSE ([`respond`]) and runs the same
//!   turn in a task of its own. Events, in this order: `delta` (zero or more), `reply` (once), then
//!   `done` or `error`; a `: ping` comment every 15 s while the turn runs.
//!
//! The turn code does not know about either. A per-turn [`VoiceSink`] rides in a task-local
//! ([`scope`]); with none the turn behaves exactly as before. It is fed from two places:
//!
//! - [`delta`], from `planner::Planner::generate_reply` (and so from every caller of it: the casual-chat
//!   shortcut, the skill replies, the fallbacks), through [`lucina_llm::ChatModel::complete_streaming`];
//! - [`settle`], from `chat::run_turn_scoped` (and `tools_turn`) the moment the turn's [`Finish`] is
//!   known, before rich-reply, Honcho, memory and the timeline. Answers that never reach a `Finish`
//!   (the `/secret` command, OTP, a replayed `clientMessageId`) settle in [`respond`] from the result.
//!
//! Two more events come from the Jev fast path (`chat::voice_fast`, protocol section 6): [`route`] (once, as
//! soon as the turn's route is decided, before any reply text) and [`ack`] (at most once, for a slow path;
//! never for a fast turn nor once text was streamed).
//!
//! A client that disconnects only drops the receiving end: the turn task goes on and writes the timeline.
//!
//! [`Finish`]: crate::chat

use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::Response;
use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::api::ApiError;
use crate::surface::BearerVerified;
use crate::AppState;

/// The spoken-style rule `"voice": true` adds to the turn's system text.
pub const SPOKEN_RULE: &str = "This turn is spoken aloud through a speaker: answer in one to three short sentences, no markdown, lists, emoji or URLs; write numbers, dates and times the way they are said.";

/// `: ping` period while a streamed turn runs.
pub const PING_EVERY: Duration = Duration::from_secs(15);

/// What a streamed turn tells its client.
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceEvent {
    /// Reply text as the model writes it.
    Delta(String),
    /// The turn's reply text, settled. `replaces`: it is not the concatenation of the deltas, so what
    /// was said from the deltas is superseded by `text`.
    Reply {
        text: String,
        sensitive: bool,
        replaces: bool,
    },
    /// The route the turn takes (protocol section 6): sent once, before any reply text.
    Route(RouteInfo),
    /// "One moment…": the turn takes a slow path and nothing was said yet. At most once.
    Ack(String),
    /// The whole non-streaming response body (`{"ok":true,"data":{…}}`).
    Done(Value),
    Error {
        code: String,
        message: String,
    },
}

/// The `route` event's data. Never holds transcript text.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteInfo {
    pub intent: String,
    pub confidence: f64,
    pub jev_ms: u64,
    pub fast: bool,
    /// `jev` (Jev's verdict decided), `tier0` (Jev gave none; a tier-0 rule did) or `fallback`.
    pub source: &'static str,
    /// The Jev layers decided before the route (protocol section 9); empty: the key is left out.
    pub layers: Vec<Layer>,
}

impl RouteInfo {
    pub fn json(&self) -> Value {
        let mut v = json!({"intent": self.intent, "confidence": self.confidence, "jev_ms": self.jev_ms,
            "fast": self.fast, "source": self.source});
        if !self.layers.is_empty() {
            v["layers"] = Value::Array(self.layers.iter().map(Layer::json).collect());
        }
        v
    }

    /// The route as the timeline's `meta.voice.route` (camelCase keys, layers included).
    fn meta_json(&self) -> Value {
        let mut v = json!({"intent": self.intent, "confidence": self.confidence, "jevMs": self.jev_ms,
            "fast": self.fast, "source": self.source});
        if !self.layers.is_empty() {
            v["layers"] = Value::Array(self.layers.iter().map(Layer::json).collect());
        }
        v
    }
}

/// One Jev decision of a spoken turn (protocol section 9): `{layer, verdict, confidence, ms, used}`.
/// `verdict` is a short word (an intent, `ok`/`no`, `ask`/`act`, a candidate's index): never text of the turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    /// `intent`, `slots`, `confirm` or `vocab`.
    pub layer: &'static str,
    pub verdict: String,
    pub confidence: f64,
    pub ms: u64,
    /// The verdict changed what the turn did.
    pub used: bool,
}

impl Layer {
    pub fn json(&self) -> Value {
        json!({"layer": self.layer, "verdict": self.verdict, "confidence": self.confidence,
            "ms": self.ms, "used": self.used})
    }
}

/// The most Jev calls one spoken turn may make, whatever the layers want (the keyproxy's caps come on top).
pub const MAX_JEV_CALLS: u8 = 3;

/// What a spoken turn decided with Jev, and how many calls it has used. One per `voice: true` turn
/// (`chat::handle_chat`); the layers are filled in order as they are decided.
#[derive(Default)]
pub struct Trace {
    calls: std::sync::atomic::AtomicU8,
    inner: Mutex<TraceInner>,
}

#[derive(Default)]
struct TraceInner {
    layers: Vec<Layer>,
    route: Option<RouteInfo>,
}

impl Trace {
    /// Reserves one of the turn's [`MAX_JEV_CALLS`]; false when they are used up (the layer then fails open).
    pub fn take_call(&self) -> bool {
        use std::sync::atomic::Ordering::SeqCst;
        self.calls
            .try_update(SeqCst, SeqCst, |n| (n < MAX_JEV_CALLS).then_some(n + 1))
            .is_ok()
    }

    pub fn calls(&self) -> u8 {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, TraceInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push(&self, layer: Layer) {
        self.inner().layers.push(layer);
    }

    pub fn layers(&self) -> Vec<Layer> {
        self.inner().layers.clone()
    }

    pub fn set_route(&self, route: RouteInfo) {
        self.inner().route = Some(route);
    }

    /// Anything to report? (A turn no layer touched, or with no Jev client, leaves the plain body alone.)
    pub fn is_empty(&self) -> bool {
        self.inner().layers.is_empty()
    }

    /// The `done` body's `voice` object: `{route, layers, jevCalls}`.
    pub fn json(&self) -> Value {
        let i = self.inner();
        json!({
            "route": i.route.as_ref().map(RouteInfo::meta_json),
            "layers": i.layers.iter().map(Layer::json).collect::<Vec<_>>(),
            "jevCalls": self.calls(),
        })
    }

    /// The timeline's `meta.voice`: the request's `base` (`turnId`, `ms`) plus the route with every layer.
    pub fn meta(&self, base: Option<&Value>) -> Value {
        let mut v = base.cloned().filter(Value::is_object).unwrap_or_else(|| json!({}));
        let i = self.inner();
        if let Some(r) = &i.route {
            let mut route = r.meta_json();
            route["layers"] = Value::Array(i.layers.iter().map(Layer::json).collect());
            v["route"] = route;
        } else if !i.layers.is_empty() {
            v["route"] = json!({"layers": i.layers.iter().map(Layer::json).collect::<Vec<_>>()});
        }
        v
    }
}

impl VoiceEvent {
    /// The SSE frame.
    pub fn frame(&self) -> String {
        let (name, data) = match self {
            VoiceEvent::Delta(text) => ("delta", json!({ "text": text })),
            VoiceEvent::Reply {
                text,
                sensitive,
                replaces,
            } => (
                "reply",
                json!({"text": text, "sensitive": sensitive, "replaces": replaces}),
            ),
            VoiceEvent::Route(info) => ("route", info.json()),
            VoiceEvent::Ack(text) => ("ack", json!({"text": text})),
            VoiceEvent::Done(body) => ("done", body.clone()),
            VoiceEvent::Error { code, message } => {
                ("error", json!({"code": code, "message": message}))
            }
        };
        format!("event: {name}\ndata: {data}\n\n")
    }

    fn last(&self) -> bool {
        matches!(self, VoiceEvent::Done(_) | VoiceEvent::Error { .. })
    }
}

#[derive(Default)]
struct State {
    /// The deltas forwarded so far, concatenated.
    streamed: String,
    /// A delta held tool-call markup: nothing more is forwarded (it is never speech).
    markup: bool,
    /// The `reply` event went out.
    replied: bool,
    /// The `route` event went out.
    routed: bool,
    /// The route is a fast one: no `ack`, ever.
    fast: bool,
    /// The `ack` event went out.
    acked: bool,
}

/// One streamed turn's event sink.
pub struct VoiceSink {
    tx: UnboundedSender<VoiceEvent>,
    state: Mutex<State>,
}

impl VoiceSink {
    pub fn new(tx: UnboundedSender<VoiceEvent>) -> Arc<Self> {
        Arc::new(Self {
            tx,
            state: Mutex::new(State::default()),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A fragment of reply text. Dropped once the reply is settled, and from the first one that holds
    /// tool-call markup on (the model's retry text then goes out in `reply`, with `replaces`).
    pub fn delta(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut s = self.state();
        if s.replied || s.markup {
            return;
        }
        if lucina_llm::has_tool_markup(&format!("{}{text}", s.streamed)) {
            s.markup = true;
            return;
        }
        s.streamed.push_str(text);
        // A gone receiver is a client that left; the turn goes on.
        let _ = self.tx.send(VoiceEvent::Delta(text.to_owned()));
    }

    /// The turn's route (once per turn; later calls do nothing).
    pub fn route(&self, info: RouteInfo) {
        let mut s = self.state();
        if s.routed {
            return;
        }
        s.routed = true;
        s.fast = info.fast;
        let _ = self.tx.send(VoiceEvent::Route(info));
    }

    /// "One moment…", once per turn, after the route and only while nothing was said: never for a fast
    /// turn, never once text was streamed or the reply settled.
    pub fn ack(&self, text: &str) {
        let mut s = self.state();
        if !s.routed || s.fast || s.acked || s.replied || !s.streamed.is_empty() {
            return;
        }
        s.acked = true;
        let _ = self.tx.send(VoiceEvent::Ack(text.to_owned()));
    }

    /// The turn's settled reply text. Exactly one `reply` event per turn: later calls do nothing.
    pub fn settle(&self, text: &str, sensitive: bool) {
        let mut s = self.state();
        if s.replied {
            return;
        }
        s.replied = true;
        let text = lucina_secrets::redactor().redact(text).into_owned();
        let replaces = s.streamed.trim() != text.trim();
        let _ = self.tx.send(VoiceEvent::Reply {
            text,
            sensitive,
            replaces,
        });
    }

    fn replied(&self) -> bool {
        self.state().replied
    }
}

tokio::task_local! {
    /// Jev's route for a spoken turn is not decided yet (`chat::voice_fast`): the context building runs
    /// next to it but must not start a model call a fast route would make pointless ([`hold`]).
    static GATE: tokio::sync::watch::Receiver<bool>;
    /// The streamed turn's sink. Absent: the turn behaves as it always did.
    static SINK: Arc<VoiceSink>;
    /// `"voice": true`: the turn is spoken aloud (set by `chat::handle_chat`, streamed or not).
    pub static SPOKEN: bool;
    /// The spoken turn's Jev decisions and call count (absent on turns that are not spoken).
    static TRACE: Arc<Trace>;
    /// The spoken turn's one intent verdict, shared with the context building (`context::classify`).
    static INTENT: tokio::sync::watch::Receiver<Option<SharedIntent>>;
}

/// Jev's intent verdict in the classifier's JSON shape plus its confidence; `None`: Jev gave none.
pub type SharedIntent = Option<(Value, f64)>;

/// The channel a spoken turn's intent verdict travels on: the turn's Jev call sends once.
pub fn intent_channel() -> (
    tokio::sync::watch::Sender<Option<SharedIntent>>,
    tokio::sync::watch::Receiver<Option<SharedIntent>>,
) {
    tokio::sync::watch::channel(None)
}

/// Runs `fut` with `rx` as the turn's shared intent verdict (none: the classifier asks Jev itself).
pub async fn with_intent<F: std::future::Future>(
    rx: Option<tokio::sync::watch::Receiver<Option<SharedIntent>>>,
    fut: F,
) -> F::Output {
    match rx {
        Some(rx) => INTENT.scope(rx, fut).await,
        None => fut.await,
    }
}

/// The spoken turn's intent verdict, waiting for the turn's own Jev call (capped by the caller): `Some(None)`
/// when it gave none, `None` when this turn has no such call (the classifier then makes its own).
pub async fn shared_intent() -> Option<SharedIntent> {
    let mut rx = INTENT.try_with(Clone::clone).ok()?;
    let verdict = rx.wait_for(Option::is_some).await.map(|v| v.clone());
    match verdict {
        Ok(v) => v,
        // The call's task is gone without a word (a panic): no verdict.
        Err(_) => Some(None),
    }
}

/// Runs `fut` with `trace` as the spoken turn's trace (none: no change).
pub async fn with_trace<F: std::future::Future>(trace: Option<Arc<Trace>>, fut: F) -> F::Output {
    match trace {
        Some(t) => TRACE.scope(t, fut).await,
        None => fut.await,
    }
}

/// The current spoken turn's trace.
pub fn trace() -> Option<Arc<Trace>> {
    TRACE.try_with(Arc::clone).ok()
}

/// Runs `fut` with `sink` as the turn's sink (none: no change).
pub async fn scope<F: std::future::Future>(sink: Option<Arc<VoiceSink>>, fut: F) -> F::Output {
    match sink {
        Some(s) => SINK.scope(s, fut).await,
        None => fut.await,
    }
}

/// A gate for a spoken turn's context building: a future wrapped by [`gated`] waits at [`hold`] until the
/// returned sender sends `true`.
pub fn gate() -> (tokio::sync::watch::Sender<bool>, tokio::sync::watch::Receiver<bool>) {
    tokio::sync::watch::channel(false)
}

/// Runs `fut` behind `rx`'s gate.
pub async fn gated<F: std::future::Future>(rx: tokio::sync::watch::Receiver<bool>, fut: F) -> F::Output {
    GATE.scope(rx, fut).await
}

/// Waits for the turn's route to be decided, when the turn has a gate (nothing otherwise). A model call
/// that only the normal path needs waits here; a fast route drops the turn's context building instead.
pub async fn hold() {
    if let Ok(mut rx) = GATE.try_with(Clone::clone) {
        let _ = rx.wait_for(|open| *open).await;
    }
}

/// Is the current turn streamed to a client?
pub fn streaming() -> bool {
    SINK.try_with(|_| ()).is_ok()
}

/// Is the current turn spoken aloud (`"voice": true`)?
pub fn spoken() -> bool {
    SPOKEN.try_with(|s| *s).unwrap_or(false)
}

/// Feeds the current turn's sink (nothing outside a streamed turn).
pub fn delta(text: &str) {
    let _ = SINK.try_with(|s| s.delta(text));
}

/// Announces the current turn's route (nothing outside a streamed turn; once per turn).
pub fn route(info: RouteInfo) {
    let _ = SINK.try_with(|s| s.route(info));
}

/// The `ack` event for a slow path (nothing outside a streamed turn; at most once, never after the first
/// words).
pub fn ack(text: &str) {
    let _ = SINK.try_with(|s| s.ack(text));
}

/// "Un attimo…" in the turn's language (`planner::TURN_LANG`).
pub fn ack_text() -> &'static str {
    match crate::jev_yesno::turn_lang_code() {
        "it" => "Un attimo…",
        "da" => "Et øjeblik…",
        _ => "One moment…",
    }
}

/// Settles the current turn's reply (nothing outside a streamed turn; once per turn).
pub fn settle(text: &str, sensitive: bool) {
    let _ = SINK.try_with(|s| s.settle(text, sensitive));
}

/// Does the request ask for the event stream?
pub fn wants_stream(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| {
            v.split(',').any(|part| {
                part.split(';')
                    .next()
                    .is_some_and(|m| m.trim().eq_ignore_ascii_case("text/event-stream"))
            })
        })
}

fn error_event(e: &ApiError) -> VoiceEvent {
    if e.status == axum::http::StatusCode::INTERNAL_SERVER_ERROR {
        tracing::error!(error = %e.message, "secretary chat stream failed");
        return VoiceEvent::Error {
            code: "internal_error".into(),
            message: "Internal error".into(),
        };
    }
    VoiceEvent::Error {
        code: e.code.clone(),
        message: e.message.clone(),
    }
}

/// The SSE answer to a chat turn: the response at once, the turn in a task of its own.
pub fn respond(
    st: AppState,
    bearer: BearerVerified,
    body: Value,
    ping_every: Duration,
) -> Response {
    let (tx, rx) = unbounded_channel::<VoiceEvent>();
    let sink = VoiceSink::new(tx.clone());
    let turn = tokio::spawn(scope(
        Some(sink.clone()),
        crate::chat::handle_chat(st, bearer, body),
    ));
    // The turn's end: the reply event if nothing settled it yet, then `done` or `error`.
    tokio::spawn(async move {
        let end = match turn.await {
            Ok(Ok(mut out)) => {
                // The route's reply sink (app, web and Lucina MCP all read it here).
                lucina_secrets::redactor().redact_json(&mut out);
                if !sink.replied() {
                    sink.settle(
                        out["reply"].as_str().unwrap_or(""),
                        out["sensitive"].as_bool().unwrap_or(false),
                    );
                }
                VoiceEvent::Done(json!({"ok": true, "data": out}))
            }
            Ok(Err(e)) => error_event(&e),
            Err(e) => {
                tracing::error!(error = %e, "secretary chat stream task failed");
                VoiceEvent::Error {
                    code: "internal_error".into(),
                    message: "Internal error".into(),
                }
            }
        };
        let _ = tx.send(end);
    });
    let pump = Pump {
        rx,
        opened: false,
        ping: None,
    };
    let stream =
        futures_util::stream::unfold((pump, false), move |(mut pump, finished)| async move {
            if finished {
                return None;
            }
            let (bytes, last) = pump.next(ping_every).await?;
            Some((Ok::<_, Infallible>(bytes), (pump, last)))
        });
    let mut resp = Response::new(Body::from_stream(stream));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    h.insert("x-accel-buffering", HeaderValue::from_static("no"));
    resp
}

/// The body's source: the first bytes are a comment (so proxies and clients see the stream open), then
/// each event, with a ping when nothing came for `ping_every`.
struct Pump {
    rx: tokio::sync::mpsc::UnboundedReceiver<VoiceEvent>,
    opened: bool,
    ping: Option<tokio::time::Interval>,
}

impl Pump {
    /// The next chunk and whether it was the last. `None`: the sender is gone without a final event.
    async fn next(&mut self, ping_every: Duration) -> Option<(Bytes, bool)> {
        if !self.opened {
            self.opened = true;
            return Some((Bytes::from_static(b": connected\n\n"), false));
        }
        let ping = self.ping.get_or_insert_with(|| {
            tokio::time::interval_at(tokio::time::Instant::now() + ping_every, ping_every)
        });
        tokio::select! {
            ev = self.rx.recv() => {
                let ev = ev?;
                let last = ev.last();
                Some((Bytes::from(ev.frame()), last))
            }
            _ = ping.tick() => Some((Bytes::from_static(b": ping\n\n"), false)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_the_documented_events() {
        assert_eq!(
            VoiceEvent::Delta("Ciao ".into()).frame(),
            "event: delta\ndata: {\"text\":\"Ciao \"}\n\n"
        );
        let f = VoiceEvent::Reply {
            text: "Ciao.".into(),
            sensitive: false,
            replaces: true,
        }
        .frame();
        assert_eq!(
            f,
            "event: reply\ndata: {\"replaces\":true,\"sensitive\":false,\"text\":\"Ciao.\"}\n\n"
        );
        assert!(VoiceEvent::Error {
            code: "x".into(),
            message: "y".into()
        }
        .last());
    }

    #[test]
    fn the_accept_header_decides() {
        let mut h = HeaderMap::new();
        assert!(!wants_stream(&h));
        h.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        assert!(!wants_stream(&h));
        h.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/event-stream"),
        );
        assert!(wants_stream(&h));
        h.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/json, Text/Event-Stream;q=0.9"),
        );
        assert!(wants_stream(&h));
    }

    fn sink() -> (
        Arc<VoiceSink>,
        tokio::sync::mpsc::UnboundedReceiver<VoiceEvent>,
    ) {
        let (tx, rx) = unbounded_channel();
        (VoiceSink::new(tx), rx)
    }

    #[test]
    fn the_reply_is_settled_once_and_compares_with_the_deltas() {
        let (s, mut rx) = sink();
        s.delta("Ciao, ");
        s.delta("come stai?");
        s.settle("Ciao, come stai? ", false);
        s.settle("another", true);
        s.delta("late");
        assert_eq!(rx.try_recv().unwrap(), VoiceEvent::Delta("Ciao, ".into()));
        assert_eq!(
            rx.try_recv().unwrap(),
            VoiceEvent::Delta("come stai?".into())
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            VoiceEvent::Reply {
                text: "Ciao, come stai? ".into(),
                sensitive: false,
                replaces: false
            }
        );
        assert!(rx.try_recv().is_err(), "once, and nothing after it");
    }

    #[test]
    fn markup_is_never_forwarded_and_the_reply_replaces() {
        let (s, mut rx) = sink();
        s.delta("Controllo. ");
        s.delta("<\u{ff5c}\u{ff5c}DSML\u{ff5c}\u{ff5c} calls>");
        s.delta("more");
        s.settle("Fatto.", false);
        assert_eq!(
            rx.try_recv().unwrap(),
            VoiceEvent::Delta("Controllo. ".into())
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            VoiceEvent::Reply {
                text: "Fatto.".into(),
                sensitive: false,
                replaces: true
            }
        );
        assert!(rx.try_recv().is_err());
    }

    fn info(fast: bool) -> RouteInfo {
        RouteInfo {
            intent: "temporal_query".into(),
            confidence: 0.95,
            jev_ms: 210,
            fast,
            source: "jev",
            layers: Vec::new(),
        }
    }

    #[test]
    fn route_and_ack_frames_are_the_documented_events() {
        assert_eq!(
            VoiceEvent::Route(info(true)).frame(),
            "event: route\ndata: {\"confidence\":0.95,\"fast\":true,\"intent\":\"temporal_query\",\"jev_ms\":210,\"source\":\"jev\"}\n\n"
        );
        assert_eq!(
            VoiceEvent::Ack("Un attimo…".into()).frame(),
            "event: ack\ndata: {\"text\":\"Un attimo…\"}\n\n"
        );
        assert!(!VoiceEvent::Route(info(false)).last());
        assert!(!VoiceEvent::Ack("x".into()).last());
    }

    #[test]
    fn the_ack_comes_once_after_the_route_and_before_any_words() {
        // Never before the route (a turn that was not routed has no ack).
        let (s, mut rx) = sink();
        s.ack("Un attimo…");
        assert!(rx.try_recv().is_err());
        // Route, then the ack once.
        s.route(info(false));
        s.route(info(true));
        s.ack("Un attimo…");
        s.ack("Un attimo…");
        assert!(matches!(rx.try_recv().unwrap(), VoiceEvent::Route(i) if !i.fast));
        assert_eq!(rx.try_recv().unwrap(), VoiceEvent::Ack("Un attimo…".into()));
        assert!(rx.try_recv().is_err(), "one route, one ack");

        // A fast turn never gets one.
        let (s, mut rx) = sink();
        s.route(info(true));
        s.ack("Un attimo…");
        assert!(matches!(rx.try_recv().unwrap(), VoiceEvent::Route(_)));
        assert!(rx.try_recv().is_err());

        // Not once words were streamed, nor once the reply is settled.
        let (s, mut rx) = sink();
        s.route(info(false));
        s.delta("Ciao");
        s.ack("Un attimo…");
        let (s2, mut rx2) = sink();
        s2.route(info(false));
        s2.settle("Fatto.", false);
        s2.ack("Un attimo…");
        assert!(matches!(rx.try_recv().unwrap(), VoiceEvent::Route(_)));
        assert!(matches!(rx.try_recv().unwrap(), VoiceEvent::Delta(_)));
        assert!(rx.try_recv().is_err());
        assert!(matches!(rx2.try_recv().unwrap(), VoiceEvent::Route(_)));
        assert!(matches!(rx2.try_recv().unwrap(), VoiceEvent::Reply { .. }));
        assert!(rx2.try_recv().is_err());
    }

    #[tokio::test]
    async fn the_gate_holds_until_it_opens_and_is_free_without_one() {
        hold().await; // no gate: returns at once
        let (open, rx) = gate();
        let started = std::time::Instant::now();
        let waiter = tokio::spawn(gated(rx, async move {
            hold().await;
            started.elapsed()
        }));
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(!waiter.is_finished());
        open.send(true).unwrap();
        assert!(waiter.await.unwrap() >= Duration::from_millis(55));
    }

    #[tokio::test]
    async fn the_body_opens_with_a_comment_and_pings_while_idle() {
        let (tx, rx) = unbounded_channel();
        let mut pump = Pump {
            rx,
            opened: false,
            ping: None,
        };
        let every = Duration::from_millis(30);
        let (first, last) = pump.next(every).await.unwrap();
        assert_eq!((&first[..], last), (&b": connected\n\n"[..], false));
        let started = std::time::Instant::now();
        let (ping, last) = pump.next(every).await.unwrap();
        assert_eq!((&ping[..], last), (&b": ping\n\n"[..], false));
        assert!(started.elapsed() >= Duration::from_millis(25));
        tx.send(VoiceEvent::Done(json!({"ok": true}))).unwrap();
        let (done, last) = pump.next(every).await.unwrap();
        assert!(done.starts_with(b"event: done\n"));
        assert!(last);
        drop(tx);
        assert!(pump.next(every).await.is_none());
    }

    #[tokio::test]
    async fn outside_a_streamed_turn_nothing_happens() {
        assert!(!streaming());
        assert!(!spoken());
        delta("x");
        settle("x", false);
        let (s, mut rx) = sink();
        scope(Some(s), async {
            assert!(streaming());
            delta("Hi");
            settle("Hi", true);
        })
        .await;
        assert!(matches!(rx.try_recv().unwrap(), VoiceEvent::Delta(_)));
        assert!(matches!(
            rx.try_recv().unwrap(),
            VoiceEvent::Reply {
                sensitive: true,
                replaces: false,
                ..
            }
        ));
    }

    #[test]
    fn the_route_event_carries_its_layers_when_there_are_any() {
        let mut i = info(false);
        i.layers = vec![Layer {
            layer: "intent",
            verdict: "temporal_query".into(),
            confidence: 0.95,
            ms: 210,
            used: true,
        }];
        let v = i.json();
        assert_eq!(v["layers"][0]["layer"], "intent");
        assert_eq!(v["layers"][0]["used"], true);
        assert!(info(false).json().get("layers").is_none());
    }

    #[test]
    fn a_trace_reports_layers_and_the_timeline_meta_adds_them_to_the_request_base() {
        let t = Trace::default();
        assert!(t.is_empty());
        t.push(Layer {
            layer: "vocab",
            verdict: "c1".into(),
            confidence: 0.9,
            ms: 120,
            used: true,
        });
        let mut r = info(true);
        r.layers = t.layers();
        t.set_route(r);
        let meta = t.meta(Some(&json!({"turnId": "t1", "ms": {"stt": 80}})));
        assert_eq!(meta["turnId"], "t1");
        assert_eq!(meta["ms"]["stt"], 80);
        assert_eq!(meta["route"]["intent"], "temporal_query");
        assert_eq!(meta["route"]["layers"][0]["layer"], "vocab");
        assert_eq!(t.json()["jevCalls"], 0);
    }
}
