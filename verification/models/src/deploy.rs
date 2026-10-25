//! Multi-session merge and deploy (`/tmp/lucina-merge.lock`, the shared
//! main checkout, deploys of secretary-server, the web app and lucina-rs).
//!
//! Actors are Claude sessions. Each has one feature on its own branch, may
//! also leave uncommitted work in the shared main checkout (the Desk/Hermes
//! and calls edits of 24 Oct), and deploys to the runtimes. Commit content is
//! modelled as a set of features (a bit mask): "commit A is an ancestor of B"
//! becomes "A's features are a subset of B's", which is what a rollback check
//! needs. Session `i`'s feature is bit `i`; its uncommitted checkout work is
//! bit `4 + i`.
//!
//! The procedures (the switches on [`Deploy`]):
//!
//! - today (25 Oct): the lock is taken, but a deploy may be built from the
//!   dirty checkout, from main or from a feature branch, with no check of what
//!   is live. Lucina Ink deployed secretary-server from a clean main that
//!   lacked the live calls module: `/v1/calls/*` went from 401 to 404.
//! - the fix: builds only from main's tip in a clean worktree, inside the
//!   lock, and `deploy-guard check` refuses unless the live build is an
//!   ancestor of the commit and the commit is on main. Live but uncommitted
//!   work is committed first (the "align main with production" step).

use stateright::{Model, Property};

use crate::{Case, Expect};

pub const PROTOCOL: &str = "deploy";

/// A build: its content and whether it came from a dirty tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Build {
    pub content: u8,
    pub dirty: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sess {
    /// The feature branch (content when branched off main).
    pub branch: Option<u8>,
    /// Main as read by a merge in progress (fetch, then push).
    pub read: Option<u8>,
    pub merged: bool,
    pub build: Option<Build>,
    /// The runtime a build is about to be pushed to (after the guard check).
    pub target: Option<u8>,
    pub locked: bool,
    /// Left uncommitted work in the shared checkout.
    pub edited: bool,
    /// Runtimes this session has deployed its work to (bit mask).
    pub deployed: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub main: u8,
    /// Uncommitted work in the shared main checkout.
    pub dirty: u8,
    pub lock: Option<u8>,
    /// What runs per runtime.
    pub prod: Vec<Build>,
    /// Ghost: every feature ever deployed per runtime.
    pub ever: Vec<u8>,
    /// Ghost: some deploy shipped a dirty tree.
    pub dirty_deployed: bool,
    /// Ghost: the guard refused a deploy.
    pub refused: bool,
    pub sessions: Vec<Sess>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    /// Branch off main and commit the feature (`wt add`).
    Branch(u8),
    /// Leave uncommitted work in the shared main checkout.
    EditCheckout(u8),
    /// `mkdir /tmp/lucina-merge.lock`.
    Acquire(u8),
    /// Fetch main (the merge's base).
    MergeRead(u8),
    /// Push the merge: main = what was read + the branch.
    MergeWrite(u8),
    /// Commit the checkout's live uncommitted work to main.
    CommitWip(u8),
    BuildMain(u8),
    BuildCheckout(u8),
    BuildBranch(u8),
    /// Pick runtime `r` for the build; with the guard, `deploy-guard check`
    /// runs here and may refuse.
    Check(u8, u8),
    /// rsync / install the build (a separate step: check-then-push).
    Push(u8),
    /// `rmdir /tmp/lucina-merge.lock`.
    Release(u8),
}

#[derive(Clone, Debug)]
pub struct Deploy {
    pub sessions: u8,
    pub runtimes: u8,
    /// Every merge, build and deploy happens inside the lock.
    pub lock_required: bool,
    /// A deploy may be built from the shared checkout (dirty or not).
    pub checkout_builds: bool,
    /// A deploy may be built from a feature branch.
    pub branch_builds: bool,
    /// `deploy-guard check` before every deploy.
    pub guard: bool,
}

const fn feature(i: u8) -> u8 {
    1 << i
}

const fn wip(i: u8) -> u8 {
    1 << (4 + i)
}

impl Deploy {
    fn all_features(&self) -> u8 {
        (0..self.sessions).fold(0, |m, i| m | feature(i))
    }

    fn all_runtimes(&self) -> u8 {
        (1u8 << self.runtimes) - 1
    }

