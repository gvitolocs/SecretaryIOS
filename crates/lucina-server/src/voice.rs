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
    /// The whole non-streaming response body (`{"ok":true,"data":{…}}`).
    Done(Value),
    Error {
        code: String,
        message: String,
    },
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
    /// The streamed turn's sink. Absent: the turn behaves as it always did.
    static SINK: Arc<VoiceSink>;
    /// `"voice": true`: the turn is spoken aloud (set by `chat::handle_chat`, streamed or not).
    pub static SPOKEN: bool;
}

/// Runs `fut` with `sink` as the turn's sink (none: no change).
pub async fn scope<F: std::future::Future>(sink: Option<Arc<VoiceSink>>, fut: F) -> F::Output {
    match sink {
        Some(s) => SINK.scope(s, fut).await,
        None => fut.await,
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
}
