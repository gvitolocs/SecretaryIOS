//! Protocol 15: Node retirement. Rust owns Telegram, Discord and reminders, one capability
//! at a time, with no Node code change (docs/verification.md §15).
//!
//! Code (branch `node-retire`, 3c22447): `crates/lucina-store/src/reminders.rs::process_due`
//! (+ `fail_stale_claims`, `status_at_tick_start`), `crates/lucina-telegram/src/outcome.rs::classify`,
//! `lib.rs::BotApi::{call, transport_error, send_text}`, `lucina-server/src/reminder_delivery.rs`,
//! `crates/lucina-telegram/src/{lease,lease_decision}.rs`. Node: `Lucina/src/gateway-lock.js`,
//! `src/index.js::launchManagedTelegramPolling` + `shutdown`, telegraf 4.16.3 `Polling`.
//! Update poller: `claim.rs` + `poller.rs` (f26eb90): the batch's claim is written before any
//! handler, `should_handle = id > claimed`, `claim = max`, and every `getUpdates` (also the
//! first after a start) sends `next_offset(claimed) = claimed + 1`. Not written yet, modeled
//! from the design: the Discord `discord-<appid>.flock` and the handover scripts.
//!
//! Durable state (files, Telegram's queue) survives a watchdog crash; volatile work and flocks
//! do not. Every model allows one Rust crash at any step. Completion checks use a drained
//! state (bounded faults, continued scheduling), not an assumption that an unavailable network
//! eventually delivers.
use crate::{Case, Expect};
use stateright::{Model, Property};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Status {
    Absent,
    Pending,
    Sending,
    Sent,
    Failed,
    Cancelled,
}

