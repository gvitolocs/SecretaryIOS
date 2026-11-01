//! The persistent voice link: one WebSocket to `/v1/voice/ws`, opened once
//! Wi-Fi is up and kept open, so a turn starts without DNS, TCP or a TLS
//! handshake (`docs/lucinavoice-protocol.md`).
//!
//! - The writer half sends what the main loop queues in [`OUT`] (start,
//!   mic frames, end, cancel) and a ping every 30 s.
//! - The reader half plays binary frames (s16le, 24 kHz) through
//!   [`crate::player::PLAY`] as they arrive, and turns text frames into
//!   [`Event`]s for the main loop.
//! - Any error drops the connection; it is reopened after 1, 2, 4 … 30 s.

use alloc::vec;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::{Read, Write};
use esp_hal::rng::Trng;
use lucinavoice_core::json;
use lucinavoice_core::settings::{Field, MAX_TOKEN};
use lucinavoice_core::ws::{self, Event as WsEvent, Parser};

use crate::net::{Client, NetError};
use crate::player::{CHUNK, PLAY, Play};

/// Bytes per uplink mic frame (32 ms at 16 kHz).
pub const FRAME: usize = 1024;
const PATH: &str = "/v1/voice/ws";
const PING_EVERY: Duration = Duration::from_secs(30);
const MAX_BACKOFF: u64 = 30;

/// The link is open (the upgrade succeeded and nothing failed since).
pub static LINK_UP: AtomicBool = AtomicBool::new(false);
/// The server's VAD heard the end of speech: stop sending mic frames.
pub static ENDPOINT: AtomicBool = AtomicBool::new(false);

#[allow(
    clippy::large_enum_variant,
    reason = "fixed-size channel slots keep mic frames off the heap"
)]
pub enum Out {
    Start,
    Audio([u8; FRAME], usize),
    End,
    Cancel,
    Pong([u8; 125], usize),
}

pub static OUT: Channel<CriticalSectionRawMutex, Out, 6> = Channel::new();

pub enum Event {
    Up,
    Down,
    Listening,
    Transcript(Field<200>),
    AudioStart,
    Reply(Field<200>),
    /// The server's `ms` timings object, as JSON text.
    Done(Field<200>),
    Error(Field<40>),
}

pub static EVENTS: Channel<CriticalSectionRawMutex, Event, 6> = Channel::new();

#[derive(Debug)]
#[allow(dead_code, reason = "the fields are read through Debug in the logs")]
enum LinkError {
    Net(NetError),
    Upgrade(u16),
    BadAccept,
    Frame,
    Closed,
}

impl From<NetError> for LinkError {
    fn from(e: NetError) -> Self {
        LinkError::Net(e)
    }
}

impl From<mbedtls_rs::SessionError> for LinkError {
    fn from(e: mbedtls_rs::SessionError) -> Self {
        LinkError::Net(NetError::Tls(e))
    }
}

