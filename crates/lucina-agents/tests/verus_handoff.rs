//! Differential test: `lucina_agents::handoff` against a plain-Rust copy of the Verus kernel
//! `verification/verus/agent_handoff.rs` (exec part), on every fact combination and every
//! chain of up to four lanes. The kernel's proven properties (H1-H7) then hold for the
//! production functions too.

use lucina_agents::handoff::{self, Candidate, FailureClass, RunFacts};

/// Plain-Rust copy of the Verus kernel (exec part).
mod kernel {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Class {
        Quota,
        Unavailable,
        Failed,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Facts {
        pub class: Class,
        pub internal: bool,
        pub external: bool,
        pub chain_run: bool,
        pub archived: bool,
        pub handoffs: u32,
        pub dirty: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Cand {
        pub registered: bool,
        pub codex: bool,
        pub forbidden: bool,
        pub tried: bool,
    }

    pub const MAX_HANDOFFS: u32 = 8;

    fn is_failed(c: Class) -> bool {
        matches!(c, Class::Failed)
    }

    pub fn should_hand_off(f: &Facts) -> bool {
        !is_failed(f.class) && !f.internal && !f.external && !f.chain_run && !f.archived && f.handoffs < MAX_HANDOFFS
    }

    pub fn eligible(c: &Cand) -> bool {
        c.registered && !c.codex && !c.forbidden && !c.tried
    }

    pub fn next_lane(chain: &[Cand]) -> Option<usize> {
        let mut i = 0;
        while i < chain.len() {
            if eligible(&chain[i]) {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    pub fn decide(f: &Facts, chain: &[Cand]) -> Option<usize> {
        if should_hand_off(f) {
            next_lane(chain)
        } else {
            None
        }
    }
}

fn k_class(c: FailureClass) -> kernel::Class {
    match c {
        FailureClass::Quota => kernel::Class::Quota,
        FailureClass::Unavailable => kernel::Class::Unavailable,
        FailureClass::Failed => kernel::Class::Failed,
    }
}

fn k_facts(f: &RunFacts) -> kernel::Facts {
    kernel::Facts {
        class: k_class(f.class),
        internal: f.internal,
        external: f.external,
        chain_run: f.chain_run,
        archived: f.archived,
        handoffs: f.handoffs,
        dirty: f.dirty,
    }
}

fn k_cand(c: &Candidate) -> kernel::Cand {
    kernel::Cand { registered: c.registered, codex: c.codex, forbidden: c.forbidden, tried: c.tried }
}

const CLASSES: [FailureClass; 3] = [FailureClass::Quota, FailureClass::Unavailable, FailureClass::Failed];

fn all_facts() -> Vec<RunFacts> {
    let mut v = Vec::new();
    let counts = [0, 1, handoff::MAX_HANDOFFS - 1, handoff::MAX_HANDOFFS, handoff::MAX_HANDOFFS + 1, u32::MAX];
    for class in CLASSES {
        for bits in 0u32..32 {
            for handoffs in counts {
                let b = |i: u32| bits & (1 << i) != 0;
                v.push(RunFacts { class, internal: b(0), external: b(1), chain_run: b(2), archived: b(3), dirty: b(4), handoffs });
            }
        }
    }
    v
}

fn candidate(bits: u32) -> Candidate {
    Candidate { registered: bits & 1 != 0, codex: bits & 2 != 0, forbidden: bits & 4 != 0, tried: bits & 8 != 0 }
}

/// Every chain of length 0..=4 over the 16 kinds of candidate.
fn all_chains() -> Vec<Vec<Candidate>> {
    let mut out = vec![Vec::new()];
    let mut layer = vec![Vec::new()];
    for _ in 0..4 {
        let mut next = Vec::new();
        for chain in &layer {
            for bits in 0..16 {
                let mut c: Vec<Candidate> = chain.clone();
                c.push(candidate(bits));
                next.push(c);
            }
        }
        out.extend(next.iter().cloned());
        layer = next;
    }
    out
}

fn quota() -> RunFacts {
    RunFacts { class: FailureClass::Quota, internal: false, external: false, chain_run: false, archived: false, handoffs: 0, dirty: false }
}

#[test]
fn quota_limit_matches_kernel() {
    assert_eq!(handoff::MAX_HANDOFFS, kernel::MAX_HANDOFFS);
}

#[test]
fn exhaustive_facts() {
    let facts = all_facts();
    assert_eq!(facts.len(), 3 * 32 * 6);
    for f in &facts {
        assert_eq!(handoff::should_hand_off(f), kernel::should_hand_off(&k_facts(f)), "{f:?}");
        // H7: dirty changes nothing.
        let flipped = RunFacts { dirty: !f.dirty, ..*f };
        assert_eq!(handoff::should_hand_off(f), handoff::should_hand_off(&flipped), "{f:?}");
    }
}

#[test]
fn exhaustive_chains() {
    let chains = all_chains();
    assert_eq!(chains.len(), 1 + 16 + 256 + 4096 + 65536);
    let failed = RunFacts { class: FailureClass::Failed, ..quota() };
    for chain in &chains {
        let k: Vec<kernel::Cand> = chain.iter().map(k_cand).collect();
        assert_eq!(handoff::next_lane(chain), kernel::next_lane(&k), "{chain:?}");
        assert_eq!(handoff::decide(&quota(), chain), kernel::decide(&k_facts(&quota()), &k), "{chain:?}");
        assert_eq!(handoff::decide(&failed, chain), None);
        assert_eq!(kernel::decide(&k_facts(&failed), &k), None);
    }
}

#[test]
fn properties_hold_on_production() {
    for chain in all_chains() {
        match handoff::decide(&quota(), &chain) {
            Some(i) => {
                let c = chain[i];
                assert!(c.registered && !c.codex && !c.forbidden && !c.tried, "{chain:?}");
                assert!(chain[..i].iter().all(|c| !handoff::eligible(c)), "{chain:?}");
            }
            None => assert!(chain.iter().all(|c| !handoff::eligible(c)), "{chain:?}"),
        }
    }
}
