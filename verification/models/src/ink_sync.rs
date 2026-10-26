//! Lucina Ink device sync (`crates/ink-sync`, `POST /v1/library/sync`).
//!
//! Actors: two reader devices with a durable outbox and a durable ack set, one
//! server, and an unreliable network (a multiset: any message may be delivered
//! next, dropped or duplicated within budgets). Devices crash (volatile state:
//! the requests in flight). The owner may mark the book completed.
//!
//! The model client may pipeline (up to two single-op requests in flight), a
//! superset of the real client (one batch at a time, oldest ops first), so the
//! server sees ops out of counter order and the watermark's sparse set is used.
//! A batch of several ops is the same as consecutive single-op deliveries
//! (the server applies a request's ops one by one under one lock).
//!
//! | Model action | Code |
//! |---|---|
//! | `Record(d)` | `Outbox::record` (append + fsync before any I/O) |
//! | `Send(d)` | `ClientMachine::next` → `Syncer::step` builds the request from `Outbox::pending` |
//! | `Timeout(d, c)` | `TransportError::Network` → `ClientMachine::on_failure` (same op ids, backoff) |
//! | `Crash(d)` | process restart: `Outbox::open` (torn tail truncated), fresh `ClientMachine` |
//! | `Deliver(Req)` | `serve` → `apply_mut` (watermark gate, then the registers) |
//! | `Deliver(Resp)` | `Outbox::ack` (append + fsync), `ClientMachine::on_response` |
//! | `Drop`, `Dup` | `FlakyTransport` faults: lost request/response, duplicate delivery |
//! | `Owner` | `PATCH /v1/library/books/{id}` `status: completed` → `owner_set_status` |
//!
//! Switches turn off one mechanism each (ablations): the model must then find
//! the bug, which shows the properties are not vacuous.
use stateright::{Model, Property};

use crate::{set_has, set_insert, Case, Expect};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum St {
    Reading,
    Completed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Progress(u8),
    Status(St),
    Highlight(u8),
    Delete(u8),
}

/// A scripted op: its device time and kind. Op `(d, c)` is script `d`'s
/// entry `c - 1` (counters start at 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScriptOp {
    pub at: u8,
    pub kind: Kind,
}

pub type OpId = (u8, u8);
/// `(at, device, counter)`: the last-writer-wins order.
pub type Stamp = (u8, u8, u8);

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Watermark {
    pub contig: u8,
    pub sparse: Vec<u8>,
}

