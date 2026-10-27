//! Who may answer a permission request, and until when (design: Permissions;
//! proven in `verification/verus/permission_gate.rs`).
//!
//! One gate per request, for agent requests (a provider asks before a tool runs) and
//! for external ones (the Infisical broker asks the owner before it hands out a
//! secret). The daemon runs every answer and every expiry through [`answer`] and
//! [`expire`] and carries out `resolve` when it is set.
//!
//! - Only the owner answers: a signed-in owner device, or `lagent` on the host
//!   from a process that is not an agent. An MCP token, an agent process or an
//!   unknown caller never changes a gate.
//! - A tool policy (`preapproved` MCP tools) may allow an agent request, never an
//!   external one, and never deny for the owner.
//! - A request is answered once, and only before it expires.
//! - A request that is still pending when it expires is denied.

use serde::{Deserialize, Serialize};

/// Who is answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// One of the owner's signed-in devices (through secretary-server).
    OwnerDevice,
    /// `lagent` on the daemon's host, from a process no agent started.
    Local,
    /// The agent's tool policy (`toolPolicy.preapproved`).
    Policy,
    /// An MCP token (Hermes, a Lucina MCP client).
    Mcp,
    /// A process an agent started (its provider or anything below it).
    Agent,
    /// Anything else (loopback TCP, a caller that can't be identified).
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Agent,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Allowed,
    Denied,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gate {
    pub kind: RequestKind,
    pub status: Status,
    /// Milliseconds since the Unix epoch.
    pub expires_at_ms: u64,
}

impl Gate {
    pub fn new(kind: RequestKind, expires_at_ms: u64) -> Self {
        Gate { kind, status: Status::Pending, expires_at_ms }
    }
}

/// What an answer did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The answer counts.
    Applied,
    /// This origin may not answer this request (nothing changed).
    NotAllowed,
    /// Answered before (nothing changed).
    AlreadyAnswered,
    /// The request expired; if it was still pending it is denied now.
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub next: Gate,
    pub outcome: Outcome,
    /// Send this to whoever waits on the request (the provider or the external
    /// caller). Set exactly when the request leaves `pending`.
    pub resolve: Option<Verdict>,
}

/// Whether `origin` may give `verdict` on a request of `kind`.
pub fn may_answer(origin: Origin, kind: RequestKind, verdict: Verdict) -> bool {
    match origin {
        Origin::OwnerDevice | Origin::Local => true,
        Origin::Policy => kind == RequestKind::Agent && verdict == Verdict::Allow,
        Origin::Mcp | Origin::Agent | Origin::Unknown => false,
    }
}

pub fn answer(g: &Gate, origin: Origin, verdict: Verdict, now_ms: u64) -> Step {
    let unchanged = |outcome| Step { next: *g, outcome, resolve: None };
    match g.status {
        Status::Expired => return unchanged(Outcome::Expired),
        Status::Allowed | Status::Denied => return unchanged(Outcome::AlreadyAnswered),
        Status::Pending => {}
    }
    if !may_answer(origin, g.kind, verdict) {
        return unchanged(Outcome::NotAllowed);
    }
    if now_ms >= g.expires_at_ms {
        return Step { next: Gate { status: Status::Expired, ..*g }, outcome: Outcome::Expired, resolve: Some(Verdict::Deny) };
    }
    let status = match verdict {
        Verdict::Allow => Status::Allowed,
        Verdict::Deny => Status::Denied,
    };
    Step { next: Gate { status, ..*g }, outcome: Outcome::Applied, resolve: Some(verdict) }
}

/// The expiry timer: a pending request past its time is denied.
pub fn expire(g: &Gate, now_ms: u64) -> Step {
    if g.status == Status::Pending && now_ms >= g.expires_at_ms {
        Step { next: Gate { status: Status::Expired, ..*g }, outcome: Outcome::Expired, resolve: Some(Verdict::Deny) }
    } else {
        Step { next: *g, outcome: Outcome::AlreadyAnswered, resolve: None }
    }
}

/// One `toolPolicy.preapproved` grant (Paseo `{kind: "mcp", server, tool}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub kind: String,
    pub server: String,
    pub tool: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolPolicy {
    #[serde(default)]
    pub preapproved: Vec<Grant>,
}

impl ToolPolicy {
    /// Whether a tool request named `name` is an exact preapproved MCP tool. Names
    /// are `mcp__<server>__<tool>` (Claude, and what agentd's adapters report for
    /// the others). No wildcards: an empty server or tool never matches.
    pub fn preapproves(&self, name: &str) -> bool {
        let Some(rest) = name.strip_prefix("mcp__") else { return false };
        let Some((server, tool)) = rest.split_once("__") else { return false };
        !server.is_empty()
            && !tool.is_empty()
            && self.preapproved.iter().any(|g| g.kind == "mcp" && g.server == server && g.tool == tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_owner_answers_once_before_expiry() {
        let g = Gate::new(RequestKind::Agent, 1_000);
        let s = answer(&g, Origin::Mcp, Verdict::Allow, 10);
        assert_eq!((s.outcome, s.resolve, s.next), (Outcome::NotAllowed, None, g));
        let s = answer(&g, Origin::Local, Verdict::Allow, 10);
        assert_eq!((s.outcome, s.resolve, s.next.status), (Outcome::Applied, Some(Verdict::Allow), Status::Allowed));
        let again = answer(&s.next, Origin::OwnerDevice, Verdict::Deny, 11);
        assert_eq!((again.outcome, again.resolve), (Outcome::AlreadyAnswered, None));
        let late = answer(&g, Origin::OwnerDevice, Verdict::Allow, 1_000);
        assert_eq!((late.outcome, late.resolve, late.next.status), (Outcome::Expired, Some(Verdict::Deny), Status::Expired));
    }

    #[test]
    fn policy_only_allows_agent_requests() {
        let agent = Gate::new(RequestKind::Agent, 1_000);
        let ext = Gate::new(RequestKind::External, 1_000);
        assert_eq!(answer(&agent, Origin::Policy, Verdict::Allow, 0).outcome, Outcome::Applied);
        assert_eq!(answer(&agent, Origin::Policy, Verdict::Deny, 0).outcome, Outcome::NotAllowed);
        assert_eq!(answer(&ext, Origin::Policy, Verdict::Allow, 0).outcome, Outcome::NotAllowed);
    }

    #[test]
    fn expiry_denies_pending_only() {
        let g = Gate::new(RequestKind::External, 100);
        assert_eq!(expire(&g, 99).resolve, None);
        let e = expire(&g, 100);
        assert_eq!((e.resolve, e.next.status), (Some(Verdict::Deny), Status::Expired));
        assert_eq!(expire(&e.next, 200).resolve, None);
    }

    #[test]
    fn preapproval_is_exact() {
        let p = ToolPolicy { preapproved: vec![Grant { kind: "mcp".into(), server: "lucina".into(), tool: "notes_search".into() }] };
        assert!(p.preapproves("mcp__lucina__notes_search"));
        assert!(!p.preapproves("mcp__lucina__note_save"));
        assert!(!p.preapproves("mcp__lucina__"));
        assert!(!p.preapproves("Bash"));
        assert!(!ToolPolicy::default().preapproves("mcp__lucina__notes_search"));
    }
}
