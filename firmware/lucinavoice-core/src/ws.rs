//! A WebSocket client (RFC 6455) on top of any byte stream: the upgrade
//! request, its `101` check, masked client frames and a streaming parser for
//! the server's unmasked frames.
//!
//! The parser hands payload bytes out as they arrive ([`Event::Data`]), so a
//! large audio frame never needs a buffer of its own size. Fragmented
//! messages are reported per frame (`fin` tells the last one); control
//! frames (ping/pong/close) are small and kept whole up to 125 bytes.

use core::fmt::Write as _;

use crate::http::SliceWriter;

pub const OP_CONT: u8 = 0x0;
pub const OP_TEXT: u8 = 0x1;
pub const OP_BINARY: u8 = 0x2;
pub const OP_CLOSE: u8 = 0x8;
pub const OP_PING: u8 = 0x9;
pub const OP_PONG: u8 = 0xA;

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// The upgrade request for `path` with a 16-byte random `nonce`. `None` if
/// `out` is too small.
pub fn upgrade_request<'a>(
    out: &'a mut [u8],
    path: &str,
    token: &str,
    nonce: &[u8; 16],
) -> Option<&'a [u8]> {
    let mut key = [0u8; 24];
    base64(nonce, &mut key);
    let key = core::str::from_utf8(&key).ok()?;
    let mut w = SliceWriter::new(out);
    let _ = write!(
        w,
        "GET {path} HTTP/1.1\r\nHost: {}\r\nUser-Agent: LucinaVoice/{}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\nAuthorization: Bearer {token}\r\n\r\n",
        crate::HOST,
        env!("CARGO_PKG_VERSION"),
    );
    if w.overflowed {
        return None;
    }
    let n = w.bytes().len();
    Some(&out[..n])
}

/// The `Sec-WebSocket-Accept` value the server must send for `nonce`.
pub fn expected_accept(nonce: &[u8; 16]) -> [u8; 28] {
    let mut key = [0u8; 24];
    base64(nonce, &mut key);
    let mut h = sha1_smol::Sha1::new();
    h.update(&key);
    h.update(GUID.as_bytes());
    let mut out = [0u8; 28];
    base64(&h.digest().bytes(), &mut out);
    out
}

/// Standard base64 with padding; `out` must be `4 * ceil(len / 3)` long.
pub fn base64(input: &[u8], out: &mut [u8]) {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for (i, c) in input.chunks(3).enumerate() {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        let o = &mut out[i * 4..i * 4 + 4];
        o[0] = T[(n >> 18) as usize & 63];
        o[1] = T[(n >> 12) as usize & 63];
        o[2] = if c.len() > 1 {
            T[(n >> 6) as usize & 63]
        } else {
            b'='
        };
        o[3] = if c.len() > 2 {
            T[n as usize & 63]
        } else {
            b'='
        };
    }
}

/// The header of one client frame (FIN set, masked with `mask`). Returns its
/// length in `out`. The payload must then be sent through [`apply_mask`].
pub fn frame_header(opcode: u8, len: usize, mask: [u8; 4], out: &mut [u8; 14]) -> usize {
    out[0] = 0x80 | (opcode & 0x0F);
    let n = if len < 126 {
        out[1] = 0x80 | len as u8;
        2
    } else if len <= 0xFFFF {
        out[1] = 0x80 | 126;
        out[2..4].copy_from_slice(&(len as u16).to_be_bytes());
        4
    } else {
        out[1] = 0x80 | 127;
        out[2..10].copy_from_slice(&(len as u64).to_be_bytes());
        10
    };
    out[n..n + 4].copy_from_slice(&mask);
    n + 4
}

/// XORs `data` with `mask`, starting at payload offset `offset`.
pub fn apply_mask(data: &mut [u8], mask: [u8; 4], offset: usize) {
    for (i, b) in data.iter_mut().enumerate() {
        *b ^= mask[(offset + i) % 4];
    }
}

/// What the server sent, as the parser sees it.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// A data frame starts: its opcode (0 for a continuation) and length.
    Start { opcode: u8, len: u64, fin: bool },
    /// Payload bytes of the current data frame: `input[.0 .. .1]`.
    Data(usize, usize),
    /// The current data frame is complete.
    End { fin: bool },
    /// A whole control frame (payload at most 125 bytes, in the parser).
    Control { opcode: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Head,
    Payload { left: u64 },
    Control { left: usize },
}

