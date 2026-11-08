----------------------------- MODULE Updates -----------------------------
EXTENDS Integers
\* Twin of verification/models/src/retirement.rs::Updates (R4). Telegram queues updates 1, 2;
\* confirmed = lowest unconfirmed id. Node (telegraf): 0 long-polling, 1 handling, 2 handled
\* and offset not synced, 3 gone, 4 back after rollback. Rust: 0 not started, 1 up, 2 crashed,
\* 3 stopped. offset 0 = no offset parameter; claimed 0 = no claim file.
CONSTANTS Durable, StartFromClaim, NodeIdleStop, Rollback
VARIABLE s
vars == <<s>>
Max(a,b) == IF a>b THEN a ELSE b
Init == s=[confirmed |-> 1,handled |-> <<0,0>>,logged |-> <<FALSE,FALSE>>,node |-> 0,ncur |-> 0,
 rust |-> 0,phase |-> 0,current |-> 0,offset |-> 0,claimed |-> 0,crash |-> FALSE,
 nodeHandled |-> FALSE,rustHandled |-> FALSE]
StartOffset(t) == IF Durable /\ StartFromClaim /\ t.claimed>0 THEN t.claimed+1 ELSE 0
NodeReceive == /\ s.node=0 /\ s.confirmed<=2 /\ s'=[s EXCEPT !.ncur=s.confirmed,!.node=1]
NodeHandle == /\ s.node=1 /\ s'=[s EXCEPT !.handled[s.ncur]=@+1,!.nodeHandled=TRUE,!.node=2]
NodeSync == /\ s.node=2 /\ s'=[s EXCEPT !.confirmed=Max(@,s.ncur+1),!.node=0]
NodeStop == /\ (s.node=0 \/ (~NodeIdleStop /\ s.node<3)) /\ s'=[s EXCEPT !.node=3]
RustStart == /\ s.rust=0 /\ s.node=3 /\ s'=[s EXCEPT !.rust=1,!.offset=StartOffset(s)]
\* getUpdates(offset) confirms ids < offset; should_handle(id, claimed) = id > claimed.
Receive == /\ s.rust=1 /\ s.phase=0 /\ Max(s.offset,s.confirmed)<=2
 /\ LET c == Max(s.offset,s.confirmed)
        skip == Durable /\ c<=s.claimed
    IN s'=[s EXCEPT !.confirmed=c,!.current=c,!.logged[c]=@ \/ skip,
           !.claimed=IF Durable /\ ~skip THEN Max(@,c) ELSE @,
           !.phase=IF skip THEN 2 ELSE 1]
Handle == /\ s.rust=1 /\ s.phase=1
 /\ s'=[s EXCEPT !.handled[s.current]=@+1,!.rustHandled=TRUE,!.phase=2]
Advance == /\ s.rust=1 /\ s.phase=2
 /\ s'=[s EXCEPT !.offset=IF Durable THEN s.claimed+1 ELSE s.current+1,!.phase=0]
Crash == /\ s.rust=1 /\ ~s.crash /\ s'=[s EXCEPT !.rust=2,!.phase=0,!.crash=TRUE]
Restart == /\ s.rust=2 /\ s'=[s EXCEPT !.rust=1,!.offset=StartOffset(s)]
\* Node takes the lock back and calls deleteWebhook({drop_pending_updates: true}).
RollbackToNode == /\ Rollback /\ s.rust=1 /\ s.phase=0
 /\ s'=[s EXCEPT !.rust=3,!.node=4,!.confirmed=3]
Next == NodeReceive \/ NodeHandle \/ NodeSync \/ NodeStop \/ RustStart \/ Receive \/ Handle
 \/ Advance \/ Crash \/ Restart \/ RollbackToNode
NoDoubleReply == \A i \in 1..2:s.handled[i]<=1
NoSilentSkip == \A i \in 1..2:s.handled[i]>0 \/ s.confirmed<=i \/ s.logged[i]
NeverAllHandled == s.handled#<<1,1>>
NeverSkipLogged == ~\E i \in 1..2:s.logged[i] /\ s.handled[i]=0
NeverNodeThenRust == ~(s.nodeHandled /\ s.rustHandled)
Spec == Init /\ [][Next]_vars
=============================================================================
