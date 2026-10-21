(***************************************************************************)
(* Lucina-bring client -> server item-op sync, mirroring the Rust OpLog   *)
(* (crates/lucina-core/src/sync.rs) and the proxy batch write path.         *)
(*                                                                         *)
(* Live architecture: stop-and-wait per client, mirroring the Rust OpLog.  *)
(* Local commits append to a FIFO outbox (a delete op only exists after    *)
(* its add); the pump sends the outbox HEAD -- an HTTP-level retransmit    *)
(* race may duplicate that head copy -- and acks (drops) the head once     *)
(* the server has applied it. The server consumes each channel head in     *)
(* order and applies each op AT MOST ONCE (op-id dedup), which is what     *)
(* makes retransmit duplicates harmless.                                   *)
(*                                                                         *)
(* Checked in MigratedSync.cfg (DedupMode = TRUE, must hold):              *)
(*   DeletedStaysDeleted -- once a delete of item i has been applied, i    *)
(*                          never re-appears on the server (duplicated     *)
(*                          add copies cannot resurrect it).               *)
(*   AllApplied          -- every committed op is eventually applied       *)
(*                          (fairness: WF on the global next-state, the    *)
(*                          same assumption class as FastStartPrompt).     *)
(*                                                                         *)
(* Witness in MigratedSync_witness.cfg (DedupMode = FALSE, MUST FAIL):     *)
(*   the same invariants without server-side dedup -- the duplicated add   *)
(*   resurrects a deleted item, i.e. the model is strong enough to catch   *)
(*   a naive implementation.                                               *)
(*                                                                         *)
(* Out of scope / environment assumptions: multi-device edits of the SAME  *)
(* item by different owners (items are single-owner here, matching the     *)
(* shared-list membership model), server-side storage failure, auth.       *)
(* Bounds: 2 clients, 1 owned item per client, each commits Add then       *)
(* Delete; at most one duplicate copy per send.                            *)
(* Terminal states are by design (queues drained) -> deadlock checking     *)
(* disabled on the command line.                                           *)
(***************************************************************************)
------------------------------- MODULE MigratedSync -------------------------------
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS C1, C2, I1, I2, DedupMode   \* anchors; Clients/ItemIds derive from them

Clients == {C1, C2}
ItemIds == {I1, I2}
OwnedBy(c) == IF c = C1 THEN {I1} ELSE {I2}
OpsOf(c) ==
    {op \in [c: Clients, kind: {"add", "del"}, item: ItemIds] :
        (op.c = c) /\ (op.item \in OwnedBy(op.c))}

VARIABLES
    committed,     \* [c |-> ops committed locally]
    outbox,        \* [c |-> FIFO seq of committed ops awaiting ack]
    chan,          \* [c |-> FIFO sequence of op copies in flight]
    retry,         \* [c |-> at most one delayed duplicate, out-of-band path]
    copies,        \* [c |-> 0 until the current head has been sent]
    applied,       \* ops the server has applied (dedup key set)
    serverItems,   \* current server item set
    everDeleted    \* [i |-> a delete of i has been applied]

DeletedStaysDeleted == \A i \in ItemIds : everDeleted[i] => (i \notin serverItems)

AllApplied == \A op \in (UNION {OpsOf(c) : c \in Clients}) :
                  ((op \in committed[op.c]) ~> (op \in applied))

