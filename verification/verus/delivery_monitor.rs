// Delivery-path monitor (protocol 15): one alert when a delivery path (Telegram poller,
// Discord session, reminder scheduler, app notifications) has been down for `after_ms`, and
// one more when it recovers. docs/verification.md §15.
//
// Production (branch lucina-rs, f26eb90; saturating subtraction since 5c87ae0):
//   step <-> crates/lucina-server/src/delivery_monitor.rs `step` (same rules, same order);
//            `Monitor::check` calls it once a minute per path and persists the states, so a
//            restart keeps `down_since` / `alerted` and only renews the start-up grace.
//
// Abstractions: `PathState::detail` is not modelled (step always returns it empty; `check`
// fills it after). Times are any i64: `now_ms.saturating_sub(..)` is mirrored by `sat_sub`
// (before 5c87ae0 a corrupt `downSince` near i64::MIN could wrap in release builds and keep that
// outage from ever being alerted; there is no precondition on the times now).
//
// Proven:
//   M1 up: the state resets; Recovered exactly when the outage was alerted.
//   M2 down: Down only if not yet alerted, only once the outage has lasted after_ms and the
//      process has been up after_ms; the outage start never moves later.
//   M3 at most one Down per outage: from an alerted state, any run of down checks stays
//      alerted and silent.
//   M4 one Recovered per outage: after an up check the state is clear, so a second up check
//      is silent.
//   M5 no missed alert: a down check at or after max(down_since, started) + after_ms of an
//      outage not yet alerted sends Down.
use vstd::prelude::*;

