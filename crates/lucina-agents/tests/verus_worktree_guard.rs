//! Differential test: `worktree_guard::{check_start, removal}` against a plain-Rust copy
//! of the Verus kernel `verification/verus/worktree_guard.rs`, on every input. The
//! kernel's proven properties (W1-W7) then hold for the production functions too.

use lucina_agents::worktree_guard::{check_start, removal, Kind, Refusal, RemoveFacts, Removal, StartFacts, Trigger};

/// Plain-Rust copy of the Verus kernel.
mod kernel {
    use super::*;

    pub fn check_start(f: StartFacts) -> Result<(), Refusal> {
        let kind = f.kind.unwrap_or(Kind::Directory);
        let worktree = kind == Kind::Worktree;
        if f.codex && f.codex_repo && !(worktree && f.under_root && f.codex_branch && f.lucina_rs_base) {
            return Err(Refusal::CodexRule);
        }
        if worktree && !f.under_root && !f.owner_override {
            return Err(Refusal::StrayWorktree);
        }
        if kind == Kind::LocalCheckout && f.protected && !f.read_only && !f.owner_override {
            return Err(Refusal::MainCheckout);
        }
        Ok(())
    }

    pub fn removal(f: RemoveFacts, trigger: Trigger) -> Removal {
        let may = f.owned && f.clean && !f.busy;
        let remove_dir = match trigger {
            Trigger::Archive => may,
            Trigger::Merged => may && f.merged,
        };
        Removal { remove_dir, delete_branch: remove_dir && f.merged }
    }
}

fn bits(n: u32, i: u32) -> bool {
    n & (1 << i) != 0
}

#[test]
fn every_start() {
    for kind in [Kind::Directory, Kind::LocalCheckout, Kind::Worktree] {
        for n in 0..256u32 {
            let f = StartFacts {
                kind: Some(kind),
                protected: bits(n, 0),
                under_root: bits(n, 1),
                read_only: bits(n, 2),
                owner_override: bits(n, 3),
                codex: bits(n, 4),
                codex_repo: bits(n, 5),
                codex_branch: bits(n, 6),
                lucina_rs_base: bits(n, 7),
            };
            assert_eq!(check_start(&f), kernel::check_start(f), "{f:?}");
        }
    }
}

#[test]
fn every_removal() {
    for trigger in [Trigger::Archive, Trigger::Merged] {
        for n in 0..16u32 {
            let f = RemoveFacts { owned: bits(n, 0), clean: bits(n, 1), busy: bits(n, 2), merged: bits(n, 3) };
            assert_eq!(removal(&f, trigger), kernel::removal(f, trigger), "{f:?} {trigger:?}");
        }
    }
}
