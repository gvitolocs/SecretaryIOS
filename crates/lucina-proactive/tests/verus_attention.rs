//! Differential tests for the attention engine against a plain-Rust copy of
//! the Verus kernel `verification/verus/attention_engine.rs` (branch
//! verus-proofs): `attention::gate`, the quiet-hours window, the verdict chain
//! of `score::evaluate`, and the score bounds/monotonicity on validated configs.

use chrono::{DateTime, Duration, Utc};
use lucina_proactive::attention::{
    gate, quiet_until, Device, Gate, GateInput, Ledger, Presence, Quiet, HOLD_FOCUS,
};
use lucina_proactive::config::{Thresholds, Weights};
use lucina_proactive::score::{evaluate, total, Verdict};
use lucina_proactive::{Config, Kind, Scores};
use proptest::prelude::*;

/// Plain-Rust copy of the Verus kernel.
mod kernel {
    pub const QUIET_DEFAULT_MS: i64 = 1_800_000;
    pub const RECHECK_MS: i64 = 300_000;

    #[derive(Debug, Clone)]
    pub struct GateIn {
        pub now: i64,
        pub expires_at: Option<i64>,
        pub muted_until: Option<i64>,
        pub quiet: bool,
        pub quiet_until: Option<i64>,
        pub presence_hold: bool,
        pub not_now_until: Option<i64>,
        pub last_sent_of_kind: Option<i64>,
        pub cooldown_ms: i64,
        pub sent_today: u32,
        pub max_per_day: u32,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Gate {
        Send,
        Hold(i64),
        Digest,
        Drop,
    }

    fn expired(expires_at: Option<i64>, t: i64) -> bool {
        match expires_at {
            Some(e) => e <= t,
            None => false,
        }
    }

    fn hold(expires_at: Option<i64>, until: i64) -> Gate {
        if expired(expires_at, until) {
            Gate::Digest
        } else {
            Gate::Hold(until)
        }
    }

    pub fn gate(g: &GateIn) -> Gate {
        let now = g.now;
        if expired(g.expires_at, now) {
            return Gate::Drop;
        }
        if let Some(m) = g.muted_until {
            if m > now {
                return Gate::Drop;
            }
        }
        if g.quiet {
            let until = g.quiet_until.unwrap_or(now + QUIET_DEFAULT_MS);
            return hold(g.expires_at, until);
        }
        if g.presence_hold {
            return hold(g.expires_at, now + RECHECK_MS);
        }
        if let Some(u) = g.not_now_until {
            if u > now {
                return hold(g.expires_at, u);
            }
        }
        if let Some(l) = g.last_sent_of_kind {
            if l + g.cooldown_ms > now {
                return Gate::Digest;
            }
        }
        if g.sent_today >= g.max_per_day {
            return Gate::Digest;
        }
        Gate::Send
    }

