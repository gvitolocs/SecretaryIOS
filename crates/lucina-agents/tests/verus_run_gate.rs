//! Differential test: `run_gate::verdict` against a plain-Rust copy of the Verus kernel
//! `verification/verus/run_gate.rs`, on every combination of inputs. The kernel's proven
//! properties (V1-V5) then hold for the production function too.

use lucina_agents::run_gate::{verdict, Check, Judge};
use lucina_agents::types::GateVerdict;

/// Plain-Rust copy of the Verus kernel.
fn kernel(diffgate: Check, proof: Check, judge: Judge, tampered: bool) -> GateVerdict {
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

#[test]
fn every_combination() {
    let checks = [Check::Pass, Check::Fail, Check::Skipped];
    let judges = [Judge::Clean, Judge::Flagged, Judge::Skipped];
    for d in checks {
        for p in checks {
            for j in judges {
                for t in [false, true] {
                    let v = verdict(d, p, j, t);
                    assert_eq!(v, kernel(d, p, j, t), "{d:?} {p:?} {j:?} {t}");
                    assert_ne!(v, GateVerdict::Running, "Running is never a verdict");
                }
            }
        }
    }
}
