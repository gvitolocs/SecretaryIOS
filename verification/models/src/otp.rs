//! Lucina OTP forwarding: the phone (`crates/lucina-android`), the relay on p1
//! (`crates/lucina-otp-relay`) and the owner's `/otp` through Lucina
//! (`crates/lucina-server/src/otp.rs`), all on branch `lucina-rs`
//! (`docs/lucina-android.md`, "Relay protocol").
//!
//! Actors and environment:
//!
//! - **Sources.** Each code arrives as a few copies (an SMS and the messaging
//!   app's notification, or a duplicate SMS), each within the code's life.
//!   `received_at` is the copy's own time (`SmsReceiver`:
//!   `System.currentTimeMillis()`; the listener: `sbn.postTime`).
//! - **The phone core** (`Bridge`, one per process behind the JNI mutex, so
//!   one request at a time): parse, dedup by code within `DEDUP_MS`, a fresh
//!   event id per forwarded copy (`store::event_id`, kept in `queue.bin` and
//!   reused on every retry), the encrypted queue (TTL, at most 8), `flush`.
//!   `state.bin` (the dedup fingerprints) is written in `finish`, after the
//!   first send returns; `queue.bin` in `enqueue`, also after it.
//! - **Reboots / process death:** the queue and the saved state survive; the
//!   request in flight and anything not yet saved are lost.
//! - **The network** loses the request, or the response after the relay
//!   acted on it (also "200 with an unreadable body"): the phone sees
//!   `Outcome::Retry` either way.
//! - **The relay** (`Store`, its own clock): armed (continuous, the deployed
//!   `LUCINA_OTP_CONTINUOUS=true`, or a timed window), the `paused` file,
//!   pending codes (newest first out, at most 8, consume once), seen event
//!   ids (kept across arm/disarm), `test` events never stored, 409/410.
//!   Seen event ids are written to `seen.json` before an accepted
//!   `/incoming` is answered (a failed write rolls the store back and
//!   answers 503); a restart keeps `paused` and `seen.json`, not the codes.
//! - **Lucina** answers the owner's `/otp` in steps, each one relay call:
//!   take; on 204 status; arm if not armed. A taken code is checked against
//!   Lucina's clock before it is shown. `/otp stop` disarms, `/otp arm` arms.
//!   Several `/otp` requests may be in flight (a client retry runs the whole
//!   flow again: the OTP path skips the `clientMessageId` cache).
//! - **Another client** (Lucina MCP, a non-owner Telegram chat) asks too;
//!   `reply::private_owner` refuses it before any relay call.
//! - **Time** moves in ticks; a code lives `ttl` ticks.
//!
//! | Model action | Code |
//! |---|---|
//! | `Arrive(c, i)` | `SmsReceiver` / `OtpNotificationListener` → `Bridge::forward` (dedup, `event_id`, `send`) |
//! | `Serve` | `Relay::handle` `POST /incoming` → `Store::receive` |
//! | `LoseRequest`, `LoseResponse` | ureq error / timeout, or `classify(200, None)` → `Outcome::Retry` |
//! | `PhoneGets` | `forward` after `send` (`enqueue`, `finish`), or the `flush_at` loop body |
//! | `Flush` | `RetryJobService` / `BootReceiver` → `Bridge::flush_at` (drops stale entries, sends the oldest) |
//! | `SelfTest` | `Bridge::probe` (`test: true`) |
//! | `Crash` | reboot or process death: `Bridge::open` reloads `state.bin` and `queue.bin` |
//! | `OwnerAsks`, `LucinaStep` | `otp::run` `OtpAction::Take`: `take`, then `status`, then `arm` |
//! | `OwnerStop`, `OwnerArm` | `/otp stop` → `/disarm` (`Store::disarm`, `paused` file); `/otp arm` → `/arm` (`Store::arm`) |
//! | `OtherAsks` | `otp::respond` / `secretary_chat` with an MCP or non-owner asker |
//! | `SeenWriteFails` | `Relay::handle` `/incoming`: `save_seen` fails → `self.store = before`, 503 → `Outcome::Retry` |
//! | `RelayRestart` | `Relay::open` (`paused` file, `seen.json`, `starts_armed`); codes were in memory |
//! | `Tick` | the clocks (phone and relay agree to within a tick) |
//!
//! Switches: `write_ahead` (F1, commit `ac9e7be`), `arm_keeps_codes` and
//! `persist_seen` (F2, F3, commit `60472d9`) model the fixes, all on branch
//! `otp-fixes`; true is the code as it is now. The `*_bug_pre_*` cases keep
//! the code from before each fix as regression witnesses. `owner_guard`,
//! `lucina_expiry_check` and `relay_dedup` are mechanisms of the code that
//! the ablation cases switch off.

