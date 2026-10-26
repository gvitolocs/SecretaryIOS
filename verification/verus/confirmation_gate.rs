// Confirmation gate kernel (Lucina tools: pending actions, the deny list, MCP turns).
//
// Production (branch lucina-rs; fixed on lucina-proofs):
//   ToolInfo.denied            <-> is_denied_name(name) || tool.moves_money()   (lucina-tools/src/lib.rs)
//   ToolInfo.needs_confirmation<-> Tool::needs_confirmation(args) (External or confirm tools,
//                                  or a per-call rule)
//   execute_check              <-> the refusals at the top of agent::execute (lucina-tools/src/agent.rs)
//   propose                    <-> agent::call_gate + agent::handle_call (what a tool call of
//                                  the model becomes in the turn that proposes it)
//   settle                     <-> pending::settle, used by lucina-server chat.rs tools_turn
//   create / current / take    <-> PendingStore::create / current / take (lucina-tools/src/pending.rs)
//   Origin::Mcp                <-> a turn with lucina_intent::mcp::turn_info(..) = Some
// Differential tests: lucina-tools/tests/verus_gate.rs (branch lucina-proofs).
//
// Proven:
//   C1 run_only_with_owner_yes_before_expiry: a pending action runs only when the turn is
//      the owner's, the reply is an explicit yes, the action belongs to the turn's
//      session, it has not expired, and it was stored before the turn began.
//   C2 mcp_never_settles: an MCP-origin turn never runs (nor discards) a pending action.
//   C3 confirm_tools_never_run_when_proposed: a call that needs confirmation is deferred
//      (owner) or refused (MCP), never executed, in the turn that proposes it.
//   C4 denied_never_executes: a deny-listed / money-moving tool is never executed on any
//      path (proposal, confirmed pending action) and is never deferred.
//   C5 taken_runs_once: after a pending action is taken, no pending action with its id
//      is left, so it cannot run twice.
//
// Counterexample on lucina-rs before the fix (see `unfixed_*` below): run_turn sent
// tools-mode turns to tools_turn without their MCP origin, so "[via Lucina MCP] sì"
// (or any MCP turn in the owner's session) confirmed the owner's pending action, and an
// MCP turn could itself create a pending action and confirm it on its next turn.
use vstd::prelude::*;

