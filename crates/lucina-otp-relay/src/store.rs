//! The relay's in-memory state: the forwarding window, the pending codes and
//! the recently seen event ids.
//!
//! Pure: every method takes the clock (`now`, Unix ms) and new window ids
//! from the caller, so the unit tests and the Stateright model
//! (`tests/model.rs`) drive it deterministically. The HTTP layer
//! ([`crate::http`]) owns the clock, the randomness and the `paused` file.
//!
//! Rules (`docs/lucina-android.md`, "Relay protocol"):
//!
//! - a code is accepted only while the relay is armed, a timed window
//!   (30–300 s) or continuous;
//! - protocol 1 sends the window id it fetched from `/window`; a wrong one is
//!   refused, protocol 2 sends none;
//! - a code expires at [`proto::expires_at`]: two minutes after the phone
//!   received it, and never after a timed window ends;
//! - an event id seen in the last two minutes is acknowledged as a duplicate
//!   and not stored again; at most [`MAX_SEEN`] ids are remembered;
//! - `test` events are validated and acknowledged, never stored;
//! - at most [`MAX_PENDING`] codes wait, the oldest is dropped first;
//! - [`Store::take`] hands out the newest fresh code once and removes it;
//! - disarming drops every waiting code; with `LUCINA_OTP_CONTINUOUS` it also
//!   pauses forwarding until the next arm.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

use lucina_phone_core::proto::{
    self, Delivered, Incoming, IncomingAck, OtpSource, ProtoError, RelayStatus, WindowState,
    CODE_TTL_MS, PROTOCOL_VERSION,
};

/// At most this many codes wait for the owner.
pub const MAX_PENDING: usize = 8;
/// At most this many event ids are remembered for duplicate detection.
pub const MAX_SEEN: usize = 64;
/// How long an event id is remembered (as long as a code lives).
pub const SEEN_TTL_MS: u64 = CODE_TTL_MS;
/// Shortest timed window.
pub const MIN_ARM_SECONDS: u64 = 30;
/// Longest timed window.
pub const MAX_ARM_SECONDS: u64 = 300;
/// Timed window when `/arm` names no length.
pub const DEFAULT_ARM_SECONDS: u64 = 180;
/// `RelayStatus::transport`.
pub const TRANSPORT: &str = "https-event-driven";

/// Capacity of a [`Store`]. The relay uses the defaults ([`MAX_PENDING`],
/// [`MAX_SEEN`]); the model checks smaller ones to keep its state space small.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Limits {
    pub pending: usize,
    pub seen: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            pending: MAX_PENDING,
            seen: MAX_SEEN,
        }
    }
}

/// Why `/incoming` refused a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The body does not pass [`Incoming::validate`] (400).
    Invalid(ProtoError),
    /// Forwarding is off: never armed, disarmed, paused or the window ended (409).
    Paused,
    /// A protocol-1 `windowId` that is not the current window's (409).
    WrongWindow,
    /// The code is already past its two minutes (410).
    Expired,
    /// [`Limits::seen`] event ids arrived within two minutes (429).
    EventLimit,
}

impl Refusal {
    /// HTTP status for the refusal.
    pub fn status(self) -> u16 {
        match self {
            Self::Invalid(_) => 400,
            Self::Paused | Self::WrongWindow => 409,
            Self::Expired => 410,
            Self::EventLimit => 429,
        }
    }

    /// The `error` field of the response body.
    pub fn error(self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_request",
            Self::Paused => "paused",
            Self::WrongWindow => "wrong_window",
            Self::Expired => "expired",
            Self::EventLimit => "event_limit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Window {
    id: String,
    /// Unix ms; `None` while continuous.
    end: Option<u64>,
}

impl Window {
    fn live(&self, now: u64) -> bool {
        self.end.is_none_or(|end| now < end)
    }
}

/// The relay's state. Codes live only here, in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    /// `LUCINA_OTP_CONTINUOUS`: arming means continuous forwarding.
    continuous: bool,
    /// Continuous forwarding was stopped by `/disarm` (the `paused` file).
    paused: bool,
    limits: Limits,
    window: Option<Window>,
    /// Oldest first.
    pending: VecDeque<Delivered>,
    /// Event id and the time it is forgotten, oldest first.
    seen: VecDeque<(String, u64)>,
}

