----------------------------- MODULE Telegram -----------------------------
EXTENDS Integers, FiniteSets
\* Twin of verification/models/src/retirement.rs::Telegram (R3). Processes 0, 1 = Rust A, B
\* (same instance "lucina-rs": each acquires over the other's live record, only the flock
\* separates them), 2 = the Node container ("lucina-nez"). pc: 0 idle, 1 read the record
\* (Stall only), 2 wrote its record and waits 250 ms, 3 polling. age 3 = older than 90 s.
CONSTANTS Flock, Stall, Lapse, NodeDegraded, RustDegraded
VARIABLE s
vars == <<s>>
Ids == 0..2
Rust == 0..1
Init == s=[pc |-> [i \in Ids |-> 0],up |-> [i \in Rust |-> TRUE],holder |-> -1,record |-> -1,
 age |-> 0,crash |-> FALSE,ticks |-> 0,dir |-> TRUE,conflict |-> FALSE,deferred |-> FALSE,
 takeover |-> FALSE,refused |-> FALSE]
Up(i) == IF i=2 THEN TRUE ELSE s.up[i]
Live == s.record#-1 /\ s.age<3
\* lease_decision == Defer (Rust) / !acquired (Node acquireGatewayLock)
Defers(i) == Live /\ IF s.record=2 THEN i<2 ELSE i=2
Polling == {i \in Ids:s.pc[i]=3}
Stop(t,i) == [t EXCEPT !.pc[i]=0,!.holder=IF @=i THEN -1 ELSE @]
Check(i) == /\ Up(i) /\ s.pc[i]=0
 /\ IF ~s.dir
    THEN IF (i=2 /\ NodeDegraded) \/ (i<2 /\ RustDegraded)
         THEN s'=[s EXCEPT !.pc[i]=3]
         ELSE /\ i<2 /\ ~s.refused /\ s'=[s EXCEPT !.refused=TRUE]
    ELSE /\ (i=2 \/ ~Flock \/ s.holder \in {-1,i})
         /\ IF Defers(i)
            THEN /\ i=2 /\ ~s.deferred /\ s'=[s EXCEPT !.deferred=TRUE]
            ELSE s'=[s EXCEPT !.takeover=@ \/ (i<2 /\ Live),
                    !.holder=IF i<2 /\ Flock THEN i ELSE @,
                    !.pc[i]=IF Stall THEN 1 ELSE 2,
                    !.record=IF Stall THEN @ ELSE i,
                    !.age=IF Stall THEN @ ELSE 0]
Write(i) == /\ Up(i) /\ s.pc[i]=1
 /\ IF ~s.dir
    THEN IF i=2 /\ NodeDegraded THEN s'=[s EXCEPT !.pc[2]=3] ELSE s'=Stop(s,i)
    ELSE s'=[s EXCEPT !.record=i,!.age=0,!.pc[i]=2]
ReadBack(i) == /\ Up(i) /\ s.pc[i]=2
 /\ IF s.dir /\ s.record=i
    THEN s'=[s EXCEPT !.pc[i]=3]
    ELSE s'=[Stop(s,i) EXCEPT !.deferred=@ \/ i=2]
Heartbeat(i) == /\ Up(i) /\ s.pc[i]=3
 /\ IF ~s.dir
    THEN /\ i<2 /\ s'=Stop(s,i)
    ELSE IF Live /\ s.record#i THEN s'=Stop(s,i) ELSE s'=[s EXCEPT !.record=i,!.age=0]
Active == s.record#-1 /\ s.pc[s.record]>=2 /\ (s.record=2 \/ s.up[s.record])
Tick == /\ s.ticks<3
 /\ s'=[s EXCEPT !.ticks=@+1,!.age=IF Lapse \/ ~Active THEN (IF @<3 THEN @+1 ELSE 3) ELSE @]
Crash(i) == /\ s.up[i] /\ ~s.crash /\ s'=[Stop(s,i) EXCEPT !.up[i]=FALSE,!.crash=TRUE]
Restart(i) == /\ ~s.up[i] /\ s'=[s EXCEPT !.up[i]=TRUE]
\* Telegram's 409: the overlap existed before it.
Conflict(i) == /\ s.pc[i]=3 /\ Cardinality(Polling)>1 /\ ~s.conflict
 /\ s'=[Stop(s,i) EXCEPT !.conflict=TRUE]
BreakDir == /\ s.dir /\ s'=[s EXCEPT !.dir=FALSE]
Next == Tick \/ BreakDir
 \/ (\E i \in Ids:Check(i) \/ Write(i) \/ ReadBack(i) \/ Heartbeat(i) \/ Conflict(i))
 \/ (\E i \in Rust:Crash(i) \/ Restart(i))
SinglePoller == Cardinality(Polling)<=1
NeverRustPolls == s.pc[0]#3 /\ s.pc[1]#3
NeverNodeDeferred == ~s.deferred
NeverTakeover == ~(s.takeover /\ (s.pc[0]=3 \/ s.pc[1]=3))
NeverRefused == ~s.refused
NeverNodePollsAfterRust == ~(s.pc[2]=3 /\ s.crash)
Never409 == ~s.conflict
Spec == Init /\ [][Next]_vars
=============================================================================
