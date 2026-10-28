// Permission gate kernel (Lucina Agents: who may answer a permission request, and until
// when; agent requests from providers and external requests from the Infisical broker).
//
// Production (branch agents-p3, crates/lucina-agents/src/permission_gate.rs):
//   Origin, RequestKind, Status, Verdict, Gate, Outcome  <-> the same types
//   Step { next, outcome, resolve }                       <-> permission_gate::Step
//   may_answer / answer / expire                          <-> the same functions
// Used by lucina-agentd's manager for every permission answer and expiry
// (crates/lucina-agentd/src/permissions.rs).
// Differential test: crates/lucina-agents/tests/verus_permission_gate.rs.
//
// Proven (postconditions, for every gate, origin, verdict and time):
//   G1 owner_only: an answer that resolves a request before it expires comes from an
//      owner device or the local CLI, or from the tool policy allowing an agent request.
//   G2 untrusted_never_changes: an answer from an MCP token, an agent process or an
//      unknown caller leaves the gate exactly as it was and resolves nothing.
//   G3 once: a request that is no longer pending is never changed or resolved again.
//   G4 before_expiry: an answer is applied only while now < expires_at.
//   G5 expiry_denies: a request still pending at or after its expiry is denied (by the
//      timer, or by the first allowed answer that arrives late), and becomes Expired.
//   G6 resolve_iff_leaves_pending: `resolve` is set exactly when the request goes from
//      Pending to another status; kind and expiry never change.
//   G7 policy_never_denies: the tool policy never denies and never answers an external
//      request.
use vstd::prelude::*;

verus! {

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Origin {
    OwnerDevice,
    Local,
    Policy,
    Mcp,
    Agent,
    Unknown,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum RequestKind {
    Agent,
    External,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Status {
    Pending,
    Allowed,
    Denied,
    Expired,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Verdict {
    Allow,
    Deny,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub struct Gate {
    pub kind: RequestKind,
    pub status: Status,
    pub expires_at_ms: u64,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Outcome {
    Applied,
    NotAllowed,
    AlreadyAnswered,
    Expired,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Resolve {
    None,
    Some(Verdict),
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub struct Step {
    pub next: Gate,
    pub outcome: Outcome,
    pub resolve: Resolve,
}

pub open spec fn trusted(o: Origin) -> bool {
    o == Origin::OwnerDevice || o == Origin::Local
}

pub open spec fn untrusted(o: Origin) -> bool {
    o == Origin::Mcp || o == Origin::Agent || o == Origin::Unknown
}

pub open spec fn may(o: Origin, k: RequestKind, v: Verdict) -> bool {
    trusted(o) || (o == Origin::Policy && k == RequestKind::Agent && v == Verdict::Allow)
}

pub open spec fn same_identity(a: Gate, b: Gate) -> bool {
    a.kind == b.kind && a.expires_at_ms == b.expires_at_ms
}

pub fn may_answer(origin: Origin, kind: RequestKind, verdict: Verdict) -> (b: bool)
    ensures
        b == may(origin, kind, verdict),
{
    match origin {
        Origin::OwnerDevice => true,
        Origin::Local => true,
        Origin::Policy => match (kind, verdict) {
            (RequestKind::Agent, Verdict::Allow) => true,
            _ => false,
        },
        Origin::Mcp => false,
        Origin::Agent => false,
        Origin::Unknown => false,
    }
}

fn is_pending(s: Status) -> (b: bool)
    ensures
        b == (s == Status::Pending),
{
    match s {
        Status::Pending => true,
        _ => false,
    }
}

pub fn answer(g: Gate, origin: Origin, verdict: Verdict, now_ms: u64) -> (s: Step)
    ensures
        // G1
        s.outcome == Outcome::Applied ==> may(origin, g.kind, verdict),
        s.resolve != Resolve::None && s.outcome != Outcome::Expired ==> may(origin, g.kind, verdict),
        // G2
        untrusted(origin) ==> s.next == g && s.resolve == Resolve::None,
        // G3
        g.status != Status::Pending ==> s.next == g && s.resolve == Resolve::None
            && s.outcome != Outcome::Applied,
        // G4
        s.outcome == Outcome::Applied ==> now_ms < g.expires_at_ms,
        // G5 (late answer)
        g.status == Status::Pending && may(origin, g.kind, verdict) && now_ms >= g.expires_at_ms
            ==> s.next.status == Status::Expired && s.resolve == Resolve::Some(Verdict::Deny),
        // G6
        (s.resolve != Resolve::None) == (g.status == Status::Pending && s.next.status
            != Status::Pending),
        same_identity(s.next, g),
        // the applied answer is the one given
        s.outcome == Outcome::Applied ==> s.resolve == Resolve::Some(verdict) && (verdict
            == Verdict::Allow ==> s.next.status == Status::Allowed) && (verdict == Verdict::Deny
            ==> s.next.status == Status::Denied),
        // G7
        origin == Origin::Policy && s.resolve != Resolve::None && s.outcome != Outcome::Expired
            ==> s.resolve == Resolve::Some(Verdict::Allow) && g.kind == RequestKind::Agent,
{
    match g.status {
        Status::Expired => {
            return Step { next: g, outcome: Outcome::Expired, resolve: Resolve::None };
        },
        Status::Allowed => {
            return Step { next: g, outcome: Outcome::AlreadyAnswered, resolve: Resolve::None };
        },
        Status::Denied => {
            return Step { next: g, outcome: Outcome::AlreadyAnswered, resolve: Resolve::None };
        },
        Status::Pending => {},
    }
    if !may_answer(origin, g.kind, verdict) {
        return Step { next: g, outcome: Outcome::NotAllowed, resolve: Resolve::None };
    }
    if now_ms >= g.expires_at_ms {
        return Step {
            next: Gate { kind: g.kind, status: Status::Expired, expires_at_ms: g.expires_at_ms },
            outcome: Outcome::Expired,
            resolve: Resolve::Some(Verdict::Deny),
        };
    }
    let status = match verdict {
        Verdict::Allow => Status::Allowed,
        Verdict::Deny => Status::Denied,
    };
    Step {
        next: Gate { kind: g.kind, status, expires_at_ms: g.expires_at_ms },
        outcome: Outcome::Applied,
        resolve: Resolve::Some(verdict),
    }
}

pub fn expire(g: Gate, now_ms: u64) -> (s: Step)
    ensures
        // G5 (timer)
        g.status == Status::Pending && now_ms >= g.expires_at_ms ==> s.next.status
            == Status::Expired && s.resolve == Resolve::Some(Verdict::Deny),
        !(g.status == Status::Pending && now_ms >= g.expires_at_ms) ==> s.next == g && s.resolve
            == Resolve::None,
        // G6
        (s.resolve != Resolve::None) == (g.status == Status::Pending && s.next.status
            != Status::Pending),
        same_identity(s.next, g),
        s.outcome != Outcome::Applied,
{
    if is_pending(g.status) && now_ms >= g.expires_at_ms {
        Step {
            next: Gate { kind: g.kind, status: Status::Expired, expires_at_ms: g.expires_at_ms },
            outcome: Outcome::Expired,
            resolve: Resolve::Some(Verdict::Deny),
        }
    } else {
        Step { next: g, outcome: Outcome::AlreadyAnswered, resolve: Resolve::None }
    }
}

} // verus!

fn main() {}
