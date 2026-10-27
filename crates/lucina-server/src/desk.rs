//! LucinaDesk: the owner's RustDesk view of Lucina's desktop (`lucinadesk.v1`).
//!
//! The wire contract is `docs/CONTRACT.md` in the lucinadesk repo. The browser never
//! speaks RustDesk and never holds the RustDesk password: `lucinadesk-gateway` (its own
//! process, AGPL, that repo) does the rendezvous, the relay, the crypto and the login, and
//! speaks the contract's JSON text frames and binary video frames on a Unix socket
//! (`LUCINA_DESK_GATEWAY`, default [`DEFAULT_GATEWAY`]). This module
//! stands between the owner's socket and the gateway:
//!
//! - `POST /api/secretary/computer/desk` `{sandbox, mode, device, codecs?}` (bearer;
//!   secretary-server names the device; `codecs` is what the browser decodes, best first,
//!   passed to the gateway): a single-use ticket, 32 random bytes in base64url, good for
//!   [`TICKET_TTL`] to connect, at most [`MAX_UNREDEEMED`] waiting per device.
//! - `GET /api/secretary/computer/desk/ws` (bearer; p1 splices the owner's socket here) and
//!   `GET /v1/computer/desk/ws` on the home-network door (`lan.rs`; no bearer, the page
//!   origin only): the socket. The ticket rides as the `ticket.<t>` subprotocol and the
//!   server selects `lucinadesk.v1`. A missing, unknown, used or expired ticket is 410.
//! - **Mode.** RustDesk's channel is encrypted from end to end, so a watch-only session
//!   cannot be enforced by a byte proxy. Here the gateway holds the session key and this
//!   module sees the plain wire: a watch socket's `mouse`/`key`/`text` never reach the
//!   gateway (`refused` once per kind, then dropped). Only `ack`, `refresh` and `ping` pass.
//!   Clipboard (contract section 8) is control-only both ways: the gateway hears `mode=control`
//!   for control sockets only, a watch socket's `clipboard` is refused, and a `clipboard` frame
//!   from the gateway never reaches a watch socket.
//! - **Lease.** A control socket takes the owner's takeover lease when it opens (Lucina's
//!   task stops, agents get 409 `owner_driving`), renews it every [`RENEW_EVERY`], and gives
//!   it back when the last control socket of the sandbox closes, unless the owner already
//!   held it before (a Cua takeover keeps its lease). When the lease ends some other way
//!   (give-back in another tab, expiry) the socket closes with 4001.
//! - **Audit** (`computer-audit.jsonl`, through `control()`): `desk_open {device, mode, via}`,
//!   `desk_closed {device, mode, via, why, code, openMs, mouse, keys, textChars, clipboardIn,
//!   clipboardOut, bytesIn, bytesOut}`, the lease's own `take`/`release`, and `input {batch: {via: "desk", …}}`
//!   every [`INPUT_LOG_EVERY`] while there is input. Typed and clipboard text is never logged,
//!   only its size (`clipboardIn`/`clipboardOut` are UTF-8 bytes).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{header, HeaderMap, Method};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use lucina_computer_use::{control, SandboxId, ViewMode};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message as GwMessage;

use crate::api::{ok, read_json, ApiError};
use crate::AppState;

/// The subprotocol the server selects.
pub const PROTOCOL: &str = "lucinadesk.v1";
/// How long a ticket may wait to be redeemed.
pub const TICKET_TTL: Duration = Duration::from_secs(30);
/// Unredeemed tickets per device.
const MAX_UNREDEEMED: usize = 4;
const RENEW_EVERY: Duration = Duration::from_secs(10);
const CHECK_EVERY: Duration = Duration::from_secs(2);
const INPUT_LOG_EVERY: Duration = Duration::from_secs(5);
/// Largest client message forwarded; larger ones are refused (`too_large`).
const MAX_MESSAGE: usize = 64 * 1024;
/// Larger than this the socket itself fails (tungstenite's limit).
const MAX_FRAME: usize = 256 * 1024;
const MAX_INPUT_PER_SEC: u32 = 200;
/// Seconds in a row over [`MAX_INPUT_PER_SEC`] before the socket closes with 4008.
const FLOOD_SECS: u32 = 5;
const MAX_TEXT_CHARS: usize = 1024;
/// Largest clipboard text either way (UTF-8 bytes).
const MAX_CLIPBOARD: usize = 60_000;
const MINT_BODY_LIMIT: usize = 4096;

