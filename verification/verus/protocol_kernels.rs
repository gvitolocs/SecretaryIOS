// Verus-verified kernels of the protocol fixes found by the Stateright and
// TLA+ models (docs/verification.md, "Bugs found"). Each section is the
// pure decision function of one fix, with the invariant the model checks
// proved for all inputs instead of the model's small bounds:
//
//   K1  timeline ids: the Rust writer's id is past the newest indexed id and
//       past its own last id (lucina-store timeline.rs `append`).
//   K2  SSE resume by arrival order (fix A) returns every line indexed after
//       the cursor; resume by id can miss one (the T1 counterexample).
//   K3  shopping apply (server, with fix S2): one active row per object and
//       "deleted stays deleted" are preserved by every op.
//   K4  OAuth code redemption (fix O1): mark-redeemed and issue in one step,
//       so any second presentation revokes the grant.
//   K5  Meshy credits (fix G4): worst-case reservation + per-step guard keep
//       what Meshy charges within the reservation and the day's budget.
//   K6  proactive attention budget with one tick at a time (fix G3).

use vstd::prelude::*;

verus! {

// ---------------------------------------------------------------------------
// K1: timeline ids (ULID = (ms, random) compared lexicographically)
// ---------------------------------------------------------------------------

pub open spec fn id_lt(a: (u64, u64), b: (u64, u64)) -> bool {
    a.0 < b.0 || (a.0 == b.0 && a.1 < b.1)
}

/// `ulid::new_id` (monotonic per process) followed by `append`'s
/// "move past the newest id in the file".
pub fn rust_append_id(now: u64, fresh: u64, last: Option<(u64, u64)>, newest: Option<(u64, u64)>) -> (id: (u64, u64))
    requires
        last.is_some() ==> last.unwrap().1 < u64::MAX,
        newest.is_some() ==> newest.unwrap().1 < u64::MAX,
    ensures
        last.is_some() ==> id_lt(last.unwrap(), id),
        newest.is_some() ==> id_lt(newest.unwrap(), id),
{
    let gen = match last {
        Some(l) => if now <= l.0 { (l.0, l.1 + 1) } else { (now, fresh) },
        None => (now, fresh),
    };
    match newest {
        Some(n) => {
            if gen.0 < n.0 || (gen.0 == n.0 && gen.1 <= n.1) {
                let id = (n.0, n.1 + 1);
                // still past `last`: gen > last and id > newest >= gen
                id
            } else {
                gen
            }
        },
        None => gen,
    }
}

// ---------------------------------------------------------------------------
// K2: SSE resume
// ---------------------------------------------------------------------------

/// The ids indexed after position `p` (arrival = file order).
pub open spec fn after_arrival(entries: Seq<nat>, p: int) -> Seq<nat> {
    entries.subrange(p + 1, entries.len() as int)
}

/// Fix A: every entry that arrived after the cursor's position is replayed.
proof fn lemma_arrival_resume_complete(entries: Seq<nat>, p: int, j: int)
    requires
        0 <= p < entries.len(),
        p < j < entries.len(),
    ensures
        after_arrival(entries, p).contains(entries[j]),
{
    let a = after_arrival(entries, p);
    assert(a[j - p - 1] == entries[j]);
}

/// Resume by id (`after(L)` = ids > L) misses a line that arrived later
/// with a smaller id: the T1 counterexample (Node line written after a
/// newer Rust id, app reconnects with the Rust id).
proof fn lemma_id_resume_can_skip()
    ensures
        ({
            let entries = seq![5nat, 3nat];  // arrival order: Rust 5, then Node 3
            let last_event = 5nat;            // the app saw 5, then reconnected
            &&& after_arrival(entries, 0).contains(3nat)
            &&& !(3nat > last_event)          // so after_by_id(5) drops 3
        }),
{
    let entries = seq![5nat, 3nat];
    assert(after_arrival(entries, 0)[0] == 3nat);
}

// ---------------------------------------------------------------------------
// K3: shopping apply (server)
// ---------------------------------------------------------------------------

pub enum St {
    Needed,
    Recent,
    Removed,
}

pub struct Row {
    pub obj: nat,
    pub st: St,
}

pub open spec fn active(r: Row) -> bool {
    !(r.st is Removed)
}

/// One active row per object.
pub open spec fn one_active(rows: Map<nat, Row>) -> bool {
    forall|a: nat, b: nat| #![auto]
        rows.dom().contains(a) && rows.dom().contains(b) && active(rows[a]) && active(rows[b])
            && rows[a].obj == rows[b].obj ==> a == b
}

/// `add`: merge into the active row of the object, or create `new_id`
/// (fix S2: the client's fresh rowId; else a server uuid).
pub open spec fn apply_add(rows: Map<nat, Row>, obj: nat, new_id: nat) -> Map<nat, Row> {
    if exists|r: nat| #![auto] rows.dom().contains(r) && active(rows[r]) && rows[r].obj == obj {
        let r = choose|r: nat| #![auto] rows.dom().contains(r) && active(rows[r]) && rows[r].obj == obj;
        rows.insert(r, Row { obj, st: St::Needed })
    } else {
        rows.insert(new_id, Row { obj, st: St::Needed })
    }
}

