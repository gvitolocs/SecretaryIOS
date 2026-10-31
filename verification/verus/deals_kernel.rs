// Lucina grocery deal watch kernel (crates/lucina-deals on branch lucina-rs, pinned in
// verification/verus/diff/deals-revision).
//
//   validity_exec      <-> validity.rs::validity (after js::parse_date; a missing or
//                          unparsable date is Expired, `validity_opt_exec`)
//   wire_exec          <-> sources.rs: flyer `tjek:<id>`, food waste `fw:<storeId>:<ean>`
//                          (the displayed ids as UTF-8 bytes)
//   deal_keys_exec     <-> watch.rs::deal_keys (key, then relatedKeys, without repeats)
//   group_exec         <-> engine.rs::group_deals (after its sort): per group id
//                          (`fw:<ean|name>` for food waste, the key otherwise) the first
//                          deal is the representative, later members add their key
//   is_new / new_exec  <-> watch.rs::compose alert filter: new iff no key of the best offer
//                          is notified at a price <= the offer's price
//   ack / ack_run      <-> watch.rs::acknowledge + WatchState::set_notified (overwrite)
//   score / term_score_exec, deal_score / match_deal_exec
//                      <-> matcher.rs::term_score and match_deal over normalized tokens
//                          (matcher::tokens; per-character \p{L} flags)
//
// Refinement boundaries (not verified here, covered by verification/verus/diff/deals.rs on
// the real code): date parsing, Unicode lowercasing/NFD folding and the token regex, the
// \p{L} class (passed in as per-character flags), the JS `display` of raw ids, SHA-256.
// Prices are integers here (the code compares f64; a NaN notified price is treated as
// "not notified" by the code, which only makes more offers new). Notified-map keys are
// abstract ids; `wire_*` shows when the byte keys are injective.
//
// Proven, for all inputs:
//   (a) lemma_validity_partition, lemma_never_active_after_to; validity_exec refines the spec.
//   (b) lemma_wire_injective (store ids without ':'), lemma_wire_collision (with ':' it is
//       not injective), group_exec gives one group per group id and keeps every member key in its
//       group (with the member -> group index as a witness),
//       deal_keys_exec keeps the key and every related key, lemma_group_ack_covers_members.
//   (c) lemma_ack_run_value, lemma_ack_idempotent (a run's acknowledged deals, at the same or
//       a higher price, are never new), lemma_strict_cheaper_is_new, lemma_alert_guard
//       (select_exec picks exactly the new offers), lemma_overwrite_forgets_cheaper (a later
//       acknowledgment at a higher price makes the earlier, cheaper price new again: the
//       price-bounce repeat of the model, G0).
//   (d) lemma_no_suffix_no_match, lemma_prefix_only_examples (skinkepizza, skinkeschnitzler,
//       rugbrødschips, mælkechokolade), lemma_single_term_needs_suffix (a one-word term
//       scores only on an equal token or a head-final compound with a >= 2-letter prefix),
//       lemma_prefix_only_never_matches, lemma_exclude_veto (any exclude hit makes
//       match_deal 0), lemma_pizza_is_vetoed (the veto is reachable); the exec functions refine
//       score and deal_score exactly (the same integers as the code).
use vstd::prelude::*;

