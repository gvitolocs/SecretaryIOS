------------------------------ MODULE Cutover ------------------------------
(***************************************************************************)
(* Phase 1 of the Lucina Rust port: deploy/lucina-rs/cutover.sh and        *)
(* rollback.sh (branch lucina-rs). TLA+ twin of the `Cutover` model in     *)
(* verification/models/src/ownership.rs.                                   *)
(*                                                                         *)
(* p1 = which runtime secretary-server calls; Rust runs shadow.env or      *)
(* cutover.env; both runtimes read-modify-write secretary-jobs.json under  *)
(* in-process locks only (cutover.env: SECRETARY_JOBS_PATH = Node's file). *)
(* A runtime that finds a running job it is not working on marks it       *)
(* "interrupted" (findJob / JobStore::find).                               *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS Jobs,        \* app jobs started
          Runs,        \* operator script runs (cutover or rollback)
          Crashes,     \* Rust restarts by Restart=on-failure / the health timer
          FixOwnFile   \* fix: Rust's own jobs file + owner/boot check

Rts == {"node", "rust"}
NONE == [op |-> "none", j |-> 0, file |-> "none", snap |-> {}]

VARIABLES p1, mode, rustBoot, script, step, runsLeft, crashesLeft,
          files, rmw, jobs, jobsLeft, falseInterrupt, lostUpdate, rollbackBad, rolledBack

vars == <<p1, mode, rustBoot, script, step, runsLeft, crashesLeft, files, rmw, jobs,
          jobsLeft, falseInterrupt, lostUpdate, rollbackBad, rolledBack>>

BootOf(rt) == IF rt = "node" THEN 0 ELSE rustBoot
FileFor(rt) == IF rt = "rust" /\ FixOwnFile THEN "rust" ELSE "node"
RecOf(f, j) == {r \in files[f] : r.j = j}
Lookup(rt, j) == IF RecOf(FileFor(rt), j) # {} THEN RecOf(FileFor(rt), j)
                 ELSE IF FixOwnFile /\ rt = "rust" THEN RecOf("node", j) ELSE {}
TargetUp == p1 = "node" \/ mode = "live"
KillRust(js) == [i \in DOMAIN js |-> IF js[i].rt = "rust" THEN [js[i] EXCEPT !.active = FALSE] ELSE js[i]]

Apply(op, j, snap, rt) ==
    CASE op = "insert" -> snap \cup {[j |-> j, st |-> "running", owner |-> rt, boot |-> BootOf(rt)]}
      [] op = "finish" -> {IF r.j = j THEN [r EXCEPT !.st = "done"] ELSE r : r \in snap}
      [] op = "mark"   -> {IF r.j = j /\ r.st = "running" THEN [r EXCEPT !.st = "interrupted"] ELSE r : r \in snap}

Init ==
    /\ p1 = "node" /\ mode = "shadow" /\ rustBoot = 1
    /\ script = "idle" /\ step = 0 /\ runsLeft = Runs /\ crashesLeft = Crashes
    /\ files = [f \in Rts |-> {}]
    /\ rmw = [rt \in Rts |-> NONE]
    /\ jobs = <<>> /\ jobsLeft = Jobs
    /\ falseInterrupt = FALSE /\ lostUpdate = FALSE /\ rollbackBad = FALSE /\ rolledBack = FALSE

StartScript(kind) ==
    /\ script = "idle" /\ runsLeft > 0
    /\ script' = kind /\ step' = 0 /\ runsLeft' = runsLeft - 1
    /\ UNCHANGED <<p1, mode, rustBoot, crashesLeft, files, rmw, jobs, jobsLeft,
                   falseInterrupt, lostUpdate, rollbackBad, rolledBack>>

ScriptFails ==   \* an ssh error: set -e stops the script where it is
    /\ script # "idle"
    /\ script' = "idle"
    /\ UNCHANGED <<p1, mode, rustBoot, step, runsLeft, crashesLeft, files, rmw, jobs, jobsLeft,
                   falseInterrupt, lostUpdate, rollbackBad, rolledBack>>

CutoverStep ==
    /\ script = "cutover"
    /\ CASE step = 0 ->   \* preflight: /health on both
              /\ step' = 1 /\ UNCHANGED <<p1, mode, rustBoot, script, jobs, rmw>>
         [] step = 1 ->   \* cutover.env + restart lucina-rs
              /\ mode' = "live" /\ rustBoot' = rustBoot + 1 /\ step' = 2
              /\ jobs' = KillRust(jobs) /\ rmw' = [rmw EXCEPT !["rust"] = NONE]
              /\ UNCHANGED <<p1, script>>
         [] step = 2 ->   \* p1: SECRETARY_LUCINA_URL -> :8790
              /\ p1' = "rust" /\ script' = "idle"
              /\ UNCHANGED <<mode, rustBoot, step, jobs, rmw>>
    /\ UNCHANGED <<runsLeft, crashesLeft, files, jobsLeft, falseInterrupt, lostUpdate,
                   rollbackBad, rolledBack>>

RollbackStep ==
    /\ script = "rollback"
    /\ CASE step = 0 ->   \* p1 back to :8788
              /\ p1' = "node" /\ step' = 1
              /\ UNCHANGED <<mode, rustBoot, script, jobs, rmw, rollbackBad, rolledBack>>
         [] step = 1 ->   \* shadow.env + restart lucina-rs
              /\ mode' = "shadow" /\ rustBoot' = rustBoot + 1 /\ script' = "idle"
              /\ jobs' = KillRust(jobs) /\ rmw' = [rmw EXCEPT !["rust"] = NONE]
              /\ rolledBack' = TRUE
              /\ rollbackBad' = (rollbackBad \/ p1 # "node")
              /\ UNCHANGED <<p1, step>>
    /\ UNCHANGED <<runsLeft, crashesLeft, files, jobsLeft, falseInterrupt, lostUpdate>>

RustCrash ==
    /\ crashesLeft > 0
    /\ crashesLeft' = crashesLeft - 1 /\ rustBoot' = rustBoot + 1
    /\ jobs' = KillRust(jobs) /\ rmw' = [rmw EXCEPT !["rust"] = NONE]
    /\ UNCHANGED <<p1, mode, script, step, runsLeft, files, jobsLeft, falseInterrupt,
                   lostUpdate, rollbackBad, rolledBack>>

StartJob ==
    /\ jobsLeft > 0 /\ TargetUp /\ rmw[p1] = NONE
    /\ LET j == Len(jobs) + 1
       IN /\ jobs' = Append(jobs, [rt |-> p1, boot |-> BootOf(p1), active |-> TRUE,
                                   inserted |-> FALSE, app |-> "none"])
          /\ rmw' = [rmw EXCEPT ![p1] = [op |-> "insert", j |-> j, file |-> FileFor(p1),
                                         snap |-> files[FileFor(p1)]]]
    /\ jobsLeft' = jobsLeft - 1
    /\ UNCHANGED <<p1, mode, rustBoot, script, step, runsLeft, crashesLeft, files,
                   falseInterrupt, lostUpdate, rollbackBad, rolledBack>>

