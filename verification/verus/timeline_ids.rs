// Timeline id assignment kernel (Lucina's owner timeline, shared JSONL with Node).
//
// Production (branch lucina-rs; fixed on lucina-proofs): crates/lucina-store/src/
//   digits / value      <-> a 26-char Crockford id (ulid.rs: CROCKFORD is in ASCII order,
//                           so comparing the strings compares the digit sequences)
//   successor_digits    <-> ulid::successor (base-32 plus one, from the right)
//   new_id_value        <-> ulid::new_id (48-bit ms * 2^80 + 80-bit random; same ms -> +1)
//   choose_id           <-> TimelineStore::append: a generated id at or below the newest id
//                           in the file (and, after the fix, the newest this writer wrote)
//                           moves to its successor
// Differential tests: lucina-store/tests/verus_timeline.rs (branch lucina-proofs).
//
// Proven:
//   T1 lex_order_is_numeric_order: for equal-length ids, string order == numeric order.
//   T2 successor_is_plus_one: successor = value + 1, except at the maximum (all 'Z'),
//      where it wraps to 0 (the bug; choose_id refuses that case after the fix).
//   T3 new_id_increases: ulid::new_id's state strictly increases on every call.
//   T4 choose_id_moves_past_everything_read: an assigned id is greater than every id read
//      from the file and every id this writer appended before.
//   T5 ids_strictly_increase_per_writer: over any sequence of appends (with foreign lines,
//      rotations, truncation in between), a writer's generated ids strictly increase.
//
// Found on lucina-rs (fixed on lucina-proofs):
//   - successor("ZZZZZZZZZZZZZZZZZZZZZZZZZZ") = "00000000000000000000000000": a line with the
//     maximal id (any writer, a backfill, a typo that passes is_timeline_id) made every
//     later generated id sort first (unfixed_wraps below).
//   - only the file was consulted, so once the lines a writer bumped past were rotated or
//     truncated away, its next id could be smaller than its previous one (unfixed_regress).
use vstd::prelude::*;

