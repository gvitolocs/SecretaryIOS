---------------------------- MODULE MediaBackup ----------------------------
(***************************************************************************)
(* Lucina media library: the iPhone photo backup against lucina-media     *)
(* behind p1. Twin of verification/models/src/media_backup.rs (same       *)
(* actors, steps and switches; read its header for the code mapping).     *)
(*                                                                         *)
(* Device 0 has photos 0 and 1, device 1 has photo 0 too (same bytes).    *)
(* Per photo: create, PUT from the offset the device last heard, finish.  *)
(* Responses can be lost and PUTs cut (the device resends from `known`),  *)
(* lucina-media can restart, p1 lets Limit requests per device through    *)
(* per window and answers 429 after that.                                 *)
(*                                                                         *)
(* Finish is three server steps (decide, insert, mark); a restart can     *)
(* fall between any two. The fixes, one switch each:                      *)
(*   Gone410: a finish whose file is gone answers 410 and the client       *)
(*     starts the asset again (before: 500 on every retry);                *)
(*   PauseOn429: the client waits for the window (before: retried at once);*)
(*   RecheckIngest (proposed, M3): the insert decides again under the      *)
(*     lock, so a concurrent finish of the same bytes is a duplicate.      *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS Gone410, PauseOn429, RecheckIngest, Limit, Ticks, Losses, Restarts, Budget

Size == 2
Photos == 0..1
D == {0, 1}
NoFinish == [up |-> 0, ok |-> FALSE, phase |-> "none"]

VARIABLES uploads, items, dev, ticks, losses, restarts, budget,
          resumed, rateLimited, dedupeHit, deadEnd, restartedAfter410

vars == <<uploads, items, dev, ticks, losses, restarts, budget,
          resumed, rateLimited, dedupeHit, deadEnd, restartedAfter410>>
ghosts == <<resumed, rateLimited, dedupeHit, deadEnd, restartedAfter410>>

Max(a, b) == IF a > b THEN a ELSE b

Device(q) == [queue |-> q, stage |-> "create", upload |-> 0, known |-> 0,
              paused |-> FALSE, sent |-> 0, rejected |-> 0, finishing |-> NoFinish]

Init ==
    /\ uploads = <<>>
    /\ items = <<>>
    /\ dev = [d \in D |-> IF d = 0 THEN Device(<<0, 1>>) ELSE Device(<<0>>)]
    /\ ticks = Ticks
    /\ losses = Losses
    /\ restarts = Restarts
    /\ budget = Budget
    /\ resumed = FALSE
    /\ rateLimited = FALSE
    /\ dedupeHit = FALSE
    /\ deadEnd = FALSE
    /\ restartedAfter410 = FALSE

Done(v) == [v EXCEPT !.queue = Tail(@), !.stage = "create", !.upload = 0, !.known = 0]

CanSend(d) ==
    /\ budget > 0
    /\ ~dev[d].paused
    /\ dev[d].queue # <<>>
    /\ dev[d].finishing = NoFinish

\* ingest_decision: 0 = unknown bytes (New), 1 = this device's asset, 2 = the other device's bytes.
Known(d, photo) ==
    IF \E j \in 1..Len(items) : items[j].photo = photo /\ items[j].dev = d THEN 1
    ELSE IF \E j \in 1..Len(items) : items[j].photo = photo THEN 2
    ELSE 0

\* p1 answers 429; the request never reaches lucina-media.
Reject429(d) ==
    /\ dev' = [dev EXCEPT ![d].sent = @ + 1, ![d].rejected = @ + 1,
                          ![d].paused = PauseOn429 \/ @]
    /\ rateLimited' = TRUE
    /\ UNCHANGED <<uploads, items, resumed, dedupeHit, deadEnd, restartedAfter410>>

ServeCreate(d, v, ok, photo) ==
    /\ uploads' = Append(uploads, [dev |-> d, photo |-> photo, received |-> 0,
                                   file |-> TRUE, finished |-> FALSE])
    /\ dev' = [dev EXCEPT ![d] = IF ok THEN [v EXCEPT !.upload = Len(uploads) + 1,
                                                      !.known = 0, !.stage = "put"]
                                       ELSE v]
    /\ UNCHANGED <<items, resumed, rateLimited, dedupeHit, deadEnd, restartedAfter410>>

\* protocol::accept_chunk (proved in verification/verus/media_upload.rs).
ServePut(d, v, k, n, ok) ==
    LET u == uploads[v.upload]
        len == IF k = "cut" THEN n ELSE Size - v.known
        end == v.known + len
        already == end <= u.received /\ len > 0
        recv == IF already THEN u.received ELSE Max(end, u.received)
    IN /\ Assert(~u.finished /\ end <= Size /\ v.known <= u.received, "chunk rejected")
       /\ uploads' = [uploads EXCEPT ![v.upload].received = recv]
       /\ resumed' = (resumed \/ already \/ u.received > v.known)
       /\ dev' = [dev EXCEPT ![d] = IF ok THEN [v EXCEPT !.known = recv,
                                                   !.stage = IF recv = Size THEN "finish" ELSE "put"]
                                          ELSE v]
       /\ UNCHANGED <<items, rateLimited, dedupeHit, deadEnd, restartedAfter410>>

