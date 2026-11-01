//! The agent state machine (Paseo `server/agent/agent-manager.js`: the
//! `onStream*` handlers, `checkAndSetAttention`, `archiveAgent`,
//! `closeStaleAgentRecords`, and the loader that brings every stored agent back
//! as `closed`, with no provider runtime). lucina-agentd sends every state change through
//! [`step`]; the daemon carries out the returned effects. The kernel is proven in
//! `verification/verus/agent_lifecycle.rs` and tied to this code by
//! `tests/verus_lifecycle.rs`.
//!
//! | From | Event | To | Effects |
//! |---|---|---|---|
//! | initializing | `HandshakeDone` | idle | |
//! | initializing | `LoadFailed` | error | `SetLastError`, attention `error` |
//! | closed | `Resume` | initializing | |
//! | idle, error | `TurnStarted` | running | `ClearLastError` |
//! | running | `TurnStarted` | running | (a turn the provider started on its own, or a steer) |
//! | running | `TurnCompleted` | idle | `ClearLastError`, attention `finished` |
//! | running | `TurnFailed` | error | `SetLastError`, `AppendSystemError`, `AutoDenyPending("Turn failed")`, attention `error` |
//! | running | `TurnCanceled` | idle | `ClearLastError`, `AutoDenyPending("Interrupted")`, attention `finished` |
//! | running | `PermissionRequested` | running | pending + 1; `NotifyAttention(permission)` for the first one |
//! | any live, pending > 0 | `PermissionResolved` | same | pending − 1 |
//! | any live | `ClearAttention` | same | attention cleared |
//! | any live but closed | `Close` | closed | `Interrupt` if running, `AutoDenyPending("Closed")` if pending, `CloseRuntime` |
//! | any live | `Archive` | closed, archived | `Interrupt` if running, `AutoDenyPending("Archived")` if pending, `CloseRuntime`, `CascadeArchiveChildren` |
//! | archived | `Unarchive` | closed | |
//! | any not deleted | `Delete` | deleted | `Interrupt` if running, `RemoveRecord` |
//! | any not deleted | `RestartRepair` | closed | `RecordInterrupted` for initializing/running; pending dropped |
//!
//! "Live" means neither archived nor deleted. "Attention X" is Paseo's
//! edge-triggered unread signal: it is set (effect `SetAttention`) only when the
//! agent is not internal and doesn't already require attention. Every other
//! state/event pair is an error, and nothing panics.

use serde::{Deserialize, Serialize};

use crate::types::{AgentStatus, AttentionReason};

/// What the kernel knows about an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentState {
    pub status: AgentStatus,
    pub archived: bool,
    pub deleted: bool,
    pub pending_permissions: u32,
    pub internal: bool,
    pub requires_attention: bool,
    pub attention_reason: Option<AttentionReason>,
}

