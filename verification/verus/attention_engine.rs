// Attention engine kernel (lucina-proactive: score, verdict, attention gate, budget,
// "Basta così", Daily Pulse).
//
// Production (branch lucina-proactive; fixes on proactive-proofs):
//   total / verdict      <-> score::total / score::evaluate   (src/score.rs), in fixed point:
//                            weights and inputs in thousandths (production: f32 in [0, 1])
//   gate                 <-> attention::gate                  (src/attention.rs), times in ms
//   quiet_inside         <-> the `inside` test of attention::quiet_until
//   muted_until          <-> Engine::ledger's muted_until = last "stop" + stop_days
//   tick_sends           <-> the send loop of Engine::tick (ledger re-read per candidate)
//   pulse_step           <-> Engine::pulse                     (src/engine.rs)
// Differential tests: lucina-proactive/tests/verus_attention.rs (branch proactive-proofs).
//
// Proven:
//   E1 score_in_unit: 0 <= score <= 1 (10^6 in fixed point) when the weights are
//      non-negative and sum to at most 1 and every input is in [0, 1].
//   E2 verdict monotone: in the score (fixed confidence), in every input of the score,
//      and antitone in the thresholds (raising `send`/`digest` never raises a verdict).
//   E3 gate: a Send implies not expired, not muted, not quiet, no presence/focus hold,
//      no "Non ora", the kind's cooldown has passed and the daily budget is not used up.
//   E4 budget: a tick never takes the day's unsolicited count past max(start, max_per_day).
//   E5 Basta così: after a "stop" at t, every candidate of that kind is dropped for
//      now in [t, t + stop_days); with the default 30, for 30 days.
//   E6 Pulse: sent only when there is something new and it is not quiet, held or muted.
//
// Found on lucina-proactive (fixed on proactive-proofs):
//   - Config::validate accepted weights such as {urgency 1.5, relevance -0.5} or
//     {0.355, 0.25, 0.2, 0.2} (sum within 0.01 of 1), so E1 failed (score -0.5 or 1.005),
//     and NaN weights passed the `abs() > 0.01` test.
//   - The Pulse offers "Basta così" but Engine::pulse never read the Pulse's own stop
//     feedback: E6's "not muted" failed (the Pulse kept coming every morning).
use vstd::prelude::*;