/// Close codes (contract section 6).
pub mod code {
    pub const NORMAL: u16 = 1000;
    pub const SESSION_ENDED: u16 = 4000;
    pub const CONTROL_ENDED: u16 = 4001;
    pub const GATEWAY_UNAVAILABLE: u16 = 4003;
    pub const VIOLATION: u16 = 4008;
}

// ---------------------------------------------------------------- tickets

#[derive(Debug, Clone)]
struct Ticket {
    sandbox: SandboxId,
    mode: ViewMode,
    device: String,
    /// `vp9,av1`: the gateway's `codecs` query; empty = its default.
    codecs: String,
    expires: Instant,
}

/// Codecs the gateway knows.
const CODECS: [&str; 5] = ["vp8", "vp9", "av1", "h264", "h265"];

/// The browser's list, known codecs only, in its order; anything malformed is ignored.
fn codecs(v: &Value) -> String {
    let mut out: Vec<&str> = Vec::new();
    for c in v.get("codecs").and_then(Value::as_array).into_iter().flatten() {
        if let Some(c) = c.as_str().and_then(|c| CODECS.iter().find(|k| **k == c)) {
            if !out.contains(c) {
                out.push(c);
            }
        }
    }
    out.join(",")
}

fn tickets() -> &'static Mutex<HashMap<String, Ticket>> {
    static T: OnceLock<Mutex<HashMap<String, Ticket>>> = OnceLock::new();
    T.get_or_init(|| Mutex::new(HashMap::new()))
}

fn valid_ticket(t: &str) -> bool {
    t.len() == 43
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A new ticket for `device`, or 429 when it already has [`MAX_UNREDEEMED`] waiting.
fn mint(sandbox: SandboxId, mode: ViewMode, device: &str, codecs: &str) -> Result<String, ApiError> {
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).map_err(|_| ApiError::internal())?;
    let ticket = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
    let mut map = tickets().lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    map.retain(|_, t| t.expires > now);
    if map.values().filter(|t| t.device == device).count() >= MAX_UNREDEEMED {
        return Err(ApiError::new(
            429,
            "too_many_tickets",
            "too many desk tickets waiting; connect or wait 30 s",
        ));
    }
    map.insert(
        ticket.clone(),
        Ticket {
            sandbox,
            mode,
            device: device.to_owned(),
            codecs: codecs.to_owned(),
            expires: now + TICKET_TTL,
        },
    );
    Ok(ticket)
}

/// Takes the ticket out (single use); `None` when unknown or expired.
fn redeem(ticket: &str) -> Option<Ticket> {
    if !valid_ticket(ticket) {
        return None;
    }
    let mut map = tickets().lock().unwrap_or_else(|e| e.into_inner());
    map.remove(ticket).filter(|t| t.expires > Instant::now())
}

/// `(lucinadesk.v1 offered, the ticket)` from `Sec-WebSocket-Protocol`.
fn offered(headers: &HeaderMap) -> (bool, Option<String>) {
    let mut v1 = false;
    let mut ticket = None;
    for value in headers.get_all(header::SEC_WEBSOCKET_PROTOCOL) {
        for p in value.to_str().unwrap_or("").split(',').map(str::trim) {
            if p == PROTOCOL {
                v1 = true;
            } else if let Some(t) = p.strip_prefix("ticket.") {
                ticket = Some(t.to_owned());
            }
        }
    }
    (v1, ticket)
}

/// A device label for the audit log: printable, at most 60 chars.
fn label(v: &Value) -> String {
    let d: String = v
        .get("device")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(60)
        .collect();
    match d.trim() {
        "" => "owner".into(),
        d => d.to_owned(),
    }
}

// ---------------------------------------------------------------- the gateway

/// The gateway's Unix socket (0600, same user; `LUCINADESK_SOCKET` on its side).
pub const DEFAULT_GATEWAY: &str = "/home/nez/Projects/Lucina/data/lucinadesk/gateway.sock";

/// The gateway's Unix socket.
pub fn gateway_path() -> PathBuf {
    std::env::var_os("LUCINA_DESK_GATEWAY")
        .filter(|p| !p.is_empty())
        .map_or_else(|| PathBuf::from(DEFAULT_GATEWAY), PathBuf::from)
}

type Gateway = tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>;

