---------------------------- MODULE Reminders ----------------------------
EXTENDS Integers, TLC
\* Twin of verification/models/src/retirement.rs::Reminders (R1): same actions, switches and
\* bounds. Index 0 = Node, 1 = Rust; file[1] is due at the start, file[2] is the create.
\* work: 0 idle, 1 create, 2 cancel, 3 Node read, 4 Node sent, 5 Rust claimed, 6 Rust sent.
CONSTANTS Early, Idle, DrainRollback
VARIABLE s
vars == <<s>>
Ids == 0..1
R == 1..2
Init == s = [node |-> TRUE, local |-> FALSE, up |-> TRUE, pos |-> 0,
 crash |-> FALSE, file |-> <<"pending", "absent">>,
 snap |-> [i \in Ids |-> <<"absent", "absent">>], work |-> [i \in Ids |-> 0],
 accepted |-> FALSE, requested |-> FALSE, disabled |-> FALSE, lost |-> FALSE,
 deliveries |-> <<0,0>>, settled |-> FALSE, handed |-> FALSE]
Phase == IF Early /\ s.pos<2 THEN 1-s.pos ELSE s.pos
\* 0 Node restarted with LUCINA_REMINDERS_ENABLED=false; 1 Rust writer=local + scheduler;
\* 2 rollback: Rust writer=node, scheduler off; 3 Node enabled again.
Operator ==
  /\ s.pos<4
  /\ (~Idle \/ s.pos \notin {0,3} \/ s.work[0]=0)
  /\ (s.pos#2 \/ ~DrainRollback \/ (s.work[1]=0 /\ (\A r \in R:s.file[r]#"sending")))
  /\ s' = [s EXCEPT !.pos=@+1,
           !.node=IF Phase=0 THEN FALSE ELSE IF Phase=3 THEN TRUE ELSE @,
           !.local=IF Phase=1 THEN TRUE ELSE IF Phase=2 THEN FALSE ELSE @,
           !.up=IF Phase=1 THEN TRUE ELSE @,
           !.handed=(@ \/ Phase=1),
           !.work[0]=IF Phase=0 THEN 0 ELSE @,
           !.work[1]=IF Phase=2 THEN 0 ELSE @]
Crash ==
  /\ s.up /\ ~s.crash
  /\ s'=[s EXCEPT !.up=FALSE, !.crash=TRUE, !.work[1]=0]
Restart ==
  /\ ~s.up
  /\ s'=[s EXCEPT !.up=TRUE]
\* via 1 = app (Rust once it is the local writer), via 0 = Telegram (Node).
Request(via,cancel) ==
  LET i == IF via=1 /\ s.local THEN 1 ELSE 0
      enabled == IF i=0 THEN s.node ELSE s.up
  IN /\ IF cancel THEN s.file[2]="pending" ELSE ~s.requested
     /\ IF ~enabled
        THEN s'=[s EXCEPT !.disabled=TRUE, !.requested=(@ \/ ~cancel)]
        ELSE /\ s.work[i]=0
             /\ s'=[s EXCEPT !.snap[i]=s.file, !.work[i]=IF cancel THEN 2 ELSE 1,
                    !.requested=(@ \/ ~cancel)]
Enabled(i) == IF i=0 THEN s.node ELSE (s.local /\ s.up)
\* Rust: process_due (fail_stale_claims, then claim on disk). Node: snapshot.
Read(i) ==
  /\ Enabled(i)
  /\ s.work[i]=0
  /\ \A r \in R: s.deliveries[r]<2
  /\ \E r \in R: (s.file[r]="pending" \/ (i=1 /\ s.file[r]="sending"))
  /\ LET f == IF i=0 THEN s.file ELSE [r \in R |->
                CASE s.file[r]="sending" -> "failed"
                  [] s.file[r]="pending" -> "sending"
                  [] OTHER -> s.file[r]]
     IN s'=[s EXCEPT !.file=f, !.snap[i]=f, !.work[i]=IF i=0 THEN 3 ELSE 5]
\* Node sends what it read as pending (it has no "sending" status); Rust sends its claims.
Send(i) ==
  /\ Enabled(i)
  /\ s.work[i] \in {3,5}
  /\ LET want == IF i=0 THEN "pending" ELSE "sending"
     IN s'=[s EXCEPT !.deliveries=[r \in R |-> s.deliveries[r]+(IF s.snap[i][r]=want THEN 1 ELSE 0)],
            !.snap[i]=[r \in R |-> IF s.snap[i][r]=want THEN "sent" ELSE s.snap[i][r]],
            !.work[i]=IF i=0 THEN 4 ELSE 6]
Save(i) ==
  /\ Enabled(i)
  /\ s.work[i] \in {1,2,4,6}
  /\ LET f == IF s.work[i]=1 THEN [s.snap[i] EXCEPT ![2]="pending"]
              ELSE IF s.work[i]=2 THEN [s.snap[i] EXCEPT ![2]="cancelled"] ELSE s.snap[i]
     IN s'=[s EXCEPT !.file=f, !.snap[i]=f, !.accepted=(@ \/ s.work[i]=1),
            !.lost=(@ \/ (\E r \in R: s.file[r]#"absent" /\ f[r]="absent")), !.work[i]=0]
Settle ==
  /\ s.pos=4
  /\ s.up
  /\ s.requested
  /\ \A i \in Ids:s.work[i]=0
  /\ \A r \in R:(s.file[r]#"pending" /\ (s.local => s.file[r]#"sending"))
  /\ s'=[s EXCEPT !.settled=TRUE]
Next ==
  /\ ~s.settled
  /\ \/ Operator \/ Crash \/ Restart \/ Settle
     \/ \E i \in Ids:(Read(i) \/ Send(i) \/ Save(i) \/ (\E c \in BOOLEAN:Request(i,c)))
SingleWriter == ~(s.node /\ s.local /\ s.up)
NoLostRecord == ~s.lost /\ (s.accepted => s.file[2]#"absent")
NoDoubleDelivery == \A r \in R:s.deliveries[r]<=1
NoSilentLoss == s.settled => \A r \in R:
  ((r=2 /\ ~s.accepted) \/ s.deliveries[r]>0 \/ s.file[r] \in {"failed","cancelled"})
NeverHandedOver == ~(s.handed /\ s.local /\ ~s.node /\ s.deliveries[1]>0)
NeverRolledBack == ~(s.settled /\ s.node /\ ~s.local)
NeverDisabled == ~s.disabled
Spec == Init /\ [][Next]_vars
=============================================================================
