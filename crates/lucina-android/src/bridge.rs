//! The OTP bridge (`docs/lucina-android.md`, "Rust behaviour").
//!
//! - **Parse first:** a message without a code returns before any disk or
//!   network access.
//! - **Dedup:** the same code from any source within [`DEDUP_MS`] is sent
//!   once. Only keyed fingerprints are kept, never the code.
//! - **Forward:** one `POST /incoming`; 409 → paused, 401 → unpaired,
//!   410 → expired, 400 → dropped, network error or 5xx → queued.
//! - **Queue:** at most [`MAX_QUEUE`] codes in `queue.bin`, each kept at most
//!   [`CODE_TTL_MS`] after it was received.
//! - **State:** switches, token and the last result in `state.bin`.

use std::io;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use lucina_phone_core::otp::parse_otp;
use lucina_phone_core::proto::{
    clean_sender, valid_token, Incoming, OtpSource, CODE_TTL_MS, ORIGIN, PROTOCOL_VERSION,
};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::net::{elapsed_ms, Outcome, Transport};
use crate::store::{event_id, Secret, Vault};

/// Version of the app this library ships in.
pub const VERSION: &str = "1.0.0";
/// The app's own package; its notifications are ignored.
pub const OWN_PACKAGE: &str = "tech.getsecretary.lucinaotp";
/// At most this many codes wait in the queue; the oldest goes first.
pub const MAX_QUEUE: usize = 8;
/// The same code within this window is forwarded once.
pub const DEDUP_MS: u64 = 120_000;
/// Subdirectory of `filesDir` holding the encrypted files.
pub const DIR_NAME: &str = "lucina";
pub const STATE_FILE: &str = "state.bin";
pub const QUEUE_FILE: &str = "queue.bin";
/// Dedup fingerprints kept at most.
const MAX_RECENT: usize = 32;
/// The parser fixture (synthetic): this body must yield [`FIXTURE_CODE`] and
/// [`FIXTURE_NOT_OTP`] nothing.
const FIXTURE_SENDER: &str = "Lucina";
const FIXTURE_BODY: &str = "Your Lucina verification code is 482913. It expires in 2 minutes.";
const FIXTURE_CODE: &str = "482913";
const FIXTURE_NOT_OTP: &str = "Lucina: parcel 482913 left the depot at 12:30.";
/// Code carried by `test: true` events; the relay never stores it.
const TEST_CODE: &str = "000000";
const TEST_SENDER: &str = "Lucina self-test";

/// What happened to one SMS or notification (`"result"` in the JNI JSON).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Forward {
    /// Forwarding for this source is off.
    Disabled,
    /// No code in the message.
    NotOtp,
    /// The same code was handled within [`DEDUP_MS`].
    Duplicate,
    /// The relay accepted it.
    Sent,
    /// The relay was unreachable; it waits in `queue.bin`.
    Queued,
    /// The relay is paused (409); dropped.
    Paused,
    /// No token, or the relay refused it (401); dropped.
    Unpaired,
    /// Older than [`CODE_TTL_MS`], or the relay said so (410); dropped.
    Expired,
    /// The relay refused the body (400 or another 4xx), or a local failure.
    Error,
}

/// `onSms` / `onNotification` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Report {
    pub result: Forward,
    /// Time spent in the call.
    pub ms: u64,
}

/// `flush` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FlushReport {
    pub sent: u32,
    pub dropped: u32,
    pub pending: u32,
    /// Codes are still waiting: schedule another flush when the network is up.
    pub retry: bool,
}

/// `status` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub version: &'static str,
    pub protocol: u32,
    pub enabled: bool,
    pub notifications: bool,
    pub paired: bool,
    pub pending: u32,
    pub last_result: Option<Forward>,
    pub last_forward_at_ms: Option<u64>,
    /// Round trip of the last forwarded code, in ms.
    pub last_latency_ms: Option<u64>,
    /// The parser round-trips its fixture.
    pub self_test: bool,
}

/// Result of the network part of `selfTest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Probe {
    /// `network` was false.
    Skipped,
    /// The relay acknowledged the test event.
    Sent,
    Paused,
    Unpaired,
    Expired,
    /// Network error, timeout or 5xx.
    Offline,
    /// Anything else (bad response, a relay that does not know test events).
    Error,
}

/// `selfTest` result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SelfTest {
    pub parse: bool,
    pub result: Probe,
    pub ms: u64,
}

