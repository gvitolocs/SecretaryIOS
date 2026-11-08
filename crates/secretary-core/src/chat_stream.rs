//! The streaming chat answer: `POST /api/secretary/chat` with
//! `Accept: text/event-stream` (see [`crate::LucinaClient::chat_stream`]).
//!
//! Lucina answers with Server-Sent Events:
//!
//! | event | data | |
//! |---|---|---|
//! | `route` | `{"intent","confidence","jev_ms","fast","source","layers"?}` | voice turns: how Lucina routed the turn, before any reply text (optional) |
//! | `ack` | `{"text"}` | voice turns: the turn takes a slow path; say a filler (optional, at most once) |
//! | `delta` | `{"text"}` | a fragment of the reply as the model writes it |
//! | `reply` | `{"text","sensitive","replaces"}` | the settled reply, sent once |
//! | `done` | the JSON the plain call returns | last event of a good turn |
//! | `error` | `{"code","message"}` | the turn failed; no `done` follows |
//!
//! A Lucina without the feature answers the same request with its ordinary
//! JSON envelope; [`ChatStream`] then yields one `Reply` (and the envelope's
//! `data` as `Done`), so callers handle both with one loop.

use std::collections::VecDeque;

use serde_json::Value;

use crate::error::{Error, Result};

/// One event of a streamed chat turn.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    /// Voice turns only: how the turn was routed (`route` event). Sent before
    /// any reply text; a Lucina without the feature never sends it.
    Route {
        intent: String,
        confidence: Option<f64>,
        jev_ms: Option<u64>,
        /// Answered without the planner LLM.
        fast: bool,
        /// `jev`, `tier0` or `fallback`.
        source: String,
        /// The Jev layers decided so far (protocol section 9); empty without them.
        layers: Vec<RouteLayer>,
    },
    /// Voice turns only: the turn will take a slow path (`ack` event), so the
    /// client may fill the wait with a short spoken phrase. At most once.
    Ack { text: String },
    /// A fragment of the reply text.
    Delta(String),
    /// The settled reply. `replaces`: `text` is not the concatenation of the
    /// deltas (a retry, a fallback text, a sensitive replacement).
    Reply {
        text: String,
        sensitive: bool,
        replaces: bool,
    },
    /// What the non-streaming call would have returned (`data` of the envelope).
    Done(Value),
}

/// Most layers kept from one turn (a voice turn makes at most 3 Jev calls).
pub const MAX_LAYERS: usize = 8;

/// One layer of a voice turn's Jev decisions (protocol section 9):
/// `{layer, verdict, confidence, ms, used}`. Facts about the decision, never transcript text.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RouteLayer {
    /// `intent`, `slots`, `confirm` or `vocab`.
    pub layer: String,
    /// What the layer decided, as a short word.
    pub verdict: String,
    pub confidence: Option<f64>,
    pub ms: Option<u64>,
    /// The verdict was acted on (not only computed).
    pub used: bool,
}

fn clean_text(v: &Value, max: usize) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Bool(_) | Value::Number(_) => v.to_string(),
        _ => String::new(),
    };
    s.chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>'))
        .take(max)
        .collect()
}

impl RouteLayer {
    /// The layers of a `layers` JSON array: at most [`MAX_LAYERS`], strings cut and without
    /// control characters or `<` `>`, confidence in 0..=1, ms bounded. An entry without a
    /// layer name is dropped; anything that is not an array gives no layers.
    pub fn parse_all(v: &Value) -> Vec<Self> {
        let Some(list) = v.as_array() else {
            return Vec::new();
        };
        list.iter()
            .filter_map(|l| {
                let layer = clean_text(&l["layer"], 32);
                if layer.is_empty() {
                    return None;
                }
                Some(Self {
                    layer,
                    verdict: clean_text(&l["verdict"], 64),
                    confidence: l["confidence"]
                        .as_f64()
                        .filter(|c| c.is_finite())
                        .map(|c| c.clamp(0.0, 1.0)),
                    ms: l["ms"]
                        .as_f64()
                        .filter(|m| m.is_finite())
                        .map(|m| m.clamp(0.0, 10_000_000.0) as u64),
                    used: l["used"].as_bool() == Some(true),
                })
            })
            .take(MAX_LAYERS)
            .collect()
    }