async fn connect_gateway(sb: SandboxId, codecs: &str, mode: ViewMode) -> Result<Gateway, String> {
    let path = gateway_path();
    let stream = tokio::time::timeout(Duration::from_secs(5), tokio::net::UnixStream::connect(&path))
        .await
        .map_err(|_| "gateway connect timed out".to_owned())?
        .map_err(|e| format!("gateway {}: {e}", path.display()))?;
    let mut url = format!("ws://lucinadesk-gateway/v1/session?sandbox={}", sb.as_str());
    if !codecs.is_empty() {
        url.push_str("&codecs=");
        url.push_str(codecs);
    }
    // The gateway enables clipboard (both ways) on control sessions only; anything else is watch.
    if mode == ViewMode::Control {
        url.push_str("&mode=control");
    }
    let (ws, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::client_async(url, stream),
    )
    .await
    .map_err(|_| "gateway handshake timed out".to_owned())?
    .map_err(|e| format!("gateway handshake: {e}"))?;
    Ok(ws)
}

// ---------------------------------------------------------------- the lease

/// Open desk control sockets per sandbox, and whether they took the lease themselves.
fn holders() -> &'static Mutex<HashMap<SandboxId, (usize, bool)>> {
    static H: OnceLock<Mutex<HashMap<SandboxId, (usize, bool)>>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(HashMap::new()))
}

/// One control socket's share of the owner's lease; gives it back on drop when it is
/// the last one and the desk took it.
struct ControlLease {
    sb: SandboxId,
    device: String,
    epoch: u64,
}

impl ControlLease {
    fn take(sb: SandboxId, device: &str) -> Self {
        let mut h = holders().lock().unwrap_or_else(|e| e.into_inner());
        let held_before = control().owner_driving(sb);
        let entry = h.entry(sb).or_insert((0, false));
        if entry.0 == 0 {
            entry.1 = !held_before;
        } else if !held_before {
            entry.1 = true;
        }
        entry.0 += 1;
        let t = control().take(sb, device);
        Self {
            sb,
            device: device.to_owned(),
            epoch: t.epoch,
        }
    }

    /// Still the owner's lease from this socket's takeover.
    fn alive(&self) -> bool {
        control().owner_driving(self.sb) && control().epoch(self.sb) == self.epoch
    }
}

impl Drop for ControlLease {
    fn drop(&mut self) {
        let mut h = holders().lock().unwrap_or_else(|e| e.into_inner());
        let Some(entry) = h.get_mut(&self.sb) else {
            return;
        };
        entry.0 = entry.0.saturating_sub(1);
        if entry.0 == 0 {
            let took = entry.1;
            h.remove(&self.sb);
            if took && self.alive() {
                control().release(self.sb, &self.device);
                crate::computer_view::drop_control_views(self.sb);
            }
        }
    }
}

// ---------------------------------------------------------------- the input filter

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Counts {
    mouse: u64,
    keys: u64,
    text_chars: u64,
    /// Clipboard text the owner pushed to the host (bytes).
    clipboard_in: u64,
}

#[derive(Debug, PartialEq)]
enum Verdict {
    Forward,
    /// Dropped; the `refused` frame to send, if this kind was not refused before.
    Drop(Option<String>),
    /// Sustained flooding: close with 4008.
    Violation,
}

/// What the client may send, per contract section 5.
struct Filter {
    mode: ViewMode,
    refused: HashSet<(&'static str, &'static str)>,
    window: Instant,
    in_window: u32,
    over_secs: u32,
    counts: Counts,
    /// Host clipboard text sent to the owner (bytes).
    clipboard_out: u64,
}

fn int_in(v: &Value, k: &str, lo: i64, hi: i64) -> bool {
    v.get(k)
        .and_then(Value::as_i64)
        .is_some_and(|n| (lo..=hi).contains(&n))
}

fn control_key_name(s: &str) -> bool {
    (1..=24).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Shape check of an input message.
fn valid_input(kind: &str, v: &Value) -> bool {
    match kind {
        "mouse" => {
            // mask = (buttons << 3) | kind, kind 0..=3, buttons 1|2|4.
            int_in(v, "mask", 0, 63)
                && v["mask"].as_i64().is_some_and(|m| m & 7 <= 3)
                && int_in(v, "x", -100_000, 100_000)
                && int_in(v, "y", -100_000, 100_000)
        }
        "text" => v
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|t| !t.is_empty() && t.chars().count() <= MAX_TEXT_CHARS),
        "clipboard" => v
            .get("text")
            .and_then(Value::as_str)
            .is_some_and(|t| t.len() <= MAX_CLIPBOARD),
        "key" => {
            let key = v.get("key").and_then(Value::as_str);
            let ch = v.get("char").and_then(Value::as_str);
            let one = match (key, ch) {
                (Some(k), None) => control_key_name(k),
                (None, Some(c)) => c.chars().count() == 1,
                _ => false,
            };
            let mods = match v.get("modifiers") {
                None | Some(Value::Null) => true,
                Some(Value::Array(a)) => {
                    a.len() <= 4
                        && a.iter().all(|m| {
                            matches!(m.as_str(), Some("Control" | "Shift" | "Alt" | "Meta"))
                        })
                }
                _ => false,
            };
            one && mods
        }
        _ => false,
    }
}