/// Stateright needs `Hash`; [`Delivered`] and [`OtpSource`] do not derive it.
impl Hash for Store {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.continuous.hash(state);
        self.paused.hash(state);
        self.limits.hash(state);
        self.window.hash(state);
        self.pending.len().hash(state);
        for d in &self.pending {
            d.sender.hash(state);
            d.code.hash(state);
            d.received_at.hash(state);
            d.expires_at.hash(state);
            (d.source == OtpSource::Sms).hash(state);
        }
        self.seen.hash(state);
    }
}

impl Store {
    /// A disarmed store. `continuous` is `LUCINA_OTP_CONTINUOUS`; `paused`
    /// whether the `paused` file exists. Call [`Store::arm`] at start when
    /// [`Store::starts_armed`] says so.
    pub fn new(continuous: bool, paused: bool) -> Self {
        Self {
            continuous,
            paused,
            limits: Limits::default(),
            window: None,
            pending: VecDeque::new(),
            seen: VecDeque::new(),
        }
    }

    /// The same store with other capacities.
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Continuous forwarding that was not paused resumes at start.
    pub fn starts_armed(&self) -> bool {
        self.continuous && !self.paused
    }

    /// Configured for continuous forwarding.
    pub fn continuous(&self) -> bool {
        self.continuous
    }

    /// Continuous forwarding is paused (the `paused` file should exist).
    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Codes waiting, fresh or not yet pruned.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Event ids remembered.
    pub fn seen_len(&self) -> usize {
        self.seen.len()
    }

