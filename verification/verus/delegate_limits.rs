// Delegation limits kernel (lucina-delegate: when Lucina may hand a task to Claude, Grok
// or ChatGPT, and the daily budget that stops a runaway).
//
// Production (branch lucina-rs, crates/lucina-delegate/src/):
//   hard_limits  <-> policy::hard_limits, in its order: empty task, MCP origin, depth
//                    (delegated origin, depth >= min(max_depth, 1), the MARKER in the
//                    task), loop (the target is the caller), money intent, proactive
//                    origin with a non-read-only kind or a workspace
//   Req          <-> the facts of types::Request it reads; `money` is policy::money_intent
//                    (a regex: an input here) and `loop_hit` the caller test
//                    (AgentId::parse(caller) == target or the caller names it)
//   reserve      <-> state::StateStore::reserve on one day's StateFile (switches, pauses,
//                    then the caps in order: total calls, the agent's calls, wall minutes,
//                    turns when capped), `release` <-> StateStore::release, `roll` <-> the
//                    day change in StateStore::with
// Differential test: crates/lucina-delegate/tests/verus_limits.rs. The release across
// midnight (finding B1) is the Stateright model crates/lucina-delegate/tests/budget_model.rs.
//
// Proven:
//   D1 mcp_never: an MCP turn never delegates.
//   D2 depth_one: a delegated turn, a turn carrying the delegation marker, or any depth
//      >= 1 never delegates (one level at most); max_depth = 0 refuses every request.
//   D3 money_never: a money intent never delegates, whoever asks.
//   D4 proactive_read_only: a proactive request delegates only read-only work without a
//      workspace.
//   D5 ok_iff: hard_limits is Ok exactly when none of the above applies.
//   (the postconditions of `reserve`, for every Day and Caps)
//   B2 within caps: a reserve is accepted only below every cap, so after it the agent's
//      calls are <= its cap and the day's total <= the total cap.
//   B3 a refused reserve never counts a call; switches and pauses refuse without
//      touching the counts and without a cap hit.
//   B4 a cap hit pauses its scope (pauses are never lifted within the day), so the pause
//      notice comes once: every later reserve that day is Paused, without a notice.
//
// No Stateright model for the decision itself (pure); the budget across a day change is
// modelled in Stateright (see above).
use vstd::prelude::*;