impl Watermark {
    pub fn contains(&self, c: u8) -> bool {
        c <= self.contig || set_has(&self.sparse, &c)
    }
    /// `Err` = the sparse set is full (deferred, not acked).
    fn insert(&mut self, c: u8, max_sparse: usize) -> Result<(), ()> {
        if self.contains(c) {
            return Ok(());
        }
        if c == self.contig + 1 {
            self.contig = c;
            while let Ok(i) = self.sparse.binary_search(&(self.contig + 1)) {
                self.sparse.remove(i);
                self.contig += 1;
            }
            return Ok(());
        }
        if self.sparse.len() >= max_sparse {
            return Err(());
        }
        set_insert(&mut self.sparse, c);
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Content {
    pub progress: Option<(Stamp, u8)>,
    /// `(completed, stamp, status)`: the device status register.
    pub status: Option<(bool, Stamp, St)>,
    pub highlights: Vec<u8>,
    pub tombstones: Vec<OpId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Server {
    pub wm: Vec<Watermark>,
    pub content: Content,
    pub owner_completed: bool,
    /// Ghost: fresh applications per op id.
    pub effects: Vec<(OpId, u8)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Device {
    /// Ops recorded durably (a prefix of the script).
    pub recorded: u8,
    /// Durable acks (the outbox's ack records).
    pub acked: Vec<u8>,
    /// Volatile: counters whose request is outstanding.
    pub inflight: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Msg {
    Req(u8, u8),
    /// `(device, counter, acked)`
    Resp(u8, u8, bool),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub server: Server,
    pub devices: Vec<Device>,
    pub net: Vec<Msg>,
    pub drops_left: u8,
    pub dups_left: u8,
    pub crashes_left: u8,
    pub spurious_timeouts_left: u8,
    /// Ghosts.
    pub ever_acked: Vec<OpId>,
    pub resent_acked: bool,
    pub owner_done: bool,
    pub status_at_owner: Option<(bool, Stamp, St)>,
    pub ever_completed: bool,
    pub superseded: bool,
    pub protected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Record(u8),
    Send(u8),
    Timeout(u8, u8),
    Crash(u8),
    Deliver(Msg),
    Drop(Msg),
    Dup(Msg),
    Owner,
}

#[derive(Clone, Debug)]
pub struct InkSync {
    pub scripts: Vec<Vec<ScriptOp>>,
    pub drops: u8,
    pub dups: u8,
    pub crashes: u8,
    pub spurious_timeouts: u8,
    pub pipeline: usize,
    pub max_sparse: usize,
    pub owner_acts: bool,
    // Mechanisms (all true = the code). Ablations turn one off.
    /// The op-id gate (watermark) before apply.
    pub gate: bool,
    /// Acks are appended to the outbox (durable) rather than kept in memory.
    pub durable_acks: bool,
    /// Ops stay in the outbox until acked (not dropped once sent).
    pub keep_until_acked: bool,
    /// Device `completed` is sticky; an owner `completed` freezes the status.
    pub status_rules: bool,
    /// Last writer wins by stamp (off: the last delivery wins).
    pub stamps: bool,
}

impl InkSync {
    fn op(&self, d: u8, c: u8) -> ScriptOp {
        self.scripts[d as usize][c as usize - 1]
    }

    /// `apply_mut` on the model state; returns whether the op is acked.
    fn apply(&self, s: &mut Server, d: u8, c: u8, ghost: Option<&mut (bool, bool)>) -> bool {
        let wm = &mut s.wm[d as usize];
        if self.gate && wm.contains(c) {
            return true; // duplicate
        }
        if wm.insert(c, self.max_sparse).is_err() && self.gate {
            return false; // deferred
        }
        match s.effects.iter_mut().find(|(id, _)| *id == (d, c)) {
            Some((_, n)) => *n += 1,
            None => {
                s.effects.push(((d, c), 1));
                s.effects.sort();
            }
        }
        let op = self.op(d, c);
        let stamp: Stamp = (op.at, d, c);
        let (mut superseded, mut protected) = (false, false);
        match op.kind {
            Kind::Progress(v) => match s.content.progress {
                Some((cur, _)) if self.stamps && cur > stamp => superseded = true,
                _ => s.content.progress = Some((stamp, v)),
            },
            Kind::Status(x) => {
                if self.status_rules && s.owner_completed {
                    protected = true;
                } else {
                    let new = (self.status_rules && x == St::Completed, stamp, x);
                    match s.content.status {
                        Some(cur) if self.stamps && (cur.0, cur.1) > (new.0, new.1) => {
                            superseded = true
                        }
                        Some(cur) if !self.stamps && self.status_rules && cur.0 && !new.0 => {
                            superseded = true
                        }
                        _ => s.content.status = Some(new),
                    }
                }
            }
            Kind::Highlight(k) => {
                set_insert(&mut s.content.highlights, k);
            }
            Kind::Delete(_) => {
                set_insert(&mut s.content.tombstones, (d, c));
            }
        }
        if let Some(g) = ghost {
            g.0 |= superseded;
            g.1 |= protected;
        }
        true
    }

    /// What the library shows (the projection).
    fn visible(&self, s: &Server) -> (Option<u8>, Option<St>, Vec<u8>, Vec<OpId>) {
        let status = if self.status_rules && s.owner_completed {
            Some(St::Completed)
        } else {
            s.content.status.map(|x| x.2)
        };
        (
            s.content.progress.map(|p| p.1),
            status,
            s.content.highlights.clone(),
            s.content.tombstones.clone(),
        )
    }

    /// A second server: the owner's edit first, then the same applied ops in
    /// the given order (each once).
    fn replay(&self, s: &Server, reverse: bool) -> Server {
        let mut ids: Vec<OpId> = Vec::new();
        for (d, w) in s.wm.iter().enumerate() {
            for c in 1..=self.scripts[d].len() as u8 {
                if w.contains(c) {
                    ids.push((d as u8, c));
                }
            }
        }
        if reverse {
            ids.reverse();
        }
        let mut t = Server {
            wm: vec![Watermark::default(); self.scripts.len()],
            content: Content::default(),
            owner_completed: s.owner_completed,
            effects: Vec::new(),
        };
        // A replay delivers each op once, in its own order: the sparse bound
        // must not defer it.
        let big = InkSync {
            max_sparse: usize::MAX,
            ..self.clone()
        };
        for (d, c) in ids {
            big.apply(&mut t, d, c, None);
        }
        t
    }

    fn all_applied(&self, s: &State) -> bool {
        self.scripts
            .iter()
            .enumerate()
            .all(|(d, sc)| (1..=sc.len() as u8).all(|c| s.server.wm[d].contains(c)))
    }
}

fn bump(net: &mut Vec<Msg>, m: Msg) {
    let i = net.partition_point(|x| *x < m);
    net.insert(i, m);
}

fn take(net: &mut Vec<Msg>, m: Msg) {
    let i = net.iter().position(|x| *x == m).expect("message in net");
    net.remove(i);
}

impl Model for InkSync {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let n = self.scripts.len();
        vec![State {
            server: Server {
                wm: vec![Watermark::default(); n],
                content: Content::default(),
                owner_completed: false,
                effects: Vec::new(),
            },
            devices: vec![
                Device {
                    recorded: 0,
                    acked: Vec::new(),
                    inflight: Vec::new(),
                };
                n
            ],
            net: Vec::new(),
            drops_left: self.drops,
            dups_left: self.dups,
            crashes_left: self.crashes,
            spurious_timeouts_left: self.spurious_timeouts,
            ever_acked: Vec::new(),
            resent_acked: false,
            owner_done: false,
            status_at_owner: None,
            ever_completed: false,
            superseded: false,
            protected: false,
        }]
    }

    fn actions(&self, s: &State, a: &mut Vec<Action>) {
        for (i, dev) in s.devices.iter().enumerate() {
            let d = i as u8;
            if (dev.recorded as usize) < self.scripts[i].len() {
                a.push(Action::Record(d));
            }
            let next =
                (1..=dev.recorded).find(|c| !set_has(&dev.acked, c) && !set_has(&dev.inflight, c));
            if next.is_some() && dev.inflight.len() < self.pipeline {
                a.push(Action::Send(d));
            }
            for &c in &dev.inflight {
                let outstanding = s.net.iter().any(|m| match *m {
                    Msg::Req(x, y) | Msg::Resp(x, y, _) => x == d && y == c,
                });
                // A timeout always fires once nothing is left to arrive; a
                // spurious one (the message is only slow) within its budget.
                if !outstanding || s.spurious_timeouts_left > 0 {
                    a.push(Action::Timeout(d, c));
                }
            }
            if s.crashes_left > 0 && (!dev.inflight.is_empty() || dev.recorded > 0) {
                a.push(Action::Crash(d));
            }
        }
        let mut last = None;
        for &m in &s.net {
            if last == Some(m) {
                continue;
            }
            last = Some(m);
            a.push(Action::Deliver(m));
            if s.drops_left > 0 {
                a.push(Action::Drop(m));
            }
            if s.dups_left > 0 {
                a.push(Action::Dup(m));
            }
        }
        if self.owner_acts && !s.owner_done {
            a.push(Action::Owner);
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut s = last.clone();
        match action {
            Action::Record(d) => s.devices[d as usize].recorded += 1,
            Action::Send(d) => {
                let dev = &mut s.devices[d as usize];
                let c = (1..=dev.recorded)
                    .find(|c| !set_has(&dev.acked, c) && !set_has(&dev.inflight, c))?;
                if set_has(&s.ever_acked, &(d, c)) {
                    s.resent_acked = true;
                }
                if self.keep_until_acked {
                    set_insert(&mut dev.inflight, c);
                } else {
                    // Fire and forget: the op leaves the outbox once sent.
                    set_insert(&mut dev.acked, c);
                    set_insert(&mut s.ever_acked, (d, c));
                }
                bump(&mut s.net, Msg::Req(d, c));
            }
            Action::Timeout(d, c) => {
                let outstanding = s.net.iter().any(|m| match *m {
                    Msg::Req(x, y) | Msg::Resp(x, y, _) => x == d && y == c,
                });
                if outstanding {
                    s.spurious_timeouts_left -= 1;
                }
                s.devices[d as usize].inflight.retain(|x| *x != c);
            }
            Action::Crash(d) => {
                s.crashes_left -= 1;
                let dev = &mut s.devices[d as usize];
                dev.inflight.clear();
                if !self.durable_acks {
                    dev.acked.clear();
                }
            }
            Action::Deliver(m) => {
                take(&mut s.net, m);
                match m {
                    Msg::Req(d, c) => {
                        let mut g = (false, false);
                        let ok = self.apply(&mut s.server, d, c, Some(&mut g));
                        s.superseded |= g.0;
                        s.protected |= g.1;
                        if let Some(st) = s.server.content.status {
                            if st.2 == St::Completed {
                                s.ever_completed = true;
                            }
                        }
                        bump(&mut s.net, Msg::Resp(d, c, ok));
                    }
                    Msg::Resp(d, c, ok) => {
                        let dev = &mut s.devices[d as usize];
                        dev.inflight.retain(|x| *x != c);
                        if ok {
                            set_insert(&mut dev.acked, c);
                            set_insert(&mut s.ever_acked, (d, c));
                        }
                    }
                }
            }
            Action::Drop(m) => {
                s.drops_left -= 1;
                take(&mut s.net, m);
            }
            Action::Dup(m) => {
                s.dups_left -= 1;
                bump(&mut s.net, m);
            }
            Action::Owner => {
                s.owner_done = true;
                s.server.owner_completed = true;
                s.status_at_owner = s.server.content.status;
            }
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            // An op the device dropped from its outbox as acked is on the server.
            Property::always("acked_never_lost", |_, s: &State| {
                s.devices
                    .iter()
                    .enumerate()
                    .all(|(d, dev)| dev.acked.iter().all(|&c| s.server.wm[d].contains(c)))
            }),
            Property::always("never_resend_acked", |_, s: &State| !s.resent_acked),
            // Idempotency: no op's effect is applied twice.
            Property::always("applied_at_most_once", |_, s: &State| {
                s.server.effects.iter().all(|(_, n)| *n <= 1)
            }),
            Property::always("owner_completed_never_reverted", |_, s: &State| {
                !s.owner_done || s.server.content.status == s.status_at_owner
            }),
            Property::always("device_completed_sticky", |_, s: &State| {
                !s.ever_completed
                    || s.server
                        .content
                        .status
                        .is_some_and(|x| x.2 == St::Completed)
            }),
            Property::always(
                "highlights_only_grow_and_deletes_are_recorded",
                |m: &InkSync, s: &State| {
                    // Every applied highlight op is visible; every applied delete
                    // is a tombstone and removed nothing.
                    s.server.wm.iter().enumerate().all(|(d, w)| {
                        (1..=m.scripts[d].len() as u8)
                            .filter(|c| w.contains(*c))
                            .all(|c| match m.op(d as u8, c).kind {
                                Kind::Highlight(k) => set_has(&s.server.content.highlights, &k),
                                Kind::Delete(_) => {
                                    set_has(&s.server.content.tombstones, &(d as u8, c))
                                }
                                _ => true,
                            })
                    })
                },
            ),
            // Convergence: two more servers applying the same ops in other
            // orders show the same library.
            Property::always("converges", |m: &InkSync, s: &State| {
                let v = m.visible(&s.server);
                v == m.visible(&m.replay(&s.server, false))
                    && v == m.visible(&m.replay(&s.server, true))
            }),
            Property::eventually("all_ops_applied", |m: &InkSync, s: &State| m.all_applied(s)),
            Property::sometimes("all_synced", |m: &InkSync, s: &State| {
                m.all_applied(s)
                    && s.net.is_empty()
                    && s.devices
                        .iter()
                        .enumerate()
                        .all(|(d, dev)| dev.acked.len() == m.scripts[d].len())
            }),
            Property::sometimes("out_of_order_applied", |_, s: &State| {
                s.server.wm.iter().any(|w| !w.sparse.is_empty())
            }),
            Property::sometimes("older_op_superseded", |_, s: &State| s.superseded),
            Property::sometimes("owner_protection_used", |_, s: &State| s.protected),
        ]
    }
}

pub const PROTOCOL: &str = "ink-sync";

pub fn cases(deep: bool) -> Vec<Case> {
    use Expect::*;
    let op = |at, kind| ScriptOp { at, kind };
    // I1: progress and status races between two devices.
    let status = InkSync {
        scripts: vec![
            vec![op(2, Kind::Status(St::Completed)), op(3, Kind::Progress(1))],
            vec![op(4, Kind::Status(St::Reading)), op(1, Kind::Progress(2))],
        ],
        drops: 1,
        dups: 1,
        crashes: if deep { 2 } else { 1 },
        spurious_timeouts: 1,
        pipeline: 2,
        max_sparse: 1,
        owner_acts: true,
        gate: true,
        durable_acks: true,
        keep_until_acked: true,
        status_rules: true,
        stamps: true,
    };
    // I2: highlights and a deletion request.
    let highlights = InkSync {
        scripts: vec![
            vec![op(1, Kind::Highlight(0)), op(2, Kind::Delete(0))],
            vec![op(1, Kind::Highlight(0)), op(3, Kind::Highlight(1))],
        ],
        owner_acts: false,
        ..status.clone()
    };
    let all = |extra: Vec<(&'static str, Expect)>| {
        let mut v = vec![
            ("acked_never_lost", Holds),
            ("never_resend_acked", Holds),
            ("applied_at_most_once", Holds),
            ("owner_completed_never_reverted", Holds),
            ("device_completed_sticky", Holds),
            ("highlights_only_grow_and_deletes_are_recorded", Holds),
            ("converges", Holds),
            ("all_ops_applied", Holds),
            ("all_synced", Reached),
            ("out_of_order_applied", Reached),
        ];
        v.extend(extra);
        v
    };
    vec![
        Case::new(
            PROTOCOL,
            "I1 two devices: progress/status races, owner edit",
            status.clone(),
            None,
            all(vec![
                ("older_op_superseded", Reached),
                ("owner_protection_used", Reached),
            ]),
        ),
        Case::new(
            PROTOCOL,
            "I2 two devices: highlights and a deletion request",
            highlights.clone(),
            None,
            all(vec![]),
        ),
        // Ablations: each removes one mechanism; the model must catch it.
        // Smaller fault budgets: each only has to show its bug.
        Case::new(
            PROTOCOL,
            "I3 ablation: no op-id gate",
            InkSync {
                gate: false,
                crashes: 0,
                spurious_timeouts: 0,
                ..highlights.clone()
            },
            None,
            vec![("applied_at_most_once", Violated), ("converges", Holds)],
        ),
        Case::new(
            PROTOCOL,
            "I4 ablation: acks kept in memory only",
            InkSync {
                durable_acks: false,
                dups: 0,
                spurious_timeouts: 0,
                ..highlights.clone()
            },
            None,
            vec![
                ("never_resend_acked", Violated),
                ("acked_never_lost", Holds),
                ("applied_at_most_once", Holds),
            ],
        ),
        Case::new(
            PROTOCOL,
            "I5 ablation: ops leave the outbox when sent",
            InkSync {
                keep_until_acked: false,
                ..highlights
            },
            None,
            vec![
                ("acked_never_lost", Violated),
                ("all_ops_applied", Violated),
            ],
        ),
        Case::new(
            PROTOCOL,
            "I6 ablation: plain last-writer-wins status",
            InkSync {
                status_rules: false,
                crashes: 0,
                dups: 0,
                spurious_timeouts: 0,
                ..status.clone()
            },
            None,
            vec![
                ("owner_completed_never_reverted", Violated),
                ("device_completed_sticky", Violated),
                ("converges", Holds),
            ],
        ),
        Case::new(
            PROTOCOL,
            "I7 ablation: the last delivery wins (no stamps)",
            InkSync {
                stamps: false,
                crashes: 0,
                dups: 0,
                spurious_timeouts: 0,
                ..status
            },
            None,
            vec![("converges", Violated)],
        ),
    ]
}
