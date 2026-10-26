//! The grocery deal watch (`crates/lucina-deals/src/watch.rs`, `schedule.rs`, `notify.rs`
//! on branch `lucina-rs`, pinned in `verification/verus/diff/deals-revision`) over two
//! local days, with the Node watcher before the cutover and secretary-server's
//! `POST /v1/notify` dedup. TLA+ twin: `formal/deals/DealWatch.tla`.
//!
//! The world:
//! - two shopping-list items; item 0 has two offers (offer 0 from Salling food waste,
//!   offer 1 from Tjek flyers: its best and an alternative), item 1 one offer (offer 2,
//!   Tjek). A price is 0 (no offer), 1 (low) or 2 (high); the sources change them
//!   (`Edit`, bounded) and each poll may lose either source (`Poll` mask).
//! - the clock: per day `0` digest window (07:30-09:00), `1` digest catch-up and the first
//!   alert tick (09:00-11:00), `2` later alert ticks (11:00-20:00); `t = 6` is the second
//!   evening. Nothing runs between 20:00 and 07:30, so the night is the step from `2` to
//!   the next day's `0`. Ticks and source edits happen between runs (see `actions`).
//! - a watcher run is `Load` (queue, captured date, read the state file, digest-day and
//!   alert-cap guards), `Poll` (`list_matches`), `Decide` (`compose`), `Send` (`deliver`:
//!   delivered, lost response, or server down), `Save` (`acknowledge` + atomic rename);
//!   a skipped digest saves its date (`SkipSave`). `Crash` kills a run at any step.
//! - the notify server: the dedup key is (kind, date, delivered best offers and prices)
//!   (`grocery-deals:<kind>:<date>:<sha256 of the delivered keys and prices>`); a key the
//!   server has seen is answered 200 `duplicate` without a notification. The model's
//!   server keeps every key: `Notifier::admit` checks the in-memory LRU (4096 keys) and
//!   the persisted inbox (500 notifications, read ones evicted first, never deleted by an
//!   API), and a few watch messages cannot evict anything.
//!
//! Switches: `node` (the Node watcher runs first, `StopNode` then `StartRust`), `overlap`
//! (both run at once), `dedup` (server dedup on), and the fixes `fix_pending` (F1: write
//! the composed message to the state file before sending; a run that finds it re-sends it
//! with the same key, then acknowledges it), `fix_ack_delivered` (F2: acknowledge only the
//! delivered best offers) and `fix_min_price` (F3: a notified price never goes up). The
//! code has all three since the commit in `deals-revision` (bugs DW1-DW3 fixed), so every
//! case runs with them; `pre_fix` is the dd7ec9e watcher, whose DW1-DW3 traces the
//! `pre_fix_traces` tests replay.
//! The code continues a run after a delivered re-send (`watch.rs::DealWatch::run`); the
//! model ends it and lets the next `Load` continue, which includes the code's order.

use stateright::{Model, Property};

use crate::{Case, Expect};

const PROTOCOL: &str = "deals";
/// Offer -> source (0 Salling food waste, 1 Tjek flyers); offers 0 and 1 are item 0, offer 2 item 1.
const SOURCE: [usize; 3] = [0, 1, 1];
/// The sources at the start: item 0 has the cheap food-waste offer and a dearer flyer
/// alternative, item 1 has no offer.
const INIT: [u8; 3] = [1, 2, 0];
/// `GROCERY_DEALS_ALERT_MAX_PER_DAY`.
const CAP: u8 = 4;
/// The last clock value (second evening).
const END: u8 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Digest,
    Alert,
}

/// Per item the delivered best offer `(offer, price)`; price 0 = the item is not in it.
pub type Batch = [(u8, u8); 2];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Msg {
    pub kind: Kind,
    pub date: u8,
    pub batch: Batch,
    /// The run's observed active offers (what `acknowledge` and the fingerprint use).
    pub obs: [u8; 3],
}

