// Worktree guard kernel (Lucina Agents: where an agent may write, and when agentd may
// remove a worktree or its branch).
//
// Production (branch agents-p4, crates/lucina-agents/src/worktree_guard.rs):
//   Kind, StartFacts, Refusal, check_start      <-> the same types and function
//   RemoveFacts, Trigger, Removal, removal       <-> the same types and function
// Used by lucina-agentd before every agent start (crates/lucina-agentd/src/manager.rs) and
// before removing a worktree on archive or after a merge (src/workspaces/mod.rs).
// Differential test: crates/lucina-agents/tests/verus_worktree_guard.rs.
//
// Proven (for every input):
//   W1 main_checkout_clean: an accepted start in a protected main checkout is read-only or
//      explicitly allowed by the owner.
//   W2 worktrees_in_root: an accepted start in a worktree is under the worktrees root, or
//      explicitly allowed by the owner.
//   W3 codex_rule: an accepted Codex start in a Codex-restricted repo is in a worktree under
//      the root, on a codex/ branch made from lucina-rs; the owner's override doesn't
//      change that.
//   W4 remove_safe: a worktree directory is removed only when agentd owns it, it is clean
//      and no agent runs in it.
//   W5 branch_merged: a branch is deleted only when it is merged and its directory goes.
//   W6 sweep_merged_only: the merge sweep removes only merged worktrees.
//   W7 directory_free: a plain directory (not git) is always accepted unless the Codex
//      rule applies.
use vstd::prelude::*;

verus! {

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Kind {
    Directory,
    LocalCheckout,
    Worktree,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub struct StartFacts {
    pub kind: Kind,
    pub protected: bool,
    pub under_root: bool,
    pub read_only: bool,
    pub owner_override: bool,
    pub codex: bool,
    pub codex_repo: bool,
    pub codex_branch: bool,
    pub lucina_rs_base: bool,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Refusal {
    MainCheckout,
    StrayWorktree,
    CodexRule,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Trigger {
    Archive,
    Merged,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub struct RemoveFacts {
    pub owned: bool,
    pub clean: bool,
    pub busy: bool,
    pub merged: bool,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub struct Removal {
    pub remove_dir: bool,
    pub delete_branch: bool,
}

fn is_kind(k: Kind, t: Kind) -> (b: bool)
    ensures
        b == (k == t),
{
    match (k, t) {
        (Kind::Directory, Kind::Directory) => true,
        (Kind::LocalCheckout, Kind::LocalCheckout) => true,
        (Kind::Worktree, Kind::Worktree) => true,
        _ => false,
    }
}

pub fn check_start(f: StartFacts) -> (r: Result<(), Refusal>)
    ensures
// W1
        r is Ok && f.kind == Kind::LocalCheckout && f.protected ==> f.read_only || f.owner_override,
        // W2
        r is Ok && f.kind == Kind::Worktree ==> f.under_root || f.owner_override,
        // W3
        r is Ok && f.codex && f.codex_repo ==> f.kind == Kind::Worktree && f.under_root
            && f.codex_branch && f.lucina_rs_base,
        // W7
        f.kind == Kind::Directory && !(f.codex && f.codex_repo) ==> r is Ok,
{
    let worktree = is_kind(f.kind, Kind::Worktree);
    if f.codex && f.codex_repo && !(worktree && f.under_root && f.codex_branch && f.lucina_rs_base) {
        return Err(Refusal::CodexRule);
    }
    if worktree && !f.under_root && !f.owner_override {
        return Err(Refusal::StrayWorktree);
    }
    if is_kind(f.kind, Kind::LocalCheckout) && f.protected && !f.read_only && !f.owner_override {
        return Err(Refusal::MainCheckout);
    }
    Ok(())
}

pub fn removal(f: RemoveFacts, trigger: Trigger) -> (r: Removal)
    ensures
// W4
        r.remove_dir ==> f.owned && f.clean && !f.busy,
        // W5
        r.delete_branch ==> f.merged && r.remove_dir,
        // W6
        trigger == Trigger::Merged && r.remove_dir ==> f.merged,
{
    let may = f.owned && f.clean && !f.busy;
    let remove_dir = match trigger {
        Trigger::Archive => may,
        Trigger::Merged => may && f.merged,
    };
    Removal { remove_dir, delete_branch: remove_dir && f.merged }
}

} // verus!

fn main() {}