impl Filter {
    fn new(mode: ViewMode) -> Self {
        Self {
            mode,
            refused: HashSet::new(),
            window: Instant::now(),
            in_window: 0,
            over_secs: 0,
            counts: Counts::default(),
            clipboard_out: 0,
        }
    }

    fn refuse(&mut self, what: &'static str, why: &'static str) -> Verdict {
        if self.refused.insert((what, why)) {
            Verdict::Drop(Some(
                json!({"type": "refused", "what": what, "why": why}).to_string(),
            ))
        } else {
            Verdict::Drop(None)
        }
    }

    /// One input message against the rate: `false` when over it.
    fn rate_ok(&mut self, now: Instant) -> Result<bool, ()> {
        let elapsed = now.duration_since(self.window);
        if elapsed >= Duration::from_secs(1) {
            if self.in_window > MAX_INPUT_PER_SEC {
                // Every whole second without input breaks the run.
                self.over_secs = if elapsed < Duration::from_secs(2) { self.over_secs + 1 } else { 1 };
            } else {
                self.over_secs = 0;
            }
            self.window = now;
            self.in_window = 0;
            if self.over_secs >= FLOOD_SECS {
                return Err(());
            }
        }
        self.in_window += 1;
        Ok(self.in_window <= MAX_INPUT_PER_SEC)
    }

    /// A text frame from the gateway: `false` drops it. Host clipboard frames reach control
    /// sockets only (the gateway sends them to control sessions only; this is the second layer).
    fn host_frame(&mut self, text: &str) -> bool {
        if !text.contains("clipboard") {
            return true;
        }
        let Ok(v) = serde_json::from_str::<Value>(text) else {
            return true;
        };
        if v.get("type").and_then(Value::as_str) != Some("clipboard") {
            return true;
        }
        if self.mode != ViewMode::Control {
            return false;
        }
        self.clipboard_out += v.get("text").and_then(Value::as_str).map_or(0, str::len) as u64;
        true
    }

    fn text(&mut self, text: &str, now: Instant) -> Verdict {
        let v: Option<Value> = (text.len() <= MAX_MESSAGE)
            .then(|| serde_json::from_str(text).ok())
            .flatten();
        let kind = v
            .as_ref()
            .and_then(|v| v.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let what: &'static str = match kind {
            "ack" | "refresh" | "ping" if v.is_some() => return Verdict::Forward,
            "mouse" => "mouse",
            "key" => "key",
            "text" => "text",
            "clipboard" => "clipboard",
            _ => "unknown",
        };
        if text.len() > MAX_MESSAGE {
            return self.refuse(what, "too_large");
        }
        let Some(v) = v.filter(|_| what != "unknown") else {
            return self.refuse("unknown", "invalid");
        };
        if self.mode == ViewMode::Watch {
            return self.refuse(what, "watch");
        }
        match self.rate_ok(now) {
            Err(()) => return Verdict::Violation,
            // Over the rate: the contract has no own reason for it, the gateway says too_large.
            Ok(false) => return self.refuse(what, "too_large"),
            Ok(true) => {}
        }
        if !valid_input(what, &v) {
            return self.refuse(what, "invalid");
        }
        match what {
            "mouse" => self.counts.mouse += 1,
            "key" => self.counts.keys += 1,
            "clipboard" => self.counts.clipboard_in += v["text"].as_str().map_or(0, str::len) as u64,
            _ => {
                self.counts.text_chars += v["text"].as_str().map_or(0, |t| t.chars().count()) as u64
            }
        }
        Verdict::Forward
    }
}

// ---------------------------------------------------------------- routes

/// `POST /api/secretary/computer/desk` (bearer).
pub async fn mint_route(State(st): State<AppState>, req: Request) -> Response {
    if !crate::api::bearer_ok(&st, req.headers()) {
        return ApiError::new(401, "unauthorized", "").into_response();
    }
    if req.method() != Method::POST {
        return ApiError::method_not_allowed(&["POST"]).into_response();
    }
    match mint_body(read_json(req.into_body(), MINT_BODY_LIMIT).await).await {
        Ok(r) => r,
        Err(e) => e.into_response(),
    }
}