verus! {

#[derive(PartialEq, Eq, Clone, Copy)]
pub struct PathState {
    pub down_since: Option<i64>,
    pub alerted: bool,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Alert { Down, Recovered }

pub open spec fn clear() -> PathState {
    PathState { down_since: None, alerted: false }
}

pub open spec fn sat_spec(a: i64, b: i64) -> int {
    let d = a as int - b as int;
    if d > i64::MAX as int { i64::MAX as int } else if d < i64::MIN as int { i64::MIN as int } else { d }
}

/// `i64::saturating_sub`.
pub fn sat_sub(a: i64, b: i64) -> (r: i64)
    ensures r as int == sat_spec(a, b),
{
    let d: i128 = (a as i128) - (b as i128);
    if d > i64::MAX as i128 { i64::MAX } else if d < i64::MIN as i128 { i64::MIN } else { d as i64 }
}

pub open spec fn since_spec(prev: PathState, now_ms: i64) -> i64 {
    match prev.down_since {
        None => now_ms,
        Some(s) => if s < now_ms { s } else { now_ms },
    }
}

pub open spec fn step_spec(prev: PathState, up: bool, now_ms: i64, started_ms: i64, after_ms: i64)
    -> (PathState, Option<Alert>)
{
    if up {
        (clear(), if prev.alerted { Some(Alert::Recovered) } else { None })
    } else {
        let since = since_spec(prev, now_ms);
        let due = sat_spec(now_ms, since) >= after_ms as int && sat_spec(now_ms, started_ms) >= after_ms as int;
        if !prev.alerted && due {
            (PathState { down_since: Some(since), alerted: true }, Some(Alert::Down))
        } else {
            (PathState { down_since: Some(since), alerted: prev.alerted }, None)
        }
    }
}

/// delivery_monitor.rs `step`.
pub fn step(prev: &PathState, up: bool, now_ms: i64, started_ms: i64, after_ms: i64) -> (r: (PathState, Option<Alert>))
    ensures
        r == step_spec(*prev, up, now_ms, started_ms, after_ms),
        // M1
        up ==> r.0 == clear() && (r.1 == Some(Alert::Recovered) <==> prev.alerted) && r.1 != Some(Alert::Down),
        // M2
        !up ==> r.1 != Some(Alert::Recovered),
        r.1 == Some(Alert::Down) ==> !prev.alerted && r.0.alerted
            && sat_spec(now_ms, r.0.down_since->Some_0) >= after_ms as int && sat_spec(now_ms, started_ms) >= after_ms as int,
        !up ==> r.0.down_since is Some && r.0.down_since->Some_0 <= now_ms
            && (prev.down_since is Some ==> r.0.down_since->Some_0 <= prev.down_since->Some_0),
        !up && prev.alerted ==> r.0.alerted && r.1 is None,
{
    if up {
        let alert = if prev.alerted { Some(Alert::Recovered) } else { None };
        return (PathState { down_since: None, alerted: false }, alert);
    }
    let since = match prev.down_since {
        Some(s) => if s < now_ms { s } else { now_ms },
        None => now_ms,
    };
    let due = sat_sub(now_ms, since) >= after_ms && sat_sub(now_ms, started_ms) >= after_ms;
    if !prev.alerted && due {
        (PathState { down_since: Some(since), alerted: true }, Some(Alert::Down))
    } else {
        (PathState { down_since: Some(since), alerted: prev.alerted }, None)
    }
}

/// The state after a run of checks `obs` (up flag, now) from `s`.
pub open spec fn run(s: PathState, obs: Seq<(bool, i64)>, started_ms: i64, after_ms: i64) -> PathState
    decreases obs.len(),
{
    if obs.len() == 0 { s } else {
        run(step_spec(s, obs[0].0, obs[0].1, started_ms, after_ms).0, obs.drop_first(), started_ms, after_ms)
    }
}

/// M3: once alerted, a run of down checks keeps the state alerted and sends nothing.
proof fn alerted_outage_stays_silent(s: PathState, obs: Seq<(bool, i64)>, started_ms: i64, after_ms: i64)
    requires
        s.alerted,
        forall|k: int| 0 <= k < obs.len() ==> !(#[trigger] obs[k]).0,
    ensures
        run(s, obs, started_ms, after_ms).alerted,
        forall|k: int| #![trigger obs[k]] 0 <= k < obs.len() ==> step_spec(run(s, obs.take(k), started_ms, after_ms), obs[k].0, obs[k].1, started_ms, after_ms).1 is None,
    decreases obs.len(),
{
    if obs.len() > 0 {
        let s1 = step_spec(s, obs[0].0, obs[0].1, started_ms, after_ms).0;
        assert(!obs[0].0);
        let rest = obs.drop_first();
        assert forall|k: int| 0 <= k < rest.len() implies !(#[trigger] rest[k]).0 by {
            assert(rest[k] == obs[k + 1]);
        }
        alerted_outage_stays_silent(s1, rest, started_ms, after_ms);
        assert forall|k: int| #![trigger obs[k]] 0 <= k < obs.len() implies
            step_spec(run(s, obs.take(k), started_ms, after_ms), obs[k].0, obs[k].1, started_ms, after_ms).1 is None by {
            if k == 0 {
                assert(obs.take(0).len() == 0);
                assert(run(s, obs.take(0), started_ms, after_ms) == s);
            } else {
                let t = obs.take(k);
                assert(t.len() > 0);
                assert(t[0] == obs[0]);
                assert(t.drop_first() =~= rest.take(k - 1));
                assert(run(s, t, started_ms, after_ms) == run(s1, rest.take(k - 1), started_ms, after_ms));
                assert(rest[k - 1] == obs[k]);
            }
        }
    }
}

/// M4: after an up check nothing is left to recover from.
proof fn recovered_once(s: PathState, now1: i64, now2: i64, started_ms: i64, after_ms: i64)
    ensures
        step_spec(step_spec(s, true, now1, started_ms, after_ms).0, true, now2, started_ms, after_ms).1 is None,
{}

/// M5: a down check late enough for an outage not yet alerted sends Down.
proof fn no_missed_alert(s: PathState, now_ms: i64, started_ms: i64, after_ms: i64)
    requires
        !s.alerted,
        s.down_since is Some,
        s.down_since->Some_0 <= now_ms,
        sat_spec(now_ms, s.down_since->Some_0) >= after_ms as int,
        sat_spec(now_ms, started_ms) >= after_ms as int,
    ensures step_spec(s, false, now_ms, started_ms, after_ms).1 == Some(Alert::Down),
{}

fn witnesses() {
    let after: i64 = 300_000;
    let s0 = PathState { down_since: None, alerted: false };
    let (s1, a1) = step(&s0, false, 1_000_000, 0, after);
    assert(a1 is None);
    let (s2, a2) = step(&s1, false, 1_300_000, 0, after);
    assert(a2 == Some(Alert::Down));
    let (s3, a3) = step(&s2, false, 1_360_000, 0, after);
    assert(a3 is None);
    let (_s4, a4) = step(&s3, true, 1_420_000, 0, after);
    assert(a4 == Some(Alert::Recovered));
    // Start-up grace: an old outage is not alerted until the process has been up after_ms.
    let (_g, ag) = step(&s1, false, 1_300_000, 1_100_000, after);
    assert(ag is None);
}

fn main() {
}

} // verus!
