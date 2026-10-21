use vstd::prelude::*;

verus! {
pub struct SessionState {
    pub active: Option<nat>,
    pub history: Set<nat>,
    pub seen: Set<nat>,
}
pub enum Action { Start {session: nat}, Finish, Cancel }
pub struct Operation {pub id: nat, pub action: Action}

pub open spec fn valid(s: SessionState) -> bool {
    match s.active {Some(id) => !s.history.contains(id),None => true}
}
pub open spec fn step(s: SessionState, op: Operation) -> SessionState {
    if s.seen.contains(op.id) {s} else {
        match op.action {
            Action::Start {session} => {
                if s.active.is_none() && !s.history.contains(session) {
                    SessionState {active:Some(session),history:s.history,seen:s.seen.insert(op.id)}
                } else {s}
            },
            Action::Finish => match s.active {
                Some(id) => SessionState {active:None,history:s.history.insert(id),seen:s.seen.insert(op.id)},
                None => s,
            },
            Action::Cancel => match s.active {
                Some(_) => SessionState {active:None,history:s.history,seen:s.seen.insert(op.id)},
                None => s,
            },
        }
    }
}

proof fn transition_preserves_lifecycle(s: SessionState, op: Operation)
    requires valid(s),
    ensures valid(step(s,op)), s.history.subset_of(step(s,op).history),
{
    match op.action {Action::Start {session:_} => {}, Action::Finish => {}, Action::Cancel => {}}
}

proof fn duplicate_delivery_is_noop(s: SessionState, op: Operation)
    requires s.seen.contains(op.id),
    ensures step(s,op) == s,
{}

proof fn finish_moves_session_to_history(s: SessionState, id: nat, op: Operation)
    requires s.active == Some(id), !s.seen.contains(op.id), op.action == Action::Finish,
    ensures step(s,op).active.is_none(), step(s,op).history.contains(id), step(s,op).seen.contains(op.id),
{}

proof fn delayed_start_cannot_resurrect_cancelled_session(s: SessionState, start: Operation, cancel: Operation, id: nat)
    requires s.active.is_none(), !s.history.contains(id), !s.seen.contains(start.id), !s.seen.contains(cancel.id),
        start.id != cancel.id, start.action == (Action::Start {session:id}), cancel.action == Action::Cancel,
    ensures step(step(step(s,start),cancel),start).active.is_none(),
{
    let started=step(s,start);
    let cancelled=step(started,cancel);
    assert(cancelled.seen.contains(start.id));
    duplicate_delivery_is_noop(cancelled,start);
}
}
fn main() {}