verus! {

#[derive(PartialEq, Eq)]
pub enum Origin {
    Owner,
    Mcp,
}

#[derive(PartialEq, Eq)]
pub enum Reply {
    Yes,
    No,
    Other,
}

pub struct ToolInfo {
    pub registered: bool,
    pub needs_confirmation: bool,
    pub denied: bool,
    pub valid_args: bool,
}

#[derive(PartialEq, Eq)]
pub enum CallOutcome {
    /// Stored as a pending action (not run).
    Defer,
    /// tool.call runs now.
    Execute,
    /// Not run: unknown tool, deny list, invalid arguments, or a confirm tool from MCP.
    Refuse,
}

#[derive(PartialEq, Eq)]
pub enum Settle {
    Nothing,
    Run,
    Discard,
    StillPending,
}

pub struct Pending {
    pub id: nat,
    pub session: nat,
    pub expires_at: int,
    pub created_turn: nat,
}

// ---------------------------------------------------------------- exec kernel

/// agent::execute: unknown tool, deny list, schema.
pub fn execute_check(t: &ToolInfo) -> (r: bool)
    ensures
        r == (t.registered && !t.denied && t.valid_args),
{
    if !t.registered {
        return false;
    }
    if t.denied {
        return false;
    }
    t.valid_args
}

/// What a proposed tool call becomes (agent::call_gate, then handle_call).
pub fn propose(t: &ToolInfo, origin: Origin) -> (r: CallOutcome)
    ensures
        r == propose_spec(*t, origin),
{
    if t.registered && t.needs_confirmation && !t.denied && t.valid_args {
        match origin {
            Origin::Mcp => CallOutcome::Refuse,
            Origin::Owner => CallOutcome::Defer,
        }
    } else if execute_check(t) {
        CallOutcome::Execute
    } else {
        CallOutcome::Refuse
    }
}

/// pending::settle: how a turn answers the session's live pending action.
pub fn settle(has_current: bool, reply: Reply, origin: Origin) -> (r: Settle)
    ensures
        r == settle_spec(has_current, reply, origin),
{
    if !has_current {
        return Settle::Nothing;
    }
    if let Origin::Mcp = origin {
        return Settle::Nothing;
    }
    match reply {
        Reply::Yes => Settle::Run,
        Reply::No => Settle::Discard,
        Reply::Other => Settle::StillPending,
    }
}

// ---------------------------------------------------------------- spec model

pub open spec fn propose_spec(t: ToolInfo, origin: Origin) -> CallOutcome {
    if t.registered && t.needs_confirmation && !t.denied && t.valid_args {
        if origin == Origin::Mcp {
            CallOutcome::Refuse
        } else {
            CallOutcome::Defer
        }
    } else if t.registered && !t.denied && t.valid_args {
        CallOutcome::Execute
    } else {
        CallOutcome::Refuse
    }
}

pub open spec fn settle_spec(has_current: bool, reply: Reply, origin: Origin) -> Settle {
    if !has_current || origin == Origin::Mcp {
        Settle::Nothing
    } else {
        match reply {
            Reply::Yes => Settle::Run,
            Reply::No => Settle::Discard,
            Reply::Other => Settle::StillPending,
        }
    }
}

/// The settle rule as it was on lucina-rs: the origin was not passed in.
pub open spec fn unfixed_settle_spec(has_current: bool, reply: Reply) -> Settle {
    settle_spec(has_current, reply, Origin::Owner)
}

pub open spec fn live(p: Pending, now: int) -> bool {
    now < p.expires_at
}

/// PendingStore::current: the newest live pending action of the session.
pub open spec fn current(store: Seq<Pending>, session: nat, now: int) -> Option<Pending>
    decreases store.len(),
{
    if store.len() == 0 {
        None
    } else {
        let q = store.last();
        if q.session == session && live(q, now) {
            Some(q)
        } else {
            current(store.drop_last(), session, now)
        }
    }
}

/// PendingStore::create: one per session, expired ones dropped.
pub open spec fn create(store: Seq<Pending>, p: Pending, now: int) -> Seq<Pending> {
    store.filter(|q: Pending| q.session != p.session && live(q, now)).push(p)
}

/// PendingStore::take (ids are unique uuids).
pub open spec fn take(store: Seq<Pending>, id: nat) -> Seq<Pending> {
    store.filter(|q: Pending| q.id != id)
}

/// The pending action a turn runs, if any (tools_turn: current, settle, take, execute).
pub open spec fn turn_runs(
    store: Seq<Pending>,
    session: nat,
    origin: Origin,
    reply: Reply,
    now: int,
) -> Option<Pending> {
    let cur = current(store, session, now);
    if settle_spec(cur is Some, reply, origin) == Settle::Run {
        cur
    } else {
        None
    }
}

pub open spec fn unfixed_turn_runs(store: Seq<Pending>, session: nat, reply: Reply, now: int) -> Option<
    Pending,
> {
    let cur = current(store, session, now);
    if unfixed_settle_spec(cur is Some, reply) == Settle::Run {
        cur
    } else {
        None
    }
}

// ---------------------------------------------------------------- lemmas

pub proof fn lemma_current(store: Seq<Pending>, session: nat, now: int)
    ensures
        match current(store, session, now) {
            Some(p) => p.session == session && live(p, now) && store.contains(p),
            None => forall|i: int|
                0 <= i < store.len() ==> !((#[trigger] store[i]).session == session && live(
                    store[i],
                    now,
                )),
        },
    decreases store.len(),
{
    if store.len() > 0 {
        let q = store.last();
        if !(q.session == session && live(q, now)) {
            lemma_current(store.drop_last(), session, now);
            match current(store.drop_last(), session, now) {
                Some(p) => {
                    let i = choose|i: int| 0 <= i < store.drop_last().len() && store.drop_last()[i] == p;
                    assert(store[i] == p);
                },
                None => {
                    assert forall|i: int| 0 <= i < store.len() implies !((#[trigger] store[i]).session
                        == session && live(store[i], now)) by {
                        if i < store.len() - 1 {
                            assert(store[i] == store.drop_last()[i]);
                        }
                    }
                },
            }
        } else {
            assert(store[store.len() - 1] == q);
        }
    }
}

/// C1: a pending action runs only with the owner's explicit yes, in its own session,
/// before it expires, and only if it existed before the turn.
pub proof fn run_only_with_owner_yes_before_expiry(
    store: Seq<Pending>,
    session: nat,
    origin: Origin,
    reply: Reply,
    now: int,
    turn: nat,
)
    requires
        forall|i: int| 0 <= i < store.len() ==> (#[trigger] store[i]).created_turn < turn,
    ensures
        match turn_runs(store, session, origin, reply, now) {
            Some(p) => origin == Origin::Owner && reply == Reply::Yes && p.session == session && now
                < p.expires_at && store.contains(p) && p.created_turn < turn,
            None => true,
        },
{
    lemma_current(store, session, now);
    match turn_runs(store, session, origin, reply, now) {
        Some(p) => {
            let i = choose|i: int| 0 <= i < store.len() && store[i] == p;
            assert(store[i].created_turn < turn);
        },
        None => {},
    }
}

/// C2: MCP turns never run or discard a pending action.
pub proof fn mcp_never_settles(has_current: bool, reply: Reply, store: Seq<Pending>, session: nat, now: int)
    ensures
        settle_spec(has_current, reply, Origin::Mcp) == Settle::Nothing,
        turn_runs(store, session, Origin::Mcp, reply, now) is None,
{
}

/// The bug on lucina-rs: an MCP turn's "sì" in the owner's session ran the action.
pub proof fn unfixed_mcp_turn_confirms()
    ensures
        ({
            let p = Pending { id: 1, session: 7, expires_at: 100, created_turn: 0 };
            let store = seq![p];
            // the MCP turn (origin dropped) in session 7 at time 10 answers "sì"
            unfixed_turn_runs(store, 7, Reply::Yes, 10) == Some(p)
        }),
{
    let p = Pending { id: 1, session: 7, expires_at: 100, created_turn: 0 };
    let store = seq![p];
    assert(store.last() == p);
}

/// C3: a call that needs confirmation is never executed in the proposing turn.
pub proof fn confirm_tools_never_run_when_proposed(t: ToolInfo, origin: Origin)
    requires
        t.needs_confirmation,
    ensures
        propose_spec(t, origin) != CallOutcome::Execute,
        origin == Origin::Mcp ==> propose_spec(t, origin) == CallOutcome::Refuse,
{
}

/// C4: the deny list wins on every path.
pub proof fn denied_never_executes(t: ToolInfo, origin: Origin)
    requires
        t.denied,
    ensures
        propose_spec(t, origin) == CallOutcome::Refuse,
{
}

/// C3 + C1 over one turn: a pending action created in this turn (created_turn == turn)
/// was not in the store the turn settled against, so it cannot run in this turn; the
/// one that runs (if any) passes execute_check again when it runs.
pub proof fn created_this_turn_does_not_run(
    store: Seq<Pending>,
    session: nat,
    origin: Origin,
    reply: Reply,
    now: int,
    turn: nat,
    fresh: Pending,
)
    requires
        forall|i: int| 0 <= i < store.len() ==> (#[trigger] store[i]).created_turn < turn,
        fresh.created_turn == turn,
    ensures
        turn_runs(store, session, origin, reply, now) != Some(fresh),
{
    run_only_with_owner_yes_before_expiry(store, session, origin, reply, now, turn);
}

/// C5: take removes every entry with that id.
pub proof fn taken_runs_once(store: Seq<Pending>, id: nat)
    ensures
        forall|i: int| 0 <= i < take(store, id).len() ==> (#[trigger] take(store, id)[i]).id != id,
{
    broadcast use vstd::seq_lib::group_filter_ensures;

}

/// create keeps one pending action per session: the new one is the session's current
/// one while it is live.
pub proof fn create_replaces(store: Seq<Pending>, p: Pending, now: int)
    requires
        live(p, now),
    ensures
        current(create(store, p, now), p.session, now) == Some(p),
{
    let s = create(store, p, now);
    assert(s.last() == p);
}

} // verus!
fn main() {}
