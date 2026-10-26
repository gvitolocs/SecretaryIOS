------------------------------- MODULE InkSync -------------------------------
(***************************************************************************)
(* Lucina Ink device sync (crates/ink-sync, POST /v1/library/sync).       *)
(* TLA+ twin of verification/models/src/ink_sync.rs: two devices with a   *)
(* durable outbox and durable acks, one server with a per-device          *)
(* watermark (contiguous + sparse, bounded), a network bag that loses,    *)
(* duplicates and reorders messages, device crashes, an owner edit.       *)
(* The client may pipeline two single-op requests (a superset of the real *)
(* client, which sends one batch at a time, oldest first).                *)
(*                                                                         *)
(* Server registers: progress last-writer-wins by (at, device, counter);  *)
(* status by (completed, stamp), frozen by an owner `completed`;          *)
(* highlights a grow-only set; deletes are tombstones keyed by op id.     *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, Sequences

CONSTANTS Scenario,     \* "status" | "highlights"
          Drops, Dups, Crashes, SpuriousTimeouts,
          OwnerActs,    \* the owner may mark the book completed
          Gate,         \* the op-id gate (watermark) before apply
          DurableAcks,  \* acks are appended to the outbox
          StatusRules   \* completed sticky + owner freeze

D == {1, 2}
Pipeline == 2
MaxSparse == 1
Scripts ==
    CASE Scenario = "status" ->
            << << [at |-> 2, k |-> "status", v |-> "completed"], [at |-> 3, k |-> "progress", v |-> 1] >>,
               << [at |-> 4, k |-> "status", v |-> "reading"],   [at |-> 1, k |-> "progress", v |-> 2] >> >>
      [] Scenario = "highlights" ->
            << << [at |-> 1, k |-> "highlight", v |-> 0], [at |-> 2, k |-> "delete", v |-> 0] >>,
               << [at |-> 1, k |-> "highlight", v |-> 0], [at |-> 3, k |-> "highlight", v |-> 1] >> >>
N(d) == Len(Scripts[d])
Op(d, c) == Scripts[d][c]
Ids == {<<d, c>> : d \in D, c \in 1..2}
Msgs == {[t |-> "req", d |-> d, c |-> c, ok |-> TRUE] : d \in D, c \in 1..2}
        \cup {[t |-> "resp", d |-> d, c |-> c, ok |-> b] : d \in D, c \in 1..2, b \in BOOLEAN}

NoProgress == [set |-> FALSE, st |-> <<0, 0, 0>>, v |-> 0]
NoStatus == [set |-> FALSE, done |-> FALSE, st |-> <<0, 0, 0>>, v |-> "none"]

VARIABLES contig, sparse, progress, status, ownerCompleted, highlights, tombstones, effects,
          recorded, acked, inflight, net, drops, dups, crashes, spurious,
          everAcked, resentAcked, ownerDone, statusAtOwner, everCompleted
server == <<contig, sparse, progress, status, ownerCompleted, highlights, tombstones, effects>>
device == <<recorded, acked, inflight>>
budget == <<drops, dups, crashes, spurious>>
ghost == <<everAcked, resentAcked, ownerDone, statusAtOwner, everCompleted>>
vars == <<server, device, net, budget, ghost>>

Stamp(d, c) == <<Op(d, c).at, d, c>>
StampLt(a, b) == \/ a[1] < b[1]
                 \/ a[1] = b[1] /\ a[2] < b[2]
                 \/ a[1] = b[1] /\ a[2] = b[2] /\ a[3] < b[3]
KeyLt(a, b) == (~a.done /\ b.done) \/ (a.done = b.done /\ StampLt(a.st, b.st))

WmHas(d, c) == c <= contig[d] \/ c \in sparse[d]
RECURSIVE Absorb(_, _)
Absorb(k, S) == IF (k + 1) \in S THEN Absorb(k + 1, S \ {k + 1}) ELSE <<k, S>>
Insert(d, c) == IF WmHas(d, c) THEN <<contig[d], sparse[d]>>
                ELSE IF c = contig[d] + 1 THEN Absorb(c, sparse[d])
                ELSE <<contig[d], sparse[d] \cup {c}>>
Deferred(d, c) == ~WmHas(d, c) /\ c # contig[d] + 1 /\ Cardinality(sparse[d]) >= MaxSparse

NewProgress(d, c) ==
    IF Op(d, c).k # "progress" THEN progress
    ELSE IF progress.set /\ StampLt(Stamp(d, c), progress.st) THEN progress
    ELSE [set |-> TRUE, st |-> Stamp(d, c), v |-> Op(d, c).v]
NewStatus(d, c) ==
    LET new == [set |-> TRUE, done |-> StatusRules /\ Op(d, c).v = "completed",
                st |-> Stamp(d, c), v |-> Op(d, c).v]
    IN IF Op(d, c).k # "status" THEN status
       ELSE IF StatusRules /\ ownerCompleted THEN status
       ELSE IF status.set /\ KeyLt(new, status) THEN status
       ELSE new

Outstanding(d, c) == \E m \in Msgs : m.d = d /\ m.c = c /\ net[m] > 0
Unsent(d) == {c \in 1..recorded[d] : c \notin acked[d] /\ c \notin inflight[d]}

Init == /\ contig = [d \in D |-> 0]
        /\ sparse = [d \in D |-> {}]
        /\ progress = NoProgress
        /\ status = NoStatus
        /\ ownerCompleted = FALSE
        /\ highlights = {}
        /\ tombstones = {}
        /\ effects = [i \in Ids |-> 0]
        /\ recorded = [d \in D |-> 0]
        /\ acked = [d \in D |-> {}]
        /\ inflight = [d \in D |-> {}]
        /\ net = [m \in Msgs |-> 0]
        /\ drops = Drops /\ dups = Dups /\ crashes = Crashes /\ spurious = SpuriousTimeouts
        /\ everAcked = {} /\ resentAcked = FALSE /\ ownerDone = FALSE
        /\ statusAtOwner = NoStatus /\ everCompleted = FALSE

(* Outbox::record: append + fsync, before any network I/O. *)
Record(d) == /\ recorded[d] < N(d)
             /\ recorded' = [recorded EXCEPT ![d] = @ + 1]
             /\ UNCHANGED <<server, acked, inflight, net, budget, ghost>>

(* ClientMachine::next / Syncer::step: the oldest unacked op not in flight. *)
Send(d) == LET cs == Unsent(d)
               c == CHOOSE x \in cs : \A y \in cs : x <= y
               req == [t |-> "req", d |-> d, c |-> c, ok |-> TRUE]
           IN /\ cs # {}
              /\ Cardinality(inflight[d]) < Pipeline
              /\ resentAcked' = (resentAcked \/ <<d, c>> \in everAcked)
              /\ inflight' = [inflight EXCEPT ![d] = @ \cup {c}]
              /\ net' = [net EXCEPT ![req] = @ + 1]
              /\ UNCHANGED <<server, recorded, acked, budget, everAcked, ownerDone,
                             statusAtOwner, everCompleted>>

(* TransportError::Network: retry with the same op id. A timeout fires once *)
(* nothing is left to arrive; a spurious one (still in the network) within   *)
(* its budget.                                                                *)
Timeout(d, c) == /\ c \in inflight[d]
                 /\ ~Outstanding(d, c)
                 /\ inflight' = [inflight EXCEPT ![d] = @ \ {c}]
                 /\ UNCHANGED <<server, recorded, acked, net, budget, ghost>>
SpuriousTimeout(d, c) == /\ c \in inflight[d]
                         /\ Outstanding(d, c)
                         /\ spurious > 0
                         /\ spurious' = spurious - 1
                         /\ inflight' = [inflight EXCEPT ![d] = @ \ {c}]
                         /\ UNCHANGED <<server, recorded, acked, net, drops, dups, crashes, ghost>>

(* Restart: volatile state lost; Outbox::open recovers ops and acks. *)
Crash(d) == /\ crashes > 0
            /\ crashes' = crashes - 1
            /\ inflight' = [inflight EXCEPT ![d] = {}]
            /\ acked' = IF DurableAcks THEN acked ELSE [acked EXCEPT ![d] = {}]
            /\ UNCHANGED <<server, recorded, net, drops, dups, spurious, ghost>>

(* serve -> apply_mut: the watermark gate, then the effect exactly once. *)
DeliverReq(m) ==
    LET d == m.d
        c == m.c
        dup == Gate /\ WmHas(d, c)
        def == ~dup /\ Gate /\ Deferred(d, c)
        go == ~dup /\ ~def
        ins == Insert(d, c)
        resp == [t |-> "resp", d |-> d, c |-> c, ok |-> ~def]
    IN /\ m.t = "req"
       /\ net[m] > 0
       /\ net' = [net EXCEPT ![m] = @ - 1, ![resp] = @ + 1]
       /\ contig' = IF go THEN [contig EXCEPT ![d] = ins[1]] ELSE contig
       /\ sparse' = IF go THEN [sparse EXCEPT ![d] = ins[2]] ELSE sparse
       /\ effects' = IF go THEN [effects EXCEPT ![<<d, c>>] = @ + 1] ELSE effects
       /\ progress' = IF go THEN NewProgress(d, c) ELSE progress
       /\ status' = IF go THEN NewStatus(d, c) ELSE status
       /\ highlights' = IF go /\ Op(d, c).k = "highlight" THEN highlights \cup {Op(d, c).v}
                        ELSE highlights
       /\ tombstones' = IF go /\ Op(d, c).k = "delete" THEN tombstones \cup {<<d, c>>}
                        ELSE tombstones
       /\ everCompleted' = (everCompleted \/ (status'.set /\ status'.v = "completed"))
       /\ UNCHANGED <<ownerCompleted, device, budget, everAcked, resentAcked, ownerDone,
                      statusAtOwner>>

(* Outbox::ack (append + fsync), ClientMachine::on_response. *)
DeliverResp(m) ==
    /\ m.t = "resp"
    /\ net[m] > 0
    /\ net' = [net EXCEPT ![m] = @ - 1]
    /\ inflight' = [inflight EXCEPT ![m.d] = @ \ {m.c}]
    /\ acked' = IF m.ok THEN [acked EXCEPT ![m.d] = @ \cup {m.c}] ELSE acked
    /\ everAcked' = IF m.ok THEN everAcked \cup {<<m.d, m.c>>} ELSE everAcked
    /\ UNCHANGED <<server, recorded, budget, resentAcked, ownerDone, statusAtOwner, everCompleted>>

Drop(m) == /\ drops > 0 /\ net[m] > 0
           /\ drops' = drops - 1
           /\ net' = [net EXCEPT ![m] = @ - 1]
           /\ UNCHANGED <<server, device, dups, crashes, spurious, ghost>>
Dup(m) == /\ dups > 0 /\ net[m] > 0
          /\ dups' = dups - 1
          /\ net' = [net EXCEPT ![m] = @ + 1]
          /\ UNCHANGED <<server, device, drops, crashes, spurious, ghost>>

(* PATCH status completed -> owner_set_status. *)
Owner == /\ OwnerActs /\ ~ownerDone
         /\ ownerDone' = TRUE
         /\ ownerCompleted' = TRUE
         /\ statusAtOwner' = status
         /\ UNCHANGED <<contig, sparse, progress, status, highlights, tombstones, effects,
                        device, net, budget, everAcked, resentAcked, everCompleted>>

Next == \/ \E d \in D : Record(d) \/ Send(d) \/ Crash(d)
        \/ \E d \in D, c \in 1..2 : Timeout(d, c) \/ SpuriousTimeout(d, c)
        \/ \E m \in Msgs : DeliverReq(m) \/ DeliverResp(m) \/ Drop(m) \/ Dup(m)
        \/ Owner

Fairness == /\ \A d \in D : WF_vars(Record(d)) /\ WF_vars(Send(d))
            /\ \A d \in D, c \in 1..2 : WF_vars(Timeout(d, c))
            /\ \A m \in Msgs : WF_vars(DeliverReq(m)) /\ WF_vars(DeliverResp(m))

Spec == Init /\ [][Next]_vars /\ Fairness

(* ---- properties (names as in the Stateright model) ---------------------- *)
Applied == {i \in Ids : i[2] <= N(i[1]) /\ WmHas(i[1], i[2])}
AppliedOf(k) == {i \in Applied : Op(i[1], i[2]).k = k}
AllApplied == \A d \in D : \A c \in 1..N(d) : WmHas(d, c)

AckedNeverLost == \A d \in D : \A c \in acked[d] : WmHas(d, c)
NeverResendAcked == ~resentAcked
AppliedAtMostOnce == \A i \in Ids : effects[i] <= 1
OwnerCompletedNeverReverted == ownerDone => status = statusAtOwner
DeviceCompletedSticky == everCompleted => (status.set /\ status.v = "completed")
HighlightsOnlyGrow == /\ {Op(i[1], i[2]).v : i \in AppliedOf("highlight")} \subseteq highlights
                      /\ AppliedOf("delete") \subseteq tombstones
(* Convergence: the server's content is a function of the set of applied   *)
(* ops (the max by stamp / by key, the union), whatever the delivery order. *)
ProgressConverges ==
    LET P == AppliedOf("progress") IN
    IF P = {} THEN ~progress.set
    ELSE \E i \in P : /\ progress.st = Stamp(i[1], i[2])
                      /\ progress.v = Op(i[1], i[2]).v
                      /\ \A j \in P : j = i \/ StampLt(Stamp(j[1], j[2]), progress.st)
StatusKey(i) == [done |-> StatusRules /\ Op(i[1], i[2]).v = "completed", st |-> Stamp(i[1], i[2])]
StatusConverges ==
    LET S == AppliedOf("status") IN
    (StatusRules /\ ownerCompleted)
    \/ (S = {} /\ ~status.set)
    \/ \E i \in S : /\ status.st = Stamp(i[1], i[2])
                    /\ \A j \in S : j = i \/ KeyLt(StatusKey(j), StatusKey(i))
Converges == /\ ProgressConverges /\ StatusConverges
             /\ highlights = {Op(i[1], i[2]).v : i \in AppliedOf("highlight")}
             /\ tombstones = AppliedOf("delete")
EventuallyAllApplied == <>AllApplied

(* Witnesses (cfgs that MUST FAIL): the interesting states are reachable. *)
NotAllSynced == ~(AllApplied /\ \A d \in D : acked[d] = 1..N(d)
                  /\ \A m \in Msgs : net[m] = 0)
NeverOutOfOrder == \A d \in D : sparse[d] = {}
=============================================================================
