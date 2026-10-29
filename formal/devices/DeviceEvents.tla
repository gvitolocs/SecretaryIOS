----------------------------- MODULE DeviceEvents -----------------------------
(***************************************************************************)
(* Device-event delivery: the phone's outbox, secretary-server             *)
(* POST /v1/device/events (device.rs::events, the SeenIds LRU) and         *)
(* Lucina's DeviceEventStore::ingest (lucina-store device.rs), branch      *)
(* lucina-rs. The phone POSTs everything not yet acknowledged, may retry   *)
(* while a request is in flight, and loses responses; secretary-server     *)
(* drops (device, id) pairs in its LRU, forwards one event per chunk and   *)
(* remembers a chunk once Lucina accepted it; a restart empties the LRU    *)
(* and kills the requests in flight. Lucina dedupes by event id alone and  *)
(* appends; an append can fail (before fix E1 the ids stayed marked).      *)
(* TLA+ twin of crates/secretary-server/tests/device_events_model.rs     *)
(* (the same state: 4 875 distinct states for the current code).         *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS TwoDevices, MaxInflight, MaxSends, Cap, MaxRestarts, MaxFails, MaxLost,
          E1Fixed, LucinaDedupe

N == 2
Evs == 1..N
\* The event id of each event (device 0 sends 1 and 2, or device 0 and device 1 send id 0).
Id(k) == IF TwoDevices THEN 0 ELSE k - 1

VARIABLES pending, acked, reqs, sends, seen, restarts, fails, lost, lucinaIds, stored,
          serverDup, lucinaDup, retriedAfterFail
vars == <<pending, acked, reqs, sends, seen, restarts, fails, lost, lucinaIds, stored,
          serverDup, lucinaDup, retriedAfterFail>>

Init == /\ pending = Evs /\ acked = {} /\ reqs = <<>> /\ sends = 0 /\ seen = <<>>
        /\ restarts = 0 /\ fails = 0 /\ lost = 0 /\ lucinaIds = {} /\ stored = [k \in Evs |-> 0]
        /\ serverDup = FALSE /\ lucinaDup = FALSE /\ retriedAfterFail = FALSE

Remove(q, i) == [j \in 1..(Len(q) - 1) |-> IF j < i THEN q[j] ELSE q[j + 1]]
Min(S) == CHOOSE x \in S : \A y \in S : x <= y
InSeq(s, x) == \E j \in 1..Len(s) : s[j] = x
\* SeenIds::insert / a `contains` hit: most recent last, evict the least recent.
Touch(s, k) == LET t == SelectSeq(s, LAMBDA x : x # k) \o <<k>>
               IN IF Len(t) > Cap THEN SubSeq(t, 2, Len(t)) ELSE t
Inflight == Cardinality({i \in 1..Len(reqs) : reqs[i].st # "done"})

Send == /\ pending # {} /\ Inflight < MaxInflight /\ sends < MaxSends
        /\ reqs' = Append(reqs, [batch |-> pending, st |-> "filter", rest |-> {}, ok |-> FALSE])
        /\ sends' = sends + 1
        /\ retriedAfterFail' = (retriedAfterFail \/ fails > 0)
        /\ UNCHANGED <<pending, acked, seen, restarts, fails, lost, lucinaIds, stored, serverDup, lucinaDup>>

\* The retain over the batch, in event order (two events).
Step(st, k, b) == IF k \notin b THEN st
                  ELSE IF InSeq(st.seen, k) THEN [st EXCEPT !.seen = Touch(st.seen, k), !.dup = TRUE]
                  ELSE [st EXCEPT !.fresh = @ \cup {k}]
Filter(i) ==
    /\ reqs[i].st = "filter"
    /\ LET r0 == [seen |-> seen, fresh |-> {}, dup |-> FALSE]
           r2 == Step(Step(r0, 1, reqs[i].batch), 2, reqs[i].batch)
       IN /\ seen' = r2.seen
          /\ serverDup' = (serverDup \/ r2.dup)
          /\ reqs' = [reqs EXCEPT ![i] = [@ EXCEPT !.st = "forward", !.rest = r2.fresh]]
    /\ UNCHANGED <<pending, acked, sends, restarts, fails, lost, lucinaIds, stored, lucinaDup, retriedAfterFail>>

LucinaOk(i) ==
    /\ reqs[i].st = "forward" /\ reqs[i].rest # {}
    /\ LET k == Min(reqs[i].rest)
       IN /\ IF LucinaDedupe /\ Id(k) \in lucinaIds
             THEN lucinaDup' = TRUE /\ UNCHANGED <<lucinaIds, stored>>
             ELSE /\ lucinaIds' = lucinaIds \cup {Id(k)}
                  /\ stored' = [stored EXCEPT ![k] = @ + 1]
                  /\ UNCHANGED lucinaDup
          /\ seen' = Touch(seen, k)
          /\ reqs' = [reqs EXCEPT ![i] = [@ EXCEPT !.rest = @ \ {k}]]
    /\ UNCHANGED <<pending, acked, sends, restarts, fails, lost, serverDup, retriedAfterFail>>

LucinaFail(i) ==
    /\ reqs[i].st = "forward" /\ reqs[i].rest # {} /\ fails < MaxFails
    /\ LET k == Min(reqs[i].rest)
       IN lucinaIds' = IF ~E1Fixed /\ LucinaDedupe THEN lucinaIds \cup {Id(k)} ELSE lucinaIds
    /\ fails' = fails + 1
    /\ reqs' = [reqs EXCEPT ![i] = [@ EXCEPT !.st = "done", !.rest = {}, !.ok = FALSE]]
    /\ UNCHANGED <<pending, acked, sends, seen, restarts, lost, stored, serverDup, lucinaDup, retriedAfterFail>>

Answerable(i) == (reqs[i].st = "forward" /\ reqs[i].rest = {}) \/ reqs[i].st = "done"
OkOf(i) == IF reqs[i].st = "forward" THEN TRUE ELSE reqs[i].ok

Respond(i) ==
    /\ Answerable(i)
    /\ reqs' = Remove(reqs, i)
    /\ IF OkOf(i) THEN /\ pending' = pending \ reqs[i].batch
                       /\ acked' = acked \cup reqs[i].batch
                  ELSE UNCHANGED <<pending, acked>>
    /\ UNCHANGED <<sends, seen, restarts, fails, lost, lucinaIds, stored, serverDup, lucinaDup, retriedAfterFail>>

Lose(i) ==
    /\ Answerable(i) /\ lost < MaxLost
    /\ reqs' = Remove(reqs, i) /\ lost' = lost + 1
    /\ UNCHANGED <<pending, acked, sends, seen, restarts, fails, lucinaIds, stored, serverDup, lucinaDup, retriedAfterFail>>

Restart ==
    /\ restarts < MaxRestarts
    /\ restarts' = restarts + 1 /\ seen' = <<>>
    /\ reqs' = [i \in 1..Len(reqs) |->
                  IF reqs[i].st = "done" THEN reqs[i] ELSE [reqs[i] EXCEPT !.st = "done", !.rest = {}, !.ok = FALSE]]
    /\ UNCHANGED <<pending, acked, sends, fails, lost, lucinaIds, stored, serverDup, lucinaDup, retriedAfterFail>>

Next == Send \/ Restart
        \/ \E i \in 1..Len(reqs) : Filter(i) \/ LucinaOk(i) \/ LucinaFail(i) \/ Respond(i) \/ Lose(i)
Spec == Init /\ [][Next]_vars

StoredAtMostOnce == \A k \in Evs : stored[k] <= 1
NoSilentLoss == \A k \in acked : stored[k] >= 1

\* Witnesses (MUST FAIL: the interesting states are reached).
NeverAllDelivered == ~(acked = Evs /\ reqs = <<>>)
NeverServerDup == ~serverDup
NeverLucinaDup == ~lucinaDup
NeverRetryAfterFail == ~retriedAfterFail
=============================================================================
