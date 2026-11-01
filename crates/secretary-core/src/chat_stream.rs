//! The streaming chat answer: `POST /api/secretary/chat` with
//! `Accept: text/event-stream` (see [`crate::LucinaClient::chat_stream`]).
//!
//! Lucina answers with Server-Sent Events:
//!
//! | event | data | |
//! |---|---|---|
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
    fn decodes_the_four_events() {
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
        assert!(decode_block(&b("delta", "not json")).is_err());
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