Finish(j) ==
    /\ jobs[j].active /\ jobs[j].inserted /\ rmw[jobs[j].rt] = NONE
    /\ rmw' = [rmw EXCEPT ![jobs[j].rt] = [op |-> "finish", j |-> j, file |-> FileFor(jobs[j].rt),
                                             snap |-> files[FileFor(jobs[j].rt)]]]
    /\ UNCHANGED <<p1, mode, rustBoot, script, step, runsLeft, crashesLeft, files, jobs,
                   jobsLeft, falseInterrupt, lostUpdate, rollbackBad, rolledBack>>

Poll(j) ==
    /\ jobs[j].inserted /\ jobs[j].app \notin {"done", "interrupted", "notfound"}
    /\ TargetUp /\ rmw[p1] = NONE
    /\ LET found == Lookup(p1, j)
           activeHere == jobs[j].rt = p1 /\ jobs[j].active
       IN IF found = {}
          THEN /\ jobs' = [jobs EXCEPT ![j].app = "notfound"] /\ UNCHANGED rmw
          ELSE LET r == CHOOSE x \in found : TRUE
                   mayMark == IF FixOwnFile THEN r.owner = p1 /\ r.boot # BootOf(p1)
                              ELSE ~activeHere
               IN IF r.st = "running" /\ mayMark
                  THEN /\ rmw' = [rmw EXCEPT ![p1] = [op |-> "mark", j |-> j, file |-> FileFor(p1),
                                                       snap |-> files[FileFor(p1)]]]
                       /\ UNCHANGED jobs
                  ELSE /\ jobs' = [jobs EXCEPT ![j].app = r.st] /\ UNCHANGED rmw
    /\ UNCHANGED <<p1, mode, rustBoot, script, step, runsLeft, crashesLeft, files, jobsLeft,
                   falseInterrupt, lostUpdate, rollbackBad, rolledBack>>

RmwWrite(rt) ==
    /\ rmw[rt] # NONE
    /\ LET w == rmw[rt]
           cur == files[w.file]
           new == Apply(w.op, w.j, w.snap, rt)
           lost == \E r \in cur : \/ \A n \in new : n.j # r.j
                                  \/ \E n \in new : n.j = r.j /\ n.st = "running" /\ r.st # "running"
       IN /\ files' = [files EXCEPT ![w.file] = new]
          /\ lostUpdate' = (lostUpdate \/ lost)
          /\ falseInterrupt' = (falseInterrupt \/ (w.op = "mark" /\ jobs[w.j].active))
          /\ jobs' = CASE w.op = "insert" -> [jobs EXCEPT ![w.j].inserted = TRUE]
                       [] w.op = "finish" -> [jobs EXCEPT ![w.j].active = FALSE]
                       [] w.op = "mark" -> [jobs EXCEPT ![w.j].app = "interrupted"]
          /\ rmw' = [rmw EXCEPT ![rt] = NONE]
    /\ UNCHANGED <<p1, mode, rustBoot, script, step, runsLeft, crashesLeft, jobsLeft,
                   rollbackBad, rolledBack>>

Next == \/ StartScript("cutover") \/ StartScript("rollback") \/ ScriptFails
        \/ CutoverStep \/ RollbackStep \/ RustCrash \/ StartJob
        \/ \E j \in 1..Len(jobs) : Finish(j) \/ Poll(j)
        \/ \E rt \in Rts : RmwWrite(rt)

Spec == Init /\ [][Next]_vars

RoutesToLiveRuntime == p1 = "node" \/ mode = "live"
RollbackRestoresNode == ~rollbackBad
NoFalseInterrupted == ~falseInterrupt
NoLostJobUpdate == ~lostUpdate
NeverCutOverWithNodeJob == ~(p1 = "rust" /\ \E j \in 1..Len(jobs) : jobs[j].rt = "node" /\ jobs[j].active)
=============================================================================
