----------------------------- MODULE DealWatch -----------------------------
(***************************************************************************)
(* The grocery deal watch: twin of verification/models/src/deals.rs        *)
(* (Stateright), with the same state, actions and invariants. See          *)
(* docs/verification.md section 7 for the mapping to                       *)
(* crates/lucina-deals (branch lucina-rs) and secretary-server's notify.   *)
(*                                                                         *)
(* Offers 0 and 1 are item 0 (food waste and a flyer alternative), offer 2 *)
(* is item 1 (flyer). A price is 0 (none), 1 or 2. Clock: per day slot 0   *)
(* digest window, 1 digest catch-up + first alert tick, 2 later alerts;    *)
(* t = 6 is the second evening. Actor 0 is the Node watcher, 1 the Rust    *)
(* one. "None" values are sentinels (price 3) so that TLC compares values  *)
(* of one type.                                                            *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS Node, Overlap, Dedup, FixPending, FixAckDelivered, FixMinPrice,
          MaxRuns, MaxEdits, MaxFaults

Offers == 0..2
Items == 0..1
Dates == 0..1
Actors == {0, 1}
NODE == 0
RUST == 1
Source(o) == IF o = 0 THEN 0 ELSE 1
ItemOf(o) == IF o = 2 THEN 1 ELSE 0
INIT == [o \in Offers |-> IF o = 0 THEN 1 ELSE IF o = 1 THEN 2 ELSE 0]
CAP == 4
END == 6
Day(x) == x \div 3
Slot(x) == x % 3
Min(a, b) == IF a < b THEN a ELSE b
Zero == [o \in Offers |-> 0]
NoVec == [o \in Offers |-> 3]
NoBest == <<0, 0>>
NoMsg == [kind |-> "none", date |-> 0, batch |-> [i \in Items |-> NoBest], obs |-> Zero]
EmptyFile == [notified |-> Zero, dd |-> 0, fp |-> NoVec,
              counts |-> [d \in Dates |-> 0], pending |-> NoMsg]
IdleRun == [phase |-> "idle", kind |-> "digest", date |-> 0, snap |-> EmptyFile,
            obs |-> Zero, failed |-> FALSE, polled |-> FALSE, msg |-> NoMsg]

VARIABLES t, src, edits, faults, runs, file, run, up, nodeStopped, server,
          recv, digests, lastDigestObs, lastObs, since, bad, seen
vars == <<t, src, edits, faults, runs, file, run, up, nodeStopped, server,
          recv, digests, lastDigestObs, lastObs, since, bad, seen>>

(* The item's best active offer <<offer, price>> (a tie keeps offer 0), NoBest if none. *)
Best(v, i) ==
    IF i = 0
    THEN IF v[0] = 0 /\ v[1] = 0 THEN NoBest
         ELSE IF v[0] > 0 /\ (v[1] = 0 \/ v[0] <= v[1]) THEN <<0, v[0]>>
         ELSE <<1, v[1]>>
    ELSE IF v[2] > 0 THEN <<2, v[2]>> ELSE NoBest

KindDue(x, kind) ==
    x < END /\ IF kind = "digest" THEN Slot(x) \in {0, 1} ELSE Slot(x) \in {1, 2}

Key(m) == <<m.kind, m.date, m.batch>>

(* compose: NoMsg = skip (no active matches, unchanged digest, nothing new or cheaper). *)
Compose(r) ==
    LET obs == r.obs
        batch == [i \in Items |->
                    LET b == Best(obs, i)
                        old == r.snap.notified[b[1]]
                    IN IF b[2] > 0 /\ (r.kind = "digest" \/ old = 0 \/ old > b[2])
                       THEN b ELSE NoBest]
    IN IF obs = Zero THEN NoMsg
       ELSE IF r.kind = "digest" /\ r.snap.fp = obs THEN NoMsg
       ELSE IF \A i \in Items : batch[i][2] = 0 THEN NoMsg
       ELSE [kind |-> r.kind, date |-> r.date, batch |-> batch, obs |-> obs]

DeliveredPrice(m, o) ==
    LET b == m.batch[ItemOf(o)] IN IF b[2] > 0 /\ b[1] = o THEN b[2] ELSE 0

SetN(old, p) == IF FixMinPrice /\ old # 0 THEN Min(old, p) ELSE p

(* acknowledge (F1: and clear the pending message). *)
Ack(f, m) ==
    [f EXCEPT
        !.notified = [o \in Offers |->
            IF FixAckDelivered
            THEN IF DeliveredPrice(m, o) > 0 THEN SetN(f.notified[o], DeliveredPrice(m, o))
                 ELSE f.notified[o]
            ELSE IF m.obs[o] > 0 THEN SetN(f.notified[o], m.obs[o]) ELSE f.notified[o]],
        !.dd = IF m.kind = "digest" THEN m.date + 1 ELSE f.dd,
        !.fp = IF m.kind = "digest" THEN m.obs ELSE f.fp,
        !.counts = IF m.kind = "alert"
                   THEN [d \in Dates |-> IF d = m.date THEN f.counts[d] + 1
                                         ELSE IF d < m.date THEN 0 ELSE f.counts[d]]
                   ELSE f.counts,
        !.pending = NoMsg]

(* no_skip at the end of a successful alert run. *)
Skip(r) ==
    IF r.kind = "alert" /\ r.polled /\ ~r.failed /\
       \E i \in Items :
          LET b == Best(src, i)
          IN b[2] > 0 /\ since[i] >= 2 /\ Best(r.obs, i) = b /\
             ~(\E p \in recv[b[1]] : p <= b[2])
    THEN {"skip"} ELSE {}

Quiet == \A a \in Actors : run[a].phase = "idle"

Init ==
    /\ t = 0
    /\ src = INIT
    /\ edits = 0
    /\ faults = 0
    /\ runs = 0
    /\ file = EmptyFile
    /\ run = [a \in Actors |-> IdleRun]
    /\ up = [a \in Actors |-> IF a = NODE THEN Node \/ Overlap ELSE ~Node \/ Overlap]
    /\ nodeStopped = FALSE
    /\ server = {}
    /\ recv = [o \in Offers |-> {}]
    /\ digests = [d \in Dates |-> 0]
    /\ lastDigestObs = NoVec
    /\ lastObs = NoVec
    /\ since = [i \in Items |-> 0]
    /\ bad = {}
    /\ seen = {}

Tick ==
    /\ Quiet /\ t < END
    /\ t' = t + 1
    /\ since' = IF t + 1 < END /\ Slot(t + 1) \in {1, 2}
                THEN [i \in Items |-> IF Best(src, i)[2] > 0 THEN Min(since[i] + 1, 2) ELSE since[i]]
                ELSE since
    /\ UNCHANGED <<src, edits, faults, runs, file, run, up, nodeStopped, server,
                   recv, digests, lastDigestObs, lastObs, bad, seen>>

Edit(o, p) ==
    /\ Quiet /\ edits < MaxEdits /\ p # src[o]
    /\ src' = [src EXCEPT ![o] = p]
    /\ edits' = edits + 1
    /\ since' = [i \in Items |-> IF Best(src', i) # Best(src, i) THEN 0 ELSE since[i]]
    /\ UNCHANGED <<t, faults, runs, file, run, up, nodeStopped, server,
                   recv, digests, lastDigestObs, lastObs, bad, seen>>

StopNode ==
    /\ Node /\ ~Overlap /\ up[NODE]
    /\ seen' = IF run[NODE].phase = "sent" THEN seen \cup {"gap"} ELSE seen
    /\ run' = [run EXCEPT ![NODE] = IdleRun]
    /\ up' = [up EXCEPT ![NODE] = FALSE]
    /\ nodeStopped' = TRUE
    /\ UNCHANGED <<t, src, edits, faults, runs, file, server, recv, digests,
                   lastDigestObs, lastObs, since, bad>>

StartRust ==
    /\ Node /\ ~Overlap /\ ~up[NODE] /\ ~up[RUST]
    /\ up' = [up EXCEPT ![RUST] = TRUE]
    /\ UNCHANGED <<t, src, edits, faults, runs, file, run, nodeStopped, server,
                   recv, digests, lastDigestObs, lastObs, since, bad, seen>>

Load(a, kind) ==
    /\ up[a] /\ run[a].phase = "idle" /\ runs < MaxRuns /\ KindDue(t, kind)
    /\ LET date == Day(t)
           f == file
           resend == FixPending /\ f.pending # NoMsg
       IN /\ resend \/ ~(kind = "digest" /\ f.dd = date + 1)
          /\ resend \/ ~(kind = "alert" /\ f.counts[date] >= CAP)
          /\ run' = [run EXCEPT ![a] =
                IF resend
                THEN [phase |-> "ready", kind |-> f.pending.kind, date |-> f.pending.date,
                      snap |-> f, obs |-> f.pending.obs, failed |-> FALSE,
                      polled |-> FALSE, msg |-> f.pending]
                ELSE [IdleRun EXCEPT !.phase = "loaded", !.kind = kind, !.date = date,
                      !.snap = f]]
    /\ runs' = runs + 1
    /\ seen' = seen \cup {IF a = NODE THEN "nodeLoad" ELSE "rustLoad"}
                    \cup (IF a = RUST /\ (nodeStopped \/ Overlap) /\ (~Overlap \/ "nodeLoad" \in seen)
                          THEN {"cutover"} ELSE {})
    /\ UNCHANGED <<t, src, edits, faults, file, up, nodeStopped, server, recv, digests,
                   lastDigestObs, lastObs, since, bad>>

(* mask bit 0: food waste failed, bit 1: flyers failed. *)
Lost(mask, s) == (IF s = 0 THEN mask % 2 ELSE mask \div 2) = 1

Poll(a, mask) ==
    /\ up[a] /\ run[a].phase = "loaded"
    /\ mask = 0 \/ faults < MaxFaults
    /\ run' = [run EXCEPT ![a].obs = [o \in Offers |-> IF Lost(mask, Source(o)) THEN 0 ELSE src[o]],
                          ![a].failed = (mask # 0), ![a].polled = TRUE, ![a].phase = "polled"]
    /\ faults' = IF mask # 0 THEN faults + 1 ELSE faults
    /\ UNCHANGED <<t, src, edits, runs, file, up, nodeStopped, server, recv, digests,
                   lastDigestObs, lastObs, since, bad, seen>>

Decide(a) ==
    /\ up[a] /\ run[a].phase = "polled"
    /\ LET r == run[a]
           m == Compose(r)
       IN IF m # NoMsg
          THEN /\ run' = [run EXCEPT ![a].msg = m,
                                     ![a].phase = IF FixPending THEN "composed" ELSE "ready"]
               /\ bad' = bad
          ELSE IF r.kind = "digest"
          THEN /\ run' = [run EXCEPT ![a].phase = "skipsave"]
               /\ bad' = bad
          ELSE /\ run' = [run EXCEPT ![a] = IdleRun]
               /\ bad' = bad \cup Skip(r)
    /\ UNCHANGED <<t, src, edits, faults, runs, file, up, nodeStopped, server, recv, digests,
                   lastDigestObs, lastObs, since, seen>>

Claim(a) ==
    /\ up[a] /\ run[a].phase = "composed"
    /\ file' = [run[a].snap EXCEPT !.pending = run[a].msg]
    /\ run' = [run EXCEPT ![a].snap = [run[a].snap EXCEPT !.pending = run[a].msg],
                          ![a].phase = "ready"]
    /\ UNCHANGED <<t, src, edits, faults, runs, up, nodeStopped, server, recv, digests,
                   lastDigestObs, lastObs, since, bad, seen>>