impl Msg {
    fn key(&self) -> (Kind, u8, Batch) {
        (self.kind, self.date, self.batch)
    }
}

/// `data/grocery/deal-watch.json` (both runtimes, same format).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct File {
    /// `notified["<rule>|<key>"].price` per offer, 0 = absent.
    pub notified: [u8; 3],
    /// `lastDigestDate`: 0 = null, else date + 1.
    pub digest_date: u8,
    /// `digestFingerprint`.
    pub fp: Option<[u8; 3]>,
    /// `alertCounts` per date.
    pub counts: [u8; 2],
    /// F1 only: the message being sent.
    pub pending: Option<Msg>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Phase {
    #[default]
    Idle,
    Loaded,
    Polled,
    /// composed; F1 must write `pending` first
    Composed,
    Ready,
    /// delivered, `acknowledge` + save next
    Sent,
    /// a skipped digest records the day
    SkipSave,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Run {
    pub phase: Phase,
    pub kind: Kind,
    pub date: u8,
    pub snap: File,
    pub obs: [u8; 3],
    pub failed: bool,
    /// false for an F1 re-send run (it does not poll)
    pub polled: bool,
    pub msg: Option<Msg>,
}

impl Default for Run {
    fn default() -> Self {
        Run {
            phase: Phase::Idle,
            kind: Kind::Digest,
            date: 0,
            snap: File::default(),
            obs: [0; 3],
            failed: false,
            polled: false,
            msg: None,
        }
    }
}

pub const NODE: usize = 0;
pub const RUST: usize = 1;

// ghost violation flags
pub const BAD_REPEAT: u8 = 1;
pub const BAD_DIGEST2: u8 = 2;
pub const BAD_SILENT: u8 = 4;
pub const BAD_SKIP: u8 = 8;
pub const BAD_ACK: u8 = 16;
// ghost witness flags
pub const SEEN_NEW_ALERT: u8 = 1;
pub const SEEN_GAP: u8 = 2;
pub const SEEN_RESUMED: u8 = 4;
pub const SEEN_CUTOVER: u8 = 8;
pub const SEEN_RELIST: u8 = 16;
pub const SEEN_NODE_LOAD: u8 = 32;
pub const SEEN_RUST_LOAD: u8 = 64;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub t: u8,
    pub src: [u8; 3],
    pub edits: u8,
    pub faults: u8,
    pub runs: u8,
    pub file: File,
    pub run: [Run; 2],
    pub up: [bool; 2],
    pub node_stopped: bool,
    /// Keys the notify server has admitted (sorted).
    pub server: Vec<(Kind, u8, Batch)>,
    // ---- ghosts: what the owner received and the property bookkeeping
    /// Per offer, bit p set when the owner received it at price p.
    pub recv: [u8; 3],
    /// Digests received per date (saturating at 2).
    pub digests: [u8; 2],
    pub last_digest_obs: Option<[u8; 3]>,
    pub last_obs: Option<[u8; 3]>,
    /// Alert ticks since the item's best offer last changed (saturating at 2).
    pub since: [u8; 2],
    pub bad: u8,
    pub seen: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    Lost,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Tick,
    Edit(u8, u8),
    Load(usize, Kind),
    /// actor, bit 0 = food waste failed, bit 1 = flyers failed
    Poll(usize, u8),
    Decide(usize),
    Claim(usize),
    Send(usize, Outcome),
    Save(usize),
    SkipSave(usize),
    Crash(usize),
    StopNode,
    StartRust,
}

#[derive(Clone, Debug)]
pub struct Deals {
    pub node: bool,
    pub overlap: bool,
    pub dedup: bool,
    pub fix_pending: bool,
    pub fix_ack_delivered: bool,
    pub fix_min_price: bool,
    pub max_runs: u8,
    pub max_edits: u8,
    pub max_faults: u8,
}

