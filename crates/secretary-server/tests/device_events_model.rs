//! Stateright model of device-event delivery: the phone's outbox,
//! secretary-server `POST /v1/device/events` (`device.rs::events`) and
//! Lucina's `DeviceEventStore::ingest` (lucina-store `device.rs`).
//!
//! - Client (the phone): keeps the events not yet acknowledged and POSTs all
//!   of them; it may retry while an earlier request is still in flight (a
//!   timeout), a response may never arrive, and it drops the events of a
//!   request only on its 200.
//! - secretary-server: drops events whose (device, id) is in its in-memory
//!   LRU (`SeenIds`, capacity `cap`), then forwards the rest one chunk at a
//!   time; after Lucina accepts a chunk its ids enter the LRU; a Lucina error
//!   ends the request with an error. A restart empties the LRU and kills the
//!   requests in flight (the client sees an error).
//! - Lucina: dedupes by event id (alone) in a durable set and appends the
//!   fresh events to `device-context.jsonl`; the append can fail. Before fix
//!   E1 the ids were marked before the append, so a failed append left them
//!   marked.
//!
//! Properties:
//! - `stored_at_most_once`: no event is stored twice at Lucina;
//! - `no_silent_loss`: an event the phone dropped from its outbox is stored.
//!
//! Cases:
//! - `current`: E1 fixed, two requests in flight, a restart, a failed append,
//!   a lost response, LRU of 1: both hold (Lucina's durable dedupe carries at-most-once).
//! - `e1_before_fix`: `no_silent_loss` **violated** (the retry after a failed
//!   append is answered "duplicate"; regression test
//!   `crates/lucina-store/tests/device_events_retry.rs`).
//! - ablations without Lucina's dedupe (rule witnesses: secretary-server's
//!   LRU alone is not enough): a concurrent retry, a restart, an LRU eviction,
//!   each violates `stored_at_most_once`.
//! - `shared_id_two_devices` (rule witness for an assumption): the phone and
//!   the Mac send different events with the same id; secretary-server keys by
//!   (device, id) but Lucina by id alone, so one is dropped:
//!   `no_silent_loss` **violated**. Clients must use globally unique ids.
//!
//! TLA+ twin: `formal/devices/DeviceEvents.tla` (the same state; TLC finds the
//! same 4 875 distinct states for `current`), run by `formal/run-gap-checks`.

use stateright::{Checker, Model, Property};

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Stage {
    Filter,
    /// Events still to forward (bitmask over `events`).
    Forward(u8),
    /// The response, on its way to the phone.
    Done(bool),
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Req {
    batch: u8,
    stage: Stage,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct State {
    pending: u8,
    acked: u8,
    reqs: Vec<Req>,
    sends: u8,
    /// secretary-server's LRU, least recent first (event indexes).
    seen: Vec<u8>,
    restarts: u8,
    fails: u8,
    lost: u8,
    /// Lucina's durable dedupe set (event ids).
    lucina_ids: u8,
    stored: [u8; 3],
    // ghost
    server_dropped_dup: bool,
    lucina_dropped_dup: bool,
    retried_after_fail: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    Send,
    Filter(usize),
    LucinaOk(usize),
    LucinaFail(usize),
    Respond(usize),
    /// The response never reaches the phone (timeout, app suspended).
    Lose(usize),
    Restart,
}

/// (device, id) of each event.
type Ev = (u8, u8);

struct Delivery {
    events: &'static [Ev],
    max_inflight: usize,
    max_sends: u8,
    cap: usize,
    restarts: u8,
    fails: u8,
    lost: u8,
    e1_fixed: bool,
    lucina_dedupe: bool,
}

const ALL_HOLD: [&str; 2] = ["stored_at_most_once", "no_silent_loss"];

impl Delivery {
    fn all(&self) -> u8 {
        ((1u16 << self.events.len()) - 1) as u8
    }
    fn touch(&self, seen: &mut Vec<u8>, k: u8) {
        seen.retain(|&x| x != k);
        seen.push(k);
        while seen.len() > self.cap {
            seen.remove(0);
        }
    }
}

impl Model for Delivery {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            pending: self.all(),
            acked: 0,
            reqs: Vec::new(),
            sends: 0,
            seen: Vec::new(),
            restarts: 0,
            fails: 0,
            lost: 0,
            lucina_ids: 0,
            stored: [0; 3],
            server_dropped_dup: false,
            lucina_dropped_dup: false,
            retried_after_fail: false,
        }]
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        let inflight = s
            .reqs
            .iter()
            .filter(|r| !matches!(r.stage, Stage::Done(_)))
            .count();
        if s.pending != 0 && inflight < self.max_inflight && s.sends < self.max_sends {
            out.push(Action::Send);
        }
        for (i, r) in s.reqs.iter().enumerate() {
            match r.stage {
                Stage::Filter => out.push(Action::Filter(i)),
                Stage::Forward(0) => {
                    out.push(Action::Respond(i));
                    if s.lost < self.lost {
                        out.push(Action::Lose(i));
                    }
                }
                Stage::Forward(_) => {
                    out.push(Action::LucinaOk(i));
                    if s.fails < self.fails {
                        out.push(Action::LucinaFail(i));
                    }
                }
                Stage::Done(_) => {
                    out.push(Action::Respond(i));
                    if s.lost < self.lost {
                        out.push(Action::Lose(i));
                    }
                }
            }
        }
        if s.restarts < self.restarts {
            out.push(Action::Restart);
        }
    }

    fn next_state(&self, s: &State, action: Action) -> Option<State> {
        let mut n = s.clone();
        match action {
            Action::Send => {
                if s.fails > 0 {
                    n.retried_after_fail = true;
                }
                n.reqs.push(Req {
                    batch: s.pending,
                    stage: Stage::Filter,
                });
                n.sends += 1;
            }
            Action::Filter(i) => {
                let mut fresh = 0u8;
                for k in 0..self.events.len() as u8 {
                    if s.reqs[i].batch & (1 << k) == 0 {
                        continue;
                    }
                    if n.seen.contains(&k) {
                        // `contains` marks it most recently used.
                        self.touch(&mut n.seen, k);
                        n.server_dropped_dup = true;
                    } else {
                        fresh |= 1 << k;
                    }
                }
                n.reqs[i].stage = Stage::Forward(fresh);
            }
            Action::LucinaOk(i) | Action::LucinaFail(i) => {
                let Stage::Forward(rest) = s.reqs[i].stage else {
                    return None;
                };
                let k = rest.trailing_zeros() as u8;
                let id_bit = 1u8 << self.events[k as usize].1;
                let ok = matches!(action, Action::LucinaOk(_));
                if ok {
                    if self.lucina_dedupe && n.lucina_ids & id_bit != 0 {
                        n.lucina_dropped_dup = true;
                    } else {
                        n.lucina_ids |= id_bit;
                        n.stored[k as usize] += 1;
                    }
                    // The chunk was accepted: its ids enter the LRU.
                    self.touch(&mut n.seen, k);
                    n.reqs[i].stage = Stage::Forward(rest & !(1 << k));
                } else {
                    n.fails += 1;
                    if !self.e1_fixed && self.lucina_dedupe && n.lucina_ids & id_bit == 0 {
                        // Before E1: marked, then the append failed.
                        n.lucina_ids |= id_bit;
                    }
                    n.reqs[i].stage = Stage::Done(false);
                }
            }
            Action::Respond(i) => {
                let r = s.reqs[i];
                let ok = match r.stage {
                    Stage::Forward(0) => true,
                    Stage::Done(ok) => ok,
                    _ => return None,
                };
                n.reqs.remove(i);
                if ok {
                    n.pending &= !r.batch;
                    n.acked |= r.batch;
                }
            }
            Action::Lose(i) => {
                n.reqs.remove(i);
                n.lost += 1;
            }
            Action::Restart => {
                n.restarts += 1;
                n.seen.clear();
                for r in &mut n.reqs {
                    if !matches!(r.stage, Stage::Done(_)) {
                        r.stage = Stage::Done(false);
                    }
                }
            }
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("stored_at_most_once", |_, s: &State| {
                s.stored.iter().all(|&c| c <= 1)
            }),
            Property::always("no_silent_loss", |m: &Delivery, s: &State| {
                (0..m.events.len()).all(|k| s.acked & (1 << k) == 0 || s.stored[k] >= 1)
            }),
            Property::sometimes(
                "everything delivered and acknowledged",
                |m: &Delivery, s: &State| s.acked == m.all() && s.reqs.is_empty(),
            ),
            Property::sometimes("secretary-server drops a duplicate", |_, s: &State| {
                s.server_dropped_dup
            }),
            Property::sometimes("Lucina drops a duplicate", |_, s: &State| {
                s.lucina_dropped_dup
            }),
            Property::sometimes("a retry after a failed append", |_, s: &State| {
                s.retried_after_fail
            }),
        ]
    }
}

