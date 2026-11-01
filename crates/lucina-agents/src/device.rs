//! Which builds may go on a leased device (the owner's iPhone first), without I/O.
//! agentd keeps the leases (`pipeline`: one holder per device, the session id) and
//! does the installs (`lucina-agentd` `devices`); these are the install rules.
//!
//! - A **main install** puts `origin/main`'s tip on the device. It must contain the live
//!   build (deploy-guard's ledger). Someone else's live lease refuses it; without a
//!   lease the installer takes one for the install.
//! - A **test install** (a branch build) or a **test run** (`xcodebuild test`, which
//!   replaces the app with its test host) needs the caller's own lease, and an approval
//!   the owner decided himself (Lucina's decide API), at most a day old, that names the
//!   commit (sha8), the session or the workspace. A test run of `main`'s tip needs no
//!   approval; the Mac side then checks that the checkout is clean.
//! - A **grant** is what the Mac executor shows back to agentd before it builds and
//!   again before it installs: issued for one commit and kind, used once, short-lived,
//!   and only while its holder still holds the device.
//!
//! Why: on 2026-11-01 sessions installed branch builds over each other with raw
//! `devicectl`; one install dropped the photo-upload extension and two never reached
//! the deploy ledger.

use serde::{Deserialize, Serialize};

/// The lease a main install takes when the caller holds none.
pub const AUTO_LEASE_MIN: i64 = 45;
pub const GRANT_TTL_MS: u64 = 60 * 60_000;
pub const APPROVAL_MAX_AGE_MS: u64 = 24 * 3_600_000;

/// A live lease, as far as these rules need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder<'a> {
    pub holder: &'a str,
    pub purpose: &'a str,
    pub expires_at: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstallKind {
    Main,
    Test,
    TestRun,
}

impl InstallKind {
    pub fn as_str(self) -> &'static str {
        match self {
            InstallKind::Main => "main",
            InstallKind::Test => "test",
            InstallKind::TestRun => "test-run",
        }
    }

    /// For messages: "main install", "test build", "test run".
    pub fn noun(self) -> &'static str {
        match self {
            InstallKind::Main => "main install",
            InstallKind::Test => "test build",
            InstallKind::TestRun => "test run",
        }
    }
}

/// A decision from Lucina's decide API (`lucina-rs decide status <id>`), the fields
/// that matter here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    pub id: String,
    pub status: String,
    pub decision: String,
    pub decided_by: String,
    pub decided_at_ms: u64,
    pub question: String,
}

/// Why an approval does not cover this commit for this caller, if it doesn't. The
/// question must name the commit (sha8), the session or the workspace.
pub fn approval_problem(a: &Approval, commit: &str, names: &[&str], now_ms: u64) -> Option<String> {
    if a.status != "decided" {
        return Some(format!("decision {} is {}, not decided", a.id, a.status));
    }
    if a.decision != "go" {
        return Some(format!("decision {} is '{}', not go", a.id, a.decision));
    }
    if a.decided_by != "owner" {
        return Some(format!(
            "decision {} was decided by {}; a test build needs the owner's own answer (ask with --impact high)",
            a.id, a.decided_by
        ));
    }
    let age = now_ms.saturating_sub(a.decided_at_ms);
    if age > APPROVAL_MAX_AGE_MS {
        return Some(format!("decision {} is {} h old (max 24 h)", a.id, age / 3_600_000));
    }
    let sha8 = &commit[..commit.len().min(8)];
    let named = sha8.len() == 8 && a.question.contains(sha8) || names.iter().any(|n| n.len() >= 8 && a.question.contains(n));
    if !named {
        return Some(format!("decision {} does not name commit {sha8} or {} in its question", a.id, names.join(" or ")));
    }
    None
}

/// What agentd found before an install or a test run.
#[derive(Debug, Clone)]
pub struct InstallFacts<'a> {
    pub kind: InstallKind,
    /// The caller (a session id).
    pub holder: &'a str,
    pub workspace_id: Option<&'a str>,
    pub commit: &'a str,
    /// `origin/main`'s tip, fetched fresh.
    pub tip: &'a str,
    /// The commit contains the live build (the newest deploy tag and the ledger entry).
    pub contains_live: bool,
    /// The device's live lease, if any.
    pub lease: Option<Holder<'a>>,
    pub approval: Option<&'a Approval>,
    pub now_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallDecision {
    /// Go ahead; `take_lease`: nobody holds the device, take it for this install.
    Proceed { take_lease: bool },
    Refuse(Vec<String>),
}