/// The item's best active offer in `v` (`group_deals` sorts by price; a tie keeps the
/// food-waste offer first).
pub fn best(v: &[u8; 3], item: usize) -> Option<(u8, u8)> {
    if item == 0 {
        match (v[0], v[1]) {
            (0, 0) => None,
            (a, b) if a > 0 && (b == 0 || a <= b) => Some((0, a)),
            (_, b) => Some((1, b)),
        }
    } else if v[2] > 0 {
        Some((2, v[2]))
    } else {
        None
    }
}

fn day(t: u8) -> u8 {
    t / 3
}

fn slot(t: u8) -> u8 {
    t % 3
}

fn bit(p: u8) -> u8 {
    1 << p
}

impl Deals {
    fn kind_due(&self, t: u8, kind: Kind) -> bool {
        t < END
            && match kind {
                Kind::Digest => matches!(slot(t), 0 | 1),
                Kind::Alert => matches!(slot(t), 1 | 2),
            }
    }

    /// `compose`: `None` = skip.
    fn compose(&self, r: &Run) -> Option<Msg> {
        let obs = r.obs;
        if obs == [0; 3] {
            return None; // no active matches
        }
        if r.kind == Kind::Digest && r.snap.fp == Some(obs) {
            return None; // unchanged digest
        }
        let mut batch: Batch = [(0, 0); 2];
        for (i, slot) in batch.iter_mut().enumerate() {
            if let Some((o, p)) = best(&obs, i) {
                let old = r.snap.notified[o as usize];
                if r.kind == Kind::Digest || old == 0 || old > p {
                    *slot = (o, p);
                }
            }
        }
        if batch.iter().all(|(_, p)| *p == 0) {
            return None; // no new or cheaper matches
        }
        Some(Msg {
            kind: r.kind,
            date: r.date,
            batch,
            obs,
        })
    }

    /// `acknowledge` (and F1: clear `pending`).
    fn ack(&self, f: &File, m: &Msg) -> File {
        let mut n = *f;
        let mut set = |o: usize, p: u8| {
            n.notified[o] = if self.fix_min_price && n.notified[o] != 0 {
                n.notified[o].min(p)
            } else {
                p
            };
        };
        if self.fix_ack_delivered {
            for &(o, p) in &m.batch {
                if p > 0 {
                    set(o as usize, p);
                }
            }
        } else {
            for (o, &p) in m.obs.iter().enumerate() {
                if p > 0 {
                    set(o, p);
                }
            }
        }
        match m.kind {
            Kind::Digest => {
                n.digest_date = m.date + 1;
                n.fp = Some(m.obs);
            }
            Kind::Alert => {
                n.counts[m.date as usize] += 1;
                for d in 0..m.date as usize {
                    n.counts[d] = 0;
                }
            }
        }
        n.pending = None;
        n
    }

    /// no_skip at the end of a successful alert run: an item whose best offer has been
    /// the same since the previous alert tick, and was observed, has reached the owner.
    fn check_no_skip(&self, s: &mut State, r: &Run) {
        if r.kind != Kind::Alert || !r.polled || r.failed {
            return;
        }
        for i in 0..2 {
            if let Some((o, p)) = best(&s.src, i) {
                let mask = (bit(p + 1) - 1) & !1; // prices 1..=p
                if s.since[i] >= 2
                    && best(&r.obs, i) == Some((o, p))
                    && s.recv[o as usize] & mask == 0
                {
                    s.bad |= BAD_SKIP;
                }
            }
        }
    }

    fn admit(&self, s: &mut State, m: &Msg) {
        for &(o, p) in &m.batch {
            if p == 0 {
                continue;
            }
            let have = s.recv[o as usize] & bit(p) != 0;
            match m.kind {
                Kind::Alert if have => s.bad |= BAD_REPEAT,
                Kind::Digest if have => s.seen |= SEEN_RELIST,
                _ => {}
            }
            if m.kind == Kind::Alert && INIT[o as usize] != p {
                s.seen |= SEEN_NEW_ALERT;
            }
            s.recv[o as usize] |= bit(p);
        }
        match m.kind {
            Kind::Digest => {
                let d = &mut s.digests[m.date as usize];
                *d = (*d + 1).min(2);
                if *d > 1 {
                    s.bad |= BAD_DIGEST2;
                }
                if s.last_digest_obs == Some(m.obs) {
                    s.bad |= BAD_SILENT;
                }
                s.last_digest_obs = Some(m.obs);
            }
            Kind::Alert => {
                if s.last_obs == Some(m.obs) {
                    s.bad |= BAD_SILENT;
                }
            }
        }
        s.last_obs = Some(m.obs);
    }
}