verus! {

pub open spec fn base() -> nat {
    32
}

pub open spec fn digits_ok(d: Seq<u8>) -> bool {
    forall|i: int| 0 <= i < d.len() ==> #[trigger] d[i] < 32
}

pub open spec fn value(d: Seq<u8>) -> nat
    decreases d.len(),
{
    if d.len() == 0 {
        0
    } else {
        value(d.drop_last()) * base() + d.last() as nat
    }
}

pub open spec fn pow32(n: nat) -> nat
    decreases n,
{
    if n == 0 {
        1
    } else {
        pow32((n - 1) as nat) * base()
    }
}

proof fn lemma_value_bound(d: Seq<u8>)
    requires
        digits_ok(d),
    ensures
        value(d) < pow32(d.len()),
    decreases d.len(),
{
    if d.len() > 0 {
        let dl = d.drop_last();
        assert(digits_ok(dl));
        lemma_value_bound(dl);
        let v = value(dl);
        let p = pow32(dl.len());
        assert(d.last() < 32);
        assert(v * 32 + d.last() < p * 32) by (nonlinear_arith)
            requires
                v < p,
                d.last() < 32,
        ;
    }
}

// ---------------------------------------------------------------- T1

/// a < b as strings of equal length: equal up to k, smaller at k.
pub open spec fn lex_lt(a: Seq<u8>, b: Seq<u8>) -> bool {
    exists|k: int| 0 <= k < a.len() && a.subrange(0, k) == b.subrange(0, k) && #[trigger] a[k] < b[k]
}

pub proof fn lemma_lex_lt_value(a: Seq<u8>, b: Seq<u8>)
    requires
        a.len() == b.len(),
        digits_ok(a),
        digits_ok(b),
        lex_lt(a, b),
    ensures
        value(a) < value(b),
    decreases a.len(),
{
    let k = choose|k: int| 0 <= k < a.len() && a.subrange(0, k) == b.subrange(0, k) && #[trigger] a[k] < b[k];
    let n = a.len() - 1;
    let (ad, bd) = (a.drop_last(), b.drop_last());
    assert(digits_ok(ad) && digits_ok(bd));
    if k == n {
        assert(ad =~= a.subrange(0, k));
        assert(bd =~= b.subrange(0, k));
        assert(value(a) == value(ad) * 32 + a.last());
        assert(value(b) == value(bd) * 32 + b.last());
    } else {
        assert(ad.subrange(0, k) =~= a.subrange(0, k));
        assert(bd.subrange(0, k) =~= b.subrange(0, k));
        assert(ad[k] == a[k] && bd[k] == b[k]);
        assert(lex_lt(ad, bd));
        lemma_lex_lt_value(ad, bd);
        let (va, vb) = (value(ad), value(bd));
        assert(a.last() < 32);
        assert(va * 32 + a.last() < vb * 32 + b.last()) by (nonlinear_arith)
            requires
                va < vb,
                a.last() < 32,
                b.last() >= 0,
        ;
    }
}

proof fn first_diff(a: Seq<u8>, b: Seq<u8>, k: int) -> (m: int)
    requires
        a.len() == b.len(),
        0 <= k < a.len(),
        a[k] != b[k],
    ensures
        0 <= m <= k,
        a[m] != b[m],
        forall|j: int| 0 <= j < m ==> a[j] == b[j],
    decreases k,
{
    if exists|j: int| 0 <= j < k && a[j] != b[j] {
        let j = choose|j: int| 0 <= j < k && a[j] != b[j];
        first_diff(a, b, j)
    } else {
        k
    }
}

/// T1: for equal-length ids, string order is numeric order.
pub proof fn lex_order_is_numeric_order(a: Seq<u8>, b: Seq<u8>)
    requires
        a.len() == b.len(),
        digits_ok(a),
        digits_ok(b),
    ensures
        lex_lt(a, b) <==> value(a) < value(b),
        a == b <==> value(a) == value(b),
{
    if lex_lt(a, b) {
        lemma_lex_lt_value(a, b);
    }
    if lex_lt(b, a) {
        lemma_lex_lt_value(b, a);
    }
    if a != b {
        // the first difference decides
        assert(!(a =~= b));
        let k0 = choose|k: int| 0 <= k < a.len() && a[k] != b[k];
        let k = first_diff(a, b, k0);
        assert(a.subrange(0, k) =~= b.subrange(0, k));
        if a[k] < b[k] {
            assert(lex_lt(a, b));
            lemma_lex_lt_value(a, b);
        } else {
            assert(lex_lt(b, a));
            lemma_lex_lt_value(b, a);
        }
    }
}

// ---------------------------------------------------------------- T2

pub open spec fn all_max(d: Seq<u8>) -> bool {
    forall|i: int| 0 <= i < d.len() ==> #[trigger] d[i] == 31
}

pub open spec fn succ(d: Seq<u8>) -> Seq<u8>
    decreases d.len(),
{
    if d.len() == 0 {
        Seq::empty()
    } else if d.last() == 31 {
        succ(d.drop_last()).push(0)
    } else {
        d.drop_last().push((d.last() + 1) as u8)
    }
}

pub proof fn successor_is_plus_one(d: Seq<u8>)
    requires
        digits_ok(d),
    ensures
        succ(d).len() == d.len(),
        digits_ok(succ(d)),
        !all_max(d) ==> value(succ(d)) == value(d) + 1,
        all_max(d) ==> value(succ(d)) == 0,
    decreases d.len(),
{
    if d.len() > 0 {
        let dl = d.drop_last();
        assert(digits_ok(dl));
        successor_is_plus_one(dl);
        let s = succ(d);
        if d.last() == 31 {
            assert(s.drop_last() =~= succ(dl));
            assert(value(s) == value(succ(dl)) * 32);
            if all_max(d) {
                assert(all_max(dl));
            } else {
                assert(!all_max(dl)) by {
                    if all_max(dl) {
                        assert forall|i: int| 0 <= i < d.len() implies #[trigger] d[i] == 31 by {
                            if i < d.len() - 1 {
                                assert(d[i] == dl[i]);
                            }
                        }
                    }
                }
                assert(value(s) == (value(dl) + 1) * 32);
                assert((value(dl) + 1) * 32 == value(dl) * 32 + 32) by (nonlinear_arith);
            }
        } else {
            assert(s.drop_last() =~= dl);
            assert(!all_max(d));
        }
        assert forall|i: int| 0 <= i < s.len() implies #[trigger] s[i] < 32 by {
            if i < s.len() - 1 {
                assert(s[i] == s.drop_last()[i]);
            }
        }
    }
}