(* What an admitted message shows the witnesses (the owner receives it in Send). *)
AdmitSeen(m) ==
    (IF m.kind = "digest" /\ \E i \in Items : m.batch[i][2] > 0 /\ m.batch[i][2] \in recv[m.batch[i][1]]
     THEN {"relist"} ELSE {})
    \cup (IF m.kind = "alert" /\ \E i \in Items : m.batch[i][2] > 0 /\ INIT[m.batch[i][1]] # m.batch[i][2]
          THEN {"newAlert"} ELSE {})

Send(a, outcome) ==
    /\ up[a] /\ run[a].phase = "ready"
    /\ outcome = "delivered" \/ faults < MaxFaults
    /\ LET m == run[a].msg
           admitted == outcome # "down" /\ (~Dedup \/ Key(m) \notin server)
           seen1 == IF admitted THEN seen \cup AdmitSeen(m) ELSE seen
       IN /\ server' = IF admitted THEN server \cup {Key(m)} ELSE server
          /\ IF admitted
             THEN /\ recv' = [o \in Offers |-> recv[o] \cup
                                (IF DeliveredPrice(m, o) > 0 THEN {DeliveredPrice(m, o)} ELSE {})]
                  /\ digests' = IF m.kind = "digest"
                                THEN [digests EXCEPT ![m.date] = Min(@ + 1, 2)] ELSE digests
                  /\ lastDigestObs' = IF m.kind = "digest" THEN m.obs ELSE lastDigestObs
                  /\ lastObs' = m.obs
                  /\ bad' = bad
                        \cup (IF m.kind = "alert" /\ \E i \in Items :
                                   m.batch[i][2] > 0 /\ m.batch[i][2] \in recv[m.batch[i][1]]
                              THEN {"repeat"} ELSE {})
                        \cup (IF m.kind = "digest" /\ digests[m.date] >= 1 THEN {"digest2"} ELSE {})
                        \cup (IF m.kind = "digest" /\ lastDigestObs = m.obs THEN {"silent"} ELSE {})
                        \cup (IF m.kind = "alert" /\ lastObs = m.obs THEN {"silent"} ELSE {})
             ELSE UNCHANGED <<recv, digests, lastDigestObs, lastObs, bad>>
          /\ CASE outcome = "delivered" ->
                    /\ run' = [run EXCEPT ![a].phase = "sent"]
                    /\ faults' = faults
                    /\ seen' = seen1
               [] outcome = "lost" ->
                    /\ run' = [run EXCEPT ![a] = IdleRun]
                    /\ faults' = faults + 1
                    /\ seen' = seen1 \cup {"gap"}
               [] outcome = "down" ->
                    /\ run' = [run EXCEPT ![a] = IdleRun]
                    /\ faults' = faults + 1
                    /\ seen' = seen1
    /\ UNCHANGED <<t, src, edits, runs, file, up, nodeStopped, since>>