impl Model for Deals {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            t: 0,
            src: INIT,
            edits: 0,
            faults: 0,
            runs: 0,
            file: File::default(),
            run: [Run::default(); 2],
            up: [self.node || self.overlap, !self.node || self.overlap],
            node_stopped: false,
            server: Vec::new(),
            recv: [0; 3],
            digests: [0; 2],
            last_digest_obs: None,
            last_obs: None,
            since: [0; 2],
            bad: 0,
            seen: 0,
        }]
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        // The clock and the sources move between runs: a run takes seconds, a clock window
        // hours, and a run reads the sources only at `Poll` (an edit during a run is the same
        // as one just before its poll or just after it ends).
        let quiet = s.run.iter().all(|r| r.phase == Phase::Idle);
        if quiet && s.t < END {
            out.push(Action::Tick);
        }
        if quiet && s.edits < self.max_edits {
            for o in 0..3u8 {
                for p in 0..=2u8 {
                    if p != s.src[o as usize] {
                        out.push(Action::Edit(o, p));
                    }
                }
            }
        }
        if self.node && !self.overlap {
            if s.up[NODE] {
                out.push(Action::StopNode);
            } else if !s.up[RUST] {
                out.push(Action::StartRust);
            }
        }
        for a in [NODE, RUST] {
            if !s.up[a] {
                continue;
            }
            let r = &s.run[a];
            match r.phase {
                Phase::Idle => {
                    if s.runs < self.max_runs {
                        for kind in [Kind::Digest, Kind::Alert] {
                            if self.kind_due(s.t, kind) {
                                out.push(Action::Load(a, kind));
                            }
                        }
                    }
                }
                Phase::Loaded => {
                    out.push(Action::Poll(a, 0));
                    if s.faults < self.max_faults {
                        for mask in 1..4 {
                            out.push(Action::Poll(a, mask));
                        }
                    }
                }
                Phase::Polled => out.push(Action::Decide(a)),
                Phase::Composed => out.push(Action::Claim(a)),
                Phase::Ready => {
                    out.push(Action::Send(a, Outcome::Delivered));
                    if s.faults < self.max_faults {
                        out.push(Action::Send(a, Outcome::Down));
                        out.push(Action::Send(a, Outcome::Lost));
                    }
                }
                Phase::Sent => out.push(Action::Save(a)),
                Phase::SkipSave => out.push(Action::SkipSave(a)),
            }
            if r.phase != Phase::Idle && s.faults < self.max_faults {
                out.push(Action::Crash(a));
            }
        }
    }

    fn next_state(&self, s: &State, action: Action) -> Option<State> {
        let mut n = s.clone();
        match action {
            Action::Tick => {
                n.t += 1;
                if n.t < END && matches!(slot(n.t), 1 | 2) {
                    for i in 0..2 {
                        if best(&n.src, i).is_some() {
                            n.since[i] = (n.since[i] + 1).min(2);
                        }
                    }
                }
            }
            Action::Edit(o, p) => {
                let before = [best(&s.src, 0), best(&s.src, 1)];
                n.src[o as usize] = p;
                n.edits += 1;
                for (i, old) in before.iter().enumerate() {
                    if best(&n.src, i) != *old {
                        n.since[i] = 0;
                    }
                }
            }
            Action::StopNode => {
                if s.run[NODE].phase == Phase::Sent {
                    n.seen |= SEEN_GAP;
                }
                n.run[NODE] = Run::default();
                n.up[NODE] = false;
                n.node_stopped = true;
            }
            Action::StartRust => n.up[RUST] = true,
            Action::Load(a, kind) => {
                let date = day(s.t);
                let f = s.file;
                n.runs += 1;
                n.seen |= if a == NODE {
                    SEEN_NODE_LOAD
                } else {
                    SEEN_RUST_LOAD
                };
                if a == RUST
                    && (s.node_stopped || self.overlap)
                    && (!self.overlap || s.seen & SEEN_NODE_LOAD != 0)
                {
                    n.seen |= SEEN_CUTOVER;
                }
                if let (true, Some(p)) = (self.fix_pending, f.pending) {
                    n.run[a] = Run {
                        phase: Phase::Ready,
                        kind: p.kind,
                        date: p.date,
                        snap: f,
                        obs: p.obs,
                        failed: false,
                        polled: false,
                        msg: Some(p),
                    };
                    return Some(n);
                }
                match kind {
                    Kind::Digest if f.digest_date == date + 1 => return None,
                    Kind::Alert if f.counts[date as usize] >= CAP => return None,
                    _ => {}
                }
                n.run[a] = Run {
                    phase: Phase::Loaded,
                    kind,
                    date,
                    snap: f,
                    ..Run::default()
                };
            }
            Action::Poll(a, mask) => {
                let r = &mut n.run[a];
                for (o, source) in SOURCE.iter().enumerate() {
                    let lost = mask & (1 << source) != 0;
                    r.obs[o] = if lost { 0 } else { s.src[o] };
                }
                r.failed = mask != 0;
                r.polled = true;
                r.phase = Phase::Polled;
                if mask != 0 {
                    n.faults += 1;
                }
            }
            Action::Decide(a) => {
                let r = s.run[a];
                match self.compose(&r) {
                    Some(m) => {
                        n.run[a].msg = Some(m);
                        n.run[a].phase = if self.fix_pending {
                            Phase::Composed
                        } else {
                            Phase::Ready
                        };
                    }
                    None if r.kind == Kind::Digest => n.run[a].phase = Phase::SkipSave,
                    None => {
                        n.run[a] = Run::default();
                        self.check_no_skip(&mut n, &r);
                    }
                }
            }
            Action::Claim(a) => {
                let r = &mut n.run[a];
                r.snap.pending = r.msg;
                n.file = r.snap;
                r.phase = Phase::Ready;
            }
            Action::Send(a, outcome) => {
                let m = s.run[a].msg.expect("composed");
                let known = s.server.contains(&m.key());
                if outcome != Outcome::Down && (!self.dedup || !known) {
                    if !known {
                        let at = n.server.partition_point(|k| *k < m.key());
                        n.server.insert(at, m.key());
                    }
                    self.admit(&mut n, &m);
                }
                match outcome {
                    Outcome::Delivered => n.run[a].phase = Phase::Sent,
                    Outcome::Lost => {
                        n.faults += 1;
                        n.seen |= SEEN_GAP;
                        n.run[a] = Run::default();
                    }
                    Outcome::Down => {
                        n.faults += 1;
                        n.run[a] = Run::default();
                    }
                }
            }
            Action::Save(a) => {
                let r = s.run[a];
                let m = r.msg.expect("sent");
                if !s.server.contains(&m.key()) {
                    n.bad |= BAD_ACK;
                }
                n.file = self.ack(&r.snap, &m);
                n.run[a] = Run::default();
                if s.seen & SEEN_GAP != 0 {
                    n.seen |= SEEN_RESUMED;
                }
                self.check_no_skip(&mut n, &r);
            }
            Action::SkipSave(a) => {
                let r = s.run[a];
                let mut f = r.snap;
                f.digest_date = r.date + 1;
                n.file = f;
                n.run[a] = Run::default();
            }
            Action::Crash(a) => {
                if s.run[a].phase == Phase::Sent {
                    n.seen |= SEEN_GAP;
                }
                n.faults += 1;
                n.run[a] = Run::default();
            }
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("no_repeat", |_, s: &State| s.bad & BAD_REPEAT == 0),
            Property::always("one_digest_per_day", |_, s: &State| {
                s.bad & BAD_DIGEST2 == 0
            }),
            Property::always("silent_when_unchanged", |_, s: &State| {
                s.bad & BAD_SILENT == 0
            }),
            Property::always("no_skip", |_, s: &State| s.bad & BAD_SKIP == 0),
            Property::always("acked_only_after_admission", |_, s: &State| {
                s.bad & BAD_ACK == 0
            }),
            Property::sometimes("new_alert", |_, s: &State| s.seen & SEEN_NEW_ALERT != 0),
            Property::sometimes("restart_continues", |_, s: &State| {
                s.seen & SEEN_RESUMED != 0
            }),
            Property::sometimes("cutover", |_, s: &State| s.seen & SEEN_CUTOVER != 0),
            Property::sometimes("digest_relists", |_, s: &State| s.seen & SEEN_RELIST != 0),
        ]
    }
}