    /// Inside the critical section: with the lock, or (old habit) without it.
    fn may_work(&self, s: &Sess) -> bool {
        s.locked || !self.lock_required
    }

    /// `deploy-guard check`: the tree is clean, the commit is on main, and the
    /// live build is an ancestor of the commit.
    pub fn guard_allows(live: Build, build: Build, main: u8) -> bool {
        !build.dirty && build.content & !main == 0 && live.content & !build.content == 0
    }

    fn in_section(s: &Sess) -> bool {
        s.read.is_some() || s.build.is_some() || s.target.is_some()
    }
}

impl Model for Deploy {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let base = Build {
            content: 0,
            dirty: false,
        };
        vec![State {
            main: 0,
            dirty: 0,
            lock: None,
            prod: vec![base; self.runtimes as usize],
            ever: vec![0; self.runtimes as usize],
            dirty_deployed: false,
            refused: false,
            sessions: vec![
                Sess {
                    branch: None,
                    read: None,
                    merged: false,
                    build: None,
                    target: None,
                    locked: false,
                    edited: false,
                    deployed: 0,
                };
                self.sessions as usize
            ],
        }]
    }

    fn actions(&self, st: &State, out: &mut Vec<Action>) {
        let done_all = self.all_runtimes();
        for i in 0..self.sessions {
            let s = &st.sessions[i as usize];
            let done = s.deployed == done_all;
            if s.branch.is_none() {
                out.push(Action::Branch(i));
            }
            if !s.edited && s.branch.is_none() {
                out.push(Action::EditCheckout(i));
            }
            if s.branch.is_some() && !s.locked && st.lock.is_none() && !done {
                out.push(Action::Acquire(i));
            }
            let work = self.may_work(s) && !done;
            if work && s.branch.is_some() && !s.merged && s.read.is_none() {
                out.push(Action::MergeRead(i));
            }
            if s.read.is_some() {
                out.push(Action::MergeWrite(i));
            }
            if work && st.dirty != 0 && s.build.is_none() {
                out.push(Action::CommitWip(i));
            }
            if work && s.build.is_none() && s.read.is_none() {
                if s.merged {
                    out.push(Action::BuildMain(i));
                    if self.checkout_builds {
                        out.push(Action::BuildCheckout(i));
                    }
                }
                if self.branch_builds && s.branch.is_some() {
                    out.push(Action::BuildBranch(i));
                }
            }
            if s.build.is_some() && s.target.is_none() {
                for r in 0..self.runtimes {
                    if s.deployed & (1 << r) == 0 {
                        out.push(Action::Check(i, r));
                    }
                }
            }
            if s.target.is_some() {
                out.push(Action::Push(i));
            }
            if s.locked && s.read.is_none() && s.build.is_none() {
                out.push(Action::Release(i));
            }
        }
    }

    fn next_state(&self, st: &State, a: Action) -> Option<State> {
        let mut n = st.clone();
        match a {
            Action::Branch(i) => {
                n.sessions[i as usize].branch = Some(st.main | feature(i));
            }
            Action::EditCheckout(i) => {
                n.dirty |= wip(i);
                n.sessions[i as usize].edited = true;
            }
            Action::Acquire(i) => {
                n.lock = Some(i);
                n.sessions[i as usize].locked = true;
            }
            Action::MergeRead(i) => {
                n.sessions[i as usize].read = Some(st.main);
            }
            Action::MergeWrite(i) => {
                let s = &mut n.sessions[i as usize];
                n.main = s.read.take()? | s.branch?;
                s.merged = true;
            }
            Action::CommitWip(_) => {
                n.main |= st.dirty;
                n.dirty = 0;
            }
            Action::BuildMain(i) => {
                n.sessions[i as usize].build = Some(Build {
                    content: st.main,
                    dirty: false,
                });
            }
            Action::BuildCheckout(i) => {
                n.sessions[i as usize].build = Some(Build {
                    content: st.main | st.dirty,
                    dirty: st.dirty != 0,
                });
            }
            Action::BuildBranch(i) => {
                let b = st.sessions[i as usize].branch?;
                n.sessions[i as usize].build = Some(Build {
                    content: b,
                    dirty: false,
                });
            }
            Action::Check(i, r) => {
                let b = st.sessions[i as usize].build?;
                if self.guard && !Self::guard_allows(st.prod[r as usize], b, st.main) {
                    n.refused = true;
                    n.sessions[i as usize].build = None;
                    return Some(n);
                }
                n.sessions[i as usize].target = Some(r);
            }
            Action::Push(i) => {
                let s = &mut n.sessions[i as usize];
                let r = s.target.take()?;
                let b = s.build.take()?;
                n.prod[r as usize] = b;
                n.ever[r as usize] |= b.content;
                n.dirty_deployed |= b.dirty;
                s.deployed |= 1 << r;
            }
            Action::Release(i) => {
                n.lock = None;
                n.sessions[i as usize].locked = false;
            }
        }
        Some(n)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            // Every deploy's commit contains every commit deployed before on that runtime.
            Property::always("no_rollback", |_, s: &State| {
                s.prod.iter().zip(&s.ever).all(|(p, e)| e & !p.content == 0)
            }),
            // Production is always a commit on main (no uncommitted or unmerged code live).
            Property::always("prod_on_main", |_, s: &State| {
                s.prod.iter().all(|p| !p.dirty && p.content & !s.main == 0)
            }),
            // The merge/build/deploy section is never occupied by two sessions.
            Property::always("single_holder", |_, s: &State| {
                s.sessions.iter().filter(|x| Deploy::in_section(x)).count() <= 1
            }),
            Property::always("no_dirty_deploy", |_, s: &State| !s.dirty_deployed),
            // Not vacuous: every session's feature reaches every runtime.
            Property::sometimes("all_features_live", |m: &Deploy, s: &State| {
                let all = m.all_features();
                s.prod.iter().all(|p| p.content & all == all)
            }),
            // The guard has work to do (it refuses a deploy that would roll back).
            Property::sometimes("guard_refused", |_, s: &State| s.refused),
        ]
    }
}

