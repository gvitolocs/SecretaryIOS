// Finished-run gate kernel (Lucina Agents: the verdict on a run that changed the
// workspace, from diffgate, the proof command agentd ran itself, the advisory judge and
// whether the run edited the workspace's own gate config).
//
// Production (branch agents-p3, crates/lucina-agents/src/run_gate.rs):
//   Check, Judge  <-> run_gate::Check, run_gate::Judge
//   Verdict       <-> types::GateVerdict (without Running, which only marks checks in
//                     progress and is never a result of `verdict`)
//   verdict       <-> run_gate::verdict
// Used by lucina-agentd's gate (crates/lucina-agentd/src/gate.rs) after every run.
// Differential test: crates/lucina-agents/tests/verus_run_gate.rs.
//
// Proven (for every combination):
//   V1 verified_needs_all: Verified ==> diffgate passed, the proof passed, the judge did
//      not flag the run and the run did not touch the gate config. A judge that was off
//      or could not answer does not make a run verified on its own.
//   V2 refused_iff_failed: Refused <==> diffgate or the proof failed.
//   V3 suspicious_only_on_flags: Suspicious ==> nothing failed and (the judge flagged the
//      run or it tampered).
//   V4 no_proof_no_verified: a skipped proof (no proof command) or skipped diffgate
//      never gives Verified.
//   V5 judge_is_advisory: the judge never turns a failed check into anything but Refused,
//      and a clean judge never upgrades a run without a proof to Verified.
use vstd::prelude::*;

verus! {

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Check {
    Pass,
    Fail,
    Skipped,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Judge {
    Clean,
    Flagged,
    Skipped,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Verdict {
    Verified,
    Refused,
    Suspicious,
    Unverified,
}

fn failed(c: Check) -> (b: bool)
    ensures
        b == (c == Check::Fail),
{
    match c {
        Check::Fail => true,
        _ => false,
    }
}

fn passed(c: Check) -> (b: bool)
    ensures
        b == (c == Check::Pass),
{
    match c {
        Check::Pass => true,
        _ => false,
    }
}

fn flagged(j: Judge) -> (b: bool)
    ensures
        b == (j == Judge::Flagged),
{
    match j {
        Judge::Flagged => true,
        _ => false,
    }
}

pub fn verdict(diffgate: Check, proof: Check, judge: Judge, tampered: bool) -> (v: Verdict)
    ensures
        // V1
        v == Verdict::Verified ==> diffgate == Check::Pass && proof == Check::Pass && judge
            != Judge::Flagged && !tampered,
        // V2
        (v == Verdict::Refused) == (diffgate == Check::Fail || proof == Check::Fail),
        // V3
        v == Verdict::Suspicious ==> diffgate != Check::Fail && proof != Check::Fail && (judge
            == Judge::Flagged || tampered),
        // V4
        proof == Check::Skipped || diffgate == Check::Skipped ==> v != Verdict::Verified,
        // V5
        (diffgate == Check::Fail || proof == Check::Fail) ==> v == Verdict::Refused,
        // completeness: every clean pass is verified
        diffgate == Check::Pass && proof == Check::Pass && judge != Judge::Flagged && !tampered
            ==> v == Verdict::Verified,
{
    if failed(diffgate) || failed(proof) {
        Verdict::Refused
    } else if flagged(judge) || tampered {
        Verdict::Suspicious
    } else if passed(diffgate) && passed(proof) {
        Verdict::Verified
    } else {
        Verdict::Unverified
    }
}

} // verus!

fn main() {}
