//! Differential test: `permission_gate::{answer, expire, may_answer}` against a
//! plain-Rust copy of the Verus kernel `verification/verus/permission_gate.rs` (exec
//! part), on every gate status, kind, origin and verdict, at times around the expiry.
//! The kernel's proven properties (G1-G7) then hold for the production functions too.

use lucina_agents::permission_gate::{self as pg, Gate, Origin, Outcome, RequestKind, Status, Verdict};

/// Plain-Rust copy of the Verus kernel (exec part).
mod kernel {
    use super::*;

    pub fn may_answer(origin: Origin, kind: RequestKind, verdict: Verdict) -> bool {
        match origin {
            Origin::OwnerDevice => true,
            Origin::Local => true,
            Origin::Policy => matches!((kind, verdict), (RequestKind::Agent, Verdict::Allow)),
            Origin::Mcp | Origin::Agent | Origin::Unknown => false,
        }
    }

    pub fn answer(g: Gate, origin: Origin, verdict: Verdict, now_ms: u64) -> (Gate, Outcome, Option<Verdict>) {
        match g.status {
            Status::Expired => return (g, Outcome::Expired, None),
            Status::Allowed | Status::Denied => return (g, Outcome::AlreadyAnswered, None),
            Status::Pending => {}
        }
        if !may_answer(origin, g.kind, verdict) {
            return (g, Outcome::NotAllowed, None);
        }
        if now_ms >= g.expires_at_ms {
            return (Gate { status: Status::Expired, ..g }, Outcome::Expired, Some(Verdict::Deny));
        }
        let status = match verdict {
            Verdict::Allow => Status::Allowed,
            Verdict::Deny => Status::Denied,
        };
        (Gate { status, ..g }, Outcome::Applied, Some(verdict))
    }

    pub fn expire(g: Gate, now_ms: u64) -> (Gate, Outcome, Option<Verdict>) {
        if g.status == Status::Pending && now_ms >= g.expires_at_ms {
            (Gate { status: Status::Expired, ..g }, Outcome::Expired, Some(Verdict::Deny))
        } else {
            (g, Outcome::AlreadyAnswered, None)
        }
    }
}

const ORIGINS: [Origin; 6] = [Origin::OwnerDevice, Origin::Local, Origin::Policy, Origin::Mcp, Origin::Agent, Origin::Unknown];
const STATUSES: [Status; 4] = [Status::Pending, Status::Allowed, Status::Denied, Status::Expired];
const KINDS: [RequestKind; 2] = [RequestKind::Agent, RequestKind::External];
const VERDICTS: [Verdict; 2] = [Verdict::Allow, Verdict::Deny];

#[test]
fn every_gate_origin_verdict_and_time() {
    let mut n = 0;
    for kind in KINDS {
        for status in STATUSES {
            for exp in [0u64, 1, 1_000, u64::MAX] {
                let g = Gate { kind, status, expires_at_ms: exp };
                let times = [0, exp.saturating_sub(1), exp, exp.saturating_add(1), u64::MAX];
                for now in times {
                    let e = pg::expire(&g, now);
                    assert_eq!((e.next, e.outcome, e.resolve), kernel::expire(g, now), "expire {g:?} at {now}");
                    for origin in ORIGINS {
                        for verdict in VERDICTS {
                            assert_eq!(pg::may_answer(origin, kind, verdict), kernel::may_answer(origin, kind, verdict));
                            let s = pg::answer(&g, origin, verdict, now);
                            assert_eq!((s.next, s.outcome, s.resolve), kernel::answer(g, origin, verdict, now), "answer {g:?} {origin:?} {verdict:?} at {now}");
                            n += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(n, 2 * 4 * 4 * 5 * 6 * 2);
}
