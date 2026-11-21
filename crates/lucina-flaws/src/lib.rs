//! Lucina's flaw tracker (design: `docs/design/flaws.md`): the small flaws the owner notices,
//! in his exact words, with one row per client saying where each flaw stands.
//!
//! The rule the whole crate exists for: **a target closes only with evidence**. `not_affected`
//! and `verified` are refused without a harness run on an installed build, a debug event or a
//! screenshot ([`check_transition`]). Reading code can open a target, never close one.
//!
//! - [`store`]: SQLite (`flaws`, `flaw_targets`, append-only `flaw_events`).
//! - [`policy`]: who may start a fix, how many run at once, and the brief an agent gets.

use serde::{Deserialize, Serialize};

pub mod policy;
pub mod store;

pub use policy::{Pick, Policy, Step, Verdict};
pub use store::{
    backoff_minutes, BlockedLook, ClaimCaps, ClaimRef, Detail, Event, Failure, Flaw, FlawPatch,
    Flaws, LaneBreaker, LaneEvent, LaneState, ListQuery, NewFlaw, Outcome, Reported, Target,
    TargetUpdate, FAILURE_WINDOW_HOURS, LANE_FAILURES_BEFORE_LEAD,
};

#[derive(Debug, thiserror::Error)]
pub enum FlawError {
    #[error("no such flaw or target")]
    NotFound,
    /// A target was closed without what closes it.
    #[error("{0}")]
    EvidenceRequired(String),
    #[error("a target cannot go from {from} to {to}")]
    BadTransition {
        from: &'static str,
        to: &'static str,
    },
    #[error("{0}")]
    Invalid(String),
    /// Someone else holds the target, or it is not ready to start.
    #[error("{0}")]
    NotClaimable(String),
    /// A limit stopped the claim (the caps, a lane that failed, a back-off): nothing was started
    /// and the target is as it was.
    #[error("{0}")]
    Busy(String),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

impl FlawError {
    /// The API's error code.
    pub fn code(&self) -> &'static str {
        match self {
            FlawError::NotFound => "flaw_not_found",
            FlawError::EvidenceRequired(_) => "evidence_required",
            FlawError::BadTransition { .. } => "bad_transition",
            FlawError::Invalid(_) => "bad_request",
            FlawError::NotClaimable(_) => "not_claimable",
            FlawError::Busy(_) => "busy",
            FlawError::Db(_) => "internal_error",
        }
    }
}

pub type Result<T> = std::result::Result<T, FlawError>;