/// The watcher as the code is now (fixes F1 F2 F3), one fault, server dedup on.
fn current() -> Deals {
    Deals {
        node: false,
        overlap: false,
        dedup: true,
        fix_pending: true,
        fix_ack_delivered: true,
        fix_min_price: true,
        max_runs: 4,
        max_edits: 2,
        max_faults: 1,
    }
}

/// The code before the fixes (lucina-rs dd7ec9e): bugs DW1-DW3.
pub fn pre_fix() -> Deals {
    Deals {
        fix_pending: false,
        fix_ack_delivered: false,
        fix_min_price: false,
        ..current()
    }
}

/// The cases. `_deep` changes nothing: the default bounds are exhaustive (about one
/// million states per case). DW1-DW3 are fixed in the code, so G0-G2 model the fixed
/// watcher; G3-G5 are rule witnesses (never two watchers; the server dedup is needed).
pub fn cases(_deep: bool) -> Vec<Case> {
    use Expect::*;
    let current = current();
    let node = Deals {
        node: true,
        max_faults: 0,
        ..current.clone()
    };
    let expect = |violated: &[&'static str], reached: &[&'static str]| {
        let mut v: Vec<(&'static str, Expect)> = [
            "no_repeat",
            "one_digest_per_day",
            "silent_when_unchanged",
            "no_skip",
            "acked_only_after_admission",
        ]
        .iter()
        .map(|p| {
            (
                *p,
                if violated.contains(p) {
                    Violated
                } else {
                    Holds
                },
            )
        })
        .collect();
        v.extend(reached.iter().map(|w| (*w, Reached)));
        v
    };
    let rust_witnesses = ["new_alert", "restart_continues", "digest_relists"];
    let cutover_witnesses = [
        "new_alert",
        "restart_continues",
        "cutover",
        "digest_relists",
    ];
    vec![
        Case::new(
            PROTOCOL,
            "G0 Rust (F1 F2 F3), no faults",
            Deals {
                max_faults: 0,
                ..current.clone()
            },
            None,
            expect(&[], &["new_alert", "digest_relists"]),
        ),
        Case::new(
            PROTOCOL,
            "G1 Rust (F1 F2 F3), one fault",
            current.clone(),
            None,
            expect(&[], &rust_witnesses),
        ),
        Case::new(
            PROTOCOL,
            "G2 stop at any step, start, same file (F1 F2 F3 in both)",
            node.clone(),
            None,
            expect(&[], &cutover_witnesses),
        ),
        Case::new(
            PROTOCOL,
            "G3 two watchers at once (rule witness)",
            Deals {
                overlap: true,
                ..node.clone()
            },
            None,
            expect(&G3_VIOLATED, &["cutover"]),
        ),
        Case::new(
            PROTOCOL,
            "G4 no server dedup, no faults",
            Deals {
                dedup: false,
                max_faults: 0,
                ..current.clone()
            },
            None,
            expect(&[], &["new_alert", "digest_relists"]),
        ),
        Case::new(
            PROTOCOL,
            "G5 no server dedup, one fault (rule witness)",
            Deals {
                dedup: false,
                ..current
            },
            None,
            expect(
                &["no_repeat", "one_digest_per_day", "silent_when_unchanged"],
                &rust_witnesses,
            ),
        ),
    ]
}