    pub fn quiet_inside(m: u32, start: u32, end: u32) -> bool {
        if start == end {
            false
        } else if start < end {
            m >= start && m < end
        } else {
            m >= start || m < end
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Verdict {
        Drop,
        Digest,
        Send,
    }

    /// The verdict chain (the kernel runs it on fixed-point scores; here on the
    /// same f32 values production compares).
    pub fn verdict(
        score: f32,
        confidence: f32,
        send: f32,
        digest: f32,
        min_confidence: f32,
    ) -> Verdict {
        if confidence < min_confidence {
            Verdict::Drop
        } else if score >= send {
            Verdict::Send
        } else if score >= digest {
            Verdict::Digest
        } else {
            Verdict::Drop
        }
    }

    pub fn rank(v: Verdict) -> u8 {
        match v {
            Verdict::Drop => 0,
            Verdict::Digest => 1,
            Verdict::Send => 2,
        }
    }
}

fn base() -> DateTime<Utc> {
    "2026-10-22T10:00:00Z".parse().unwrap()
}

fn ms(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

fn opt_minutes() -> impl Strategy<Value = Option<i64>> {
    proptest::option::of(-200i64..200)
}

#[derive(Debug, Clone)]
struct In {
    expires: Option<i64>,
    muted: Option<i64>,
    quiet: bool,
    quiet_until: Option<i64>,
    gaming: bool,
    dnd: bool,
    focus: Option<&'static str>,
    hold_gaming: bool,
    hold_dnd: bool,
    not_now: Option<i64>,
    last: Option<i64>,
    cooldown: u32,
    sent: u32,
    max: u32,
}

fn input() -> impl Strategy<Value = In> {
    (
        (opt_minutes(), opt_minutes(), any::<bool>(), opt_minutes()),
        (
            any::<bool>(),
            any::<bool>(),
            proptest::option::of(proptest::sample::select(vec![
                "sleep", "Driving", "work", "fitness", "personal",
            ])),
        ),
        (any::<bool>(), any::<bool>(), opt_minutes(), opt_minutes()),
        (0u32..300, 0u32..5, 0u32..5),
    )
        .prop_map(
            |(
                (expires, muted, quiet, quiet_until),
                (gaming, dnd, focus),
                (hold_gaming, hold_dnd, not_now, last),
                (cooldown, sent, max),
            )| In {
                expires,
                muted,
                quiet,
                quiet_until,
                gaming,
                dnd,
                focus,
                hold_gaming,
                hold_dnd,
                not_now,
                last,
                cooldown,
                sent,
                max,
            },
        )
}

fn at_min(m: i64) -> DateTime<Utc> {
    base() + Duration::minutes(m)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(5000))]

    #[test]
    fn gate_matches_kernel(i in input()) {
        let now = base();
        let mut cfg = Config::default();
        cfg.budget.max_per_day = i.max;
        cfg.budget.hold_while_gaming = i.hold_gaming;
        cfg.budget.hold_on_discord_dnd = i.hold_dnd;
        cfg.budget.cooldown_minutes.insert(Kind::ReminderOverdue.as_str().to_owned(), i.cooldown);
        let quiet = Quiet {
            quiet: i.quiet,
            until: i.quiet_until.map(at_min),
            source: "server".into(),
            ..Quiet::default()
        };
        let presence = Presence {
            status: if i.dnd { "dnd".into() } else { "online".into() },
            gaming: i.gaming.then(|| "Hades".to_owned()),
            online_since: None,
        };
        let device = Device { focus: i.focus.map(str::to_owned) };
        let ledger = Ledger {
            sent_today: i.sent,
            last_sent_of_kind: i.last.map(at_min),
            not_now_until: i.not_now.map(at_min),
            muted_until: i.muted.map(at_min),
        };
        let p = gate(&cfg, now, &GateInput {
            kind: Kind::ReminderOverdue,
            expires_at: i.expires.map(at_min),
            quiet: &quiet,
            presence: &presence,
            device: &device,
            ledger: &ledger,
        });
        let focus_hold = i.focus.is_some_and(|f| HOLD_FOCUS.contains(&f.to_ascii_lowercase().as_str()));
        let k = kernel::gate(&kernel::GateIn {
            now: ms(now),
            expires_at: i.expires.map(|m| ms(at_min(m))),
            muted_until: i.muted.map(|m| ms(at_min(m))),
            quiet: i.quiet,
            quiet_until: i.quiet_until.map(|m| ms(at_min(m))),
            presence_hold: (i.hold_gaming && i.gaming) || (i.hold_dnd && i.dnd) || focus_hold,
            not_now_until: i.not_now.map(|m| ms(at_min(m))),
            last_sent_of_kind: i.last.map(|m| ms(at_min(m))),
            cooldown_ms: i64::from(i.cooldown) * 60_000,
            sent_today: i.sent,
            max_per_day: i.max,
        });
        let p = match p {
            Gate::Send => kernel::Gate::Send,
            Gate::Hold { until, .. } => kernel::Gate::Hold(ms(until)),
            Gate::Digest { .. } => kernel::Gate::Digest,
            Gate::Drop { .. } => kernel::Gate::Drop,
        };
        prop_assert_eq!(p, k, "{:?}", i);
    }

    #[test]
    fn quiet_window_matches_kernel(minute in 0u32..1440, start in 0u32..1440, end in 0u32..1440) {
        let now = "2026-10-22T00:00:00Z".parse::<DateTime<Utc>>().unwrap() + Duration::minutes(i64::from(minute));
        let p = quiet_until(now, chrono_tz::UTC, start, end).is_some();
        prop_assert_eq!(p, kernel::quiet_inside(minute, start, end));
    }

    /// The verdict chain, and E1/E2 on production: with weights that pass
    /// Config::validate, the score is in [0, 1] and raising any input never
    /// lowers the verdict.
    #[test]
    fn evaluate_matches_kernel_and_is_monotone(
        w in (0u32..=1000, 0u32..=1000, 0u32..=1000, 0u32..=1000),
        s in (0.0f32..=1.0, 0.0f32..=1.0, 0.0f32..=1.0, 0.0f32..=1.0),
        bump in (0.0f32..=0.5, 0.0f32..=0.5, 0.0f32..=0.5, 0.0f32..=0.5),
        t in (0.0f32..=1.0, 0.0f32..=1.0, 0.0f32..=1.0, 0.0f32..=1.0),
    ) {
        let sum = (w.0 + w.1 + w.2 + w.3).max(1) as f32;
        let (send, digest) = if t.0 >= t.1 { (t.0, t.1) } else { (t.1, t.0) };
        let cfg = Config {
            weights: Weights {
                urgency: w.0 as f32 / sum,
                relevance: w.1 as f32 / sum,
                confidence: w.2 as f32 / sum,
                actionability: w.3 as f32 / sum,
            },
            thresholds: Thresholds { send, digest, min_confidence: t.2, high_priority: t.3 },
            ..Config::default()
        };
        prop_assume!(cfg.validate().is_ok());
        let a = Scores::new(s.0, s.1, s.2, s.3);
        let b = Scores::new(s.0 + bump.0, s.1 + bump.1, s.2 + bump.2, s.3 + bump.3);
        let ea = evaluate(&cfg.weights, &cfg.thresholds, &a);
        let eb = evaluate(&cfg.weights, &cfg.thresholds, &b);
        for (e, sc) in [(&ea, &a), (&eb, &b)] {
            prop_assert!((0.0..=1.0).contains(&e.score), "score {}", e.score);
            let k = kernel::verdict(e.score, sc.confidence, send, digest, t.2);
            let p = match e.verdict { Verdict::Drop => kernel::Verdict::Drop, Verdict::Digest => kernel::Verdict::Digest, Verdict::Send => kernel::Verdict::Send };
            prop_assert_eq!(k, p);
        }
        prop_assert!(total(&cfg.weights, &a) <= total(&cfg.weights, &b));
        let rank = |v: Verdict| kernel::rank(match v { Verdict::Drop => kernel::Verdict::Drop, Verdict::Digest => kernel::Verdict::Digest, Verdict::Send => kernel::Verdict::Send });
        prop_assert!(rank(ea.verdict) <= rank(eb.verdict));
    }

    /// E1 counterexamples are rejected by Config::validate now.
    #[test]
    fn validate_rejects_weights_outside_the_simplex(u in -2.0f32..2.0, r in -2.0f32..2.0) {
        let cfg = Config {
            weights: Weights { urgency: u, relevance: r, confidence: 0.0, actionability: 0.0 },
            ..Config::default()
        };
        if cfg.validate().is_ok() {
            for s in [Scores::new(1.0, 1.0, 1.0, 1.0), Scores::new(0.0, 1.0, 1.0, 1.0), Scores::new(1.0, 0.0, 1.0, 1.0)] {
                let raw = u * s.urgency + r * s.relevance;
                prop_assert!((0.0..=1.011).contains(&raw), "validated weights {u} {r} give {raw}");
                prop_assert!((0.0..=1.0).contains(&total(&cfg.weights, &s)));
            }
        }
    }
}

#[test]
fn old_counterexamples_fail_validation() {
    let mut cfg = Config::default();
    for w in [
        (1.5, -0.5, 0.0, 0.0),
        (f32::NAN, 0.5, 0.25, 0.25),
        (1.0, 0.0, 0.0, f32::NAN),
    ] {
        cfg.weights = Weights {
            urgency: w.0,
            relevance: w.1,
            confidence: w.2,
            actionability: w.3,
        };
        assert!(cfg.validate().is_err(), "{w:?}");
    }
    // the clamp: weights summing to 1.005 still give a score <= 1
    let w = Weights {
        urgency: 0.355,
        relevance: 0.25,
        confidence: 0.2,
        actionability: 0.2,
    };
    assert!(total(&w, &Scores::new(1.0, 1.0, 1.0, 1.0)) <= 1.0);
}
