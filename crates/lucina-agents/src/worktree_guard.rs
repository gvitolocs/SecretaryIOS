//! Where an agent may write, and when agentd may remove a worktree (design:
//! "Workspaces and worktrees"; proven in `verification/verus/worktree_guard.rs`).
//!
//! The daemon reduces a workspace to [`StartFacts`] / [`RemoveFacts`] (paths are
//! compared in `lucina-agentd`) and acts only on what these functions return.
//!
//! - **Main checkouts stay clean:** an agent that can edit never runs in a main checkout
//!   under the owner's projects root, unless the owner explicitly allows it for that run.
//!   Read-only agents (plan mode) may.
//! - **Worktrees live in `~/Projects/.worktrees/<Repo>/`:** a stray worktree elsewhere is
//!   refused (the owner can override).
//! - **Codex rule:** in a Codex-restricted repo (SecretaryIOS), Codex runs only in a
//!   worktree there, on a `codex/` branch made from `lucina-rs`. No override.
//! - **Removal:** a worktree directory is removed only when agentd owns it, it is clean
//!   and no agent is running in it; its branch is deleted only when it is merged too.
//!   The merge sweep removes only merged worktrees.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Not a git checkout.
    Directory,
    /// A repository's main working tree.
    LocalCheckout,
    /// A linked worktree.
    Worktree,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StartFacts {
    pub kind: Option<Kind>,
    /// A main checkout under the owner's projects root.
    pub protected: bool,
    /// The worktree is under `<worktrees root>/<Repo>/`.
    pub under_root: bool,
    /// The agent's mode can't edit (plan).
    pub read_only: bool,
    /// The owner allowed this run in a main checkout or stray worktree.
    pub owner_override: bool,
    pub codex: bool,
    pub codex_repo: bool,
    pub codex_branch: bool,
    pub lucina_rs_base: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    MainCheckout,
    StrayWorktree,
    CodexRule,
}

impl Refusal {
    pub fn message(self) -> &'static str {
        match self {
            Refusal::MainCheckout => "Agents don't edit a main checkout: use a worktree (--new-workspace worktree), plan mode, or --allow-main-checkout",
            Refusal::StrayWorktree => "This worktree is outside ~/Projects/.worktrees/<Repo>/: create one with --new-workspace worktree, or pass --allow-main-checkout",
            Refusal::CodexRule => "Codex works in SecretaryIOS only in a worktree on a codex/ branch made from lucina-rs",
        }
    }
}

pub fn check_start(f: &StartFacts) -> Result<(), Refusal> {
    let kind = f.kind.unwrap_or(Kind::Directory);
    if f.codex && f.codex_repo && !(kind == Kind::Worktree && f.under_root && f.codex_branch && f.lucina_rs_base) {
        return Err(Refusal::CodexRule);
    }
    if kind == Kind::Worktree && !f.under_root && !f.owner_override {
        return Err(Refusal::StrayWorktree);
    }
    if kind == Kind::LocalCheckout && f.protected && !f.read_only && !f.owner_override {
        return Err(Refusal::MainCheckout);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The owner archived the workspace.
    Archive,
    /// The merge sweep found the branch merged.
    Merged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RemoveFacts {
    /// agentd created this worktree (under the worktrees root).
    pub owned: bool,
    pub clean: bool,
    /// An agent is running in it.
    pub busy: bool,
    /// Every commit of the branch is on its base (or another branch).
    pub merged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Removal {
    pub remove_dir: bool,
    pub delete_branch: bool,
}

pub fn removal(f: &RemoveFacts, trigger: Trigger) -> Removal {
    let may = f.owned && f.clean && !f.busy;
    let remove_dir = match trigger {
        Trigger::Archive => may,
        Trigger::Merged => may && f.merged,
    };
    Removal { remove_dir, delete_branch: remove_dir && f.merged }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_rules() {
        let main = StartFacts { kind: Some(Kind::LocalCheckout), protected: true, ..Default::default() };
        assert_eq!(check_start(&main), Err(Refusal::MainCheckout));
        assert_eq!(check_start(&StartFacts { read_only: true, ..main }), Ok(()));
        assert_eq!(check_start(&StartFacts { owner_override: true, ..main }), Ok(()));
        let tmp = StartFacts { kind: Some(Kind::LocalCheckout), ..Default::default() };
        assert_eq!(check_start(&tmp), Ok(()), "a repo outside ~/Projects");
        let stray = StartFacts { kind: Some(Kind::Worktree), ..Default::default() };
        assert_eq!(check_start(&stray), Err(Refusal::StrayWorktree));
        let codex = StartFacts { kind: Some(Kind::Worktree), under_root: true, codex: true, codex_repo: true, codex_branch: true, ..Default::default() };
        assert_eq!(check_start(&codex), Err(Refusal::CodexRule), "not from lucina-rs");
        assert_eq!(check_start(&StartFacts { lucina_rs_base: true, ..codex }), Ok(()));
        assert_eq!(check_start(&StartFacts { owner_override: true, kind: Some(Kind::LocalCheckout), ..codex }), Err(Refusal::CodexRule));
    }

    #[test]
    fn removal_rules() {
        let f = RemoveFacts { owned: true, clean: true, busy: false, merged: false };
        assert_eq!(removal(&f, Trigger::Archive), Removal { remove_dir: true, delete_branch: false });
        assert_eq!(removal(&f, Trigger::Merged), Removal::default());
        assert_eq!(removal(&RemoveFacts { merged: true, ..f }, Trigger::Merged), Removal { remove_dir: true, delete_branch: true });
        assert_eq!(removal(&RemoveFacts { clean: false, merged: true, ..f }, Trigger::Archive), Removal::default());
    }
}
