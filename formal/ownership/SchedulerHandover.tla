--------------------------- MODULE SchedulerHandover ---------------------------
(***************************************************************************)
(* The planned phase-2 handover of reminders.json and the reminder         *)
(* scheduler (docs/lucina-rust/PORT_PLAN.md section 5 phase 2, section 6). *)
(* TLA+ twin of the `Scheduler` model in verification/models/src/          *)
(* ownership.rs.                                                           *)
(*                                                                         *)
(* Both runtimes read-modify-write reminders.json, each under its own      *)
(* in-process lock only. A scheduler pass reads the file, sends every due  *)
(* reminder (Telegram sendMessage), then writes them all `sent` at the end *)
(* (Node processDueReminders, Rust ReminderStore::process_due). A restart  *)
(* (an operator step, or a crash) drops whatever the process was doing.    *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS Reminders, Crashes,
          PlanName,    \* "as_written" | "fixed" | "fixed_then_rollback"
          FixClaim,    \* claim (`sending`) before sendMessage; a dead claim is failed, not resent
          FixDrain     \* Node is restarted only with its scheduler idle

Rts == {"node", "rust"}
R == 1..Reminders

S(rt, sched, flag, internal) == [rt |-> rt, sched |-> sched, flag |-> flag, internal |-> internal]
(* (1) Rust up as the reminders store, scheduler off; (2) Node with the shim, *)
(* scheduler off; (3) Rust scheduler on.                                      *)
AsWritten == <<S("rust", FALSE, TRUE, TRUE), S("node", FALSE, TRUE, FALSE), S("rust", TRUE, TRUE, TRUE)>>
(* Proposed: (1) Rust opens only the internal endpoint; (2) Node shim on,     *)
(* drained; (3) Rust writes locally and schedules.                            *)
Fixed == <<S("rust", FALSE, FALSE, TRUE), S("node", FALSE, TRUE, FALSE), S("rust", TRUE, TRUE, TRUE)>>
(* Rollback: Rust stops writing first, then Node takes over again.            *)
Rollback == <<S("rust", FALSE, FALSE, FALSE), S("node", TRUE, FALSE, FALSE)>>
Plan == CASE PlanName = "as_written" -> AsWritten
          [] PlanName = "fixed" -> Fixed
          [] PlanName = "fixed_then_rollback" -> Fixed \o Rollback
IDLE == [kind |-> "idle", r |-> 0, snap |-> {}, due |-> {}, sent |-> {}]

VARIABLES env, work, file, created, requested, deliveries, pos, crashesLeft, lostRecord, settled
vars == <<env, work, file, created, requested, deliveries, pos, crashesLeft, lostRecord, settled>>

StatusIn(f, r) == {x.st : x \in {y \in f : y.r = r}}
Pending(f) == {x.r : x \in {y \in f : y.st = "pending"}}

(* Who writes a create that arrives through `via` (Telegram -> Node, app -> Rust). *)
WriterVia(via) ==
    IF via = "node"
    THEN IF ~env["node"].up THEN "none"
         ELSE IF env["node"].flag   \* Node shim -> Rust /api/internal/reminders
              THEN (IF env["rust"].up /\ env["rust"].internal THEN "rust" ELSE "none")
              ELSE "node"
    ELSE IF ~env["rust"].up THEN "none"
         ELSE IF env["rust"].flag THEN "rust"
         \* writer=node: forwarded to Node's /api/secretary/reminders
         ELSE IF ~env["node"].up THEN "none"
         ELSE IF env["node"].flag
              THEN (IF env["rust"].internal THEN "rust" ELSE "none")
              ELSE "node"

SchedulerOn(rt) == IF rt = "node" THEN env["node"].up /\ env["node"].sched
                   \* LUCINA_SCHEDULER=true is ignored while LUCINA_REMINDERS_WRITER=node
                   ELSE env["rust"].up /\ env["rust"].sched /\ env["rust"].flag

WriteFile(new) ==
    /\ lostRecord' = (lostRecord \/ \E x \in file :
                         \/ \A y \in new : y.r # x.r
                         \/ \E y \in new : y.r = x.r /\ y.st = "pending" /\ x.st # "pending")
    /\ file' = new

Restarted(rt) ==   \* <<work, file, requested>> after rt's process restarts
    LET w == work[rt]
        f2 == IF FixClaim /\ w.kind = "tick"
              THEN {IF x.st = "sending" /\ x.r \in w.due THEN [x EXCEPT !.st = "failed"] ELSE x : x \in file}
              ELSE file
        q2 == IF w.kind = "create" THEN [requested EXCEPT ![w.r] = FALSE] ELSE requested
    IN <<[work EXCEPT ![rt] = IDLE], f2, q2>>

Init ==
    /\ env = [node |-> [up |-> TRUE, sched |-> TRUE, flag |-> FALSE, internal |-> FALSE],
              rust |-> [up |-> TRUE, sched |-> FALSE, flag |-> FALSE, internal |-> FALSE]]
    /\ work = [rt \in Rts |-> IDLE]
    /\ file = {}
    /\ created = [r \in R |-> FALSE] /\ requested = [r \in R |-> FALSE]
    /\ deliveries = [r \in R |-> 0]
    /\ pos = 1 /\ crashesLeft = Crashes /\ lostRecord = FALSE /\ settled = FALSE

Operator ==
    /\ pos <= Len(Plan) /\ ~settled
    /\ LET st == Plan[pos]
           rs == Restarted(st.rt)
       IN /\ ~(FixDrain /\ st.rt = "node" /\ work["node"].kind # "idle")
          /\ work' = rs[1] /\ file' = rs[2] /\ requested' = rs[3]
          /\ env' = [env EXCEPT ![st.rt] = [up |-> TRUE, sched |-> st.sched, flag |-> st.flag,
                                             internal |-> st.internal]]
    /\ pos' = pos + 1
    /\ UNCHANGED <<created, deliveries, crashesLeft, lostRecord, settled>>

