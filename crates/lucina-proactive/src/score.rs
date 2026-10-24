//! Deterministic scoring. No model is involved: every source turns its
//! signal into four inputs in `[0, 1]` with the functions below, and
//!
//! ```text
//! score = 0.35·urgency + 0.25·relevance + 0.20·confidence + 0.20·actionability
//! ```
//!
//! (weights in `[weights]`). Then, with `[thresholds]`:
//!
//! | rule | verdict |
//! |---|---|
//! | confidence < `min_confidence` (0.60) | drop ("not verified enough") |
//! | score ≥ `send` (0.70) | send now, if the attention gate agrees |
//! | score ≥ `digest` (0.45) | Daily Pulse |
//! | otherwise | drop |
//!
//! Priority: `high` at score ≥ `high_priority` (0.85), `normal` for a send,
//! `low` for the Pulse.
//!
//! The inputs per kind (u, r, c, a):
//!
//! | kind | urgency | relevance | confidence | actionability |
//! |---|---|---|---|---|
//! | meeting_prep, with material | 1 − 0.4·(min until / max lead), 0.3 beyond | 0.80 | 0.95 (calendar) | 0.90 |
//! | meeting_prep, nothing to prepare | half of the above | 0.60 | 0.95 | 0.00 |
//! | reminder_overdue | 0.6 + 0.4·(min late / 240), ≤ 1 | 0.70 | 0.95 with a delivery error, else 0.85 | 0.70 |
//! | due_no_plan | 0.70 ≤ 24 h, 0.55 ≤ 48 h, else 0.45 | 0.60 | 0.80 | 0.60 |
//! | note_for_event | 0.60 ≤ 3 h, 0.45 ≤ 24 h, else 0.30 | 0.70 | 0.75 | 0.60 |
//! | thread_resolved | 0.60 if he asked to be told, else 0.35 | 0.90 / 0.70 | the judge's confidence | 0.60 / 0.30 |
//! | thread_open | 0.25 | 0.60 | the judge's confidence | 0.40 |
//! | service_down | 0.90, 1.0 after 30 min | 0.95 own product, else 0.80 | 0.90 (hysteresis) | 0.60 |
//! | service_recovered | 0.70 if we alerted, else 0.30 | 0.90 / 0.60 | 0.90 | 0.40 / 0.10 |
//!
//! What that gives with the default weights (examples in the tests):
//! a meeting in 60 min with a related note scores 0.83 (send), the same
//! meeting with nothing to prepare 0.50 (Pulse: the calendar app already
//! reminds him), a sustained outage of pokoin.com 0.85 (send, high).

use crate::config::{Thresholds, Weights};
use crate::model::{Priority, Scores};

/// What the score alone says (the attention gate may still hold or
/// downgrade a `Send`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Drop,
    Digest,
    Send,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub score: f32,
    pub verdict: Verdict,
    pub priority: Priority,
    pub reason: String,
}

/// The weighted sum, in `[0, 1]`: validated weights may sum to 1 ± 0.01 and
/// f32 rounding may land just above 1, so it is clamped (NaN counts as 0).
pub fn total(w: &Weights, s: &Scores) -> f32 {
    let t = w.urgency * s.urgency
        + w.relevance * s.relevance
        + w.confidence * s.confidence
        + w.actionability * s.actionability;
    if t > 0.0 {
        t.min(1.0)
    } else {
        0.0
    }
}

pub fn evaluate(w: &Weights, t: &Thresholds, s: &Scores) -> Scored {
    let score = total(w, s);
    let inputs = format!(
        "u {:.2}, r {:.2}, c {:.2}, a {:.2}",
        s.urgency, s.relevance, s.confidence, s.actionability
    );
    let (verdict, reason) = if s.confidence < t.min_confidence {
        (
            Verdict::Drop,
            format!(
                "not verified enough: confidence {:.2} < {:.2} ({inputs})",
                s.confidence, t.min_confidence
            ),
        )
    } else if score >= t.send {
        (
            Verdict::Send,
            format!("score {score:.2} ≥ send {:.2} ({inputs})", t.send),
        )
    } else if score >= t.digest {
        (
            Verdict::Digest,
            format!(
                "score {score:.2} ≥ digest {:.2}, < send {:.2}: for the Daily Pulse ({inputs})",
                t.digest, t.send
            ),
        )
    } else {
        (
            Verdict::Drop,
            format!("score {score:.2} < digest {:.2} ({inputs})", t.digest),
        )
    };
    let priority = match verdict {
        Verdict::Send if score >= t.high_priority => Priority::High,
        Verdict::Send => Priority::Normal,
        _ => Priority::Low,
    };
    Scored {
        score,
        verdict,
        priority,
        reason,
    }
}

// ------------------------------------------------------------------ inputs per kind

