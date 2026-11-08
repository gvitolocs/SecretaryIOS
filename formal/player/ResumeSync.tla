----------------------------- MODULE ResumeSync -----------------------------
(***************************************************************************)
(* Resume positions of one film across devices (iPhone, web, Mac) and the  *)
(* server. TLA+ twin of verification/models/src/player.rs.                 *)
(*                                                                         *)
(*  device  Play     a new position, stamped with the device clock (fix    *)
(*                   R2: after the entry the device holds)                 *)
(*          Push     POST /v1/media/progress (one request at a time)       *)
(*          Pull     GET  /v1/media/progress                               *)
(*          Offline  its messages in flight are lost                       *)
(*  server  Store::put: keep the newer entry, acknowledge the submitted    *)
(*          stamp, send back the stored entry when it is newer             *)
(*  device  Book::acknowledge (clear dirty unless changed after the acked  *)
(*          write), Book::merge (adopt only a newer entry)                 *)
(* "Newer" is (updated_at, device) with fix R1, updated_at alone before.   *)
(***************************************************************************)
EXTENDS Naturals, FiniteSets

CONSTANTS N,               \* devices
          Writes, Ticks, Outages,
          TieBreak,        \* fix R1
          StampAfterHeld,  \* fix R2
          ServerLww, MergeLww, AckChecksTime

Devs == 0..(N - 1)
NoW == [pos |-> 0, at |-> 0, dev |-> 0]        \* "no entry"

VARIABLES now, local, dirty, online, pushing, seen, server, version, net,
          latest, writes, ticks, outages, nextPos,
          staleAdopted, serverRegressed, bothDirty, hadOutage
vars == <<now, local, dirty, online, pushing, seen, server, version, net,
          latest, writes, ticks, outages, nextPos,
          staleAdopted, serverRegressed, bothDirty, hadOutage>>

Later(a, b) == a.at > b.at \/ (a.at = b.at /\ a.dev > b.dev)
CodeNewer(a, b) == IF TieBreak THEN Later(a, b) ELSE a.at > b.at

Init == /\ now = 1
        /\ local = [d \in Devs |-> NoW] /\ dirty = [d \in Devs |-> FALSE]
        /\ online = [d \in Devs |-> TRUE] /\ pushing = [d \in Devs |-> FALSE]
        /\ seen = [d \in Devs |-> 0]
        /\ server = NoW /\ version = 0 /\ net = {}
        /\ latest = NoW /\ writes = Writes /\ ticks = Ticks /\ outages = Outages /\ nextPos = 1
        /\ staleAdopted = FALSE /\ serverRegressed = FALSE /\ bothDirty = FALSE /\ hadOutage = FALSE

Tick == /\ ticks > 0 /\ now' = now + 1 /\ ticks' = ticks - 1
        /\ UNCHANGED <<local, dirty, online, pushing, seen, server, version, net, latest, writes,
                       outages, nextPos, staleAdopted, serverRegressed, bothDirty, hadOutage>>

Play(d) ==
    /\ writes > 0
    /\ StampAfterHeld \/ local[d] = NoW \/ local[d].dev # d \/ local[d].at < now
    /\ LET at == IF StampAfterHeld /\ local[d] # NoW /\ local[d].at >= now THEN local[d].at + 1 ELSE now
           w  == [pos |-> nextPos, at |-> at, dev |-> d]
           dirty2 == [dirty EXCEPT ![d] = TRUE]
       IN /\ local' = [local EXCEPT ![d] = w]
          /\ dirty' = dirty2
          /\ latest' = IF latest = NoW \/ Later(w, latest) THEN w ELSE latest
          /\ bothDirty' = (bothDirty \/ Cardinality({x \in Devs : dirty2[x]}) >= 2)
    /\ nextPos' = nextPos + 1 /\ writes' = writes - 1
    /\ UNCHANGED <<now, online, pushing, seen, server, version, net, ticks, outages,
                   staleAdopted, serverRegressed, hadOutage>>

Offline(d) ==
    /\ online[d] /\ outages > 0
    /\ outages' = outages - 1
    /\ online' = [online EXCEPT ![d] = FALSE]
    /\ pushing' = [pushing EXCEPT ![d] = FALSE]
    /\ hadOutage' = (hadOutage \/ dirty[d])
    /\ net' = {m \in net : m.dev # d}          \* its requests and the replies to it are lost
    /\ UNCHANGED <<now, local, dirty, seen, server, version, latest, writes, ticks, nextPos,
                   staleAdopted, serverRegressed, bothDirty>>

Online(d) ==
    /\ ~online[d] /\ online' = [online EXCEPT ![d] = TRUE]
    /\ UNCHANGED <<now, local, dirty, pushing, seen, server, version, net, latest, writes, ticks,
                   outages, nextPos, staleAdopted, serverRegressed, bothDirty, hadOutage>>

