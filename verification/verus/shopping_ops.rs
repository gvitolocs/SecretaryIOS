// Shopping v2 op application kernel (one list, one op batch per transaction).
// Complements sync_kernel.rs (the v1 lucina-core item map) with the v2 server rules.
//
// Production: crates/lucina-grocery/src/lib.rs on branch lucina-rs (the Rust port of
// Lucina's src/grocery/shopping.js, docs/shopping-v2.md):
//   apply_batch        <-> GroceryRepo::apply_ops (dedup against grocery.shopping_ops
//                          receipts and within the batch, the expectedRevision 409 check,
//                          one SAVEPOINT per op, the list revision bump, receipts)
//   fresh_ops          <-> the `seen` / `fresh` / `duplicates` split
//   touched_rev        <-> GroceryRepo::find_conflicts (per op: the rev of the row it
//                          would change; add only when it overwrites spec/quantity/unit/section)
//   step               <-> GroceryRepo::apply_one for add / need / purchase / remove /
//                          set_spec|set_quantity|set_section (Edit) / rename / move /
//                          clear_recent; a rename onto another active row's key is the
//                          unique index shopping_list_items_one_active_per_key (23505 ->
//                          rejected `duplicate_item`)
//   Row.key            <-> COALESCE(catalog_item_id, name_key) (item object id on lucina-items)
// Differential test: lucina-grocery/tests/verus_ops.rs (branch lucina-proofs), random op
// batches against Postgres and a plain-Rust copy of this model.
//
// Proven:
//   S1 batch_replay_is_noop: re-delivering a committed batch (any expectedRevision)
//      changes nothing: every op is a duplicate (idempotency by opId).
//   S2 revision: +1 exactly when the batch applied an op, unchanged otherwise and on 409.
//   S3 removed_stays_removed: no op brings a removed row back (re-adding makes a new row).
//   S4 one_active_row_per_key: the invariant holds after every batch.
//   S5 the 409 rule: a stale expectedRevision is a conflict iff a fresh op would change a
//      row changed after it, and then nothing is applied; every row a batch changes
//      carries the new revision, so a later stale write to it is a 409.
use vstd::prelude::*;

