//! Stateright model of the relay store (owner rule: TLA+-style models in
//! Rust, checked in the gates).
//!
//! The model drives the real [`Store`], not a copy of its logic: each state
//! is the store plus ghost variables the checker needs (the clock, what the
//! phone sent, what the owner got). Actions:
//!
//! - `Arm` (a 30 s window, or continuous and unpausing when configured),
//!   `Disarm` (which pauses continuous forwarding). Arming keeps the codes
//!   already waiting (fix F2 of the forwarding model,
//!   `verification/models/src/otp.rs`);
//! - `Incoming(kind)`: a new event from the phone, protocol 2 (fresh, sent
//!   late, too old, a test event) or protocol 1 (right or wrong window id);
//! - `Resend(i)`: the phone sends event `i` again, unchanged. The phone keeps
//!   an event for at most `CODE_TTL_MS` after receiving it, so a resend only
//!   happens within that time;
//! - `Take` (`GET /otp`) and `Tick` (the clock moves 40 s);
//! - `Restart`: the relay process restarts, as `Relay::open` does it: a new
//!   store with the same `paused` state, the seen ids reloaded from
//!   `seen.json` (fix F3), the waiting codes lost (they live only in memory).
//!
//! The three initial states are a timed relay, a continuous one, and a
//! continuous one restarted while paused (the `paused` file).
//!
//! Properties, for every reachable state:
//!
//! - a code is delivered at most once, also across a restart;
//! - arming never drops a code that is waiting and fresh;
//! - nothing is delivered at or after its expiry;
//! - nothing accepted before a disarm is delivered after it;
//! - test events are never delivered, and only accepted codes are;
//! - pending never exceeds the limit (8 in the relay).
//!
//! `sometimes` properties check the interesting paths are reachable, so the
//! `always` properties are not vacuous.

use lucina_otp_relay::store::{Limits, Refusal, Store, MAX_PENDING};
use lucina_phone_core::proto::{Incoming, OtpSource, CODE_TTL_MS};
use stateright::{Checker, Model, Property};