verus! {

pub const ONE: u64 = 1000;
pub const DAY_MS: i64 = 86_400_000;
pub const RECHECK_MS: i64 = 300_000;
pub const QUIET_DEFAULT_MS: i64 = 1_800_000;
pub const T_MAX: i64 = 0x10_0000_0000_0000; // 2^52 ms: far beyond any date

// ---------------------------------------------------------------- score

pub open spec fn total_spec(w: (u64, u64, u64, u64), s: (u64, u64, u64, u64)) -> int {
    w.0 * s.0 + w.1 * s.1 + w.2 * s.2 + w.3 * s.3
}

pub fn total(w: (u64, u64, u64, u64), s: (u64, u64, u64, u64)) -> (r: u64)
    requires
        w.0 + w.1 + w.2 + w.3 <= ONE,
        s.0 <= ONE && s.1 <= ONE && s.2 <= ONE && s.3 <= ONE,
    ensures
        r == total_spec(w, s),
        r <= ONE * ONE,
{
    proof {
        lemma_score_in_unit(w, s);
    }
    w.0 * s.0 + w.1 * s.1 + w.2 * s.2 + w.3 * s.3
}

/// E1
pub proof fn lemma_score_in_unit(w: (u64, u64, u64, u64), s: (u64, u64, u64, u64))
    requires
        w.0 + w.1 + w.2 + w.3 <= ONE,
        s.0 <= ONE && s.1 <= ONE && s.2 <= ONE && s.3 <= ONE,
    ensures
        0 <= total_spec(w, s) <= ONE * ONE,
{
    assert(w.0 * s.0 <= w.0 * ONE) by (nonlinear_arith)
        requires
            s.0 <= ONE,
    ;
    assert(w.1 * s.1 <= w.1 * ONE) by (nonlinear_arith)
        requires
            s.1 <= ONE,
    ;
    assert(w.2 * s.2 <= w.2 * ONE) by (nonlinear_arith)
        requires
            s.2 <= ONE,
    ;
    assert(w.3 * s.3 <= w.3 * ONE) by (nonlinear_arith)
        requires
            s.3 <= ONE,
    ;
}

/// Raising any input never lowers the score.
pub proof fn lemma_score_monotone(
    w: (u64, u64, u64, u64),
    s: (u64, u64, u64, u64),
    t: (u64, u64, u64, u64),
)
    requires
        s.0 <= t.0 && s.1 <= t.1 && s.2 <= t.2 && s.3 <= t.3,
    ensures
        total_spec(w, s) <= total_spec(w, t),
{
    assert(w.0 * s.0 <= w.0 * t.0) by (nonlinear_arith)
        requires
            s.0 <= t.0,
    ;
    assert(w.1 * s.1 <= w.1 * t.1) by (nonlinear_arith)
        requires
            s.1 <= t.1,
    ;
    assert(w.2 * s.2 <= w.2 * t.2) by (nonlinear_arith)
        requires
            s.2 <= t.2,
    ;
    assert(w.3 * s.3 <= w.3 * t.3) by (nonlinear_arith)
        requires
            s.3 <= t.3,
    ;
}

// ---------------------------------------------------------------- verdict

#[derive(PartialEq, Eq)]
pub enum Verdict {
    Drop,
    Digest,
    Send,
}

pub open spec fn rank(v: Verdict) -> int {
    match v {
        Verdict::Drop => 0,
        Verdict::Digest => 1,
        Verdict::Send => 2,
    }
}

pub struct Thresholds {
    pub send: u64,
    pub digest: u64,
    pub min_confidence: u64,
}

pub open spec fn verdict_spec(score: u64, confidence: u64, t: Thresholds) -> Verdict {
    if confidence < t.min_confidence {
        Verdict::Drop
    } else if score >= t.send {
        Verdict::Send
    } else if score >= t.digest {
        Verdict::Digest
    } else {
        Verdict::Drop
    }
}

/// score::evaluate's verdict chain.
pub fn verdict(score: u64, confidence: u64, t: &Thresholds) -> (r: Verdict)
    ensures
        r == verdict_spec(score, confidence, *t),
{
    if confidence < t.min_confidence {
        Verdict::Drop
    } else if score >= t.send {
        Verdict::Send
    } else if score >= t.digest {
        Verdict::Digest
    } else {
        Verdict::Drop
    }
}

/// E2: monotone in the score.
pub proof fn lemma_verdict_monotone_in_score(a: u64, b: u64, c: u64, t: Thresholds)
    requires
        a <= b,
    ensures
        rank(verdict_spec(a, c, t)) <= rank(verdict_spec(b, c, t)),
{
}

/// E2: monotone in the confidence (for the same score).
pub proof fn lemma_verdict_monotone_in_confidence(s: u64, c1: u64, c2: u64, t: Thresholds)
    requires
        c1 <= c2,
    ensures
        rank(verdict_spec(s, c1, t)) <= rank(verdict_spec(s, c2, t)),
{
}

/// E2: stricter thresholds never raise a verdict.
pub proof fn lemma_verdict_antitone_in_thresholds(s: u64, c: u64, t: Thresholds, u: Thresholds)
    requires
        t.send <= u.send,
        t.digest <= u.digest,
        t.min_confidence <= u.min_confidence,
    ensures
        rank(verdict_spec(s, c, u)) <= rank(verdict_spec(s, c, t)),
{
}

/// E2 end to end: raising any input (confidence included) never lowers the verdict.
pub proof fn lemma_evaluate_monotone(
    w: (u64, u64, u64, u64),
    s: (u64, u64, u64, u64),
    x: (u64, u64, u64, u64),
    t: Thresholds,
)
    requires
        w.0 + w.1 + w.2 + w.3 <= ONE,
        s.0 <= x.0 && s.1 <= x.1 && s.2 <= x.2 && s.3 <= x.3,
        x.0 <= ONE && x.1 <= ONE && x.2 <= ONE && x.3 <= ONE,
    ensures
        ({
            let a = total_spec(w, s);
            let b = total_spec(w, x);
            0 <= a <= b <= ONE * ONE
        }),
        rank(verdict_spec(total_spec(w, s) as u64, s.2, t)) <= rank(
            verdict_spec(total_spec(w, x) as u64, x.2, t),
        ),
{
    lemma_score_monotone(w, s, x);
    lemma_score_in_unit(w, x);
    lemma_score_in_unit(w, s);
    let a = total_spec(w, s) as u64;
    let b = total_spec(w, x) as u64;
    lemma_verdict_monotone_in_score(a, b, s.2, t);
    lemma_verdict_monotone_in_confidence(b, s.2, x.2, t);
}

// ---------------------------------------------------------------- the attention gate

pub struct GateIn {
    pub now: i64,
    pub expires_at: Option<i64>,
    pub muted_until: Option<i64>,
    pub quiet: bool,
    pub quiet_until: Option<i64>,
    /// Gaming (when held), Discord DND (when held) or an iPhone focus mode.
    pub presence_hold: bool,
    pub not_now_until: Option<i64>,
    pub last_sent_of_kind: Option<i64>,
    pub cooldown_ms: i64,
    pub sent_today: u32,
    pub max_per_day: u32,
}

#[derive(PartialEq, Eq)]
pub enum Gate {
    Send,
    Hold(i64),
    Digest,
    Drop,
}

pub open spec fn in_range(t: i64) -> bool {
    0 <= t <= T_MAX
}

pub open spec fn opt_in_range(t: Option<i64>) -> bool {
    match t {
        Some(x) => in_range(x),
        None => true,
    }
}

pub open spec fn gate_input_ok(g: GateIn) -> bool {
    in_range(g.now) && opt_in_range(g.expires_at) && opt_in_range(g.muted_until) && opt_in_range(
        g.quiet_until,
    ) && opt_in_range(g.not_now_until) && opt_in_range(g.last_sent_of_kind) && 0 <= g.cooldown_ms
        <= T_MAX
}

pub open spec fn expired_at(g: GateIn, t: int) -> bool {
    match g.expires_at {
        Some(e) => e <= t,
        None => false,
    }
}

fn expired(expires_at: Option<i64>, t: i64) -> (r: bool)
    ensures
        r == match expires_at {
            Some(e) => e <= t,
            None => false,
        },
{
    match expires_at {
        Some(e) => e <= t,
        None => false,
    }
}

fn hold(expires_at: Option<i64>, until: i64) -> (r: Gate)
    ensures
        r == (if (match expires_at {
            Some(e) => e <= until,
            None => false,
        }) {
            Gate::Digest
        } else {
            Gate::Hold(until)
        }),
{
    if expired(expires_at, until) {
        Gate::Digest
    } else {
        Gate::Hold(until)
    }
}

/// attention::gate
pub fn gate(g: &GateIn) -> (r: Gate)
    requires
        gate_input_ok(*g),
    ensures
        r == Gate::Send ==> !expired_at(*g, g.now as int) && !g.quiet && !g.presence_hold
            && g.sent_today < g.max_per_day && (match g.muted_until {
            Some(m) => m <= g.now,
            None => true,
        }) && (match g.not_now_until {
            Some(u) => u <= g.now,
            None => true,
        }) && (match g.last_sent_of_kind {
            Some(l) => l + g.cooldown_ms <= g.now,
            None => true,
        }),
        (match g.muted_until {
            Some(m) => m > g.now,
            None => false,
        }) ==> r == Gate::Drop,
        expired_at(*g, g.now as int) ==> r == Gate::Drop,
        g.quiet && r != Gate::Drop ==> (r is Hold || r == Gate::Digest),
{
    let now = g.now;
    if expired(g.expires_at, now) {
        return Gate::Drop;
    }
    match g.muted_until {
        Some(m) => {
            if m > now {
                return Gate::Drop;
            }
        },
        None => {},
    }
    if g.quiet {
        let until = match g.quiet_until {
            Some(u) => u,
            None => now + QUIET_DEFAULT_MS,
        };
        return hold(g.expires_at, until);
    }
    if g.presence_hold {
        return hold(g.expires_at, now + RECHECK_MS);
    }
    match g.not_now_until {
        Some(u) => {
            if u > now {
                return hold(g.expires_at, u);
            }
        },
        None => {},
    }
    match g.last_sent_of_kind {
        Some(l) => {
            if l + g.cooldown_ms > now {
                return Gate::Digest;
            }
        },
        None => {},
    }
    if g.sent_today >= g.max_per_day {
        return Gate::Digest;
    }
    Gate::Send
}

/// The `inside` test of attention::quiet_until (minutes after local midnight).
pub fn quiet_inside(m: u32, start: u32, end: u32) -> (r: bool)
    ensures
        start == end ==> !r,
        start < end ==> (r == (start <= m && m < end)),
        start > end ==> (r == (m >= start || m < end)),
{
    if start == end {
        false
    } else if start < end {
        m >= start && m < end
    } else {
        m >= start || m < end
    }
}

// ---------------------------------------------------------------- budget over a tick

/// Count after a tick: each queued candidate whose other gates pass is sent iff the
/// day's count (re-read per candidate) is below the budget.
pub open spec fn tick_sends(sent: nat, max: nat, ok: Seq<bool>) -> nat
    decreases ok.len(),
{
    if ok.len() == 0 {
        sent
    } else {
        let next = if ok[0] && sent < max {
            sent + 1
        } else {
            sent
        };
        tick_sends(next, max, ok.drop_first())
    }
}

/// E4
pub proof fn lemma_budget_never_exceeded(sent: nat, max: nat, ok: Seq<bool>)
    ensures
        tick_sends(sent, max, ok) <= if sent > max {
            sent
        } else {
            max
        },
        sent <= max ==> tick_sends(sent, max, ok) <= max,
    decreases ok.len(),
{
    if ok.len() > 0 {
        let next = if ok[0] && sent < max {
            sent + 1
        } else {
            sent
        };
        lemma_budget_never_exceeded(next, max, ok.drop_first());
    }
}

// ---------------------------------------------------------------- Basta così

/// Engine::ledger: the last "stop" of this kind plus stop_days.
pub open spec fn muted_until(last_stop: Option<int>, stop_days: int) -> Option<int> {
    match last_stop {
        Some(t) => Some(t + stop_days * DAY_MS),
        None => None,
    }
}

/// E5: a "stop" at `t` (last_stop is the newest stop, so >= t) drops the kind until
/// t + stop_days days.
pub proof fn lemma_basta_cosi_mutes(g: GateIn, t: int, last_stop: int, stop_days: int, r: Gate)
    requires
        last_stop >= t,
        stop_days >= 0,
        t <= g.now < t + stop_days * DAY_MS,
        g.muted_until == Some((last_stop + stop_days * DAY_MS) as i64),
        last_stop + stop_days * DAY_MS <= T_MAX,
        // r is what gate(g) returned
        (match g.muted_until {
            Some(m) => m > g.now,
            None => false,
        }) ==> r == Gate::Drop,
    ensures
        r == Gate::Drop,
{
}

/// With the default stop_days (30): 30 days.
pub proof fn lemma_basta_cosi_thirty_days(t: int, now: int)
    requires
        t <= now < t + 30 * DAY_MS,
    ensures
        muted_until(Some(t), 30) matches Some(m) && m > now,
{
}

// ---------------------------------------------------------------- Daily Pulse

#[derive(PartialEq, Eq)]
pub enum PulseStep {
    /// Disabled, outside the window, or already done today.
    Skip,
    /// Quiet hours, gaming or a focus mode: try again next tick.
    Wait,
    /// Nothing to say, nothing new, or muted ("Basta così" on the Pulse).
    Suppress,
    Send,
}

pub struct PulseIn {
    pub enabled: bool,
    pub in_window: bool,
    pub done_today: bool,
    pub quiet: bool,
    pub gaming_hold: bool,
    pub focus_hold: bool,
    pub muted: bool,
    pub empty: bool,
    pub new_items: u32,
}

/// Engine::pulse (with the mute check of the fix).
pub fn pulse_step(p: &PulseIn) -> (r: PulseStep)
    ensures
        r == PulseStep::Send ==> p.enabled && p.in_window && !p.done_today && !p.quiet
            && !p.gaming_hold && !p.focus_hold && !p.muted && !p.empty && p.new_items > 0,
        p.empty ==> r != PulseStep::Send,
        p.muted ==> r != PulseStep::Send,
{
    if !p.enabled || !p.in_window || p.done_today {
        return PulseStep::Skip;
    }
    if p.muted {
        return PulseStep::Suppress;
    }
    if p.quiet || p.gaming_hold || p.focus_hold {
        return PulseStep::Wait;
    }
    if p.empty || p.new_items == 0 {
        return PulseStep::Suppress;
    }
    PulseStep::Send
}

} // verus!
fn main() {}