/// `need` / `purchase` / `remove` on a row id; a removed row stays removed;
/// an unknown row is rejected (unchanged).
pub open spec fn apply_status(rows: Map<nat, Row>, id: nat, st: St) -> Map<nat, Row> {
    if rows.dom().contains(id) && active(rows[id]) {
        rows.insert(id, Row { obj: rows[id].obj, st })
    } else {
        rows
    }
}

proof fn lemma_add_keeps_one_active(rows: Map<nat, Row>, obj: nat, new_id: nat)
    requires
        one_active(rows),
        !rows.dom().contains(new_id),
    ensures
        one_active(apply_add(rows, obj, new_id)),
{
    let out = apply_add(rows, obj, new_id);
    if exists|r: nat| #![auto] rows.dom().contains(r) && active(rows[r]) && rows[r].obj == obj {
        let r = choose|r: nat| #![auto] rows.dom().contains(r) && active(rows[r]) && rows[r].obj == obj;
        assert forall|a: nat, b: nat| #![auto]
            out.dom().contains(a) && out.dom().contains(b) && active(out[a]) && active(out[b])
                && out[a].obj == out[b].obj implies a == b by {
            if a == r && b != r {
                assert(rows[b].obj == obj);
            } else if b == r && a != r {
                assert(rows[a].obj == obj);
            }
        }
    } else {
        assert forall|a: nat, b: nat| #![auto]
            out.dom().contains(a) && out.dom().contains(b) && active(out[a]) && active(out[b])
                && out[a].obj == out[b].obj implies a == b by {
            if a == new_id && b != new_id {
                assert(rows.dom().contains(b) && active(rows[b]) && rows[b].obj == obj);
            } else if b == new_id && a != new_id {
                assert(rows.dom().contains(a) && active(rows[a]) && rows[a].obj == obj);
            }
        }
    }
}

proof fn lemma_status_keeps_one_active(rows: Map<nat, Row>, id: nat, st: St)
    requires
        one_active(rows),
    ensures
        one_active(apply_status(rows, id, st)),
{
    let out = apply_status(rows, id, st);
    assert forall|a: nat, b: nat| #![auto]
        out.dom().contains(a) && out.dom().contains(b) && active(out[a]) && active(out[b])
            && out[a].obj == out[b].obj implies a == b by {
        if rows.dom().contains(id) && active(rows[id]) {
            if a == id || b == id {
                assert(active(rows[a]) && active(rows[b]));
            }
        }
    }
}

/// Deleted stays deleted: no op makes a removed row active again (a new add
/// creates a different row; with fix S2 the new id is a fresh rowId).
proof fn lemma_removed_stays_removed(rows: Map<nat, Row>, r: nat, obj: nat, new_id: nat, id: nat, st: St)
    requires
        rows.dom().contains(r),
        rows[r].st is Removed,
        !rows.dom().contains(new_id),
    ensures
        apply_add(rows, obj, new_id)[r].st is Removed,
        apply_status(rows, id, st)[r].st is Removed,
{
    if exists|x: nat| #![auto] rows.dom().contains(x) && active(rows[x]) && rows[x].obj == obj {
        let x = choose|x: nat| #![auto] rows.dom().contains(x) && active(rows[x]) && rows[x].obj == obj;
        assert(x != r);
    }
}