verus! {

pub enum Status {
    Needed,
    Recent,
    Removed,
}

pub struct Row {
    pub key: nat,
    pub status: Status,
    pub rev: int,
}

pub enum Op {
    Add { key: nat, overwrites: bool },
    Need(nat),
    Purchase(nat),
    Remove(nat),
    Edit(nat),
    Rename(nat, nat),
    /// Move to another list; `true` when that list already has the key active, so the
    /// row is merged into it and marked removed here (else it leaves this list).
    Move(nat, bool),
    ClearRecent,
}

pub struct TaggedOp {
    pub id: nat,
    pub op: Op,
}

pub struct St {
    pub rows: Map<nat, Row>,
    pub next_row: nat,
}

pub struct ListState {
    pub st: St,
    pub revision: int,
    pub receipts: Set<nat>,
}

pub enum BatchResult {
    Conflict,
    Done(ListState, nat),
}

pub open spec fn active(r: Row) -> bool {
    !(r.status is Removed)
}

pub open spec fn has_active(rows: Map<nat, Row>, key: nat) -> bool {
    exists|r: nat| rows.dom().contains(r) && active(#[trigger] rows[r]) && rows[r].key == key
}

pub open spec fn the_active(rows: Map<nat, Row>, key: nat) -> nat
    recommends
        has_active(rows, key),
{
    choose|r: nat| rows.dom().contains(r) && active(#[trigger] rows[r]) && rows[r].key == key
}

pub open spec fn touch_status(s: St, r: nat, rev: int, status: Status) -> (St, bool) {
    if !s.rows.dom().contains(r) {
        (s, false)
    } else if !active(s.rows[r]) {
        (s, true)
    } else {
        (St { rows: s.rows.insert(r, Row { key: s.rows[r].key, status, rev }), next_row: s.next_row }, true)
    }
}

pub open spec fn clear_recent(rows: Map<nat, Row>, rev: int) -> Map<nat, Row> {
    Map::new(
        rows.dom(),
        |k: nat|
            if rows[k].status is Recent {
                Row { key: rows[k].key, status: Status::Removed, rev }
            } else {
                rows[k]
            },
    )
}

/// apply_one: the new state and whether the op was applied (false = rejected).
pub open spec fn step(s: St, rev: int, op: Op) -> (St, bool) {
    match op {
        Op::Add { key, overwrites: _ } => {
            if has_active(s.rows, key) {
                let r = the_active(s.rows, key);
                (
                    St {
                        rows: s.rows.insert(r, Row { key, status: Status::Needed, rev }),
                        next_row: s.next_row,
                    },
                    true,
                )
            } else {
                (
                    St {
                        rows: s.rows.insert(s.next_row, Row { key, status: Status::Needed, rev }),
                        next_row: s.next_row + 1,
                    },
                    true,
                )
            }
        },
        Op::Need(r) => touch_status(s, r, rev, Status::Needed),
        Op::Purchase(r) => touch_status(s, r, rev, Status::Recent),
        Op::Remove(r) => touch_status(s, r, rev, Status::Removed),
        Op::Edit(r) => {
            if !s.rows.dom().contains(r) {
                (s, false)
            } else if !active(s.rows[r]) {
                (s, true)
            } else {
                (
                    St {
                        rows: s.rows.insert(r, Row { key: s.rows[r].key, status: s.rows[r].status, rev }),
                        next_row: s.next_row,
                    },
                    true,
                )
            }
        },
        Op::Rename(r, key) => {
            if !s.rows.dom().contains(r) {
                (s, false)
            } else if !active(s.rows[r]) {
                (s, true)
            } else if has_active(s.rows, key) && the_active(s.rows, key) != r {
                (s, false)
            } else {
                (
                    St {
                        rows: s.rows.insert(r, Row { key, status: s.rows[r].status, rev }),
                        next_row: s.next_row,
                    },
                    true,
                )
            }
        },
        Op::Move(r, merge) => {
            if !s.rows.dom().contains(r) {
                (s, false)
            } else if !active(s.rows[r]) {
                (s, true)
            } else if merge {
                touch_status(s, r, rev, Status::Removed)
            } else {
                (St { rows: s.rows.remove(r), next_row: s.next_row }, true)
            }
        },
        Op::ClearRecent => (St { rows: clear_recent(s.rows, rev), next_row: s.next_row }, true),
    }
}

pub open spec fn apply_ops(s: St, rev: int, ops: Seq<TaggedOp>) -> (St, nat)
    decreases ops.len(),
{
    if ops.len() == 0 {
        (s, 0)
    } else {
        let (s1, a) = step(s, rev, ops[0].op);
        let (s2, n) = apply_ops(s1, rev, ops.drop_first());
        (
            s2,
            n + if a {
                1nat
            } else {
                0nat
            },
        )
    }
}

/// The ops not seen before (receipts, or earlier in the batch).
pub open spec fn fresh_ops(ops: Seq<TaggedOp>, seen: Set<nat>) -> Seq<TaggedOp>
    decreases ops.len(),
{
    if ops.len() == 0 {
        Seq::empty()
    } else if seen.contains(ops[0].id) {
        fresh_ops(ops.drop_first(), seen)
    } else {
        seq![ops[0]] + fresh_ops(ops.drop_first(), seen.insert(ops[0].id))
    }
}

pub open spec fn ids(ops: Seq<TaggedOp>) -> Set<nat>
    decreases ops.len(),
{
    if ops.len() == 0 {
        Set::empty()
    } else {
        ids(ops.drop_first()).insert(ops[0].id)
    }
}

proof fn lemma_ids(ops: Seq<TaggedOp>)
    ensures
        forall|x: nat| ids(ops).contains(x) <==> exists|i: int| 0 <= i < ops.len() && (#[trigger] ops[i]).id == x,
    decreases ops.len(),
{
    if ops.len() > 0 {
        let rest = ops.drop_first();
        lemma_ids(rest);
        assert(ids(ops) == ids(rest).insert(ops[0].id));
        assert forall|x: nat| ids(ops).contains(x) implies exists|i: int| 0 <= i < ops.len() && (#[trigger] ops[i]).id == x by {
            if x == ops[0].id {
                assert(ops[0].id == x);
            } else {
                assert(ids(rest).contains(x));
                let j = choose|j: int| 0 <= j < rest.len() && (#[trigger] rest[j]).id == x;
                assert(ops[j + 1] == rest[j]);
            }
        }
        assert forall|x: nat| (exists|i: int| 0 <= i < ops.len() && (#[trigger] ops[i]).id == x) implies ids(ops).contains(x) by {
            let i = choose|i: int| 0 <= i < ops.len() && (#[trigger] ops[i]).id == x;
            if i > 0 {
                assert(rest[i - 1] == ops[i]);
                assert(ids(rest).contains(x));
            }
        }
    }
}

/// find_conflicts: the rev of the row an op would change.
pub open spec fn touched_rev(s: St, op: Op) -> Option<int> {
    match op {
        Op::Add { key, overwrites } => {
            if overwrites && has_active(s.rows, key) {
                Some(s.rows[the_active(s.rows, key)].rev)
            } else {
                None
            }
        },
        Op::ClearRecent => None,
        Op::Need(r) | Op::Purchase(r) | Op::Remove(r) | Op::Edit(r) | Op::Rename(r, _) | Op::Move(r, _) => {
            if s.rows.dom().contains(r) {
                Some(s.rows[r].rev)
            } else {
                None
            }
        },
    }
}

pub open spec fn touched_after(s: St, op: Op, expected: int) -> bool {
    match touched_rev(s, op) {
        Some(r) => r > expected,
        None => false,
    }
}

pub open spec fn conflict(s: St, fresh: Seq<TaggedOp>, expected: int) -> bool {
    exists|i: int| 0 <= i < fresh.len() && #[trigger] touched_after(s, fresh[i].op, expected)
}

/// The 409 condition of apply_ops.
pub open spec fn stale_conflict(l: ListState, fresh: Seq<TaggedOp>, expected: Option<int>) -> bool {
    match expected {
        Some(e) => e < l.revision && conflict(l.st, fresh, e),
        None => false,
    }
}

pub open spec fn apply_batch(l: ListState, ops: Seq<TaggedOp>, expected: Option<int>) -> BatchResult {
    let fresh = fresh_ops(ops, l.receipts);
    if stale_conflict(l, fresh, expected) {
        BatchResult::Conflict
    } else {
        let (s2, n) = apply_ops(l.st, l.revision + 1, fresh);
        BatchResult::Done(
            ListState {
                st: s2,
                revision: if n > 0 {
                    l.revision + 1
                } else {
                    l.revision
                },
                receipts: l.receipts.union(ids(fresh)),
            },
            n,
        )
    }
}

// ---------------------------------------------------------------- invariants

pub open spec fn unique_active(rows: Map<nat, Row>) -> bool {
    forall|a: nat, b: nat|
        #![trigger rows[a], rows[b]]
        rows.dom().contains(a) && rows.dom().contains(b) && a != b && active(rows[a]) && active(
            rows[b],
        ) ==> rows[a].key != rows[b].key
}

pub open spec fn inv(s: St) -> bool {
    unique_active(s.rows) && forall|r: nat| #[trigger] s.rows.dom().contains(r) ==> r < s.next_row
}

/// Removed rows stay (in the map) and stay removed.
pub open spec fn keeps_removed(s: St, t: St) -> bool {
    forall|r: nat|
        #[trigger] s.rows.dom().contains(r) && !active(s.rows[r]) ==> t.rows.dom().contains(r)
            && !active(t.rows[r])
}

/// Every row that differs carries `rev`.
pub open spec fn stamps(s: St, t: St, rev: int) -> bool {
    forall|r: nat|
        #[trigger] t.rows.dom().contains(r) ==> t.rows[r].rev == rev || (s.rows.dom().contains(r)
            && t.rows[r] == s.rows[r])
}

proof fn lemma_the_active(rows: Map<nat, Row>, key: nat, b: nat)
    requires
        unique_active(rows),
        rows.dom().contains(b),
        active(rows[b]),
        rows[b].key == key,
    ensures
        has_active(rows, key),
        the_active(rows, key) == b,
{
    assert(rows.dom().contains(b) && active(rows[b]) && rows[b].key == key);
    let a = the_active(rows, key);
    if a != b {
        assert(rows[a].key != rows[b].key);
    }
}

pub proof fn lemma_step(s: St, rev: int, op: Op)
    requires
        inv(s),
    ensures
        inv(step(s, rev, op).0),
        keeps_removed(s, step(s, rev, op).0),
        stamps(s, step(s, rev, op).0, rev),
        step(s, rev, op).0.next_row >= s.next_row,
{
    let t = step(s, rev, op).0;
    match op {
        Op::Add { key, overwrites } => {
            if has_active(s.rows, key) {
                let r = the_active(s.rows, key);
                assert forall|a: nat, b: nat|
                    #![trigger t.rows[a], t.rows[b]]
                    t.rows.dom().contains(a) && t.rows.dom().contains(b) && a != b && active(
                        t.rows[a],
                    ) && active(t.rows[b]) implies t.rows[a].key != t.rows[b].key by {
                    if a == r {
                        assert(s.rows[b] == t.rows[b]);
                        assert(s.rows[r].key == key);
                        assert(s.rows[r].key != s.rows[b].key);
                    } else if b == r {
                        assert(s.rows[a] == t.rows[a]);
                        assert(s.rows[r].key != s.rows[a].key);
                    } else {
                        assert(s.rows[a] == t.rows[a] && s.rows[b] == t.rows[b]);
                    }
                }
            } else {
                let n = s.next_row;
                assert(!s.rows.dom().contains(n));
                assert forall|a: nat, b: nat|
                    #![trigger t.rows[a], t.rows[b]]
                    t.rows.dom().contains(a) && t.rows.dom().contains(b) && a != b && active(
                        t.rows[a],
                    ) && active(t.rows[b]) implies t.rows[a].key != t.rows[b].key by {
                    if a == n {
                        assert(s.rows[b] == t.rows[b]);
                        assert(!(s.rows.dom().contains(b) && active(s.rows[b]) && s.rows[b].key
                            == key));
                    } else if b == n {
                        assert(s.rows[a] == t.rows[a]);
                        assert(!(s.rows.dom().contains(a) && active(s.rows[a]) && s.rows[a].key
                            == key));
                    } else {
                        assert(s.rows[a] == t.rows[a] && s.rows[b] == t.rows[b]);
                    }
                }
            }
        },
        Op::Need(r) | Op::Purchase(r) | Op::Remove(r) | Op::Edit(r) => {
            if s.rows.dom().contains(r) && active(s.rows[r]) {
                assert forall|a: nat, b: nat|
                    #![trigger t.rows[a], t.rows[b]]
                    t.rows.dom().contains(a) && t.rows.dom().contains(b) && a != b && active(
                        t.rows[a],
                    ) && active(t.rows[b]) implies t.rows[a].key != t.rows[b].key by {
                    assert(t.rows[a].key == s.rows[a].key && t.rows[b].key == s.rows[b].key);
                    assert(active(s.rows[a]) && active(s.rows[b]));
                }
            }
        },
        Op::Rename(r, key) => {
            if s.rows.dom().contains(r) && active(s.rows[r]) && !(has_active(s.rows, key)
                && the_active(s.rows, key) != r) {
                assert forall|a: nat, b: nat|
                    #![trigger t.rows[a], t.rows[b]]
                    t.rows.dom().contains(a) && t.rows.dom().contains(b) && a != b && active(
                        t.rows[a],
                    ) && active(t.rows[b]) implies t.rows[a].key != t.rows[b].key by {
                    if a == r {
                        assert(s.rows[b] == t.rows[b]);
                        if s.rows[b].key == key {
                            lemma_the_active(s.rows, key, b);
                        }
                    } else if b == r {
                        assert(s.rows[a] == t.rows[a]);
                        if s.rows[a].key == key {
                            lemma_the_active(s.rows, key, a);
                        }
                    } else {
                        assert(s.rows[a] == t.rows[a] && s.rows[b] == t.rows[b]);
                    }
                }
            }
        },
        Op::Move(r, merge) => {
            if merge && s.rows.dom().contains(r) && active(s.rows[r]) {
                assert forall|a: nat, b: nat|
                    #![trigger t.rows[a], t.rows[b]]
                    t.rows.dom().contains(a) && t.rows.dom().contains(b) && a != b && active(
                        t.rows[a],
                    ) && active(t.rows[b]) implies t.rows[a].key != t.rows[b].key by {
                    assert(t.rows[a].key == s.rows[a].key && t.rows[b].key == s.rows[b].key);
                    assert(active(s.rows[a]) && active(s.rows[b]));
                }
            }
        },
        Op::ClearRecent => {
            assert forall|a: nat, b: nat|
                #![trigger t.rows[a], t.rows[b]]
                t.rows.dom().contains(a) && t.rows.dom().contains(b) && a != b && active(t.rows[a])
                    && active(t.rows[b]) implies t.rows[a].key != t.rows[b].key by {
                assert(t.rows[a].key == s.rows[a].key && t.rows[b].key == s.rows[b].key);
                assert(active(s.rows[a]) && active(s.rows[b]));
            }
        },
    }
}

pub proof fn lemma_apply_ops(s: St, rev: int, ops: Seq<TaggedOp>)
    requires
        inv(s),
    ensures
        inv(apply_ops(s, rev, ops).0),
        keeps_removed(s, apply_ops(s, rev, ops).0),
        stamps(s, apply_ops(s, rev, ops).0, rev),
        apply_ops(s, rev, ops).1 == 0 ==> apply_ops(s, rev, ops).0 == s,
    decreases ops.len(),
{
    if ops.len() > 0 {
        let (s1, a) = step(s, rev, ops[0].op);
        lemma_step(s, rev, ops[0].op);
        lemma_apply_ops(s1, rev, ops.drop_first());
        let s2 = apply_ops(s1, rev, ops.drop_first()).0;
        // keeps_removed and stamps compose
        assert forall|r: nat| #[trigger] s.rows.dom().contains(r) && !active(s.rows[r]) implies s2.rows.dom().contains(r)
            && !active(s2.rows[r]) by {
            assert(s1.rows.dom().contains(r) && !active(s1.rows[r]));
        }
        assert forall|r: nat| #[trigger] s2.rows.dom().contains(r) implies s2.rows[r].rev == rev || (s.rows.dom().contains(r)
            && s2.rows[r] == s.rows[r]) by {
            if s2.rows[r].rev != rev {
                assert(s1.rows.dom().contains(r) && s2.rows[r] == s1.rows[r]);
            }
        }
        if !a {
            // a rejected op leaves the state as it was
            match ops[0].op {
                Op::Rename(r, key) => {},
                _ => {},
            }
            assert(s1 == s);
        }
    }
}

proof fn lemma_fresh(ops: Seq<TaggedOp>, seen: Set<nat>)
    ensures
        forall|i: int| 0 <= i < ops.len() ==> seen.contains((#[trigger] ops[i]).id) || ids(fresh_ops(ops, seen)).contains(ops[i].id),
        forall|i: int| 0 <= i < fresh_ops(ops, seen).len() ==> !seen.contains((#[trigger] fresh_ops(ops, seen)[i]).id),
    decreases ops.len(),
{
    if ops.len() > 0 {
        let rest = ops.drop_first();
        if seen.contains(ops[0].id) {
            lemma_fresh(rest, seen);
            assert forall|i: int| 0 <= i < ops.len() implies seen.contains((#[trigger] ops[i]).id) || ids(fresh_ops(ops, seen)).contains(ops[i].id) by {
                if i > 0 {
                    assert(ops[i] == rest[i - 1]);
                }
            }
        } else {
            let s2 = seen.insert(ops[0].id);
            lemma_fresh(rest, s2);
            let f = fresh_ops(ops, seen);
            let fr = fresh_ops(rest, s2);
            lemma_ids(f);
            lemma_ids(fr);
            assert(f == seq![ops[0]] + fr);
            assert(f[0] == ops[0]);
            assert forall|i: int| 0 <= i < ops.len() implies seen.contains((#[trigger] ops[i]).id) || ids(f).contains(ops[i].id) by {
                if i == 0 {
                    assert(ids(f).contains(ops[0].id));
                } else {
                    assert(ops[i] == rest[i - 1]);
                    if !seen.contains(ops[i].id) && ops[i].id != ops[0].id {
                        assert(ids(fr).contains(ops[i].id));
                        let j = choose|j: int| 0 <= j < fr.len() && (#[trigger] fr[j]).id == ops[i].id;
                        assert(f[j + 1] == fr[j]);
                        assert(ids(f).contains(ops[i].id));
                    }
                }
            }
            assert forall|i: int| 0 <= i < f.len() implies !seen.contains((#[trigger] f[i]).id) by {
                if i > 0 {
                    assert(f[i] == fr[i - 1]);
                }
            }
        }
    }
}

proof fn lemma_fresh_empty(ops: Seq<TaggedOp>, seen: Set<nat>)
    requires
        forall|i: int| 0 <= i < ops.len() ==> seen.contains((#[trigger] ops[i]).id),
    ensures
        fresh_ops(ops, seen) == Seq::<TaggedOp>::empty(),
    decreases ops.len(),
{
    if ops.len() > 0 {
        let rest = ops.drop_first();
        assert forall|i: int| 0 <= i < rest.len() implies seen.contains((#[trigger] rest[i]).id) by {
            assert(rest[i] == ops[i + 1]);
        }
        assert(seen.contains(ops[0].id));
        lemma_fresh_empty(rest, seen);
    }
}

// ---------------------------------------------------------------- theorems

/// S2 + S3 + S4 + S5 for one batch.
pub proof fn batch_properties(l: ListState, ops: Seq<TaggedOp>, expected: Option<int>)
    requires
        inv(l.st),
    ensures
        match apply_batch(l, ops, expected) {
            BatchResult::Conflict => stale_conflict(l, fresh_ops(ops, l.receipts), expected),
            BatchResult::Done(l2, n) => {
                &&& inv(l2.st)
                &&& keeps_removed(l.st, l2.st)
                &&& stamps(l.st, l2.st, l.revision + 1)
                &&& (n > 0 ==> l2.revision == l.revision + 1)
                &&& (n == 0 ==> l2.revision == l.revision && l2.st == l.st)
                &&& l.receipts.subset_of(l2.receipts)
                &&& (forall|i: int| 0 <= i < ops.len() ==> l2.receipts.contains((#[trigger] ops[i]).id))
                &&& !stale_conflict(l, fresh_ops(ops, l.receipts), expected)
            },
        },
{
    let fresh = fresh_ops(ops, l.receipts);
    lemma_apply_ops(l.st, l.revision + 1, fresh);
    lemma_fresh(ops, l.receipts);
}

/// S1: replaying a committed batch is a no-op, whatever expectedRevision it carries.
pub proof fn batch_replay_is_noop(
    l: ListState,
    ops: Seq<TaggedOp>,
    expected: Option<int>,
    expected2: Option<int>,
)
    requires
        inv(l.st),
        apply_batch(l, ops, expected) is Done,
    ensures
        ({
            let l1 = apply_batch(l, ops, expected)->Done_0;
            apply_batch(l1, ops, expected2) == BatchResult::Done(l1, 0)
        }),
{
    batch_properties(l, ops, expected);
    let l1 = apply_batch(l, ops, expected)->Done_0;
    lemma_fresh_empty(ops, l1.receipts);
    let f2 = fresh_ops(ops, l1.receipts);
    lemma_ids(f2);
    assert(ids(f2) =~= Set::<nat>::empty());
    assert(l1.receipts.union(ids(f2)) =~= l1.receipts);
    assert(!stale_conflict(l1, f2, expected2));
}

/// S5, the other direction: after a batch, a client still at an older revision that
/// writes to a row this batch changed gets a 409.
pub proof fn stale_write_to_changed_row_conflicts(
    l: ListState,
    ops: Seq<TaggedOp>,
    expected: Option<int>,
    later: Seq<TaggedOp>,
    stale: int,
    i: int,
    r: nat,
)
    requires
        inv(l.st),
        apply_batch(l, ops, expected) is Done,
        stale <= l.revision,
        ({
            let l1 = apply_batch(l, ops, expected)->Done_0;
            &&& 0 <= i < fresh_ops(later, l1.receipts).len()
            &&& touched_rev(l1.st, fresh_ops(later, l1.receipts)[i].op) == Some(l1.st.rows[r].rev)
            &&& l1.st.rows.dom().contains(r)
            &&& !(l.st.rows.dom().contains(r) && l1.st.rows[r] == l.st.rows[r])
        }),
    ensures
        apply_batch(apply_batch(l, ops, expected)->Done_0, later, Some(stale)) is Conflict,
{
    batch_properties(l, ops, expected);
    let l1 = apply_batch(l, ops, expected)->Done_0;
    assert(l1.st.rows[r].rev == l.revision + 1);
    if l1.revision == l.revision {
        assert(l1.st == l.st);
    }
    let f = fresh_ops(later, l1.receipts);
    assert(touched_after(l1.st, f[i].op, stale));
}

} // verus!
fn main() {}
