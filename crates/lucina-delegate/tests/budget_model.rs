//! Stateright model of the delegation budget across a day change (finding B1,
//! docs/verification-gaps.md).
//!
//! `Delegator::start` claims the agent (one job per agent), reserves one call
//! of today's budget (`StateStore::reserve`), then runs Lucina's self-review
//! (a model call) and prepares the sandbox (a git clone) before the job runs.
//! When the review skips the job, or the sandbox fails, it gives the call back
//! with `StateStore::release`, which decrements the counters of the day the
//! release runs on. If midnight passed in between, that is the new day: its
//! shared `total_calls` loses a call it never counted, and the new day can
//! start one delegation more than `total_calls_per_day` per such release.
//!
//! Actors: two agents, each Idle -> Reserved(day) -> (Skip -> Idle | Run ->
//! Running(day) -> Finish -> Idle), and the clock (`Midnight`, three days).
//! The counters are stepped with the Verus kernel's `reserve`, `release` and
//! `roll` (`verification/verus/delegate_limits.rs`, diff-tested against the
//! real StateStore in `verus_limits.rs`). Ghost: the jobs that ran, by the day
//! of their reservation.
//!
//! Properties:
//! - `day_total_within_cap`: the jobs run on a day's reservations are at most
//!   the total cap. **Violated** in the current code (B1, kept as the witness);
//!   holds with the fix switch (release only counts against the day of its
//!   reservation).
//! - `agent_within_cap`: per agent, holds in both (the one-job-per-agent claim
//!   means an agent has no other reservation on the new day to undercount).
//! - sometimes: a job is in flight at midnight, a skipped job gives its call
//!   back, a cap hit pauses delegation, a day reaches its cap exactly.

use stateright::{Checker, Model, Property};

/// The kernel's exec part (copy of `verification/verus/delegate_limits.rs`).
mod kernel {
    pub struct Caps {
        pub total: u32,
        pub calls: u32,
    }

    #[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
    pub struct Day {
        pub paused_all: bool,
        pub paused_agent: bool,
        pub calls: u32,
        pub total: u32,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Outcome {
        Ok,
        Paused,
        Hit,
    }

    /// `reserve` with the switches off and no wall/turn caps (they do not change
    /// the counting).
    pub fn reserve(d: &Day, c: &Caps) -> (Outcome, Day) {
        if d.paused_all || d.paused_agent {
            return (Outcome::Paused, *d);
        }
        if d.total >= c.total {
            return (
                Outcome::Hit,
                Day {
                    paused_all: true,
                    ..*d
                },
            );
        }
        if d.calls >= c.calls {
            return (
                Outcome::Hit,
                Day {
                    paused_agent: true,
                    ..*d
                },
            );
        }
        (
            Outcome::Ok,
            Day {
                calls: d.calls + 1,
                total: d.total + 1,
                ..*d
            },
        )
    }

