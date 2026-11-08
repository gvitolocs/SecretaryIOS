------------------------------ MODULE Timeline ------------------------------
(***************************************************************************)
(* Shared writers of Lucina/data/timeline.jsonl and the SSE reader.        *)
(* TLA+ twin of verification/models/src/timeline.rs (same actions, same    *)
(* invariants; docs/verification.md has the action -> code table).         *)
(*                                                                         *)
(*  Node   (Lucina/src/timeline.js)  id at normalize, refresh, open,      *)
(*         one O_APPEND write; no cross-process lock.                      *)
(*  Rust   (lucina-store/src/timeline.rs) flock, refresh, id past the     *)
(*         newest indexed id, rotation, one write, refresh + publish.      *)
(*  Pro    (lucina-proactive/src/channels.rs) id at decision time,        *)
(*         `docker exec cat >>`: the line may arrive as two writes.        *)
(*  Reader (lucina-server/src/api.rs::timeline_stream) replay after       *)
(*         Last-Event-ID, live broadcast, follow tick.                     *)
(*                                                                         *)
(* Files are inodes holding chunks; paths map to inodes, so a writer that *)
(* opened the file before a rotation writes into `.1`.                     *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    NodeLines, RustLines, ProLines,  \* lines per writer
    MaxMs,                           \* clock bound
    RotateAt,                        \* Rust rotates when the file holds this many lines
    ProSplit,                        \* cat may split one line into two writes
    Disconnects,                     \* SSE drops (browser auto-reconnects)
    FixArrival,                      \* fix A: resume/follow in arrival order, seen from replay
    FixWatchPrev,                    \* fix B: keep indexing the tail of `.1` after a rotation
    FixQueryId                       \* fix D: a reconnect without an event id still catches up (resync)

W == {"old", "node", "rust", "pro"}
WN(w) == CASE w = "old" -> 0 [] w = "node" -> 1 [] w = "rust" -> 2 [] w = "pro" -> 3 [] OTHER -> 9
Less(a, b) == \/ a.ms < b.ms
              \/ a.ms = b.ms /\ a.rank < b.rank
              \/ a.ms = b.ms /\ a.rank = b.rank /\ WN(a.w) < WN(b.w)
OLD == [ms |-> 0, rank |-> 0, w |-> "old"]
NONE == [ms |-> 99, rank |-> 99, w |-> "none"]   \* "no id" (records only compare with records)

Line(i) == [k |-> "line", id |-> i]
HeadC(i) == [k |-> "head", id |-> i]
TailC(i) == [k |-> "tail", id |-> i]
NL == [k |-> "nl", id |-> OLD]

(* Complete lines of s after `from` chunks: <<valid ids, garbage, boundary>> *)
RECURSIVE ParseAcc(_, _, _, _)
ParseAcc(s, i, start, acc) ==
    IF i > Len(s) THEN acc
    ELSE IF s[i].k = "head" THEN ParseAcc(s, i + 1, start, acc)
    ELSE LET line == SubSeq(s, start, i)
             ok == \/ Len(line) = 1 /\ line[1].k = "line"
                   \/ Len(line) = 2 /\ line[1].k = "head" /\ line[2].k = "tail"
                                    /\ line[1].id = line[2].id
             empty == Len(line) = 1 /\ line[1].k = "nl"
             acc2 == IF ok THEN <<Append(acc[1], line[1].id), acc[2], i>>
                     ELSE IF empty THEN <<acc[1], acc[2], i>>
                     ELSE <<acc[1], acc[2] + 1, i>>
         IN ParseAcc(s, i + 1, i + 1, acc2)
Parse(s, from) == ParseAcc(s, from + 1, from + 1, <<<<>>, 0, from>>)

SeqToSet(q) == {q[i] : i \in 1..Len(q)}
MaxOf(S) == CHOOSE m \in S : \A x \in S : x = m \/ Less(x, m)

VARIABLES now, inodes, paths, rotations, emptyRot,
          node, rust, pro, idx, sse, lastEvent, delivered, discLeft,
          written, settled, reconnected

vars == <<now, inodes, paths, rotations, emptyRot, node, rust, pro, idx, sse,
          lastEvent, delivered, discLeft, written, settled, reconnected>>

(* ---- files ------------------------------------------------------------ *)
Retained == {paths[j] : j \in {i \in 1..3 : paths[i] # 0}}
RetainedIds == UNION {SeqToSet(Parse(inodes[n], 0)[1]) : n \in Retained}
Garbage == \E n \in Retained : Parse(inodes[n], 0)[2] > 0
EndsPartial(n) == Len(inodes[n]) > 0 /\ inodes[n][Len(inodes[n])].k = "head"

(* ---- the Rust index --------------------------------------------------- *)
EntryIds(e) == {e[i].id : i \in 1..Len(e)}
Indexed == EntryIds(idx.entries)
AddEntries(e, ids, n) ==   \* append ids (a sequence) not yet indexed
    LET RECURSIVE Go(_, _)
        Go(acc, i) == IF i > Len(ids) THEN acc
                      ELSE IF ids[i] \in EntryIds(acc) THEN Go(acc, i + 1)
                      ELSE Go(Append(acc, [id |-> ids[i], ino |-> n]), i + 1)
    IN Go(e, 1)
NewIds(old, new) == SelectSeq([i \in 1..Len(new) |-> new[i].id], LAMBDA x : x \notin EntryIds(old))

LoadAll ==   \* the whole index rebuilt from the retained files (not published)
    LET e2 == AddEntries(IF paths[3] # 0 THEN AddEntries(<<>>, Parse(inodes[paths[3]], 0)[1], paths[3]) ELSE <<>>,
                         IF paths[2] # 0 THEN Parse(inodes[paths[2]], 0)[1] ELSE <<>>, paths[2])
        e3 == AddEntries(e2, IF paths[1] # 0 THEN Parse(inodes[paths[1]], 0)[1] ELSE <<>>, paths[1])
    IN [loaded |-> TRUE, cur |-> paths[1],
        known |-> IF paths[1] # 0 THEN Parse(inodes[paths[1]], 0)[3] ELSE 0,
        prev |-> IF FixWatchPrev THEN paths[2] ELSE 0,
        prevKnown |-> IF FixWatchPrev /\ paths[2] # 0 THEN Parse(inodes[paths[2]], 0)[3] ELSE 0,
        entries |-> e3]

(* Refresh(i) = <<new index, ids to publish>> for files f *)
RefreshWith(i, f, p) ==
    IF ~i.loaded THEN <<LoadAll, <<>>>>
    ELSE LET pv == IF i.prev # 0 THEN Parse(f[i.prev], i.prevKnown) ELSE <<<<>>, 0, 0>>
             e1 == IF i.prev # 0 THEN AddEntries(i.entries, pv[1], i.prev) ELSE i.entries
             i1 == [i EXCEPT !.entries = e1, !.prevKnown = IF i.prev # 0 THEN pv[3] ELSE 0]
             pub1 == NewIds(i.entries, e1)
         IN IF p[1] = 0 THEN <<i1, pub1>>
            ELSE IF (i.cur # 0 /\ i.cur # p[1]) \/ Len(f[p[1]]) < i.known
                 THEN <<LoadAll, <<>>>>
                 ELSE LET pc == Parse(f[p[1]], i1.known)
                          e2 == AddEntries(i1.entries, pc[1], p[1])
                      IN <<[i1 EXCEPT !.entries = e2, !.cur = p[1], !.known = pc[3]],
                           pub1 \o NewIds(i1.entries, e2)>>

Publish(q, ids) == IF sse.connected THEN q \o ids ELSE q

(* ---- the reader -------------------------------------------------------- *)
AfterById(e, c) == LET ids == {x \in EntryIds(e) : c = NONE \/ Less(c, x)}
                       RECURSIVE Sorted(_)
                       Sorted(S) == IF S = {} THEN <<>>
                                    ELSE LET m == CHOOSE m \in S : \A x \in S : x = m \/ Less(m, x)
                                         IN <<m>> \o Sorted(S \ {m})
                   IN Sorted(ids)
PosOf(e, c) == CHOOSE i \in 0..Len(e) : (i = 0 /\ \A j \in 1..Len(e) : e[j].id # c) \/ (i > 0 /\ e[i].id = c)
AfterByArrival(e, c) ==
    IF c = NONE THEN [i \in 1..Len(e) |-> e[i].id]
    ELSE LET p == PosOf(e, c)
         IN IF p = 0 THEN AfterById(e, c) ELSE [i \in 1..(Len(e) - p) |-> e[p + i].id]
After(e, c) == IF FixArrival THEN AfterByArrival(e, c) ELSE AfterById(e, c)

Skip(s, x) == x \in s.seen \/ (~FixArrival /\ ~s.open /\ s.replayEnd # NONE /\ (x = s.replayEnd \/ Less(x, s.replayEnd)))
(* Send a sequence of ids through the stream filter: <<stream, delivered, lastEvent>> *)
RECURSIVE SendAll(_, _, _, _)
SendAll(s, d, le, ids) ==
    IF ids = <<>> THEN <<s, d, le>>
    ELSE LET x == Head(ids)
         IN IF Skip(s, x) THEN SendAll(s, d, le, Tail(ids))
            ELSE SendAll([s EXCEPT !.seen = @ \cup {x},
                                   !.cursor = IF FixArrival \/ @ = NONE \/ Less(@, x) THEN x ELSE @],
                         d \cup {x}, x, Tail(ids))

(* Connect: poll, subscribe, replay after the resume id (or start fresh). *)
ConnectFrom(i, le) ==
    LET resume == IF le # NONE THEN le ELSE IF FixQueryId THEN OLD ELSE NONE
        items == IF resume # NONE THEN After(i.entries, resume) ELSE <<>>
        last == IF Len(items) > 0 THEN items[Len(items)] ELSE resume
        newest == IF Len(i.entries) = 0 THEN NONE
                  ELSE IF FixArrival THEN i.entries[Len(i.entries)].id ELSE MaxOf(Indexed)
    IN [s |-> [connected |-> TRUE, queue |-> <<>>,
               seen |-> IF FixArrival THEN SeqToSet(items) ELSE {},
               replayEnd |-> IF resume # NONE THEN last ELSE newest,
               open |-> resume = NONE,
               cursor |-> IF resume # NONE THEN last ELSE newest],
        d |-> delivered \cup SeqToSet(items),
        le |-> IF Len(items) > 0 THEN last ELSE le]

NextId(st, w, r) == IF st.last # NONE /\ now <= st.last.ms
                    THEN [ms |-> st.last.ms, rank |-> st.last.rank + 1, w |-> w]
                    ELSE [ms |-> now, rank |-> r, w |-> w]

Quiescent == node.left = 0 /\ rust.left = 0 /\ pro.left = 0
             /\ node.step = "idle" /\ rust.step = "idle" /\ pro.step = "idle"

(* ---- initial state ----------------------------------------------------- *)
Init ==
    /\ now = 0
    /\ inodes = <<<<Line(OLD)>>>>
    /\ paths = <<1, 0, 0>>
    /\ rotations = 0 /\ emptyRot = 0
    /\ node = [left |-> NodeLines, step |-> "idle", id |-> OLD, ino |-> 0, nl |-> FALSE, last |-> NONE]
    /\ rust = [left |-> RustLines, step |-> "idle", id |-> OLD, rot |-> FALSE, nl |-> FALSE, last |-> NONE]
    /\ pro  = [left |-> ProLines, step |-> "idle", id |-> OLD, ino |-> 0, last |-> NONE]
    /\ idx = [loaded |-> TRUE, cur |-> 1, known |-> 1, prev |-> 0, prevKnown |-> 0,
              entries |-> <<[id |-> OLD, ino |-> 1]>>]
    /\ delivered = {OLD}
    /\ lastEvent = NONE
    /\ LET c == ConnectFrom([loaded |-> TRUE, cur |-> 1, known |-> 1, prev |-> 0, prevKnown |-> 0,
                             entries |-> <<[id |-> OLD, ino |-> 1]>>], NONE)
       IN sse = c.s
    /\ discLeft = Disconnects
    /\ written = {}
    /\ settled = FALSE
    /\ reconnected = FALSE

(* ---- actions ----------------------------------------------------------- *)
Tick == now < MaxMs /\ now' = now + 1
        /\ UNCHANGED <<inodes, paths, rotations, emptyRot, node, rust, pro, idx, sse,
                       lastEvent, delivered, discLeft, written, settled, reconnected>>

EndsPartial2(f, n) == Len(f[n]) > 0 /\ f[n][Len(f[n])].k = "head"

CurOrNew == IF paths[1] # 0 THEN <<inodes, paths, paths[1]>>
            ELSE <<Append(inodes, <<>>), [paths EXCEPT ![1] = Len(inodes) + 1], Len(inodes) + 1>>

NodeGen(r) == node.step = "idle" /\ node.left > 0
    /\ LET id == NextId(node, "node", r)
       IN node' = [node EXCEPT !.step = "gen", !.id = id, !.last = id]
    /\ UNCHANGED <<now, inodes, paths, rotations, emptyRot, rust, pro, idx, sse,
                   lastEvent, delivered, discLeft, written, settled, reconnected>>

NodeOpen == node.step = "gen"    \* refresh (rotation off: LUCINA_TIMELINE_MAX_BYTES raised) + open
    /\ LET c == CurOrNew
       IN /\ inodes' = c[1] /\ paths' = c[2]
          /\ node' = [node EXCEPT !.step = "open", !.ino = c[3], !.nl = EndsPartial2(c[1], c[3])]
    /\ UNCHANGED <<now, rotations, emptyRot, rust, pro, idx, sse, lastEvent, delivered,
                   discLeft, written, settled, reconnected>>

NodeWrite == node.step = "open"
    /\ inodes' = [inodes EXCEPT ![node.ino] = @ \o (IF node.nl THEN <<NL, Line(node.id)>> ELSE <<Line(node.id)>>)]
    /\ written' = written \cup {node.id}
    /\ node' = [node EXCEPT !.step = "idle", !.left = @ - 1]
    /\ UNCHANGED <<now, paths, rotations, emptyRot, rust, pro, idx, sse, lastEvent, delivered,
                   discLeft, settled, reconnected>>

ProGen(r) == pro.step = "idle" /\ pro.left > 0
    /\ LET id == NextId(pro, "pro", r)
       IN pro' = [pro EXCEPT !.step = "gen", !.id = id, !.last = id]
    /\ UNCHANGED <<now, inodes, paths, rotations, emptyRot, node, rust, idx, sse,
                   lastEvent, delivered, discLeft, written, settled, reconnected>>

ProOpen == pro.step = "gen"
    /\ LET c == CurOrNew
       IN /\ inodes' = c[1] /\ paths' = c[2]
          /\ pro' = [pro EXCEPT !.step = "open", !.ino = c[3]]
    /\ UNCHANGED <<now, rotations, emptyRot, node, rust, idx, sse, lastEvent, delivered,
                   discLeft, written, settled, reconnected>>

ProWrite == pro.step = "open"
    /\ (IF ProSplit
        THEN /\ inodes' = [inodes EXCEPT ![pro.ino] = Append(@, HeadC(pro.id))]
             /\ pro' = [pro EXCEPT !.step = "half"]
             /\ written' = written
        ELSE /\ inodes' = [inodes EXCEPT ![pro.ino] = Append(@, Line(pro.id))]
             /\ pro' = [pro EXCEPT !.step = "idle", !.left = @ - 1]
             /\ written' = written \cup {pro.id})
    /\ UNCHANGED <<now, paths, rotations, emptyRot, node, rust, idx, sse, lastEvent, delivered,
                   discLeft, settled, reconnected>>

ProTail == pro.step = "half"
    /\ inodes' = [inodes EXCEPT ![pro.ino] = Append(@, TailC(pro.id))]
    /\ pro' = [pro EXCEPT !.step = "idle", !.left = @ - 1]
    /\ written' = written \cup {pro.id}
    /\ UNCHANGED <<now, paths, rotations, emptyRot, node, rust, idx, sse, lastEvent, delivered,
                   discLeft, settled, reconnected>>

RustBegin(r) == rust.step = "idle" /\ rust.left > 0
    /\ LET rf == RefreshWith(idx, inodes, paths)
           i == rf[1]
           gen == NextId(rust, "rust", r)
           id == IF i.entries = <<>> THEN gen
                 ELSE LET mx == MaxOf(EntryIds(i.entries))
                      IN IF Less(mx, gen) THEN gen ELSE [ms |-> mx.ms, rank |-> mx.rank + 1, w |-> "rust"]
           lines == IF i.cur # 0 THEN Len(Parse(SubSeq(inodes[i.cur], 1, i.known), 0)[1]) ELSE 0
       IN /\ idx' = i
          /\ sse' = [sse EXCEPT !.queue = Publish(@, rf[2])]
          /\ rust' = [rust EXCEPT !.step = "locked", !.id = id, !.last = gen,
                                  !.rot = (i.known > 0 /\ lines >= RotateAt),
                                  !.nl = (i.cur # 0 /\ Len(inodes[i.cur]) > i.known)]
    /\ UNCHANGED <<now, inodes, paths, rotations, emptyRot, node, pro, lastEvent, delivered,
                   discLeft, written, settled, reconnected>>

RustRotate == rust.step = "locked" /\ rust.rot
    /\ emptyRot' = (IF paths[1] = 0 \/ Len(inodes[paths[1]]) = 0 THEN emptyRot + 1 ELSE emptyRot)
    /\ paths' = <<0, paths[1], paths[2]>>
    /\ rotations' = rotations + 1
    /\ LET kept == {paths[1], paths[2]} \ {0}
       IN idx' = [idx EXCEPT !.entries = SelectSeq(@, LAMBDA e : e.ino \in kept),
                             !.prev = IF FixWatchPrev THEN paths[1] ELSE 0,
                             !.prevKnown = IF FixWatchPrev THEN idx.known ELSE 0,
                             !.cur = 0, !.known = 0]
    /\ rust' = [rust EXCEPT !.rot = FALSE, !.nl = FALSE]
    /\ UNCHANGED <<now, inodes, node, pro, sse, lastEvent, delivered, discLeft, written,
                   settled, reconnected>>

RustWrite == rust.step = "locked" /\ ~rust.rot
    /\ LET c == CurOrNew
           f == [c[1] EXCEPT ![c[3]] = @ \o (IF rust.nl THEN <<NL, Line(rust.id)>> ELSE <<Line(rust.id)>>)]
           rf == RefreshWith(idx, f, c[2])
       IN /\ inodes' = f /\ paths' = c[2]
          /\ idx' = rf[1]
          /\ sse' = [sse EXCEPT !.queue = Publish(@, rf[2])]
    /\ written' = written \cup {rust.id}
    /\ rust' = [rust EXCEPT !.step = "idle", !.left = @ - 1]
    /\ UNCHANGED <<now, rotations, emptyRot, node, pro, lastEvent, delivered, discLeft,
                   settled, reconnected>>

Follow == LET rf == RefreshWith(idx, inodes, paths)
              q == Publish(sse.queue, rf[2])
              r == SendAll([sse EXCEPT !.queue = q], delivered, lastEvent, After(rf[1].entries, sse.cursor))
          IN <<rf[1], r[1], r[2], r[3]>>

SseFollow == rust.step = "idle" /\ sse.connected /\ ~settled
    /\ LET f == Follow
       IN idx' = f[1] /\ sse' = f[2] /\ delivered' = f[3] /\ lastEvent' = f[4]
    /\ UNCHANGED <<now, inodes, paths, rotations, emptyRot, node, rust, pro, discLeft, written,
                   settled, reconnected>>

SseDeliver == sse.connected /\ sse.queue # <<>> /\ ~settled
    /\ LET r == SendAll([sse EXCEPT !.queue = Tail(@)], delivered, lastEvent, <<Head(sse.queue)>>)
       IN sse' = r[1] /\ delivered' = r[2] /\ lastEvent' = r[3]
    /\ UNCHANGED <<now, inodes, paths, rotations, emptyRot, node, rust, pro, idx, discLeft,
                   written, settled, reconnected>>

SseDisconnect == sse.connected /\ discLeft > 0 /\ rust.step = "idle"
    /\ sse' = [sse EXCEPT !.connected = FALSE, !.queue = <<>>]
    /\ discLeft' = discLeft - 1
    /\ UNCHANGED <<now, inodes, paths, rotations, emptyRot, node, rust, pro, idx, lastEvent,
                   delivered, written, settled, reconnected>>

SseConnect == ~sse.connected /\ rust.step = "idle"
    /\ LET rf == RefreshWith(idx, inodes, paths)
           c == ConnectFrom(rf[1], lastEvent)
       IN /\ idx' = rf[1] /\ sse' = c.s /\ delivered' = c.d /\ lastEvent' = c.le
    /\ reconnected' = TRUE
    /\ UNCHANGED <<now, inodes, paths, rotations, emptyRot, node, rust, pro, discLeft, written, settled>>

Settle == Quiescent /\ sse.connected /\ sse.queue = <<>> /\ ~settled
    /\ LET f == Follow
           r == SendAll([f[2] EXCEPT !.queue = <<>>], f[3], f[4], f[2].queue)
       IN idx' = f[1] /\ sse' = r[1] /\ delivered' = r[2] /\ lastEvent' = r[3]
    /\ settled' = TRUE
    /\ UNCHANGED <<now, inodes, paths, rotations, emptyRot, node, rust, pro, discLeft, written,
                   reconnected>>

Next == \/ Tick
        \/ \E r \in {0, 1} : NodeGen(r) \/ ProGen(r) \/ RustBegin(r)
        \/ NodeOpen \/ NodeWrite \/ ProOpen \/ ProWrite \/ ProTail
        \/ RustRotate \/ RustWrite
        \/ SseFollow \/ SseDeliver \/ SseDisconnect \/ SseConnect \/ Settle

Spec == Init /\ [][Next]_vars

(* ---- invariants (same names as the Stateright model) ------------------- *)
LinesWhole == ~Garbage
IdsUnique == \A n \in Retained, m \in Retained :
                 LET a == Parse(inodes[n], 0)[1]
                     b == Parse(inodes[m], 0)[1]
                 IN \A i \in 1..Len(a), j \in 1..Len(b) : (n = m /\ i = j) \/ a[i] # b[j]
RotationKeepsLines == emptyRot = 0 /\ written \subseteq RetainedIds
IndexComplete == settled => RetainedIds \subseteq Indexed
ReaderNeverSkips == settled => RetainedIds \subseteq delivered

(* Witnesses: must be violated (reachability), see the *_witness cfgs. *)
NeverRotatedAndSettled == ~(rotations > 0 /\ settled)
NeverResumedAndSettled == ~(reconnected /\ settled)
=============================================================================
