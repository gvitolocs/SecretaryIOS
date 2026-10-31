---------------------------- MODULE AttentionBudget ----------------------------
(***************************************************************************)
(* The proactive engine's attention gate (lucina-proactive/src/attention.rs,*)
(* engine.rs::tick, branch lucina-proactive): quiet hours hold, per-kind    *)
(* cooldown and the daily budget send to the Pulse, otherwise send. The     *)
(* ledger is read per candidate from Postgres (unsolicited_since); nothing  *)
(* serialises two ticks (the `run` daemon and a `lucina-proactive once`).   *)
(* TLA+ twin of `Attention` in verification/models/src/gates.rs.            *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS MaxPerDay, Cooldown, Slots, QuietFrom, Ticks, FixTickLock
Candidates == <<0, 1, 0, 1>>

VARIABLES slot, sent, lastSent, todo, inflight, sentInQuiet, cooldownBroken, maxSeen, pulse
vars == <<slot, sent, lastSent, todo, inflight, sentInQuiet, cooldownBroken, maxSeen, pulse>>

Init == /\ slot = 0 /\ sent = 0 /\ lastSent = [k \in {0, 1} |-> -100]
        /\ todo = Candidates /\ inflight = <<>>
        /\ sentInQuiet = FALSE /\ cooldownBroken = FALSE /\ maxSeen = 0 /\ pulse = FALSE

Hour == /\ slot + 1 < Slots /\ inflight = <<>> /\ slot' = slot + 1
        /\ UNCHANGED <<sent, lastSent, todo, inflight, sentInQuiet, cooldownBroken, maxSeen, pulse>>

Remove(q, i) == [j \in 1..(Len(q) - 1) |-> IF j < i THEN q[j] ELSE q[j + 1]]

Gate(i) ==   \* attention::gate on a fresh ledger read
    /\ Len(inflight) < (IF FixTickLock THEN 1 ELSE Ticks)
    /\ LET k == todo[i]
           quiet == slot >= QuietFrom
           cooling == slot < lastSent[k] + Cooldown
           spent == sent >= MaxPerDay
       IN /\ todo' = IF quiet THEN Append(Remove(todo, i), k) ELSE Remove(todo, i)
          /\ inflight' = IF ~quiet /\ ~cooling /\ ~spent THEN Append(inflight, k) ELSE inflight
    /\ UNCHANGED <<slot, sent, lastSent, sentInQuiet, cooldownBroken, maxSeen, pulse>>

Send(i) ==
    /\ LET k == inflight[i]
       IN /\ sentInQuiet' = (sentInQuiet \/ slot >= QuietFrom)
          /\ cooldownBroken' = (cooldownBroken \/ slot < lastSent[k] + Cooldown)
          /\ lastSent' = [lastSent EXCEPT ![k] = slot]
    /\ sent' = sent + 1 /\ maxSeen' = IF sent + 1 > maxSeen THEN sent + 1 ELSE maxSeen
    /\ inflight' = Remove(inflight, i)
    /\ UNCHANGED <<slot, todo, pulse>>

Pulse == /\ ~pulse /\ slot < QuietFrom /\ pulse' = TRUE
         /\ UNCHANGED <<slot, sent, lastSent, todo, inflight, sentInQuiet, cooldownBroken, maxSeen>>

Next == Hour \/ Pulse \/ (\E i \in 1..Len(todo) : Gate(i)) \/ (\E n \in 1..Len(inflight) : Send(n))
Spec == Init /\ [][Next]_vars

DailyBudget == maxSeen <= MaxPerDay
QuietHours == ~sentInQuiet
CooldownKept == ~cooldownBroken
(* channels.rs builds only URL buttons and feedback actions: no external action exists to send. *)
NeverBudgetUsedUp == sent < MaxPerDay
=============================================================================