/// ulid::successor as the loop production runs (from the right, carry on 31).
pub fn successor_digits(d: &Vec<u8>) -> (r: Vec<u8>)
    requires
        digits_ok(d@),
    ensures
        r@ == succ(d@),
{
    let mut r = d.clone();
    let mut i = r.len();
    // r = d[0..i] ++ succ-tail; carry still pending while i > 0 and we keep going
    while i > 0
        invariant
            i <= d.len(),
            r.len() == d.len(),
            digits_ok(d@),
            r@.subrange(0, i as int) == d@.subrange(0, i as int),
            r@.subrange(i as int, r.len() as int) == Seq::new((d.len() - i) as nat, |j: int| 0u8),
            forall|k: int| i <= k < d.len() ==> d@[k] == 31,
        decreases i,
    {
        let x = r[i - 1];
        proof {
            assert(r@.subrange(0, i as int)[i - 1] == d@.subrange(0, i as int)[i - 1]);
            assert(x == d@[i - 1]);
            assert(d@[i - 1] < 32);
        }
        if x == 31 {
            let ghost r0 = r@;
            r.set(i - 1, 0);
            i -= 1;
            proof {
                assert forall|q: int| 0 <= q < i implies r@[q] == d@[q] by {
                    assert(r@[q] == r0[q]);
                    assert(r0.subrange(0, i + 1)[q] == d@.subrange(0, i + 1)[q]);
                }
                assert(r@.subrange(0, i as int) =~= d@.subrange(0, i as int));
                assert forall|q: int| i <= q < r.len() implies r@[q] == 0u8 by {
                    if q > i {
                        assert(r@[q] == r0[q]);
                        assert(r0.subrange(i + 1, r.len() as int)[q - i - 1] == 0u8);
                    }
                }
                assert(r@.subrange(i as int, r.len() as int) =~= Seq::new((d.len() - i) as nat, |j: int| 0u8));
            }
        } else {
            let ghost r0 = r@;
            r.set(i - 1, x + 1);
            proof {
                assert forall|q: int| 0 <= q < i - 1 implies r@[q] == d@[q] by {
                    assert(r0.subrange(0, i as int)[q] == d@.subrange(0, i as int)[q]);
                }
                assert forall|q: int| i <= q < r.len() implies r@[q] == 0u8 by {
                    assert(r0.subrange(i as int, r.len() as int)[q - i] == 0u8);
                }
                lemma_succ_shape(d@, (i - 1) as int);
                assert(r@ =~= d@.subrange(0, i - 1).push((x + 1) as u8) + Seq::new((d.len() - i) as nat, |j: int| 0u8));
            }
            return r;
        }
    }
    proof {
        lemma_succ_all_max(d@);
        assert(r@ =~= Seq::new(d.len() as nat, |j: int| 0u8));
    }
    r
}