Crash(rt) ==
    /\ crashesLeft > 0 /\ work[rt].kind # "idle" /\ ~settled
    /\ LET rs == Restarted(rt) IN work' = rs[1] /\ file' = rs[2] /\ requested' = rs[3]
    /\ crashesLeft' = crashesLeft - 1
    /\ UNCHANGED <<env, created, deliveries, pos, lostRecord, settled>>

Create(r, via) ==
    /\ ~created[r] /\ ~requested[r] /\ ~settled
    /\ LET w == WriterVia(via)
       IN /\ w # "none" /\ work[w].kind = "idle"
          /\ work' = [work EXCEPT ![w] = [kind |-> "create", r |-> r, snap |-> file, due |-> {}, sent |-> {}]]
    /\ requested' = [requested EXCEPT ![r] = TRUE]
    /\ UNCHANGED <<env, file, created, deliveries, pos, crashesLeft, lostRecord, settled>>

CreateWrite(rt) ==
    /\ work[rt].kind = "create"
    /\ WriteFile(work[rt].snap \cup {[r |-> work[rt].r, st |-> "pending"]})
    /\ created' = [created EXCEPT ![work[rt].r] = TRUE]
    /\ requested' = [requested EXCEPT ![work[rt].r] = FALSE]
    /\ work' = [work EXCEPT ![rt] = IDLE]
    /\ UNCHANGED <<env, deliveries, pos, crashesLeft, settled>>

TickRead(rt) ==
    /\ work[rt].kind = "idle" /\ SchedulerOn(rt) /\ Pending(file) # {} /\ ~settled
    /\ work' = [work EXCEPT ![rt] = [kind |-> "tick", r |-> 0, snap |-> file, due |-> Pending(file),
                                     sent |-> {}]]
    /\ UNCHANGED <<env, file, created, requested, deliveries, pos, crashesLeft, lostRecord, settled>>

Claimed(rt) == \A d \in work[rt].due : "sending" \in StatusIn(work[rt].snap, d)

TickClaim(rt) ==
    /\ FixClaim /\ work[rt].kind = "tick" /\ ~Claimed(rt)
    /\ LET snap2 == {IF x.r \in work[rt].due THEN [x EXCEPT !.st = "sending"] ELSE x : x \in work[rt].snap}
       IN /\ WriteFile(snap2)
          /\ work' = [work EXCEPT ![rt].snap = snap2]
    /\ UNCHANGED <<env, created, requested, deliveries, pos, crashesLeft, settled>>

TickSend(rt) ==
    /\ work[rt].kind = "tick" /\ (~FixClaim \/ Claimed(rt))
    /\ \E d \in work[rt].due \ work[rt].sent :
          /\ deliveries' = [deliveries EXCEPT ![d] = @ + 1]
          /\ work' = [work EXCEPT ![rt].sent = @ \cup {d}]
    /\ UNCHANGED <<env, file, created, requested, pos, crashesLeft, lostRecord, settled>>

TickWrite(rt) ==
    /\ work[rt].kind = "tick" /\ (~FixClaim \/ Claimed(rt)) /\ work[rt].sent = work[rt].due
    /\ WriteFile({IF x.r \in work[rt].sent THEN [x EXCEPT !.st = "sent"] ELSE x : x \in work[rt].snap})
    /\ work' = [work EXCEPT ![rt] = IDLE]
    /\ UNCHANGED <<env, created, requested, deliveries, pos, crashesLeft, settled>>

Settle ==
    /\ ~settled /\ \A rt \in Rts : work[rt].kind = "idle"
    /\ \A r \in R : created[r]
    /\ pos > Len(Plan)
    /\ Pending(file) = {} \/ ~(SchedulerOn("node") \/ SchedulerOn("rust"))
    /\ settled' = TRUE
    /\ UNCHANGED <<env, work, file, created, requested, deliveries, pos, crashesLeft, lostRecord>>

Next == \/ Operator \/ Settle
        \/ \E rt \in Rts : Crash(rt) \/ CreateWrite(rt) \/ TickRead(rt) \/ TickClaim(rt)
                           \/ TickSend(rt) \/ TickWrite(rt)
        \/ \E r \in R, via \in Rts : Create(r, via)

Spec == Init /\ [][Next]_vars

SingleScheduler == ~(SchedulerOn("node") /\ SchedulerOn("rust"))
SingleRemindersWriter == ~(work["node"].kind # "idle" /\ work["rust"].kind # "idle")
NoLostReminderRecord == ~lostRecord
NoDoubleDelivery == \A r \in R : deliveries[r] <= 1
NoSilentLoss == settled => \A r \in R : deliveries[r] > 0 \/ "failed" \in StatusIn(file, r)
NeverHandedOver == ~(settled /\ SchedulerOn("rust"))
=============================================================================