pub fn decide_install(f: &InstallFacts) -> InstallDecision {
    let mut why = Vec::new();
    let mine = f.lease.as_ref().is_some_and(|l| l.holder == f.holder);
    if let Some(l) = f.lease.as_ref().filter(|_| !mine) {
        why.push(format!("the device is leased to {} until {} ({})", l.holder, l.expires_at, l.purpose));
    }
    let sha8 = &f.commit[..f.commit.len().min(8)];
    match f.kind {
        InstallKind::Main => {
            if f.commit != f.tip {
                why.push(format!(
                    "{sha8} is not origin/main's tip ({}): merge it first, or install a test build with the owner's approval",
                    &f.tip[..f.tip.len().min(8)]
                ));
            } else if !f.contains_live {
                why.push(format!("{sha8} does not contain the live build"));
            }
        }
        InstallKind::Test | InstallKind::TestRun => {
            if !mine {
                why.push(format!("a {} needs your own lease: lagent device lease <device> --purpose \"…\"", f.kind.noun()));
            }
            let needs_approval = f.kind == InstallKind::Test || f.commit != f.tip;
            let names: Vec<&str> = std::iter::once(f.holder).chain(f.workspace_id).collect();
            match (needs_approval, f.approval) {
                (true, None) => why.push(format!(
                    "a {} of {sha8} needs --approval <decision id>: the owner's go from Lucina (lucina-rs decide --impact high), naming {sha8} or {}",
                    f.kind.noun(),
                    names.join(" or ")
                )),
                (_, Some(a)) => {
                    if let Some(p) = approval_problem(a, f.commit, &names, f.now_ms) {
                        why.push(p);
                    }
                }
                (false, None) => {}
            }
        }
    }
    if why.is_empty() {
        InstallDecision::Proceed { take_lease: f.lease.is_none() }
    } else {
        InstallDecision::Refuse(why)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantState {
    Issued,
    Done,
    Failed,
}

/// The Mac executor's permission for one build of one commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    pub id: String,
    pub device: String,
    pub holder: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub kind: InstallKind,
    pub commit: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub state: GrantState,
    #[serde(default)]
    pub approval: Option<String>,
    /// Nothing goes on the device: the executor checks, and builds only if asked.
    #[serde(default)]
    pub dry_run: bool,
}

/// Whether the executor may still use the grant: issued, not expired, and its holder
/// still holds the device (a dry run needs no lease).
pub fn grant_problem(g: &Grant, lease: Option<&Holder>, now_ms: u64) -> Option<String> {
    if g.state != GrantState::Issued {
        return Some(format!("grant {} is already {}", g.id, if g.state == GrantState::Done { "used" } else { "failed" }));
    }
    if now_ms >= g.expires_at_ms {
        return Some(format!("grant {} expired", g.id));
    }
    match lease {
        Some(l) if l.holder == g.holder => None,
        Some(l) => Some(format!("the device is now leased to {} until {}", l.holder, l.expires_at)),
        None if g.dry_run => None,
        None => Some(format!("{} no longer holds the device", g.holder)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "local_aaaaaaaa";
    const THEM: &str = "local_bbbbbbbb";
    const WS: &str = "wks_0123456789abcdef";
    const TIP: &str = "3ab79969d49c198d227b55242a5cc218843232a9";
    const BRANCH: &str = "fd3a9fec00000000000000000000000000000000";

    fn lease(holder: &str) -> Holder<'_> {
        Holder { holder, purpose: "owner test", expires_at: "2026-11-01T13:00:00Z" }
    }

    fn approval(by: &str, at: u64, q: &str) -> Approval {
        Approval { id: "dc-0123456789ab".into(), status: "decided".into(), decision: "go".into(), decided_by: by.into(), decided_at_ms: at, question: q.into() }
    }

    fn facts<'a>(kind: InstallKind, commit: &'a str, lease: Option<Holder<'a>>, approval: Option<&'a Approval>) -> InstallFacts<'a> {
        InstallFacts { kind, holder: ME, workspace_id: Some(WS), commit, tip: TIP, contains_live: true, lease, approval, now_ms: 1_000 }
    }

    #[test]
    fn main_installs_only_the_tip_that_contains_the_live_build() {
        assert_eq!(decide_install(&facts(InstallKind::Main, TIP, None, None)), InstallDecision::Proceed { take_lease: true });
        assert_eq!(decide_install(&facts(InstallKind::Main, TIP, Some(lease(ME)), None)), InstallDecision::Proceed { take_lease: false });
        assert!(matches!(decide_install(&facts(InstallKind::Main, BRANCH, None, None)), InstallDecision::Refuse(w) if w[0].contains("not origin/main's tip")));
        let mut f = facts(InstallKind::Main, TIP, None, None);
        f.contains_live = false;
        assert!(matches!(decide_install(&f), InstallDecision::Refuse(w) if w[0].contains("live build")));
        assert!(matches!(decide_install(&facts(InstallKind::Main, TIP, Some(lease(THEM)), None)), InstallDecision::Refuse(w) if w[0].contains(THEM)));
    }

    #[test]
    fn test_builds_need_the_owners_approval_and_a_lease() {
        let ok = approval("owner", 900, "Install test build fd3a9fec of ig-gallery-paging?");
        assert_eq!(decide_install(&facts(InstallKind::Test, BRANCH, Some(lease(ME)), Some(&ok))), InstallDecision::Proceed { take_lease: false });
        for q in [format!("Let {ME} install its build?"), format!("Install the build of workspace {WS}?")] {
            let a = approval("owner", 900, &q);
            assert_eq!(decide_install(&facts(InstallKind::Test, BRANCH, Some(lease(ME)), Some(&a))), InstallDecision::Proceed { take_lease: false }, "{q}");
        }
        // No lease and no approval: both reasons.
        assert!(matches!(decide_install(&facts(InstallKind::Test, BRANCH, None, None)), InstallDecision::Refuse(w) if w.len() == 2));
        for (a, expect) in [
            (approval("lucina", 900, "fd3a9fec"), "decided by lucina"),
            (approval("owner", 900, "some other build"), "does not name"),
            (Approval { decision: "hold".into(), ..ok.clone() }, "not go"),
            (Approval { status: "pending".into(), ..ok.clone() }, "pending"),
        ] {
            assert!(matches!(decide_install(&facts(InstallKind::Test, BRANCH, Some(lease(ME)), Some(&a))), InstallDecision::Refuse(w) if w[0].contains(expect)), "{expect}");
        }
        let mut f = facts(InstallKind::Test, BRANCH, Some(lease(ME)), Some(&ok));
        f.now_ms = 900 + APPROVAL_MAX_AGE_MS + 1;
        assert!(matches!(decide_install(&f), InstallDecision::Refuse(w) if w[0].contains("h old")));
    }

    #[test]
    fn test_runs_of_the_tip_need_only_the_lease() {
        assert_eq!(decide_install(&facts(InstallKind::TestRun, TIP, Some(lease(ME)), None)), InstallDecision::Proceed { take_lease: false });
        assert!(matches!(decide_install(&facts(InstallKind::TestRun, TIP, None, None)), InstallDecision::Refuse(w) if w[0].contains("own lease")));
        assert!(matches!(decide_install(&facts(InstallKind::TestRun, BRANCH, Some(lease(ME)), None)), InstallDecision::Refuse(w) if w[0].contains("--approval")));
    }

    #[test]
    fn grants() {
        let g = Grant {
            id: "gr_1".into(),
            device: "iphone".into(),
            holder: ME.into(),
            workspace_id: None,
            kind: InstallKind::Main,
            commit: TIP.into(),
            created_at_ms: 0,
            expires_at_ms: 1_000,
            state: GrantState::Issued,
            approval: None,
            dry_run: false,
        };
        assert_eq!(grant_problem(&g, Some(&lease(ME)), 10), None);
        assert!(grant_problem(&g, Some(&lease(THEM)), 10).unwrap().contains(THEM));
        assert!(grant_problem(&g, None, 10).unwrap().contains("no longer holds"));
        assert!(grant_problem(&g, Some(&lease(ME)), 1_000).unwrap().contains("expired"));
        assert!(grant_problem(&Grant { state: GrantState::Done, ..g.clone() }, Some(&lease(ME)), 10).unwrap().contains("used"));
        assert_eq!(grant_problem(&Grant { dry_run: true, ..g }, None, 10), None);
    }
}