Save(a) ==
    /\ up[a] /\ run[a].phase = "sent"
    /\ LET r == run[a]
           m == r.msg
       IN /\ file' = Ack(r.snap, m)
          /\ bad' = bad \cup (IF Key(m) \notin server THEN {"ack"} ELSE {}) \cup Skip(r)
    /\ run' = [run EXCEPT ![a] = IdleRun]
    /\ seen' = IF "gap" \in seen THEN seen \cup {"resumed"} ELSE seen
    /\ UNCHANGED <<t, src, edits, faults, runs, up, nodeStopped, server, recv, digests,
                   lastDigestObs, lastObs, since>>

SkipSave(a) ==
    /\ up[a] /\ run[a].phase = "skipsave"
    /\ file' = [run[a].snap EXCEPT !.dd = run[a].date + 1]
    /\ run' = [run EXCEPT ![a] = IdleRun]
    /\ UNCHANGED <<t, src, edits, faults, runs, up, nodeStopped, server, recv, digests,
                   lastDigestObs, lastObs, since, bad, seen>>

Crash(a) ==
    /\ up[a] /\ run[a].phase # "idle" /\ faults < MaxFaults
    /\ seen' = IF run[a].phase = "sent" THEN seen \cup {"gap"} ELSE seen
    /\ faults' = faults + 1
    /\ run' = [run EXCEPT ![a] = IdleRun]
    /\ UNCHANGED <<t, src, edits, runs, file, up, nodeStopped, server, recv, digests,
                   lastDigestObs, lastObs, since, bad>>