verus! {

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Origin {
    Owner,
    Mcp,
    Proactive,
    Delegated,
}

#[derive(PartialEq, Eq, Clone, Copy)]
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

pub open spec fn min1(m: u8) -> u8 {
    if m < 1 {
        m
    } else {
        1
    }
}

pub open spec fn allowed(r: Req, max_depth: u8) -> bool {
    !r.empty && r.origin != Origin::Mcp && !(r.origin == Origin::Delegated || r.depth >= min1(
        max_depth,
    ) || r.marker) && !r.loop_hit && !r.money && !(r.origin == Origin::Proactive && (
    !r.read_only || r.workspace))
}

fn is_mcp(o: Origin) -> (b: bool)
    ensures
        b == (o == Origin::Mcp),
{
    match o {
        Origin::Mcp => true,
        _ => false,
    }
}

fn is_delegated(o: Origin) -> (b: bool)
    ensures
        b == (o == Origin::Delegated),
{
    match o {
        Origin::Delegated => true,
        _ => false,
    }
}

fn is_proactive(o: Origin) -> (b: bool)
    ensures
        b == (o == Origin::Proactive),
{
    match o {
        Origin::Proactive => true,
        _ => false,
    }
}

pub fn hard_limits(r: &Req, max_depth: u8) -> (res: Result<(), Refusal>)
    ensures
        res is Ok <==> allowed(*r, max_depth),
        r.origin == Origin::Mcp && !r.empty ==> res == Err::<(), Refusal>(Refusal::McpOrigin),
{
    if r.empty {
        return Err(Refusal::Empty);
    }
    if is_mcp(r.origin) {
        return Err(Refusal::McpOrigin);
    }
    let m = if max_depth < 1 {
        max_depth
    } else {
        1
    };
    if is_delegated(r.origin) || r.depth >= m || r.marker {
        return Err(Refusal::Depth);
    }
    if r.loop_hit {
        return Err(Refusal::Loop);
    }
    if r.money {
        return Err(Refusal::Money);
    }
    if is_proactive(r.origin) && (!r.read_only || r.workspace) {
        return Err(Refusal::ProactiveReadOnly);
    }
    Ok(())
}

pub proof fn mcp_never(r: Req, m: u8)
    requires
        r.origin == Origin::Mcp,
    ensures
        !allowed(r, m),
{
}

pub proof fn depth_one(r: Req, m: u8)
    requires
        r.origin == Origin::Delegated || r.marker || r.depth >= 1 || m == 0,
    ensures
        !allowed(r, m),
{
}

pub proof fn money_never(r: Req, m: u8)
    requires
        r.money,
    ensures
        !allowed(r, m),
{
}

pub proof fn proactive_read_only(r: Req, m: u8)
    requires
        r.origin == Origin::Proactive,
        allowed(r, m),
    ensures
        r.read_only && !r.workspace,
{
}

/// Not vacuous: the owner's research question at depth 0 delegates.
pub proof fn witness_owner_allowed() {
    let r = Req {
        empty: false,
        origin: Origin::Owner,
        depth: 0,
        marker: false,
        loop_hit: false,
        money: false,
        read_only: true,
        workspace: false,
    };
    assert(allowed(r, 1));
}

// ---------------------------------------------------------------- the budget

pub struct Caps {
    pub total: u32,
    pub calls: u32,
    pub wall_minutes: u32,
    /// 0 = no cap.
    pub turns: u32,
}

/// One agent's view of one day's StateFile.
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

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Outcome {
    Ok,
    Stopped,
    AgentOff,
    Paused,
    /// A cap was hit now: the scope is paused; `notice` the first time.
    Hit { all: bool, notice: bool },
}

pub open spec fn cap_hit_all(d: Day, c: Caps) -> bool {
    d.total >= c.total
}

pub open spec fn cap_hit_agent(d: Day, c: Caps) -> bool {
    d.calls >= c.calls || d.wall_ms >= c.wall_minutes as int * 60_000 || (c.turns > 0 && d.turns
        >= c.turns)
}

pub fn reserve(d: &Day, c: &Caps) -> (r: (Outcome, Day))
    ensures
        r.0 == Outcome::Ok ==> d.calls < c.calls && d.total < c.total && r.1.calls == d.calls + 1
            && r.1.total == d.total + 1,
        r.0 != Outcome::Ok ==> r.1.calls == d.calls && r.1.total == d.total && r.1.wall_ms
            == d.wall_ms && r.1.turns == d.turns,
        r.0 == Outcome::Ok <==> !d.stopped && !d.disabled && !d.paused_all && !d.paused_agent
            && !cap_hit_all(*d, *c) && !cap_hit_agent(*d, *c),
        (d.stopped || d.disabled || d.paused_all || d.paused_agent) ==> (r.0 != Outcome::Ok && !(r.0
            matches Outcome::Hit { .. })),
        r.0 matches Outcome::Hit { all, notice } ==> (if all {
            r.1.paused_all && notice == !d.paused_all
        } else {
            r.1.paused_agent && notice == !d.paused_agent
        }),
        r.1.stopped == d.stopped && r.1.disabled == d.disabled,
        d.paused_all ==> r.1.paused_all,
        d.paused_agent ==> r.1.paused_agent,
{
    let same = Day {
        stopped: d.stopped,
        disabled: d.disabled,
        paused_all: d.paused_all,
        paused_agent: d.paused_agent,
        calls: d.calls,
        wall_ms: d.wall_ms,
        turns: d.turns,
        total: d.total,
    };
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
        // `paused.insert("all", why).is_none()`: always new here (paused_all is false).
        let after = Day { paused_all: true, ..same };
        return (Outcome::Hit { all: true, notice: true }, after);
    }
    if d.calls >= c.calls || d.wall_ms >= (c.wall_minutes as u64) * 60_000 || (c.turns > 0
        && d.turns >= c.turns) {
        let after = Day { paused_agent: true, ..same };
        return (Outcome::Hit { all: false, notice: true }, after);
    }
    let after = Day { calls: d.calls + 1, total: d.total + 1, ..same };
    (Outcome::Ok, after)
}

/// `release`: give back one call (saturating), on whatever day it runs.
pub fn release(d: &Day) -> (r: Day)
    ensures
        r.calls == if d.calls == 0 {
            0
        } else {
            d.calls - 1
        },
        r.total == if d.total == 0 {
            0
        } else {
            d.total - 1
        },
{
    Day {
        stopped: d.stopped,
        disabled: d.disabled,
        paused_all: d.paused_all,
        paused_agent: d.paused_agent,
        calls: if d.calls == 0 {
            0
        } else {
            d.calls - 1
        },
        wall_ms: d.wall_ms,
        turns: d.turns,
        total: if d.total == 0 {
            0
        } else {
            d.total - 1
        },
    }
}

/// The day change: counts and pauses cleared, the owner's switches kept.
pub fn roll(d: &Day) -> (r: Day)
    ensures
        r.calls == 0 && r.total == 0 && r.wall_ms == 0 && r.turns == 0,
        !r.paused_all && !r.paused_agent,
        r.stopped == d.stopped && r.disabled == d.disabled,
{
    Day {
        stopped: d.stopped,
        disabled: d.disabled,
        paused_all: false,
        paused_agent: false,
        calls: 0,
        wall_ms: 0,
        turns: 0,
        total: 0,
    }
}

} // verus!

fn main() {}
