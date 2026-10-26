//! Differential test: `policy::hard_limits` and `state::StateStore`
//! (reserve, release, record, the owner's switches, the day change) against a
//! plain-Rust copy of the Verus kernel `verification/verus/delegate_limits.rs`.
//!
//! - hard_limits: random requests (every origin and kind, depths, the
//!   delegation marker, money and non-money tasks, callers that name the
//!   target, workspaces, max_depth 0..3). The kernel's inputs `money` and
//!   `loop_hit` are computed with the real `money_intent` and the caller test.
//! - the budget: random operations on an in-memory `StateStore` with small
//!   caps, over three days; every reserve's outcome (ok, stopped, agent off,
//!   paused, paused with a notice) and the counts after every operation must
//!   equal the kernel's. Releases here happen on the day of their reserve; the
//!   release across midnight is the Stateright model `budget_model.rs`.

use chrono::{DateTime, Duration, TimeZone, Utc};
use lucina_delegate::config::{AgentBudget, Budget};
use lucina_delegate::policy::{self, money_intent, Refusal, MARKER};
use lucina_delegate::state::StateStore;
use lucina_delegate::{AgentId, Kind, Origin, Request};

/// Plain-Rust copy of the Verus kernel (exec part).
mod kernel {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Origin {
        Owner,
        Mcp,
        Proactive,
        Delegated,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Refusal {
        Empty,
        McpOrigin,
        Depth,
        Loop,
        Money,
        ProactiveReadOnly,
    }

    pub struct Req {
        pub empty: bool,
        pub origin: Origin,
        pub depth: u8,
        pub marker: bool,
        pub loop_hit: bool,
        pub money: bool,
        pub read_only: bool,
        pub workspace: bool,
    }

    pub fn hard_limits(r: &Req, max_depth: u8) -> Result<(), Refusal> {
        if r.empty {
            return Err(Refusal::Empty);
        }
        if r.origin == Origin::Mcp {
            return Err(Refusal::McpOrigin);
        }
        let m = if max_depth < 1 { max_depth } else { 1 };
        if r.origin == Origin::Delegated || r.depth >= m || r.marker {
            return Err(Refusal::Depth);
        }
        if r.loop_hit {
            return Err(Refusal::Loop);
        }
        if r.money {
            return Err(Refusal::Money);
        }
        if r.origin == Origin::Proactive && (!r.read_only || r.workspace) {
            return Err(Refusal::ProactiveReadOnly);
        }
        Ok(())
    }