/// Model clock start (so `now - CODE_TTL_MS` does not underflow).
const T0: u64 = 1_700_000_000_000;
const TICK_MS: u64 = 40_000;
const ARM_SECONDS: u64 = 30;
const WRONG_WINDOW: &str = "ffffffffffffffff";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    /// Protocol 2, received now.
    Fresh,
    /// Protocol 2, received 100 s ago (20 s of life left).
    Late,
    /// Protocol 2, received two minutes ago: already expired.
    Stale,
    /// Protocol 2, `test: true`.
    Test,
    /// Protocol 1 with the window id the phone last fetched.
    V1,
    /// Protocol 1 with a wrong window id.
    V1Wrong,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Action {
    Arm,
    Disarm,
    Incoming(Kind),
    Resend(u8),
    Take,
    Tick,
    Restart,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Sent {
    kind: Kind,
    /// When the phone received the message.
    phone_received: u64,
    window_id: Option<String>,
    /// Disarm count when the relay accepted it; `None` until it does.
    accepted_epoch: Option<u8>,
    /// Arm and restart counts when the relay accepted it.
    accepted_arms: u8,
    accepted_restarts: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct State {
    store: Store,
    now: u64,
    ticks: u8,
    arms: u8,
    disarms: u8,
    restarts: u8,
    windows: u16,
    sent: Vec<Sent>,
    /// Indices into `sent`, in delivery order.
    delivered: Vec<u8>,
    // Sticky ghost flags.
    late_delivery: bool,
    delivered_after_disarm: bool,
    unaccepted_delivery: bool,
    saw_duplicate: bool,
    saw_expired: bool,
    saw_paused: bool,
    saw_event_limit: bool,
    saw_overflow: bool,
    arm_dropped_code: bool,
    delivered_across_arm: bool,
    duplicate_after_restart: bool,
}

struct RelayModel {
    limits: Limits,
    kinds: &'static [Kind],
    max_events: usize,
    max_ticks: u8,
    max_arms: u8,
    max_disarms: u8,
    max_restarts: u8,
    /// `sometimes` properties this configuration must reach.
    reachable: fn() -> Vec<Property<RelayModel>>,
}

fn code(i: usize) -> String {
    format!("{:06}", 100_000 + i)
}

fn event_id(i: usize) -> String {
    format!("model-event-{i:04}")
}

fn window_id(n: u16) -> String {
    format!("{n:016x}")
}

impl RelayModel {
    fn incoming(sent: &Sent, i: usize) -> Incoming {
        let p2 = !matches!(sent.kind, Kind::V1 | Kind::V1Wrong);
        Incoming {
            window_id: sent.window_id.clone(),
            event_id: event_id(i),
            code: code(i),
            sender: "Bank".into(),
            received_at: p2.then_some(sent.phone_received),
            source: OtpSource::Sms,
            test: sent.kind == Kind::Test,
        }
    }

    fn send(s: &mut State, i: usize) {
        let event = Self::incoming(&s.sent[i], i);
        let before = s.store.pending_len();
        match s.store.receive(&event, s.now) {
            Ok(ack) if ack.duplicate => {
                s.saw_duplicate = true;
                if s.sent[i].accepted_epoch.is_some() && s.sent[i].accepted_restarts < s.restarts {
                    s.duplicate_after_restart = true;
                }
            }
            Ok(ack) if ack.test => {}
            Ok(_) => {
                if s.sent[i].accepted_epoch.is_none() {
                    s.sent[i].accepted_epoch = Some(s.disarms);
                    s.sent[i].accepted_arms = s.arms;
                    s.sent[i].accepted_restarts = s.restarts;
                }
                if before == s.store.limits().pending {
                    s.saw_overflow = true;
                }
            }
            Err(Refusal::Expired) => s.saw_expired = true,
            Err(Refusal::Paused) => s.saw_paused = true,
            Err(Refusal::EventLimit) => s.saw_event_limit = true,
            Err(Refusal::WrongWindow) => {}
            Err(Refusal::Invalid(e)) => panic!("model events are valid: {e}"),
        }
    }
}

impl Model for RelayModel {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        [(false, false), (true, false), (true, true)]
            .into_iter()
            .map(|(continuous, paused)| {
                let mut store = Store::new(continuous, paused).with_limits(self.limits);
                let mut windows = 0;
                if store.starts_armed() {
                    store.arm(0, T0, window_id(windows));
                    windows += 1;
                }
                State {
                    store,
                    now: T0,
                    ticks: 0,
                    arms: 0,
                    disarms: 0,
                    restarts: 0,
                    windows,
                    sent: Vec::new(),
                    delivered: Vec::new(),
                    late_delivery: false,
                    delivered_after_disarm: false,
                    unaccepted_delivery: false,
                    saw_duplicate: false,
                    saw_expired: false,
                    saw_paused: false,
                    saw_event_limit: false,
                    saw_overflow: false,
                    arm_dropped_code: false,
                    delivered_across_arm: false,
                    duplicate_after_restart: false,
                }
            })
            .collect()
    }

    fn actions(&self, s: &State, actions: &mut Vec<Action>) {
        if s.arms < self.max_arms {
            actions.push(Action::Arm);
        }
        if s.disarms < self.max_disarms {
            actions.push(Action::Disarm);
        }
        if s.sent.len() < self.max_events {
            actions.extend(self.kinds.iter().map(|k| Action::Incoming(*k)));
        }
        for (i, sent) in s.sent.iter().enumerate() {
            if s.now < sent.phone_received + CODE_TTL_MS {
                actions.push(Action::Resend(i as u8));
            }
        }
        actions.push(Action::Take);
        if s.ticks < self.max_ticks {
            actions.push(Action::Tick);
        }
        if s.restarts < self.max_restarts {
            actions.push(Action::Restart);
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut s = last.clone();
        match action {
            Action::Arm => {
                s.arms += 1;
                let fresh = s.store.fresh_len(s.now);
                s.store.arm(ARM_SECONDS, s.now, window_id(s.windows));
                if s.store.fresh_len(s.now) < fresh {
                    s.arm_dropped_code = true;
                }
                s.windows += 1;
            }
            Action::Restart => {
                s.restarts += 1;
                let old = &s.store;
                let mut store = Store::new(old.continuous(), old.paused()).with_limits(self.limits);
                store.restore_seen(old.seen().map(|(h, u)| (h.to_owned(), u)), s.now);
                if store.starts_armed() {
                    store.arm(0, s.now, window_id(s.windows));
                    s.windows += 1;
                }
                s.store = store;
            }
            Action::Disarm => {
                s.disarms += 1;
                s.store.disarm();
            }
            Action::Incoming(kind) => {
                let phone_received = match kind {
                    Kind::Late => s.now - 100_000,
                    Kind::Stale => s.now - CODE_TTL_MS,
                    _ => s.now,
                };
                let window_id = match kind {
                    // The phone fetched `/window` just before; unarmed it
                    // would not send, so send a stale id instead.
                    Kind::V1 => Some(
                        s.store
                            .window(s.now)
                            .window_id
                            .unwrap_or_else(|| window_id(u16::MAX)),
                    ),
                    Kind::V1Wrong => Some(WRONG_WINDOW.into()),
                    _ => None,
                };
                s.sent.push(Sent {
                    kind,
                    phone_received,
                    window_id,
                    accepted_epoch: None,
                    accepted_arms: 0,
                    accepted_restarts: 0,
                });
                let i = s.sent.len() - 1;
                Self::send(&mut s, i);
            }
            Action::Resend(i) => Self::send(&mut s, usize::from(i)),
            Action::Take => {
                if let Some(d) = s.store.take(s.now) {
                    let i: usize = d.code.parse::<usize>().ok()? - 100_000;
                    let sent = &s.sent[i];
                    if d.expires_at <= s.now {
                        s.late_delivery = true;
                    }
                    match sent.accepted_epoch {
                        None => s.unaccepted_delivery = true,
                        Some(epoch) if epoch < s.disarms => s.delivered_after_disarm = true,
                        Some(_) => {}
                    }
                    if sent.kind == Kind::Test {
                        s.unaccepted_delivery = true;
                    }
                    if sent.accepted_arms < s.arms {
                        s.delivered_across_arm = true;
                    }
                    s.delivered.push(i as u8);
                }
            }
            Action::Tick => {
                s.ticks += 1;
                s.now += TICK_MS;
            }
        }
        Some(s)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always("a code is delivered at most once", |_, s: &State| {
                s.delivered
                    .iter()
                    .enumerate()
                    .all(|(n, i)| !s.delivered[..n].contains(i))
            }),
            Property::always("nothing is delivered after it expires", |_, s: &State| {
                !s.late_delivery
            }),
            Property::always("nothing is delivered after disarm", |_, s: &State| {
                !s.delivered_after_disarm
            }),
            Property::always("test events are never delivered", |_, s: &State| {
                !s.delivered
                    .iter()
                    .any(|i| s.sent[usize::from(*i)].kind == Kind::Test)
            }),
            Property::always("only accepted codes are delivered", |_, s: &State| {
                !s.unaccepted_delivery
            }),
            Property::always("arming never drops a waiting code", |_, s: &State| {
                !s.arm_dropped_code
            }),
            Property::always(
                "pending never exceeds the limit",
                |m: &RelayModel, s: &State| {
                    s.store.pending_len() <= m.limits.pending && s.store.seen_len() <= m.limits.seen
                },
            ),
            Property::sometimes("a code is delivered", |_, s: &State| {
                !s.delivered.is_empty()
            }),
            Property::sometimes("pending reaches the limit", |m: &RelayModel, s: &State| {
                s.store.pending_len() == m.limits.pending
            }),
            Property::sometimes("the oldest code is dropped", |_, s: &State| s.saw_overflow),
        ];
        properties.extend((self.reachable)());
        properties
    }
}

fn delivered_kind(s: &State, kind: Kind) -> bool {
    s.delivered
        .iter()
        .any(|i| s.sent[usize::from(*i)].kind == kind)
}

/// Every action kind with small limits: 2 events against 1 pending slot and
/// 2 remembered ids, 3 ticks (120 s, past a window and a code's life), 2 arms
/// and 1 disarm.
#[test]
fn store_model_all_actions() {
    let model = RelayModel {
        limits: Limits {
            pending: 1,
            seen: 2,
        },
        kinds: &[
            Kind::Fresh,
            Kind::Late,
            Kind::Stale,
            Kind::Test,
            Kind::V1,
            Kind::V1Wrong,
        ],
        max_events: 2,
        max_ticks: 3,
        max_arms: 2,
        max_disarms: 1,
        max_restarts: 0,
        reachable: || {
            vec![
                Property::sometimes("a duplicate is acknowledged", |_, s: &State| {
                    s.saw_duplicate
                }),
                Property::sometimes("an expired code is refused", |_, s: &State| s.saw_expired),
                Property::sometimes("a paused relay refuses", |_, s: &State| s.saw_paused),
                Property::sometimes("a code is delivered after a re-arm", |_, s: &State| {
                    s.delivered.iter().any(|i| {
                        s.sent[usize::from(*i)]
                            .accepted_epoch
                            .is_some_and(|e| e > 0)
                    })
                }),
                Property::sometimes(
                    "a code waiting across an arm is delivered",
                    |_, s: &State| s.delivered_across_arm,
                ),
                Property::sometimes("a late code is delivered", |_, s: &State| {
                    delivered_kind(s, Kind::Late)
                }),
                Property::sometimes("a protocol-1 code is delivered", |_, s: &State| {
                    delivered_kind(s, Kind::V1)
                }),
            ]
        },
    };
    let checker = model.checker().spawn_bfs().join();
    checker.assert_properties();
    println!("all actions: {} states", checker.unique_state_count());
}

/// The seen-id limit: 3 events against 1 pending slot and 2 remembered ids.
#[test]
fn store_model_event_limit() {
    let model = RelayModel {
        limits: Limits {
            pending: 1,
            seen: 2,
        },
        kinds: &[Kind::Fresh, Kind::V1],
        max_events: 3,
        max_ticks: 2,
        max_arms: 1,
        max_disarms: 1,
        max_restarts: 0,
        reachable: || {
            vec![Property::sometimes(
                "the event limit refuses",
                |_, s: &State| s.saw_event_limit,
            )]
        },
    };
    let checker = model.checker().spawn_bfs().join();
    checker.assert_properties();
    println!("event limit: {} states", checker.unique_state_count());
}

/// The relay's real limit of 8 pending codes: 9 fresh codes and takes.
#[test]
fn store_model_real_pending_limit() {
    let model = RelayModel {
        limits: Limits::default(),
        kinds: &[Kind::Fresh],
        max_events: MAX_PENDING + 1,
        max_ticks: 0,
        max_arms: 0,
        max_disarms: 0,
        max_restarts: 0,
        reachable: Vec::new,
    };
    let checker = model.checker().spawn_bfs().join();
    checker.assert_properties();
    println!("real limits: {} states", checker.unique_state_count());
}

/// A relay restart (F3): seen ids survive it, codes do not. 2 events against
/// 1 pending slot, a restart, and an arm and a disarm around it.
#[test]
fn store_model_restart() {
    let model = RelayModel {
        limits: Limits {
            pending: 1,
            seen: 2,
        },
        kinds: &[Kind::Fresh, Kind::V1],
        max_events: 2,
        max_ticks: 2,
        max_arms: 1,
        max_disarms: 1,
        max_restarts: 1,
        reachable: || {
            vec![
                Property::sometimes("a retry after a restart is a duplicate", |_, s: &State| {
                    s.duplicate_after_restart
                }),
                Property::sometimes("a code is delivered after a restart", |_, s: &State| {
                    s.delivered
                        .iter()
                        .any(|i| s.sent[usize::from(*i)].accepted_restarts > 0)
                }),
            ]
        },
    };
    let checker = model.checker().spawn_bfs().join();
    checker.assert_properties();
    println!("restart: {} states", checker.unique_state_count());
}
