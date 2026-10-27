----------------------------- MODULE Discord -----------------------------
EXTENDS Integers, FiniteSets
\* Twin of verification/models/src/retirement.rs::Discord (R5).
CONSTANTS Early,Flock
VARIABLE s
vars == <<s>>
Ids == 0..1
Init == s=[node |-> TRUE,enabled |-> FALSE,up |-> [i \in Ids |-> TRUE],
 sessions |-> [i \in Ids |-> FALSE],crash |-> FALSE,pos |-> 0]
Phase == IF Early /\ s.pos<2 THEN 1-s.pos ELSE s.pos
Operator == /\ s.pos<4
 /\ s'=[s EXCEPT !.pos=@+1,
 !.node=IF Phase=0 THEN FALSE ELSE IF Phase=3 THEN TRUE ELSE @,
 !.enabled=IF Phase=1 THEN TRUE ELSE IF Phase=2 THEN FALSE ELSE @,
 !.sessions=IF Phase=2 THEN [i \in Ids |-> FALSE] ELSE @]
Connect(i) == /\ s.up[i] /\ s.enabled /\ ~s.sessions[i] /\ (~Flock \/ ~s.sessions[1-i])
 /\ s'=[s EXCEPT !.sessions[i]=TRUE]
Crash(i) == /\ s.up[i] /\ ~s.crash
 /\ s'=[s EXCEPT !.up[i]=FALSE,!.sessions[i]=FALSE,!.crash=TRUE]
Restart(i) == /\ ~s.up[i] /\ s'=[s EXCEPT !.up[i]=TRUE]
Next == Operator \/ (\E i \in Ids:Connect(i) \/ Crash(i) \/ Restart(i))
SingleGateway == Cardinality({i \in Ids:s.sessions[i]})+(IF s.node THEN 1 ELSE 0)<=1
NeverRustConnects == ~\E i \in Ids:s.sessions[i]
NeverRolledBack == ~(s.pos=4 /\ s.node /\ ~s.enabled)
Spec == Init /\ [][Next]_vars
=============================================================================