const ONE_DEVICE: &[Ev] = &[(0, 0), (0, 1)];
const TWO_DEVICES: &[Ev] = &[(0, 0), (1, 0)];

fn current() -> Delivery {
    Delivery {
        events: ONE_DEVICE,
        max_inflight: 2,
        max_sends: 4,
        cap: 1,
        restarts: 1,
        fails: 1,
        lost: 1,
        e1_fixed: true,
        lucina_dedupe: true,
    }
}

#[test]
fn device_events_current_code_holds() {
    let checker = current().checker().spawn_bfs().join();
    checker.assert_properties();
    println!("current: {} states", checker.unique_state_count());
}

#[test]
fn device_events_e1_before_fix_loses_an_event() {
    let checker = Delivery {
        e1_fixed: false,
        ..current()
    }
    .checker()
    .spawn_bfs()
    .join();
    let path = checker.assert_any_discovery("no_silent_loss");
    println!("E1 counterexample: {:?}", path.into_actions());
    checker.assert_no_discovery("stored_at_most_once");
}

/// Without Lucina's dedupe, each of the three ways around secretary-server's
/// LRU stores an event twice.
#[test]
fn device_events_server_lru_alone_is_not_enough() {
    let base = Delivery {
        lucina_dedupe: false,
        max_inflight: 1,
        cap: 8,
        restarts: 0,
        fails: 0,
        lost: 0,
        ..current()
    };
    for (name, m) in [
        (
            "concurrent retry",
            Delivery {
                max_inflight: 2,
                ..base
            },
        ),
        (
            "restart",
            Delivery {
                restarts: 1,
                ..base
            },
        ),
        (
            "eviction",
            Delivery {
                cap: 1,
                lost: 1,
                ..base
            },
        ),
    ] {
        let checker = m.checker().spawn_bfs().join();
        let path = checker.assert_any_discovery("stored_at_most_once");
        println!("{name}: {:?}", path.into_actions());
        checker.assert_no_discovery("no_silent_loss");
    }
    // Control: one request at a time, no restart, no lost response, a large
    // LRU: holds.
    let checker = base.checker().spawn_bfs().join();
    for p in ALL_HOLD {
        checker.assert_no_discovery(p);
    }
}

#[test]
fn device_events_shared_id_two_devices_loses_one() {
    let checker = Delivery {
        events: TWO_DEVICES,
        ..current()
    }
    .checker()
    .spawn_bfs()
    .join();
    checker.assert_any_discovery("no_silent_loss");
    checker.assert_no_discovery("stored_at_most_once");
}
