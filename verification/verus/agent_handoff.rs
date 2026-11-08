// Quota hand-off kernel (Lucina Agents: whether a failed run continues on another agent,
// and on which lane of the Lucina chain).
//
// Production: crates/lucina-agents/src/handoff.rs
//   Class      <-> handoff::FailureClass
//   Facts      <-> handoff::RunFacts
//   Cand       <-> handoff::Candidate
//   should_hand_off / eligible / next_lane / decide <-> the functions of the same name
// Differential test: crates/lucina-agents/tests/verus_handoff.rs.
// Mutants (each must fail verification): verification/verus/mutants/agent_handoff/*.rs,
// run by scripts/verus-mutants-agents.sh.
//
// Proven:
//   H1 failed_stays: a real failure (class Failed) is never handed on.
//   H2 excluded_stay: internal helpers, external agents, explicit chain runs and archived
//      agents are never handed on.
//   H3 bounded: a hand-off happens only while fewer than MAX_HANDOFFS led to the run, so a
//      sequence of hand-offs ends.
//   H4 picked_is_allowed: the picked lane is registered, not Codex, not forbidden (paid
//      DeepSeek) and not already tried.
//   H5 first_in_order: every lane before the picked one is not eligible (chain order kept).
//   H6 none_means_none_left: no lane picked although the run qualifies only when no lane of
//      the chain is eligible.
//   H7 dirty_irrelevant: uncommitted changes never change the decision (the continuation
//      carries on from them).
use vstd::prelude::*;

verus! {

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Class {
    Quota,
    Unavailable,
    Failed,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub struct Facts {
    pub class: Class,
    pub internal: bool,
    pub external: bool,
    pub chain_run: bool,
    pub archived: bool,
    pub handoffs: u32,
    pub dirty: bool,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub struct Cand {
    pub registered: bool,
    pub codex: bool,
    pub forbidden: bool,
    pub tried: bool,
}

pub const MAX_HANDOFFS: u32 = 8;

pub open spec fn spec_should(f: Facts) -> bool {
    f.class != Class::Failed && !f.internal && !f.external && !f.chain_run && !f.archived
        && f.handoffs < MAX_HANDOFFS
}

pub open spec fn spec_eligible(c: Cand) -> bool {
    c.registered && !c.codex && !c.forbidden && !c.tried
}

fn is_failed(c: Class) -> (b: bool)
    ensures
        b == (c == Class::Failed),
{
    match c {
        Class::Failed => true,
        _ => false,
    }
}

pub fn should_hand_off(f: &Facts) -> (b: bool)
    ensures
        b == spec_should(*f),
        // H1
        f.class == Class::Failed ==> !b,
        // H2
        (f.internal || f.external || f.chain_run || f.archived) ==> !b,
        // H3
        b ==> f.handoffs < MAX_HANDOFFS,
{
    !is_failed(f.class) && !f.internal && !f.external && !f.chain_run && !f.archived
        && f.handoffs < MAX_HANDOFFS
}

pub fn eligible(c: &Cand) -> (b: bool)
    ensures
        b == spec_eligible(*c),
{
    c.registered && !c.codex && !c.forbidden && !c.tried
}

pub fn next_lane(chain: &[Cand]) -> (r: Option<usize>)
    ensures
        match r {
            // H4, H5
            Some(i) => i < chain@.len() && spec_eligible(chain@[i as int]) && (forall|j: int|
                0 <= j < i ==> !spec_eligible(#[trigger] chain@[j])),
            // H6
            None => forall|j: int| 0 <= j < chain@.len() ==> !spec_eligible(#[trigger] chain@[j]),
        },
{
    let mut i: usize = 0;
    while i < chain.len()
        invariant
            0 <= i <= chain@.len(),
            forall|j: int| 0 <= j < i ==> !spec_eligible(#[trigger] chain@[j]),
        decreases chain@.len() - i,
    {
        if eligible(&chain[i]) {
            return Some(i);
        }
        i = i + 1;
    }
    None
}

pub fn decide(f: &Facts, chain: &[Cand]) -> (r: Option<usize>)
    ensures
        !spec_should(*f) ==> r is None,
        // H1
        f.class == Class::Failed ==> r is None,
        match r {
            // H3, H4, H5
            Some(i) => spec_should(*f) && i < chain@.len() && chain@[i as int].registered
                && !chain@[i as int].codex && !chain@[i as int].forbidden && !chain@[i as int].tried
                && (forall|j: int| 0 <= j < i ==> !spec_eligible(#[trigger] chain@[j])),
            // H6
            None => spec_should(*f) ==> (forall|j: int|
                0 <= j < chain@.len() ==> !spec_eligible(#[trigger] chain@[j])),
        },
{
    if should_hand_off(f) {
        next_lane(chain)
    } else {
        None
    }
}

// H7: flipping `dirty` changes nothing.
pub proof fn dirty_irrelevant(f: Facts)
    ensures
        spec_should(f) == spec_should(
            Facts {
                class: f.class,
                internal: f.internal,
                external: f.external,
                chain_run: f.chain_run,
                archived: f.archived,
                handoffs: f.handoffs,
                dirty: !f.dirty,
            },
        ),
{
}

} // verus!

fn main() {}