/// Streaming parser for server frames.
#[derive(Debug)]
pub struct Parser {
    state: State,
    head: [u8; 10],
    head_len: usize,
    opcode: u8,
    fin: bool,
    control: [u8; 125],
    control_len: usize,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: State::Head,
            head: [0; 10],
            head_len: 0,
            opcode: 0,
            fin: false,
            control: [0; 125],
            control_len: 0,
        }
    }

    /// True when [`Self::step`] has an event due even without input (the end
    /// of a data frame whose payload is complete).
    pub fn wants_step(&self) -> bool {
        matches!(self.state, State::Payload { left: 0 })
    }

    /// The payload of the last [`Event::Control`].
    pub fn control_payload(&self) -> &[u8] {
        &self.control[..self.control_len]
    }

    /// Consumes bytes from the front of `input`; returns how many and at
    /// most one event. Call again with the rest until it is empty.
    pub fn step(&mut self, input: &[u8]) -> Result<(usize, Option<Event>), Malformed> {
        match self.state {
            State::Head => {
                let mut used = 0;
                for &b in input {
                    self.head[self.head_len] = b;
                    self.head_len += 1;
                    used += 1;
                    let ev = self.try_head()?;
                    if ev.is_some() || self.state != State::Head {
                        return Ok((used, ev));
                    }
                }
                Ok((used, None))
            }
            State::Payload { left } => {
                if left == 0 {
                    self.state = State::Head;
                    return Ok((0, Some(Event::End { fin: self.fin })));
                }
                let take = (input.len() as u64).min(left) as usize;
                self.state = State::Payload {
                    left: left - take as u64,
                };
                Ok((take, (take > 0).then_some(Event::Data(0, take))))
            }
            State::Control { left } => {
                let take = input.len().min(left);
                self.control[self.control_len..self.control_len + take]
                    .copy_from_slice(&input[..take]);
                self.control_len += take;
                if take == left {
                    self.state = State::Head;
                    return Ok((
                        take,
                        Some(Event::Control {
                            opcode: self.opcode,
                        }),
                    ));
                }
                self.state = State::Control { left: left - take };
                Ok((take, None))
            }
        }
    }

    fn try_head(&mut self) -> Result<Option<Event>, Malformed> {
        let h = &self.head[..self.head_len];
        if h.len() < 2 {
            return Ok(None);
        }
        if h[1] & 0x80 != 0 || h[0] & 0x70 != 0 {
            // Servers never mask; no extensions were negotiated.
            return Err(Malformed);
        }
        let need = match h[1] & 0x7F {
            126 => 4,
            127 => 10,
            _ => 2,
        };
        if h.len() < need {
            return Ok(None);
        }
        let len = match need {
            4 => u64::from(u16::from_be_bytes([h[2], h[3]])),
            10 => u64::from_be_bytes(h[2..10].try_into().map_err(|_| Malformed)?),
            _ => u64::from(h[1] & 0x7F),
        };
        let fin = h[0] & 0x80 != 0;
        let opcode = h[0] & 0x0F;
        self.head_len = 0;
        if opcode >= 0x8 {
            if !fin || len > 125 {
                return Err(Malformed);
            }
            self.opcode = opcode;
            self.control_len = 0;
            if len == 0 {
                return Ok(Some(Event::Control { opcode }));
            }
            self.state = State::Control { left: len as usize };
            return Ok(None);
        }
        if !matches!(opcode, OP_CONT | OP_TEXT | OP_BINARY) {
            return Err(Malformed);
        }
        self.fin = fin;
        self.state = State::Payload { left: len };
        Ok(Some(Event::Start { opcode, len, fin }))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    #[test]
    fn accept_matches_rfc_example() {
        // RFC 6455 §1.3: key "dGhlIHNhbXBsZSBub25jZQ==" is the nonce "the sample nonce".
        let nonce: [u8; 16] = *b"the sample nonce";
        let mut key = [0u8; 24];
        base64(&nonce, &mut key);
        assert_eq!(&key, b"dGhlIHNhbXBsZSBub25jZQ==");
        assert_eq!(&expected_accept(&nonce), b"s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn upgrade_request_has_the_headers() {
        let mut buf = [0u8; 400];
        let r = upgrade_request(&mut buf, "/v1/voice/ws", "sec_t", b"the sample nonce").unwrap();
        let r = core::str::from_utf8(r).unwrap();
        assert!(r.starts_with("GET /v1/voice/ws HTTP/1.1\r\nHost: getsecretary.tech\r\n"));
        assert!(r.contains("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"));
        assert!(r.contains("Authorization: Bearer sec_t\r\n"));
        assert!(r.ends_with("\r\n\r\n"));
    }

    fn client_frame(opcode: u8, payload: &[u8], mask: [u8; 4]) -> Vec<u8> {
        let mut h = [0u8; 14];
        let n = frame_header(opcode, payload.len(), mask, &mut h);
        let mut out = h[..n].to_vec();
        let mut p = payload.to_vec();
        apply_mask(&mut p, mask, 0);
        out.extend(p);
        out
    }

    #[test]
    fn masks_client_frames() {
        // RFC 6455 §5.7: masked "Hello".
        let f = client_frame(OP_TEXT, b"Hello", [0x37, 0xfa, 0x21, 0x3d]);
        assert_eq!(
            f,
            [
                0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58
            ]
        );
        let f = client_frame(OP_BINARY, &[0u8; 300], [1, 2, 3, 4]);
        assert_eq!(&f[..4], &[0x82, 0xFE, 0x01, 0x2C]);
        let mut split = *b"abcdef";
        apply_mask(&mut split[..3], [1, 2, 3, 4], 0);
        apply_mask(&mut split[3..], [1, 2, 3, 4], 3);
        let mut whole = *b"abcdef";
        apply_mask(&mut whole, [1, 2, 3, 4], 0);
        assert_eq!(split, whole);
    }

    fn server_frame(first: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = std::vec![first];
        match payload.len() {
            n if n < 126 => out.push(n as u8),
            n if n <= 0xFFFF => {
                out.push(126);
                out.extend((n as u16).to_be_bytes());
            }
            n => {
                out.push(127);
                out.extend((n as u64).to_be_bytes());
            }
        }
        out.extend(payload);
        out
    }

    /// Feeds `raw` in pieces of `piece`; collects (kind, bytes) per message.
    fn parse(raw: &[u8], piece: usize) -> Result<Vec<(u8, Vec<u8>)>, Malformed> {
        let mut p = Parser::new();
        let mut out: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut cur: Option<(u8, Vec<u8>)> = None;
        for part in raw.chunks(piece) {
            let mut rest = part;
            loop {
                let (used, ev) = p.step(rest)?;
                match ev {
                    Some(Event::Start { opcode, .. }) => {
                        if opcode != OP_CONT {
                            cur = Some((opcode, Vec::new()));
                        }
                    }
                    Some(Event::Data(a, b)) => {
                        cur.as_mut().unwrap().1.extend_from_slice(&rest[a..b])
                    }
                    Some(Event::End { fin: true }) => out.push(cur.take().unwrap()),
                    Some(Event::End { fin: false }) => {}
                    Some(Event::Control { opcode }) => {
                        out.push((opcode, p.control_payload().to_vec()))
                    }
                    None => {}
                }
                rest = &rest[used..];
                if rest.is_empty() && !p.wants_step() {
                    break;
                }
            }
        }
        Ok(out)
    }

    #[test]
    fn parses_frames_in_any_pieces() {
        let mut raw = server_frame(0x81, br#"{"type":"listening"}"#);
        raw.extend(server_frame(0x89, b"hi"));
        raw.extend(server_frame(0x82, &[7u8; 300]));
        raw.extend(server_frame(0x02, b"ab")); // binary, not final
        raw.extend(server_frame(0x80, b"cd")); // continuation, final
        raw.extend(server_frame(0x82, &[1u8; 70_000]));
        raw.extend(server_frame(0x88, &[0x03, 0xE8]));
        for piece in [1, 2, 3, 7, 1000, raw.len()] {
            let msgs = parse(&raw, piece).unwrap();
            assert_eq!(msgs.len(), 6, "piece {piece}");
            assert_eq!(msgs[0], (OP_TEXT, br#"{"type":"listening"}"#.to_vec()));
            assert_eq!(msgs[1], (OP_PING, b"hi".to_vec()));
            assert_eq!(msgs[2], (OP_BINARY, std::vec![7u8; 300]));
            assert_eq!(msgs[3], (OP_BINARY, b"abcd".to_vec()));
            assert_eq!(msgs[4].1.len(), 70_000);
            assert_eq!(msgs[5], (OP_CLOSE, std::vec![0x03, 0xE8]));
        }
    }

    #[test]
    fn empty_frames_end_at_once() {
        let mut raw = server_frame(0x82, b"");
        raw.extend(server_frame(0x8A, b""));
        let msgs = parse(&raw, 1).unwrap();
        assert_eq!(
            msgs,
            std::vec![(OP_BINARY, Vec::new()), (OP_PONG, Vec::new())]
        );
    }

    #[test]
    fn rejects_masked_or_odd_frames() {
        assert!(parse(&[0x81, 0x85, 0, 0, 0, 0], 8).is_err());
        assert!(parse(&[0x83, 0x00], 8).is_err());
        assert!(parse(&[0x09, 0x00], 8).is_err()); // fragmented ping
        assert!(parse(&[0xC1, 0x00], 8).is_err()); // RSV bit
        let mut big_ping = std::vec![0x89, 126, 0, 200];
        big_ping.extend([0u8; 200]);
        assert!(parse(&big_ping, 300).is_err());
    }
}