verus! {

// ------------------------------------------------------------------ (a) validity
pub open spec fn upcoming_ms() -> int {
    259_200_000
}

pub open spec fn active(from: int, to: int, now: int) -> bool {
    from <= now && now < to
}

pub open spec fn upcoming(from: int, to: int, now: int) -> bool {
    now < from && from <= now + upcoming_ms() && from < to
}

pub open spec fn expired(from: int, to: int, now: int) -> bool {
    !active(from, to, now) && !upcoming(from, to, now)
}

/// 0 = active, 1 = upcoming, 2 = expired.
pub open spec fn status(from: int, to: int, now: int) -> u8 {
    if active(from, to, now) {
        0
    } else if upcoming(from, to, now) {
        1
    } else {
        2
    }
}

proof fn lemma_validity_partition(from: int, to: int, now: int)
    ensures
        active(from, to, now) || upcoming(from, to, now) || expired(from, to, now),
        !(active(from, to, now) && upcoming(from, to, now)),
        !(active(from, to, now) && expired(from, to, now)),
        !(upcoming(from, to, now) && expired(from, to, now)),
{
}

proof fn lemma_never_active_after_to(from: int, to: int, now: int)
    requires
        now >= to,
    ensures
        !active(from, to, now),
        status(from, to, now) != 0,
{
}

/// The code's branches in its order (`end <= now || end <= start`, `start <= now`,
/// `start <= now + 3 days`); the sum is widened (the code's i64 sum would overflow only
/// for clocks within 3 days of i64::MAX).
pub fn validity_exec(from: i64, to: i64, now: i64) -> (r: u8)
    ensures
        r == status(from as int, to as int, now as int),
{
    if to <= now || to <= from {
        2
    } else if from <= now {
        0
    } else if (from as i128) <= (now as i128) + 259_200_000 {
        1
    } else {
        2
    }
}

pub fn validity_opt_exec(from: Option<i64>, to: Option<i64>, now: i64) -> (r: u8)
    ensures
        from.is_none() || to.is_none() ==> r == 2,
        from.is_some() && to.is_some() ==> r == status(
            from.unwrap() as int,
            to.unwrap() as int,
            now as int,
        ),
{
    match (from, to) {
        (Some(f), Some(t)) => validity_exec(f, t, now),
        _ => 2,
    }
}

// ------------------------------------------------------------------ (b) keys
pub open spec fn colon() -> u8 {
    58u8
}

pub open spec fn join(a: Seq<u8>, b: Seq<u8>) -> Seq<u8> {
    a + seq![colon()] + b
}

/// `fw:<store>:<ean>` when `food_waste`, else `tjek:<id>` (the ean is unused).
pub open spec fn wire(food_waste: bool, id: Seq<u8>, ean: Seq<u8>) -> Seq<u8> {
    if food_waste {
        seq![102u8, 119u8, 58u8] + join(id, ean)
    } else {
        seq![116u8, 106u8, 101u8, 107u8, 58u8] + id
    }
}

pub fn append_exec(out: &mut Vec<u8>, tail: &Vec<u8>)
    ensures
        final(out)@ == old(out)@ + tail@,
{
    let ghost before = out@;
    let mut i = 0;
    while i < tail.len()
        invariant
            i <= tail.len(),
            out@ == before + tail@.subrange(0, i as int),
        decreases tail.len() - i,
    {
        out.push(tail[i]);
        i += 1;
        assert(out@ == before + tail@.subrange(0, i as int));
    }
    assert(tail@.subrange(0, tail.len() as int) == tail@);
}

pub fn wire_exec(food_waste: bool, id: &Vec<u8>, ean: &Vec<u8>) -> (r: Vec<u8>)
    ensures
        r@ == wire(food_waste, id@, ean@),
{
    let mut out = Vec::new();
    if food_waste {
        out.push(102);
        out.push(119);
        out.push(58);
        append_exec(&mut out, id);
        out.push(58);
        append_exec(&mut out, ean);
        assert(out@ =~= wire(food_waste, id@, ean@));
    } else {
        out.push(116);
        out.push(106);
        out.push(101);
        out.push(107);
        out.push(58);
        append_exec(&mut out, id);
        assert(out@ =~= wire(food_waste, id@, ean@));
    }
    out
}

proof fn lemma_join_injective(a: Seq<u8>, b: Seq<u8>, c: Seq<u8>, d: Seq<u8>)
    requires
        !a.contains(colon()),
        !c.contains(colon()),
        join(a, b) == join(c, d),
    ensures
        a == c,
        b == d,
{
    assert(join(a, b).len() == a.len() + 1 + b.len());
    assert(join(c, d).len() == c.len() + 1 + d.len());
    if a.len() < c.len() {
        assert(join(a, b)[a.len() as int] == colon());
        assert(join(c, d)[a.len() as int] == c[a.len() as int]);
        assert(c.contains(colon()));
    }
    if c.len() < a.len() {
        assert(join(c, d)[c.len() as int] == colon());
        assert(join(a, b)[c.len() as int] == a[c.len() as int]);
        assert(a.contains(colon()));
    }
    assert(a.len() == c.len());
    assert forall|i: int| 0 <= i < a.len() implies a[i] == c[i] by {
        assert(join(a, b)[i] == a[i]);
        assert(join(c, d)[i] == c[i]);
    }
    assert(a =~= c);
    assert(b.len() == d.len());
    assert forall|i: int| 0 <= i < b.len() implies b[i] == d[i] by {
        assert(join(a, b)[i + a.len() + 1] == b[i]);
        assert(join(c, d)[i + c.len() + 1] == d[i]);
    }
    assert(b =~= d);
}

/// Keys are injective over (source, ids) when food-waste store ids have no ':'
/// (Salling store ids are UUIDs); flyer keys never equal food-waste keys.
proof fn lemma_wire_injective(
    s: bool,
    a: Seq<u8>,
    b: Seq<u8>,
    t: bool,
    c: Seq<u8>,
    d: Seq<u8>,
)
    requires
        s ==> !a.contains(colon()),
        t ==> !c.contains(colon()),
        wire(s, a, b) == wire(t, c, d),
    ensures
        s == t,
        a == c,
        s ==> b == d,
{
    assert(wire(s, a, b)[0] == if s {
        102u8
    } else {
        116u8
    });
    assert(wire(t, c, d)[0] == if t {
        102u8
    } else {
        116u8
    });
    if s {
        assert(t);
        assert(wire(s, a, b).len() == 3 + join(a, b).len());
        assert(wire(t, c, d).len() == 3 + join(c, d).len());
        assert(join(a, b).len() == join(c, d).len());
        assert forall|i: int| 0 <= i < join(a, b).len() implies join(a, b)[i] == join(c, d)[i] by {
            assert(wire(s, a, b)[i + 3] == join(a, b)[i]);
            assert(wire(t, c, d)[i + 3] == join(c, d)[i]);
        }
        assert(join(a, b) =~= join(c, d));
        lemma_join_injective(a, b, c, d);
    } else {
        assert(!t);
        assert(wire(s, a, b).len() == 5 + a.len());
        assert(wire(t, c, d).len() == 5 + c.len());
        assert(a.len() == c.len());
        assert forall|i: int| 0 <= i < a.len() implies a[i] == c[i] by {
            assert(wire(s, a, b)[i + 5] == a[i]);
            assert(wire(t, c, d)[i + 5] == c[i]);
        }
        assert(a =~= c);
    }
}

/// Without the restriction the claim is false: store "a:b" + ean "c" and store "a" +
/// ean "b:c" are both `fw:a:b:c` (sources.rs::food_waste joins the raw strings).
proof fn lemma_wire_collision()
    ensures
        wire(true, seq![97u8, 58u8, 98u8], seq![99u8]) == wire(true, seq![97u8], seq![98u8, 58u8, 99u8]),
        seq![97u8, 58u8, 98u8] != seq![97u8],
{
    assert(wire(true, seq![97u8, 58u8, 98u8], seq![99u8]) =~= wire(
        true,
        seq![97u8],
        seq![98u8, 58u8, 99u8],
    ));
    assert(seq![97u8, 58u8, 98u8].len() != seq![97u8].len());
}

/// `deal_keys`: the key, then each related key not seen yet.
pub fn deal_keys_exec(key: u64, related: &Vec<u64>) -> (r: Vec<u64>)
    ensures
        r@.contains(key),
        r@[0] == key,
        forall|i: int| 0 <= i < related@.len() ==> r@.contains(related@[i]),
        forall|i: int| 0 <= i < r@.len() ==> r@[i] == key || related@.contains(r@[i]),
        forall|i: int, j: int| 0 <= i < j < r@.len() ==> r@[i] != r@[j],
{
    let mut out: Vec<u64> = Vec::new();
    out.push(key);
    let mut i = 0;
    while i < related.len()
        invariant
            i <= related.len(),
            out@.len() >= 1,
            out@[0] == key,
            forall|j: int| 0 <= j < i ==> out@.contains(related@[j]),
            forall|j: int| 0 <= j < out@.len() ==> out@[j] == key || related@.contains(out@[j]),
            forall|a: int, b: int| 0 <= a < b < out@.len() ==> out@[a] != out@[b],
        decreases related.len() - i,
    {
        let k = related[i];
        let ghost prev = out@;
        if !contains_exec(&out, k) {
            out.push(k);
            assert(out@[out@.len() - 1] == k);
            assert(related@[i as int] == k);
            assert forall|j: int| 0 <= j < i implies out@.contains(related@[j]) by {
                let x = choose|x: int| 0 <= x < prev.len() && prev[x] == related@[j];
                assert(out@[x] == prev[x]);
            }
            assert forall|j: int| 0 <= j < out@.len() implies out@[j] == key || related@.contains(
                out@[j],
            ) by {
                if j < prev.len() {
                    assert(out@[j] == prev[j]);
                } else {
                    assert(related@[i as int] == out@[j]);
                }
            }
        }
        assert(out@.contains(related@[i as int]));
        i += 1;
    }
    assert(out@[0] == key);
    out
}

pub fn contains_exec(v: &Vec<u64>, k: u64) -> (r: bool)
    ensures
        r == v@.contains(k),
{
    let mut i = 0;
    while i < v.len()
        invariant
            i <= v.len(),
            forall|j: int| 0 <= j < i ==> v@[j] != k,
        decreases v.len() - i,
    {
        if v[i] == k {
            assert(v@[i as int] == k);
            return true;
        }
        i += 1;
    }
    false
}

pub fn copy_exec(v: &Vec<u64>) -> (r: Vec<u64>)
    ensures
        r@ == v@,
{
    let mut out = Vec::new();
    append_u64(&mut out, v);
    assert(out@ =~= v@);
    out
}

fn append_u64(out: &mut Vec<u64>, tail: &Vec<u64>)
    ensures
        final(out)@ == old(out)@ + tail@,
{
    let ghost before = out@;
    let mut i = 0;
    while i < tail.len()
        invariant
            i <= tail.len(),
            out@ == before + tail@.subrange(0, i as int),
        decreases tail.len() - i,
    {
        out.push(tail[i]);
        i += 1;
        assert(out@ == before + tail@.subrange(0, i as int));
    }
    assert(tail@.subrange(0, tail.len() as int) == tail@);
}
proof fn lemma_push_contains(s: Seq<u64>, k: u64, v: u64)
    requires
        s.contains(v),
    ensures
        s.push(k).contains(v),
{
    let i = choose|i: int| 0 <= i < s.len() && s[i] == v;
    assert(s.push(k)[i] == v);
}

fn find_group(out: &Vec<(u64, Vec<u64>)>, gid: u64) -> (r: Option<usize>)
    ensures
        match r {
            Some(j) => j < out.len() && out@[j as int].0 == gid,
            None => forall|y: int| 0 <= y < out.len() ==> out@[y].0 != gid,
        },
{
    let mut j = 0;
    while j < out.len()
        invariant
            j <= out.len(),
            forall|y: int| 0 <= y < j ==> out@[y].0 != gid,
        decreases out.len() - j,
    {
        if out[j].0 == gid {
            return Some(j);
        }
        j += 1;
    }
    None
}

pub open spec fn grouped(deals: Seq<(u64, u64)>, out: Seq<(u64, Vec<u64>)>, idx: Seq<usize>, n: int) -> bool {
    &&& idx.len() == n
    &&& forall|a: int, b: int| 0 <= a < b < out.len() ==> #[trigger] out[a].0 != #[trigger] out[b].0
    &&& forall|j: int| 0 <= j < out.len() ==> (#[trigger] out[j]).1@.len() >= 1
    &&& forall|x: int|
        0 <= x < n ==> (#[trigger] idx[x] as int) < out.len() && out[idx[x] as int].0 == deals[x].0
            && out[idx[x] as int].1@.contains(deals[x].1)
}

/// `group_deals` over (group id, key) in the sorted order: one group per group id, in
/// first-seen order, the representative's key first; a member whose key differs from the
/// representative's is appended (`Some(old) if deal.key != old.key`). `r.1[i]` names the
/// group of deal i: every member's key is in its group, so acknowledging the group's
/// deal_keys covers each branch.
pub fn group_exec(deals: &Vec<(u64, u64)>) -> (r: (Vec<(u64, Vec<u64>)>, Vec<usize>))
    ensures
        grouped(deals@, r.0@, r.1@, deals@.len() as int),
{
    let mut out: Vec<(u64, Vec<u64>)> = Vec::new();
    let mut idx: Vec<usize> = Vec::new();
    let mut i = 0;
    while i < deals.len()
        invariant
            i <= deals.len(),
            grouped(deals@, out@, idx@, i as int),
        decreases deals.len() - i,
    {
        let (gid, key) = deals[i];
        let ghost prev = out@;
        let ghost previdx = idx@;
        match find_group(&out, gid) {
            None => {
                let mut keys = Vec::new();
                keys.push(key);
                out.push((gid, keys));
                idx.push(out.len() - 1);
                assert(out@[out@.len() - 1].1@[0] == key);
                assert forall|x: int| 0 <= x < i + 1 implies (#[trigger] idx@[x] as int) < out@.len()
                    && out@[idx@[x] as int].0 == deals@[x].0 && out@[idx@[x] as int].1@.contains(
                    deals@[x].1,
                ) by {
                    if x < i {
                        assert(idx@[x] == previdx[x]);
                        assert(out@[previdx[x] as int] == prev[previdx[x] as int]);
                    }
                }
                assert forall|j: int| 0 <= j < out@.len() implies (#[trigger] out@[j]).1@.len() >= 1 by {
                    if j < prev.len() {
                        assert(out@[j] == prev[j]);
                    }
                }
            },
            Some(j) => {
                if key != out[j].1[0] {
                    let mut keys = copy_exec(&out[j].1);
                    keys.push(key);
                    out.set(j, (gid, keys));
                    assert(out@[j as int].1@ == prev[j as int].1@.push(key));
                    assert forall|x: int| 0 <= x < i implies (#[trigger] idx@[x] as int) < out@.len()
                        && out@[idx@[x] as int].0 == deals@[x].0 && out@[idx@[x] as int].1@.contains(
                        deals@[x].1,
                    ) by {
                        if previdx[x] == j {
                            lemma_push_contains(prev[j as int].1@, key, deals@[x].1);
                        } else {
                            assert(out@[previdx[x] as int] == prev[previdx[x] as int]);
                        }
                    }
                    assert(out@[j as int].1@[prev[j as int].1@.len() as int] == key);
                    assert forall|j2: int| 0 <= j2 < out@.len() implies (#[trigger] out@[j2]).1@.len() >= 1 by {
                        if j2 != j {
                            assert(out@[j2] == prev[j2]);
                        }
                    }
                } else {
                    assert(out@[j as int].1@[0] == key);
                }
                idx.push(j);
                assert forall|x: int| 0 <= x < i + 1 implies (#[trigger] idx@[x] as int) < out@.len()
                    && out@[idx@[x] as int].0 == deals@[x].0 && out@[idx@[x] as int].1@.contains(
                    deals@[x].1,
                ) by {
                    if x < i {
                        assert(idx@[x] == previdx[x]);
                    }
                }
            },
        }
        i += 1;
    }
    (out, idx)
}

// ------------------------------------------------------------------ (c) new / acknowledge
/// No key of the offer is notified at a price <= `price`.
pub open spec fn is_new(keys: Set<int>, m: Map<int, int>, price: int) -> bool {
    forall|k: int| keys.contains(k) && m.dom().contains(k) ==> m[k] > price
}

/// `set_notified(id, price)` for every key (an overwrite, also with a higher price).
pub open spec fn ack(keys: Set<int>, m: Map<int, int>, price: int) -> Map<int, int> {
    Map::new(
        keys.union(m.dom()),
        |k: int|
            if keys.contains(k) {
                price
            } else {
                m[k]
            },
    )
}

/// `acknowledge` of one run: every (keys, price) of the run, in order.
pub open spec fn ack_run(acks: Seq<(Set<int>, int)>, m: Map<int, int>) -> Map<int, int>
    decreases acks.len(),
{
    if acks.len() == 0 {
        m
    } else {
        ack(acks.last().0, ack_run(acks.drop_last(), m), acks.last().1)
    }
}

/// Within one run the acknowledged key sets are disjoint: deals of one item have
/// distinct keys (collect dedups by key, group_exec assigns each key one group), deals of
/// different items carry different rule ids, and `assigned` keeps a deal to one item.
pub open spec fn disjoint(acks: Seq<(Set<int>, int)>) -> bool {
    forall|a: int, b: int, k: int|
        0 <= a < acks.len() && 0 <= b < acks.len() && a != b && acks[a].0.contains(k)
            ==> !acks[b].0.contains(k)
}

proof fn lemma_ack_run_value(acks: Seq<(Set<int>, int)>, m: Map<int, int>, j: int, k: int)
    requires
        disjoint(acks),
        0 <= j < acks.len(),
        acks[j].0.contains(k),
    ensures
        ack_run(acks, m).dom().contains(k),
        ack_run(acks, m)[k] == acks[j].1,
    decreases acks.len(),
{
    let last = acks.len() - 1;
    if j == last {
    } else {
        let rest = acks.drop_last();
        assert(!acks[last].0.contains(k));
        assert forall|a: int, b: int, x: int|
            0 <= a < rest.len() && 0 <= b < rest.len() && a != b && rest[a].0.contains(
                x,
            ) implies !rest[b].0.contains(x) by {
            assert(rest[a] == acks[a]);
            assert(rest[b] == acks[b]);
        }
        assert(rest[j] == acks[j]);
        lemma_ack_run_value(rest, m, j, k);
    }
}

/// Idempotence: after a run's acknowledgment, any offer sharing a key with an acknowledged
/// deal is not new at that deal's price or above.
proof fn lemma_ack_idempotent(
    acks: Seq<(Set<int>, int)>,
    m: Map<int, int>,
    j: int,
    keys: Set<int>,
    k: int,
    q: int,
)
    requires
        disjoint(acks),
        0 <= j < acks.len(),
        acks[j].0.contains(k),
        keys.contains(k),
        q >= acks[j].1,
    ensures
        !is_new(keys, ack_run(acks, m), q),
{
    lemma_ack_run_value(acks, m, j, k);
}

/// Strictly cheaper than the acknowledged price (on every key of the offer) is new.
proof fn lemma_strict_cheaper_is_new(
    acks: Seq<(Set<int>, int)>,
    m: Map<int, int>,
    j: int,
    keys: Set<int>,
    q: int,
)
    requires
        disjoint(acks),
        0 <= j < acks.len(),
        keys.subset_of(acks[j].0),
        q < acks[j].1,
    ensures
        is_new(keys, ack_run(acks, m), q),
{
    assert forall|k: int| keys.contains(k) && ack_run(acks, m).dom().contains(k) implies ack_run(
        acks,
        m,
    )[k] > q by {
        lemma_ack_run_value(acks, m, j, k);
    }
}

/// Grouped food waste: acknowledging the representative (its deal_keys hold every member
/// key) makes each member alone, at the same or a higher price, not new.
proof fn lemma_group_ack_covers_members(
    group: Set<int>,
    m: Map<int, int>,
    p: int,
    member: int,
    later: Set<int>,
    q: int,
)
    requires
        group.contains(member),
        later.contains(member),
        q >= p,
    ensures
        !is_new(later, ack(group, m, p), q),
{
    assert(ack(group, m, p).dom().contains(member));
}

/// The code overwrites: notified at 1, then acknowledged again at 2 (the offer was active
/// at 2 in a later delivered run), and the same offer at 1 is new again.
proof fn lemma_overwrite_forgets_cheaper()
    ensures
        !is_new(set![0int], ack(set![0int], Map::<int, int>::empty(), 1), 1),
        is_new(set![0int], ack(set![0int], ack(set![0int], Map::<int, int>::empty(), 1), 2), 1),
{
    let m1 = ack(set![0int], Map::<int, int>::empty(), 1);
    assert(m1.dom().contains(0));
    let m2 = ack(set![0int], m1, 2);
    assert forall|k: int| set![0int].contains(k) && m2.dom().contains(k) implies m2[k] > 1 by {
        assert(k == 0);
    }
}

/// One alert candidate's keys and their notified prices (`notified_price`, aligned).
pub open spec fn new_seq(old: Seq<Option<i64>>, price: i64) -> bool {
    forall|i: int| 0 <= i < old.len() && old[i].is_some() ==> old[i].unwrap() > price
}

pub fn new_exec(old: &Vec<Option<i64>>, price: i64) -> (r: bool)
    ensures
        r == new_seq(old@, price),
{
    let mut i = 0;
    while i < old.len()
        invariant
            i <= old.len(),
            forall|j: int| 0 <= j < i && old@[j].is_some() ==> old@[j].unwrap() > price,
        decreases old.len() - i,
    {
        if let Some(p) = old[i] {
            if p <= price {
                return false;
            }
        }
        i += 1;
    }
    true
}

/// The alert filter of `compose`: one flag per item's best offer.
pub fn select_exec(olds: &Vec<Vec<Option<i64>>>, prices: &Vec<i64>) -> (r: Vec<bool>)
    requires
        olds.len() == prices.len(),
    ensures
        r.len() == prices.len(),
        forall|i: int| 0 <= i < r.len() ==> r@[i] == new_seq(olds@[i]@, prices@[i]),
{
    let mut out = Vec::new();
    let mut i = 0;
    while i < prices.len()
        invariant
            i <= prices.len(),
            olds.len() == prices.len(),
            out.len() == i,
            forall|j: int| 0 <= j < i ==> out@[j] == new_seq(olds@[j]@, prices@[j]),
        decreases prices.len() - i,
    {
        out.push(new_exec(&olds[i], prices[i]));
        i += 1;
    }
    out
}

/// The aligned lookup means the map: `old[i]` is the notified price of `keys[i]`.
pub open spec fn aligned(keys: Seq<int>, old: Seq<Option<i64>>, m: Map<int, int>) -> bool {
    keys.len() == old.len() && forall|i: int|
        0 <= i < keys.len() ==> (if m.dom().contains(keys[i]) {
            old[i] == Some(m[keys[i]] as i64) && m[keys[i]] == old[i].unwrap() as int
        } else {
            old[i].is_none()
        })
}

/// An alert never carries an offer with a key already notified at <= its price, and
/// every new offer is selected (the filter is exact).
proof fn lemma_alert_guard(keys: Seq<int>, old: Seq<Option<i64>>, m: Map<int, int>, price: i64)
    requires
        aligned(keys, old, m),
    ensures
        new_seq(old, price) == is_new(keys.to_set(), m, price as int),
{
    if new_seq(old, price) {
        assert forall|k: int|
            keys.to_set().contains(k) && m.dom().contains(k) implies m[k] > price as int by {
            let i = choose|i: int| 0 <= i < keys.len() && keys[i] == k;
            assert(old[i].is_some());
        }
    }
    if is_new(keys.to_set(), m, price as int) {
        assert forall|i: int| 0 <= i < old.len() && old[i].is_some() implies old[i].unwrap()
            > price by {
            assert(keys.to_set().contains(keys[i]));
        }
    }
}

// ------------------------------------------------------------------ (d) matcher
pub open spec fn ends(w: Seq<char>, t: Seq<char>) -> bool {
    t.len() <= w.len() && w.subrange(w.len() - t.len(), w.len() as int) == t
}

/// `w.strip_suffix(t)` is a prefix of >= 2 characters, all \p{L} (`flags` per char of w).
pub open spec fn head_final(w: Seq<char>, flags: Seq<bool>, t: Seq<char>) -> bool {
    ends(w, t) && w.len() - t.len() >= 2 && forall|i: int|
        0 <= i < w.len() - t.len() ==> flags[i]
}

pub open spec fn phrase_at(words: Seq<Seq<char>>, needle: Seq<Seq<char>>, i: int) -> bool {
    0 <= i && i + needle.len() <= words.len() && forall|j: int|
        0 <= j < needle.len() ==> words[i + j] == needle[j]
}

/// The first position >= i where the phrase occurs, or -1.
pub open spec fn first_phrase(words: Seq<Seq<char>>, needle: Seq<Seq<char>>, i: int) -> int
    decreases words.len() - i,
{
    if i < 0 || i + needle.len() > words.len() {
        -1
    } else if phrase_at(words, needle, i) {
        i
    } else {
        first_phrase(words, needle, i + 1)
    }
}

/// The first word >= i that is a head-final compound of t, or -1.
pub open spec fn first_head(
    words: Seq<Seq<char>>,
    flags: Seq<Seq<bool>>,
    t: Seq<char>,
    i: int,
) -> int
    decreases words.len() - i,
{
    if i < 0 || i >= words.len() {
        -1
    } else if head_final(words[i], flags[i], t) {
        i
    } else {
        first_head(words, flags, t, i + 1)
    }
}

pub open spec fn min400(i: int) -> int {
    if i < 400 {
        i
    } else {
        400
    }
}

/// `term_score(text, term)` over `tokens(text)` and `tokens(term)`.
pub open spec fn score(words: Seq<Seq<char>>, flags: Seq<Seq<bool>>, needle: Seq<Seq<char>>) -> int {
    if needle.len() == 0 {
        0
    } else if first_phrase(words, needle, 0) >= 0 {
        1000 - min400(first_phrase(words, needle, 0))
    } else if needle.len() == 1 && first_head(words, flags, needle[0], 0) >= 0 {
        500 - min400(first_head(words, flags, needle[0], 0))
    } else {
        0
    }
}

pub open spec fn flags_ok(words: Seq<Seq<char>>, flags: Seq<Seq<bool>>) -> bool {
    words.len() == flags.len() && forall|i: int|
        0 <= i < words.len() ==> #[trigger] flags[i].len() == words[i].len()
}

pub open spec fn view_words(v: Seq<Vec<char>>) -> Seq<Seq<char>> {
    v.map_values(|w: Vec<char>| w@)
}

pub open spec fn view_flags(v: Seq<Vec<bool>>) -> Seq<Seq<bool>> {
    v.map_values(|w: Vec<bool>| w@)
}

proof fn lemma_first_phrase_found(words: Seq<Seq<char>>, needle: Seq<Seq<char>>, i: int)
    ensures
        first_phrase(words, needle, i) >= 0 ==> phrase_at(
            words,
            needle,
            first_phrase(words, needle, i),
        ),
        first_phrase(words, needle, i) == -1 || first_phrase(words, needle, i) >= i,
    decreases words.len() - i,
{
    if i < 0 || i + needle.len() > words.len() {
    } else if phrase_at(words, needle, i) {
    } else {
        lemma_first_phrase_found(words, needle, i + 1);
    }
}

proof fn lemma_first_head_found(words: Seq<Seq<char>>, flags: Seq<Seq<bool>>, t: Seq<char>, i: int)
    ensures
        first_head(words, flags, t, i) >= 0 ==> {
            let h = first_head(words, flags, t, i);
            0 <= h < words.len() && head_final(words[h], flags[h], t)
        },
    decreases words.len() - i,
{
    if i < 0 || i >= words.len() {
    } else if head_final(words[i], flags[i], t) {
    } else {
        lemma_first_head_found(words, flags, t, i + 1);
    }
}

pub fn eq_exec(a: &Vec<char>, b: &Vec<char>) -> (r: bool)
    ensures
        r == (a@ == b@),
{
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len()
        invariant
            a.len() == b.len(),
            i <= a.len(),
            forall|j: int| 0 <= j < i ==> a@[j] == b@[j],
        decreases a.len() - i,
    {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    assert(a@ =~= b@);
    true
}

pub fn ends_exec(w: &Vec<char>, t: &Vec<char>) -> (r: bool)
    ensures
        r == ends(w@, t@),
{
    if t.len() > w.len() {
        return false;
    }
    let off = w.len() - t.len();
    let mut i = 0;
    while i < t.len()
        invariant
            off + t.len() == w.len(),
            i <= t.len(),
            forall|j: int| 0 <= j < i ==> w@[off + j] == t@[j],
        decreases t.len() - i,
    {
        if w[off + i] != t[i] {
            assert(w@.subrange(off as int, w.len() as int)[i as int] != t@[i as int]);
            return false;
        }
        i += 1;
    }
    assert(w@.subrange(off as int, w.len() as int) =~= t@);
    true
}

pub fn head_final_exec(w: &Vec<char>, flags: &Vec<bool>, t: &Vec<char>) -> (r: bool)
    requires
        flags.len() == w.len(),
    ensures
        r == head_final(w@, flags@, t@),
{
    if !ends_exec(w, t) {
        return false;
    }
    let n = w.len() - t.len();
    if n < 2 {
        return false;
    }
    let mut i = 0;
    while i < n
        invariant
            flags.len() == w.len(),
            n == w.len() - t.len(),
            n >= 2,
            ends(w@, t@),
            i <= n,
            forall|j: int| 0 <= j < i ==> flags@[j],
        decreases n - i,
    {
        if !flags[i] {
            assert(!flags@[i as int]);
            return false;
        }
        i += 1;
    }
    true
}

fn phrase_at_exec(words: &Vec<Vec<char>>, needle: &Vec<Vec<char>>, i: usize) -> (r: bool)
    requires
        i + needle.len() <= words.len(),
    ensures
        r == phrase_at(view_words(words@), view_words(needle@), i as int),
{
    let ghost ws = view_words(words@);
    let ghost ns = view_words(needle@);
    let mut j = 0;
    while j < needle.len()
        invariant
            ws == view_words(words@),
            ns == view_words(needle@),
            ws.len() == words.len(),
            ns.len() == needle.len(),
            i + needle.len() <= words.len(),
            j <= needle.len(),
            forall|x: int| 0 <= x < j ==> ws[i + x] == ns[x],
        decreases needle.len() - j,
    {
        let same = eq_exec(&words[i + j], &needle[j]);
        assert(ws[i + j] == words@[i + j]@);
        assert(ns[j as int] == needle@[j as int]@);
        if !same {
            return false;
        }
        j += 1;
    }
    true
}

fn min400_exec(i: usize) -> (r: i32)
    ensures
        r as int == min400(i as int),
{
    if i < 400 {
        i as i32
    } else {
        400
    }
}

/// `term_score` over tokens.
pub fn term_score_exec(words: &Vec<Vec<char>>, flags: &Vec<Vec<bool>>, needle: &Vec<Vec<char>>) -> (r: i32)
    requires
        flags_ok(view_words(words@), view_flags(flags@)),
    ensures
        r as int == score(view_words(words@), view_flags(flags@), view_words(needle@)),
{
    let ghost ws = view_words(words@);
    let ghost fs = view_flags(flags@);
    let ghost ns = view_words(needle@);
    assert(ws.len() == words.len() && fs.len() == flags.len() && ns.len() == needle.len());
    if needle.len() == 0 {
        return 0;
    }
    if words.len() >= needle.len() {
        let mut i = 0;
        while i <= words.len() - needle.len()
            invariant
                ws == view_words(words@),
                ns == view_words(needle@),
                ws.len() == words.len(),
                ns.len() == needle.len(),
                words.len() >= needle.len(),
                needle.len() > 0,
                i <= words.len() - needle.len() + 1,
                first_phrase(ws, ns, 0) == first_phrase(ws, ns, i as int),
            decreases words.len() - needle.len() + 1 - i,
        {
            if phrase_at_exec(words, needle, i) {
                assert(first_phrase(ws, ns, i as int) == i);
                return 1000 - min400_exec(i);
            }
            i += 1;
        }
        assert(first_phrase(ws, ns, i as int) == -1);
    } else {
        assert(first_phrase(ws, ns, 0) == -1);
    }
    if needle.len() == 1 {
        let mut i = 0;
        while i < words.len()
            invariant
                ws == view_words(words@),
                fs == view_flags(flags@),
                ns == view_words(needle@),
                ws.len() == words.len(),
                fs.len() == flags.len(),
                ns.len() == needle.len(),
                needle.len() == 1,
                i <= words.len(),
                first_phrase(ws, ns, 0) == -1,
                flags_ok(ws, fs),
                first_head(ws, fs, ns[0], 0) == first_head(ws, fs, ns[0], i as int),
            decreases words.len() - i,
        {
            assert(fs[i as int].len() == ws[i as int].len());
            assert(ws[i as int] == words@[i as int]@);
            assert(fs[i as int] == flags@[i as int]@);
            assert(ns[0] == needle@[0]@);
            if head_final_exec(&words[i], &flags[i], &needle[0]) {
                assert(first_head(ws, fs, ns[0], i as int) == i);
                return 500 - min400_exec(i);
            }
            i += 1;
        }
        assert(first_head(ws, fs, ns[0], i as int) == -1);
    }
    0
}

pub open spec fn any_exclude(
    words: Seq<Seq<char>>,
    flags: Seq<Seq<bool>>,
    excludes: Seq<Seq<Seq<char>>>,
) -> bool {
    exists|e: int| 0 <= e < excludes.len() && score(words, flags, #[trigger] excludes[e]) != 0
}

pub open spec fn best_term(
    words: Seq<Seq<char>>,
    flags: Seq<Seq<bool>>,
    terms: Seq<Seq<Seq<char>>>,
) -> int
    decreases terms.len(),
{
    if terms.len() == 0 {
        0
    } else {
        let rest = best_term(words, flags, terms.drop_last());
        let s = score(words, flags, terms.last());
        if s > rest {
            s
        } else {
            rest
        }
    }
}

pub open spec fn deal_score(
    words: Seq<Seq<char>>,
    flags: Seq<Seq<bool>>,
    excludes: Seq<Seq<Seq<char>>>,
    category_ok: bool,
    terms: Seq<Seq<Seq<char>>>,
) -> int {
    if any_exclude(words, flags, excludes) || !category_ok {
        0
    } else {
        best_term(words, flags, terms)
    }
}

pub open spec fn view_terms(v: Seq<Vec<Vec<char>>>) -> Seq<Seq<Seq<char>>> {
    v.map_values(|t: Vec<Vec<char>>| view_words(t@))
}

pub fn match_deal_exec(
    words: &Vec<Vec<char>>,
    flags: &Vec<Vec<bool>>,
    excludes: &Vec<Vec<Vec<char>>>,
    category_ok: bool,
    terms: &Vec<Vec<Vec<char>>>,
) -> (r: i32)
    requires
        flags_ok(view_words(words@), view_flags(flags@)),
    ensures
        r as int == deal_score(
            view_words(words@),
            view_flags(flags@),
            view_terms(excludes@),
            category_ok,
            view_terms(terms@),
        ),
{
    let ghost ws = view_words(words@);
    let ghost fs = view_flags(flags@);
    let ghost es = view_terms(excludes@);
    let ghost ts = view_terms(terms@);
    assert(es.len() == excludes.len() && ts.len() == terms.len());
    let mut e = 0;
    while e < excludes.len()
        invariant
            ws == view_words(words@),
            fs == view_flags(flags@),
            es == view_terms(excludes@),
            es.len() == excludes.len(),
            e <= excludes.len(),
            flags_ok(ws, fs),
            forall|x: int| 0 <= x < e ==> score(ws, fs, #[trigger] es[x]) == 0,
        decreases excludes.len() - e,
    {
        let s = term_score_exec(words, flags, &excludes[e]);
        assert(es[e as int] == view_words(excludes@[e as int]@));
        if s != 0 {
            assert(score(ws, fs, es[e as int]) != 0);
            return 0;
        }
        e += 1;
    }
    assert(!any_exclude(ws, fs, es));
    if !category_ok {
        return 0;
    }
    let mut best: i32 = 0;
    let mut t = 0;
    assert(ts.subrange(0, 0) =~= Seq::<Seq<Seq<char>>>::empty());
    while t < terms.len()
        invariant
            ws == view_words(words@),
            fs == view_flags(flags@),
            ts == view_terms(terms@),
            ts.len() == terms.len(),
            t <= terms.len(),
            flags_ok(ws, fs),
            best as int == best_term(ws, fs, ts.subrange(0, t as int)),
            0 <= best <= 1000,
        decreases terms.len() - t,
    {
        let s = term_score_exec(words, flags, &terms[t]);
        proof {
            assert(ts[t as int] == view_words(terms@[t as int]@));
            lemma_score_range(ws, fs, ts[t as int]);
            let sub = ts.subrange(0, t as int + 1);
            assert(sub.drop_last() =~= ts.subrange(0, t as int));
            assert(sub.last() == ts[t as int]);
        }
        if s > best {
            best = s;
        }
        t += 1;
    }
    assert(ts.subrange(0, terms.len() as int) =~= ts);
    best
}

proof fn lemma_score_range(words: Seq<Seq<char>>, flags: Seq<Seq<bool>>, needle: Seq<Seq<char>>)
    ensures
        0 <= score(words, flags, needle) <= 1000,
{
}

/// A token without the term as a suffix never matches the term, whatever its letters:
/// equality implies the suffix, and the head-final rule requires it.
proof fn lemma_no_suffix_no_match(w: Seq<char>, flags: Seq<bool>, t: Seq<char>)
    requires
        !ends(w, t),
    ensures
        w != t,
        !head_final(w, flags, t),
{
    if w == t {
        assert(w.subrange(0, w.len() as int) =~= w);
    }
}

/// A one-word term scores only on a token equal to it or a head-final compound of it.
proof fn lemma_single_term_needs_suffix(
    words: Seq<Seq<char>>,
    flags: Seq<Seq<bool>>,
    t: Seq<char>,
)
    requires
        score(words, flags, seq![t]) != 0,
    ensures
        exists|i: int|
            0 <= i < words.len() && (words[i] == t || head_final(words[i], flags[i], t)),
{
    let needle = seq![t];
    lemma_first_phrase_found(words, needle, 0);
    lemma_first_head_found(words, flags, t, 0);
    if first_phrase(words, needle, 0) >= 0 {
        let p = first_phrase(words, needle, 0);
        assert(words[p + 0] == needle[0]);
    }
}

/// So a heading whose tokens all start with the term but do not end with it never
/// matches: no token equals the term and none is a head-final compound.
proof fn lemma_prefix_only_never_matches(
    words: Seq<Seq<char>>,
    flags: Seq<Seq<bool>>,
    t: Seq<char>,
)
    requires
        forall|i: int| 0 <= i < words.len() ==> !ends(#[trigger] words[i], t),
    ensures
        score(words, flags, seq![t]) == 0,
{
    if score(words, flags, seq![t]) != 0 {
        lemma_single_term_needs_suffix(words, flags, t);
        let i = choose|i: int|
            0 <= i < words.len() && (words[i] == t || head_final(words[i], flags[i], t));
        lemma_no_suffix_no_match(words[i], flags[i], t);
    }
}

pub open spec fn skinke() -> Seq<char> {
    seq!['s', 'k', 'i', 'n', 'k', 'e']
}

pub open spec fn rugbrod() -> Seq<char> {
    seq!['r', 'u', 'g', 'b', 'r', 'ø', 'd']
}

pub open spec fn maelk() -> Seq<char> {
    seq!['m', 'æ', 'l', 'k']
}

/// The owner's examples: the term is a prefix, not a suffix, so they never match.
proof fn lemma_prefix_only_examples(flags: Seq<bool>)
    ensures
        !ends(skinke() + seq!['p', 'i', 'z', 'z', 'a'], skinke()),
        !ends(skinke() + seq!['s', 'c', 'h', 'n', 'i', 't', 'z', 'l', 'e', 'r'], skinke()),
        !ends(rugbrod() + seq!['c', 'h', 'i', 'p', 's'], rugbrod()),
        !ends(maelk() + seq!['e', 'c', 'h', 'o', 'k', 'o', 'l', 'a', 'd', 'e'], maelk()),
        score(seq![skinke() + seq!['p', 'i', 'z', 'z', 'a']], seq![flags], seq![skinke()]) == 0,
        score(seq![maelk() + seq!['e', 'c', 'h', 'o', 'k', 'o', 'l', 'a', 'd', 'e']], seq![flags], seq![maelk()]) == 0,
{
    let a = skinke() + seq!['p', 'i', 'z', 'z', 'a'];
    let b = skinke() + seq!['s', 'c', 'h', 'n', 'i', 't', 'z', 'l', 'e', 'r'];
    let c = rugbrod() + seq!['c', 'h', 'i', 'p', 's'];
    let d = maelk() + seq!['e', 'c', 'h', 'o', 'k', 'o', 'l', 'a', 'd', 'e'];
    assert(a.subrange(a.len() - 6, a.len() as int)[5] == 'a');
    assert(b.subrange(b.len() - 6, b.len() as int)[5] == 'r');
    assert(c.subrange(c.len() - 7, c.len() as int)[6] == 's');
    assert(d.subrange(d.len() - 4, d.len() as int)[3] == 'e');
    assert(skinke()[5] == 'e');
    assert(rugbrod()[6] == 'd');
    assert(maelk()[3] == 'k');
    lemma_prefix_only_never_matches(seq![a], seq![flags], skinke());
    lemma_prefix_only_never_matches(seq![d], seq![flags], maelk());
}

/// Any exclude hit vetoes the match, whatever the terms score.
proof fn lemma_exclude_veto(
    words: Seq<Seq<char>>,
    flags: Seq<Seq<bool>>,
    excludes: Seq<Seq<Seq<char>>>,
    category_ok: bool,
    terms: Seq<Seq<Seq<char>>>,
    e: int,
)
    requires
        0 <= e < excludes.len(),
        score(words, flags, excludes[e]) != 0,
    ensures
        deal_score(words, flags, excludes, category_ok, terms) == 0,
{
    assert(any_exclude(words, flags, excludes));
}

/// Non-vacuity of the veto: "skinkepizza" hits the global exclude "pizza" as a
/// head-final compound (prefix "skinke", all letters), so a "skinke" rule is vetoed.
proof fn lemma_pizza_is_vetoed(category_ok: bool, terms: Seq<Seq<Seq<char>>>)
    ensures
        deal_score(
            seq![skinke() + seq!['p', 'i', 'z', 'z', 'a']],
            seq![seq![true, true, true, true, true, true, true, true, true, true, true]],
            seq![seq![seq!['p', 'i', 'z', 'z', 'a']]],
            category_ok,
            terms,
        ) == 0,
{
    let w = skinke() + seq!['p', 'i', 'z', 'z', 'a'];
    let words = seq![w];
    let flags = seq![seq![true, true, true, true, true, true, true, true, true, true, true]];
    let pizza = seq!['p', 'i', 'z', 'z', 'a'];
    let needle = seq![pizza];
    assert(w.subrange(w.len() - 5, w.len() as int) =~= pizza);
    assert(head_final(words[0], flags[0], pizza));
    if first_phrase(words, needle, 0) < 0 {
        assert(first_head(words, flags, needle[0], 0) == 0);
    }
    assert(score(words, flags, needle) != 0);
    lemma_exclude_veto(words, flags, seq![needle], category_ok, terms, 0);
}

} // verus!

fn main() {
}