use stateright::{Model, Property};

use crate::{set_insert, Case, Expect};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Src {
    Sms,
    Notification,
}

/// Event id: `(code, copy)` of the forwarded copy (`event_id()` is fresh per
/// forwarded message). The self-test uses the code [`Otp::test_code`].
pub type Eid = (u8, u8);

/// A queued or in-flight event (`Pending` in `bridge.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Entry {
    pub code: u8,
    pub eid: Eid,
    pub recv: u8,
    pub src: Src,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// The first send inside `forward` (not saved anywhere yet).
    Live,
    /// A send of the oldest `queue.bin` entry (`flush_at`).
    Queued,
    /// `probe` (`test: true`).
    Test,
}

/// `net::Outcome` as the phone sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    Accepted,
    /// 200 `duplicate: true` (also `Accepted` for the phone).
    Duplicate,
    /// 200 `test: true`.
    TestAck,
    /// 409.
    Paused,
    /// 410.
    Expired,
    /// Network error, timeout, unreadable 200, 5xx.
    Retry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    Request,
    Response(Outcome),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InFlight {
    pub entry: Entry,
    pub kind: Kind,
    pub stage: Stage,
}

/// A code waiting at the relay (`Delivered`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Waiting {
    pub code: u8,
    pub exp: u8,
    pub src: Src,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Window {
    Off,
    Continuous,
    /// A timed window ending at this tick.
    Until(u8),
}

/// One owner `/otp` request inside Lucina (`otp::run`, `OtpAction::Take`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Lucina {
    /// About to call `GET /otp`.
    Take,
    /// `GET /otp` returned this code; next: the expiry check and the reply.
    Got(Waiting),
    /// 204: about to call `GET /status`.
    Status,
    /// Not armed: about to call `POST /arm`.
    Arm,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub now: u8,
    // Environment (ghost): first arrival per code, copies arrived (bit mask).
    pub born: Vec<Option<u8>>,
    pub arrived: Vec<u8>,
    // Phone.
    pub queue: Vec<Entry>,
    /// `state.recent` in memory: `(code, handled at)`.
    pub recent: Vec<(u8, u8)>,
    /// `state.recent` as last saved in `state.bin`.
    pub recent_disk: Vec<(u8, u8)>,
    pub inflight: Option<InFlight>,
    // Relay.
    pub window: Window,
    pub paused: bool,
    /// Oldest first.
    pub pending: Vec<Waiting>,
    /// `(event id, forgotten at)`.
    pub seen: Vec<(Eid, u8)>,
    // Lucina: the owner's `/otp` requests in progress (a multiset, sorted).
    pub lucina: Vec<Lucina>,
    // Budgets.
    pub drops: u8,
    pub crashes: u8,
    pub restarts: u8,
    pub seen_failures: u8,
    pub takes: u8,
    pub stops: u8,
    pub arms: u8,
    pub tests: u8,
    pub others: u8,
    // Ghosts.
    /// `(code, tick, source)` shown to the owner.
    pub delivered: Vec<(u8, u8, Src)>,
    /// Codes dropped by an explicit, documented path (bit mask): 409, 410,
    /// the phone's TTL, a pause or window end, a full queue, a relay restart.
    pub exempt: u8,
    /// Codes first seen before a crash (bit mask).
    pub pre_crash: u8,
    /// A code reached a client that is not the owner's private chat.
    pub leaked: bool,
    /// The relay answered a retry `duplicate: true`.
    pub retry_dedup: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Arrive(u8, u8),
    Serve,
    LoseRequest,
    LoseResponse,
    SeenWriteFails,
    PhoneGets,
    Flush,
    SelfTest,
    Crash,
    Tick,
    OwnerAsks,
    LucinaStep(Lucina),
    OwnerStop,
    OwnerArm,
    OtherAsks,
    RelayRestart,
}