/// What two fixed watchers on one file break (stale snapshots, lost updates); `no_skip`
/// holds with F2.
const G3_VIOLATED: [&str; 3] = ["no_repeat", "one_digest_per_day", "silent_when_unchanged"];

/// DW1-DW3 stay reproducible: the shortest traces of `docs/verification.md` §14, replayed
/// on the pre-fix watcher, break the property they broke (no model check needed).
#[cfg(test)]
mod pre_fix_traces {
    use super::*;
    use Action::*;
    use Kind::*;
    use Outcome::*;

    fn replay(m: &Deals, trace: &[Action]) -> State {
        let mut s = m.init_states().remove(0);
        for (i, a) in trace.iter().enumerate() {
            let mut enabled = Vec::new();
            m.actions(&s, &mut enabled);
            assert!(enabled.contains(a), "step {i}: {a:?} not enabled");
            s = m
                .next_state(&s, *a)
                .unwrap_or_else(|| panic!("step {i}: {a:?}"));
        }
        s
    }

    #[test]
    fn dw1_lost_response_then_alert_repeats() {
        let trace = [
            Tick,
            Load(RUST, Digest),
            Poll(RUST, 0),
            Decide(RUST),
            Send(RUST, Lost),
            Load(RUST, Alert),
            Poll(RUST, 0),
            Decide(RUST),
            Send(RUST, Delivered),
        ];
        let s = replay(&pre_fix(), &trace);
        assert_ne!(s.bad & BAD_REPEAT, 0);
        assert_ne!(s.bad & BAD_SILENT, 0);
    }

