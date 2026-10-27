// Agent lifecycle kernel (Lucina Agents: the state machine every agent state change
// in lucina-agentd goes through; Paseo 0.13.2 `server/agent/agent-manager.js`).
//
// Production (branch agents-p1, crates/lucina-agents/src/lifecycle.rs):
//   State   <-> lifecycle::AgentState (status, archived, deleted, pending_permissions,
//               internal, requires_attention; the attention reason is not needed here)
//   Ev      <-> lifecycle::Event
//   Out     <-> lifecycle::Transition: `next`, plus one flag per Effect the daemon acts on
//               (deny_all <-> AutoDenyPending(_), set_attention <-> SetAttention(_),
//               notify_permission <-> NotifyAttention(Permission), interrupt <-> Interrupt,
//               close_runtime <-> CloseRuntime, cascade <-> CascadeArchiveChildren,
//               remove <-> RemoveRecord, interrupted_note <-> RecordInterrupted)
//   step    <-> lifecycle::step
// Differential test: crates/lucina-agents/tests/verus_lifecycle.rs.
//
// Proven (postconditions of `step`, for every state and event):
//   L1 archived_terminal: an archived agent accepts only Unarchive, Delete and
//      RestartRepair; every other event is Err(Archived). RestartRepair keeps it archived.
//   L2 restart_closes: after RestartRepair the agent is Closed (no provider runtime survives
//      a daemon restart, as in Paseo's loader), so never Running or Initializing, and no
//      permission is pending.
//   L3 failure_denies_pending: an accepted TurnFailed or TurnCanceled denies every pending
//      permission (deny_all, pending 0 after).
//   L4 running_needs_turn: an accepted event that makes the status Running from another
//      status is TurnStarted.
//   L5 total: `step` returns Ok or Err for every state and event; Verus checks it has no
//      panics and no arithmetic overflow (the pending count saturates).
//   L6 deleted_absorbing: a deleted agent refuses every event with Err(Deleted).
//   L7 attention_edge: attention is set only for a non-internal agent that doesn't already
//      require attention, and then it requires attention after the step.
use vstd::prelude::*;