pub fn cases(deep: bool) -> Vec<Case> {
    use Expect::*;
    // The bug witnesses need two sessions; the deep run checks the fix with three.
    let fixed_sessions = if deep { 3 } else { 2 };
    let model = |lock_required, checkout_builds, branch_builds, guard| Deploy {
        sessions: if guard && lock_required {
            fixed_sessions
        } else {
            2
        },
        runtimes: 2,
        lock_required,
        checkout_builds,
        branch_builds,
        guard,
    };
    vec![
        Case::new(
            PROTOCOL,
            "D1 24-25 Oct: lock, builds from the checkout or main, no guard",
            model(true, true, false, false),
            None,
            vec![
                ("no_rollback", Violated),
                ("prod_on_main", Violated),
                ("single_holder", Holds),
                ("no_dirty_deploy", Violated),
                ("all_features_live", Reached),
            ],
        ),
        Case::new(
            PROTOCOL,
            "D2 lock skipped (old habit), clean builds only",
            model(false, false, false, false),
            None,
            vec![
                ("no_rollback", Violated),
                ("single_holder", Violated),
                ("no_dirty_deploy", Holds),
                ("all_features_live", Reached),
            ],
        ),
        Case::new(
            PROTOCOL,
            "D3 lock + clean builds, branch deploys, no guard",
            model(true, false, true, false),
            None,
            vec![
                ("no_rollback", Violated),
                ("prod_on_main", Violated),
                ("single_holder", Holds),
                ("no_dirty_deploy", Holds),
                ("all_features_live", Reached),
            ],
        ),
        Case::new(
            PROTOCOL,
            "D4 fix: lock + deploy-guard + commit live WIP",
            model(true, false, true, true),
            None,
            vec![
                ("no_rollback", Holds),
                ("prod_on_main", Holds),
                ("single_holder", Holds),
                ("no_dirty_deploy", Holds),
                ("all_features_live", Reached),
                ("guard_refused", Reached),
            ],
        ),
        Case::new(
            PROTOCOL,
            "D5 guard without the lock (check-then-push race)",
            model(false, false, true, true),
            None,
            vec![
                ("no_rollback", Violated),
                ("prod_on_main", Violated),
                ("single_holder", Violated),
                ("no_dirty_deploy", Holds),
                ("all_features_live", Reached),
                ("guard_refused", Reached),
            ],
        ),
    ]
}