async fn mint_body(v: Result<Value, ApiError>) -> Result<Response, ApiError> {
    let v = v?;
    let sandbox = v.get("sandbox").and_then(Value::as_str).unwrap_or("workspace");
    let sb = SandboxId::parse(sandbox)
        .ok_or_else(|| ApiError::bad_request("sandbox must be workspace"))?;
    if sb != SandboxId::Workspace {
        return Err(ApiError::new(
            400,
            "desk_unsupported",
            "LucinaDesk runs on the workspace only",
        ));
    }
    let mode = ViewMode::parse(v.get("mode").and_then(Value::as_str).unwrap_or("watch"))
        .ok_or_else(|| ApiError::bad_request("mode must be watch or control"))?;
    if !gateway_path().exists() {
        return Err(ApiError::new(
            503,
            "desk_unavailable",
            "the LucinaDesk gateway is not running",
        ));
    }
    let ticket = mint(sb, mode, &label(&v), &codecs(&v))?;
    Ok(ok(json!({
        "ticket": ticket,
        "protocol": PROTOCOL,
        "expiresInSecs": TICKET_TTL.as_secs(),
        "sandbox": sb.as_str(),
        "mode": mode.as_str(),
    })))
}

/// `GET /api/secretary/computer/desk/ws` (bearer; secretary-server splices the owner's socket).
pub async fn ws_route(State(st): State<AppState>, req: Request) -> Response {
    if !crate::api::bearer_ok(&st, req.headers()) {
        return ApiError::new(401, "unauthorized", "").into_response();
    }
    upgrade(req, "p1").await
}

/// The socket for a caller already let in (bearer from p1, or the home-network door):
/// the ticket is the only capability.
pub async fn upgrade(req: Request, via: &'static str) -> Response {
    let (mut parts, _) = req.into_parts();
    let (v1, ticket) = offered(&parts.headers);
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    // Redeemed only once the request is a valid upgrade: a stray GET never burns a ticket.
    let Some(t) = ticket.filter(|_| v1).and_then(|t| redeem(&t)) else {
        tracing::warn!(via, "desk socket refused: no valid ticket");
        return ApiError::new(410, "ticket_refused", "mint a new desk ticket").into_response();
    };
    upgrade
        .protocols([PROTOCOL])
        .max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| session(socket, t, via))
}

// ---------------------------------------------------------------- one session

#[derive(Debug, Default)]
struct Bytes {
    inbound: u64,
    outbound: u64,
}

fn closed_frame(reason: &str) -> Message {
    Message::Text(json!({"type": "closed", "reason": reason}).to_string().into())
}

async fn session(mut browser: WebSocket, t: Ticket, via: &'static str) {
    let opened = Instant::now();
    control().note(
        t.sandbox,
        "desk_open",
        json!({"device": t.device, "mode": t.mode.as_str(), "via": via}),
    );
    let mut filter = Filter::new(t.mode);
    let mut bytes = Bytes::default();
    let (code, why) = run(&mut browser, &t, &mut filter, &mut bytes).await;
    let _ = browser
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: why.as_str().into(),
        })))
        .await;
    control().note(
        t.sandbox,
        "desk_closed",
        json!({
            "device": t.device,
            "mode": t.mode.as_str(),
            "via": via,
            "why": why,
            "code": code,
            "openMs": opened.elapsed().as_millis() as u64,
            "mouse": filter.counts.mouse,
            "keys": filter.counts.keys,
            "textChars": filter.counts.text_chars,
            "clipboardIn": filter.counts.clipboard_in,
            "clipboardOut": filter.clipboard_out,
            "bytesIn": bytes.inbound,
            "bytesOut": bytes.outbound,
        }),
    );
}

/// Pumps until one side ends; returns the close code and reason for the owner's socket.
async fn run(browser: &mut WebSocket, t: &Ticket, filter: &mut Filter, bytes: &mut Bytes) -> (u16, String) {
    let hello = json!({"type": "hello", "v": 1, "mode": t.mode.as_str(), "sandbox": t.sandbox.as_str()});
    if browser.send(Message::Text(hello.to_string().into())).await.is_err() {
        return (code::NORMAL, "client gone".into());
    }
    let mut gw = match connect_gateway(t.sandbox, &t.codecs, t.mode).await {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(error = %e, "desk: gateway unavailable");
            let _ = browser.send(closed_frame("LucinaDesk is unavailable")).await;
            return (code::GATEWAY_UNAVAILABLE, "gateway unavailable".into());
        }
    };
    let lease = (t.mode == ViewMode::Control).then(|| ControlLease::take(t.sandbox, &t.device));
    let end = pump(browser, &mut gw, t, lease.as_ref(), filter, bytes).await;
    let _ = gw.close(None).await;
    drop(lease);
    end
}

