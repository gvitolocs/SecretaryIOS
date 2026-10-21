(***************************************************************************)
(* Original reference-client sync semantics, as observed in the decompiled *)
(* APK (bring-apk-analysis): every client keeps a local item set and       *)
(* periodically pushes a FULL snapshot of the list to the server; the      *)
(* server stores the last push verbatim (last-write-wins full replace).    *)
(*                                                                         *)
(* Invariants checked in OriginalSync.cfg:                                 *)
(*   ServerIsSomePushedSnapshot  -- the server only ever holds a snapshot  *)
(*                                  some client actually pushed (atomic    *)
(*                                  replace, no torn state).               *)
(*                                                                         *)
(* Witness checked in OriginalSync_witness.cfg (MUST FAIL, rc 12):         *)
(*   CommittedSurvive -- an item a client committed and pushed survives    *)
(*                       on the server. Interleaved full-snapshot pushes   *)
(*                       lose concurrent commits; this model proves the   *)
(*                       race exists in the original design. The migrated  *)
(*                       design (MigratedSync.tla) fixes it with           *)
(*                       idempotent per-item ops.                          *)
(*                                                                         *)
(* Out of scope / environment assumptions: network failures (the real      *)
(* client retries; a retry pushes a fresh full snapshot, which is exactly  *)
(* the modeled action), auth, membership changes mid-sync.                 *)
(* Bounds: 2 clients, 2 items, one owned item per client (ItemOwner).      *)
(* Terminal states are by design (both clients done) -> deadlock checking  *)
(* disabled on the command line.                                           *)
(***************************************************************************)
------------------------------- MODULE OriginalSync -------------------------------
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS C1, C2, I1, I2   \* anchors; Clients/ItemIds derive from them

Clients == {C1, C2}
ItemIds == {I1, I2}
OwnedBy(c) == IF c = C1 THEN {I1} ELSE {I2}

VARIABLES
    localItems,      \* [c |-> client's local item set]
    localCommitted,  \* [c |-> items the client has committed locally]
    pushedOnce,      \* [c |-> client has done its final push]
    serverItems,     \* current server snapshot
    pushed           \* set of all snapshots ever pushed

ServerIsSomePushedSnapshot == serverItems \in pushed

CommittedSurvive ==
    \A c \in Clients : \A i \in OwnedBy(c) :
        ((i \in localCommitted[c]) /\ pushedOnce[c]) => (i \in serverItems)

-----------------------------------------------------------------------------
(* --fair algorithm OrigSync {
  variables
    localItems = [c \in Clients |-> {}],
    localCommitted = [c \in Clients |-> {}],
    pushedOnce = [c \in Clients |-> FALSE],
    serverItems = {},
    pushed = {{}};

  process (CProc \in Clients)
  {
    Work:
      while (~pushedOnce[self]) {
        either {
          await (\E i \in OwnedBy(self) : i \notin localCommitted[self]);
          with (i \in {i \in OwnedBy(self) : i \notin localCommitted[self]}) {
            localItems[self] := localItems[self] \cup {i};
            localCommitted[self] := localCommitted[self] \cup {i};
          }
        } or {
          await (localCommitted[self] = OwnedBy(self));
          serverItems := localItems[self];
          pushed := pushed \cup {localItems[self]};
          pushedOnce[self] := TRUE;
        }
      }
  }
}
end algorithm;
 *)
\* BEGIN TRANSLATION (chksum(pcal) = "7c28162a" /\ chksum(tla) = "a58c9ffd")
VARIABLES pc, localItems, localCommitted, pushedOnce, serverItems, pushed

vars == << pc, localItems, localCommitted, pushedOnce, serverItems, pushed >>

ProcSet == (Clients)

Init == (* Global variables *)
        /\ localItems = [c \in Clients |-> {}]
        /\ localCommitted = [c \in Clients |-> {}]
        /\ pushedOnce = [c \in Clients |-> FALSE]
        /\ serverItems = {}
        /\ pushed = {{}}
        /\ pc = [self \in ProcSet |-> "Work"]

Work(self) == /\ pc[self] = "Work"
              /\ IF ~pushedOnce[self]
                    THEN /\ \/ /\ (\E i \in OwnedBy(self) : i \notin localCommitted[self])
                               /\ \E i \in {i \in OwnedBy(self) : i \notin localCommitted[self]}:
                                    /\ localItems' = [localItems EXCEPT ![self] = localItems[self] \cup {i}]
                                    /\ localCommitted' = [localCommitted EXCEPT ![self] = localCommitted[self] \cup {i}]
                               /\ UNCHANGED <<pushedOnce, serverItems, pushed>>
                            \/ /\ (localCommitted[self] = OwnedBy(self))
                               /\ serverItems' = localItems[self]
                               /\ pushed' = (pushed \cup {localItems[self]})
                               /\ pushedOnce' = [pushedOnce EXCEPT ![self] = TRUE]
                               /\ UNCHANGED <<localItems, localCommitted>>
                         /\ pc' = [pc EXCEPT ![self] = "Work"]
                    ELSE /\ pc' = [pc EXCEPT ![self] = "Done"]
                         /\ UNCHANGED << localItems, localCommitted, 
                                         pushedOnce, serverItems, pushed >>

CProc(self) == Work(self)

(* Allow infinite stuttering to prevent deadlock on termination. *)
Terminating == /\ \A self \in ProcSet: pc[self] = "Done"
               /\ UNCHANGED vars

Next == (\E self \in Clients: CProc(self))
           \/ Terminating

Spec == /\ Init /\ [][Next]_vars
        /\ WF_vars(Next)

Termination == <>(\A self \in ProcSet: pc[self] = "Done")

\* END TRANSLATION 
=============================================================================
