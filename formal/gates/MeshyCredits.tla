----------------------------- MODULE MeshyCredits -----------------------------
(***************************************************************************)
(* Meshy credit spending (lucina-meshy/src/budget.rs, service.rs,          *)
(* lucina-tools/src/tools/meshy.rs; uncommitted work in SecretaryIOS-meshy).*)
(* plan -> the owner's yes -> Ledger::reserve (check + reserve in one step)*)
(* -> steps (Meshy charges what it reports) -> settle with consumed.       *)
(* TLA+ twin of `Meshy` in verification/models/src/gates.rs.               *)
(***************************************************************************)
EXTENDS Integers, Sequences

CONSTANTS DailyBudget, Overrun, McpCanConfirm, FixStepGuard
Plans == << [est |-> 10, steps |-> 2], [est |-> 20, steps |-> 1] >>
J == 1..Len(Plans)

VARIABLES jobs, ledger, charged, refused, mcpConfirmed, spentUnconfirmed
vars == <<jobs, ledger, charged, refused, mcpConfirmed, spentUnconfirmed>>

Worst(j) == Plans[j].est + (IF FixStepGuard THEN Overrun ELSE 0)

Init == /\ jobs = [j \in J |-> [confirmed |-> FALSE, reserved |-> 0, done |-> 0, consumed |-> 0, finished |-> FALSE]]
        /\ ledger = 0 /\ charged = 0 /\ refused = FALSE /\ mcpConfirmed = FALSE /\ spentUnconfirmed = FALSE

Confirm(j, mcp) == /\ ~jobs[j].finished /\ ~jobs[j].confirmed /\ (~mcp \/ McpCanConfirm)
                   /\ jobs' = [jobs EXCEPT ![j].confirmed = TRUE]
                   /\ mcpConfirmed' = (mcpConfirmed \/ mcp)
                   /\ UNCHANGED <<ledger, charged, refused, spentUnconfirmed>>

Reserve(j) == /\ ~jobs[j].finished /\ jobs[j].confirmed /\ jobs[j].reserved = 0
              /\ LET total == Worst(j) * Plans[j].steps
                 IN IF ledger + total > DailyBudget
                    THEN /\ refused' = TRUE /\ jobs' = [jobs EXCEPT ![j].finished = TRUE] /\ UNCHANGED ledger
                    ELSE /\ ledger' = ledger + total /\ jobs' = [jobs EXCEPT ![j].reserved = total]
                         /\ UNCHANGED refused
              /\ UNCHANGED <<charged, mcpConfirmed, spentUnconfirmed>>

Step(j, cost) ==
    /\ ~jobs[j].finished /\ jobs[j].reserved > 0 /\ jobs[j].done < Plans[j].steps
    /\ IF FixStepGuard /\ jobs[j].consumed + Worst(j) > jobs[j].reserved
       THEN /\ jobs' = [jobs EXCEPT ![j].done = Plans[j].steps] /\ UNCHANGED <<charged, spentUnconfirmed>>
       ELSE /\ charged' = charged + cost
            /\ spentUnconfirmed' = (spentUnconfirmed \/ ~jobs[j].confirmed)
            /\ jobs' = [jobs EXCEPT ![j].done = @ + 1, ![j].consumed = @ + cost]
    /\ UNCHANGED <<ledger, refused, mcpConfirmed>>

Settle(j) == /\ ~jobs[j].finished /\ jobs[j].reserved > 0 /\ jobs[j].done = Plans[j].steps
             /\ ledger' = ledger - jobs[j].reserved + jobs[j].consumed
             /\ jobs' = [jobs EXCEPT ![j].finished = TRUE]
             /\ UNCHANGED <<charged, refused, mcpConfirmed, spentUnconfirmed>>

Next == \E j \in J : \/ Confirm(j, FALSE) \/ Confirm(j, TRUE) \/ Reserve(j) \/ Settle(j)
                     \/ \E extra \in 0..Overrun : Step(j, Plans[j].est + extra)
Spec == Init /\ [][Next]_vars

ChargedWithinDailyCap == charged <= DailyBudget
NoSpendWithoutYes == ~spentUnconfirmed
OnlyOwnerConfirms == ~mcpConfirmed
NeverRefused == ~refused
=============================================================================
