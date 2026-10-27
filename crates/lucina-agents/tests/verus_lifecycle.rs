//! Differential test: `lifecycle::step` against a plain-Rust copy of the Verus kernel
//! `verification/verus/agent_lifecycle.rs` (exec part), on every state × event and on
//! random event sequences. The kernel's proven properties (L1-L7) then hold for the
//! production function too.

use lucina_agents::lifecycle::{step, AgentState, Effect, Event, LifecycleError};
use lucina_agents::AgentStatus;

/// Plain-Rust copy of the Verus kernel (exec part).
mod kernel {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Status {
        Initializing,
        Idle,
        Running,
        Error,
        Closed,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Ev {
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

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Refusal {
        Deleted,
        Archived,
        NotAllowed,
        NoPendingPermission,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct State {
        pub status: Status,
        pub archived: bool,
        pub deleted: bool,
        pub pending: u32,
        pub internal: bool,
        pub requires_attention: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Out {
        pub next: State,
        pub deny_all: bool,
        pub set_attention: bool,
        pub notify_permission: bool,
        pub interrupt: bool,
        pub close_runtime: bool,
        pub cascade: bool,
        pub remove: bool,
        pub interrupted_note: bool,
    }

    fn quiet(next: State) -> Out {
        Out {
            next,
            deny_all: false,
            set_attention: false,
            notify_permission: false,
            interrupt: false,
            close_runtime: false,
            cascade: false,
            remove: false,
            interrupted_note: false,
        }
    }

    fn with_attention(o: Out) -> Out {
        if o.next.internal || o.next.requires_attention {
            return o;
        }
        let mut r = o;
        r.next.requires_attention = true;
        r.set_attention = true;
        r
    }

    pub fn step(s: State, ev: Ev) -> Result<Out, Refusal> {
        if s.deleted {
            return Err(Refusal::Deleted);
        }
        if s.archived && !matches!(ev, Ev::Unarchive | Ev::Delete | Ev::RestartRepair) {
            return Err(Refusal::Archived);
        }
        let mut n = s;
        match ev {
            Ev::HandshakeDone => {
                if s.status != Status::Initializing {
                    return Err(Refusal::NotAllowed);
                }
                n.status = Status::Idle;
                Ok(quiet(n))
            }
            Ev::LoadFailed => {
                if s.status != Status::Initializing {
                    return Err(Refusal::NotAllowed);
                }
                n.status = Status::Error;
                Ok(with_attention(quiet(n)))
            }
            Ev::Resume => {
                if s.status != Status::Closed {
                    return Err(Refusal::NotAllowed);
                }
                n.status = Status::Initializing;
                Ok(quiet(n))
            }
            Ev::TurnStarted => match s.status {
                Status::Idle | Status::Error => {
                    n.status = Status::Running;
                    Ok(quiet(n))
                }
                Status::Running => Ok(quiet(n)),
                _ => Err(Refusal::NotAllowed),
            },
            Ev::TurnCompleted => {
                if s.status != Status::Running {
                    return Err(Refusal::NotAllowed);
                }
                n.status = Status::Idle;
                Ok(with_attention(quiet(n)))
            }
            Ev::TurnFailed | Ev::TurnCanceled => {
                if s.status != Status::Running {
                    return Err(Refusal::NotAllowed);
                }
                n.status = if ev == Ev::TurnFailed { Status::Error } else { Status::Idle };
                n.pending = 0;
                let mut o = quiet(n);
                o.deny_all = true;
                Ok(with_attention(o))
            }
            Ev::PermissionRequested => {
                if s.status != Status::Running {
                    return Err(Refusal::NotAllowed);
                }
                n.pending = s.pending.saturating_add(1);
                let mut o = quiet(n);
                o.notify_permission = s.pending == 0 && !s.internal;
                Ok(o)
            }
            Ev::PermissionResolved => {
                if s.pending == 0 {
                    return Err(Refusal::NoPendingPermission);
                }
                n.pending = s.pending - 1;
                Ok(quiet(n))
            }
            Ev::ClearAttention => {
                n.requires_attention = false;
                Ok(quiet(n))
            }
            Ev::Close => {
                if s.status == Status::Closed {
                    return Err(Refusal::NotAllowed);
                }
                n.status = Status::Closed;
                n.pending = 0;
                let mut o = quiet(n);
                o.interrupt = s.status == Status::Running;
                o.deny_all = s.pending > 0;
                o.close_runtime = true;
                Ok(o)
            }
            Ev::Archive => {
                n.status = Status::Closed;
                n.archived = true;
                n.pending = 0;
                let mut o = quiet(n);
                o.interrupt = s.status == Status::Running;
                o.deny_all = s.pending > 0;
                o.close_runtime = true;
                o.cascade = true;
                Ok(o)
            }
            Ev::Unarchive => {
                if !s.archived {
                    return Err(Refusal::NotAllowed);
                }
                n.archived = false;
                n.status = Status::Closed;
                Ok(quiet(n))
            }
            Ev::Delete => {
                n.deleted = true;
                n.status = Status::Closed;
                n.pending = 0;
                let mut o = quiet(n);
                o.interrupt = s.status == Status::Running;
                o.remove = true;
                Ok(o)
            }
            Ev::RestartRepair => {
                n.pending = 0;
                let was_active = matches!(s.status, Status::Initializing | Status::Running);
                if was_active {
                    n.status = Status::Closed;
                }
                let mut o = quiet(n);
                o.interrupted_note = was_active;
                Ok(o)
            }
        }
    }
}

fn to_kernel_status(s: AgentStatus) -> kernel::Status {
    match s {
        AgentStatus::Initializing => kernel::Status::Initializing,
        AgentStatus::Idle => kernel::Status::Idle,
        AgentStatus::Running => kernel::Status::Running,
        AgentStatus::Error => kernel::Status::Error,
        AgentStatus::Closed => kernel::Status::Closed,
    }
}

fn to_kernel_event(e: Event) -> kernel::Ev {
    match e {
        Event::HandshakeDone => kernel::Ev::HandshakeDone,
        Event::LoadFailed => kernel::Ev::LoadFailed,
        Event::Resume => kernel::Ev::Resume,
        Event::TurnStarted => kernel::Ev::TurnStarted,
        Event::TurnCompleted => kernel::Ev::TurnCompleted,
        Event::TurnFailed => kernel::Ev::TurnFailed,
        Event::TurnCanceled => kernel::Ev::TurnCanceled,
        Event::PermissionRequested => kernel::Ev::PermissionRequested,
        Event::PermissionResolved => kernel::Ev::PermissionResolved,
        Event::ClearAttention => kernel::Ev::ClearAttention,
        Event::Close => kernel::Ev::Close,
        Event::Archive => kernel::Ev::Archive,
        Event::Unarchive => kernel::Ev::Unarchive,
        Event::Delete => kernel::Ev::Delete,
        Event::RestartRepair => kernel::Ev::RestartRepair,
    }
}

fn to_kernel_state(s: &AgentState) -> kernel::State {
    kernel::State {
        status: to_kernel_status(s.status),
        archived: s.archived,
        deleted: s.deleted,
        pending: s.pending_permissions,
        internal: s.internal,
        requires_attention: s.requires_attention,
    }
}

fn to_kernel_refusal(e: LifecycleError) -> kernel::Refusal {
    match e {
        LifecycleError::Deleted => kernel::Refusal::Deleted,
        LifecycleError::Archived => kernel::Refusal::Archived,
        LifecycleError::NotAllowed { .. } => kernel::Refusal::NotAllowed,
        LifecycleError::NoPendingPermission => kernel::Refusal::NoPendingPermission,
    }
}

/// Compare one step; returns the production state after it (if accepted).
fn check(state: &AgentState, event: Event) -> Option<AgentState> {
    let prod = step(state, event);
    let model = kernel::step(to_kernel_state(state), to_kernel_event(event));
    match (prod, model) {
        (Err(p), Err(m)) => {
            assert_eq!(to_kernel_refusal(p), m, "{state:?} {event:?}");
            None
        }
        (Ok(t), Ok(o)) => {
            let has = |f: &dyn Fn(&Effect) -> bool| t.effects.iter().any(f);
            assert_eq!(to_kernel_state(&t.next), o.next, "{state:?} {event:?}");
            assert_eq!(has(&|e| matches!(e, Effect::AutoDenyPending(_))), o.deny_all, "deny {state:?} {event:?}");
            assert_eq!(has(&|e| matches!(e, Effect::SetAttention(_))), o.set_attention, "attention {state:?} {event:?}");
            assert_eq!(has(&|e| matches!(e, Effect::NotifyAttention(_))), o.notify_permission, "notify {state:?} {event:?}");
            assert_eq!(has(&|e| *e == Effect::Interrupt), o.interrupt, "interrupt {state:?} {event:?}");
            assert_eq!(has(&|e| *e == Effect::CloseRuntime), o.close_runtime, "close {state:?} {event:?}");
            assert_eq!(has(&|e| *e == Effect::CascadeArchiveChildren), o.cascade, "cascade {state:?} {event:?}");
            assert_eq!(has(&|e| *e == Effect::RemoveRecord), o.remove, "remove {state:?} {event:?}");
            assert_eq!(has(&|e| *e == Effect::RecordInterrupted), o.interrupted_note, "note {state:?} {event:?}");
            if t.next.requires_attention != state.requires_attention && t.next.requires_attention {
                assert!(t.next.attention_reason.is_some(), "a set attention carries its reason");
            }
            Some(t.next)
        }
        (p, m) => panic!("{state:?} {event:?}: production {p:?} vs kernel {m:?}"),
    }
}

#[test]
fn every_state_and_event() {
    let mut n = 0;
    for status in AgentStatus::ALL {
        for archived in [false, true] {
            for deleted in [false, true] {
                for pending in [0, 1, 3, u32::MAX] {
                    for internal in [false, true] {
                        for requires_attention in [false, true] {
                            let state = AgentState {
                                status,
                                archived,
                                deleted,
                                pending_permissions: pending,
                                internal,
                                requires_attention,
                                attention_reason: None,
                            };
                            for event in Event::ALL {
                                check(&state, event);
                                n += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(n, 5 * 2 * 2 * 4 * 2 * 2 * 15);
}

/// xorshift64: deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn random_sequences() {
    let mut rng = Rng(0x5eed_1a6e_7a11_c0de);
    let mut accepted = 0;
    for _ in 0..2_000 {
        let mut state = AgentState::new(rng.next().is_multiple_of(5));
        for _ in 0..40 {
            let event = Event::ALL[(rng.next() % Event::ALL.len() as u64) as usize];
            if let Some(next) = check(&state, event) {
                state = next;
                accepted += 1;
            }
            if state.deleted {
                break;
            }
        }
    }
    assert!(accepted > 3_000, "the walk should take many legal steps ({accepted})");
}