macro_rules! text_enum {
    ($(#[$m:meta])* $name:ident, $what:literal { $($(#[$vm:meta])* $v:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$vm])* $v),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$v),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$v => $s),+ }
            }

            pub fn parse(s: &str) -> Result<Self> {
                match s.trim().to_ascii_lowercase().as_str() {
                    $($s => Ok($name::$v),)+
                    other => Err(FlawError::Invalid(format!(
                        concat!($what, " must be one of {} (got {:?})"),
                        Self::ALL.iter().map(|x| x.as_str()).collect::<Vec<_>>().join(", "),
                        other,
                    ))),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

text_enum!(
    /// The clients tracked now. Minerva and Diana come later: add them here and every new flaw
    /// gets their row.
    Platform, "platform" {
        Web => "web",
        Mac => "mac",
        Android => "android",
        Ios => "ios",
        Juno => "juno",
    }
);

text_enum!(
    /// Where the fix belongs.
    Layer, "layer" {
        Server => "server",
        Core => "core",
        Client => "client",
    }
);

text_enum!(
    /// Worst last, so sorting descending starts with the worst.
    Severity, "severity" {
        Low => "low",
        Medium => "medium",
        High => "high",
        Critical => "critical",
    }
);

text_enum!(
    /// A flaw as a whole. `open`, `fixing` and `resolved` follow from its targets
    /// ([`store`]); only `duplicate` and `dismissed` are set by hand.
    FlawState, "state" {
        Open => "open",
        Fixing => "fixing",
        Resolved => "resolved",
        Duplicate => "duplicate",
        Dismissed => "dismissed",
    }
);

text_enum!(
    /// One client's state for one flaw.
    TargetStatus, "status" {
        /// Nobody has looked at this client yet.
        Untriaged => "untriaged",
        /// Believed broken here (the owner saw it, or the code shows it).
        Affected => "affected",
        /// A task or a session holds it.
        Fixing => "fixing",
        /// The fix is committed; nothing has shown it works yet.
        Fixed => "fixed",
        /// Closed: the fix was seen working on an installed build.
        Verified => "verified",
        /// Closed: it was tried here and does not happen.
        NotAffected => "not_affected",
        /// Agents could not fix it; a person needs to look.
        Blocked => "blocked",
    }
);

text_enum!(
    /// What a status rests on. Only the first three can close a target.
    EvidenceKind, "evidence kind" {
        /// A harness run against an installed build (`build` is required).
        HarnessRun => "harness_run",
        /// A debug or telemetry event from the client.
        DebugEvent => "debug_event",
        /// A screenshot of the client.
        Screenshot => "screenshot",
        /// Reading the code: opens a target, never closes one.
        CodeReading => "code_reading",
        /// The owner saw it himself: opens a target, never closes one.
        OwnerReport => "owner_report",
    }
);

text_enum!(
    /// Why a fix needs the owner's go first. Everything else starts without asking.
    Gate, "gate" {
        /// The fix can't be undone.
        Irreversible => "irreversible",
        /// It needs his credentials.
        Credentials => "credentials",
        /// It costs money.
        Payment => "payment",
    }
);

impl EvidenceKind {
    pub fn closes(self) -> bool {
        matches!(
            self,
            EvidenceKind::HarnessRun | EvidenceKind::DebugEvent | EvidenceKind::Screenshot
        )
    }
}

impl TargetStatus {
    /// Closed, with evidence.
    pub fn closed(self) -> bool {
        matches!(self, TargetStatus::Verified | TargetStatus::NotAffected)
    }
}

/// What a status rests on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub kind: EvidenceKind,
    /// Where to see it: the run's log, the event id, the screenshot's path.
    pub reference: String,
    /// The installed build it was seen on (required for a harness run).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Whether `ev` may close a target. The only place that decides it.
pub fn check_closing_evidence(to: TargetStatus, ev: Option<&Evidence>) -> Result<()> {
    let refuse = |why: String| {
        Err(FlawError::EvidenceRequired(format!(
            "{to} needs evidence: {why}"
        )))
    };
    let Some(ev) = ev else {
        return refuse("a harness run on an installed build, a debug event or a screenshot".into());
    };
    if !ev.kind.closes() {
        return refuse(format!(
            "{} can open a target but never close one; show it with a harness run on an installed build, a debug event or a screenshot",
            ev.kind
        ));
    }
    if ev.reference.trim().is_empty() {
        return refuse(format!(
            "a {} with nothing to look at (reference is empty)",
            ev.kind
        ));
    }
    if ev.kind == EvidenceKind::HarnessRun
        && ev.build.as_deref().is_none_or(|b| b.trim().is_empty())
    {
        return refuse("a harness run must name the installed build it ran on (build)".into());
    }
    Ok(())
}

/// Whether a target may go from `from` to `to` with this evidence.
pub fn check_transition(
    from: TargetStatus,
    to: TargetStatus,
    ev: Option<&Evidence>,
    has_commit: bool,
) -> Result<()> {
    use TargetStatus::*;
    let allowed = match from {
        Untriaged => matches!(to, Affected | NotAffected),
        Affected => matches!(to, Fixing | Fixed | NotAffected | Blocked),
        Fixing => matches!(to, Affected | Fixed | NotAffected | Blocked),
        Fixed => matches!(to, Verified | Affected),
        Verified | NotAffected | Blocked => matches!(to, Affected),
    };
    if !allowed {
        return Err(FlawError::BadTransition {
            from: from.as_str(),
            to: to.as_str(),
        });
    }
    if to.closed() {
        check_closing_evidence(to, ev)?;
    }
    if to == Fixed && !has_commit {
        return Err(FlawError::Invalid(
            "fixed needs the commit that fixes it".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use TargetStatus::*;

    fn ev(kind: EvidenceKind, reference: &str, build: Option<&str>) -> Evidence {
        Evidence {
            kind,
            reference: reference.into(),
            build: build.map(str::to_owned),
            note: None,
        }
    }

    #[test]
    fn closing_without_evidence_is_refused() {
        for (from, to) in [
            (Untriaged, NotAffected),
            (Affected, NotAffected),
            (Fixed, Verified),
        ] {
            let e = check_transition(from, to, None, true).unwrap_err();
            assert_eq!(e.code(), "evidence_required", "{from} -> {to}");
        }
    }

    #[test]
    fn reading_code_opens_but_never_closes() {
        let code = ev(EvidenceKind::CodeReading, "rich.rs:12 strips nothing", None);
        check_transition(Untriaged, Affected, Some(&code), false).unwrap();
        for (from, to) in [(Untriaged, NotAffected), (Fixed, Verified)] {
            assert_eq!(
                check_transition(from, to, Some(&code), true)
                    .unwrap_err()
                    .code(),
                "evidence_required"
            );
        }
        // Nor does the owner's word close it: he reported it, nobody showed the fix.
        let said = ev(EvidenceKind::OwnerReport, "chat", None);
        assert_eq!(
            check_transition(Fixed, Verified, Some(&said), true)
                .unwrap_err()
                .code(),
            "evidence_required"
        );
    }

    #[test]
    fn what_closes_a_target() {
        for good in [
            ev(
                EvidenceKind::HarnessRun,
                "logs/run-7.txt",
                Some("web deploy/web/20261102T0100Z-abcd1234"),
            ),
            ev(EvidenceKind::DebugEvent, "evt_123", None),
            ev(EvidenceKind::Screenshot, "shots/after.png", None),
        ] {
            check_transition(Fixed, Verified, Some(&good), true).unwrap();
            check_transition(Untriaged, NotAffected, Some(&good), false).unwrap();
        }
    }

    #[test]
    fn hollow_evidence_is_refused() {
        // A harness run must name the installed build.
        for build in [None, Some("  ")] {
            let e = ev(EvidenceKind::HarnessRun, "logs/run-7.txt", build);
            assert_eq!(
                check_transition(Fixed, Verified, Some(&e), true)
                    .unwrap_err()
                    .code(),
                "evidence_required"
            );
        }
        let e = ev(EvidenceKind::Screenshot, "  ", None);
        assert_eq!(
            check_transition(Fixed, Verified, Some(&e), true)
                .unwrap_err()
                .code(),
            "evidence_required"
        );
    }

    #[test]
    fn verified_only_after_a_fix_and_a_fix_needs_its_commit() {
        let shot = ev(EvidenceKind::Screenshot, "a.png", None);
        for from in [Untriaged, Affected, Fixing, Blocked, NotAffected] {
            assert_eq!(
                check_transition(from, Verified, Some(&shot), true)
                    .unwrap_err()
                    .code(),
                "bad_transition",
                "{from}"
            );
        }
        assert_eq!(
            check_transition(Fixing, Fixed, None, false)
                .unwrap_err()
                .code(),
            "bad_request"
        );
        check_transition(Fixing, Fixed, None, true).unwrap();
    }

    #[test]
    fn a_closed_target_reopens() {
        check_transition(Verified, Affected, None, true).unwrap();
        check_transition(NotAffected, Affected, None, false).unwrap();
    }
}
