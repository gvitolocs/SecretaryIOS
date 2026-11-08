---------------------------- MODULE PendingActions ----------------------------
(***************************************************************************)
(* Lucina's pending-action flow (lucina-tools/src/pending.rs, agent.rs,    *)
(* lucina-server/src/chat.rs::tools_turn, branch lucina-rs). A gated tool  *)
(* call becomes a pending action (one per session, TTL); it runs only when *)
(* a later turn of the same session is an explicit yes. Session 0 is the   *)
(* owner's app session, session 1 an MCP caller's (`mcp-<caller>`).        *)
(* With LUCINA_PLANNER=tools, run_turn calls tools_turn before any MCP     *)
(* check and drops mcp_turn. TLA+ twin of `Pending` in                     *)
(* verification/models/src/gates.rs.                                       *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS Budget, TTL,
          McpMessages,   \* MCP callers send turns too
          FixMcp,        \* G1: tools_turn knows MCP turns; they never create or confirm
          FixShown       \* G2: confirmable only after the asking reply was delivered

Sessions == {0, 1}
NOP == [id |-> 0, age |-> 0, by |-> 0, shown |-> FALSE]

VARIABLES pending, nextId, turns, nextTurn, budget, executed
vars == <<pending, nextId, turns, nextTurn, budget, executed>>

Init == /\ pending = [s \in Sessions |-> NOP] /\ nextId = 1
        /\ turns = {} /\ nextTurn = 1 /\ budget = Budget /\ executed = {}

Message(s, msg) ==
    /\ budget > 0 /\ Cardinality(turns) < 2
    /\ s = 0 \/ McpMessages
    /\ turns' = turns \cup {[id |-> nextTurn, s |-> s, mcp |-> s = 1, msg |-> msg, phase |-> 0]}
    /\ nextTurn' = nextTurn + 1 /\ budget' = budget - 1
    /\ UNCHANGED <<pending, nextId, executed>>

TurnStart(t) ==
    /\ t \in turns /\ t.phase = 0
    /\ LET p0 == IF pending[t.s].id # 0 /\ pending[t.s].age >= TTL THEN NOP ELSE pending[t.s]
           live == p0.id # 0 /\ (~FixShown \/ p0.shown)
           yes == live /\ t.msg = "yes" /\ ~(FixMcp /\ t.mcp)
           no == live /\ t.msg = "no"
           afterYes == IF yes \/ no THEN NOP ELSE p0
           creates == t.msg = "request" /\ ~(FixMcp /\ t.mcp)
       IN /\ executed' = IF yes THEN executed \cup {[id |-> p0.id, s |-> t.s, turn |-> t.id, mcp |-> t.mcp,
                                                     age |-> p0.age, by |-> p0.by, shown |-> p0.shown]}
                         ELSE executed
          /\ pending' = [pending EXCEPT ![t.s] = IF creates THEN [id |-> nextId, age |-> 0, by |-> t.id, shown |-> FALSE]
                                                ELSE afterYes]
          /\ nextId' = IF creates THEN nextId + 1 ELSE nextId
    /\ turns' = (turns \ {t}) \cup {[t EXCEPT !.phase = 1]}
    /\ UNCHANGED <<nextTurn, budget>>

TurnEnd(t) ==
    /\ t \in turns /\ t.phase = 1
    /\ pending' = [pending EXCEPT ![t.s] = IF @.id # 0 /\ @.by = t.id THEN [@ EXCEPT !.shown = TRUE] ELSE @]
    /\ turns' = turns \ {t}
    /\ UNCHANGED <<nextId, nextTurn, budget, executed>>

Tick == /\ \E s \in Sessions : pending[s].id # 0
        /\ pending' = [s \in Sessions |-> IF pending[s].id # 0 /\ pending[s].age < TTL
                                          THEN [pending[s] EXCEPT !.age = @ + 1] ELSE pending[s]]
        /\ UNCHANGED <<nextId, turns, nextTurn, budget, executed>>

Next == \/ Tick
        \/ \E s \in Sessions, m \in {"request", "yes", "no"} : Message(s, m)
        \/ \E t \in turns : TurnStart(t) \/ TurnEnd(t)
Spec == Init /\ [][Next]_vars

NeverInTheRequestingTurn == \A e \in executed : e.turn # e.by
OnlyOwnerYesExecutes == \A e \in executed : ~e.mcp /\ e.s = 0
ExpiredNeverExecutes == \A e \in executed : e.age < TTL
AtMostOnce == \A e, f \in executed : e.id = f.id => e = f
ConfirmedAfterShown == \A e \in executed : e.shown
NeverConfirmed == executed = {}
=============================================================================