#[derive(Clone, Debug)]
pub struct Otp {
    /// Per code, its copies (SMS, notification, a duplicate SMS).
    pub copies: Vec<Vec<Src>>,
    /// Ticks a code lives (`CODE_TTL_MS`); the phone's `DEDUP_MS` and the
    /// relay's `SEEN_TTL_MS` are the same length.
    pub ttl: u8,
    pub max_time: u8,
    /// `LUCINA_OTP_CONTINUOUS=true` (deployed); else timed windows.
    pub continuous: bool,
    /// Length of a timed window (`ARM_SECONDS`), in ticks.
    pub window: u8,
    /// `MAX_PENDING` and `MAX_QUEUE` (8), scaled down.
    pub relay_limit: usize,
    pub queue_limit: usize,
    pub drops: u8,
    pub crashes: u8,
    pub restarts: u8,
    /// Failed `seen.json` writes (needs `persist_seen`).
    pub seen_failures: u8,
    /// Owner `/otp` requests (in total, and at once).
    pub takes: u8,
    pub threads: usize,
    pub stops: u8,
    pub arms: u8,
    pub tests: u8,
    pub others: u8,
    // Fixes (true = the code now; false = before the fix).
    /// F1 (`ac9e7be`): `forward` queues the code and saves its fingerprint
    /// before the first send (`Bridge::open` re-derives fingerprints of
    /// queued codes), and removes the entry on any answer but Retry.
    pub write_ahead: bool,
    /// F2 (`60472d9`): `Store::arm` keeps the waiting codes and drops only
    /// expired ones. Before: `pending.clear()`.
    pub arm_keeps_codes: bool,
    /// F3 (`60472d9`): seen ids go to `seen.json` before the 200 and are
    /// reloaded at start. Before: memory only.
    pub persist_seen: bool,
    // Mechanisms (true = the code; ablations switch one off).
    pub owner_guard: bool,
    pub lucina_expiry_check: bool,
    pub relay_dedup: bool,
}

impl Otp {
    pub fn test_code(&self) -> u8 {
        self.copies.len() as u8
    }

    fn exempt(s: &mut State, code: u8) {
        if code < 8 {
            s.exempt |= 1 << code;
        }
    }

    fn armed(s: &State) -> bool {
        match s.window {
            Window::Off => false,
            Window::Continuous => true,
            Window::Until(end) => s.now < end,
        }
    }

    fn fresh(&self, e: &Entry, now: u8) -> bool {
        e.recv + self.ttl > now
    }

    /// `Store::receive`.
    fn receive(&self, s: &mut State, e: Entry) -> Outcome {
        if e.code == self.test_code() {
            return Outcome::TestAck;
        }
        if !Self::armed(s) {
            return Outcome::Paused;
        }
        let now = s.now;
        // prune
        let (keep, gone): (Vec<Waiting>, Vec<Waiting>) =
            s.pending.iter().partition(|w| w.exp > now);
        s.pending = keep;
        for w in gone {
            Self::exempt(s, w.code);
        }
        s.seen.retain(|x| x.1 > now);
        if self.relay_dedup && s.seen.iter().any(|x| x.0 == e.eid) {
            return Outcome::Duplicate;
        }
        let mut exp = e.recv.min(now) + self.ttl;
        if let Window::Until(end) = s.window {
            exp = exp.min(end);
        }
        if exp <= now {
            return Outcome::Expired;
        }
        if self.relay_dedup {
            set_insert(&mut s.seen, (e.eid, now + self.ttl));
        }
        if s.pending.len() >= self.relay_limit {
            let w = s.pending.remove(0);
            Self::exempt(s, w.code);
        }
        s.pending.push(Waiting {
            code: e.code,
            exp,
            src: e.src,
        });
        Outcome::Accepted
    }

    /// `Store::take`.
    fn take(s: &mut State) -> Option<Waiting> {
        if !Self::armed(s) {
            s.window = Window::Off;
            for w in std::mem::take(&mut s.pending) {
                Self::exempt(s, w.code);
            }
            return None;
        }
        while let Some(w) = s.pending.pop() {
            if w.exp > s.now {
                return Some(w);
            }
            Self::exempt(s, w.code);
        }
        None
    }

