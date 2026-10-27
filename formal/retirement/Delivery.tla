----------------------------- MODULE Delivery -----------------------------
EXTENDS Integers
\* Twin of verification/models/src/retirement.rs::Delivery (R2). Outcome(n) is the composition
\* classify (outcome.rs) . BotApi::call/transport_error (lib.rs) . reminder_delivery.rs, as
\* Network::outcome in the Stateright model; Save is process_due's match on the outcome.
CONSTANTS RetryUncertain, FailKnown
VARIABLE s
vars == <<s>>
Networks == {"refuse","error","reply","body_lost","delivered_timeout","lost_timeout"}
Delivered(n) == n \in {"reply","body_lost","delivered_timeout"}
Outcome(n) == CASE n = "reply" -> "delivered"
             [] n \in {"refuse","error"} -> "not_delivered"
             [] OTHER -> "uncertain"
Init == s=[status |-> "pending",phase |-> 0,sends |-> 0,received |-> 0,
 crash |-> FALSE,up |-> TRUE,outcome |-> "none",visible |-> FALSE,known |-> FALSE]
Claim == /\ s.phase=0 /\ s.status="pending" /\ s.sends<2
 /\ s'=[s EXCEPT !.status="sending",!.phase=1]
\* One transient fault, then a healthy send.
Send(n) == /\ s.phase=1 /\ (n="reply" \/ s.sends=0)
 /\ s'=[s EXCEPT !.sends=@+1,!.outcome=n,!.phase=2,
 !.received=@+IF Delivered(n) THEN 1 ELSE 0]
Save == /\ s.phase=2
 /\ LET o == Outcome(s.outcome)
        st == CASE o="delivered" -> "sent"
              [] o="uncertain" -> IF RetryUncertain THEN "pending" ELSE "failed"
              [] OTHER -> IF FailKnown THEN "failed" ELSE "pending"
 IN s'=[s EXCEPT !.status=st,!.visible=(st="failed"),!.phase=0,!.outcome="none",
 !.known=(st="failed" /\ o="not_delivered")]
Crash == /\ s.up /\ ~s.crash
 /\ s'=[s EXCEPT !.up=FALSE,!.crash=TRUE,!.phase=0,!.outcome="none"]
Restart == /\ ~s.up /\ s'=[s EXCEPT !.up=TRUE]
\* fail_stale_claims: "interrupted; not resent".
Recover == /\ s.phase=0 /\ s.status="sending"
 /\ s'=[s EXCEPT !.status="failed",!.visible=TRUE]
Next == Crash \/ Restart \/ (s.up /\ (Claim \/ Save \/ Recover \/ (\E n \in Networks:Send(n))))
NoDoubleDelivery == s.received<=1
Finished == s.status="sent" \/ (s.status="failed" /\ s.visible)
NoSilentLoss == <>Finished
NeverDelivered == s.status#"sent"
NeverUncertainFailed == ~(s.status="failed" /\ s.received=1)
NeverRetryableFailed == ~s.known
Spec == Init /\ [][Next]_vars /\ WF_vars(Next)
=============================================================================