async fn pump(
    browser: &mut WebSocket,
    gw: &mut Gateway,
    t: &Ticket,
    lease: Option<&ControlLease>,
    filter: &mut Filter,
    bytes: &mut Bytes,
) -> (u16, String) {
    let mut tick = tokio::time::interval(CHECK_EVERY);
    let mut last_renew = Instant::now();
    let mut last_log = Instant::now();
    let mut logged = Counts::default();
    loop {
        tokio::select! {
            m = browser.recv() => {
                let m = match m {
                    Some(Ok(m)) => m,
                    _ => return (code::NORMAL, "client closed".into()),
                };
                let fwd = match m {
                    Message::Text(text) => {
                        bytes.inbound += text.len() as u64;
                        match filter.text(text.as_str(), Instant::now()) {
                            Verdict::Forward => GwMessage::text(text.as_str()),
                            Verdict::Drop(Some(refused)) => {
                                bytes.outbound += refused.len() as u64;
                                if browser.send(Message::Text(refused.into())).await.is_err() {
                                    return (code::NORMAL, "client gone".into());
                                }
                                continue;
                            }
                            Verdict::Drop(None) => continue,
                            Verdict::Violation => {
                                let _ = browser.send(closed_frame("too much input")).await;
                                return (code::VIOLATION, "input flood".into());
                            }
                        }
                    }
                    Message::Binary(b) => {
                        bytes.inbound += b.len() as u64;
                        if let Verdict::Drop(Some(refused)) = filter.refuse("unknown", "invalid") {
                            let _ = browser.send(Message::Text(refused.into())).await;
                        }
                        continue;
                    }
                    Message::Close(_) => return (code::NORMAL, "client closed".into()),
                    Message::Ping(_) | Message::Pong(_) => continue,
                };
                if gw.send(fwd).await.is_err() {
                    let _ = browser.send(closed_frame("the desktop session ended")).await;
                    return (code::SESSION_ENDED, "gateway closed".into());
                }
            }
            m = gw.next() => {
                let out = match m {
                    Some(Ok(GwMessage::Text(s))) => {
                        if !filter.host_frame(s.as_str()) {
                            continue;
                        }
                        Message::Text(s.as_str().into())
                    }
                    Some(Ok(GwMessage::Binary(b))) => Message::Binary(b),
                    Some(Ok(GwMessage::Close(frame))) => {
                        let (c, r) = frame
                            .map(|f| (u16::from(f.code), f.reason.as_str().to_owned()))
                            .unwrap_or((code::SESSION_ENDED, String::new()));
                        // The gateway ends RustDesk sessions with 4000 (or 1000); never let
                        // it speak for the lease or the server.
                        let c = if c == code::NORMAL { code::SESSION_ENDED } else { c };
                        let c = if (4000..=4999).contains(&c) && c != code::CONTROL_ENDED { c } else { code::SESSION_ENDED };
                        return (c, if r.is_empty() { "session ended".into() } else { r });
                    }
                    Some(Ok(_)) => continue,
                    _ => {
                        let _ = browser.send(closed_frame("the desktop session ended")).await;
                        return (code::SESSION_ENDED, "gateway closed".into());
                    }
                };
                bytes.outbound += match &out {
                    Message::Text(s) => s.len(),
                    Message::Binary(b) => b.len(),
                    _ => 0,
                } as u64;
                if browser.send(out).await.is_err() {
                    return (code::NORMAL, "client gone".into());
                }
            }
            _ = tick.tick() => {
                let Some(lease) = lease else { continue };
                if !lease.alive() {
                    let _ = browser.send(closed_frame("control ended")).await;
                    return (code::CONTROL_ENDED, "control ended".into());
                }
                if last_renew.elapsed() >= RENEW_EVERY {
                    if control().heartbeat(t.sandbox).is_err() {
                        let _ = browser.send(closed_frame("control ended")).await;
                        return (code::CONTROL_ENDED, "control ended".into());
                    }
                    last_renew = Instant::now();
                }
                if last_log.elapsed() >= INPUT_LOG_EVERY {
                    let c = filter.counts;
                    if c != logged {
                        let batch = json!({
                            "via": "desk",
                            "mouse": c.mouse - logged.mouse,
                            "keys": c.keys - logged.keys,
                            "textChars": c.text_chars - logged.text_chars,
                            "clipboardIn": c.clipboard_in - logged.clipboard_in,
                        });
                        if control().owner_input(t.sandbox, &t.device, batch).is_err() {
                            let _ = browser.send(closed_frame("control ended")).await;
                            return (code::CONTROL_ENDED, "control ended".into());
                        }
                        logged = c;
                    }
                    last_log = Instant::now();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tickets_are_single_use_bounded_and_opaque() {
        let dev = "tickets-test";
        let t = mint(SandboxId::Workspace, ViewMode::Watch, dev, "").unwrap();
        assert!(valid_ticket(&t), "{t}");
        let got = redeem(&t).expect("redeemed once");
        assert_eq!((got.sandbox, got.mode, got.device.as_str()), (SandboxId::Workspace, ViewMode::Watch, dev));
        assert!(redeem(&t).is_none(), "single use");
        assert!(redeem("nope").is_none());
        let waiting: Vec<_> = (0..MAX_UNREDEEMED)
            .map(|_| mint(SandboxId::Workspace, ViewMode::Control, dev, "").unwrap())
            .collect();
        let e = mint(SandboxId::Workspace, ViewMode::Control, dev, "").unwrap_err();
        assert_eq!((e.status.as_u16(), e.code.as_str()), (429, "too_many_tickets"));
        assert!(mint(SandboxId::Workspace, ViewMode::Control, "another-device", "").is_ok());
        assert!(redeem(&waiting[0]).is_some());
        assert!(mint(SandboxId::Workspace, ViewMode::Control, dev, "").is_ok(), "one slot free again");
    }

    #[test]
    fn codecs_keep_the_browsers_order_and_drop_the_rest() {
        assert_eq!(codecs(&json!({"codecs": ["vp9", "av1", "h264"]})), "vp9,av1,h264");
        assert_eq!(codecs(&json!({"codecs": ["h264", "vp9", "h264", "x;rm", 7, "VP9"]})), "h264,vp9");
        assert_eq!(codecs(&json!({"codecs": "vp9"})), "", "not a list: ignored");
        assert_eq!(codecs(&json!({})), "");
    }

    #[test]
    fn expired_tickets_are_refused() {
        let t = mint(SandboxId::Workspace, ViewMode::Watch, "expiry-test", "").unwrap();
        tickets().lock().unwrap().get_mut(&t).unwrap().expires = Instant::now();
        assert!(redeem(&t).is_none());
    }

    #[test]
    fn the_ticket_rides_as_a_subprotocol() {
        let mut h = HeaderMap::new();
        h.insert(header::SEC_WEBSOCKET_PROTOCOL, "lucinadesk.v1, ticket.abc".parse().unwrap());
        assert_eq!(offered(&h), (true, Some("abc".into())));
        let mut h = HeaderMap::new();
        h.insert(header::SEC_WEBSOCKET_PROTOCOL, "ticket.abc".parse().unwrap());
        assert_eq!(offered(&h), (false, Some("abc".into())));
        assert_eq!(offered(&HeaderMap::new()), (false, None));
    }

    #[test]
    fn watch_sockets_never_forward_input() {
        let now = Instant::now();
        let mut f = Filter::new(ViewMode::Watch);
        assert_eq!(f.text(r#"{"type":"ack"}"#, now), Verdict::Forward);
        assert_eq!(f.text(r#"{"type":"refresh"}"#, now), Verdict::Forward);
        assert_eq!(f.text(r#"{"type":"ping","t":1}"#, now), Verdict::Forward);
        let mouse = r#"{"type":"mouse","mask":9,"x":10,"y":20}"#;
        match f.text(mouse, now) {
            Verdict::Drop(Some(r)) => {
                let r: Value = serde_json::from_str(&r).unwrap();
                assert_eq!(r, json!({"type": "refused", "what": "mouse", "why": "watch"}));
            }
            v => panic!("{v:?}"),
        }
        assert_eq!(f.text(mouse, now), Verdict::Drop(None), "refused once per kind");
        assert!(matches!(f.text(r#"{"type":"text","text":"hi"}"#, now), Verdict::Drop(Some(_))));
        assert!(matches!(f.text(r#"{"type":"key","key":"Return"}"#, now), Verdict::Drop(Some(_))));
        match f.text(r#"{"type":"clipboard","text":"secret"}"#, now) {
            Verdict::Drop(Some(r)) => assert_eq!(
                serde_json::from_str::<Value>(&r).unwrap(),
                json!({"type": "refused", "what": "clipboard", "why": "watch"})
            ),
            v => panic!("{v:?}"),
        }
        assert!(!f.host_frame(r#"{"type":"clipboard","text":"from the host"}"#), "never to a watch socket");
        assert!(f.host_frame(r#"{"type":"login","hostname":"clipboard-box"}"#), "other frames pass");
        assert_eq!(f.counts, Counts::default(), "nothing counted as input");
        assert_eq!(f.clipboard_out, 0);
    }

    #[test]
    fn control_sockets_forward_valid_input_and_count_it() {
        let now = Instant::now();
        let mut f = Filter::new(ViewMode::Control);
        assert_eq!(f.text(r#"{"type":"mouse","mask":9,"x":10,"y":20}"#, now), Verdict::Forward);
        assert_eq!(f.text(r#"{"type":"text","text":"ciao"}"#, now), Verdict::Forward);
        assert_eq!(f.text(r#"{"type":"key","char":"l","modifiers":["Control"]}"#, now), Verdict::Forward);
        assert_eq!(f.text(r#"{"type":"key","key":"Return"}"#, now), Verdict::Forward);
        assert_eq!(f.text(r#"{"type":"clipboard","text":"héllo"}"#, now), Verdict::Forward);
        assert_eq!(f.counts, Counts { mouse: 1, keys: 2, text_chars: 4, clipboard_in: 6 });
        assert!(f.host_frame(r#"{"type":"clipboard","text":"ciao"}"#));
        assert_eq!(f.clipboard_out, 4, "bytes, not the text");
        let big = json!({"type": "clipboard", "text": "x".repeat(MAX_CLIPBOARD + 1)}).to_string();
        assert!(matches!(f.text(&big, now), Verdict::Drop(_)), "over 60,000 bytes");
        for bad in [
            r#"{"type":"mouse","mask":7,"x":1,"y":1}"#,
            r#"{"type":"mouse","mask":9,"x":"1","y":1}"#,
            r#"{"type":"key","key":"Return","char":"x"}"#,
            r#"{"type":"key","char":"ab"}"#,
            r#"{"type":"key","key":"Return","modifiers":["Hyper"]}"#,
            r#"{"type":"text","text":""}"#,
        ] {
            assert!(matches!(f.text(bad, now), Verdict::Drop(_)), "{bad}");
        }
        let long = json!({"type": "text", "text": "x".repeat(MAX_TEXT_CHARS + 1)}).to_string();
        assert!(matches!(f.text(&long, now), Verdict::Drop(_)));
        let huge = json!({"type": "text", "text": "x".repeat(MAX_MESSAGE)}).to_string();
        match Filter::new(ViewMode::Control).text(&huge, now) {
            Verdict::Drop(Some(r)) => assert!(r.contains("too_large"), "{r}"),
            v => panic!("{v:?}"),
        }
        assert!(matches!(f.text("not json", now), Verdict::Drop(_)));
        assert!(matches!(f.text(r#"{"type":"files"}"#, now), Verdict::Drop(_)));
    }

    #[test]
    fn floods_are_rate_limited_then_closed() {
        let mut f = Filter::new(ViewMode::Control);
        let t0 = Instant::now();
        let m = r#"{"type":"mouse","mask":0,"x":1,"y":1}"#;
        for _ in 0..MAX_INPUT_PER_SEC {
            assert_eq!(f.text(m, t0), Verdict::Forward);
        }
        assert!(matches!(f.text(m, t0), Verdict::Drop(Some(r)) if r.contains("too_large")));
        let mut verdict = Verdict::Forward;
        'outer: for s in 1..=FLOOD_SECS as u64 {
            let at = t0 + Duration::from_millis(s * 1000 + 1);
            for _ in 0..=MAX_INPUT_PER_SEC + 1 {
                verdict = f.text(m, at);
                if verdict == Verdict::Violation {
                    break 'outer;
                }
            }
        }
        assert_eq!(verdict, Verdict::Violation);
        // A calm second resets the run.
        let mut g = Filter::new(ViewMode::Control);
        for _ in 0..=MAX_INPUT_PER_SEC + 1 {
            g.text(m, t0);
        }
        assert_eq!(g.text(m, t0 + Duration::from_millis(1500)), Verdict::Forward);
        assert_eq!(g.over_secs, 1);
        assert_eq!(g.text(m, t0 + Duration::from_millis(2600)), Verdict::Forward);
        assert_eq!(g.over_secs, 0);
    }
}