// ---------------------------------------------------------------------------
// K4: OAuth code redemption (fix O1)
// ---------------------------------------------------------------------------

/// The code record and the grants file, as the token endpoint sees them.
pub struct CodeState {
    pub redeemed: Option<nat>,
    pub grants: Map<nat, bool>,  // grant id -> revoked
}

/// Fixed redemption: under the codes lock, either mark redeemed AND write
/// the grant (fresh id `g`), or (already redeemed) revoke that grant.
pub open spec fn redeem(s: CodeState, g: nat) -> CodeState {
    match s.redeemed {
        None => CodeState { redeemed: Some(g), grants: s.grants.insert(g, false) },
        Some(old) => CodeState { redeemed: s.redeemed, grants: s.grants.insert(old, true) },
    }
}

/// The lock-held invariant the fix establishes: a redeemed code's grant is
/// always already in the file, so a replay's revoke is never a no-op.
pub open spec fn inv(s: CodeState) -> bool {
    s.redeemed.is_some() ==> s.grants.dom().contains(s.redeemed.unwrap())
}

proof fn lemma_redeem_keeps_inv(s: CodeState, g: nat)
    requires
        inv(s),
        !s.grants.dom().contains(g),
    ensures
        inv(redeem(s, g)),
{
}

/// Two presentations of one code, in either order or racing: the grant of
/// the first is revoked after the second.
proof fn lemma_second_presentation_revokes(s: CodeState, g1: nat, g2: nat)
    requires
        inv(s),
        s.redeemed.is_none(),
        !s.grants.dom().contains(g1),
    ensures
        ({
            let s2 = redeem(redeem(s, g1), g2);
            s2.redeemed == Some(g1) && s2.grants[g1] == true
        }),
{
}

// ---------------------------------------------------------------------------
// K5: Meshy credits (fix G4)
// ---------------------------------------------------------------------------

/// `Ledger::reserve`: check + reserve in one step.
pub fn reserve(spent_today: u64, total: u64, daily_budget: u64) -> (r: Option<u64>)
    requires
        spent_today <= daily_budget,
    ensures
        r.is_some() ==> r.unwrap() == spent_today + total && r.unwrap() <= daily_budget,
        r.is_none() ==> spent_today + total > daily_budget,
{
    if total <= daily_budget - spent_today {
        Some(spent_today + total)
    } else {
        None
    }
}

/// The per-step guard: run the next step only if its worst case still fits
/// the reservation. Meshy charges at most the worst case per step.
pub fn step(consumed: u64, worst: u64, reserved: u64, charged: u64) -> (r: Option<u64>)
    requires
        consumed <= reserved,
        charged <= worst,
    ensures
        r.is_some() ==> r.unwrap() == consumed + charged && r.unwrap() <= reserved,
        r.is_none() ==> worst > reserved - consumed,
{
    if worst <= reserved - consumed {
        Some(consumed + charged)
    } else {
        None
    }
}

/// Settling never raises the day's total: consumed <= reserved.
pub fn settle(spent_today: u64, reserved: u64, consumed: u64) -> (r: u64)
    requires
        reserved <= spent_today,
        consumed <= reserved,
    ensures
        r <= spent_today,
{
    spent_today - reserved + consumed
}

// ---------------------------------------------------------------------------
// K6: attention budget, one tick at a time (fix G3)
// ---------------------------------------------------------------------------

/// `attention::gate` on a ledger read in the same critical section as the send.
pub fn gate_and_send(sent_today: u64, max_per_day: u64, quiet: bool, cooling: bool) -> (r: u64)
    requires
        sent_today <= max_per_day,
    ensures
        r <= max_per_day,
        quiet ==> r == sent_today,
        cooling ==> r == sent_today,
{
    if quiet || cooling || sent_today >= max_per_day {
        sent_today
    } else {
        sent_today + 1
    }
}

fn main() {
}

} // verus!