-----------------------------------------------------------------------------
(* --fair algorithm MigratedSync {
  variables
    committed = [c \in Clients |-> {}],
    outbox = [c \in Clients |-> <<>>],
    chan = [c \in Clients |-> <<>>],
    retry = [c \in Clients |-> {}],
    copies = [c \in Clients |-> 0],
    applied = {},
    serverItems = {},
    everDeleted = [i \in ItemIds |-> FALSE];

  process (CProc \in Clients)
  {
    Commit:
      while (\E op \in OpsOf(self) : op \notin committed[self]) {
        with (op \in {oo \in OpsOf(self) :
                /\ oo \notin committed[self]
                \* a delete op only exists after its add was committed
                /\ (oo.kind = "del" =>
                      [c |-> self, kind |-> "add", item |-> oo.item] \in committed[self])}) {
          committed[self] := committed[self] \cup {op};
          outbox[self] := Append(outbox[self], op);
        }
      };
    Pump:
      \* stop-and-wait, FIFO outbox: exactly the OpLog drain-from-front
      \* discipline. The op goes out on the in-order channel; the
      \* retransmit race becomes ONE delayed duplicate on an out-of-band
      \* path (retry over a new connection) that may overtake later ops.
      while (outbox[self] # <<>>) {
        either {
          await Head(outbox[self]) \in applied;
          outbox[self] := Tail(outbox[self]);
          copies[self] := 0;
        } or {
          await (Head(outbox[self]) \notin applied) /\ (copies[self] = 0);
          chan[self] := Append(chan[self], Head(outbox[self]));
          retry[self] := {Head(outbox[self])};
          copies[self] := 1;
        } or {
          await outbox[self] # <<>>;
          skip;
        }
      }
  }

  process (Server = "server")
  {
    Deliver:
      \* the server never terminates: it idles (stutters) while no work
      \* is available and wakes up when a copy arrives
      while (TRUE) {
        either {
          \* in-order delivery of the channel head
          await (\E cc \in Clients : chan[cc] # <<>>);
          with (c \in {cc \in Clients : chan[cc] # <<>>}) {
            with (h = Head(chan[c])) {
              if (~(DedupMode /\ h \in applied)) {
                if (h.kind = "add") {
                  serverItems := serverItems \cup {h.item};
                } else {
                  serverItems := serverItems \ {h.item};
                  everDeleted[h.item] := TRUE;
                }
              };
              applied := applied \cup {h};
              chan[c] := Tail(chan[c]);
            }
          }
        } or {
          \* out-of-band retry copy: may overtake later in-order ops
          await (\E cc \in Clients : retry[cc] # {});
          with (c \in {cc \in Clients : retry[cc] # {}}) {
            with (op \in retry[c]) {
              if (~(DedupMode /\ op \in applied)) {
                if (op.kind = "add") {
                  serverItems := serverItems \cup {op.item};
                } else {
                  serverItems := serverItems \ {op.item};
                  everDeleted[op.item] := TRUE;
                }
              };
              applied := applied \cup {op};
              retry[c] := retry[c] \ {op};
            }
          }
        }
      }
  }
}
end algorithm;
 *)
\* BEGIN TRANSLATION (chksum(pcal) = "7c28162a" /\ chksum(tla) = "a8abd14f")
VARIABLES pc, committed, outbox, chan, retry, copies, applied, serverItems, 
          everDeleted

vars == << pc, committed, outbox, chan, retry, copies, applied, serverItems, 
           everDeleted >>

ProcSet == (Clients) \cup {"server"}

Init == (* Global variables *)
        /\ committed = [c \in Clients |-> {}]
        /\ outbox = [c \in Clients |-> <<>>]
        /\ chan = [c \in Clients |-> <<>>]
        /\ retry = [c \in Clients |-> {}]
        /\ copies = [c \in Clients |-> 0]
        /\ applied = {}
        /\ serverItems = {}
        /\ everDeleted = [i \in ItemIds |-> FALSE]
        /\ pc = [self \in ProcSet |-> CASE self \in Clients -> "Commit"
                                        [] self = "server" -> "Deliver"]

Commit(self) == /\ pc[self] = "Commit"
                /\ IF \E op \in OpsOf(self) : op \notin committed[self]
                      THEN /\ \E op \in      {oo \in OpsOf(self) :
                                        /\ oo \notin committed[self]
                                        
                                        /\ (oo.kind = "del" =>
                                              [c |-> self, kind |-> "add", item |-> oo.item] \in committed[self])}:
                                /\ committed' = [committed EXCEPT ![self] = committed[self] \cup {op}]
                                /\ outbox' = [outbox EXCEPT ![self] = Append(outbox[self], op)]
                           /\ pc' = [pc EXCEPT ![self] = "Commit"]
                      ELSE /\ pc' = [pc EXCEPT ![self] = "Pump"]
                           /\ UNCHANGED << committed, outbox >>
                /\ UNCHANGED << chan, retry, copies, applied, serverItems, 
                                everDeleted >>

Pump(self) == /\ pc[self] = "Pump"
              /\ IF outbox[self] # <<>>
                    THEN /\ \/ /\ Head(outbox[self]) \in applied
                               /\ outbox' = [outbox EXCEPT ![self] = Tail(outbox[self])]
                               /\ copies' = [copies EXCEPT ![self] = 0]
                               /\ UNCHANGED <<chan, retry>>
                            \/ /\ (Head(outbox[self]) \notin applied) /\ (copies[self] = 0)
                               /\ chan' = [chan EXCEPT ![self] = Append(chan[self], Head(outbox[self]))]
                               /\ retry' = [retry EXCEPT ![self] = {Head(outbox[self])}]
                               /\ copies' = [copies EXCEPT ![self] = 1]
                               /\ UNCHANGED outbox
                            \/ /\ outbox[self] # <<>>
                               /\ TRUE
                               /\ UNCHANGED <<outbox, chan, retry, copies>>
                         /\ pc' = [pc EXCEPT ![self] = "Pump"]
                    ELSE /\ pc' = [pc EXCEPT ![self] = "Done"]
                         /\ UNCHANGED << outbox, chan, retry, copies >>
              /\ UNCHANGED << committed, applied, serverItems, everDeleted >>

CProc(self) == Commit(self) \/ Pump(self)

Deliver == /\ pc["server"] = "Deliver"
           /\ \/ /\ (\E cc \in Clients : chan[cc] # <<>>)
                 /\ \E c \in {cc \in Clients : chan[cc] # <<>>}:
                      LET h == Head(chan[c]) IN
                        /\ IF ~(DedupMode /\ h \in applied)
                              THEN /\ IF h.kind = "add"
                                         THEN /\ serverItems' = (serverItems \cup {h.item})
                                              /\ UNCHANGED everDeleted
                                         ELSE /\ serverItems' = serverItems \ {h.item}
                                              /\ everDeleted' = [everDeleted EXCEPT ![h.item] = TRUE]
                              ELSE /\ TRUE
                                   /\ UNCHANGED << serverItems, everDeleted >>
                        /\ applied' = (applied \cup {h})
                        /\ chan' = [chan EXCEPT ![c] = Tail(chan[c])]
                 /\ retry' = retry
              \/ /\ (\E cc \in Clients : retry[cc] # {})
                 /\ \E c \in {cc \in Clients : retry[cc] # {}}:
                      \E op \in retry[c]:
                        /\ IF ~(DedupMode /\ op \in applied)
                              THEN /\ IF op.kind = "add"
                                         THEN /\ serverItems' = (serverItems \cup {op.item})
                                              /\ UNCHANGED everDeleted
                                         ELSE /\ serverItems' = serverItems \ {op.item}
                                              /\ everDeleted' = [everDeleted EXCEPT ![op.item] = TRUE]
                              ELSE /\ TRUE
                                   /\ UNCHANGED << serverItems, everDeleted >>
                        /\ applied' = (applied \cup {op})
                        /\ retry' = [retry EXCEPT ![c] = retry[c] \ {op}]
                 /\ chan' = chan
           /\ pc' = [pc EXCEPT !["server"] = "Deliver"]
           /\ UNCHANGED << committed, outbox, copies >>

Server == Deliver

Next == Server
           \/ (\E self \in Clients: CProc(self))

Spec == /\ Init /\ [][Next]_vars
        /\ WF_vars(Next)

\* END TRANSLATION 
=============================================================================