pub fn meeting_prep(minutes_until: i64, max_lead: i64, has_material: bool) -> Scores {
    let max_lead = max_lead.max(1);
    let urgency = if minutes_until <= max_lead {
        1.0 - 0.4 * (minutes_until.max(0) as f32 / max_lead as f32)
    } else {
        0.3
    };
    if has_material {
        Scores::new(urgency, 0.80, 0.95, 0.90)
    } else {
        // The calendar app already reminds him; at most Pulse material.
        Scores::new(urgency * 0.5, 0.60, 0.95, 0.0)
    }
}

pub fn reminder_overdue(minutes_late: i64, delivery_error: bool) -> Scores {
    let urgency = (0.6 + 0.4 * (minutes_late.max(0) as f32 / 240.0)).min(1.0);
    Scores::new(
        urgency,
        0.70,
        if delivery_error { 0.95 } else { 0.85 },
        0.70,
    )
}

pub fn due_no_plan(hours_left: i64) -> Scores {
    let urgency = match hours_left {
        h if h <= 24 => 0.70,
        h if h <= 48 => 0.55,
        _ => 0.45,
    };
    Scores::new(urgency, 0.60, 0.80, 0.60)
}

pub fn note_for_event(hours_until: i64) -> Scores {
    let urgency = match hours_until {
        h if h <= 3 => 0.60,
        h if h <= 24 => 0.45,
        _ => 0.30,
    };
    Scores::new(urgency, 0.70, 0.75, 0.60)
}

pub fn thread_resolved(judge_confidence: f32, asked_to_be_told: bool) -> Scores {
    if asked_to_be_told {
        Scores::new(0.60, 0.90, judge_confidence, 0.60)
    } else {
        Scores::new(0.35, 0.70, judge_confidence, 0.30)
    }
}

pub fn thread_open(judge_confidence: f32) -> Scores {
    Scores::new(0.25, 0.60, judge_confidence, 0.40)
}

pub fn service_down(minutes_down: i64, own_product: bool) -> Scores {
    let urgency = if minutes_down >= 30 { 1.0 } else { 0.90 };
    Scores::new(urgency, if own_product { 0.95 } else { 0.80 }, 0.90, 0.60)
}

pub fn service_recovered(we_alerted: bool) -> Scores {
    if we_alerted {
        Scores::new(0.70, 0.90, 0.90, 0.40)
    } else {
        Scores::new(0.30, 0.60, 0.90, 0.10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(s: Scores) -> Scored {
        evaluate(&Weights::default(), &Thresholds::default(), &s)
    }

    #[test]
    fn scoring_table_examples() {
        let m = eval(meeting_prep(60, 90, true));
        assert!((m.score - 0.8267).abs() < 0.001, "{}", m.score);
        assert_eq!((m.verdict, m.priority), (Verdict::Send, Priority::Normal));
        let m = eval(meeting_prep(25, 90, true));
        assert_eq!((m.verdict, m.priority), (Verdict::Send, Priority::High));
        for m in [5, 20, 45, 60, 75] {
            let bare = eval(meeting_prep(m, 90, false));
            assert_eq!(bare.verdict, Verdict::Digest, "{m}: {}", bare.reason);
        }

        assert_eq!(eval(reminder_overdue(20, false)).verdict, Verdict::Send);
        assert_eq!(eval(due_no_plan(24)).verdict, Verdict::Digest);
        assert_eq!(eval(due_no_plan(70)).verdict, Verdict::Digest);
        assert_eq!(eval(note_for_event(2)).verdict, Verdict::Digest);

        assert_eq!(eval(thread_resolved(0.8, true)).verdict, Verdict::Send);
        assert_eq!(eval(thread_resolved(0.8, false)).verdict, Verdict::Digest);
        assert_eq!(eval(thread_open(0.75)).verdict, Verdict::Digest);
        assert_eq!(eval(thread_open(0.6)).verdict, Verdict::Drop);

        let down = eval(service_down(10, true));
        assert_eq!(
            (down.verdict, down.priority),
            (Verdict::Send, Priority::High)
        );
        assert_eq!(eval(service_recovered(true)).verdict, Verdict::Send);
        assert_eq!(eval(service_recovered(false)).verdict, Verdict::Digest);
    }

    #[test]
    fn low_confidence_is_dropped_whatever_the_score() {
        let s = eval(Scores::new(1.0, 1.0, 0.5, 1.0));
        assert_eq!(s.verdict, Verdict::Drop);
        assert!(s.reason.starts_with("not verified enough"), "{}", s.reason);
    }

    #[test]
    fn reasons_name_the_threshold() {
        let s = eval(meeting_prep(60, 90, true));
        assert!(s.reason.contains("≥ send 0.70"), "{}", s.reason);
        let d = eval(Scores::new(0.1, 0.1, 0.9, 0.1));
        assert_eq!(d.verdict, Verdict::Drop);
        assert!(d.reason.contains("< digest 0.45"), "{}", d.reason);
    }
}
