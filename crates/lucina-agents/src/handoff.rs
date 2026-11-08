//! Quota hand-off: whether a failed run continues on another agent, and on which.
//!
//! When a provider stops a run on a quota, a usage limit or a rate limit (or is
//! unavailable: login gone, server down), lucina-agentd starts a linked continuation
//! on the next lane of a Lucina chain (lane's `deep-lucina` order) with the original
//! task, the chat history and the same workspace, uncommitted changes included. A
//! real failure is never handed on. This module is the pure decision; the daemon
//! (`lucina-agentd/src/manager/handoff.rs`) gathers the facts and acts on it.
//!
//! Verus model: `verification/verus/agent_handoff.rs` (H1-H7 there); differential test
//! `tests/verus_handoff.rs`.

/// How the lane detectors classified the failure (lane `Outcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// Quota, credits, usage or rate limit.
    Quota,
    /// Login missing, provider down or busy.
    Unavailable,
    /// Anything else: the task's own failure.
    Failed,
}

/// What the decision reads about the failed run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunFacts {
    pub class: FailureClass,
    /// A hidden helper agent (titles, branch names).
    pub internal: bool,
    /// An agent another host runs, or a Claude session's mirror.
    pub external: bool,
    /// An explicit `lagent run --chain` run: its own chain loop decides.
    pub chain_run: bool,
    pub archived: bool,
    /// How many hand-offs already led to this run.
    pub handoffs: u32,
    /// The workspace has uncommitted changes. Never a reason to stop: the
    /// continuation carries on from them (owner, 2026-11-08).
    pub dirty: bool,
}

/// At most this many hand-offs in a row (the chain is shorter anyway).
pub const MAX_HANDOFFS: u32 = 8;

/// One lane of the chain, as the daemon sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// Its provider is registered in agentd.
    pub registered: bool,
    /// Codex: never picked automatically (no Codex on Lucina; the driver-only
    /// Codex terra exception is the owner's call, not the daemon's).
    pub codex: bool,
    /// The paid DeepSeek key, or anything else the owner forbade.
    pub forbidden: bool,
    /// This run or one before it in the hand-off already used the provider.
    pub tried: bool,
}

/// Whether the failed run is handed on at all (H1-H3, H7).
pub fn should_hand_off(f: &RunFacts) -> bool {
    f.class != FailureClass::Failed && !f.internal && !f.external && !f.chain_run && !f.archived && f.handoffs < MAX_HANDOFFS
}

/// A lane the continuation may run on (H4).
pub fn eligible(c: &Candidate) -> bool {
    c.registered && !c.codex && !c.forbidden && !c.tried
}

/// The first eligible lane in chain order (H5, H6), or `None`.
pub fn next_lane(chain: &[Candidate]) -> Option<usize> {
    chain.iter().position(eligible)
}

/// The whole decision: `Some(lane index)` to hand on, `None` to leave the run in error.
pub fn decide(f: &RunFacts, chain: &[Candidate]) -> Option<usize> {
    if should_hand_off(f) {
        next_lane(chain)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(class: FailureClass) -> RunFacts {
        RunFacts { class, internal: false, external: false, chain_run: false, archived: false, handoffs: 0, dirty: false }
    }

    const OK: Candidate = Candidate { registered: true, codex: false, forbidden: false, tried: false };

    #[test]
    fn quota_and_unavailable_hand_on_failed_does_not() {
        let chain = [OK];
        assert_eq!(decide(&facts(FailureClass::Quota), &chain), Some(0));
        assert_eq!(decide(&facts(FailureClass::Unavailable), &chain), Some(0));
        assert_eq!(decide(&facts(FailureClass::Failed), &chain), None);
    }

    #[test]
    fn dirty_workspace_is_handed_on() {
        let f = RunFacts { dirty: true, ..facts(FailureClass::Quota) };
        assert_eq!(decide(&f, &[OK]), Some(0));
    }

    #[test]
    fn helpers_externals_chains_and_archived_stay() {
        let q = facts(FailureClass::Quota);
        for f in [
            RunFacts { internal: true, ..q },
            RunFacts { external: true, ..q },
            RunFacts { chain_run: true, ..q },
            RunFacts { archived: true, ..q },
            RunFacts { handoffs: MAX_HANDOFFS, ..q },
        ] {
            assert_eq!(decide(&f, &[OK]), None, "{f:?}");
        }
        assert_eq!(decide(&RunFacts { handoffs: MAX_HANDOFFS - 1, ..q }, &[OK]), Some(0));
    }

    #[test]
    fn first_eligible_skipping_tried_codex_forbidden_and_missing() {
        let chain = [
            Candidate { tried: true, ..OK },
            Candidate { codex: true, ..OK },
            Candidate { forbidden: true, ..OK },
            Candidate { registered: false, ..OK },
            OK,
            OK,
        ];
        assert_eq!(decide(&facts(FailureClass::Quota), &chain), Some(4));
        assert_eq!(decide(&facts(FailureClass::Quota), &chain[..4]), None);
        assert_eq!(decide(&facts(FailureClass::Quota), &[]), None);
    }
}