    #[test]
    fn dw2_price_bounce_repeats() {
        let trace = [
            Tick,
            Tick,
            Load(RUST, Alert),
            Poll(RUST, 0),
            Decide(RUST),
            Send(RUST, Delivered),
            Save(RUST),
            Tick,
            Tick,
            Edit(0, 2),
            Load(RUST, Digest),
            Poll(RUST, 0),
            Decide(RUST),
            Send(RUST, Delivered),
            Save(RUST),
            Edit(0, 1),
            Load(RUST, Alert),
            Poll(RUST, 0),
            Decide(RUST),
            Send(RUST, Delivered),
        ];
        let m = Deals {
            max_faults: 0,
            ..pre_fix()
        };
        assert_ne!(replay(&m, &trace).bad & BAD_REPEAT, 0);
        // F3 alone removes it: the same steps up to the last Decide compose nothing.
        let f3 = Deals {
            fix_min_price: true,
            ..m
        };
        let s = replay(&f3, &trace[..trace.len() - 1]);
        assert_eq!(s.run[RUST].phase, Phase::Idle);
        assert_eq!(s.bad, 0);
    }

    #[test]
    fn dw3_alternative_skipped() {
        let trace = [
            Load(RUST, Digest),
            Poll(RUST, 0),
            Decide(RUST),
            Send(RUST, Delivered),
            Save(RUST),
            Edit(0, 0),
            Tick,
            Tick,
            Load(RUST, Alert),
            Poll(RUST, 0),
            Decide(RUST),
        ];
        let m = Deals {
            max_faults: 0,
            ..pre_fix()
        };
        assert_ne!(replay(&m, &trace).bad & BAD_SKIP, 0);
        // F2 alone: the alternative is composed (the run goes on to send it).
        let f2 = Deals {
            fix_ack_delivered: true,
            ..m
        };
        let s = replay(&f2, &trace);
        assert_eq!(s.run[RUST].phase, Phase::Ready);
        assert_eq!(s.bad, 0);
    }
}
