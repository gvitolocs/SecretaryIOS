----------------------------- MODULE ShoppingOps -----------------------------
(***************************************************************************)
(* Shopping v2 ops (docs/shopping-v2.md) with the client offline outbox.   *)
(* Builds on formal/MigratedSync.tla (op-id dedup, deleted stays deleted)  *)
(* and adds expectedRevision, the 409 rule, the client rebase with new op  *)
(* ids, optimistic row ids and one active row per (list, object).          *)
(* TLA+ twin of verification/models/src/shopping.rs.                       *)
(*                                                                         *)
(* Server: Lucina/src/grocery/shopping.js::applyShoppingOps (and the Rust  *)
(* port lucina-grocery). Client: web/src/state/shopping.ts::flushOutbox,   *)
(* lib/shoppingOps.ts::rebaseOps.                                          *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS Scenario,        \* "S1" | "S2" | "S3"
          Drops,           \* lost requests/responses
          FixDupsIn409,    \* fix S1: the 409 body carries the duplicate op ids
          FixClientRowIds  \* fix S2: rowId kept as the row id, merges alias it

C == 1..2
Srv(n) == [k |-> "srv", a |-> n, b |-> 0]
Cli(c, n) == [k |-> "cli", a |-> c, b |-> n]
OWN(n) == [k |-> "own", a |-> 0, b |-> n]   \* template: the acting client's row n
K(t, o, r, sp) == [t |-> t, obj |-> o, row |-> r, spec |-> sp]
SEED == Srv(0)
Milk == [id |-> SEED, obj |-> 0, st |-> "needed", spec |-> FALSE, rev |-> 0]

Scripts == CASE Scenario = "S1" -> << <<K("add", 0, OWN(0), TRUE), K("purchase", 0, SEED, FALSE)>>,
                                      <<K("purchase", 0, SEED, FALSE)>> >>
             [] Scenario = "S2" -> << <<K("add", 1, OWN(0), FALSE), K("remove", 0, OWN(0), FALSE)>>,
                                      <<K("purchase", 0, SEED, FALSE)>> >>
             [] Scenario = "S3" -> << <<K("add", 1, OWN(0), TRUE), K("remove", 0, OWN(0), FALSE)>>,
                                      <<K("add", 1, OWN(0), FALSE), K("purchase", 0, SEED, FALSE)>> >>
MaxConflicts == 3
NORESP == [code |-> 0, dup |-> {}, ok |-> {}, rej |-> {}, rows |-> {}, rev |-> 0]
NOINF == [on |-> FALSE, batch |-> <<>>, resp |-> NORESP]
Intents == {<<c, k>> : c \in C, k \in 1..2}

VARIABLES revision, rows, nextRow, receipts, applied, aliases, clients,
          silentlyDropped, acked, everRemoved, resurrected, dropsLeft
vars == <<revision, rows, nextRow, receipts, applied, aliases, clients,
          silentlyDropped, acked, everRemoved, resurrected, dropsLeft>>

Retarget(k, from, to) == IF k.row = from THEN [k EXCEPT !.row = to] ELSE k
Active(rs, obj) == {r \in rs : r.obj = obj /\ r.st # "removed"}
View(rs) == {[id |-> r.id, obj |-> r.obj, st |-> r.st, spec |-> r.spec] : r \in rs}

(* applyOne: <<rows', ok, changed>> *)
Apply(rs, k, rev, newId) ==
    IF k.t = "add"
    THEN IF Active(rs, k.obj) # {}
         THEN LET r == CHOOSE x \in Active(rs, k.obj) : TRUE
                  r2 == [r EXCEPT !.st = "needed", !.spec = r.spec \/ k.spec, !.rev = rev]
                  rs2 == (rs \ {r}) \cup {r2}
              IN <<rs2, TRUE, View(rs2) # View(rs)>>
         ELSE LET rs2 == rs \cup {[id |-> newId, obj |-> k.obj, st |-> "needed", spec |-> k.spec, rev |-> rev]}
              IN <<rs2, TRUE, TRUE>>
    ELSE IF \A r \in rs : r.id # k.row THEN <<rs, FALSE, FALSE>>
    ELSE LET r == CHOOSE x \in rs : x.id = k.row
         IN IF r.st = "removed" THEN <<rs, TRUE, FALSE>>   \* deleted stays deleted
            ELSE LET st == CASE k.t = "purchase" -> "recent" [] k.t = "need" -> "needed" [] k.t = "remove" -> "removed"
                     rs2 == (rs \ {r}) \cup {[r EXCEPT !.st = st, !.rev = rev]}
                 IN <<rs2, TRUE, View(rs2) # View(rs)>>

Resolve(al, k) == IF \E p \in al : p[1] = k.row /\ k.t # "add"
                  THEN Retarget(k, k.row, (CHOOSE p \in al : p[1] = k.row)[2]) ELSE k

(* The server's transaction for one batch: a response record and the new server variables. *)
RECURSIVE Run(_, _, _)
Run(ops, st, rev) ==   \* st: [rows, nextRow, receipts, applied, aliases, ok, dup, rej]
    IF ops = <<>> THEN st
    ELSE LET op == Head(ops)
         IN IF op.id \in st.receipts
            THEN Run(Tail(ops), [st EXCEPT !.dup = @ \cup {op.id}], rev)
            ELSE LET k == Resolve(st.aliases, op.kind)
                     useCli == FixClientRowIds /\ k.t = "add" /\ k.row.k = "cli" /\ \A r \in st.rows : r.id # k.row
                     newId == IF useCli THEN k.row ELSE Srv(st.nextRow)
                     al2 == IF FixClientRowIds /\ k.t = "add" /\ k.row.k = "cli" /\ Active(st.rows, k.obj) # {}
                                /\ \A p \in st.aliases : p[1] # k.row
                            THEN st.aliases \cup {<<k.row, (CHOOSE r \in Active(st.rows, k.obj) : TRUE).id>>}
                            ELSE st.aliases
                     res == Apply(st.rows, k, rev, newId)
                     grew == res[2] /\ k.t = "add" /\ Active(st.rows, k.obj) = {} /\ ~useCli
                 IN Run(Tail(ops),
                        [st EXCEPT !.rows = res[1], !.aliases = al2,
                                   !.nextRow = IF grew THEN @ + 1 ELSE @,
                                   !.receipts = @ \cup {op.id},
                                   !.ok = IF res[2] THEN @ \cup {op.id} ELSE @,
                                   !.rej = IF res[2] THEN @ ELSE @ \cup {op.id},
                                   !.applied = IF res[2] /\ res[3] THEN [@ EXCEPT ![op.intent] = @ + 1] ELSE @],
                        rev)

Conflicting(batch, expected) ==
    \E i \in 1..Len(batch) :
        LET op == batch[i]
            k == Resolve(aliases, op.kind)
            row == IF k.t = "add" THEN (IF k.spec THEN Active(rows, k.obj) ELSE {})
                   ELSE {r \in rows : r.id = k.row}
        IN op.id \notin receipts /\ \E r \in row : r.rev > expected

Serve(c) ==
    /\ clients[c].inflight.on /\ clients[c].inflight.resp.code = 0
    /\ LET batch == clients[c].inflight.batch
           expected == clients[c].baseRev
           dups == {batch[i].id : i \in {j \in 1..Len(batch) : batch[j].id \in receipts}}
       IN IF expected < revision /\ Conflicting(batch, expected)
          THEN /\ clients' = [clients EXCEPT ![c].inflight.resp =
                                 [code |-> 409, dup |-> IF FixDupsIn409 THEN dups ELSE {},
                                  ok |-> {}, rej |-> {}, rows |-> {}, rev |-> 0]]
               /\ UNCHANGED <<revision, rows, nextRow, receipts, applied, aliases, everRemoved, resurrected>>
          ELSE LET st == Run(batch, [rows |-> rows, nextRow |-> nextRow, receipts |-> receipts,
                                     applied |-> applied, aliases |-> aliases,
                                     ok |-> {}, dup |-> {}, rej |-> {}], revision + 1)
                   rev2 == IF st.ok # {} THEN revision + 1 ELSE revision
               IN /\ rows' = st.rows /\ nextRow' = st.nextRow /\ receipts' = st.receipts
                  /\ applied' = st.applied /\ aliases' = st.aliases /\ revision' = rev2
                  /\ everRemoved' = everRemoved \cup {r.id : r \in {x \in st.rows : x.st = "removed"}}
                  /\ resurrected' = (resurrected \/ \E r \in st.rows : r.id \in everRemoved /\ r.st # "removed")
                  /\ clients' = [clients EXCEPT ![c].inflight.resp =
                                    [code |-> 200, dup |-> st.dup, ok |-> st.ok, rej |-> st.rej,
                                     rows |-> st.rows, rev |-> rev2]]
    /\ UNCHANGED <<silentlyDropped, acked, dropsLeft>>

(* rebaseOps: drop ops on rows that are gone or that change nothing; new op ids. *)
RECURSIVE Rebase(_, _, _, _)
Rebase(ops, items, out, n) ==   \* <<ops, next op counter>>
    IF ops = <<>> THEN <<out, n>>
    ELSE LET op == Head(ops)
             gone == op.kind.t # "add" /\ ~\E r \in items : r.id = op.kind.row /\ r.st # "removed"
             res == Apply(items, op.kind, 0, op.kind.row)
         IN IF gone \/ ~res[2] \/ ~res[3] THEN Rebase(Tail(ops), items, out, n)
            ELSE Rebase(Tail(ops), res[1], Append(out, [op EXCEPT !.id = <<op.id[1], n>>]), n + 1)

RECURSIVE RemapAll(_, _, _)
RemapAll(q, from, to) == IF q = <<>> THEN <<>>
                         ELSE <<[Head(q) EXCEPT !.kind = Retarget(@, from, to)]>> \o RemapAll(Tail(q), from, to)

RECURSIVE Remap(_, _, _)
Remap(cl, batch, rs) ==   \* remapIds: optimistic add rows -> the server row of their object
    IF batch = <<>> THEN cl
    ELSE LET op == Head(batch)
             k == op.kind
         IN IF k.t = "add" /\ (\A r \in rs : r.id # k.row) /\ Active(rs, k.obj) # {}
            THEN LET to == (CHOOSE r \in Active(rs, k.obj) : TRUE).id
                 IN Remap([cl EXCEPT !.outbox = RemapAll(@, k.row, to), !.alias = @ \cup {<<k.row, to>>}],
                          Tail(batch), rs)
            ELSE Remap(cl, Tail(batch), rs)

Receive(c) ==
    /\ clients[c].inflight.on /\ clients[c].inflight.resp.code # 0
    /\ LET cl == clients[c]
           batch == cl.inflight.batch
           resp == cl.inflight.resp
           sent == {batch[i].id : i \in 1..Len(batch)}
       IN IF resp.code = 200
          THEN LET cl1 == [cl EXCEPT !.inflight = NOINF, !.conflicts = 0,
                                     !.outbox = SelectSeq(@, LAMBDA o : o.id \notin sent)]
                   cl2 == Remap(cl1, batch, resp.rows)
                   cl3 == IF resp.rev >= cl.baseRev THEN [cl2 EXCEPT !.baseRev = resp.rev, !.base = View(resp.rows)] ELSE cl2
               IN /\ clients' = [clients EXCEPT ![c] = cl3]
                  /\ silentlyDropped' = silentlyDropped \cup {batch[i].intent : i \in {j \in 1..Len(batch) : batch[j].id \in resp.rej}}
                  /\ acked' = acked \cup {batch[i].intent : i \in {j \in 1..Len(batch) : batch[j].id \in resp.ok \cup resp.dup}}
          ELSE LET cl1 == [cl EXCEPT !.inflight = NOINF, !.conflicts = @ + 1, !.baseRev = revision, !.base = View(rows)]
                   kept == SelectSeq(cl.outbox, LAMBDA o : o.id \notin resp.dup)
                   rb == Rebase(kept, rows, <<>>, cl.nextOp)
                   cl2 == IF cl1.conflicts > MaxConflicts THEN [cl1 EXCEPT !.outbox = <<>>]
                          ELSE [cl1 EXCEPT !.outbox = rb[1], !.nextOp = rb[2]]
               IN /\ clients' = [clients EXCEPT ![c] = cl2]
                  /\ UNCHANGED <<silentlyDropped, acked>>
    /\ UNCHANGED <<revision, rows, nextRow, receipts, applied, aliases, everRemoved, resurrected, dropsLeft>>

User(c) ==
    /\ clients[c].scriptPos <= Len(Scripts[c])
    /\ LET cl == clients[c]
           t == Scripts[c][cl.scriptPos]
           k0 == IF t.row.k = "own" THEN [t EXCEPT !.row = Cli(c, t.row.b)] ELSE t
           k == IF k0.t # "add" /\ \E p \in cl.alias : p[1] = k0.row
                THEN [k0 EXCEPT !.row = (CHOOSE p \in cl.alias : p[1] = k0.row)[2]] ELSE k0
           op == [id |-> <<c, cl.nextOp>>, intent |-> <<c, cl.scriptPos>>, kind |-> k]
       IN clients' = [clients EXCEPT ![c].outbox = Append(@, op), ![c].nextOp = @ + 1, ![c].scriptPos = @ + 1]
    /\ UNCHANGED <<revision, rows, nextRow, receipts, applied, aliases, silentlyDropped, acked,
                   everRemoved, resurrected, dropsLeft>>

Send(c) ==
    /\ ~clients[c].inflight.on /\ clients[c].outbox # <<>>
    /\ clients' = [clients EXCEPT ![c].inflight = [on |-> TRUE, batch |-> clients[c].outbox, resp |-> NORESP]]
    /\ UNCHANGED <<revision, rows, nextRow, receipts, applied, aliases, silentlyDropped, acked,
                   everRemoved, resurrected, dropsLeft>>

Lose(c) ==   \* transient: the outbox stays, same op ids next time
    /\ clients[c].inflight.on /\ dropsLeft > 0
    /\ clients' = [clients EXCEPT ![c].inflight = NOINF]
    /\ dropsLeft' = dropsLeft - 1
    /\ UNCHANGED <<revision, rows, nextRow, receipts, applied, aliases, silentlyDropped, acked,
                   everRemoved, resurrected>>

Init ==
    /\ revision = 0 /\ rows = {Milk} /\ nextRow = 1 /\ receipts = {}
    /\ applied = [i \in Intents |-> 0] /\ aliases = {}
    /\ clients = [c \in C |-> [outbox |-> <<>>, baseRev |-> 0, base |-> View({Milk}), inflight |-> NOINF,
                               nextOp |-> 0, scriptPos |-> 1, conflicts |-> 0, alias |-> {}]]
    /\ silentlyDropped = {} /\ acked = {} /\ everRemoved = {} /\ resurrected = FALSE
    /\ dropsLeft = Drops

Next == \E c \in C : User(c) \/ Send(c) \/ Serve(c) \/ Receive(c) \/ Lose(c)
Spec == Init /\ [][Next]_vars

NoDuplicateApplication == \A i \in Intents : applied[i] <= 1
AckedAppliedExactlyOnce == \A i \in acked : applied[i] <= 1
NoSilentDrop == silentlyDropped = {}
DeletedStaysDeleted == ~resurrected
OneActiveRowPerObject == \A o \in {r.obj : r \in rows} : Cardinality(Active(rows, o)) <= 1
RevisionIdentifiesState == \A c \in C : clients[c].baseRev = revision => clients[c].base = View(rows)
NeverConflicted == \A c \in C : clients[c].conflicts = 0
=============================================================================
