//! Just enough HTTP/1.1 for three requests: request heads, a streaming
//! response-head parser, and chunked transfer coding in both directions.
//!
//! Responses are read with [`HeadParser`] (bytes in, until the blank line;
//! lines longer than its buffer are cut, which only shortens the logged
//! transcript), then the body with [`ChunkedDecoder`] when
//! `Transfer-Encoding: chunked`, else up to `Content-Length` or the close.

use core::fmt::Write as _;

use crate::settings::Field;

/// How a request body is framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    None,
    Length(usize),
    Chunked,
}

/// A `core::fmt::Write` into a byte slice; `overflowed` once it is full.
pub struct SliceWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
    pub overflowed: bool,
}

impl<'a> SliceWriter<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            len: 0,
            overflowed: false,
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl core::fmt::Write for SliceWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.len + s.len();
        if end > self.buf.len() {
            self.overflowed = true;
            return Err(core::fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

/// Writes a request head (`Connection: close`, one request per connection).
/// Returns `None` if `out` is too small.
pub fn request_head<'a>(
    out: &'a mut [u8],
    method: &str,
    path: &str,
    token: Option<&str>,
    content_type: Option<&str>,
    body: Body,
) -> Option<&'a [u8]> {
    let mut w = SliceWriter::new(out);
    let _ = write!(
        w,
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nUser-Agent: LucinaVoice/{}\r\n",
        crate::HOST,
        env!("CARGO_PKG_VERSION")
    );
    let _ = write!(w, "Accept: */*\r\nConnection: close\r\n");
    if let Some(t) = token {
        let _ = write!(w, "Authorization: Bearer {t}\r\n");
    }
    if let Some(ct) = content_type {
        let _ = write!(w, "Content-Type: {ct}\r\n");
    }
    let _ = match body {
        Body::None => Ok(()),
        Body::Length(n) => write!(w, "Content-Length: {n}\r\n"),
        Body::Chunked => write!(w, "Transfer-Encoding: chunked\r\n"),
    };
    let _ = write!(w, "\r\n");
    if w.overflowed {
        return None;
    }
    let len = w.len;
    Some(&w.buf[..len])
}

/// The size line of one outgoing chunk (`<hex>\r\n`) in `out`.
pub fn chunk_size_line(len: usize, out: &mut [u8; 18]) -> &[u8] {
    let mut w = SliceWriter::new(out);
    let _ = write!(w, "{len:x}\r\n");
    let n = w.len;
    &out[..n]
}

/// The last chunk and the end of the body.
pub const LAST_CHUNK: &[u8] = b"0\r\n\r\n";

/// The response violated HTTP/1.1 framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed;

/// Response head, parsed as bytes arrive.
#[derive(Debug)]
pub struct HeadParser {
    line: [u8; 512],
    line_len: usize,
    saw_status: bool,
    pub done: bool,
    pub status: u16,
    pub content_length: Option<usize>,
    pub chunked: bool,
    /// `X-Lucina-Transcript`, percent-decoded, cut to fit.
    pub transcript: Field<200>,
    /// `X-Lucina-Reply`, percent-decoded, cut to fit.
    pub reply: Field<200>,
    /// `Content-Type`, cut to fit.
    pub content_type: Field<64>,
}

impl Default for HeadParser {
    fn default() -> Self {
        Self::new()
    }
}

impl HeadParser {
    pub fn new() -> Self {
        Self {
            line: [0; 512],
            line_len: 0,
            saw_status: false,
            done: false,
            status: 0,
            content_length: None,
            chunked: false,
            transcript: Field::default(),
            reply: Field::default(),
            content_type: Field::default(),
        }
    }

    /// Feeds response bytes; returns how many belong to the head. Once
    /// `done`, the rest of `data` (and everything after) is the body.
    /// `Err` for a malformed status line.
    pub fn feed(&mut self, data: &[u8]) -> Result<usize, Malformed> {
        for (i, &b) in data.iter().enumerate() {
            if self.done {
                return Ok(i);
            }
            if b == b'\n' {
                let mut len = self.line_len;
                if len > 0 && self.line[len - 1] == b'\r' {
                    len -= 1;
                }
                self.line_len = 0;
                self.end_line(len)?;
            } else if self.line_len < self.line.len() {
                self.line[self.line_len] = b;
                self.line_len += 1;
            }
        }
        Ok(data.len())
    }