/// Why `configure` refused a patch. Nothing was changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigError {
    /// Not a JSON object with only the known fields.
    Json,
    /// `url` is not [`ORIGIN`].
    Url,
    /// `token` is not 64 lower-case hex characters.
    Token,
}

/// Switches, pairing and the last result (`state.bin`).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct State {
    enabled: bool,
    notifications: bool,
    token: Option<Secret>,
    last_result: Option<Forward>,
    last_forward_at_ms: Option<u64>,
    last_latency_ms: Option<u64>,
    /// `(fingerprint, handled at ms)` of recent codes.
    recent: Vec<(u64, u64)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            enabled: true,
            notifications: false,
            token: None,
            last_result: None,
            last_forward_at_ms: None,
            last_latency_ms: None,
            recent: Vec::new(),
        }
    }
}

/// A code waiting for the network (`queue.bin`).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pending {
    event_id: String,
    code: Secret,
    sender: String,
    received_at: u64,
    source: OtpSource,
}

impl Pending {
    fn fresh(&self, now: u64) -> bool {
        self.received_at.saturating_add(CODE_TTL_MS) > now
    }
}

/// `configure` patch.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    enabled: Option<bool>,
    notifications: Option<bool>,
    token: Option<Secret>,
    url: Option<String>,
}

/// The request body; the code copy is wiped when it is dropped.
struct WireEvent(Incoming);

impl Drop for WireEvent {
    fn drop(&mut self) {
        self.0.code.zeroize();
    }
}

/// Unix time in ms.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The parser round-trips its fixture.
pub fn parser_self_test() -> bool {
    parse_otp(FIXTURE_SENDER, FIXTURE_BODY).as_deref() == Some(FIXTURE_CODE)
        && parse_otp(FIXTURE_SENDER, FIXTURE_NOT_OTP).is_none()
}

/// The bridge: one per process, behind the JNI layer's lock.
pub struct Bridge {
    vault: Vault,
    transport: Transport,
    state: State,
    queue: Vec<Pending>,
    self_test: OnceLock<bool>,
}

impl Bridge {
    /// Opens `files_dir/lucina/` with the 32-byte `data_key`, importing
    /// `legacy_token` (the 0.4.0 pairing) when no token is stored yet and it
    /// is valid. Unreadable files (another key, tampering) start over empty.
    pub fn open(files_dir: &Path, data_key: &[u8], legacy_token: Option<&str>) -> io::Result<Self> {
        Self::open_with(files_dir, data_key, legacy_token, Transport::production())
    }

    pub(crate) fn open_with(
        files_dir: &Path,
        data_key: &[u8],
        legacy_token: Option<&str>,
        transport: Transport,
    ) -> io::Result<Self> {
        let vault = Vault::open(&files_dir.join(DIR_NAME), data_key)?;
        let state: State = match vault.read(STATE_FILE) {
            Ok(Some(plain)) => serde_json::from_slice(&plain).unwrap_or_default(),
            _ => State::default(),
        };
        let queue: Vec<Pending> = match vault.read(QUEUE_FILE) {
            Ok(Some(plain)) => serde_json::from_slice(&plain).unwrap_or_default(),
            _ => Vec::new(),
        };
        let mut bridge = Self {
            vault,
            transport,
            state,
            queue,
            self_test: OnceLock::new(),
        };
        if bridge.state.token.is_none() {
            if let Some(token) = legacy_token.filter(|t| valid_token(t)) {
                bridge.state.token = Some(Secret::new(token.to_owned()));
                bridge.save_state()?;
            }
        }
        Ok(bridge)
    }

    /// The directory holding `state.bin` and `queue.bin`.
    pub fn dir(&self) -> &Path {
        self.vault.dir()
    }

    /// An incoming SMS.
    pub fn on_sms(&mut self, sender: &str, body: &str, received_at_ms: i64) -> Report {
        self.on_sms_at(sender, body, received_at_ms, now_ms())
    }

    pub(crate) fn on_sms_at(
        &mut self,
        sender: &str,
        body: &str,
        received_at_ms: i64,
        now: u64,
    ) -> Report {
        let start = Instant::now();
        let result = if !self.state.enabled {
            Forward::Disabled
        } else {
            match parse_otp(sender, body).map(Secret::new) {
                None => Forward::NotOtp,
                Some(code) => self.forward(
                    code,
                    clean_sender(sender),
                    received_at_ms,
                    OtpSource::Sms,
                    now,
                ),
            }
        };
        Report {
            result,
            ms: elapsed_ms(start),
        }
    }