    /// The layers a finished chat turn reports: `data.voice.layers` of the `done` body
    /// (the SSE `done` carries `{ok, data}`, a plain JSON answer just `data`).
    pub fn from_done(done: &Value) -> Vec<Self> {
        let data = if done["data"].is_object() { &done["data"] } else { done };
        Self::parse_all(&data["voice"]["layers"])
    }
}

/// One parsed SSE block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseBlock {
    pub event: String,
    pub data: String,
}

/// Incremental SSE parser: bytes in, complete blocks out. Handles `\n`,
/// `\r\n` and `\r` line ends, comment lines (`: ping`), `event:` and
/// multi-line `data:`.
#[derive(Debug, Default)]
pub struct SseParser {
    line: Vec<u8>,
    /// The previous byte was `\r` (a following `\n` belongs to it).
    after_cr: bool,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseBlock> {
        let mut out = Vec::new();
        for &b in bytes {
            match b {
                b'\n' if self.after_cr => self.after_cr = false,
                b'\n' | b'\r' => {
                    self.after_cr = b == b'\r';
                    let line = std::mem::take(&mut self.line);
                    self.line_done(&line, &mut out);
                }
                _ => {
                    self.after_cr = false;
                    self.line.push(b);
                }
            }
        }
        out
    }

    fn line_done(&mut self, line: &[u8], out: &mut Vec<SseBlock>) {
        if line.is_empty() {
            // Blank line: dispatch what was collected.
            if !self.data.is_empty() || self.event.is_some() {
                out.push(SseBlock {
                    event: self.event.take().unwrap_or_else(|| "message".into()),
                    data: std::mem::take(&mut self.data).join("\n"),
                });
            }
            return;
        }
        if line[0] == b':' {
            return; // comment
        }
        let text = String::from_utf8_lossy(line);
        let (field, value) = match text.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (text.as_ref(), ""),
        };
        match field {
            "event" => self.event = Some(value.to_owned()),
            "data" => self.data.push(value.to_owned()),
            _ => {} // id, retry, unknown
        }
    }
}

/// An SSE block as a [`ChatEvent`]. `Ok(None)` for events to ignore;
/// an `error` block is an `Err`.
fn decode_block(block: &SseBlock) -> Result<Option<ChatEvent>> {
    let json = || -> Result<Value> {
        serde_json::from_str(&block.data).map_err(|e| Error::Decode(format!("sse data: {e}")))
    };
    match block.event.as_str() {
        "route" => {
            let v = json()?;
            Ok(Some(ChatEvent::Route {
                intent: v["intent"].as_str().unwrap_or("").to_owned(),
                confidence: v["confidence"].as_f64().filter(|c| c.is_finite()),
                jev_ms: v["jev_ms"]
                    .as_f64()
                    .filter(|m| m.is_finite())
                    .map(|m| m.max(0.0) as u64),
                fast: v["fast"].as_bool() == Some(true),
                source: v["source"].as_str().unwrap_or("").to_owned(),
                layers: RouteLayer::parse_all(&v["layers"]),
            }))
        }
        "ack" => {
            let v = json()?;
            Ok(Some(ChatEvent::Ack {
                text: v["text"].as_str().unwrap_or("").to_owned(),
            }))
        }
        "delta" => {
            let v = json()?;
            Ok(v["text"]
                .as_str()
                .filter(|t| !t.is_empty())
                .map(|t| ChatEvent::Delta(t.to_owned())))
        }
        "reply" => {
            let v = json()?;
            Ok(Some(ChatEvent::Reply {
                text: v["text"].as_str().unwrap_or("").to_owned(),
                sensitive: v["sensitive"].as_bool() == Some(true),
                replaces: v["replaces"].as_bool() == Some(true),
            }))
        }
        "done" => Ok(Some(ChatEvent::Done(json().unwrap_or(Value::Null)))),
        "error" => {
            let v = json().unwrap_or(Value::Null);
            Err(Error::Api {
                status: 502,
                code: v["code"].as_str().unwrap_or("lucina_error").to_owned(),
                message: v["message"].as_str().map(str::to_owned),
            })
        }
        _ => Ok(None),
    }
}

enum Source {
    Sse(reqwest::Response),
    /// Nothing more to read from the network.
    Closed,
}

/// The events of one chat turn, read with [`ChatStream::next`].
pub struct ChatStream {
    source: Source,
    parser: SseParser,
    queue: VecDeque<ChatEvent>,
    /// A `reply` or `done` was seen: ending the stream is then fine.
    settled: bool,
}