    fn end_line(&mut self, len: usize) -> Result<(), Malformed> {
        let line = &self.line[..len];
        if !self.saw_status {
            self.saw_status = true;
            // "HTTP/1.1 200 OK"
            let mut parts = line.split(|&b| b == b' ');
            let version = parts.next().ok_or(Malformed)?;
            if !version.starts_with(b"HTTP/1.") {
                return Err(Malformed);
            }
            let code = parts.next().ok_or(Malformed)?;
            self.status = core::str::from_utf8(code)
                .ok()
                .and_then(|c| c.parse().ok())
                .ok_or(Malformed)?;
            return Ok(());
        }
        if line.is_empty() {
            self.done = true;
            return Ok(());
        }
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            return Ok(());
        };
        let name = &line[..colon];
        let value = trim(&line[colon + 1..]);
        if name.eq_ignore_ascii_case(b"content-length") {
            self.content_length = core::str::from_utf8(value)
                .ok()
                .and_then(|v| v.parse().ok());
        } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
            self.chunked = value.eq_ignore_ascii_case(b"chunked");
        } else if name.eq_ignore_ascii_case(b"content-type") {
            set_prefix(&mut self.content_type, value);
        } else if name.eq_ignore_ascii_case(b"x-lucina-transcript") {
            percent_decode_into(value, &mut self.transcript);
        } else if name.eq_ignore_ascii_case(b"x-lucina-reply") {
            percent_decode_into(value, &mut self.reply);
        }
        Ok(())
    }
}

fn trim(mut v: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = v {
        v = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = v {
        v = rest;
    }
    v
}

/// The longest valid UTF-8 prefix of `bytes` that fits `N`.
fn set_prefix<const N: usize>(field: &mut Field<N>, bytes: &[u8]) {
    let mut end = bytes.len().min(N);
    loop {
        if let Ok(s) = core::str::from_utf8(&bytes[..end]) {
            field.set(s);
            return;
        }
        end -= 1;
    }
}

/// Percent-decodes `value` into `field`, cutting at `N` bytes on a UTF-8
/// boundary. A bad escape is kept literally.
pub fn percent_decode_into<const N: usize>(value: &[u8], field: &mut Field<N>) {
    let mut buf = [0u8; N];
    let mut n = 0;
    let mut i = 0;
    while i < value.len() && n < N {
        let b = value[i];
        let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        if b == b'%'
            && i + 2 < value.len()
            && let (Some(h), Some(l)) = (hex(value[i + 1]), hex(value[i + 2]))
        {
            buf[n] = h << 4 | l;
            n += 1;
            i += 3;
            continue;
        }
        buf[n] = b;
        n += 1;
        i += 1;
    }
    set_prefix(field, &buf[..n]);
}

/// Decoder for `Transfer-Encoding: chunked` bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkedDecoder {
    /// Reading the hex size; `Some` once a digit was seen.
    Size(Option<usize>),
    /// Skipping a chunk extension up to the end of the size line.
    Extension(usize),
    Data(usize),
    DataCr,
    DataLf,
    /// After the last chunk (trailers are not read).
    Done,
}

impl Default for ChunkedDecoder {
    fn default() -> Self {
        ChunkedDecoder::Size(None)
    }
}

/// One step of [`ChunkedDecoder::step`].
#[derive(Debug, PartialEq, Eq)]
pub struct Step {
    /// Input bytes used by this step.
    pub consumed: usize,
    /// Payload bytes of `input[data.0..data.1]` (possibly empty).
    pub data: (usize, usize),
}

impl ChunkedDecoder {
    pub fn is_done(&self) -> bool {
        *self == ChunkedDecoder::Done
    }

    /// Consumes framing from the front of `input` and returns the payload
    /// that follows, if any. Call again with the unconsumed rest until it
    /// is empty. `Err` on malformed framing.
    pub fn step(&mut self, input: &[u8]) -> Result<Step, Malformed> {
        let mut i = 0;
        while i < input.len() {
            let b = input[i];
            match *self {
                ChunkedDecoder::Size(acc) => {
                    if let Some(d) = (b as char).to_digit(16) {
                        let v = acc
                            .unwrap_or(0)
                            .checked_mul(16)
                            .and_then(|v| v.checked_add(d as usize))
                            .ok_or(Malformed)?;
                        *self = ChunkedDecoder::Size(Some(v));
                    } else if b == b'\r' || b == b';' || b == b' ' {
                        *self = ChunkedDecoder::Extension(acc.ok_or(Malformed)?);
                    } else if b == b'\n' {
                        self.start_chunk(acc.ok_or(Malformed)?);
                    } else {
                        return Err(Malformed);
                    }
                    i += 1;
                }
                ChunkedDecoder::Extension(size) => {
                    if b == b'\n' {
                        self.start_chunk(size);
                    }
                    i += 1;
                }
                ChunkedDecoder::Data(remaining) => {
                    let take = remaining.min(input.len() - i);
                    *self = if take == remaining {
                        ChunkedDecoder::DataCr
                    } else {
                        ChunkedDecoder::Data(remaining - take)
                    };
                    return Ok(Step {
                        consumed: i + take,
                        data: (i, i + take),
                    });
                }
                ChunkedDecoder::DataCr => {
                    *self = match b {
                        b'\r' => ChunkedDecoder::DataLf,
                        b'\n' => ChunkedDecoder::Size(None),
                        _ => return Err(Malformed),
                    };
                    i += 1;
                }
                ChunkedDecoder::DataLf => {
                    if b != b'\n' {
                        return Err(Malformed);
                    }
                    *self = ChunkedDecoder::Size(None);
                    i += 1;
                }
                ChunkedDecoder::Done => {
                    return Ok(Step {
                        consumed: input.len(),
                        data: (0, 0),
                    });
                }
            }
        }
        Ok(Step {
            consumed: input.len(),
            data: (0, 0),
        })
    }