    pub fn release(d: &Day) -> Day {
        Day {
            calls: d.calls.saturating_sub(1),
            total: d.total.saturating_sub(1),
            ..*d
        }
    }
}

const AGENTS: usize = 2;
const DAYS: usize = 3;
const TOTAL_CAP: u32 = 2;
const AGENT_CAP: u32 = 2;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Job {
    Idle,
    Reserved(u8),
    Running(u8),
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct State {
    day: u8,
    paused_all: bool,
    paused: [bool; AGENTS],
    calls: [u32; AGENTS],
    total: u32,
    jobs: [Job; AGENTS],
    // ghost
    ran: [u8; DAYS],
    ran_agent: [[u8; AGENTS]; DAYS],
    in_flight_at_midnight: bool,
    released: bool,
    hit: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    Reserve(usize),
    Skip(usize),
    Run(usize),
    Finish(usize),
    Midnight,
}

struct Budget {
    /// Fix switch: `release` counts against the reservation's day only.
    fixed: bool,
}

impl Budget {
    fn view(s: &State, a: usize) -> kernel::Day {
        kernel::Day {
            paused_all: s.paused_all,
            paused_agent: s.paused[a],
            calls: s.calls[a],
            total: s.total,
        }
    }
    fn set(s: &mut State, a: usize, d: kernel::Day) {
        s.paused_all = d.paused_all;
        s.paused[a] = d.paused_agent;
        s.calls[a] = d.calls;
        s.total = d.total;
    }
}

impl Model for Budget {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            day: 0,
            paused_all: false,
            paused: [false; AGENTS],
            calls: [0; AGENTS],
            total: 0,
            jobs: [Job::Idle; AGENTS],
            ran: [0; DAYS],
            ran_agent: [[0; AGENTS]; DAYS],
            in_flight_at_midnight: false,
            released: false,
            hit: false,
        }]
    }

    fn actions(&self, s: &State, out: &mut Vec<Action>) {
        for a in 0..AGENTS {
            match s.jobs[a] {
                Job::Idle => out.push(Action::Reserve(a)),
                Job::Reserved(_) => {
                    out.push(Action::Skip(a));
                    out.push(Action::Run(a));
                }
                Job::Running(_) => out.push(Action::Finish(a)),
            }
        }
        if (s.day as usize) + 1 < DAYS {
            out.push(Action::Midnight);
        }
    }

    fn next_state(&self, s: &State, action: Action) -> Option<State> {
        let mut n = s.clone();
        match action {
            Action::Reserve(a) => {
                let caps = kernel::Caps {
                    total: TOTAL_CAP,
                    calls: AGENT_CAP,
                };
                let (o, d) = kernel::reserve(&Self::view(s, a), &caps);
                Self::set(&mut n, a, d);
                match o {
                    kernel::Outcome::Ok => n.jobs[a] = Job::Reserved(s.day),
                    kernel::Outcome::Hit => n.hit = true,
                    kernel::Outcome::Paused => {}
                }
            }
            Action::Skip(a) => {
                let Job::Reserved(d) = s.jobs[a] else {
                    return None;
                };
                if !self.fixed || d == s.day {
                    let r = kernel::release(&Self::view(s, a));
                    Self::set(&mut n, a, r);
                }
                n.jobs[a] = Job::Idle;
                n.released = true;
            }
            Action::Run(a) => {
                let Job::Reserved(d) = s.jobs[a] else {
                    return None;
                };
                n.jobs[a] = Job::Running(d);
                n.ran[d as usize] += 1;
                n.ran_agent[d as usize][a] += 1;
            }
            Action::Finish(a) => n.jobs[a] = Job::Idle,
            Action::Midnight => {
                n.day += 1;
                // `with`: usage, total and pauses cleared.
                n.paused_all = false;
                n.paused = [false; AGENTS];
                n.calls = [0; AGENTS];
                n.total = 0;
                if s.jobs.iter().any(|j| matches!(j, Job::Reserved(_))) {
                    n.in_flight_at_midnight = true;
                }
            }
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always("day_total_within_cap", |_, s: &State| {
                s.ran.iter().all(|&r| u32::from(r) <= TOTAL_CAP)
            }),
            Property::always("agent_within_cap", |_, s: &State| {
                s.ran_agent
                    .iter()
                    .all(|d| d.iter().all(|&r| u32::from(r) <= AGENT_CAP))
            }),
            Property::sometimes("a job is in flight at midnight", |_, s: &State| {
                s.in_flight_at_midnight
            }),
            Property::sometimes("a skipped job gives its call back", |_, s: &State| {
                s.released
            }),
            Property::sometimes("a cap hit pauses delegation", |_, s: &State| s.hit),
            Property::sometimes("a day reaches its cap exactly", |_, s: &State| {
                s.ran.iter().any(|&r| u32::from(r) == TOTAL_CAP)
            }),
        ]
    }
}

const WITNESSES: [&str; 4] = [
    "a job is in flight at midnight",
    "a skipped job gives its call back",
    "a cap hit pauses delegation",
    "a day reaches its cap exactly",
];

/// B1, current code: the witness must keep failing until the code is fixed
/// (then flip `fixed` here and in the doc).
#[test]
fn budget_release_across_midnight_current_code_violates_the_cap() {
    let checker = Budget { fixed: false }.checker().spawn_bfs().join();
    let trace = checker.assert_any_discovery("day_total_within_cap");
    println!("B1 counterexample: {:?}", trace.into_actions());
    checker.assert_no_discovery("agent_within_cap");
    for w in WITNESSES {
        checker.assert_any_discovery(w);
    }
    println!("current: {} states", checker.unique_state_count());
}

#[test]
fn budget_release_against_its_own_day_holds() {
    let checker = Budget { fixed: true }.checker().spawn_bfs().join();
    checker.assert_properties();
    println!("fixed: {} states", checker.unique_state_count());
}
