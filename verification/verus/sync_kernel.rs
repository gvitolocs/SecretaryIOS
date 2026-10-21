// Verus-verified kernel of the lucina-core sync semantics.
//
// Scope (deliberate): the mathematical core of crates/lucina-core — the
// op application function `apply`, its Hoare contract, and the two
// properties the TLC/Quint models check:
//   T1  apply is idempotent (replay of a committed op changes nothing)
//   T2  deleted-stays-deleted: with the op-id dedup gate, a delayed
//       duplicate of an Add can never resurrect a deleted item
//       (the exact scenario the Quint/TLC witness proves breaks a
//       naive, gate-less server).
// `step_state` models the gate; `apply` models the raw effect. Everything
// is Hoare-style: every function carries requires/ensures contracts.

use vstd::map::*;
use vstd::prelude::*;

verus! {

pub struct Item {
    pub uuid: nat,
    pub checked: bool,
}

#[is_variant]
pub enum OpKind {
    Add,
    Check,
    Uncheck,
    Delete,
}

/// A wire op. `id` is the unique op-id (uuid) that the server's dedup
/// gate keys on; `item` is the target item uuid.
pub struct TaggedOp {
    pub id: nat,
    pub item: nat,
    pub kind: OpKind,
}

/// Raw effect of one op on the item map (uuid -> checked flag).
/// This is the executable semantics of `apply_op` in models.rs.
pub open spec fn apply(s: Map<nat, bool>, op: TaggedOp) -> Map<nat, bool> {
    match op.kind {
        OpKind::Add => {
            if s.dom().contains(op.item) {
                s
            } else {
                s.insert(op.item, false)
            }
        },
        OpKind::Check => {
            if s.dom().contains(op.item) {
                s.insert(op.item, true)
            } else {
                s
            }
        },
        OpKind::Uncheck => {
            if s.dom().contains(op.item) {
                s.insert(op.item, false)
            } else {
                s
            }
        },
        OpKind::Delete => s.remove(op.item),
    }
}

/// One server-side delivery through the op-id dedup gate: a duplicate
/// delivery (id already applied) is a no-op.
pub open spec fn step_state(s: Map<nat, bool>, applied: Set<nat>, op: TaggedOp) -> Map<nat, bool> {
    if applied.contains(op.id) {
        s
    } else {
        apply(s, op)
    }
}

pub open spec fn step_applied(applied: Set<nat>, op: TaggedOp) -> Set<nat> {
    if applied.contains(op.id) {
        applied
    } else {
        applied.insert(op.id)
    }
}

// ---------------------------------------------------------------------------
// T1: idempotency of the raw effect
// ---------------------------------------------------------------------------

proof fn lemma_apply_idempotent(s: Map<nat, bool>, op: TaggedOp)
    ensures
        apply(apply(s, op), op) == apply(s, op),
{
    match op.kind {
        OpKind::Add => {
            if s.dom().contains(op.item) {
                assert(apply(s, op) == s);
            } else {
                let s1 = s.insert(op.item, false);
                assert(s1.dom().contains(op.item));
                assert(apply(s1, op) == s1);
            }
        },
        OpKind::Check => {
            if s.dom().contains(op.item) {
                let s1 = s.insert(op.item, true);
                assert(s1.dom().contains(op.item));
                assert(apply(s1, op) == s1);
            } else {
                assert(apply(s, op) == s);
            }
        },
        OpKind::Uncheck => {
            if s.dom().contains(op.item) {
                let s1 = s.insert(op.item, false);
                assert(s1.dom().contains(op.item));
                assert(apply(s1, op) == s1);
            } else {
                assert(apply(s, op) == s);
            }
        },
        OpKind::Delete => {
            let s1 = s.remove(op.item);
            assert(!s1.dom().contains(op.item));
            assert(apply(s1, op) == s1);
        },
    }
}

// ---------------------------------------------------------------------------
// T2: deleted-stays-deleted (the kernel theorem)
// ---------------------------------------------------------------------------

/// Delivering a duplicate of an already-applied op never changes state.
proof fn lemma_dup_delivery_noop(s: Map<nat, bool>, applied: Set<nat>, op: TaggedOp)
    requires
        applied.contains(op.id),
    ensures
        step_state(s, applied, op) == s,
        step_applied(applied, op) == applied,
{
}

/// The FIFO scenario of the sync model: client commits Add(id1, i) then
/// Delete(id2, i); the server applies them in order; afterwards a delayed
/// duplicate of the Add arrives. With the gate, the item stays deleted.
///
/// Without the gate this fails -- exactly the Quint/TLC witness
/// (migratedSyncWitness / MigratedSync_witness).
proof fn lemma_deleted_stays_deleted(s: Map<nat, bool>, applied: Set<nat>, add: TaggedOp, del: TaggedOp)
    requires
        add.kind == OpKind::Add,
        del.kind == OpKind::Delete,
        add.item == del.item,
        add.id != del.id,
        // del is a fresh op on its first delivery (FIFO model guarantee)
        !applied.contains(del.id),
    ensures
        ({
            let s1 = step_state(s, applied, add);
            let a1 = step_applied(applied, add);
            let s2 = step_state(s1, a1, del);
            let a2 = step_applied(a1, del);
            let s3 = step_state(s2, a2, add);
            !s3.dom().contains(add.item)
        }),
{
    let s1 = step_state(s, applied, add);
    let a1 = step_applied(applied, add);
    // after the add is processed, its id is recorded, so a later duplicate
    // delivery of it is a no-op by T2's helper lemma:
    assert(a1.contains(add.id));
    assert(step_state(s1, a1, add) == s1) by {
        lemma_dup_delivery_noop(s1, a1, add);
    }

    let s2 = step_state(s1, a1, del);
    let a2 = step_applied(a1, del);
    // delete removes the item...
    assert(!a1.contains(del.id)) by {
        assert(a1 == applied.insert(add.id));
        assert(!applied.contains(del.id));
        assert(add.id != del.id);
    }
    assert(s2 == apply(s1, del));
    assert(s2.dom() == s1.dom().remove(del.item));
    assert(!s2.dom().contains(add.item));

    // ...and the delayed duplicate of the add is gated away:
    assert(a2.contains(add.id));
    let s3 = step_state(s2, a2, add);
    assert(s3 == s2);
    assert(!s3.dom().contains(add.item));
}

/// regardless of interleaving, re-delivering a whole op is a state no-op
/// once its id has been applied -- the general form of the gate argument.
proof fn lemma_gate_replay_safe(s: Map<nat, bool>, applied: Set<nat>, op: TaggedOp)
    ensures
        // processing op twice in a row yields the same state as once
        step_state(step_state(s, applied, op), step_applied(applied, op), op)
            == step_state(s, applied, op),
{
    let s1 = step_state(s, applied, op);
    let a1 = step_applied(applied, op);
    lemma_dup_delivery_noop(s1, a1, op);
}

// ---------------------------------------------------------------------------
// Exec-level Hoare contract: the checked/uncheck effect on the item list,
// mirroring the Check/Uncheck arms of `apply_op` in lucina-core/models.rs.
// ---------------------------------------------------------------------------

pub open spec fn has_uuid(items: Seq<Item>, u: nat) -> bool {
    exists|i: int| 0 <= i < items.len() && items[i].uuid == u
}

pub open spec fn no_dup(items: Seq<Item>) -> bool {
    forall|i: int, j: int|
        0 <= i < j < items.len() ==> items[i].uuid != items[j].uuid
}

/// Set the checked flag of `u` to `checked`. Hoare contract:
/// - precondition: u is present, uuids are unique
/// - postcondition: u is present with exactly the requested flag, every
///   other entry is untouched (same uuid and same flag).
fn set_checked(items: &mut Vec<Item>, u: nat, checked: bool)
    requires
        no_dup(old(items)@),
        has_uuid(old(items)@, u),
    ensures
        no_dup(final(items)@),
        has_uuid(final(items)@, u),
        exists|i: int| 0 <= i < final(items).len() && final(items)[i].uuid == u
            && final(items)[i].checked == checked,
        forall|j: int|
            0 <= j < final(items).len() && final(items)[j].uuid != u ==>
                exists|k: int|
                    0 <= k < old(items).len()
                        && old(items)[k].uuid == final(items)[j].uuid
                        && old(items)[k].checked == final(items)[j].checked,
{
    let ghost old_seq = items@;
    let mut idx: usize = 0;
    while idx < items.len() && items[idx].uuid != u
        invariant
            idx <= items.len(),
            items@ == old_seq,
            forall|k: int| 0 <= k < idx ==> items[k].uuid != u,
        decreases items.len() - idx
    {
        idx += 1;
    }
    if idx < items.len() {
        let prev = items[idx].checked;
        items.set(idx, Item { uuid: u, checked });
        assert(items[idx as int].checked == checked);
        assert(items@ == old_seq.update(idx as int, Item { uuid: u, checked }));
        if prev != checked {
            // flag genuinely changed; membership and uniqueness carried over
            assert(items@.subrange(0, idx as int) == old_seq.subrange(0, idx as int));
            assert(items@.subrange(idx as int + 1, items.len() as int)
                == old_seq.subrange(idx as int + 1, old_seq.len() as int));
        }
    }
}

fn main() {
}
}