    /// `Store::arm`. F2: keep the waiting codes, drop the expired ones.
    /// Before F2: drop them all, armed or not.
    fn arm(&self, s: &mut State) {
        let was_armed = Self::armed(s);
        if self.arm_keeps_codes {
            let now = s.now;
            let (keep, gone): (Vec<Waiting>, Vec<Waiting>) =
                s.pending.iter().partition(|w| w.exp > now);
            s.pending = keep;
            for w in gone {
                Self::exempt(s, w.code);
            }
        } else {
            for w in std::mem::take(&mut s.pending) {
                // Codes of a window that already ended are gone anyway; codes
                // dropped from a live window are not exempt (the bug).
                if !was_armed {
                    Self::exempt(s, w.code);
                }
            }
        }
        if self.continuous {
            s.paused = false;
            s.window = Window::Continuous;
        } else {
            s.window = Window::Until(s.now + self.window);
        }
    }

    /// `Store::disarm` (+ the `paused` file).
    fn disarm(&self, s: &mut State) {
        s.window = Window::Off;
        for w in std::mem::take(&mut s.pending) {
            Self::exempt(s, w.code);
        }
        if self.continuous {
            s.paused = true;
        }
    }

    /// `Bridge::enqueue`: drop stale entries, the oldest when full, push.
    fn enqueue(&self, s: &mut State, e: Entry) {
        let now = s.now;
        let (keep, gone): (Vec<Entry>, Vec<Entry>) =
            s.queue.iter().partition(|q| self.fresh(q, now));
        s.queue = keep;
        for q in gone {
            Self::exempt(s, q.code);
        }
        while s.queue.len() >= self.queue_limit {
            let q = s.queue.remove(0);
            Self::exempt(s, q.code);
        }
        s.queue.push(e);
    }

    fn held(s: &State, c: u8) -> bool {
        s.queue.iter().any(|e| e.code == c)
            || s.inflight.is_some_and(|f| f.entry.code == c)
            || s.pending.iter().any(|w| w.code == c)
            || s.lucina
                .iter()
                .any(|l| matches!(l, Lucina::Got(w) if w.code == c))
    }
}

