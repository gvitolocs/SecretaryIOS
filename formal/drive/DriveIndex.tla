----------------------------- MODULE DriveIndex -----------------------------
(***************************************************************************)
(* Lucina media library: the Drive's folder index against a disk that      *)
(* comes and goes. Twin of verification/models/src/drive_index.rs (same    *)
(* steps and switches; read its header for the code mapping).              *)
(*                                                                         *)
(* A Drive folder that is the big disk's mountpoint holds two files. A     *)
(* scan walks them, then the stray position (a file written into the      *)
(* empty mountpoint while the disk is away), then decides whether to trash *)
(* the catalog rows it did not see (DK4). The environment unmounts and     *)
(* remounts, deletes and edits files, and a phone uploads the bytes of     *)
(* file 0's first version (DK3).                                           *)
(*                                                                         *)
(* PruneMode: "kernel" (DK4 as shipped), "nofs" (without the filesystem   *)
(* check), "noguard" (trash whatever was not seen).                        *)
(* OwnedUpgrade: DK3 as shipped (the phone copy replaces an in-place      *)
(* blob); FALSE: the phone copy is dropped whenever a blob row exists.     *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS PruneMode, OwnedUpgrade, Scans, Mounts, Deletes, Edits

Files == 0..1
Stray == 2
Pos == 0..2
Deleted == -1
NoScan == [active |-> FALSE, fs |-> "disk", rootOk |-> FALSE, sameFs |-> FALSE,
           next |-> 0, seen |-> [i \in Pos |-> FALSE], errors |-> 0,
           kept |-> [f \in Files |-> FALSE]]

VARIABLES mounted, disk, stray, rows, recordedFs, scan, blob, phoneItem,
          scans, mounts, deletes, edits,
          badPrune, wasOwned, prunedDeleted, straySeen, walkError, upgraded

vars == <<mounted, disk, stray, rows, recordedFs, scan, blob, phoneItem,
          scans, mounts, deletes, edits,
          badPrune, wasOwned, prunedDeleted, straySeen, walkError, upgraded>>

\* DK4 and DK4b, as in crates/lucina-media/src/drive_kernel_shared.rs.
MayPrune(rootOk, sameFs, errors, seen, known) ==
    rootOk /\ sameFs /\ errors = 0 /\ (seen > 0 \/ known = 0)
MayAdoptFs(rootOk, errors, seen, missing) ==
    rootOk /\ errors = 0 /\ seen > 0 /\ missing = 0

Init ==
    /\ mounted = TRUE
    /\ disk = [f \in Files |-> 0]
    /\ stray = FALSE
    /\ rows = [i \in Pos |-> IF i = Stray THEN [present |-> FALSE, trashed |-> FALSE]
                                         ELSE [present |-> TRUE, trashed |-> FALSE]]
    /\ recordedFs = "disk"
    /\ scan = NoScan
    /\ blob = "inplace"
    /\ phoneItem = FALSE
    /\ scans = Scans /\ mounts = Mounts /\ deletes = Deletes /\ edits = Edits
    /\ badPrune = FALSE /\ wasOwned = FALSE /\ prunedDeleted = FALSE
    /\ straySeen = FALSE /\ walkError = FALSE /\ upgraded = FALSE

Owned(b) == wasOwned' = (wasOwned \/ b = "owned")

StartScan ==
    /\ ~scan.active /\ scans > 0
    /\ scans' = scans - 1
    /\ LET fs == IF mounted THEN "disk" ELSE "mountpoint" IN
       scan' = [active |-> TRUE, fs |-> fs, rootOk |-> TRUE,
                sameFs |-> (recordedFs = "none" \/ recordedFs = fs),
                next |-> 0, seen |-> [i \in Pos |-> FALSE], errors |-> 0,
                kept |-> [f \in Files |-> disk[f] # Deleted]]
    /\ UNCHANGED <<mounted, disk, stray, rows, recordedFs, blob, phoneItem, mounts, deletes, edits,
                   badPrune, wasOwned, prunedDeleted, straySeen, upgraded, walkError>>

\* One walk step at scan.next: what the walk sees there, and whether it is an error.
SeesAt(i) ==
    IF scan.fs = "disk"
    THEN i \in Files /\ mounted /\ disk[i] # Deleted
    ELSE i = Stray /\ ~mounted /\ stray
ErrorAt(i) ==
    IF scan.fs = "disk"
    THEN i \in Files /\ ~mounted
    ELSE i = Stray /\ mounted /\ stray

Visit ==
    /\ scan.active /\ scan.next <= Stray
    /\ LET i == scan.next
           sees == SeesAt(i)
           err == ErrorAt(i) IN
       /\ scan' = [scan EXCEPT !.next = i + 1,
                               !.seen[i] = (scan.seen[i] \/ sees),
                               !.errors = IF err THEN scan.errors + 1 ELSE scan.errors]
       /\ rows' = IF sees THEN [rows EXCEPT ![i] = [present |-> TRUE, trashed |-> FALSE]] ELSE rows
       /\ straySeen' = (straySeen \/ (sees /\ i = Stray))
       /\ walkError' = (walkError \/ err \/ scan.errors > 0)
    /\ UNCHANGED <<mounted, disk, stray, recordedFs, blob, phoneItem, scans, mounts, deletes, edits,
                   badPrune, wasOwned, prunedDeleted, upgraded>>

Finish ==
    /\ scan.active /\ scan.next > Stray
    /\ LET known == Cardinality({i \in Pos : rows[i].present})
           seen == Cardinality({i \in Pos : scan.seen[i]})
           gone == {i \in Pos : rows[i].present /\ ~scan.seen[i]}
           prune == CASE PruneMode = "kernel" -> MayPrune(scan.rootOk, scan.sameFs, scan.errors, seen, known)
                      [] PruneMode = "nofs" -> MayPrune(scan.rootOk, TRUE, scan.errors, seen, known)
                      [] OTHER -> TRUE IN
       /\ recordedFs' = IF MayAdoptFs(scan.rootOk, scan.errors, seen, Cardinality(gone))
                        THEN scan.fs ELSE recordedFs
       /\ rows' = IF prune
                  THEN [i \in Pos |-> IF i \in gone THEN [present |-> FALSE, trashed |-> TRUE] ELSE rows[i]]
                  ELSE rows
       /\ badPrune' = (badPrune \/ (prune /\ \E f \in Files : f \in gone /\ scan.kept[f] /\ disk[f] # Deleted))
       /\ prunedDeleted' = (prunedDeleted \/ (prune /\ \E f \in Files : f \in gone /\ disk[f] = Deleted))
    /\ scan' = NoScan
    /\ UNCHANGED <<mounted, disk, stray, blob, phoneItem, scans, mounts, deletes, edits,
                   wasOwned, straySeen, walkError, upgraded>>

Unmount ==
    /\ mounts > 0 /\ mounted
    /\ mounts' = mounts - 1 /\ mounted' = FALSE
    /\ UNCHANGED <<disk, stray, rows, recordedFs, scan, blob, phoneItem, scans, deletes, edits,
                   badPrune, wasOwned, prunedDeleted, straySeen, walkError, upgraded>>

Mount ==
    /\ mounts > 0 /\ ~mounted
    /\ mounts' = mounts - 1 /\ mounted' = TRUE
    /\ UNCHANGED <<disk, stray, rows, recordedFs, scan, blob, phoneItem, scans, deletes, edits,
                   badPrune, wasOwned, prunedDeleted, straySeen, walkError, upgraded>>

Delete(f) ==
    /\ deletes > 0 /\ disk[f] # Deleted
    /\ deletes' = deletes - 1
    /\ disk' = [disk EXCEPT ![f] = Deleted]
    /\ scan' = IF scan.active THEN [scan EXCEPT !.kept[f] = FALSE] ELSE scan
    /\ UNCHANGED <<mounted, stray, rows, recordedFs, blob, phoneItem, scans, mounts, edits,
                   badPrune, wasOwned, prunedDeleted, straySeen, walkError, upgraded>>

Edit(f) ==
    /\ edits > 0 /\ disk[f] = 0
    /\ edits' = edits - 1
    /\ disk' = [disk EXCEPT ![f] = 1]
    /\ UNCHANGED <<mounted, stray, rows, recordedFs, scan, blob, phoneItem, scans, mounts, deletes,
                   badPrune, wasOwned, prunedDeleted, straySeen, walkError, upgraded>>

StrayWrite ==
    /\ ~mounted /\ ~stray
    /\ stray' = TRUE
    /\ UNCHANGED <<mounted, disk, rows, recordedFs, scan, blob, phoneItem, scans, mounts, deletes, edits,
                   badPrune, wasOwned, prunedDeleted, straySeen, walkError, upgraded>>

\* keep_owned_bytes (DK3) under the catalog lock.
PhoneUpload ==
    /\ ~phoneItem
    /\ phoneItem' = TRUE
    /\ LET b == CASE blob = "none" -> "owned"
                  [] blob = "owned" -> "owned"
                  [] OwnedUpgrade -> "owned"
                  [] OTHER -> "inplace" IN
       /\ blob' = b
       /\ Owned(b)
    /\ upgraded' = (upgraded \/ (blob = "inplace" /\ OwnedUpgrade))
    /\ UNCHANGED <<mounted, disk, stray, rows, recordedFs, scan, scans, mounts, deletes, edits,
                   badPrune, prunedDeleted, straySeen, walkError>>

Next ==
    \/ StartScan \/ Visit \/ Finish \/ Unmount \/ Mount \/ StrayWrite \/ PhoneUpload
    \/ \E f \in Files : Delete(f) \/ Edit(f)

Spec == Init /\ [][Next]_vars

\* The catalog never trashes a file that stayed on the disk through the scan.
UnmountNeverTrashes == ~badPrune
\* The phone's original is always somewhere: owned, or file 0 still holds its bytes.
PhoneOriginalKept == phoneItem => (blob = "owned" \/ (blob = "inplace" /\ disk[0] = 0))
OwnedNeverDowngraded == wasOwned => blob = "owned"

\* Witnesses (MUST FAIL): the interesting states are reached.
NeverPrunedDeleted == ~prunedDeleted
NeverStraySeen == ~straySeen
NeverWalkError == ~walkError
NeverUpgraded == ~upgraded
=============================================================================