#[embassy_executor::task]
pub async fn link_task(client: Client, token: Field<MAX_TOKEN>) {
    let mut rx = vec![0u8; 16 * 1024];
    let mut tx = vec![0u8; 4 * 1024];
    let mut backoff = 1;
    loop {
        if !client.is_up() {
            Timer::after(Duration::from_secs(1)).await;
            continue;
        }
        let opened = Instant::now();
        let result = run(&client, token.as_str(), &mut rx, &mut tx).await;
        if LINK_UP.swap(false, Ordering::AcqRel) {
            let _ = EVENTS.try_send(Event::Down);
        }
        while OUT.try_receive().is_ok() {}
        match result {
            Ok(()) => log::info!("link: closed by the server"),
            Err(e) => log::warn!("link: {e:?}"),
        }
        if opened.elapsed() > Duration::from_secs(60) {
            backoff = 1;
        }
        Timer::after(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

async fn run(client: &Client, token: &str, rx: &mut [u8], tx: &mut [u8]) -> Result<(), LinkError> {
    let started = Instant::now();
    let mut session = client.connect(rx, tx).await?;
    let rng = Trng::try_new().map_err(|_| LinkError::Closed)?;
    let mut nonce = [0u8; 16];
    rng.read(&mut nonce);
    let mut head = [0u8; 512];
    let request = ws::upgrade_request(&mut head, PATH, token, &nonce)
        .ok_or(LinkError::Net(NetError::TooLong))?;
    session.write_all(request).await?;
    session.flush().await?;

    // The 101 head; bytes after it are already WebSocket frames.
    let mut buf = [0u8; 1024];
    let mut parser = lucinavoice_core::http::HeadParser::new();
    let (mut start, mut end) = (0, 0);
    while !parser.done {
        let n = session.read(&mut buf).await?;
        if n == 0 {
            return Err(LinkError::Closed);
        }
        let used = parser.feed(&buf[..n]).map_err(|_| LinkError::Upgrade(0))?;
        (start, end) = (used, n);
    }
    if parser.status != 101 {
        return Err(LinkError::Upgrade(parser.status));
    }
    if parser.ws_accept.as_str().as_bytes() != ws::expected_accept(&nonce) {
        return Err(LinkError::BadAccept);
    }
    log::info!("link: up in {} ms", started.elapsed().as_millis());
    LINK_UP.store(true, Ordering::Release);
    let _ = EVENTS.try_send(Event::Up);

    let mut early = [0u8; 1024];
    let early_len = end - start;
    early[..early_len].copy_from_slice(&buf[start..end]);
    let (mut r, mut w) = session.split().await?;
    match select(reader(&mut r, &early[..early_len]), writer(&mut w, &rng)).await {
        Either::First(res) | Either::Second(res) => res,
    }
}

/// Reads frames until the connection ends.
async fn reader<R: Read>(r: &mut R, early: &[u8]) -> Result<(), LinkError> {
    let mut rd = Reader::default();
    rd.feed(early).await?;
    let mut buf = [0u8; 1024];
    loop {
        let n = r.read(&mut buf).await.map_err(|_| LinkError::Closed)?;
        if n == 0 {
            return Err(LinkError::Closed);
        }
        rd.feed(&buf[..n]).await?;
    }
}

#[derive(Default)]
struct Reader {
    parser: Parser,
    opcode: u8,
    text: TextBuf,
    pcm: PcmBuf,
}

struct TextBuf {
    buf: [u8; 1024],
    len: usize,
}

impl Default for TextBuf {
    fn default() -> Self {
        Self {
            buf: [0; 1024],
            len: 0,
        }
    }
}

/// Audio on its way to the player, cut into even-length chunks.
pub struct PcmBuf {
    buf: [u8; CHUNK],
    len: usize,
}

impl Default for PcmBuf {
    fn default() -> Self {
        Self {
            buf: [0; CHUNK],
            len: 0,
        }
    }
}

impl PcmBuf {
    pub async fn push(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let take = data.len().min(CHUNK - self.len);
            self.buf[self.len..self.len + take].copy_from_slice(&data[..take]);
            self.len += take;
            data = &data[take..];
            if self.len == CHUNK {
                self.send().await;
            }
        }
    }

    /// Sends what is buffered, keeping an odd trailing byte for later.
    pub async fn send(&mut self) {
        let even = self.len & !1;
        if even == 0 {
            return;
        }
        let mut chunk = [0u8; CHUNK];
        chunk[..even].copy_from_slice(&self.buf[..even]);
        PLAY.send(Play::Pcm(chunk, even)).await;
        let odd = self.len - even;
        if odd == 1 {
            self.buf[0] = self.buf[even];
        }
        self.len = odd;
    }
}

impl Reader {
    async fn feed(&mut self, mut rest: &[u8]) -> Result<(), LinkError> {
        loop {
            if rest.is_empty() && !self.parser.wants_step() {
                return Ok(());
            }
            let (used, ev) = self.parser.step(rest).map_err(|_| LinkError::Frame)?;
            match ev {
                Some(WsEvent::Start { opcode, .. }) => {
                    if opcode != ws::OP_CONT {
                        self.opcode = opcode;
                        self.text.len = 0;
                    }
                }
                Some(WsEvent::Data(a, b)) => {
                    let data = &rest[a..b];
                    if self.opcode == ws::OP_TEXT {
                        let t = &mut self.text;
                        let take = data.len().min(t.buf.len() - t.len);
                        t.buf[t.len..t.len + take].copy_from_slice(&data[..take]);
                        t.len += take;
                    } else {
                        self.pcm.push(data).await;
                    }
                }
                Some(WsEvent::End { fin: true }) => {
                    if self.opcode == ws::OP_TEXT {
                        let len = self.text.len;
                        self.on_text(len).await;
                    } else {
                        self.pcm.send().await;
                    }
                }
                Some(WsEvent::End { fin: false }) | None => {}
                Some(WsEvent::Control { opcode }) => match opcode {
                    ws::OP_PING => {
                        let p = self.parser.control_payload();
                        let mut pong = [0u8; 125];
                        pong[..p.len()].copy_from_slice(p);
                        let _ = OUT.try_send(Out::Pong(pong, p.len()));
                    }
                    ws::OP_CLOSE => return Err(LinkError::Closed),
                    _ => {}
                },
            }
            rest = &rest[used..];
        }
    }

    async fn on_text(&mut self, len: usize) {
        let text = core::str::from_utf8(&self.text.buf[..len]).unwrap_or("");
        let field = |key: &str| -> Field<200> {
            let mut f = Field::default();
            f.set_prefix(json::str_field(text, key).unwrap_or("?"));
            f
        };
        let event = match json::str_field(text, "type") {
            Some("listening") => Event::Listening,
            Some("endpoint") => {
                ENDPOINT.store(true, Ordering::Release);
                return;
            }
            Some("transcript") => Event::Transcript(field("text")),
            Some("audio") => Event::AudioStart,
            Some("reply") => Event::Reply(field("text")),
            Some("done") => {
                self.pcm.send().await;
                PLAY.send(Play::Stop).await;
                let mut f = Field::default();
                f.set_prefix(text.find("\"ms\"").map_or("", |i| &text[i..]));
                Event::Done(f)
            }
            Some("error") => {
                self.pcm.len = 0;
                let mut f = Field::default();
                f.set_prefix(json::str_field(text, "code").unwrap_or("?"));
                Event::Error(f)
            }
            _ => return,
        };
        EVENTS.send(event).await;
    }
}

/// Sends queued frames and keep-alive pings until a write fails.
async fn writer<W: Write>(w: &mut W, rng: &Trng) -> Result<(), LinkError> {
    loop {
        let out = match select(OUT.receive(), Timer::after(PING_EVERY)).await {
            Either::First(out) => out,
            Either::Second(()) => {
                send(w, rng, ws::OP_PING, &mut []).await?;
                continue;
            }
        };
        match out {
            Out::Start => send_text(w, rng, r#"{"type":"start","mode":"ptt"}"#).await?,
            Out::End => send_text(w, rng, r#"{"type":"end"}"#).await?,
            Out::Cancel => send_text(w, rng, r#"{"type":"cancel"}"#).await?,
            Out::Audio(mut buf, n) => send(w, rng, ws::OP_BINARY, &mut buf[..n]).await?,
            Out::Pong(mut buf, n) => send(w, rng, ws::OP_PONG, &mut buf[..n]).await?,
        }
    }
}

async fn send_text<W: Write>(w: &mut W, rng: &Trng, text: &str) -> Result<(), LinkError> {
    let mut buf = [0u8; 64];
    buf[..text.len()].copy_from_slice(text.as_bytes());
    send(w, rng, ws::OP_TEXT, &mut buf[..text.len()]).await
}

async fn send<W: Write>(
    w: &mut W,
    rng: &Trng,
    opcode: u8,
    payload: &mut [u8],
) -> Result<(), LinkError> {
    let mask = rng.random().to_le_bytes();
    let mut head = [0u8; 14];
    let n = ws::frame_header(opcode, payload.len(), mask, &mut head);
    ws::apply_mask(payload, mask, 0);
    w.write_all(&head[..n])
        .await
        .map_err(|_| LinkError::Closed)?;
    w.write_all(payload).await.map_err(|_| LinkError::Closed)?;
    w.flush().await.map_err(|_| LinkError::Closed)?;
    Ok(())
}