impl ChatStream {
    pub(crate) fn sse(resp: reqwest::Response) -> Self {
        Self {
            source: Source::Sse(resp),
            parser: SseParser::default(),
            queue: VecDeque::new(),
            settled: false,
        }
    }

    /// An ordinary JSON answer (`data` of the envelope) as a two-event stream.
    pub(crate) fn json(data: Value) -> Self {
        let text = data["reply"].as_str().unwrap_or("").to_owned();
        let sensitive = data["sensitive"].as_bool() == Some(true);
        Self {
            source: Source::Closed,
            parser: SseParser::default(),
            queue: VecDeque::from([
                ChatEvent::Reply {
                    text,
                    sensitive,
                    replaces: false,
                },
                ChatEvent::Done(data),
            ]),
            settled: true,
        }
    }

    /// The next event; `Ok(None)` at the end. An `error` event, a broken or
    /// stalled connection, and an end before any `reply` are `Err`. Safe to
    /// drop mid-await (no event is lost: state only changes when a read
    /// completes).
    pub async fn next(&mut self) -> Result<Option<ChatEvent>> {
        loop {
            if let Some(ev) = self.queue.pop_front() {
                if matches!(ev, ChatEvent::Reply { .. } | ChatEvent::Done(_)) {
                    self.settled = true;
                }
                return Ok(Some(ev));
            }
            let Source::Sse(resp) = &mut self.source else {
                return Ok(None);
            };
            match resp.chunk().await.map_err(Error::from_reqwest)? {
                Some(bytes) => {
                    for block in self.parser.push(&bytes) {
                        if let Some(ev) = decode_block(&block)? {
                            self.queue.push_back(ev);
                        }
                    }
                }
                None => {
                    self.source = Source::Closed;
                    if !self.settled && self.queue.is_empty() {
                        return Err(Error::Unreachable(
                            "chat stream ended before the reply".into(),
                        ));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(chunks: &[&str]) -> Vec<SseBlock> {
        let mut p = SseParser::default();
        chunks.iter().flat_map(|c| p.push(c.as_bytes())).collect()
    }

    #[test]
    fn parses_blocks_split_anywhere() {
        let all = "event: delta\ndata: {\"text\":\"Ciao\"}\n\n: ping\n\nevent: reply\ndata: {\"text\":\"Ciao.\"}\n\n";
        let whole = blocks(&[all]);
        assert_eq!(whole.len(), 2);
        assert_eq!(whole[0].event, "delta");
        assert_eq!(whole[0].data, "{\"text\":\"Ciao\"}");
        assert_eq!(whole[1].event, "reply");
        // one byte at a time gives the same blocks
        let bytewise: Vec<&str> = all
            .char_indices()
            .map(|(i, c)| &all[i..i + c.len_utf8()])
            .collect();
        assert_eq!(blocks(&bytewise), whole);
    }

    #[test]
    fn handles_crlf_multiline_data_and_defaults() {
        let b = blocks(&["data: a\r\ndata: b\r\n\r\nevent: done\r\ndata: {}\r\n\r\n"]);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0].event, "message");
        assert_eq!(b[0].data, "a\nb");
        assert_eq!(b[1].event, "done");
        // CR alone, and a CRLF split between chunks
        let b = blocks(&[
            "event: x\rdata: 1\r\r",
            "event: y\r",
            "\ndata: 2\r",
            "\n\r\n",
        ]);
        assert_eq!(b.len(), 2);
        assert_eq!((b[0].event.as_str(), b[0].data.as_str()), ("x", "1"));
        assert_eq!((b[1].event.as_str(), b[1].data.as_str()), ("y", "2"));
        // an incomplete block is held back
        assert!(blocks(&["event: delta\ndata: {}\n"]).is_empty());
    }

    #[test]
    fn decodes_the_events() {
        let b = |event: &str, data: &str| SseBlock {
            event: event.into(),
            data: data.into(),
        };
        assert_eq!(
            decode_block(&b("delta", r#"{"text":"Ciao "}"#)).unwrap(),
            Some(ChatEvent::Delta("Ciao ".into()))
        );
        assert_eq!(decode_block(&b("delta", r#"{"text":""}"#)).unwrap(), None);
        assert_eq!(
            decode_block(&b(
                "reply",
                r#"{"text":"Ok.","sensitive":true,"replaces":true}"#
            ))
            .unwrap(),
            Some(ChatEvent::Reply {
                text: "Ok.".into(),
                sensitive: true,
                replaces: true
            })
        );
        assert_eq!(
            decode_block(&b("reply", r#"{"text":"Ok."}"#)).unwrap(),
            Some(ChatEvent::Reply {
                text: "Ok.".into(),
                sensitive: false,
                replaces: false
            })
        );
        assert!(matches!(
            decode_block(&b("done", r#"{"reply":"x"}"#)).unwrap(),
            Some(ChatEvent::Done(_))
        ));
        assert!(matches!(
            decode_block(&b("error", r#"{"code":"llm_failed","message":"boom"}"#)),
            Err(Error::Api { status: 502, code, message: Some(m) }) if code == "llm_failed" && m == "boom"
        ));
        assert_eq!(decode_block(&b("other", "x")).unwrap(), None);
        // route and ack are first-class; missing fields default
        assert_eq!(
            decode_block(&b(
                "route",
                r#"{"intent":"temporal_query","confidence":0.93,"jev_ms":212,"fast":true,"source":"jev"}"#
            ))
            .unwrap(),
            Some(ChatEvent::Route {
                intent: "temporal_query".into(),
                confidence: Some(0.93),
                jev_ms: Some(212),
                fast: true,
                source: "jev".into(),
                layers: Vec::new()
            })
        );
        assert_eq!(
            decode_block(&b("route", "{}")).unwrap(),
            Some(ChatEvent::Route {
                intent: String::new(),
                confidence: None,
                jev_ms: None,
                fast: false,
                source: String::new(),
                layers: Vec::new()
            })
        );
        // layers ride on the route event, cleaned
        let Some(ChatEvent::Route { layers, .. }) = decode_block(&b(
            "route",
            r#"{"intent":"chat","layers":[{"layer":"intent","verdict":"chat","confidence":1.7,"ms":212,"used":true},{"verdict":"x"},{"layer":"vo<c>ab","verdict":7,"ms":-5}]}"#,
        ))
        .unwrap() else {
            panic!("not a route")
        };
        assert_eq!(
            layers,
            vec![
                RouteLayer {
                    layer: "intent".into(),
                    verdict: "chat".into(),
                    confidence: Some(1.0),
                    ms: Some(212),
                    used: true
                },
                RouteLayer {
                    layer: "vocab".into(),
                    verdict: "7".into(),
                    confidence: None,
                    ms: Some(0),
                    used: false
                },
            ]
        );
        assert_eq!(
            decode_block(&b("ack", r#"{"text":"Un attimo…"}"#)).unwrap(),
            Some(ChatEvent::Ack {
                text: "Un attimo…".into()
            })
        );
        assert!(decode_block(&b("delta", "not json")).is_err());
    }

    #[test]
    fn done_layers_come_from_voice_layers_in_either_envelope() {
        let layers = serde_json::json!([{"layer": "vocab", "verdict": "none", "confidence": 0.5, "ms": 80, "used": false}]);
        let sse = serde_json::json!({"ok": true, "data": {"reply": "x", "voice": {"layers": layers}}});
        let plain = serde_json::json!({"reply": "x", "voice": {"layers": layers}});
        let want = vec![RouteLayer {
            layer: "vocab".into(),
            verdict: "none".into(),
            confidence: Some(0.5),
            ms: Some(80),
            used: false,
        }];
        assert_eq!(RouteLayer::from_done(&sse), want);
        assert_eq!(RouteLayer::from_done(&plain), want);
        assert!(RouteLayer::from_done(&serde_json::json!({"reply": "x"})).is_empty());
        assert!(RouteLayer::parse_all(&serde_json::json!("nope")).is_empty());
        // at most MAX_LAYERS
        let many: Vec<_> = (0..20).map(|i| serde_json::json!({"layer": format!("l{i}")})).collect();
        assert_eq!(RouteLayer::parse_all(&serde_json::Value::Array(many)).len(), MAX_LAYERS);
    }

    #[test]
    fn a_json_answer_is_one_reply() {
        let s = ChatStream::json(
            serde_json::json!({"reply": "Sto bene.", "sensitive": true, "action": "reply"}),
        );
        assert!(s.settled && matches!(s.source, Source::Closed));
        assert_eq!(s.queue.len(), 2);
        assert_eq!(
            s.queue[0],
            ChatEvent::Reply {
                text: "Sto bene.".into(),
                sensitive: true,
                replaces: false
            }
        );
        assert!(matches!(s.queue[1], ChatEvent::Done(_)));
    }
}