ServeFinish(d, v, ok, photo) ==
    LET i == v.upload
        u == uploads[i]
        known == Known(d, photo)
    IN CASE u.finished ->
              \* Finish again (the first answer was lost): the same item.
              /\ dev' = [dev EXCEPT ![d] = IF ok THEN Done(v) ELSE v]
              /\ UNCHANGED <<uploads, items, ghosts>>
         [] ~u.file /\ Gone410 ->
              \* 410 "upload lost; start again".
              /\ dev' = [dev EXCEPT ![d] = IF ok THEN [v EXCEPT !.upload = 0, !.stage = "create"] ELSE v]
              /\ restartedAfter410' = (restartedAfter410 \/ ok)
              /\ UNCHANGED <<uploads, items, resumed, rateLimited, dedupeHit, deadEnd>>
         [] ~u.file ->
              \* 500: the next pass sends the same finish again.
              /\ dev' = [dev EXCEPT ![d] = v]
              /\ deadEnd' = TRUE
              /\ UNCHANGED <<uploads, items, resumed, rateLimited, dedupeHit, restartedAfter410>>
         [] u.received < Size ->
              /\ dev' = [dev EXCEPT ![d] = IF ok THEN [v EXCEPT !.known = u.received, !.stage = "put"]
                                                 ELSE v]
              /\ UNCHANGED <<uploads, items, ghosts>>
         [] OTHER ->
              \* check_finish passed: decide (a duplicate's spool file goes now).
              /\ uploads' = IF known # 0 THEN [uploads EXCEPT ![i].file = FALSE] ELSE uploads
              /\ dedupeHit' = (dedupeHit \/ known = 2)
              /\ dev' = [dev EXCEPT ![d] = [v EXCEPT !.finishing =
                            [up |-> i, ok |-> ok, phase |-> IF known # 0 THEN "mark" ELSE "insert"]]]
              /\ UNCHANGED <<items, resumed, rateLimited, deadEnd, restartedAfter410>>

Serve(d, k, n) ==
    LET v == [dev[d] EXCEPT !.sent = @ + 1]
        ok == k = "ok"
        photo == Head(v.queue)
    IN CASE v.stage = "create" -> ServeCreate(d, v, ok, photo)
         [] v.stage = "put" -> ServePut(d, v, k, n, ok)
         [] v.stage = "finish" -> ServeFinish(d, v, ok, photo)

\* k: "ok", "lost" (response lost), "cut" (a PUT cut after n bytes, no response).
Request(d, k, n) ==
    /\ CanSend(d)
    /\ k = "ok" \/ losses > 0
    /\ k = "cut" => (dev[d].stage = "put" /\ n \in 1..(Size - dev[d].known - 1))
    /\ budget' = budget - 1
    /\ losses' = IF k = "ok" THEN losses ELSE losses - 1
    /\ IF dev[d].sent + 1 > Limit THEN Reject429(d) ELSE Serve(d, k, n)
    /\ UNCHANGED <<ticks, restarts>>

\* store::place_new + insert_item (deciding again under the lock with RecheckIngest).
Insert(d) ==
    LET f == dev[d].finishing
        u == uploads[f.up]
        known == IF RecheckIngest THEN Known(d, u.photo) ELSE 0
    IN /\ f.phase = "insert"
       /\ items' = IF known = 0 THEN Append(items, [photo |-> u.photo, bytes |-> u.received, dev |-> d])
                   ELSE items
       /\ dedupeHit' = (dedupeHit \/ known = 2)
       /\ uploads' = [uploads EXCEPT ![f.up].file = FALSE]
       /\ dev' = [dev EXCEPT ![d].finishing.phase = "mark"]
       /\ UNCHANGED <<ticks, losses, restarts, budget, resumed, rateLimited, deadEnd, restartedAfter410>>

\* UPDATE uploads SET finished = 1; the response goes out.
Mark(d) ==
    LET f == dev[d].finishing
        v == [dev[d] EXCEPT !.finishing = NoFinish]
    IN /\ f.phase = "mark"
       /\ uploads' = [uploads EXCEPT ![f.up].finished = TRUE]
       /\ dev' = [dev EXCEPT ![d] = IF f.ok THEN Done(v) ELSE v]
       /\ UNCHANGED <<items, ticks, losses, restarts, budget, ghosts>>

\* A finish between its steps is lost; the device resends it.
Restart ==
    /\ restarts > 0
    /\ restarts' = restarts - 1
    /\ dev' = [d \in D |-> [dev[d] EXCEPT !.finishing = NoFinish]]
    /\ UNCHANGED <<uploads, items, ticks, losses, budget, ghosts>>

Tick ==
    /\ ticks > 0
    /\ \E d \in D : dev[d].sent > 0
    /\ ticks' = ticks - 1
    /\ dev' = [d \in D |-> [dev[d] EXCEPT !.sent = 0, !.rejected = 0, !.paused = FALSE]]
    /\ UNCHANGED <<uploads, items, losses, restarts, budget, ghosts>>

Next ==
    \/ \E d \in D, k \in {"ok", "lost"} : Request(d, k, 0)
    \/ \E d \in D, n \in 1..Size : Request(d, "cut", n)
    \/ \E d \in D : Insert(d) \/ Mark(d)
    \/ Restart
    \/ Tick

Spec == Init /\ [][Next]_vars

AllDone == \A d \in D : dev[d].queue = <<>>

StoredOnce == \A p \in Photos : Cardinality({j \in 1..Len(items) : items[j].photo = p}) <= 1
NoPartialObject == \A j \in 1..Len(items) : items[j].bytes = Size
NoDeadEndFinish == ~deadEnd
NoRequestStorm == \A d \in D : dev[d].sent <= Limit + 1
DoneMeansBackedUp == AllDone => \A p \in Photos : \E j \in 1..Len(items) : items[j].photo = p

\* Witnesses (MUST FAIL): the interesting states are reached.
NeverResumed == ~resumed
NeverRateLimited == ~rateLimited
NeverDedupe == ~dedupeHit
NeverRestartedAfter410 == ~restartedAfter410
NeverAllBackedUp == ~AllDone
=============================================================================