    pub struct Caps {
        pub total: u32,
        pub calls: u32,
        pub wall_minutes: u32,
        pub turns: u32,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Day {
        pub stopped: bool,
        pub disabled: bool,
        pub paused_all: bool,
        pub paused_agent: bool,
        pub calls: u32,
        pub wall_ms: u64,
        pub turns: u32,
        pub total: u32,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Outcome {
        Ok,
        Stopped,
        AgentOff,
        Paused,
        Hit { all: bool, notice: bool },
    }

    pub fn reserve(d: &Day, c: &Caps) -> (Outcome, Day) {
        let same = *d;
        if d.stopped {
            return (Outcome::Stopped, same);
        }
        if d.disabled {
            return (Outcome::AgentOff, same);
        }
        if d.paused_all || d.paused_agent {
            return (Outcome::Paused, same);
        }
        if d.total >= c.total {
            let after = Day {
                paused_all: true,
                ..same
            };
            return (
                Outcome::Hit {
                    all: true,
                    notice: true,
                },
                after,
            );
        }
        if d.calls >= c.calls
            || d.wall_ms >= (c.wall_minutes as u64) * 60_000
            || (c.turns > 0 && d.turns >= c.turns)
        {
            let after = Day {
                paused_agent: true,
                ..same
            };
            return (
                Outcome::Hit {
                    all: false,
                    notice: true,
                },
                after,
            );
        }
        let after = Day {
            calls: d.calls + 1,
            total: d.total + 1,
            ..same
        };
        (Outcome::Ok, after)
    }

    pub fn release(d: &Day) -> Day {
        Day {
            calls: d.calls.saturating_sub(1),
            total: d.total.saturating_sub(1),
            ..*d
        }
    }

    pub fn roll(d: &Day) -> Day {
        Day {
            paused_all: false,
            paused_agent: false,
            calls: 0,
            wall_ms: 0,
            turns: 0,
            total: 0,
            ..*d
        }
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const TASKS: &[&str] = &[
    "what changed in the Rust 2024 edition?",
    "   ",
    "",
    "buy me a new laptop",
    "which laptop should I buy?",
    "send money to Marco",
    "compra le azioni Tesla",
    "[Lucina delegation · job x] summarise this",
    "refactor the parser",
    "pay for the hotel",
    "latest news on the election",
];
const CALLERS: &[Option<&str>] = &[
    None,
    Some("claude-code"),
    Some("grok"),
    Some("ChatGPT"),
    Some("someone"),
    Some("x.ai"),
];

#[test]
fn hard_limits_agree_with_the_kernel() {
    let mut rng = Rng(0xde1e_9a7e);
    let origins = [
        Origin::Owner,
        Origin::Mcp,
        Origin::Proactive,
        Origin::Delegated,
    ];
    let kinds = [Kind::Question, Kind::Research, Kind::Analysis, Kind::Code];
    let mut ok = 0;
    for _ in 0..50_000 {
        let mut req = Request::owner(kinds[rng.below(4)], TASKS[rng.below(TASKS.len())]);
        req.origin = origins[rng.below(4)];
        req.depth = [0, 0, 0, 1, 2][rng.below(5)];
        req.caller = CALLERS[rng.below(CALLERS.len())].map(str::to_owned);
        if rng.below(4) == 0 {
            req.workspace = Some("/home/x/repo".into());
        }
        let target = [
            None,
            Some(AgentId::Claude),
            Some(AgentId::Grok),
            Some(AgentId::Chatgpt),
        ][rng.below(4)];
        let max_depth = [0u8, 1, 1, 2, 3][rng.below(5)];

        let loop_hit = match (target, req.caller.as_deref()) {
            (Some(t), Some(c)) => {
                AgentId::parse(c) == Some(t) || c.to_lowercase().contains(t.as_str())
            }
            _ => false,
        };
        let k = kernel::Req {
            empty: req.task.trim().is_empty(),
            origin: match req.origin {
                Origin::Owner => kernel::Origin::Owner,
                Origin::Mcp => kernel::Origin::Mcp,
                Origin::Proactive => kernel::Origin::Proactive,
                Origin::Delegated => kernel::Origin::Delegated,
            },
            depth: req.depth,
            marker: req.task.contains(MARKER),
            loop_hit,
            money: money_intent(&req.task),
            read_only: req.kind.read_only(),
            workspace: req.workspace.is_some(),
        };
        let want = kernel::hard_limits(&k, max_depth);
        let got = policy::hard_limits(&req, target, max_depth).map_err(|r| match r {
            Refusal::Empty => kernel::Refusal::Empty,
            Refusal::McpOrigin => kernel::Refusal::McpOrigin,
            Refusal::Depth => kernel::Refusal::Depth,
            Refusal::Loop(_) => kernel::Refusal::Loop,
            Refusal::Money => kernel::Refusal::Money,
            Refusal::ProactiveReadOnly => kernel::Refusal::ProactiveReadOnly,
            other => panic!("unexpected {other:?}"),
        });
        assert_eq!(got, want, "{req:?} target {target:?} max_depth {max_depth}");
        ok += usize::from(got.is_ok());
    }
    assert!(ok > 1_000, "not vacuous: {ok}");
}

fn budget() -> Budget {
    Budget {
        total_calls_per_day: 6,
        claude: AgentBudget {
            calls_per_day: 3,
            wall_minutes_per_day: 2,
            turns_per_day: 5,
        },
        grok: AgentBudget {
            calls_per_day: 4,
            wall_minutes_per_day: 1,
            turns_per_day: 0,
        },
        chatgpt: AgentBudget {
            calls_per_day: 2,
            wall_minutes_per_day: 3,
            turns_per_day: 0,
        },
    }
}

/// The test's mirror of one StateFile, stepped with the kernel.
#[derive(Clone, Default)]
struct Mirror {
    stopped: bool,
    disabled: [bool; 3],
    paused_all: bool,
    paused: [bool; 3],
    calls: [u32; 3],
    wall_ms: [u64; 3],
    turns: [u32; 3],
    total: u32,
}

impl Mirror {
    fn day(&self, a: usize) -> kernel::Day {
        kernel::Day {
            stopped: self.stopped,
            disabled: self.disabled[a],
            paused_all: self.paused_all,
            paused_agent: self.paused[a],
            calls: self.calls[a],
            wall_ms: self.wall_ms[a],
            turns: self.turns[a],
            total: self.total,
        }
    }
    fn set(&mut self, a: usize, d: kernel::Day) {
        self.stopped = d.stopped;
        self.disabled[a] = d.disabled;
        self.paused_all = d.paused_all;
        self.paused[a] = d.paused_agent;
        self.calls[a] = d.calls;
        self.wall_ms[a] = d.wall_ms;
        self.turns[a] = d.turns;
        self.total = d.total;
    }
    fn roll(&mut self) {
        for a in 0..3 {
            let d = kernel::roll(&self.day(a));
            self.set(a, d);
        }
    }
}

const AGENTS: [AgentId; 3] = [AgentId::Claude, AgentId::Grok, AgentId::Chatgpt];

fn caps(b: &Budget, a: usize) -> kernel::Caps {
    let ab = b.of(AGENTS[a]);
    kernel::Caps {
        total: b.total_calls_per_day,
        calls: ab.calls_per_day,
        wall_minutes: ab.wall_minutes_per_day,
        turns: ab.turns_per_day,
    }
}

#[test]
fn budget_operations_agree_with_the_kernel() {
    let b = budget();
    let mut rng = Rng(0x00b0_d9e7);
    let mut seen = [0usize; 5];
    for run in 0..40u32 {
        let store = StateStore::new(None, chrono_tz::UTC);
        let mut m = Mirror::default();
        let start: DateTime<Utc> =
            Utc.with_ymd_and_hms(2026, 10, 1, 8, 0, 0).unwrap() + Duration::days(i64::from(run));
        let mut now = start;
        let mut day = now.date_naive();
        for _ in 0..120 {
            now += if rng.below(25) == 0 {
                Duration::hours(13)
            } else {
                Duration::seconds(rng.below(600) as i64)
            };
            if now.date_naive() != day {
                day = now.date_naive();
                m.roll();
            }
            let a = rng.below(3);
            let agent = AGENTS[a];
            match rng.below(10) {
                0..=4 => {
                    let (want, after) = kernel::reserve(&m.day(a), &caps(&b, a));
                    m.set(a, after);
                    let got = store.reserve(agent, &b, now);
                    let got_k = match &got {
                        Ok(()) => kernel::Outcome::Ok,
                        Err((Refusal::Stopped, None)) => kernel::Outcome::Stopped,
                        Err((Refusal::AgentOff(_), None)) => kernel::Outcome::AgentOff,
                        Err((Refusal::Paused(_), None)) => kernel::Outcome::Paused,
                        Err((Refusal::Paused(_), Some(n))) => kernel::Outcome::Hit {
                            all: n.scope == "all",
                            notice: true,
                        },
                        Err(other) => panic!("unexpected {other:?}"),
                    };
                    assert_eq!(got_k, want, "run {run} {agent:?} at {now}");
                    seen[match want {
                        kernel::Outcome::Ok => 0,
                        kernel::Outcome::Stopped => 1,
                        kernel::Outcome::AgentOff => 2,
                        kernel::Outcome::Paused => 3,
                        kernel::Outcome::Hit { .. } => 4,
                    }] += 1;
                    if want == kernel::Outcome::Ok && rng.below(3) == 0 {
                        // The self-review skipped it: give the call back, same day.
                        let d = kernel::release(&m.day(a));
                        m.set(a, d);
                        store.release(agent, now);
                    }
                }
                5..=6 => {
                    let (wall, turns) = (rng.below(50_000) as u64, rng.below(3) as u32);
                    m.wall_ms[a] += wall;
                    m.turns[a] += turns;
                    store.record(agent, wall, turns, now);
                }
                7 => {
                    let on = rng.below(3) != 0;
                    if rng.below(2) == 0 {
                        if on {
                            m.disabled[a] = false;
                            m.stopped = false;
                        } else {
                            m.disabled[a] = true;
                        }
                        store.set_enabled(Some(agent), on, now);
                    } else {
                        m.stopped = !on;
                        store.set_enabled(None, on, now);
                    }
                }
                _ => {
                    let d = kernel::release(&m.day(a));
                    m.set(a, d);
                    store.release(agent, now);
                }
            }
            for (i, ag) in AGENTS.iter().enumerate() {
                let (u, total) = store.usage(*ag, now);
                assert_eq!(
                    (u.calls, u.wall_ms, u.turns, total),
                    (m.calls[i], m.wall_ms[i], m.turns[i], m.total),
                    "run {run} {ag:?} at {now}"
                );
            }
        }
    }
    assert!(seen.iter().all(|&n| n > 20), "every outcome: {seen:?}");
}