impl Model for Otp {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let k = self.copies.len();
        vec![State {
            now: 0,
            born: vec![None; k],
            arrived: vec![0; k],
            queue: Vec::new(),
            recent: Vec::new(),
            recent_disk: Vec::new(),
            inflight: None,
            // Relay::open: continuous and not paused starts armed.
            window: if self.continuous {
                Window::Continuous
            } else {
                Window::Off
            },
            paused: false,
            pending: Vec::new(),
            seen: Vec::new(),
            lucina: Vec::new(),
            drops: self.drops,
            crashes: self.crashes,
            restarts: self.restarts,
            seen_failures: self.seen_failures,
            takes: self.takes,
            stops: self.stops,
            arms: self.arms,
            tests: self.tests,
            others: self.others,
            delivered: Vec::new(),
            exempt: 0,
            pre_crash: 0,
            leaked: false,
            retry_dedup: false,
        }]
    }

    fn actions(&self, s: &State, a: &mut Vec<Action>) {
        if s.inflight.is_none() {
            for (c, copies) in self.copies.iter().enumerate() {
                let alive = s.born[c].is_none_or(|b| s.now < b + self.ttl);
                for i in 0..copies.len() {
                    if alive && s.arrived[c] & (1 << i) == 0 {
                        a.push(Action::Arrive(c as u8, i as u8));
                    }
                }
            }
            if !s.queue.is_empty() {
                a.push(Action::Flush);
            }
            if s.tests > 0 {
                a.push(Action::SelfTest);
            }
        }
        match s.inflight.map(|f| f.stage) {
            Some(Stage::Request) => {
                a.push(Action::Serve);
                if s.drops > 0 {
                    a.push(Action::LoseRequest);
                }
                if self.persist_seen && s.seen_failures > 0 {
                    a.push(Action::SeenWriteFails);
                }
            }
            Some(Stage::Response(o)) => {
                a.push(Action::PhoneGets);
                if s.drops > 0 && o != Outcome::Retry {
                    a.push(Action::LoseResponse);
                }
            }
            None => {}
        }
        if s.crashes > 0 && (s.inflight.is_some() || !s.queue.is_empty()) {
            a.push(Action::Crash);
        }
        if s.now < self.max_time {
            a.push(Action::Tick);
        }
        if s.takes > 0 && s.lucina.len() < self.threads {
            a.push(Action::OwnerAsks);
        }
        let mut last = None;
        for &l in &s.lucina {
            if last != Some(l) {
                a.push(Action::LucinaStep(l));
            }
            last = Some(l);
        }
        if s.stops > 0 {
            a.push(Action::OwnerStop);
        }
        if s.arms > 0 {
            a.push(Action::OwnerArm);
        }
        if s.others > 0 {
            a.push(Action::OtherAsks);
        }
        if s.restarts > 0 {
            a.push(Action::RelayRestart);
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut s = last.clone();
        let now = s.now;
        match action {
            Action::Arrive(c, i) => {
                let ci = c as usize;
                s.arrived[ci] |= 1 << i;
                if s.born[ci].is_none() {
                    s.born[ci] = Some(now);
                }
                // forward(): received_at is the copy's own time.
                let ttl = self.ttl;
                s.recent.retain(|&(_, at)| now - at < ttl);
                if s.recent.iter().any(|&(x, _)| x == c) {
                    return Some(s); // Forward::Duplicate
                }
                set_insert(&mut s.recent, (c, now));
                let entry = Entry {
                    code: c,
                    eid: (c, i),
                    recv: now,
                    src: self.copies[ci][i as usize],
                };
                let kind = if self.write_ahead {
                    s.recent_disk = s.recent.clone();
                    self.enqueue(&mut s, entry);
                    Kind::Queued
                } else {
                    Kind::Live
                };
                s.inflight = Some(InFlight {
                    entry,
                    kind,
                    stage: Stage::Request,
                });
            }
            Action::Serve => {
                let mut f = s.inflight?;
                let o = self.receive(&mut s, f.entry);
                if o == Outcome::Duplicate {
                    s.retry_dedup = true;
                }
                f.stage = Stage::Response(o);
                s.inflight = Some(f);
            }
            Action::SeenWriteFails => {
                // Only an acceptance writes seen.json; a failed write rolls
                // the whole store back (`self.store = before`) and answers 503.
                let mut f = s.inflight?;
                let mut probe = s.clone();
                if self.receive(&mut probe, f.entry) != Outcome::Accepted {
                    return None;
                }
                s.seen_failures -= 1;
                f.stage = Stage::Response(Outcome::Retry);
                s.inflight = Some(f);
            }
            Action::LoseRequest | Action::LoseResponse => {
                s.drops -= 1;
                let mut f = s.inflight?;
                f.stage = Stage::Response(Outcome::Retry);
                s.inflight = Some(f);
            }
            Action::PhoneGets => {
                let f = s.inflight.take()?;
                let Stage::Response(o) = f.stage else {
                    return None;
                };
                let e = f.entry;
                match f.kind {
                    Kind::Live => {
                        match o {
                            Outcome::Retry => self.enqueue(&mut s, e),
                            Outcome::Paused | Outcome::Expired => Self::exempt(&mut s, e.code),
                            _ => {}
                        }
                        // finish(): save_state.
                        s.recent_disk = s.recent.clone();
                    }
                    Kind::Queued => match o {
                        Outcome::Retry => {}
                        Outcome::Paused | Outcome::Expired => {
                            s.queue.retain(|q| q.eid != e.eid);
                            Self::exempt(&mut s, e.code);
                        }
                        _ => s.queue.retain(|q| q.eid != e.eid),
                    },
                    Kind::Test => {}
                }
            }
            Action::Flush => {
                let (keep, gone): (Vec<Entry>, Vec<Entry>) =
                    s.queue.iter().partition(|q| self.fresh(q, now));
                s.queue = keep;
                for q in gone {
                    Self::exempt(&mut s, q.code);
                }
                if let Some(&e) = s.queue.first() {
                    s.inflight = Some(InFlight {
                        entry: e,
                        kind: Kind::Queued,
                        stage: Stage::Request,
                    });
                }
            }
            Action::SelfTest => {
                s.tests -= 1;
                let t = self.test_code();
                s.inflight = Some(InFlight {
                    entry: Entry {
                        code: t,
                        eid: (t, 0),
                        recv: now,
                        src: Src::Sms,
                    },
                    kind: Kind::Test,
                    stage: Stage::Request,
                });
            }
            Action::Crash => {
                s.crashes -= 1;
                s.inflight = None;
                s.recent = s.recent_disk.clone();
                for (c, b) in s.born.iter().enumerate() {
                    if b.is_some() {
                        s.pre_crash |= 1 << c;
                    }
                }
            }
            Action::Tick => {
                s.now += 1;
                // Entries past their window never match again: drop them
                // (a pure state reduction, the code prunes them lazily).
                let (n, ttl) = (s.now, self.ttl);
                s.recent.retain(|&(_, at)| n - at < ttl);
                s.recent_disk.retain(|&(_, at)| n - at < ttl);
                s.seen.retain(|x| x.1 > n);
            }
            Action::OwnerAsks => {
                s.takes -= 1;
                set_insert_multi(&mut s.lucina, Lucina::Take);
            }
            Action::LucinaStep(l) => {
                let i = s.lucina.iter().position(|x| *x == l)?;
                s.lucina.remove(i);
                match l {
                    Lucina::Take => match Self::take(&mut s) {
                        Some(w) => set_insert_multi(&mut s.lucina, Lucina::Got(w)),
                        None => set_insert_multi(&mut s.lucina, Lucina::Status),
                    },
                    Lucina::Got(w) => {
                        if self.lucina_expiry_check && w.exp <= now {
                            // "expired code" → UNAVAILABLE; the code is gone.
                            Self::exempt(&mut s, w.code);
                        } else {
                            s.delivered.push((w.code, now, w.src));
                        }
                    }
                    Lucina::Status => {
                        if !Self::armed(&s) {
                            set_insert_multi(&mut s.lucina, Lucina::Arm);
                        }
                    }
                    Lucina::Arm => self.arm(&mut s),
                }
            }
            Action::OwnerStop => {
                s.stops -= 1;
                self.disarm(&mut s);
            }
            Action::OwnerArm => {
                s.arms -= 1;
                self.arm(&mut s);
            }
            Action::OtherAsks => {
                s.others -= 1;
                if !self.owner_guard && Self::take(&mut s).is_some() {
                    s.leaked = true;
                }
            }
            Action::RelayRestart => {
                s.restarts -= 1;
                for w in std::mem::take(&mut s.pending) {
                    Self::exempt(&mut s, w.code);
                }
                if !self.persist_seen {
                    s.seen.clear();
                }
                s.window = if self.continuous && !s.paused {
                    Window::Continuous
                } else {
                    Window::Off
                };
            }
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("at_most_once", |m: &Otp, s: &State| {
                (0..m.copies.len() as u8)
                    .all(|c| s.delivered.iter().filter(|d| d.0 == c).count() <= 1)
            }),
            Property::always("never_after_expiry", |m: &Otp, s: &State| {
                s.delivered.iter().all(|&(c, t, _)| {
                    s.born
                        .get(c as usize)
                        .copied()
                        .flatten()
                        .is_some_and(|b| t < b + m.ttl)
                })
            }),
            Property::always("owner_only", |_, s: &State| !s.leaked),
            Property::always("test_never_delivered", |m: &Otp, s: &State| {
                let t = m.test_code();
                s.delivered.iter().all(|d| d.0 != t) && s.pending.iter().all(|w| w.code != t)
            }),
            // Every received, unexpired, undelivered code that no explicit
            // path dropped is in the phone queue, in flight, waiting at the
            // relay, or in Lucina's hands.
            Property::always("no_silent_loss", |m: &Otp, s: &State| {
                (0..m.copies.len() as u8).all(|c| match s.born[c as usize] {
                    None => true,
                    Some(b) => {
                        s.now >= b + m.ttl
                            || s.delivered.iter().any(|d| d.0 == c)
                            || s.exempt & (1 << c) != 0
                            || Otp::held(s, c)
                    }
                })
            }),
            Property::always("pending_bounded", |m: &Otp, s: &State| {
                s.pending.len() <= m.relay_limit && s.queue.len() <= m.queue_limit
            }),
            Property::sometimes("retry_deduplicated", |_, s: &State| s.retry_dedup),
            Property::sometimes("code_expired_undelivered", |m: &Otp, s: &State| {
                (0..m.copies.len()).any(|c| {
                    s.born[c].is_some_and(|b| s.now >= b + m.ttl)
                        && !s.delivered.iter().any(|d| d.0 as usize == c)
                })
            }),
            Property::sometimes("delivered_after_reboot", |_, s: &State| {
                s.delivered.iter().any(|d| s.pre_crash & (1 << d.0) != 0)
            }),
            Property::sometimes("delivered_from_notification", |_, s: &State| {
                s.delivered.iter().any(|d| d.2 == Src::Notification)
            }),
        ]
    }
}

