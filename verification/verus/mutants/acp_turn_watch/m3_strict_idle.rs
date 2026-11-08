// MUTANT m3_strict_idle: idle must exceed the limit (> instead of >=). Verus must reject this file.
// ACP turn watch kernel (Lucina Agents: when a running ACP turn is ended as a quota stop
// although the agent never answered; opencode on Zen's free limit hangs instead).
//
// Production: crates/lucina-agents/src/turn_watch.rs
//   Facts   <-> turn_watch::WatchFacts (stall_ms None <-> has_stall false)
//   Verdict <-> turn_watch::Verdict
//   verdict <-> turn_watch::verdict
// Differential test: crates/lucina-agents/tests/verus_turn_watch.rs.
// Mutants (each must fail verification): verification/verus/mutants/acp_turn_watch/*.rs,
// run by scripts/verus-mutants-agents.sh.
//
// Proven:
//   W1 permission_waits: while a permission request is open the turn is never ended
//      (the owner may answer hours later).
//   W2 tool_not_stall: an open tool call is never a stall (a long build sends nothing).
//   W3 stall_needs_time: Stalled only with a stall rule and idle >= its limit, nothing open.
//   W4 limit_wins: a usage limit in the agent's log ends the turn unless a permission is open.
//   W5 complete: idle past the limit with nothing open and a stall rule always ends the turn.
use vstd::prelude::*;

verus! {

#[derive(PartialEq, Eq, Clone, Copy)]
pub struct Facts {
    pub idle_ms: u64,
    pub has_stall: bool,
    pub stall_ms: u64,
    pub open_tools: u32,
    pub open_permissions: u32,
    pub log_limit: bool,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Verdict {
    Wait,
    Limit,
    Stalled,
}

pub fn verdict(f: &Facts) -> (v: Verdict)
    ensures
        // W1
        f.open_permissions > 0 ==> v == Verdict::Wait,
        // W2
        f.open_tools > 0 ==> v != Verdict::Stalled,
        // W3
        v == Verdict::Stalled ==> (f.has_stall && f.idle_ms >= f.stall_ms && f.open_tools == 0
            && f.open_permissions == 0 && !f.log_limit),
        // W4
        (f.log_limit && f.open_permissions == 0) ==> v == Verdict::Limit,
        v == Verdict::Limit ==> f.log_limit,
        // W5
        (f.has_stall && f.idle_ms >= f.stall_ms && f.open_tools == 0 && f.open_permissions == 0)
            ==> v != Verdict::Wait,
{
    if f.open_permissions > 0 {
        return Verdict::Wait;
    }
    if f.log_limit {
        return Verdict::Limit;
    }
    if f.has_stall && f.open_tools == 0 && f.idle_ms > f.stall_ms {
        Verdict::Stalled
    } else {
        Verdict::Wait
    }
}

} // verus!

fn main() {}
