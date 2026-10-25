//! Resume positions of one film across devices (iPhone, web, Mac) and the
//! server (`crates/secretary-player/src/progress.rs` on every device,
//! `crates/secretary-media-stream/src/progress.rs` on the server).
//!
//! Each device plays (a new position stamped with its clock), pushes its
//! unacknowledged entry (`POST /v1/media/progress`, one request at a time),
//! pulls the server's entry (`GET /v1/media/progress`) and goes offline and
//! back. The network delays and reorders messages; going offline loses that
//! device's messages in flight. Clocks are shared milliseconds, so two
//! devices can stamp a write with the same `updated_at`.
//!
//! - server `Store::put`: store the incoming entry if it is newer, reply with
//!   an acknowledgement of the submitted stamp and the stored entry when it is
//!   newer than the submitted one;
//! - device `Book::acknowledge`: clear `dirty` unless the entry changed after
//!   the acknowledged write; `Book::merge`: adopt an entry only if it is newer.
//!
//! "Newer" is `(updated_at, device)` with fix R1 (`Entry::is_newer_than`),
//! `updated_at` alone before it. Fix R2 (`write_stamp`): a device stamps a
//! new write after the entry it holds, so a position played after adopting
//! another device's entry (a faster clock, or the same millisecond) is newer.

use stateright::{Model, Property};

use crate::{Case, Expect};

/// One write: a position (unique per write), the device clock, the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Write {
    pub pos: u8,
    pub at: u8,
    pub dev: u8,
}