/// Sorted insert that keeps duplicates (a multiset).
fn set_insert_multi<T: Ord>(v: &mut Vec<T>, x: T) {
    let i = v.partition_point(|y| *y <= x);
    v.insert(i, x);
}

pub const PROTOCOL: &str = "otp";

pub fn cases(deep: bool) -> Vec<Case> {
    use Expect::*;
    use Src::*;
    // The deployed configuration, the code as it is now (branch `otp-fixes`),
    // no environment events: each case below turns on the events it needs.
    let now = Otp {
        // Code 0: an SMS and its notification; code 1: an SMS.
        copies: vec![vec![Sms, Notification], vec![Sms]],
        ttl: 2,
        max_time: 3,
        continuous: true,
        window: 2,
        relay_limit: 2,
        queue_limit: 2,
        drops: 0,
        crashes: 0,
        restarts: 0,
        seen_failures: 0,
        takes: 2,
        threads: 1,
        stops: 0,
        arms: 0,
        tests: 0,
        others: 0,
        write_ahead: true,
        arm_keeps_codes: true,
        persist_seen: true,
        owner_guard: true,
        lucina_expiry_check: true,
        relay_dedup: true,
    };
    // Regression witnesses: the code just before each fix commit.
    // 60472d9 (relay F2, F3) landed before ac9e7be (phone F1).
    let pre_ac9e7be = Otp {
        write_ahead: false,
        ..now.clone()
    };
    let pre_60472d9 = Otp {
        write_ahead: false,
        arm_keeps_codes: false,
        persist_seen: false,
        ..now.clone()
    };
    let expect = |loss, once, expiry, witnesses: &[&'static str]| {
        let mut v = vec![
            ("at_most_once", once),
            ("never_after_expiry", expiry),
            ("owner_only", Holds),
            ("test_never_delivered", Holds),
            ("no_silent_loss", loss),
            ("pending_bounded", Holds),
        ];
        v.extend(witnesses.iter().map(|w| (*w, Reached)));
        v
    };
    let all_witnesses = [
        "retry_deduplicated",
        "code_expired_undelivered",
        "delivered_after_reboot",
        "delivered_from_notification",
    ];
    let reboot = Otp {
        crashes: 1,
        ..now.clone()
    };
    let arm = Otp {
        arms: 1,
        others: 1,
        ..now.clone()
    };
    let race = Otp {
        stops: 1,
        threads: 2,
        ..now.clone()
    };
    let restart = Otp {
        drops: 1,
        restarts: 1,
        seen_failures: 1,
        ..now.clone()
    };
    let mut cases = vec![
        Case::new(
            PROTOCOL,
            "OTP1 phone reboot during the first send",
            reboot.clone(),
            None,
            expect(
                Holds,
                Holds,
                Holds,
                &["delivered_after_reboot", "delivered_from_notification"],
            ),
        ),
        Case::new(
            PROTOCOL,
            "OTP1_bug_pre_ac9e7be phone reboot during the first send",
            pre_ac9e7be.with(&reboot),
            None,
            expect(
                Violated,
                Violated,
                Violated,
                &["delivered_after_reboot", "delivered_from_notification"],
            ),
        ),
        Case::new(
            PROTOCOL,
            "OTP2 /otp arm while a code waits; an MCP /otp",
            arm.clone(),
            None,
            expect(Holds, Holds, Holds, &["code_expired_undelivered"]),
        ),
        Case::new(
            PROTOCOL,
            "OTP2_bug_pre_60472d9 /otp arm while a code waits",
            pre_60472d9.with(&arm),
            None,
            expect(Violated, Holds, Holds, &["code_expired_undelivered"]),
        ),
        Case::new(
            PROTOCOL,
            "OTP3 two /otp after a pause (auto-arm race)",
            race.clone(),
            None,
            expect(Holds, Holds, Holds, &["code_expired_undelivered"]),
        ),
        Case::new(
            PROTOCOL,
            "OTP3_bug_pre_60472d9 two /otp after a pause",
            pre_60472d9.with(&race),
            None,
            expect(Violated, Holds, Holds, &["code_expired_undelivered"]),
        ),
        Case::new(
            PROTOCOL,
            "OTP4 relay restart, a failed seen.json write (503)",
            restart.clone(),
            None,
            expect(Holds, Holds, Holds, &["retry_deduplicated"]),
        ),
        Case::new(
            PROTOCOL,
            "OTP4_bug_pre_60472d9 relay restart forgets seen ids",
            pre_60472d9.with(&restart),
            None,
            expect(Holds, Violated, Holds, &["retry_deduplicated"]),
        ),
        Case::new(
            PROTOCOL,
            "OTP5 drops, reboot, relay restart",
            Otp {
                drops: 1,
                crashes: 1,
                restarts: 1,
                ..now.clone()
            },
            None,
            expect(Holds, Holds, Holds, &all_witnesses),
        ),
        Case::new(
            PROTOCOL,
            "OTP6 pause/arm, two /otp, self-test",
            Otp {
                stops: 1,
                arms: 1,
                threads: 2,
                tests: 1,
                ..now.clone()
            },
            None,
            expect(
                Holds,
                Holds,
                Holds,
                &["code_expired_undelivered", "delivered_from_notification"],
            ),
        ),
        Case::new(
            PROTOCOL,
            "OTP7 timed windows (not continuous): drops, reboot, restart",
            Otp {
                continuous: false,
                drops: 1,
                crashes: 1,
                restarts: 1,
                ..now.clone()
            },
            None,
            expect(Holds, Holds, Holds, &all_witnesses),
        ),
        // Ablations: each switches off one mechanism of the code.
        Case::new(
            PROTOCOL,
            "OTP8 ablation: no private-owner guard",
            Otp {
                owner_guard: false,
                others: 1,
                ..now.clone()
            },
            None,
            vec![("owner_only", Violated), ("at_most_once", Holds)],
        ),
        Case::new(
            PROTOCOL,
            "OTP9 ablation: Lucina skips its expiry check",
            Otp {
                lucina_expiry_check: false,
                ..now.clone()
            },
            None,
            vec![("never_after_expiry", Violated), ("at_most_once", Holds)],
        ),
        Case::new(
            PROTOCOL,
            "OTP10 ablation: relay without seen event ids",
            Otp {
                relay_dedup: false,
                drops: 1,
                ..now.clone()
            },
            None,
            vec![("at_most_once", Violated), ("no_silent_loss", Holds)],
        ),
    ];
    if deep {
        // Everything at once: about 10^8 states.
        cases.push(Case::new(
            PROTOCOL,
            "OTP11 every environment event (deep)",
            Otp {
                drops: 1,
                crashes: 1,
                restarts: 1,
                seen_failures: 1,
                stops: 1,
                arms: 1,
                tests: 1,
                others: 1,
                ..now
            },
            None,
            expect(Holds, Holds, Holds, &all_witnesses),
        ));
    }
    cases
}

impl Otp {
    /// `self`'s code switches with `env`'s environment events.
    fn with(&self, env: &Otp) -> Otp {
        Otp {
            write_ahead: self.write_ahead,
            arm_keeps_codes: self.arm_keeps_codes,
            persist_seen: self.persist_seen,
            owner_guard: self.owner_guard,
            lucina_expiry_check: self.lucina_expiry_check,
            relay_dedup: self.relay_dedup,
            ..env.clone()
        }
    }
}