    /// A posted notification (`title` and the big text or text).
    pub fn on_notification(
        &mut self,
        pkg: &str,
        title: &str,
        text: &str,
        posted_at_ms: i64,
    ) -> Report {
        self.on_notification_at(pkg, title, text, posted_at_ms, now_ms())
    }

    pub(crate) fn on_notification_at(
        &mut self,
        pkg: &str,
        title: &str,
        text: &str,
        posted_at_ms: i64,
        now: u64,
    ) -> Report {
        let start = Instant::now();
        let result = if !self.state.notifications {
            Forward::Disabled
        } else if pkg == OWN_PACKAGE {
            Forward::NotOtp
        } else {
            let body = Zeroizing::new(format!("{title}\n{text}"));
            match parse_otp(title, &body).map(Secret::new) {
                None => Forward::NotOtp,
                Some(code) => {
                    let sender = if title.trim().is_empty() { pkg } else { title };
                    self.forward(
                        code,
                        clean_sender(sender),
                        posted_at_ms,
                        OtpSource::Notification,
                        now,
                    )
                }
            }
        };
        Report {
            result,
            ms: elapsed_ms(start),
        }
    }

    /// Dedup, then send or queue one parsed code.
    fn forward(
        &mut self,
        code: Secret,
        sender: String,
        received_at_ms: i64,
        source: OtpSource,
        now: u64,
    ) -> Forward {
        let received_at = u64::try_from(received_at_ms)
            .ok()
            .filter(|&t| t > 0)
            .map_or(now, |t| t.min(now));
        if received_at.saturating_add(CODE_TTL_MS) <= now {
            return self.finish(Forward::Expired);
        }
        let fingerprint = self.vault.fingerprint(code.as_str());
        self.state
            .recent
            .retain(|&(_, at)| at <= now && now - at < DEDUP_MS);
        if self.state.recent.iter().any(|&(f, _)| f == fingerprint) {
            return Forward::Duplicate;
        }
        if self.state.recent.len() >= MAX_RECENT {
            self.state.recent.remove(0);
        }
        self.state.recent.push((fingerprint, now));
        let Ok(event_id) = event_id() else {
            return self.finish(Forward::Error);
        };
        let pending = Pending {
            event_id,
            code,
            sender,
            received_at,
            source,
        };
        let result = match self.send(&pending, now) {
            Outcome::Retry => {
                self.enqueue(pending, now);
                Forward::Queued
            }
            other => forward_result(other),
        };
        self.finish(result)
    }

    /// Sends one code; records the latency when it was accepted.
    fn send(&mut self, pending: &Pending, now: u64) -> Outcome {
        let Some(token) = self.state.token.as_ref() else {
            return Outcome::Unpaired;
        };
        let event = WireEvent(Incoming {
            window_id: None,
            event_id: pending.event_id.clone(),
            code: pending.code.as_str().to_owned(),
            sender: pending.sender.clone(),
            received_at: Some(pending.received_at),
            source: pending.source,
            test: false,
        });
        if event.0.validate().is_err() {
            return Outcome::Rejected;
        }
        let (outcome, ms) = self.transport.post_incoming(token.as_str(), &event.0);
        if let Outcome::Accepted { .. } = outcome {
            self.state.last_forward_at_ms = Some(now);
            self.state.last_latency_ms = Some(ms);
        }
        outcome
    }

    fn enqueue(&mut self, pending: Pending, now: u64) {
        self.queue.retain(|p| p.fresh(now));
        while self.queue.len() >= MAX_QUEUE {
            self.queue.remove(0);
        }
        self.queue.push(pending);
        let _ = self.save_queue();
    }

    /// Records `result` as the last one and saves the state.
    fn finish(&mut self, result: Forward) -> Forward {
        self.state.last_result = Some(result);
        let _ = self.save_state();
        result
    }

    /// Sends queued codes that are still fresh, oldest first, and stops at
    /// the first network failure.
    pub fn flush(&mut self) -> FlushReport {
        self.flush_at(now_ms())
    }

    pub(crate) fn flush_at(&mut self, now: u64) -> FlushReport {
        let before = self.queue.len();
        self.queue.retain(|p| p.fresh(now));
        let mut dropped = before - self.queue.len();
        let mut sent = 0;
        let mut last = None;
        while !self.queue.is_empty() {
            let pending = self.queue.remove(0);
            let outcome = self.send(&pending, now);
            if outcome == Outcome::Retry {
                self.queue.insert(0, pending);
                break;
            }
            let result = forward_result(outcome);
            if result == Forward::Sent {
                sent += 1;
            } else {
                dropped += 1;
            }
            last = Some(result);
        }
        if self.queue.len() != before {
            let _ = self.save_queue();
        }
        if let Some(result) = last {
            self.finish(result);
        }
        FlushReport {
            sent,
            dropped: u32::try_from(dropped).unwrap_or(u32::MAX),
            pending: self.pending(),
            retry: !self.queue.is_empty(),
        }
    }