Push(d) ==
    /\ online[d] /\ dirty[d] /\ ~pushing[d]
    /\ pushing' = [pushing EXCEPT ![d] = TRUE]
    /\ net' = net \cup {[kind |-> "push", dev |-> d, w |-> local[d], acked |-> 0, newer |-> NoW, ver |-> 0]}
    /\ UNCHANGED <<now, local, dirty, online, seen, server, version, latest, writes, ticks,
                   outages, nextPos, staleAdopted, serverRegressed, bothDirty, hadOutage>>

Pull(d) ==
    /\ online[d]
    /\ ~\E m \in net : m.kind = "pull" /\ m.dev = d
    /\ net' = net \cup {[kind |-> "pull", dev |-> d, w |-> server, acked |-> 0, newer |-> NoW, ver |-> version]}
    /\ UNCHANGED <<now, local, dirty, online, pushing, seen, server, version, latest, writes, ticks,
                   outages, nextPos, staleAdopted, serverRegressed, bothDirty, hadOutage>>

\* Store::put
ServerRecv(m) ==
    LET w == m.w
        store == server = NoW \/ ~ServerLww \/ CodeNewer(w, server)
        newer == IF server # NoW /\ ServerLww /\ CodeNewer(server, w) THEN server ELSE NoW
        changes == store /\ server # w
    IN /\ server' = IF changes THEN w ELSE server
       /\ version' = IF changes THEN version + 1 ELSE version
       /\ serverRegressed' = (serverRegressed \/ (changes /\ server # NoW /\ Later(server, w)))
       /\ net' = (net \ {m}) \cup {[kind |-> "reply", dev |-> m.dev, w |-> NoW, acked |-> w.at, newer |-> newer, ver |-> 0]}
       /\ UNCHANGED <<now, local, dirty, online, pushing, seen, latest, writes, ticks, outages,
                      nextPos, staleAdopted, bothDirty, hadOutage>>

\* Book::merge for one entry, after `dirtyIn` (the acknowledgement's effect)
AdoptVals(d, w, dirtyIn) ==
    LET take == local[d] = NoW \/ ~MergeLww \/ CodeNewer(w, local[d])
    IN IF w # NoW /\ take
       THEN <<w, FALSE, local[d] # NoW /\ Later(local[d], w)>>
       ELSE <<local[d], dirtyIn, FALSE>>

\* Book::acknowledge, then Book::merge of the newer entry
DeviceReply(m) ==
    LET d == m.dev
        acked == ~AckChecksTime \/ (local[d] # NoW /\ local[d].at <= m.acked)
        r == AdoptVals(d, m.newer, IF acked THEN FALSE ELSE dirty[d])
    IN /\ local' = [local EXCEPT ![d] = r[1]]
       /\ dirty' = [dirty EXCEPT ![d] = r[2]]
       /\ staleAdopted' = (staleAdopted \/ r[3])
       /\ pushing' = [pushing EXCEPT ![d] = FALSE]
       /\ net' = net \ {m}
       /\ UNCHANGED <<now, online, seen, server, version, latest, writes, ticks, outages,
                      nextPos, serverRegressed, bothDirty, hadOutage>>

DevicePull(m) ==
    LET d == m.dev
        r == AdoptVals(d, m.w, dirty[d])
    IN /\ seen' = [seen EXCEPT ![d] = m.ver]
       /\ local' = [local EXCEPT ![d] = r[1]]
       /\ dirty' = [dirty EXCEPT ![d] = r[2]]
       /\ staleAdopted' = (staleAdopted \/ r[3])
       /\ net' = net \ {m}
       /\ UNCHANGED <<now, online, pushing, server, version, latest, writes, ticks, outages,
                      nextPos, serverRegressed, bothDirty, hadOutage>>

Deliver(m) == \/ m.kind = "push" /\ ServerRecv(m)
              \/ m.kind = "reply" /\ DeviceReply(m)
              \/ m.kind = "pull" /\ DevicePull(m)

Next == \/ Tick
        \/ \E d \in Devs : Play(d) \/ Offline(d) \/ Online(d) \/ Push(d) \/ Pull(d)
        \/ \E m \in net : Deliver(m)

Spec == Init /\ [][Next]_vars

-----------------------------------------------------------------------------
Quiescent == net = {} /\ \A d \in Devs : online[d] /\ ~dirty[d]

NoPositionLost == Quiescent => server = latest
DevicesConverge == Quiescent => \A d \in Devs : seen[d] # version \/ version = 0 \/ local[d] = server
NeverBackToStale == ~staleAdopted
ServerNeverRegresses == ~serverRegressed

\* Witnesses (checked as invariants that must be violated)
NeverConcurrent == ~bothDirty
NoOfflineGapResolved ==
    ~(/\ hadOutage /\ Quiescent /\ server = latest
      /\ Cardinality({d \in Devs : local[d] # NoW}) >= 2
      /\ \A d \in Devs : seen[d] = version /\ local[d] = server)
=============================================================================