// ---------------------------------------------------------------------------------------
// R1: reminders handover (Node disabled while idle, then Rust writer=local + scheduler).
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Work {
    Idle,
    Create,
    Cancel,
    NodeRead,
    NodeSent,
    Claim,
    Sent,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ReminderState {
    /// Node runs with `LUCINA_REMINDERS_ENABLED=true` (scheduler + create/cancel).
    node: bool,
    /// Rust runs with `LUCINA_REMINDERS_WRITER=local` and `LUCINA_SCHEDULER=true`.
    local: bool,
    /// The lucina-rs process is up (the watchdog restarts it after a crash).
    up: bool,
    /// Operator step: 0 disable Node, 1 Rust local, 2 rollback Rust to writer=node, 3 enable Node.
    pos: u8,
    crash: bool,
    /// `reminders.json`: reminder 0 is due at the start, reminder 1 is the app/Telegram create.
    file: [Status; 2],
    /// Each runtime's read-modify-write copy (index 0 Node, 1 Rust).
    snap: [[Status; 2]; 2],
    work: [Work; 2],
    accepted: bool,
    requested: bool,
    disabled_visible: bool,
    lost: bool,
    deliveries: [u8; 2],
    settled: bool,
    handed: bool,
}
#[derive(Clone, Debug)]
pub struct Reminders {
    /// Witness: Rust becomes the local writer before Node is disabled.
    pub early: bool,
    /// Node is restarted (disabled / re-enabled) only while its tick and requests are idle.
    pub idle: bool,
    /// Rollback waits until Rust is idle and has resolved every `sending` claim.
    pub drain_rollback: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderAction {
    Operator,
    Crash,
    Restart,
    CreateApp,
    CreateTelegram,
    CancelApp,
    CancelTelegram,
    Read(usize),
    Send(usize),
    Save(usize),
    Settle,
}
impl Model for Reminders {
    type State = ReminderState;
    type Action = ReminderAction;
    fn init_states(&self) -> Vec<Self::State> {
        vec![ReminderState {
            node: true,
            local: false,
            up: true,
            pos: 0,
            crash: false,
            file: [Status::Pending, Status::Absent],
            snap: [[Status::Absent; 2]; 2],
            work: [Work::Idle; 2],
            accepted: false,
            requested: false,
            disabled_visible: false,
            lost: false,
            deliveries: [0; 2],
            settled: false,
            handed: false,
        }]
    }
    fn actions(&self, s: &Self::State, a: &mut Vec<Self::Action>) {
        use ReminderAction::*;
        use Work::*;
        if s.settled {
            return;
        }
        if s.pos < 4
            && (!(s.pos == 0 || s.pos == 3) || !self.idle || s.work[0] == Idle)
            && (s.pos != 2
                || !self.drain_rollback
                || (s.work[1] == Idle && !s.file.contains(&Status::Sending)))
        {
            a.push(Operator);
        }
        if s.up && !s.crash {
            a.push(Crash);
        }
        if !s.up {
            a.push(Restart);
        }
        if !s.requested {
            a.extend([CreateApp, CreateTelegram]);
        }
        if s.file[1] == Status::Pending {
            a.extend([CancelApp, CancelTelegram]);
        }
        for i in 0..2 {
            let enabled = if i == 0 { s.node } else { s.local && s.up };
            if !enabled {
                continue;
            }
            match s.work[i] {
                // Node's tick runs when something is pending; Rust's process_due also runs
                // to recover stale `sending` claims (fail_stale_claims).
                Idle if s
                    .file
                    .iter()
                    .any(|x| *x == Status::Pending || (i == 1 && *x == Status::Sending))
                    && s.deliveries.iter().all(|n| *n < 2) =>
                {
                    a.push(Read(i))
                }
                NodeRead | Claim => a.push(Send(i)),
                Create | Cancel | NodeSent | Sent => a.push(Save(i)),
                _ => (),
            }
        }
        if s.pos == 4
            && s.up
            && s.work == [Idle; 2]
            && s.requested
            && !s
                .file
                .iter()
                .any(|x| *x == Status::Pending || (s.local && *x == Status::Sending))
        {
            a.push(Settle);
        }
    }
    fn next_state(&self, old: &Self::State, act: Self::Action) -> Option<Self::State> {
        use ReminderAction::*;
        use Status::*;
        use Work::*;
        let mut s = old.clone();
        match act {
            Operator => {
                // Forward: disable idle Node, then local Rust. Rollback reverses ownership.
                let phase = if self.early && s.pos < 2 {
                    1 - s.pos
                } else {
                    s.pos
                };
                match phase {
                    0 => {
                        // Node restart with LUCINA_REMINDERS_ENABLED=false: in-flight work dies.
                        s.node = false;
                        s.work[0] = Idle;
                    }
                    1 => {
                        s.local = true;
                        s.up = true;
                        s.handed = true;
                    }
                    2 => {
                        // Rust restart with writer=node, scheduler off: in-flight work dies.
                        s.local = false;
                        s.work[1] = Idle;
                    }
                    _ => {
                        s.node = true;
                    }
                }
                s.pos += 1;
            }
            Crash => {
                s.up = false;
                s.crash = true;
                s.work[1] = Idle;
            }
            Restart => {
                s.up = true;
            }
            CreateApp | CreateTelegram | CancelApp | CancelTelegram => {
                // App requests go to Rust once it is the local writer (before that Rust
                // forwards them to Node, writer=node); Telegram requests reach Node.
                let cancel = matches!(act, CancelApp | CancelTelegram);
                let i = if matches!(act, CreateApp | CancelApp) && s.local {
                    1
                } else {
                    0
                };
                if !(if i == 0 { s.node } else { s.up }) {
                    // "reminders disabled" / Rust down: the request fails visibly.
                    s.disabled_visible = true;
                    if !cancel {
                        s.requested = true;
                    }
                } else {
                    if s.work[i] != Idle {
                        return None;
                    }
                    s.snap[i] = s.file;
                    s.work[i] = if cancel { Cancel } else { Create };
                    if !cancel {
                        s.requested = true;
                    }
                }
            }
            Read(i) => {
                if i == 1 {
                    // process_due under the mutex + flock: fail_stale_claims, then claim
                    // every due pending reminder and write the claim before any send.
                    for st in &mut s.file {
                        if *st == Sending {
                            *st = Failed;
                        }
                    }
                    for st in &mut s.file {
                        if *st == Pending {
                            *st = Sending;
                        }
                    }
                    s.work[i] = Claim;
                } else {
                    // Node's processDueReminders: snapshot, send, then commit (P2b).
                    s.work[i] = NodeRead;
                }
                s.snap[i] = s.file;
            }
            Send(i) => {
                // Node sends what it read as `pending` (it has no `sending` status); Rust sends
                // exactly the records it claimed. R1 assumes delivery (R2 covers outcomes).
                let want = if i == 0 { Pending } else { Sending };
                for r in 0..2 {
                    if s.snap[i][r] == want {
                        s.deliveries[r] += 1;
                        s.snap[i][r] = Status::Sent;
                    }
                }
                s.work[i] = if i == 0 { NodeSent } else { Work::Sent };
            }
            Save(i) => {
                if s.work[i] == Create {
                    s.snap[i][1] = Pending;
                    s.accepted = true;
                }
                if s.work[i] == Cancel {
                    s.snap[i][1] = Cancelled;
                }
                for r in 0..2 {
                    if s.file[r] != Absent && s.snap[i][r] == Absent {
                        s.lost = true;
                    }
                }
                s.file = s.snap[i];
                s.work[i] = Idle;
            }
            Settle => s.settled = true,
        }
        Some(s)
    }
    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("single_reminders_writer", |_, s: &ReminderState| {
                !(s.node && s.local && s.up)
            }),
            Property::always("no_lost_reminder_record", |_, s: &ReminderState| {
                !s.lost && (!s.accepted || s.file[1] != Status::Absent)
            }),
            Property::always("no_double_delivery", |_, s: &ReminderState| {
                s.deliveries.iter().all(|n| *n <= 1)
            }),
            Property::always("no_silent_loss", |_, s: &ReminderState| {
                !s.settled
                    || (0..2).all(|r| {
                        (r == 1 && !s.accepted)
                            || s.deliveries[r] > 0
                            || matches!(s.file[r], Status::Failed | Status::Cancelled)
                    })
            }),
            Property::sometimes("handed_over", |_, s: &ReminderState| {
                s.handed && s.local && !s.node && s.deliveries[0] > 0
            }),
            Property::sometimes("disabled_visible", |_, s: &ReminderState| {
                s.disabled_visible
            }),
            Property::sometimes("rolled_back", |_, s: &ReminderState| {
                s.settled && s.node && !s.local
            }),
        ]
    }
}