/// The true order of writes: what "the newest position" means.
fn later(a: Write, b: Write) -> bool {
    (a.at, a.dev) > (b.at, b.dev)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Dev {
    pub local: Option<Write>,
    pub dirty: bool,
    pub online: bool,
    /// A push is in flight (`pushing` in the apps).
    pub pushing: bool,
    /// The server version the device last pulled.
    pub seen: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Msg {
    Push { dev: u8, w: Write },
    /// Acknowledges the submitted stamp; carries the stored entry when it is newer.
    Reply { dev: u8, acked_at: u8, newer: Option<Write> },
    PullReply { dev: u8, w: Option<Write>, version: u8 },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub now: u8,
    pub devices: Vec<Dev>,
    pub server: Option<Write>,
    pub version: u8,
    /// In flight, sorted (a multiset).
    pub net: Vec<Msg>,
    /// The newest write ever made (true order).
    pub latest: Option<Write>,
    pub writes: u8,
    pub ticks: u8,
    pub outages: u8,
    pub next_pos: u8,
    /// A device adopted an entry older than its own.
    pub stale_adopted: bool,
    /// The server replaced its entry with an older one.
    pub server_regressed: bool,
    /// Witness bookkeeping.
    pub both_dirty: bool,
    pub had_outage_with_dirty: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Tick,
    Play(u8),
    Offline(u8),
    Online(u8),
    Push(u8),
    Pull(u8),
    Deliver(Msg),
}

#[derive(Clone, Debug)]
pub struct ResumeSync {
    pub devices: u8,
    pub writes: u8,
    pub ticks: u8,
    pub outages: u8,
    /// Fix R1: `(updated_at, device)` order everywhere (else `updated_at` only).
    pub tie_break: bool,
    /// Fix R2: a write is stamped after the entry the device holds
    /// (`write_stamp`), else with the device clock alone.
    pub stamp_after_held: bool,
    /// The server keeps the newer entry (else a plain overwrite).
    pub server_lww: bool,
    /// Devices adopt only newer entries (else whatever the server sends).
    pub merge_lww: bool,
    /// `acknowledge` keeps an entry dirty when it changed after the acked write.
    pub ack_checks_time: bool,
}

impl ResumeSync {
    /// The comparison the code makes.
    fn code_newer(&self, a: Write, b: Write) -> bool {
        if self.tie_break {
            later(a, b)
        } else {
            a.at > b.at
        }
    }

    fn adopt(&self, s: &mut State, d: usize, w: Write) {
        let dev = &mut s.devices[d];
        let take = match dev.local {
            None => true,
            Some(l) => !self.merge_lww || self.code_newer(w, l),
        };
        if take {
            if let Some(l) = dev.local {
                if later(l, w) {
                    s.stale_adopted = true;
                }
            }
            dev.local = Some(w);
            dev.dirty = false;
        }
    }

    fn quiescent(s: &State) -> bool {
        s.net.is_empty() && s.devices.iter().all(|d| d.online && !d.dirty)
    }
}

fn add(net: &mut Vec<Msg>, m: Msg) {
    let i = net.binary_search(&m).unwrap_or_else(|e| e);
    net.insert(i, m);
}

impl Model for ResumeSync {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            now: 1,
            devices: (0..self.devices)
                .map(|_| Dev { local: None, dirty: false, online: true, pushing: false, seen: 0 })
                .collect(),
            server: None,
            version: 0,
            net: Vec::new(),
            latest: None,
            writes: self.writes,
            ticks: self.ticks,
            outages: self.outages,
            next_pos: 1,
            stale_adopted: false,
            server_regressed: false,
            both_dirty: false,
            had_outage_with_dirty: false,
        }]
    }

    fn actions(&self, s: &State, actions: &mut Vec<Action>) {
        if s.ticks > 0 {
            actions.push(Action::Tick);
        }
        for (i, d) in s.devices.iter().enumerate() {
            let i = i as u8;
            // A device's clock moves forward between its own writes.
            if s.writes > 0 && (self.stamp_after_held || d.local.is_none_or(|l| l.dev != i || l.at < s.now)) {
                actions.push(Action::Play(i));
            }
            if d.online && s.outages > 0 {
                actions.push(Action::Offline(i));
            }
            if !d.online {
                actions.push(Action::Online(i));
            }
            if d.online && d.dirty && !d.pushing {
                actions.push(Action::Push(i));
            }
            if d.online && !s.net.iter().any(|m| matches!(m, Msg::PullReply { dev, .. } if *dev == i)) {
                actions.push(Action::Pull(i));
            }
        }
        for m in &s.net {
            actions.push(Action::Deliver(*m));
        }
        actions.dedup();
    }

    fn next_state(&self, s: &State, a: Action) -> Option<State> {
        let mut s = s.clone();
        match a {
            Action::Tick => {
                s.now += 1;
                s.ticks -= 1;
            }
            Action::Play(d) => {
                let held = s.devices[d as usize].local.map(|l| l.at);
                let at = match held {
                    Some(h) if self.stamp_after_held && h >= s.now => h + 1,
                    _ => s.now,
                };
                let w = Write { pos: s.next_pos, at, dev: d };
                s.next_pos += 1;
                s.writes -= 1;
                let dev = &mut s.devices[d as usize];
                dev.local = Some(w);
                dev.dirty = true;
                if s.latest.is_none_or(|l| later(w, l)) {
                    s.latest = Some(w);
                }
                if s.devices.iter().filter(|x| x.dirty).count() >= 2 {
                    s.both_dirty = true;
                }
            }
            Action::Offline(d) => {
                s.outages -= 1;
                let dev = &mut s.devices[d as usize];
                dev.online = false;
                dev.pushing = false;
                if dev.dirty {
                    s.had_outage_with_dirty = true;
                }
                // The device's requests and the replies to it are lost.
                s.net.retain(|m| match *m {
                    Msg::Push { dev, .. } | Msg::Reply { dev, .. } | Msg::PullReply { dev, .. } => dev != d,
                });
            }
            Action::Online(d) => s.devices[d as usize].online = true,
            Action::Push(d) => {
                let dev = &mut s.devices[d as usize];
                dev.pushing = true;
                let w = dev.local.expect("dirty implies an entry");
                add(&mut s.net, Msg::Push { dev: d, w });
            }
            Action::Pull(d) => {
                let m = Msg::PullReply { dev: d, w: s.server, version: s.version };
                add(&mut s.net, m);
            }
            Action::Deliver(m) => {
                let i = s.net.binary_search(&m).ok()?;
                s.net.remove(i);
                match m {
                    Msg::Push { dev, w } => {
                        // Store::put
                        let store = match s.server {
                            None => true,
                            Some(cur) => !self.server_lww || self.code_newer(w, cur),
                        };
                        let newer = match s.server {
                            Some(cur) if self.server_lww && self.code_newer(cur, w) => Some(cur),
                            _ => None,
                        };
                        if store && s.server != Some(w) {
                            if let Some(cur) = s.server {
                                if later(cur, w) {
                                    s.server_regressed = true;
                                }
                            }
                            s.server = Some(w);
                            s.version += 1;
                        }
                        add(&mut s.net, Msg::Reply { dev, acked_at: w.at, newer });
                    }
                    Msg::Reply { dev, acked_at, newer } => {
                        let d = &mut s.devices[dev as usize];
                        d.pushing = false;
                        // Book::acknowledge
                        if !self.ack_checks_time || d.local.is_some_and(|l| l.at <= acked_at) {
                            d.dirty = false;
                        }
                        if let Some(w) = newer {
                            self.adopt(&mut s, dev as usize, w);
                        }
                    }
                    Msg::PullReply { dev, w, version } => {
                        s.devices[dev as usize].seen = version;
                        if let Some(w) = w {
                            self.adopt(&mut s, dev as usize, w);
                        }
                    }
                }
            }
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            // When everything is delivered and every device is online and clean,
            // the server holds the newest position anyone played.
            Property::always("no_position_lost", |_, s: &State| {
                !ResumeSync::quiescent(s) || s.server == s.latest
            }),
            // ...and every device that pulled since the last change shows it.
            Property::always("devices_converge", |_, s: &State| {
                !ResumeSync::quiescent(s)
                    || s.devices.iter().all(|d| d.seen != s.version || s.version == 0 || d.local == s.server)
            }),
            // A device never goes back to an older entry because of a stale update.
            Property::always("never_back_to_stale", |_, s: &State| !s.stale_adopted),
            Property::always("server_never_regresses", |_, s: &State| !s.server_regressed),
            // Not vacuous: two devices played between syncs, and an offline gap with
            // unsynced positions still ended with everyone on the newest position.
            Property::sometimes("concurrent_playback", |_, s: &State| s.both_dirty),
            Property::sometimes("offline_gap_resolved", |_, s: &State| {
                s.had_outage_with_dirty
                    && ResumeSync::quiescent(s)
                    && s.server == s.latest
                    && s.devices.iter().filter(|d| d.local.is_some()).count() >= 2
                    && s.devices.iter().all(|d| d.seen == s.version && d.local == s.server)
            }),
        ]
    }
}