    /// The store's capacities.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    fn live_window(&self, now: u64) -> Option<&Window> {
        self.window.as_ref().filter(|w| w.live(now))
    }

    /// Codes are accepted now.
    pub fn armed(&self, now: u64) -> bool {
        self.live_window(now).is_some()
    }

    /// `GET /window`, and the answer to `POST /arm`.
    pub fn window(&self, now: u64) -> WindowState {
        match self.live_window(now) {
            Some(w) => WindowState {
                armed: true,
                window_id: Some(w.id.clone()),
                expires_at: w.end,
                continuous: w.end.is_none(),
            },
            None => WindowState::default(),
        }
    }

    /// `GET /status`.
    pub fn status(&self, now: u64) -> RelayStatus {
        let window = self.window(now);
        RelayStatus {
            version: env!("CARGO_PKG_VERSION").into(),
            transport: TRANSPORT.into(),
            armed: window.armed,
            continuous: window.continuous,
            protocol: PROTOCOL_VERSION,
        }
    }

    /// `POST /arm`: drops waiting codes and opens a new window with id
    /// `window_id`. Continuous (and unpaused) when configured so, else
    /// `seconds` long, clamped to 30–300 s.
    pub fn arm(&mut self, seconds: u64, now: u64, window_id: String) -> WindowState {
        self.pending.clear();
        let end = if self.continuous {
            self.paused = false;
            None
        } else {
            let seconds = seconds.clamp(MIN_ARM_SECONDS, MAX_ARM_SECONDS);
            Some(now.saturating_add(seconds * 1000))
        };
        self.window = Some(Window { id: window_id, end });
        self.window(now)
    }

    /// `POST /disarm`: closes the window and drops waiting codes; pauses
    /// continuous forwarding.
    ///
    /// Seen event ids are kept (unlike 0.4.0), so a phone retrying a code it
    /// already delivered after the owner re-arms gets a duplicate answer
    /// instead of a second delivery.
    pub fn disarm(&mut self) {
        self.window = None;
        self.pending.clear();
        if self.continuous {
            self.paused = true;
        }
    }

    fn prune(&mut self, now: u64) {
        self.pending.retain(|d| d.expires_at > now);
        self.seen.retain(|(_, until)| *until > now);
    }

    /// `POST /incoming`.
    pub fn receive(&mut self, event: &Incoming, now: u64) -> Result<IncomingAck, Refusal> {
        event.validate().map_err(Refusal::Invalid)?;
        if event.test {
            return Ok(IncomingAck {
                accepted: true,
                test: true,
                ..Default::default()
            });
        }
        let window = self.live_window(now).ok_or(Refusal::Paused)?;
        if event.window_id.as_deref().is_some_and(|id| id != window.id) {
            return Err(Refusal::WrongWindow);
        }
        let window_end = window.end.unwrap_or(u64::MAX);
        self.prune(now);
        if self.seen.iter().any(|(id, _)| *id == event.event_id) {
            return Ok(IncomingAck {
                accepted: true,
                duplicate: true,
                ..Default::default()
            });
        }
        let expires_at =
            proto::expires_at(event.received_at, now, window_end).map_err(|_| Refusal::Expired)?;
        if self.seen.len() >= self.limits.seen {
            return Err(Refusal::EventLimit);
        }
        self.seen
            .push_back((event.event_id.clone(), now.saturating_add(SEEN_TTL_MS)));
        if self.pending.len() >= self.limits.pending {
            self.pending.pop_front();
        }
        self.pending.push_back(Delivered {
            sender: proto::clean_sender(&event.sender),
            code: event.code.clone(),
            received_at: event.received_at.map_or(now, |t| t.min(now)),
            expires_at,
            source: event.source,
        });
        Ok(IncomingAck {
            accepted: true,
            ..Default::default()
        })
    }

    /// `GET /otp`: the newest code that has not expired, removed by this
    /// call. Newer expired codes are dropped on the way. A store that is no
    /// longer armed (its window ended) drops everything and returns `None`.
    pub fn take(&mut self, now: u64) -> Option<Delivered> {
        if !self.armed(now) {
            self.window = None;
            self.pending.clear();
            return None;
        }
        while let Some(d) = self.pending.pop_back() {
            if d.expires_at > now {
                return Some(d);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u64 = 1_700_000_000_000;
    const WINDOW: &str = "00112233445566778899aabbccddeeff0011223344556677";

    fn event(n: u32) -> Incoming {
        Incoming {
            window_id: None,
            event_id: format!("8c1f0a52-7d2e-4f0b-9a51-{n:012}"),
            code: format!("{:06}", 100_000 + n),
            sender: "Bank".into(),
            received_at: Some(NOW),
            source: OtpSource::Sms,
            test: false,
        }
    }

    fn timed() -> Store {
        let mut s = Store::new(false, false);
        s.arm(60, NOW, WINDOW.into());
        s
    }

    #[test]
    fn protocol_one_window_and_consume_once() {
        let mut s = Store::new(false, false);
        assert!(!s.starts_armed());
        assert_eq!(s.window(NOW), WindowState::default());
        let w = s.arm(60, NOW, WINDOW.into());
        assert_eq!(
            serde_json::to_value(&w).unwrap(),
            json!({"armed": true, "windowId": WINDOW, "expiresAt": NOW + 60_000, "continuous": false})
        );
        // The exact body app 0.4.0 sends.
        let v1: Incoming = serde_json::from_value(json!({
            "windowId": WINDOW, "eventId": "1b4e28ba-2fa1-11d2-883f-0016d3cca427", "code": "482917", "sender": "Bank"
        }))
        .unwrap();
        assert_eq!(
            s.receive(&v1, NOW).unwrap(),
            IncomingAck {
                accepted: true,
                ..Default::default()
            }
        );
        assert!(s.receive(&v1, NOW + 1).unwrap().duplicate);
        let d = s.take(NOW + 2).unwrap();
        assert_eq!((d.code.as_str(), d.sender.as_str()), ("482917", "Bank"));
        assert_eq!((d.received_at, d.expires_at), (NOW, NOW + 60_000));
        assert_eq!(s.take(NOW + 3), None);
        let mut wrong = v1.clone();
        wrong.event_id = "1b4e28ba-2fa1-11d2-883f-0016d3cca428".into();
        wrong.window_id = Some("ffffffffffffffff".into());
        assert_eq!(s.receive(&wrong, NOW), Err(Refusal::WrongWindow));
        s.disarm();
        assert!(!s.paused(), "a timed relay never pauses");
        wrong.window_id = Some(WINDOW.into());
        assert_eq!(s.receive(&wrong, NOW), Err(Refusal::Paused));
    }

    #[test]
    fn protocol_two_expiry_and_window_end() {
        let mut s = timed();
        let mut e = event(1);
        e.received_at = Some(NOW - 20_000);
        s.receive(&e, NOW).unwrap();
        let d = s.take(NOW).unwrap();
        // Two minutes from the phone's clock, capped by the window end.
        assert_eq!(d.expires_at, NOW + 60_000);
        assert_eq!(d.received_at, NOW - 20_000);
        let mut stale = event(2);
        stale.received_at = Some(NOW - CODE_TTL_MS);
        assert_eq!(s.receive(&stale, NOW), Err(Refusal::Expired));
        assert_eq!(s.seen_len(), 1, "an expired code is not remembered");
        s.receive(&event(3), NOW).unwrap();
        assert!(s.armed(NOW + 59_999));
        assert_eq!(s.take(NOW + 60_000), None);
        assert_eq!(s.pending_len(), 0);
        assert_eq!(s.receive(&event(4), NOW + 60_000), Err(Refusal::Paused));
    }

    #[test]
    fn continuous_pause_and_resume() {
        let mut s = Store::new(true, false);
        assert!(s.starts_armed());
        let w = s.arm(0, NOW, WINDOW.into());
        assert!(w.armed && w.continuous && w.expires_at.is_none());
        assert_eq!(
            serde_json::to_value(s.status(NOW + 10 * CODE_TTL_MS)).unwrap(),
            json!({"version": env!("CARGO_PKG_VERSION"), "transport": TRANSPORT, "armed": true, "continuous": true, "protocol": 2})
        );
        s.receive(&event(1), NOW).unwrap();
        assert_eq!(s.take(NOW + 1).unwrap().expires_at, NOW + CODE_TTL_MS);
        s.receive(&event(2), NOW).unwrap();
        s.disarm();
        assert!(s.paused() && !s.armed(NOW));
        assert_eq!(s.receive(&event(3), NOW), Err(Refusal::Paused));
        assert_eq!(s.take(NOW), None);
        // A restart keeps the pause.
        assert!(!Store::new(true, true).starts_armed());
        s.arm(0, NOW, WINDOW.into());
        assert!(!s.paused());
        assert_eq!(s.take(NOW), None, "disarm dropped the waiting code");
        // A retry of a code delivered before the pause is still a duplicate.
        assert!(s.receive(&event(1), NOW + 1).unwrap().duplicate);
    }

    #[test]
    fn test_events_are_acknowledged_never_stored() {
        let mut s = Store::new(false, false);
        let mut e = event(1);
        e.test = true;
        let ack = s.receive(&e, NOW).unwrap();
        assert!(ack.accepted && ack.test && !ack.duplicate);
        s.arm(60, NOW, WINDOW.into());
        s.receive(&e, NOW).unwrap();
        assert_eq!((s.pending_len(), s.seen_len()), (0, 0));
        assert_eq!(s.take(NOW), None);
        e.code = "12".into();
        assert_eq!(
            s.receive(&e, NOW),
            Err(Refusal::Invalid(ProtoError::InvalidCode))
        );
    }

    #[test]
    fn limits_and_newest_first() {
        let mut s = timed();
        for n in 0..9 {
            s.receive(&event(n), NOW + u64::from(n)).unwrap();
        }
        assert_eq!(s.pending_len(), MAX_PENDING);
        let codes: Vec<String> = std::iter::from_fn(|| s.take(NOW + 10).map(|d| d.code)).collect();
        assert_eq!(codes.len(), MAX_PENDING);
        assert_eq!(codes[0], "100008");
        assert_eq!(codes[7], "100001", "the oldest code was dropped");
        for n in 9..MAX_SEEN as u32 {
            s.receive(&event(n), NOW + 10).unwrap();
        }
        assert_eq!(s.seen_len(), MAX_SEEN);
        assert_eq!(s.receive(&event(100), NOW + 10), Err(Refusal::EventLimit));
        // Ids are forgotten after two minutes.
        let mut late = event(100);
        late.received_at = None;
        s.disarm();
        s.arm(300, NOW + SEEN_TTL_MS + 10, WINDOW.into());
        assert!(s.receive(&late, NOW + SEEN_TTL_MS + 10).is_ok());
        assert_eq!(s.seen_len(), 1);
    }

    #[test]
    fn arm_clamps_and_sender_is_cleaned() {
        let mut s = Store::new(false, false);
        assert_eq!(s.arm(5, NOW, WINDOW.into()).expires_at, Some(NOW + 30_000));
        assert_eq!(
            s.arm(9_999, NOW, WINDOW.into()).expires_at,
            Some(NOW + 300_000)
        );
        let mut e = event(1);
        e.sender = "<b>Bank\u{7}</b>".into();
        e.source = OtpSource::Notification;
        s.receive(&e, NOW).unwrap();
        let d = s.take(NOW).unwrap();
        assert_eq!(
            (d.sender.as_str(), d.source),
            ("bBank/b", OtpSource::Notification)
        );
        let mut future = event(2);
        future.received_at = Some(NOW + 50_000);
        s.receive(&future, NOW).unwrap();
        assert_eq!(
            s.take(NOW).unwrap().received_at,
            NOW,
            "a phone clock ahead counts as now"
        );
    }
}