/// succ when position p is the last non-31 digit.
proof fn lemma_succ_shape(d: Seq<u8>, p: int)
    requires
        0 <= p < d.len(),
        d[p] != 31,
        forall|k: int| p < k < d.len() ==> d[k] == 31,
    ensures
        succ(d) == d.subrange(0, p).push((d[p] + 1) as u8) + Seq::new((d.len() - p - 1) as nat, |j: int| 0u8),
    decreases d.len(),
{
    let dl = d.drop_last();
    if p == d.len() - 1 {
        assert(dl =~= d.subrange(0, p));
        assert(Seq::new(0, |j: int| 0u8) =~= Seq::<u8>::empty());
        assert(d.subrange(0, p).push((d[p] + 1) as u8) + Seq::<u8>::empty() =~= d.subrange(0, p).push((d[p] + 1) as u8));
    } else {
        assert(d.last() == 31);
        assert(dl[p] == d[p]);
        lemma_succ_shape(dl, p);
        assert(dl.subrange(0, p) =~= d.subrange(0, p));
        assert((dl.subrange(0, p).push((d[p] + 1) as u8) + Seq::new((dl.len() - p - 1) as nat, |j: int| 0u8)).push(0)
            =~= d.subrange(0, p).push((d[p] + 1) as u8) + Seq::new((d.len() - p - 1) as nat, |j: int| 0u8));
    }
}

proof fn lemma_succ_all_max(d: Seq<u8>)
    requires
        all_max(d),
    ensures
        succ(d) == Seq::new(d.len(), |j: int| 0u8),
    decreases d.len(),
{
    if d.len() > 0 {
        let dl = d.drop_last();
        assert(all_max(dl));
        lemma_succ_all_max(dl);
        assert(Seq::new(dl.len(), |j: int| 0u8).push(0) =~= Seq::new(d.len(), |j: int| 0u8));
    } else {
        assert(Seq::<u8>::empty() =~= Seq::new(0, |j: int| 0u8));
    }
}

/// The bug: the maximal id's successor sorts first.
pub proof fn unfixed_wraps(d: Seq<u8>)
    requires
        digits_ok(d),
        all_max(d),
        d.len() > 0,
    ensures
        value(succ(d)) < value(d),
{
    successor_is_plus_one(d);
    assert(d.last() == 31);
    assert(value(d) == value(d.drop_last()) * 32 + 31);
}

// ---------------------------------------------------------------- T3

/// ulid::new_id on the 128-bit state ms * 2^80 + random: same or older ms -> state + 1
/// (the random part's carry goes into the ms part), newer ms -> ms * 2^80 + r, r < 2^79.
pub open spec fn new_id_value(state: nat, now_ms: nat, r: nat) -> nat {
    let last_ms = state / pow2_80();
    if now_ms <= last_ms {
        state + 1
    } else {
        now_ms * pow2_80() + r
    }
}

pub open spec fn pow2_80() -> nat {
    0x1_0000_0000_0000_0000_0000
}

pub proof fn new_id_increases(state: nat, now_ms: nat, r: nat)
    requires
        r < pow2_80(),
    ensures
        new_id_value(state, now_ms, r) > state,
{
    let p = pow2_80();
    let last_ms = state / p;
    if now_ms > last_ms {
        assert(state < (last_ms + 1) * p) by (nonlinear_arith)
            requires
                last_ms == state / p,
                p > 0,
        ;
        assert((last_ms + 1) * p <= now_ms * p) by (nonlinear_arith)
            requires
                last_ms + 1 <= now_ms,
                p > 0,
        ;
    }
}

// ---------------------------------------------------------------- T4, T5

pub open spec fn max_opt(a: Option<nat>, b: Option<nat>) -> Option<nat> {
    match (a, b) {
        (Some(x), Some(y)) => Some(if x >= y { x } else { y }),
        (Some(x), None) => Some(x),
        (None, y) => y,
    }
}

/// TimelineStore::append's choice (values): the generated id, or the successor of the
/// floor when the id is not above it; None = the id space is exhausted (refused).
pub open spec fn choose_id(gen: nat, floor: Option<nat>, max_id: nat) -> Option<nat> {
    match floor {
        Some(f) => if gen > f {
            Some(gen)
        } else if f + 1 < max_id {
            Some(f + 1)
        } else {
            None
        },
        None => Some(gen),
    }
}

/// The choice on lucina-rs: the floor was the newest id in the file only, and the
/// successor wrapped.
pub open spec fn unfixed_choose_id(gen: nat, newest_read: Option<nat>, max_id: nat) -> nat {
    match newest_read {
        Some(f) => if gen > f {
            gen
        } else {
            (f + 1) % max_id
        },
        None => gen,
    }
}