pub const PROTOCOL: &str = "player";

pub fn cases(deep: bool) -> Vec<Case> {
    use Expect::*;
    let base = ResumeSync {
        devices: if deep { 3 } else { 2 },
        writes: 3,
        ticks: 1,
        outages: 1,
        tie_break: true,
        stamp_after_held: true,
        server_lww: true,
        merge_lww: true,
        ack_checks_time: true,
    };
    let all = |lost: Expect, converge: Expect, stale: Expect, regress: Expect| {
        vec![
            ("no_position_lost", lost),
            ("devices_converge", converge),
            ("never_back_to_stale", stale),
            ("server_never_regresses", regress),
            ("concurrent_playback", Reached),
            ("offline_gap_resolved", Reached),
        ]
    };
    vec![
        Case::new(
            PROTOCOL,
            "R before fixes R1, R2 (updated_at only)",
            ResumeSync { tie_break: false, stamp_after_held: false, ..base.clone() },
            None,
            all(Violated, Violated, Holds, Holds),
        ),
        Case::new(
            PROTOCOL,
            "R + fix R1 only (stamped with the device clock)",
            ResumeSync { stamp_after_held: false, ..base.clone() },
            None,
            vec![("devices_converge", Violated), ("never_back_to_stale", Holds), ("server_never_regresses", Holds)],
        ),
        Case::new(PROTOCOL, "R + fixes R1, R2", base.clone(), None, all(Holds, Holds, Holds, Holds)),
        Case::new(
            PROTOCOL,
            "R naive server: every push overwrites",
            ResumeSync { server_lww: false, ..base.clone() },
            None,
            vec![("no_position_lost", Violated), ("server_never_regresses", Violated)],
        ),
        Case::new(
            PROTOCOL,
            "R naive device: adopts whatever the server sends",
            ResumeSync { merge_lww: false, ..base.clone() },
            None,
            vec![("never_back_to_stale", Violated)],
        ),
        Case::new(
            PROTOCOL,
            "R naive acknowledge: clears dirty for a later write",
            ResumeSync { ack_checks_time: false, ..base },
            None,
            vec![("no_position_lost", Violated)],
        ),
    ]
}