// ---------------------------------------------------------------------------------------
// R2: delivery outcome (classify + transport + process_due status rule).
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stage {
    Connect,
    Sent,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    Delivered,
    NotDelivered,
    Uncertain,
}

/// Mirror of `crates/lucina-telegram/src/outcome.rs::classify` (same branch order; also
/// mirrored and proved in `verification/verus/retirement_kernels.rs`).
pub fn classify(stage: Stage, http_status: Option<u16>, api_ok: Option<bool>) -> Outcome {
    if api_ok == Some(true) {
        Outcome::Delivered
    } else if api_ok == Some(false)
        || http_status.is_some_and(|s| s >= 400)
        || stage == Stage::Connect
    {
        Outcome::NotDelivered
    } else if http_status.is_none() {
        Outcome::Uncertain
    } else {
        Outcome::NotDelivered
    }
}

/// What the network does with one `sendMessage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Network {
    /// Connection refused / DNS / connect timeout: the request never left.
    Refuse,
    /// A complete error response (`{"ok":false}`, e.g. 429 or 5xx).
    Error,
    /// Delivered, `{"ok":true}` received.
    Reply,
    /// Delivered, 200 headers received, then the body read is reset.
    BodyLost,
    /// Delivered, then the reply is lost (timeout).
    DeliveredTimeout,
    /// The request is lost after it was sent (timeout).
    LostTimeout,
}
impl Network {
    pub const ALL: [Network; 6] = [
        Network::Refuse,
        Network::Error,
        Network::Reply,
        Network::BodyLost,
        Network::DeliveredTimeout,
        Network::LostTimeout,
    ];
    /// Telegram accepted the message (the owner sees it).
    pub fn delivered(self) -> bool {
        matches!(
            self,
            Network::Reply | Network::BodyLost | Network::DeliveredTimeout
        )
    }
    /// Mirror of `BotApi::call` / `transport_error` for one part, then
    /// `reminder_delivery.rs` (any error that is not NotDelivered becomes Uncertain).
    pub fn outcome(self) -> Outcome {
        let err = match self {
            Network::Reply => return Outcome::Delivered, // envelope.ok
            // transport_error(e, None) with e.is_connect()
            Network::Refuse => classify(Stage::Connect, None, None),
            // envelope.ok == false
            Network::Error => classify(Stage::Sent, Some(429), Some(false)),
            // transport_error(e, Some(200)) on bytes(): status.filter(|s| *s >= 400)
            Network::BodyLost => classify(Stage::Sent, Some(200).filter(|s| *s >= 400), None),
            // transport_error(e, None), e.is_timeout(), not a connect error
            Network::DeliveredTimeout | Network::LostTimeout => classify(Stage::Sent, None, None),
        };
        match err {
            Outcome::NotDelivered => Outcome::NotDelivered,
            _ => Outcome::Uncertain,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Delivery {
    /// Witness: an Uncertain send goes back to `pending` (retried).
    pub retry_uncertain: bool,
    /// Ablation: a NotDelivered send becomes `failed` (never retried).
    pub fail_known: bool,
}
impl Delivery {
    /// `process_due`'s match on the outcome of a claimed (`sending`) reminder.
    fn next_status(&self, o: Outcome) -> Status {
        match o {
            Outcome::Delivered => Status::Sent,
            Outcome::NotDelivered if self.fail_known => Status::Failed,
            Outcome::NotDelivered => Status::Pending,
            Outcome::Uncertain if self.retry_uncertain => Status::Pending,
            Outcome::Uncertain => Status::Failed,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DeliveryState {
    status: Status,
    /// 0 idle, 1 claimed on disk (sending), 2 the send returned (outcome in memory).
    phase: u8,
    sends: u8,
    received: u8,
    crash: bool,
    up: bool,
    outcome: Option<Network>,
    visible: bool,
    known_failed: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryAction {
    Claim,
    Send(Network),
    Save,
    Crash,
    Restart,
    Recover,
}
impl Model for Delivery {
    type State = DeliveryState;
    type Action = DeliveryAction;
    fn init_states(&self) -> Vec<Self::State> {
        vec![DeliveryState {
            status: Status::Pending,
            phase: 0,
            sends: 0,
            received: 0,
            crash: false,
            up: true,
            outcome: None,
            visible: false,
            known_failed: false,
        }]
    }
    fn actions(&self, s: &Self::State, a: &mut Vec<Self::Action>) {
        use DeliveryAction::*;
        if s.up && !s.crash {
            a.push(Crash);
        }
        if !s.up {
            a.push(Restart);
            return;
        }
        if s.phase == 0 && s.status == Status::Sending {
            a.push(Recover);
        }
        if s.phase == 0 && s.status == Status::Pending && s.sends < 2 {
            a.push(Claim);
        }
        if s.phase == 1 {
            // One transient fault, then a healthy send: permits bounded progress.
            a.push(Send(Network::Reply));
            if s.sends == 0 {
                for n in Network::ALL {
                    if n != Network::Reply {
                        a.push(Send(n));
                    }
                }
            }
        }
        if s.phase == 2 {
            a.push(Save);
        }
    }
    fn next_state(&self, o: &Self::State, a: Self::Action) -> Option<Self::State> {
        use DeliveryAction::*;
        let mut s = o.clone();
        match a {
            Claim => {
                // The claim is on disk before anything is sent.
                s.status = Status::Sending;
                s.phase = 1;
            }
            Send(n) => {
                s.sends += 1;
                s.outcome = Some(n);
                s.phase = 2;
                if n.delivered() {
                    s.received += 1;
                }
            }
            Save => {
                let n = s.outcome?;
                s.status = self.next_status(n.outcome());
                s.known_failed = s.status == Status::Failed && n.outcome() == Outcome::NotDelivered;
                s.visible = s.status == Status::Failed;
                s.phase = 0;
                s.outcome = None;
            }
            Crash => {
                s.up = false;
                s.crash = true;
                s.phase = 0;
                s.outcome = None;
            }
            Restart => s.up = true,
            Recover => {
                // fail_stale_claims at the next tick: "interrupted; not resent".
                s.status = Status::Failed;
                s.visible = true;
            }
        }
        Some(s)
    }
    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("no_double_delivery", |_, s: &DeliveryState| s.received <= 1),
            Property::eventually("no_silent_loss", |_, s: &DeliveryState| {
                s.status == Status::Sent || (s.status == Status::Failed && s.visible)
            }),
            Property::sometimes("delivered", |_, s: &DeliveryState| s.status == Status::Sent),
            Property::sometimes("uncertain_failed", |_, s: &DeliveryState| {
                s.status == Status::Failed && s.received == 1
            }),
            Property::sometimes("retryable_failed", |_, s: &DeliveryState| s.known_failed),
        ]
    }
}

// ---------------------------------------------------------------------------------------
// R3: Telegram single poller (Rust flock + Node-compatible record + read-back; Node's
// record-only rendezvous; Telegram's 409).
// ---------------------------------------------------------------------------------------

/// Processes: 0 = Rust A, 1 = Rust B (a stray second lucina-rs), 2 = the Node container.
/// A and B share `HERMES_GATEWAY_INSTANCE` (`lucina-rs`), so `is_own_previous_run` makes
/// each acquire over the other's live record: only the flock separates them. Node's
/// instance is `lucina-nez`; the pid namespaces differ, so Node and Rust defer to each other's
/// live record. The boot id is fixed (one boot); the Verus kernel covers boot ids.
#[derive(Clone, Debug)]
pub struct Telegram {
    /// Rust takes `telegram-<bot>.flock` before the record (the code: true).
    pub flock: bool,
    /// A process may stall for more than 250 ms between reading the record and writing its
    /// own (heavy swap, a stopped container). False: read+write is one step.
    pub stall: bool,
    /// The record of a process that is still polling may get older than 90 s (its heartbeat
    /// stalls). False: only the record of a stopped/crashed writer ages.
    pub lapse: bool,
    /// Node starts polling without a lock when the lock dir is unusable (Node's code: true).
    pub node_degraded: bool,
    /// Ablation: Rust does the same instead of refusing to start.
    pub rust_degraded: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TelegramState {
    /// 0 idle, 1 read the record (stall only), 2 wrote its record and waits 250 ms, 3 polling.
    pc: [u8; 3],
    up: [bool; 2],
    /// The Rust process holding the flock.
    holder: Option<u8>,
    /// The process whose record `telegram-<bot>.lock` holds.
    record: Option<u8>,
    /// 30 s ticks since the record's heartbeat; 3 = older than 90 s (not live).
    age: u8,
    crash: bool,
    ticks: u8,
    dir: bool,
    conflict: bool,
    deferred: bool,
    takeover: bool,
    refused: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TelegramAction {
    Check(u8),
    Write(u8),
    ReadBack(u8),
    Heartbeat(u8),
    Tick,
    Crash(u8),
    Restart(u8),
    Conflict(u8),
    BreakDir,
}
impl TelegramState {
    fn live(&self) -> bool {
        self.record.is_some() && self.age < 3
    }
    /// `lease_decision(...) == Defer` (Rust) / `!acquired` (Node `acquireGatewayLock`).
    fn defers(&self, i: u8) -> bool {
        self.live()
            && match self.record {
                Some(2) => i < 2,  // Node's record: foreign for Rust
                Some(_) => i == 2, // a Rust record: foreign for Node, own instance for Rust
                None => false,
            }
    }
    fn polling(&self) -> usize {
        self.pc.iter().filter(|p| **p == 3).count()
    }
    fn stop(&mut self, i: u8) {
        self.pc[i as usize] = 0;
        if self.holder == Some(i) {
            self.holder = None;
        }
    }
}
impl Model for Telegram {
    type State = TelegramState;
    type Action = TelegramAction;
    fn init_states(&self) -> Vec<Self::State> {
        vec![TelegramState {
            pc: [0; 3],
            up: [true; 2],
            holder: None,
            record: None,
            age: 0,
            crash: false,
            ticks: 0,
            dir: true,
            conflict: false,
            deferred: false,
            takeover: false,
            refused: false,
        }]
    }
    fn actions(&self, s: &Self::State, a: &mut Vec<Self::Action>) {
        use TelegramAction::*;
        for i in 0..3u8 {
            let iu = i as usize;
            if i < 2 && !s.up[iu] {
                a.push(Restart(i));
                continue;
            }
            if i < 2 && !s.crash {
                a.push(Crash(i));
            }
            match s.pc[iu] {
                0 => a.push(Check(i)),
                1 => a.push(Write(i)),
                2 => a.push(ReadBack(i)),
                _ => a.push(Heartbeat(i)),
            }
            if s.pc[iu] == 3 && s.polling() > 1 && !s.conflict {
                a.push(Conflict(i));
            }
        }
        if s.dir {
            a.push(BreakDir);
        }
        if s.ticks < 3 {
            a.push(Tick);
        }
    }
    fn next_state(&self, o: &Self::State, a: Self::Action) -> Option<Self::State> {
        use TelegramAction::*;
        let mut s = o.clone();
        match a {
            Check(i) => {
                let iu = i as usize;
                if !s.dir {
                    // Rust: create_dir_all / open flock fail -> Err (refuses to start).
                    // Node: mkdirSync throws -> catch -> { acquired: true, degraded: true }.
                    let degraded = if i == 2 {
                        self.node_degraded
                    } else {
                        self.rust_degraded
                    };
                    if degraded {
                        s.pc[iu] = 3;
                    } else if i < 2 && !s.refused {
                        s.refused = true;
                    } else {
                        return None;
                    }
                    return Some(s);
                }
                if i < 2 && self.flock && s.holder.is_some_and(|h| h != i) {
                    return None; // try_lock_exclusive: WouldBlock -> Ok(None)
                }
                if s.defers(i) {
                    if i == 2 && !s.deferred {
                        s.deferred = true; // "Telegram polling deferred", retry in 60 s
                        return Some(s);
                    }
                    return None;
                }
                if i < 2 && s.live() {
                    s.takeover = true; // own previous run (same instance)
                }
                if i < 2 && self.flock {
                    s.holder = Some(i);
                }
                if self.stall {
                    s.pc[iu] = 1;
                } else {
                    s.record = Some(i);
                    s.age = 0;
                    s.pc[iu] = 2;
                }
            }
            Write(i) => {
                if !s.dir {
                    if i == 2 && self.node_degraded {
                        // writeRecord throws inside the try: Node starts degraded.
                        s.pc[2] = 3;
                    } else {
                        s.stop(i); // Rust: write_record(..)? -> Err
                    }
                } else {
                    s.record = Some(i);
                    s.age = 0;
                    s.pc[i as usize] = 2;
                }
            }
            ReadBack(i) => {
                // Both runtimes: wait 250 ms, read the record back, poll only if it is ours.
                if s.dir && s.record == Some(i) {
                    s.pc[i as usize] = 3;
                } else {
                    if i == 2 {
                        s.deferred = true;
                    }
                    s.stop(i);
                }
            }
            Heartbeat(i) => {
                let foreign_live = s.live() && s.record.is_some_and(|r| r != i);
                if !s.dir {
                    if i == 2 {
                        return None; // Node: "heartbeat failed" warning, keeps polling
                    }
                    s.stop(i); // Rust: write fails -> lost -> the poller stops
                } else if foreign_live {
                    s.stop(i); // both: "lease lost to another holder" -> stop polling
                } else {
                    s.record = Some(i);
                    s.age = 0;
                }
            }
            Tick => {
                s.ticks += 1;
                let active = s.record.is_some_and(|w| {
                    let wu = w as usize;
                    s.pc[wu] >= 2 && (w == 2 || s.up[wu])
                });
                if self.lapse || !active {
                    s.age = (s.age + 1).min(3);
                }
            }
            Crash(i) => {
                // The kernel releases the flock; the record stays on disk.
                s.up[i as usize] = false;
                s.crash = true;
                s.stop(i);
            }
            Restart(i) => s.up[i as usize] = true,
            BreakDir => s.dir = false,
            // Telegram terminates an overlapping getUpdates with 409. The overlap existed
            // before this response; a 409 cannot prove exclusion.
            Conflict(i) => {
                s.conflict = true;
                s.stop(i);
            }
        }
        Some(s)
    }
    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("single_poller", |_, s: &TelegramState| s.polling() <= 1),
            Property::sometimes("rust_polls", |_, s: &TelegramState| {
                s.pc[0] == 3 || s.pc[1] == 3
            }),
            Property::sometimes("node_deferred", |_, s: &TelegramState| s.deferred),
            Property::sometimes("rust_takeover", |_, s: &TelegramState| {
                s.takeover && (s.pc[0] == 3 || s.pc[1] == 3)
            }),
            Property::sometimes("rust_refused", |_, s: &TelegramState| s.refused),
            Property::sometimes("node_polls_after_rust", |_, s: &TelegramState| {
                s.pc[2] == 3 && s.crash
            }),
            Property::sometimes("telegram_409", |_, s: &TelegramState| s.conflict),
        ]
    }
}

// ---------------------------------------------------------------------------------------
// R4: update handling at most once (claim before handle; Node -> Rust; rollback).
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Updates {
    /// Rust persists the highest claimed update_id before handling (false: offset only).
    pub durable: bool,
    /// The first getUpdates after a start sends `offset = claimed + 1` (NR4; the code:
    /// poller.rs uses `claim::next_offset` on every request). False (proposed fix): the first
    /// request sends no offset, so Telegram re-delivers unconfirmed updates and the
    /// already-claimed ones are skipped with a log line (`skippedReplayed`).
    pub start_from_claim: bool,
    /// Node is stopped only while its Telegram handlers are idle (long-polling).
    pub node_idle_stop: bool,
    /// Rollback: Rust stops polling, Node starts and calls
    /// `deleteWebhook({ drop_pending_updates: true })` (index.js, after the lock).
    pub rollback: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UpdateState {
    /// Telegram: the lowest unconfirmed update_id (ids 1 and 2 are queued; 3 = none left).
    confirmed: u8,
    handled: [u8; 2],
    /// Rust logged "skipped already-claimed update" for this id.
    logged: [bool; 2],
    /// Node: 0 long-polling (handled ones confirmed), 1 handling `ncur`, 2 handled, offset not
    /// yet synced, 3 gone, 4 back after rollback.
    node: u8,
    ncur: u8,
    /// Rust: 0 not started, 1 up, 2 crashed, 3 stopped for rollback.
    rust: u8,
    phase: u8,
    current: u8,
    /// Next request offset (volatile; 0 = no offset parameter).
    offset: u8,
    /// Durable claim file (0 = none).
    claimed: u8,
    crash: bool,
    node_handled: bool,
    rust_handled: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateAction {
    NodeReceive,
    NodeHandle,
    NodeSync,
    NodeStop,
    RustStart,
    Receive,
    Handle,
    Advance,
    Crash,
    Restart,
    Rollback,
}
impl Updates {
    fn start_offset(&self, s: &UpdateState) -> u8 {
        if self.durable && self.start_from_claim && s.claimed > 0 {
            s.claimed + 1
        } else {
            0
        }
    }
}
impl Model for Updates {
    type State = UpdateState;
    type Action = UpdateAction;
    fn init_states(&self) -> Vec<Self::State> {
        vec![UpdateState {
            confirmed: 1,
            handled: [0; 2],
            logged: [false; 2],
            node: 0,
            ncur: 0,
            rust: 0,
            phase: 0,
            current: 0,
            offset: 0,
            claimed: 0,
            crash: false,
            node_handled: false,
            rust_handled: false,
        }]
    }
    fn actions(&self, s: &Self::State, a: &mut Vec<Self::Action>) {
        use UpdateAction::*;
        match s.node {
            0 if s.confirmed <= 2 => a.push(NodeReceive),
            1 => a.push(NodeHandle),
            2 => a.push(NodeSync),
            _ => (),
        }
        if s.node == 0 || (!self.node_idle_stop && s.node < 3) {
            a.push(NodeStop);
        }
        match s.rust {
            0 if s.node == 3 => a.push(RustStart),
            1 => {
                if !s.crash {
                    a.push(Crash);
                }
                match s.phase {
                    0 if s.offset.max(s.confirmed) <= 2 => a.push(Receive),
                    1 => a.push(Handle),
                    2 => a.push(Advance),
                    _ => (),
                }
                if self.rollback && s.phase == 0 {
                    a.push(Rollback);
                }
            }
            2 => a.push(Restart),
            _ => (),
        }
    }
    fn next_state(&self, o: &Self::State, a: Self::Action) -> Option<Self::State> {
        use UpdateAction::*;
        let mut s = o.clone();
        match a {
            NodeReceive => {
                s.ncur = s.confirmed;
                s.node = 1;
            }
            NodeHandle => {
                s.handled[(s.ncur - 1) as usize] += 1;
                s.node_handled = true;
                s.node = 2;
            }
            NodeSync => {
                // telegraf's next getUpdates sends offset = last.update_id + 1.
                s.confirmed = s.confirmed.max(s.ncur + 1);
                s.node = 0;
            }
            NodeStop => s.node = 3,
            RustStart => {
                s.rust = 1;
                s.offset = self.start_offset(&s);
            }
            Receive => {
                // getUpdates(offset) confirms every id < offset and returns the first
                // unconfirmed one.
                s.confirmed = s.confirmed.max(s.offset);
                s.current = s.confirmed;
                if self.durable && s.current <= s.claimed {
                    s.logged[(s.current - 1) as usize] = true; // !should_handle
                    s.phase = 2;
                } else {
                    if self.durable {
                        s.claimed = s.claimed.max(s.current); // claim, persisted atomically
                    }
                    s.phase = 1;
                }
            }
            Handle => {
                s.handled[(s.current - 1) as usize] += 1;
                s.rust_handled = true;
                s.phase = 2;
            }
            Advance => {
                s.offset = if self.durable {
                    s.claimed + 1
                } else {
                    s.current + 1
                };
                s.phase = 0;
            }
            Crash => {
                s.rust = 2;
                s.phase = 0;
                s.crash = true;
            }
            Restart => {
                s.rust = 1;
                s.offset = self.start_offset(&s);
            }
            Rollback => {
                s.rust = 3;
                s.node = 4;
                s.confirmed = 3; // drop_pending_updates: true
            }
        }
        Some(s)
    }
    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("no_double_reply", |_, s: &UpdateState| {
                s.handled.iter().all(|n| *n <= 1)
            }),
            // An update nobody handled, that Telegram will never deliver again, must be in the log.
            Property::always("no_silent_skip", |_, s: &UpdateState| {
                (0..2).all(|i| s.handled[i] > 0 || s.confirmed <= i as u8 + 1 || s.logged[i])
            }),
            Property::sometimes("all_handled", |_, s: &UpdateState| s.handled == [1, 1]),
            Property::sometimes("crash_skip_logged", |_, s: &UpdateState| {
                (0..2).any(|i| s.logged[i] && s.handled[i] == 0)
            }),
            Property::sometimes("node_then_rust", |_, s: &UpdateState| {
                s.node_handled && s.rust_handled
            }),
        ]
    }
}

// ---------------------------------------------------------------------------------------
// R5: Discord single message gateway (Node token emptied first; Rust app-id flock).
// ---------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Discord {
    /// Witness: Rust is enabled before Node's token is emptied.
    pub early: bool,
    /// Each Rust process holds `discord-<appid>.flock` for its message session.
    pub flock: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DiscordState {
    node: bool,
    enabled: bool,
    up: [bool; 2],
    sessions: [bool; 2],
    crash: bool,
    pos: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscordAction {
    Operator,
    Connect(usize),
    Crash(usize),
    Restart(usize),
}
impl Model for Discord {
    type State = DiscordState;
    type Action = DiscordAction;
    fn init_states(&self) -> Vec<Self::State> {
        vec![DiscordState {
            node: true,
            enabled: false,
            up: [true; 2],
            sessions: [false; 2],
            crash: false,
            pos: 0,
        }]
    }
    fn actions(&self, s: &Self::State, a: &mut Vec<Self::Action>) {
        use DiscordAction::*;
        if s.pos < 4 {
            a.push(Operator);
        }
        for i in 0..2 {
            if !s.up[i] {
                a.push(Restart(i));
            } else {
                if !s.crash {
                    a.push(Crash(i));
                }
                if s.enabled && !s.sessions[i] && (!self.flock || !s.sessions[1 - i]) {
                    a.push(Connect(i));
                }
            }
        }
    }
    fn next_state(&self, o: &Self::State, a: Self::Action) -> Option<Self::State> {
        use DiscordAction::*;
        let mut s = o.clone();
        match a {
            Operator => {
                let p = if self.early && s.pos < 2 {
                    1 - s.pos
                } else {
                    s.pos
                };
                match p {
                    // Node restarted with an empty Discord token; its session is gone.
                    0 => s.node = false,
                    1 => s.enabled = true,
                    // Rollback: Rust's Discord off first (sessions close), then Node.
                    2 => {
                        s.enabled = false;
                        s.sessions = [false; 2];
                    }
                    _ => s.node = true,
                }
                s.pos += 1;
            }
            Connect(i) => s.sessions[i] = true,
            Crash(i) => {
                s.up[i] = false;
                s.sessions[i] = false;
                s.crash = true;
            }
            Restart(i) => s.up[i] = true,
        }
        Some(s)
    }
    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("single_discord_gateway", |_, s: &DiscordState| {
                s.node as usize + s.sessions.iter().filter(|x| **x).count() <= 1
            }),
            Property::sometimes("rust_connects", |_, s: &DiscordState| {
                s.sessions.iter().any(|x| *x)
            }),
            Property::sometimes("rolled_back", |_, s: &DiscordState| {
                s.pos == 4 && s.node && !s.enabled
            }),
        ]
    }
}

pub fn cases(_: bool) -> Vec<Case> {
    use Expect::*;
    let hv = |bad: bool| if bad { Violated } else { Holds };
    let r = |name, m: Reminders, writer: bool, double: bool, loss: bool| {
        Case::new(
            "retirement",
            name,
            m,
            None,
            vec![
                ("single_reminders_writer", hv(writer)),
                ("no_lost_reminder_record", hv(writer)),
                ("no_double_delivery", hv(double)),
                ("no_silent_loss", hv(loss)),
                ("handed_over", Reached),
                ("disabled_visible", Reached),
                ("rolled_back", Reached),
            ],
        )
    };
    let d = |name, m: Delivery| {
        let (bad, known) = (m.retry_uncertain, m.fail_known);
        let mut e = vec![
            ("no_double_delivery", hv(bad)),
            ("no_silent_loss", Holds),
            ("delivered", Reached),
        ];
        if !bad {
            e.push(("uncertain_failed", Reached));
        }
        if known {
            e.push(("retryable_failed", Reached));
        }
        Case::new("retirement", name, m, None, e)
    };
    let t = |name, m: Telegram| {
        let fixed = m.flock && !m.stall && !m.lapse && !m.node_degraded && !m.rust_degraded;
        let mut e = vec![
            ("single_poller", hv(!fixed)),
            ("rust_polls", Reached),
            ("node_deferred", Reached),
        ];
        if fixed {
            e.extend([
                ("rust_takeover", Reached),
                ("rust_refused", Reached),
                ("node_polls_after_rust", Reached),
            ]);
        } else {
            e.push(("telegram_409", Reached));
        }
        Case::new("retirement", name, m, None, e)
    };
    let fixed_tg = Telegram {
        flock: true,
        stall: false,
        lapse: false,
        node_degraded: false,
        rust_degraded: false,
    };
    let u = |name, m: Updates| {
        let double = !m.durable || !m.node_idle_stop;
        let skip = (m.durable && m.start_from_claim) || m.rollback;
        let fixed = !double && !skip;
        let mut e = vec![
            ("no_double_reply", hv(double)),
            ("no_silent_skip", hv(skip)),
            ("all_handled", Reached),
        ];
        if fixed {
            e.extend([("crash_skip_logged", Reached), ("node_then_rust", Reached)]);
        }
        Case::new("retirement", name, m, None, e)
    };
    let fixed_up = Updates {
        durable: true,
        start_from_claim: false,
        node_idle_stop: true,
        rollback: false,
    };
    let g = |name, m: Discord, bad| {
        Case::new(
            "retirement",
            name,
            m,
            None,
            vec![
                ("single_discord_gateway", hv(bad)),
                ("rust_connects", Reached),
                ("rolled_back", Reached),
            ],
        )
    };
    vec![
        r(
            "R1 idle handover + drained Rust rollback",
            Reminders {
                early: false,
                idle: true,
                drain_rollback: true,
            },
            false,
            false,
            false,
        ),
        r(
            "R1 early local writer witness",
            Reminders {
                early: true,
                idle: true,
                drain_rollback: true,
            },
            true,
            true,
            true,
        ),
        r(
            "R1 mid-tick Node restart witness",
            Reminders {
                early: false,
                idle: false,
                drain_rollback: true,
            },
            false,
            true,
            false,
        ),
        r(
            "R1 rollback without drain (NR3)",
            Reminders {
                early: false,
                idle: true,
                drain_rollback: false,
            },
            false,
            false,
            true,
        ),
        d(
            "R2 three-way outcomes (classify + transport)",
            Delivery {
                retry_uncertain: false,
                fail_known: false,
            },
        ),
        d(
            "R2 uncertain retried witness",
            Delivery {
                retry_uncertain: true,
                fail_known: false,
            },
        ),
        d(
            "R2 not-delivered terminal ablation",
            Delivery {
                retry_uncertain: false,
                fail_known: true,
            },
        ),
        t("R3 flock + record + read-back (A1-A3)", fixed_tg.clone()),
        t(
            "R3 stall inside the rendezvous (NR1a)",
            Telegram {
                stall: true,
                ..fixed_tg.clone()
            },
        ),
        t(
            "R3 heartbeat lapse while polling (NR1b)",
            Telegram {
                lapse: true,
                ..fixed_tg.clone()
            },
        ),
        t(
            "R3 lock dir lost, Node starts degraded (NR1c)",
            Telegram {
                node_degraded: true,
                ..fixed_tg.clone()
            },
        ),
        t(
            "R3 record only, no flock witness",
            Telegram {
                flock: false,
                ..fixed_tg.clone()
            },
        ),
        t(
            "R3 Rust starts degraded witness",
            Telegram {
                rust_degraded: true,
                ..fixed_tg
            },
        ),
        u(
            "R4 claim-before-handle + no-offset restart (fix)",
            fixed_up.clone(),
        ),
        u(
            "R4 code: first getUpdates at claimed+1 (NR4)",
            Updates {
                start_from_claim: true,
                ..fixed_up.clone()
            },
        ),
        u(
            "R4 offset-only witness",
            Updates {
                durable: false,
                ..fixed_up.clone()
            },
        ),
        u(
            "R4 Node stopped mid-handler (NR5)",
            Updates {
                node_idle_stop: false,
                ..fixed_up.clone()
            },
        ),
        u(
            "R4 rollback drops pending updates (NR6)",
            Updates {
                rollback: true,
                ..fixed_up
            },
        ),
        g(
            "R5 ordered Discord + flock",
            Discord {
                early: false,
                flock: true,
            },
            false,
        ),
        g(
            "R5 early Discord witness",
            Discord {
                early: true,
                flock: true,
            },
            true,
        ),
        g(
            "R5 second Rust without flock witness",
            Discord {
                early: false,
                flock: false,
            },
            true,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The mirror agrees with the rows of outcome.rs's own unit test.
    #[test]
    fn classify_rows() {
        for stage in [Stage::Connect, Stage::Sent] {
            assert_eq!(classify(stage, Some(200), Some(true)), Outcome::Delivered);
            for status in [200, 400, 429, 500] {
                assert_eq!(
                    classify(stage, Some(status), Some(false)),
                    Outcome::NotDelivered
                );
            }
            for status in [400, 429, 500] {
                assert_eq!(classify(stage, Some(status), None), Outcome::NotDelivered);
            }
        }
        assert_eq!(classify(Stage::Connect, None, None), Outcome::NotDelivered);
        assert_eq!(classify(Stage::Sent, None, None), Outcome::Uncertain);
        // NR2: the code's success bit wins over the status.
        assert_eq!(
            classify(Stage::Sent, Some(500), Some(true)),
            Outcome::Delivered
        );
    }
    #[test]
    fn network_outcomes() {
        use Network::*;
        assert_eq!(Refuse.outcome(), Outcome::NotDelivered);
        assert_eq!(Error.outcome(), Outcome::NotDelivered);
        assert_eq!(Reply.outcome(), Outcome::Delivered);
        for n in [BodyLost, DeliveredTimeout, LostTimeout] {
            assert_eq!(n.outcome(), Outcome::Uncertain);
        }
        // Never NotDelivered when Telegram has the message: retrying cannot duplicate.
        for n in Network::ALL {
            assert!(!(n.delivered() && n.outcome() == Outcome::NotDelivered));
        }
    }
}