    fn start_chunk(&mut self, size: usize) {
        *self = if size == 0 {
            ChunkedDecoder::Done
        } else {
            ChunkedDecoder::Data(size)
        };
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    #[test]
    fn writes_request_heads() {
        let mut buf = [0u8; 400];
        let head = request_head(
            &mut buf,
            "POST",
            "/v1/voice",
            Some("sec_x"),
            Some("audio/x-s16le;rate=16000"),
            Body::Chunked,
        )
        .unwrap();
        let head = core::str::from_utf8(head).unwrap();
        assert!(head.starts_with("POST /v1/voice HTTP/1.1\r\nHost: getsecretary.tech\r\n"));
        assert!(head.contains("Authorization: Bearer sec_x\r\n"));
        assert!(head.contains("Transfer-Encoding: chunked\r\n"));
        assert!(head.ends_with("\r\n\r\n"));
        let head = request_head(&mut buf, "GET", "/x", None, None, Body::Length(3)).unwrap();
        assert!(
            core::str::from_utf8(head)
                .unwrap()
                .contains("Content-Length: 3\r\n")
        );
        assert!(request_head(&mut [0u8; 20], "GET", "/", None, None, Body::None).is_none());
    }

    #[test]
    fn chunk_size_lines_are_hex() {
        let mut b = [0u8; 18];
        assert_eq!(chunk_size_line(4096, &mut b), b"1000\r\n");
    }

    fn parse_all(raw: &[u8], piece: usize) -> (HeadParser, usize) {
        let mut p = HeadParser::new();
        let mut at = 0;
        while !p.done && at < raw.len() {
            let end = (at + piece).min(raw.len());
            at += p.feed(&raw[at..end]).unwrap();
        }
        (p, at)
    }

    #[test]
    fn parses_heads_in_any_pieces() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: audio/x-s16le;rate=24000\r\nTransfer-Encoding: chunked\r\nX-Lucina-Transcript: che%20ore%20sono%3F\r\nX-Lucina-Reply: Sono%20le%2010%20%E2%80%94%20ciao\r\n\r\nBODY";
        for piece in [1, 3, 7, raw.len()] {
            let (p, at) = parse_all(raw, piece);
            assert!(p.done);
            assert_eq!(&raw[at..], b"BODY");
            assert_eq!(p.status, 200);
            assert!(p.chunked);
            assert_eq!(p.content_type.as_str(), "audio/x-s16le;rate=24000");
            assert_eq!(p.transcript.as_str(), "che ore sono?");
            assert_eq!(p.reply.as_str(), "Sono le 10 — ciao");
        }
    }

    #[test]
    fn reads_content_length_and_rejects_garbage() {
        let (p, _) = parse_all(
            b"HTTP/1.1 422 Unprocessable\r\ncontent-length: 42\r\n\r\n",
            5,
        );
        assert_eq!(
            (p.status, p.content_length, p.chunked),
            (422, Some(42), false)
        );
        assert!(HeadParser::new().feed(b"SSH-2.0-x\r\n").is_err());
    }

    #[test]
    fn cuts_long_headers_on_a_char_boundary() {
        let mut f = Field::<5>::default();
        percent_decode_into(b"ab%E2%80%94cd", &mut f);
        assert_eq!(f.as_str(), "ab—");
        let mut f = Field::<4>::default();
        percent_decode_into(b"ab%E2%80%94cd", &mut f);
        assert_eq!(f.as_str(), "ab");
        let mut f = Field::<8>::default();
        percent_decode_into(b"100%zz%4", &mut f);
        assert_eq!(f.as_str(), "100%zz%4");
    }

    fn decode_all(raw: &[u8], piece: usize) -> Result<(Vec<u8>, bool), Malformed> {
        let mut d = ChunkedDecoder::default();
        let mut out = Vec::new();
        for part in raw.chunks(piece) {
            let mut rest = part;
            while !rest.is_empty() {
                let s = d.step(rest)?;
                out.extend_from_slice(&rest[s.data.0..s.data.1]);
                rest = &rest[s.consumed..];
            }
        }
        Ok((out, d.is_done()))
    }

    #[test]
    fn decodes_chunked_bodies_in_any_pieces() {
        let raw = b"5\r\nhello\r\nA;ext=1\r\n0123456789\r\n0\r\n\r\n";
        for piece in [1, 2, 4, 9, raw.len()] {
            let (out, done) = decode_all(raw, piece).unwrap();
            assert_eq!(out, b"hello0123456789");
            assert!(done);
        }
    }

    #[test]
    fn rejects_bad_framing() {
        assert!(decode_all(b"zz\r\n", 4).is_err());
        assert!(decode_all(b"2\r\nabX", 8).is_err());
        assert!(decode_all(b"\r\n", 8).is_err());
        assert!(decode_all(b"ffffffffffffffffff\r\n", 32).is_err());
    }
}