impl AgentState {
    /// A newly created agent.
    pub fn new(internal: bool) -> Self {
        AgentState {
            status: AgentStatus::Initializing,
            archived: false,
            deleted: false,
            pending_permissions: 0,
            internal,
            requires_attention: false,
            attention_reason: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Event {
    HandshakeDone,
    LoadFailed,
    Resume,
    TurnStarted,
    TurnCompleted,
    TurnFailed,
    TurnCanceled,
    PermissionRequested,
    PermissionResolved,
    ClearAttention,
    Close,
    Archive,
    Unarchive,
    Delete,
    RestartRepair,
}

impl Event {
    pub const ALL: [Event; 15] = [
        Event::HandshakeDone,
        Event::LoadFailed,
        Event::Resume,
        Event::TurnStarted,
        Event::TurnCompleted,
        Event::TurnFailed,
        Event::TurnCanceled,
        Event::PermissionRequested,
        Event::PermissionResolved,
        Event::ClearAttention,
        Event::Close,
        Event::Archive,
        Event::Unarchive,
        Event::Delete,
        Event::RestartRepair,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effect {
    ClearLastError,
    SetLastError,
    /// Paseo `appendSystemErrorTimelineMessage` (`[System Error] …`).
    AppendSystemError,
    /// Deny every pending permission with this message.
    AutoDenyPending(&'static str),
    /// Set the record's attention and notify (Paseo `checkAndSetAttention`).
    SetAttention(AttentionReason),
    /// Notify without changing the record (Paseo `broadcastAgentAttention`).
    NotifyAttention(AttentionReason),
    Interrupt,
    CloseRuntime,
    CascadeArchiveChildren,
    RemoveRecord,
    /// After a restart: a notification item saying the run was interrupted.
    RecordInterrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub next: AgentState,
    pub effects: Vec<Effect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleError {
    #[error("agent is deleted")]
    Deleted,
    #[error("agent is archived")]
    Archived,
    #[error("{event:?} is not allowed while the agent is {status}")]
    NotAllowed { status: &'static str, event: Event },
    #[error("no pending permission to resolve")]
    NoPendingPermission,
}

fn attention(state: &mut AgentState, effects: &mut Vec<Effect>, reason: AttentionReason) {
    if state.internal || state.requires_attention {
        return;
    }
    state.requires_attention = true;
    state.attention_reason = Some(reason);
    effects.push(Effect::SetAttention(reason));
}

fn deny_pending(state: &mut AgentState, effects: &mut Vec<Effect>, message: &'static str) {
    effects.push(Effect::AutoDenyPending(message));
    state.pending_permissions = 0;
}

/// One state change. Total: every state and event gives `Ok` or `Err`.
pub fn step(state: &AgentState, event: Event) -> Result<Transition, LifecycleError> {
    use AgentStatus::*;
    if state.deleted {
        return Err(LifecycleError::Deleted);
    }
    if state.archived && !matches!(event, Event::Unarchive | Event::Delete | Event::RestartRepair) {
        return Err(LifecycleError::Archived);
    }
    let not_allowed = || LifecycleError::NotAllowed { status: state.status.as_str(), event };
    let mut next = *state;
    let mut effects = Vec::new();
    match event {
        Event::HandshakeDone => {
            if state.status != Initializing {
                return Err(not_allowed());
            }
            next.status = Idle;
        }
        Event::LoadFailed => {
            if state.status != Initializing {
                return Err(not_allowed());
            }
            next.status = Error;
            effects.push(Effect::SetLastError);
            attention(&mut next, &mut effects, AttentionReason::Error);
        }
        Event::Resume => {
            if state.status != Closed {
                return Err(not_allowed());
            }
            next.status = Initializing;
        }
        Event::TurnStarted => match state.status {
            Idle | Error => {
                next.status = Running;
                effects.push(Effect::ClearLastError);
            }
            Running => {}
            _ => return Err(not_allowed()),
        },
        Event::TurnCompleted => {
            if state.status != Running {
                return Err(not_allowed());
            }
            next.status = Idle;
            effects.push(Effect::ClearLastError);
            attention(&mut next, &mut effects, AttentionReason::Finished);
        }
        Event::TurnFailed => {
            if state.status != Running {
                return Err(not_allowed());
            }
            next.status = Error;
            effects.push(Effect::SetLastError);
            effects.push(Effect::AppendSystemError);
            deny_pending(&mut next, &mut effects, "Turn failed");
            attention(&mut next, &mut effects, AttentionReason::Error);
        }
        Event::TurnCanceled => {
            if state.status != Running {
                return Err(not_allowed());
            }
            next.status = Idle;
            effects.push(Effect::ClearLastError);
            deny_pending(&mut next, &mut effects, "Interrupted");
            attention(&mut next, &mut effects, AttentionReason::Finished);
        }
        Event::PermissionRequested => {
            if state.status != Running {
                return Err(not_allowed());
            }
            if state.pending_permissions == 0 && !state.internal {
                effects.push(Effect::NotifyAttention(AttentionReason::Permission));
            }
            next.pending_permissions = state.pending_permissions.saturating_add(1);
        }
        Event::PermissionResolved => {
            if state.pending_permissions == 0 {
                return Err(LifecycleError::NoPendingPermission);
            }
            next.pending_permissions = state.pending_permissions - 1;
        }
        Event::ClearAttention => {
            next.requires_attention = false;
            next.attention_reason = None;
        }
        Event::Close => {
            if state.status == Closed {
                return Err(not_allowed());
            }
            if state.status == Running {
                effects.push(Effect::Interrupt);
            }
            if state.pending_permissions > 0 {
                deny_pending(&mut next, &mut effects, "Closed");
            }
            effects.push(Effect::CloseRuntime);
            next.status = Closed;
        }
        Event::Archive => {
            if state.status == Running {
                effects.push(Effect::Interrupt);
            }
            if state.pending_permissions > 0 {
                deny_pending(&mut next, &mut effects, "Archived");
            }
            effects.push(Effect::CloseRuntime);
            effects.push(Effect::CascadeArchiveChildren);
            next.status = Closed;
            next.archived = true;
        }
        Event::Unarchive => {
            if !state.archived {
                return Err(not_allowed());
            }
            next.archived = false;
            next.status = Closed;
        }
        Event::Delete => {
            if state.status == Running {
                effects.push(Effect::Interrupt);
            }
            effects.push(Effect::RemoveRecord);
            next.deleted = true;
            next.status = Closed;
            next.pending_permissions = 0;
        }
        Event::RestartRepair => {
            next.pending_permissions = 0;
            next.status = Closed;
            if matches!(state.status, Initializing | Running) {
                effects.push(Effect::RecordInterrupted);
            }
        }
    }
    Ok(Transition { next, effects })
}

#[cfg(test)]
mod tests {
    use super::*;
    use AgentStatus::*;

    fn st(status: AgentStatus) -> AgentState {
        AgentState { status, ..AgentState::new(false) }
    }

    fn run(mut state: AgentState, events: &[Event]) -> AgentState {
        for &e in events {
            state = step(&state, e).unwrap_or_else(|err| panic!("{e:?}: {err}")).next;
        }
        state
    }

    #[test]
    fn happy_path_sets_finished_attention_once() {
        let s = run(AgentState::new(false), &[Event::HandshakeDone, Event::TurnStarted]);
        assert_eq!(s.status, Running);
        let t = step(&s, Event::TurnCompleted).unwrap();
        assert_eq!(t.next.status, Idle);
        assert_eq!(t.effects, vec![Effect::ClearLastError, Effect::SetAttention(AttentionReason::Finished)]);
        let s = run(t.next, &[Event::TurnStarted]);
        let t = step(&s, Event::TurnCompleted).unwrap();
        assert!(!t.effects.contains(&Effect::SetAttention(AttentionReason::Finished)), "edge triggered");
    }

    #[test]
    fn failure_and_cancel_deny_pending() {
        let s = run(st(Running), &[Event::PermissionRequested, Event::PermissionRequested]);
        assert_eq!(s.pending_permissions, 2);
        let failed = step(&s, Event::TurnFailed).unwrap();
        assert_eq!(failed.next.status, Error);
        assert_eq!(failed.next.pending_permissions, 0);
        assert!(failed.effects.contains(&Effect::AutoDenyPending("Turn failed")));
        let canceled = step(&s, Event::TurnCanceled).unwrap();
        assert!(canceled.effects.contains(&Effect::AutoDenyPending("Interrupted")));
        assert_eq!(canceled.next.status, Idle);
    }

    #[test]
    fn permission_notifies_on_first_only() {
        let t = step(&st(Running), Event::PermissionRequested).unwrap();
        assert_eq!(t.effects, vec![Effect::NotifyAttention(AttentionReason::Permission)]);
        let t2 = step(&t.next, Event::PermissionRequested).unwrap();
        assert!(t2.effects.is_empty());
        assert_eq!(step(&st(Idle), Event::PermissionResolved), Err(LifecycleError::NoPendingPermission));
        let internal = AgentState { internal: true, ..st(Running) };
        assert!(step(&internal, Event::PermissionRequested).unwrap().effects.is_empty());
    }

    #[test]
    fn archive_is_terminal_until_unarchive() {
        let a = step(&st(Running), Event::Archive).unwrap();
        assert_eq!(a.effects[0], Effect::Interrupt);
        assert!(a.effects.contains(&Effect::CascadeArchiveChildren));
        assert!(a.next.archived);
        for e in Event::ALL {
            let r = step(&a.next, e);
            match e {
                Event::Unarchive | Event::Delete | Event::RestartRepair => assert!(r.is_ok(), "{e:?}"),
                _ => assert_eq!(r, Err(LifecycleError::Archived), "{e:?}"),
            }
        }
        let u = step(&a.next, Event::Unarchive).unwrap().next;
        assert_eq!((u.status, u.archived), (Closed, false));
        assert_eq!(run(u, &[Event::Resume, Event::HandshakeDone]).status, Idle);
    }

    #[test]
    fn delete_is_final() {
        let d = step(&st(Idle), Event::Delete).unwrap();
        assert_eq!(d.effects, vec![Effect::RemoveRecord]);
        for e in Event::ALL {
            assert_eq!(step(&d.next, e), Err(LifecycleError::Deleted));
        }
    }

    #[test]
    fn restart_repair() {
        for status in AgentStatus::ALL {
            let t = step(&AgentState { pending_permissions: 3, ..st(status) }, Event::RestartRepair).unwrap();
            assert_eq!(t.next.status, Closed);
            assert_eq!(t.next.pending_permissions, 0);
            assert_eq!(t.effects.contains(&Effect::RecordInterrupted), matches!(status, Running | Initializing));
        }
    }

    #[test]
    fn illegal_pairs_are_errors() {
        assert!(step(&st(Idle), Event::TurnCompleted).is_err());
        assert!(step(&st(Closed), Event::TurnStarted).is_err());
        assert!(step(&st(Idle), Event::HandshakeDone).is_err());
        assert!(step(&st(Closed), Event::Close).is_err());
        assert!(step(&st(Idle), Event::Unarchive).is_err());
        let s = run(AgentState::new(false), &[Event::LoadFailed]);
        assert_eq!((s.status, s.attention_reason), (Error, Some(AttentionReason::Error)));
        assert_eq!(run(s, &[Event::TurnStarted]).status, Running);
    }
}