/// T4
pub proof fn choose_id_moves_past_everything_read(
    gen: nat,
    read: Seq<nat>,
    newest_read: Option<nat>,
    last_written: Option<nat>,
    max_id: nat,
)
    requires
        // newest_read is the maximum of the ids read (entries.last() of the sorted index)
        match newest_read {
            Some(m) => forall|i: int| 0 <= i < read.len() ==> #[trigger] read[i] <= m,
            None => read.len() == 0,
        },
    ensures
        match choose_id(gen, max_opt(newest_read, last_written), max_id) {
            Some(id) => (forall|i: int| 0 <= i < read.len() ==> #[trigger] read[i] < id) && (match last_written {
                Some(w) => w < id,
                None => true,
            }),
            None => true,
        },
{
}

/// The bug, concretely: the writer appended 100 (bumped past a foreign line), the file
/// was rotated away, its clock-based id is 50: the next id goes backwards.
pub proof fn unfixed_regress()
    ensures
        unfixed_choose_id(50, None, 1000) < 100,
        unfixed_choose_id(50, Some(999), 1000) == 0,
{
}

/// T5: a writer's appends over time. Each step: the writer's clock-based candidate (any
/// value: new_id only guarantees it is above the previous candidate) and whatever the file
/// holds then (foreign lines, rotations, truncation). The ids it gets strictly increase.
pub open spec fn run(gens: Seq<nat>, files: Seq<Option<nat>>, last: Option<nat>, max_id: nat) -> Seq<
    Option<nat>,
>
    decreases gens.len(),
{
    if gens.len() == 0 || files.len() == 0 {
        Seq::empty()
    } else {
        let id = choose_id(gens[0], max_opt(files[0], last), max_id);
        let next_last = match id {
            Some(x) => Some(x),
            None => last,
        };
        seq![id] + run(gens.drop_first(), files.drop_first(), next_last, max_id)
    }
}

pub proof fn ids_strictly_increase_per_writer(
    gens: Seq<nat>,
    files: Seq<Option<nat>>,
    last: Option<nat>,
    max_id: nat,
    i: int,
    j: int,
)
    requires
        0 <= i < j < run(gens, files, last, max_id).len(),
        run(gens, files, last, max_id)[i] is Some,
        run(gens, files, last, max_id)[j] is Some,
    ensures
        run(gens, files, last, max_id)[i]->0 < run(gens, files, last, max_id)[j]->0,
    decreases gens.len(),
{
    let id = choose_id(gens[0], max_opt(files[0], last), max_id);
    let next_last = match id {
        Some(x) => Some(x),
        None => last,
    };
    let rest = run(gens.drop_first(), files.drop_first(), next_last, max_id);
    assert(run(gens, files, last, max_id) == seq![id] + rest);
    if i == 0 {
        lemma_run_above(gens.drop_first(), files.drop_first(), next_last, max_id, j - 1);
    } else {
        ids_strictly_increase_per_writer(gens.drop_first(), files.drop_first(), next_last, max_id, i - 1, j - 1);
    }
}

/// Every id a run assigns is above the writer's last id going in.
proof fn lemma_run_above(gens: Seq<nat>, files: Seq<Option<nat>>, last: Option<nat>, max_id: nat, k: int)
    requires
        0 <= k < run(gens, files, last, max_id).len(),
        run(gens, files, last, max_id)[k] is Some,
        last is Some,
    ensures
        last->0 < run(gens, files, last, max_id)[k]->0,
    decreases gens.len(),
{
    let id = choose_id(gens[0], max_opt(files[0], last), max_id);
    let next_last = match id {
        Some(x) => Some(x),
        None => last,
    };
    let rest = run(gens.drop_first(), files.drop_first(), next_last, max_id);
    assert(run(gens, files, last, max_id) == seq![id] + rest);
    if k > 0 {
        lemma_run_above(gens.drop_first(), files.drop_first(), next_last, max_id, k - 1);
    }
}

} // verus!
fn main() {}
