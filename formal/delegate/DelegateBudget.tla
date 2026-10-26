---------------------------- MODULE DelegateBudget ----------------------------
(***************************************************************************)
(* The delegation budget across a day change (lucina-delegate              *)
(* engine.rs::start + state.rs::{reserve, release, with}, branch           *)
(* lucina-rs). An agent is claimed (one job per agent), reserves a call,   *)
(* then the self-review and the sandbox run; a skip releases the call on   *)
(* whatever day the release runs (finding B1). Midnight clears the counts  *)
(* and pauses. TLA+ twin of crates/lucina-delegate/tests/budget_model.rs   *)
(* (the same state, the same counts: 17 788 current, 14 094 fixed).        *)
(***************************************************************************)
EXTENDS Integers

CONSTANTS Fixed, TotalCap, AgentCap, Days
Agents == {0, 1}
DayIdx == 0..(Days - 1)

VARIABLES day, pausedAll, paused, calls, total, jobs, ran, ranAgent,
          inflightAtMidnight, released, hit
vars == <<day, pausedAll, paused, calls, total, jobs, ran, ranAgent,
          inflightAtMidnight, released, hit>>

Idle == [k |-> "idle", d |-> 0]

Init == /\ day = 0 /\ pausedAll = FALSE /\ paused = [a \in Agents |-> FALSE]
        /\ calls = [a \in Agents |-> 0] /\ total = 0 /\ jobs = [a \in Agents |-> Idle]
        /\ ran = [d \in DayIdx |-> 0] /\ ranAgent = [d \in DayIdx |-> [a \in Agents |-> 0]]
        /\ inflightAtMidnight = FALSE /\ released = FALSE /\ hit = FALSE

Sat(x) == IF x = 0 THEN 0 ELSE x - 1

\* StateStore::reserve (switches off; no wall or turn caps).
Reserve(a) ==
    /\ jobs[a].k = "idle"
    /\ IF pausedAll \/ paused[a] THEN UNCHANGED vars
       ELSE IF total >= TotalCap
            THEN /\ pausedAll' = TRUE /\ hit' = TRUE
                 /\ UNCHANGED <<day, paused, calls, total, jobs, ran, ranAgent, inflightAtMidnight, released>>
       ELSE IF calls[a] >= AgentCap
            THEN /\ paused' = [paused EXCEPT ![a] = TRUE] /\ hit' = TRUE
                 /\ UNCHANGED <<day, pausedAll, calls, total, jobs, ran, ranAgent, inflightAtMidnight, released>>
       ELSE /\ calls' = [calls EXCEPT ![a] = @ + 1] /\ total' = total + 1
            /\ jobs' = [jobs EXCEPT ![a] = [k |-> "reserved", d |-> day]]
            /\ UNCHANGED <<day, pausedAll, paused, ran, ranAgent, inflightAtMidnight, released, hit>>

\* The self-review skipped it (or the sandbox failed): StateStore::release.
Skip(a) ==
    /\ jobs[a].k = "reserved"
    /\ IF ~Fixed \/ jobs[a].d = day
       THEN calls' = [calls EXCEPT ![a] = Sat(@)] /\ total' = Sat(total)
       ELSE UNCHANGED <<calls, total>>
    /\ jobs' = [jobs EXCEPT ![a] = Idle] /\ released' = TRUE
    /\ UNCHANGED <<day, pausedAll, paused, ran, ranAgent, inflightAtMidnight, hit>>

Run(a) ==
    /\ jobs[a].k = "reserved"
    /\ LET d == jobs[a].d
       IN /\ jobs' = [jobs EXCEPT ![a] = [k |-> "running", d |-> d]]
          /\ ran' = [ran EXCEPT ![d] = @ + 1]
          /\ ranAgent' = [ranAgent EXCEPT ![d][a] = @ + 1]
    /\ UNCHANGED <<day, pausedAll, paused, calls, total, inflightAtMidnight, released, hit>>

Finish(a) ==
    /\ jobs[a].k = "running"
    /\ jobs' = [jobs EXCEPT ![a] = Idle]
    /\ UNCHANGED <<day, pausedAll, paused, calls, total, ran, ranAgent, inflightAtMidnight, released, hit>>

\* StateStore::with on a new day.
Midnight ==
    /\ day + 1 < Days
    /\ day' = day + 1 /\ pausedAll' = FALSE /\ paused' = [a \in Agents |-> FALSE]
    /\ calls' = [a \in Agents |-> 0] /\ total' = 0
    /\ inflightAtMidnight' = (inflightAtMidnight \/ \E a \in Agents : jobs[a].k = "reserved")
    /\ UNCHANGED <<jobs, ran, ranAgent, released, hit>>

Next == Midnight \/ \E a \in Agents : Reserve(a) \/ Skip(a) \/ Run(a) \/ Finish(a)
Spec == Init /\ [][Next]_vars

DayTotalWithinCap == \A d \in DayIdx : ran[d] <= TotalCap
AgentWithinCap == \A d \in DayIdx : \A a \in Agents : ranAgent[d][a] <= AgentCap

\* Witnesses (MUST FAIL: the interesting states are reached).
NeverInFlightAtMidnightThenReleased == ~(inflightAtMidnight /\ released)
NeverCapHit == ~hit
NeverCapReached == \A d \in DayIdx : ran[d] < TotalCap
=============================================================================
