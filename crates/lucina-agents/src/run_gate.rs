//! The finished-run gate's verdict (design: Finished-run gate; proven in
//! `verification/verus/run_gate.rs`).
//!
//! Three checks run after a turn that changed the workspace: diffgate (stubs, placeholder
//! markers, skipped or removed tests, deleted checks), the proof command (run by
//! agentd, never taken from the agent's report) and an advisory judge. A change to
//! the workspace's own gate config (`.lucina/agents.toml`) by the run counts as
//! tampering.
//!
//! - `verified` only when diffgate and the proof passed, the judge did not flag the
//!   run and the run did not touch the gate config;
//! - `refused` exactly when diffgate or the proof failed;
//! - `suspicious` when nothing failed but the judge flagged the run or it tampered;
//! - `unverified` otherwise (no proof command, or a check could not run).

use serde::{Deserialize, Serialize};

use crate::types::GateVerdict;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    Pass,
    Fail,
    /// Not run (no proof command, not a git workspace, the tool is missing).
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Judge {
    Clean,
    Flagged,
    /// Off, or the judge model could not answer.
    Skipped,
}

pub fn verdict(diffgate: Check, proof: Check, judge: Judge, tampered: bool) -> GateVerdict {
    if diffgate == Check::Fail || proof == Check::Fail {
        GateVerdict::Refused
    } else if judge == Judge::Flagged || tampered {
        GateVerdict::Suspicious
    } else if diffgate == Check::Pass && proof == Check::Pass {
        GateVerdict::Verified
    } else {
        GateVerdict::Unverified
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts() {
        use Check::*;
        assert_eq!(verdict(Pass, Pass, Judge::Clean, false), GateVerdict::Verified);
        assert_eq!(verdict(Pass, Pass, Judge::Skipped, false), GateVerdict::Verified);
        assert_eq!(verdict(Pass, Pass, Judge::Flagged, false), GateVerdict::Suspicious);
        assert_eq!(verdict(Pass, Pass, Judge::Clean, true), GateVerdict::Suspicious);
        assert_eq!(verdict(Fail, Pass, Judge::Clean, false), GateVerdict::Refused);
        assert_eq!(verdict(Pass, Fail, Judge::Flagged, true), GateVerdict::Refused);
        assert_eq!(verdict(Pass, Skipped, Judge::Clean, false), GateVerdict::Unverified);
        assert_eq!(verdict(Skipped, Pass, Judge::Clean, false), GateVerdict::Unverified);
    }
}