    /// Current switches, pairing, queue and last result.
    pub fn status(&self) -> Status {
        Status {
            version: VERSION,
            protocol: PROTOCOL_VERSION,
            enabled: self.state.enabled,
            notifications: self.state.notifications,
            paired: self.state.token.is_some(),
            pending: self.pending(),
            last_result: self.state.last_result,
            last_forward_at_ms: self.state.last_forward_at_ms,
            last_latency_ms: self.state.last_latency_ms,
            self_test: *self.self_test.get_or_init(parser_self_test),
        }
    }

    fn pending(&self) -> u32 {
        u32::try_from(self.queue.len()).unwrap_or(u32::MAX)
    }

    /// Applies a JSON patch `{"enabled"?, "notifications"?, "token"?, "url"?}`.
    /// Turning a source off drops its queued codes.
    pub fn configure(&mut self, json: &str) -> Result<Status, ConfigError> {
        let patch: Patch = serde_json::from_str(json).map_err(|_| ConfigError::Json)?;
        if patch.url.as_deref().is_some_and(|u| u != ORIGIN) {
            return Err(ConfigError::Url);
        }
        if patch
            .token
            .as_ref()
            .is_some_and(|t| !valid_token(t.as_str()))
        {
            return Err(ConfigError::Token);
        }
        if let Some(enabled) = patch.enabled {
            self.state.enabled = enabled;
        }
        if let Some(notifications) = patch.notifications {
            self.state.notifications = notifications;
        }
        if let Some(token) = patch.token {
            self.state.token = Some(token);
        }
        let before = self.queue.len();
        let (sms, notifications) = (self.state.enabled, self.state.notifications);
        self.queue.retain(|p| match p.source {
            OtpSource::Sms => sms,
            OtpSource::Notification => notifications,
        });
        if self.queue.len() != before {
            let _ = self.save_queue();
        }
        let _ = self.save_state();
        Ok(self.status())
    }

    /// The parser fixture and, with `network`, a `test: true` round trip.
    pub fn self_test(&mut self, network: bool) -> SelfTest {
        let start = Instant::now();
        let parse = *self.self_test.get_or_init(parser_self_test);
        let result = if network {
            self.probe()
        } else {
            Probe::Skipped
        };
        SelfTest {
            parse,
            result,
            ms: elapsed_ms(start),
        }
    }

    fn probe(&mut self) -> Probe {
        let Some(token) = self.state.token.as_ref() else {
            return Probe::Unpaired;
        };
        let Ok(event_id) = event_id() else {
            return Probe::Error;
        };
        let event = Incoming {
            window_id: None,
            event_id,
            code: TEST_CODE.to_owned(),
            sender: TEST_SENDER.to_owned(),
            received_at: Some(now_ms()),
            source: OtpSource::Sms,
            test: true,
        };
        match self.transport.post_incoming(token.as_str(), &event).0 {
            Outcome::Accepted { test: true } => Probe::Sent,
            Outcome::Accepted { test: false } | Outcome::Rejected => Probe::Error,
            Outcome::Paused => Probe::Paused,
            Outcome::Unpaired => Probe::Unpaired,
            Outcome::Expired => Probe::Expired,
            Outcome::Retry => Probe::Offline,
        }
    }

    fn save_state(&self) -> io::Result<()> {
        let plain = Zeroizing::new(serde_json::to_vec(&self.state)?);
        self.vault.write(STATE_FILE, &plain)
    }

    fn save_queue(&self) -> io::Result<()> {
        if self.queue.is_empty() {
            return self.vault.remove(QUEUE_FILE);
        }
        let plain = Zeroizing::new(serde_json::to_vec(&self.queue)?);
        self.vault.write(QUEUE_FILE, &plain)
    }
}

fn forward_result(outcome: Outcome) -> Forward {
    match outcome {
        Outcome::Accepted { .. } => Forward::Sent,
        Outcome::Paused => Forward::Paused,
        Outcome::Unpaired => Forward::Unpaired,
        Outcome::Expired => Forward::Expired,
        Outcome::Rejected => Forward::Error,
        Outcome::Retry => Forward::Queued,
    }
}