verus! {

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Status {
    Initializing,
    Idle,
    Running,
    Error,
    Closed,
}

#[derive(PartialEq, Eq, Clone, Copy)]
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

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Refusal {
    Deleted,
    Archived,
    NotAllowed,
    NoPendingPermission,
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub struct State {
    pub status: Status,
    pub archived: bool,
    pub deleted: bool,
    pub pending: u32,
    pub internal: bool,
    pub requires_attention: bool,
}

#[derive(PartialEq, Eq, Clone, Copy)]
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

pub open spec fn passes_archive(ev: Ev) -> bool {
    ev == Ev::Unarchive || ev == Ev::Delete || ev == Ev::RestartRepair
}

fn is_status(s: Status, t: Status) -> (b: bool)
    ensures
        b == (s == t),
{
    match (s, t) {
        (Status::Initializing, Status::Initializing) => true,
        (Status::Idle, Status::Idle) => true,
        (Status::Running, Status::Running) => true,
        (Status::Error, Status::Error) => true,
        (Status::Closed, Status::Closed) => true,
        _ => false,
    }
}

fn archive_passes(ev: Ev) -> (b: bool)
    ensures
        b == passes_archive(ev),
{
    match ev {
        Ev::Unarchive => true,
        Ev::Delete => true,
        Ev::RestartRepair => true,
        _ => false,
    }
}

fn quiet(next: State) -> (o: Out)
    ensures
        o.next == next,
        !o.deny_all && !o.set_attention && !o.notify_permission && !o.interrupt,
        !o.close_runtime && !o.cascade && !o.remove && !o.interrupted_note,
{
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

// The edge-triggered attention of Paseo `checkAndSetAttention`.
fn with_attention(o: Out) -> (r: Out)
    requires
        !o.set_attention,
    ensures
        r.set_attention == (!o.next.internal && !o.next.requires_attention),
        r.set_attention ==> r.next.requires_attention,
        !r.set_attention ==> r.next == o.next,
        r.next.status == o.next.status && r.next.archived == o.next.archived,
        r.next.deleted == o.next.deleted && r.next.pending == o.next.pending,
        r.next.internal == o.next.internal,
        r.deny_all == o.deny_all && r.notify_permission == o.notify_permission,
        r.interrupt == o.interrupt && r.close_runtime == o.close_runtime,
        r.cascade == o.cascade && r.remove == o.remove && r.interrupted_note == o.interrupted_note,
{
    if o.next.internal || o.next.requires_attention {
        return o;
    }
    let mut r = o;
    r.next.requires_attention = true;
    r.set_attention = true;
    r
}

pub fn step(s: State, ev: Ev) -> (res: Result<Out, Refusal>)
    ensures
        // L6
        s.deleted ==> res == Err::<Out, Refusal>(Refusal::Deleted),
        // L1
        !s.deleted && s.archived && !passes_archive(ev) ==> res == Err::<Out, Refusal>(
            Refusal::Archived,
        ),
        s.archived && ev == Ev::RestartRepair && res is Ok ==> res->Ok_0.next.archived,
        // L2
        ev == Ev::RestartRepair && res is Ok ==> res->Ok_0.next.status == Status::Closed
            && res->Ok_0.next.pending == 0,
        // L3
        (ev == Ev::TurnFailed || ev == Ev::TurnCanceled) && res is Ok ==> res->Ok_0.deny_all
            && res->Ok_0.next.pending == 0,
        // L4
        res is Ok && res->Ok_0.next.status == Status::Running && s.status != Status::Running
            ==> ev == Ev::TurnStarted,
        // L7
        res is Ok && res->Ok_0.set_attention ==> !s.internal && !s.requires_attention
            && res->Ok_0.next.requires_attention,
        // internal never changes
        res is Ok ==> res->Ok_0.next.internal == s.internal,
{
    if s.deleted {
        return Err(Refusal::Deleted);
    }
    if s.archived && !archive_passes(ev) {
        return Err(Refusal::Archived);
    }
    let mut n = s;
    match ev {
        Ev::HandshakeDone => {
            if !is_status(s.status, Status::Initializing) {
                return Err(Refusal::NotAllowed);
            }
            n.status = Status::Idle;
            Ok(quiet(n))
        },
        Ev::LoadFailed => {
            if !is_status(s.status, Status::Initializing) {
                return Err(Refusal::NotAllowed);
            }
            n.status = Status::Error;
            Ok(with_attention(quiet(n)))
        },
        Ev::Resume => {
            if !is_status(s.status, Status::Closed) {
                return Err(Refusal::NotAllowed);
            }
            n.status = Status::Initializing;
            Ok(quiet(n))
        },
        Ev::TurnStarted => {
            if is_status(s.status, Status::Idle) || is_status(s.status, Status::Error) {
                n.status = Status::Running;
                Ok(quiet(n))
            } else if is_status(s.status, Status::Running) {
                Ok(quiet(n))
            } else {
                Err(Refusal::NotAllowed)
            }
        },
        Ev::TurnCompleted => {
            if !is_status(s.status, Status::Running) {
                return Err(Refusal::NotAllowed);
            }
            n.status = Status::Idle;
            Ok(with_attention(quiet(n)))
        },
        Ev::TurnFailed => {
            if !is_status(s.status, Status::Running) {
                return Err(Refusal::NotAllowed);
            }
            n.status = Status::Error;
            n.pending = 0;
            let mut o = quiet(n);
            o.deny_all = true;
            Ok(with_attention(o))
        },
        Ev::TurnCanceled => {
            if !is_status(s.status, Status::Running) {
                return Err(Refusal::NotAllowed);
            }
            n.status = Status::Idle;
            n.pending = 0;
            let mut o = quiet(n);
            o.deny_all = true;
            Ok(with_attention(o))
        },
        Ev::PermissionRequested => {
            if !is_status(s.status, Status::Running) {
                return Err(Refusal::NotAllowed);
            }
            n.pending = if s.pending < u32::MAX {
                s.pending + 1
            } else {
                s.pending
            };
            let mut o = quiet(n);
            o.notify_permission = s.pending == 0 && !s.internal;
            Ok(o)
        },
        Ev::PermissionResolved => {
            if s.pending == 0 {
                return Err(Refusal::NoPendingPermission);
            }
            n.pending = s.pending - 1;
            Ok(quiet(n))
        },
        Ev::ClearAttention => {
            n.requires_attention = false;
            Ok(quiet(n))
        },
        Ev::Close => {
            if is_status(s.status, Status::Closed) {
                return Err(Refusal::NotAllowed);
            }
            n.status = Status::Closed;
            n.pending = 0;
            let mut o = quiet(n);
            o.interrupt = is_status(s.status, Status::Running);
            o.deny_all = s.pending > 0;
            o.close_runtime = true;
            Ok(o)
        },
        Ev::Archive => {
            n.status = Status::Closed;
            n.archived = true;
            n.pending = 0;
            let mut o = quiet(n);
            o.interrupt = is_status(s.status, Status::Running);
            o.deny_all = s.pending > 0;
            o.close_runtime = true;
            o.cascade = true;
            Ok(o)
        },
        Ev::Unarchive => {
            if !s.archived {
                return Err(Refusal::NotAllowed);
            }
            n.archived = false;
            n.status = Status::Closed;
            Ok(quiet(n))
        },
        Ev::Delete => {
            n.deleted = true;
            n.status = Status::Closed;
            n.pending = 0;
            let mut o = quiet(n);
            o.interrupt = is_status(s.status, Status::Running);
            o.remove = true;
            Ok(o)
        },
        Ev::RestartRepair => {
            n.pending = 0;
            n.status = Status::Closed;
            let was_active = is_status(s.status, Status::Initializing) || is_status(
                s.status,
                Status::Running,
            );
            let mut o = quiet(n);
            o.interrupted_note = was_active;
            Ok(o)
        },
    }
}

} // verus!

fn main() {}