Next ==
    \/ Tick
    \/ \E o \in Offers, p \in 0..2 : Edit(o, p)
    \/ StopNode \/ StartRust
    \/ \E a \in Actors :
          \/ \E kind \in {"digest", "alert"} : Load(a, kind)
          \/ \E mask \in 0..3 : Poll(a, mask)
          \/ Decide(a) \/ Claim(a) \/ Save(a) \/ SkipSave(a) \/ Crash(a)
          \/ \E outcome \in {"delivered", "lost", "down"} : Send(a, outcome)

Spec == Init /\ [][Next]_vars

(* ---- invariants (Stateright: no_repeat, one_digest_per_day, silent_when_unchanged,
   no_skip, acked_only_after_admission) ---- *)
NoRepeat == "repeat" \notin bad
OneDigestPerDay == "digest2" \notin bad
SilentWhenUnchanged == "silent" \notin bad
NoSkip == "skip" \notin bad
AckedOnlyAfterAdmission == "ack" \notin bad

(* ---- witnesses: each must be violated (Stateright `sometimes`) ---- *)
NeverNewAlert == "newAlert" \notin seen
NeverRestartContinues == "resumed" \notin seen
NeverCutover == "cutover" \notin seen
NeverDigestRelists == "relist" \notin seen
=============================================================================
