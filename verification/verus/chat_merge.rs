// The iOS chat engine's kernel: the timeline merge and order, the sync cursor loop.
//
// Production: crates/secretary-chat/src/ (Swift caller: ios/Secretary/Features/Chat/{Timeline,ChatSync,ChatStore}.swift
// through `secretary_chat_request` in crates/secretary-ffi).
//   Item                  <-> merge::Key. Ids are interned to u64 in byte order (so `<` on ids is the byte order of
//                             the strings), 0 means absent (None) for client / reply / share, `local` is
//                             "the id starts with local-", `lucina` is the role (false = user, sorts first)
//   key_lt                <-> merge::order (ts_us, role, id)
//   hit                   <-> merge::hit: the local item l is replaced by the incoming x (same role, other id, same
//                             client_message_id; Lucina: same reply_to; user: same share_id)
//   retarget              <-> the reply_to loop in merge::merge: after a user item replaced local items, every local
//                             item that replied to one of them replies to the user item
//   keep_map / step       <-> one iteration of merge::merge: x goes in (its id overwritten), the hits go out, the rest
//                             is retargeted. Swift decides the hits on a snapshot of the map and merge::merge decides
//                             them on the map as it stands with x in: the spec decides them on the whole map, so the
//                             order the locals are visited in cannot matter (lemma_step_ext, lemma_merge_order_independent)
//   ordered / part        <-> "every user item first, then every Lucina item, each group in input order"
//   run                   <-> the loop over the ordered incoming items
//   sort / ins            <-> the final sort_by(order); a stable insertion sort here
//   merge                 <-> merge::merge(...).ids (the set of surviving items, in display order)
//   repl                  <-> merge::MergeResult::replaced (its keys)
//   Plan, next_spec/next  <-> sync::SyncPlan, SyncPlan::next (cursors interned the same way, 0 = None)
//   server_ok, next_ids   <-> the server's page: the next `limit` ids after the cursor
// Differential tests: crates/secretary-chat/tests/verus_chat_merge.rs (the function bodies below, copied without the
// Verus annotations, against the production code on generated inputs) and tests/merge_props.rs (a port of the Swift
// loop with a shuffled dictionary order).
//
// Proven (merge, M):
//   M1 lemma_run_unique, lemma_merge_props: the merged sequence has distinct ids.
//   M2 lemma_sort, lemma_sorted_unique, lemma_merge_order_independent: it is strictly sorted by (ts, role, id), and a
//      strictly sorted sequence is determined by its elements, so the result is a function of the merged set alone: it
//      does not depend on the input order of `current` (distinct ids) nor on the order the locals are visited in.
//   M3 lemma_merge_idempotent: merging the same incoming batch again changes nothing (the same items, hence the same
//      order), for a batch of distinct non-local items (a server page, or the in-memory timeline `hydrate` merges).
//   M4 lemma_nothing_lost, lemma_repl_sound, lemma_nonlocal_survives: every current and every incoming id is in the
//      result or was replaced; a replaced id belongs to a local item of the role of some incoming item; so a
//      non-local item (locality being a function of the id) is never lost.
// Proven (sync, C):
//   C1 theorem_cursor_run: in a run that ends without a reset, every log id above the starting cursor and at most
//      the final cursor was in an applied page. The server is any `server_ok` (a page never moves the cursor
//      back; `more` means the log goes on past it; a page that says `more` moved forward); lemma_server_model shows
//      that "the next `limit` ids after the cursor" is such a server and that its page is exactly the log ids in
//      (after, pc]. Errors (410, other) and a stuck or missing cursor may happen at any step.
//   C2 theorem_cursor_run, lemma_sys_step: the stored cursor never moves backwards in a run without a reset.
//   C3 lemma_next_inv, lemma_fetch_budget, lemma_sys_step: at most 11 fetches (10 cursor pages and the fresh window),
//      1 without a cursor; and a reset is reported exactly for the fresh window: Apply{reset} is true iff the fetch
//      before it was `after = None` in a run that started with a cursor.
//
// Found while proving (kept identical to Swift, so the property is weaker than the obvious statement):
//   - The merge is NOT idempotent when the batch carries a local item that a server item of the same batch replaces and
//     a local reply points at that local item. Counterexample (checked by the Rust test
//     `idempotence_fails_only_when_the_batch_carries_a_local_item`):
//         current  = [R: lucina local-r, reply_to local-c1]
//         incoming = [S: user tl_10, client c1; U: user local-c1, client c1; S2: lucina tl_11, reply_to tl_10]
//         merge(current, incoming)         = [local-c1, tl_10, local-r, tl_11]
//             S went in before U, so nothing replaced U, and R still points at local-c1.
//         merge(that, incoming)            = [local-c1, tl_10, tl_11]
//             now S replaces U (U is in the map), R is repointed at tl_10, and S2 (reply_to tl_10) replaces R.
//     So the id set and order are idempotent only for batches without such a local item; M3 is proved for a batch of
//     non-local items, which is every server page. `hydrate` merges the in-memory timeline (locals included) as the
//     incoming batch, but into a current that has already seen it, and the case needs a pending local reply.
//   - "Every incoming id is in the result" (the obvious M4) is false for an incoming local item that a later incoming
//     item of the same batch replaces (hydrate again); lemma_nothing_lost says "or it was replaced".
//   - Duplicate ids in `current` collapse to the last one in Swift; the proofs take distinct ids in `current`
//     (a Swift timeline never has two items of one id) and production keeps Swift's last-wins.
use vstd::prelude::*;

verus! {


// ---------------------------------------------------------------- items

/// A timeline item as the merge sees it (`secretary_chat::merge::Key`). Ids are interned: `id`, `client`,
/// `reply` and `share` are naturals, 0 means absent (`None`), and the interning keeps the byte order of the ids.
pub struct Item {
    pub id: u64,
    pub ts: i64,
    pub lucina: bool,
    pub local: bool,
    pub client: u64,
    pub reply: u64,
    pub share: u64,
}

// ---------------------------------------------------------------- generic Seq helpers

proof fn lemma_push_contains<A>(s: Seq<A>, v: A, a: A)
    ensures
        s.push(v).contains(a) <==> (s.contains(a) || a == v),
{
    if s.contains(a) {
        let i = choose|i: int| 0 <= i < s.len() && s[i] == a;
        assert(s.push(v)[i] == a);
    }
    if a == v {
        assert(s.push(v)[s.len() as int] == a);
    }
    if s.push(v).contains(a) {
        let k = choose|k: int| 0 <= k < s.push(v).len() && s.push(v)[k] == a;
        if k < s.len() {
            assert(s[k] == a);
        }
    }
}

proof fn lemma_add_contains<A>(a: Seq<A>, b: Seq<A>, o: A)
    ensures
        (a + b).contains(o) <==> (a.contains(o) || b.contains(o)),
{
    if a.contains(o) {
        let i = choose|i: int| 0 <= i < a.len() && a[i] == o;
        assert((a + b)[i] == o);
    }
    if b.contains(o) {
        let j = choose|j: int| 0 <= j < b.len() && b[j] == o;
        assert((a + b)[a.len() + j] == o);
    }
    if (a + b).contains(o) {
        let k = choose|k: int| 0 <= k < (a + b).len() && (a + b)[k] == o;
        if k < a.len() {
            assert(a[k] == o);
        } else {
            assert(b[k - a.len()] == o);
        }
    }
}

proof fn lemma_singleton_contains<A>(x: A, o: A)
    ensures
        seq![x].contains(o) <==> o == x,
{
    if o == x {
        assert(seq![x][0] == o);
    }
    if seq![x].contains(o) {
        let k = choose|k: int| 0 <= k < seq![x].len() && seq![x][k] == o;
        assert(k == 0);
    }
}

/// `s` is its `drop_last` with the `last` pushed back, so membership splits.
proof fn lemma_last_contains<A>(s: Seq<A>, o: A)
    requires
        s.len() > 0,
    ensures
        s.contains(o) <==> (s.drop_last().contains(o) || o == s.last()),
{
    assert(s =~= s.drop_last().push(s.last()));
    lemma_push_contains(s.drop_last(), s.last(), o);
}

/// `s` is its first element followed by `drop_first`.
proof fn lemma_first_contains<A>(s: Seq<A>, o: A)
    requires
        s.len() > 0,
    ensures
        s.contains(o) <==> (s.drop_first().contains(o) || o == s[0]),
{
    assert(s =~= seq![s[0]] + s.drop_first());
    lemma_add_contains(seq![s[0]], s.drop_first(), o);
    lemma_singleton_contains(s[0], o);
}

// ---------------------------------------------------------------- the display order

/// `merge::order`: time, the user before Lucina, then the id (byte order, interned).
pub open spec fn key_lt(a: Item, b: Item) -> bool {
    a.ts < b.ts || (a.ts == b.ts && ((!a.lucina && b.lucina) || (a.lucina == b.lucina && a.id
        < b.id)))
}

proof fn lemma_key_order(a: Item, b: Item, c: Item)
    ensures
        !key_lt(a, a),
        key_lt(a, b) ==> !key_lt(b, a),
        key_lt(a, b) && key_lt(b, c) ==> key_lt(a, c),
        a.id != b.id ==> (key_lt(a, b) || key_lt(b, a)),
{
}

pub open spec fn sorted(s: Seq<Item>) -> bool {
    forall|i: int, j: int| 0 <= i < j < s.len() ==> key_lt(#[trigger] s[i], #[trigger] s[j])
}

pub open spec fn unique_ids(s: Seq<Item>) -> bool {
    forall|i: int, j: int| 0 <= i < j < s.len() ==> (#[trigger] s[i]).id != (#[trigger] s[j]).id
}

proof fn lemma_unique_contains(s: Seq<Item>, a: Item, b: Item)
    requires
        unique_ids(s),
        s.contains(a),
        s.contains(b),
        a.id == b.id,
    ensures
        a == b,
{
    let i = choose|i: int| 0 <= i < s.len() && s[i] == a;
    let j = choose|j: int| 0 <= j < s.len() && s[j] == b;
    if i < j {
        assert(s[i].id != s[j].id);
    } else if j < i {
        assert(s[j].id != s[i].id);
    }
}

proof fn lemma_drop_last_props(s: Seq<Item>)
    requires
        s.len() > 0,
    ensures
        sorted(s) ==> sorted(s.drop_last()),
        unique_ids(s) ==> unique_ids(s.drop_last()),
        unique_ids(s) ==> forall|k: int|
            0 <= k < s.len() - 1 ==> (#[trigger] s.drop_last()[k]).id != s.last().id,
{
    assert forall|k: int| 0 <= k < s.drop_last().len() implies s.drop_last()[k] == s[k] by {}
}

proof fn lemma_drop_first_props(s: Seq<Item>)
    requires
        s.len() > 0,
    ensures
        sorted(s) ==> sorted(s.drop_first()),
        unique_ids(s) ==> unique_ids(s.drop_first()),
        sorted(s) ==> forall|k: int| 0 <= k < s.drop_first().len() ==> key_lt(s[0], #[trigger] s.drop_first()[k]),
        unique_ids(s) ==> forall|k: int| 0 <= k < s.drop_first().len() ==> s[0].id != (#[trigger] s.drop_first()[k]).id,
{
    assert forall|k: int| 0 <= k < s.drop_first().len() implies s.drop_first()[k] == s[k + 1] by {}
}

/// Insert `x` before the first element it sorts before.
pub open spec fn ins(s: Seq<Item>, x: Item) -> Seq<Item>
    decreases s.len(),
{
    if s.len() == 0 {
        seq![x]
    } else if key_lt(x, s[0]) {
        seq![x] + s
    } else {
        seq![s[0]] + ins(s.drop_first(), x)
    }
}

proof fn lemma_ins(s: Seq<Item>, x: Item)
    requires
        sorted(s),
        unique_ids(s),
        forall|k: int| 0 <= k < s.len() ==> (#[trigger] s[k]).id != x.id,
    ensures
        sorted(ins(s, x)),
        unique_ids(ins(s, x)),
        ins(s, x).len() == s.len() + 1,
        forall|o: Item| #[trigger] ins(s, x).contains(o) <==> (s.contains(o) || o == x),
    decreases s.len(),
{
    if s.len() == 0 {
        assert(ins(s, x) =~= seq![x]);
        assert forall|o: Item| #[trigger] ins(s, x).contains(o) <==> (s.contains(o) || o == x) by {
            lemma_singleton_contains(x, o);
        }
    } else if key_lt(x, s[0]) {
        let r = seq![x] + s;
        assert(ins(s, x) =~= r);
        assert forall|i: int, j: int| 0 <= i < j < r.len() implies key_lt(#[trigger] r[i], #[trigger] r[j]) by {
            if i == 0 {
                assert(r[0] == x);
                assert(r[j] == s[j - 1]);
                if j - 1 > 0 {
                    lemma_key_order(x, s[0], s[j - 1]);
                }
            } else {
                assert(r[i] == s[i - 1]);
                assert(r[j] == s[j - 1]);
            }
        }
        assert forall|i: int, j: int| 0 <= i < j < r.len() implies (#[trigger] r[i]).id != (#[trigger] r[j]).id by {
            if i == 0 {
                assert(r[0] == x);
                assert(r[j] == s[j - 1]);
            } else {
                assert(r[i] == s[i - 1]);
                assert(r[j] == s[j - 1]);
            }
        }
        assert forall|o: Item| #[trigger] ins(s, x).contains(o) <==> (s.contains(o) || o == x) by {
            lemma_add_contains(seq![x], s, o);
            lemma_singleton_contains(x, o);
        }
    } else {
        let rest = s.drop_first();
        lemma_drop_first_props(s);
        assert forall|k: int| 0 <= k < rest.len() implies (#[trigger] rest[k]).id != x.id by {
            assert(rest[k] == s[k + 1]);
        }
        lemma_ins(rest, x);
        let t = ins(rest, x);
        let r = seq![s[0]] + t;
        assert(ins(s, x) =~= r);
        lemma_key_order(x, s[0], x);
        // s[0] sorts before x: the order is total on distinct ids
        assert(s[0].id != x.id);
        lemma_key_order(s[0], x, x);
        assert(key_lt(s[0], x));
        // s[0] sorts before and differs from everything in t
        assert forall|k: int| 0 <= k < t.len() implies key_lt(s[0], #[trigger] t[k]) && s[0].id != t[k].id by {
            assert(t.contains(t[k]));
            if rest.contains(t[k]) {
                let q = choose|q: int| 0 <= q < rest.len() && rest[q] == t[k];
                assert(rest[q] == s[q + 1]);
                assert(key_lt(s[0], s[q + 1]));
                assert(s[0].id != s[q + 1].id);
            } else {
                assert(t[k] == x);
            }
        }
        assert forall|i: int, j: int| 0 <= i < j < r.len() implies key_lt(#[trigger] r[i], #[trigger] r[j]) by {
            if i == 0 {
                assert(r[0] == s[0]);
                assert(r[j] == t[j - 1]);
            } else {
                assert(r[i] == t[i - 1]);
                assert(r[j] == t[j - 1]);
            }
        }
        assert forall|i: int, j: int| 0 <= i < j < r.len() implies (#[trigger] r[i]).id != (#[trigger] r[j]).id by {
            if i == 0 {
                assert(r[0] == s[0]);
                assert(r[j] == t[j - 1]);
            } else {
                assert(r[i] == t[i - 1]);
                assert(r[j] == t[j - 1]);
            }
        }
        assert forall|o: Item| #[trigger] ins(s, x).contains(o) <==> (s.contains(o) || o == x) by {
            lemma_add_contains(seq![s[0]], t, o);
            lemma_singleton_contains(s[0], o);
            lemma_first_contains(s, o);
        }
    }
}

/// Insertion sort.
pub open spec fn sort(s: Seq<Item>) -> Seq<Item>
    decreases s.len(),
{
    if s.len() == 0 {
        Seq::empty()
    } else {
        ins(sort(s.drop_last()), s.last())
    }
}

/// M1 + M2 for the sort: from distinct ids it gives a strictly sorted sequence with the same elements.
proof fn lemma_sort(s: Seq<Item>)
    requires
        unique_ids(s),
    ensures
        sorted(sort(s)),
        unique_ids(sort(s)),
        sort(s).len() == s.len(),
        forall|o: Item| #[trigger] sort(s).contains(o) <==> s.contains(o),
    decreases s.len(),
{
    if s.len() > 0 {
        let dl = s.drop_last();
        lemma_drop_last_props(s);
        lemma_sort(dl);
        let t = sort(dl);
        assert forall|k: int| 0 <= k < t.len() implies (#[trigger] t[k]).id != s.last().id by {
            assert(t.contains(t[k]));
            let q = choose|q: int| 0 <= q < dl.len() && dl[q] == t[k];
            assert(dl[q] == s[q]);
        }
        lemma_ins(t, s.last());
        assert forall|o: Item| #[trigger] sort(s).contains(o) <==> s.contains(o) by {
            lemma_last_contains(s, o);
        }
    }
}

/// M2: a strictly sorted sequence is determined by its elements, so the order is a function of the set.
proof fn lemma_sorted_unique(s: Seq<Item>, t: Seq<Item>)
    requires
        sorted(s),
        sorted(t),
        forall|o: Item| s.contains(o) <==> t.contains(o),
    ensures
        s =~= t,
    decreases s.len(),
{
    if s.len() == 0 {
        if t.len() > 0 {
            assert(t.contains(t[0]));
            assert(s.contains(t[0]));
        }
    } else {
        assert(s.contains(s[0]));
        assert(t.contains(s[0]));
        assert(t.len() > 0);
        assert(t.contains(t[0]));
        assert(s.contains(t[0]));
        let j = choose|j: int| 0 <= j < t.len() && t[j] == s[0];
        let i = choose|i: int| 0 <= i < s.len() && s[i] == t[0];
        lemma_key_order(s[0], t[0], s[0]);
        if t[0] != s[0] {
            assert(j > 0);
            assert(i > 0);
            assert(key_lt(t[0], t[j]));
            assert(key_lt(s[0], s[i]));
        }
        assert(s[0] == t[0]);
        lemma_drop_first_props(s);
        lemma_drop_first_props(t);
        assert forall|o: Item| s.drop_first().contains(o) <==> t.drop_first().contains(o) by {
            lemma_first_contains(s, o);
            lemma_first_contains(t, o);
            if s.drop_first().contains(o) {
                let k = choose|k: int| 0 <= k < s.drop_first().len() && s.drop_first()[k] == o;
                assert(key_lt(s[0], o));
                lemma_key_order(o, s[0], o);
            }
            if t.drop_first().contains(o) {
                let k = choose|k: int| 0 <= k < t.drop_first().len() && t.drop_first()[k] == o;
                assert(key_lt(t[0], o));
                lemma_key_order(o, t[0], o);
            }
        }
        lemma_sorted_unique(s.drop_first(), t.drop_first());
        assert(s =~= seq![s[0]] + s.drop_first());
        assert(t =~= seq![t[0]] + t.drop_first());
    }
}

// ---------------------------------------------------------------- the merge step

/// Is the local item `e` replaced by the incoming `x`? (`merge::hit`; `x` itself is never replaced.)
pub open spec fn hit(x: Item, e: Item) -> bool {
    e.local && e.lucina == x.lucina && e.id != x.id && ((x.client != 0 && x.client == e.client) || (
    x.lucina && x.reply != 0 && x.reply == e.reply) || (!x.lucina && x.share != 0 && x.share
        == e.share))
}

/// Some item of `m` with this id is replaced by `x`.
pub open spec fn hit_id(m: Seq<Item>, x: Item, id: u64) -> bool {
    exists|e: Item| #[trigger] m.contains(e) && hit(x, e) && e.id == id
}

/// The `reply_to` update: after a user item replaced local items, the local items that replied to
/// one of them reply to the user item.
pub open spec fn retarget(m: Seq<Item>, x: Item, e: Item) -> Item {
    if !x.lucina && e.local && e.reply != 0 && hit_id(m, x, e.reply) {
        Item {
            id: e.id,
            ts: e.ts,
            lucina: e.lucina,
            local: e.local,
            client: e.client,
            reply: x.id,
            share: e.share,
        }
    } else {
        e
    }
}

/// The items of `s` that survive `x` (not replaced, not overwritten by it), retargeted. The hits and the
/// retargeting are decided on the whole map `m`, so the visiting order does not matter.
pub open spec fn keep_map(s: Seq<Item>, m: Seq<Item>, x: Item) -> Seq<Item>
    decreases s.len(),
{
    if s.len() == 0 {
        Seq::empty()
    } else {
        let e = s.last();
        let rest = keep_map(s.drop_last(), m, x);
        if e.id != x.id && !hit(x, e) {
            rest.push(retarget(m, x, e))
        } else {
            rest
        }
    }
}

/// One incoming item `x` against the map `m` (`merge::merge`'s loop body).
pub open spec fn step(m: Seq<Item>, x: Item) -> Seq<Item> {
    keep_map(m, m, x).push(retarget(m, x, x))
}

/// The incoming items in the order Swift processes them.
pub open spec fn run(m: Seq<Item>, ord: Seq<Item>) -> Seq<Item>
    decreases ord.len(),
{
    if ord.len() == 0 {
        m
    } else {
        step(run(m, ord.drop_last()), ord.last())
    }
}

/// The items of `s` written by `lucina` (or the user), in order.
pub open spec fn part(s: Seq<Item>, l: bool) -> Seq<Item>
    decreases s.len(),
{
    if s.len() == 0 {
        Seq::empty()
    } else {
        let r = part(s.drop_last(), l);
        if s.last().lucina == l {
            r.push(s.last())
        } else {
            r
        }
    }
}

/// Every user item first, then every Lucina item.
pub open spec fn ordered(inc: Seq<Item>) -> Seq<Item> {
    part(inc, false) + part(inc, true)
}

/// `merge::merge` on distinct-id `current`: the display order of the final map.
pub open spec fn merge(cur: Seq<Item>, inc: Seq<Item>) -> Seq<Item> {
    sort(run(cur, ordered(inc)))
}

proof fn lemma_retarget(m: Seq<Item>, x: Item, e: Item)
    ensures
        retarget(m, x, e).id == e.id,
        retarget(m, x, e).ts == e.ts,
        retarget(m, x, e).lucina == e.lucina,
        retarget(m, x, e).local == e.local,
        retarget(m, x, e).client == e.client,
        retarget(m, x, e).share == e.share,
        !e.local ==> retarget(m, x, e) == e,
        x.lucina ==> retarget(m, x, e) == e,
        retarget(m, x, e) != e ==> (!x.lucina && e.local),
{
}

/// A user item's `hit` does not read `reply`.
proof fn lemma_hit_user(x: Item, e: Item, e2: Item)
    requires
        !x.lucina,
        e.id == e2.id,
        e.local == e2.local,
        e.lucina == e2.lucina,
        e.client == e2.client,
        e.share == e2.share,
    ensures
        hit(x, e) == hit(x, e2),
{
}

proof fn lemma_keep_map(s: Seq<Item>, m: Seq<Item>, x: Item, o: Item)
    ensures
        keep_map(s, m, x).contains(o) <==> exists|e: Item|
            #[trigger] s.contains(e) && e.id != x.id && !hit(x, e) && o == retarget(m, x, e),
    decreases s.len(),
{
    if s.len() > 0 {
        let dl = s.drop_last();
        let e0 = s.last();
        let rest = keep_map(dl, m, x);
        lemma_keep_map(dl, m, x, o);
        if e0.id != x.id && !hit(x, e0) {
            lemma_push_contains(rest, retarget(m, x, e0), o);
        }
        assert forall|e: Item| s.contains(e) <==> (dl.contains(e) || e == e0) by {
            lemma_last_contains(s, e);
        }
        if keep_map(s, m, x).contains(o) {
            if rest.contains(o) {
                let e = choose|e: Item|
                    dl.contains(e) && e.id != x.id && !hit(x, e) && o == retarget(m, x, e);
                assert(s.contains(e));
            } else {
                assert(o == retarget(m, x, e0));
                assert(s.contains(e0));
            }
        }
        if exists|e: Item| s.contains(e) && e.id != x.id && !hit(x, e) && o == retarget(m, x, e) {
            let e = choose|e: Item|
                s.contains(e) && e.id != x.id && !hit(x, e) && o == retarget(m, x, e);
            if e == e0 {
                assert(keep_map(s, m, x) == rest.push(retarget(m, x, e0)));
                lemma_push_contains(rest, retarget(m, x, e0), o);
            } else {
                assert(dl.contains(e));
                assert(rest.contains(o));
                if e0.id != x.id && !hit(x, e0) {
                    lemma_push_contains(rest, retarget(m, x, e0), o);
                }
            }
        }
    }
}

/// The items after one step: `x` itself, and every other item that is not replaced.
proof fn lemma_step(m: Seq<Item>, x: Item, o: Item)
    ensures
        step(m, x).contains(o) <==> (o == retarget(m, x, x) || exists|e: Item|
            #[trigger] m.contains(e) && e.id != x.id && !hit(x, e) && o == retarget(m, x, e)),
{
    lemma_keep_map(m, m, x, o);
    lemma_push_contains(keep_map(m, m, x), retarget(m, x, x), o);
}

/// What `keep_map` keeps: ids of `s`, never `x`'s, and one per source item.
proof fn lemma_keep_map_ids(s: Seq<Item>, m: Seq<Item>, x: Item)
    requires
        unique_ids(s),
    ensures
        unique_ids(keep_map(s, m, x)),
        forall|k: int|
            0 <= k < keep_map(s, m, x).len() ==> (#[trigger] keep_map(s, m, x)[k]).id != x.id,
    decreases s.len(),
{
    if s.len() > 0 {
        let dl = s.drop_last();
        let e0 = s.last();
        lemma_drop_last_props(s);
        lemma_keep_map_ids(dl, m, x);
        let rest = keep_map(dl, m, x);
        if e0.id != x.id && !hit(x, e0) {
            let r = rest.push(retarget(m, x, e0));
            lemma_retarget(m, x, e0);
            assert forall|k: int| 0 <= k < rest.len() implies (#[trigger] rest[k]).id != e0.id by {
                assert(rest.contains(rest[k]));
                lemma_keep_map(dl, m, x, rest[k]);
                let e = choose|e: Item|
                    dl.contains(e) && e.id != x.id && !hit(x, e) && rest[k] == retarget(m, x, e);
                lemma_retarget(m, x, e);
                let q = choose|q: int| 0 <= q < dl.len() && dl[q] == e;
                assert(dl[q] == s[q]);
            }
            assert forall|i: int, j: int| 0 <= i < j < r.len() implies (#[trigger] r[i]).id != (
            #[trigger] r[j]).id by {
                if j == r.len() - 1 {
                    assert(r[j] == retarget(m, x, e0));
                    assert(r[i] == rest[i]);
                } else {
                    assert(r[i] == rest[i]);
                    assert(r[j] == rest[j]);
                }
            }
            assert forall|k: int| 0 <= k < r.len() implies (#[trigger] r[k]).id != x.id by {
                if k == r.len() - 1 {
                    assert(r[k] == retarget(m, x, e0));
                } else {
                    assert(r[k] == rest[k]);
                }
            }
        }
    }
}

/// M1 for one step.
proof fn lemma_step_unique(m: Seq<Item>, x: Item)
    requires
        unique_ids(m),
    ensures
        unique_ids(step(m, x)),
{
    lemma_keep_map_ids(m, m, x);
    let kmap = keep_map(m, m, x);
    let r = kmap.push(retarget(m, x, x));
    lemma_retarget(m, x, x);
    assert forall|i: int, j: int| 0 <= i < j < r.len() implies (#[trigger] r[i]).id != (
    #[trigger] r[j]).id by {
        if j == r.len() - 1 {
            assert(r[j] == retarget(m, x, x));
            assert(r[i] == kmap[i]);
        } else {
            assert(r[i] == kmap[i]);
            assert(r[j] == kmap[j]);
        }
    }
}

/// M1 for the whole run.
proof fn lemma_run_unique(m: Seq<Item>, ord: Seq<Item>)
    requires
        unique_ids(m),
    ensures
        unique_ids(run(m, ord)),
    decreases ord.len(),
{
    if ord.len() > 0 {
        lemma_run_unique(m, ord.drop_last());
        lemma_step_unique(run(m, ord.drop_last()), ord.last());
    }
}

// ---------------------------------------------------------------- M4: nothing is lost

pub open spec fn has_id(s: Seq<Item>, id: u64) -> bool {
    exists|o: Item| #[trigger] s.contains(o) && o.id == id
}

/// `e` comes from the current or the incoming items.
pub open spec fn in2(m: Seq<Item>, ord: Seq<Item>, e: Item) -> bool {
    m.contains(e) || ord.contains(e)
}

/// The ids replaced while the incoming items are processed (the `replaced` map's keys).
pub open spec fn repl(m: Seq<Item>, ord: Seq<Item>, id: u64) -> bool
    decreases ord.len(),
{
    if ord.len() == 0 {
        false
    } else {
        repl(m, ord.drop_last(), id) || hit_id(run(m, ord.drop_last()), ord.last(), id)
    }
}

proof fn lemma_step_has_x(m: Seq<Item>, x: Item)
    ensures
        has_id(step(m, x), x.id),
{
    lemma_retarget(m, x, x);
    lemma_step(m, x, retarget(m, x, x));
    assert(step(m, x).contains(retarget(m, x, x)));
}

proof fn lemma_step_keeps(m: Seq<Item>, x: Item, e: Item)
    requires
        m.contains(e),
    ensures
        has_id(step(m, x), e.id) || hit_id(m, x, e.id),
{
    lemma_retarget(m, x, e);
    if e.id != x.id && !hit(x, e) {
        lemma_step(m, x, retarget(m, x, e));
        assert(step(m, x).contains(retarget(m, x, e)));
    } else if hit(x, e) {
        assert(hit_id(m, x, e.id));
    } else {
        lemma_step_has_x(m, x);
    }
}

/// M4, part 1: every current and incoming id is in the result or was replaced.
proof fn lemma_nothing_lost(m: Seq<Item>, ord: Seq<Item>, e: Item)
    requires
        in2(m, ord, e),
    ensures
        has_id(run(m, ord), e.id) || repl(m, ord, e.id),
    decreases ord.len(),
{
    if ord.len() == 0 {
        assert(m.contains(e));
        assert(run(m, ord) == m);
    } else {
        let dl = ord.drop_last();
        let x = ord.last();
        let r = run(m, dl);
        lemma_last_contains(ord, e);
        if e == x {
            lemma_step_has_x(r, x);
        } else {
            lemma_nothing_lost(m, dl, e);
            if has_id(r, e.id) {
                let w = choose|w: Item| r.contains(w) && w.id == e.id;
                lemma_step_keeps(r, x, w);
            }
        }
    }
}

/// Every item of a run has an origin with the same id, locality and role.
proof fn lemma_run_origin(m: Seq<Item>, ord: Seq<Item>, o: Item)
    requires
        run(m, ord).contains(o),
    ensures
        exists|e: Item|
            #[trigger] in2(m, ord, e) && e.id == o.id && e.local == o.local && e.lucina == o.lucina,
    decreases ord.len(),
{
    if ord.len() == 0 {
        assert(run(m, ord) == m);
        assert(in2(m, ord, o));
    } else {
        let dl = ord.drop_last();
        let x = ord.last();
        let r = run(m, dl);
        lemma_step(r, x, o);
        if o == retarget(r, x, x) {
            lemma_retarget(r, x, x);
            lemma_last_contains(ord, x);
            assert(in2(m, ord, x));
        } else {
            let e = choose|e: Item|
                r.contains(e) && e.id != x.id && !hit(x, e) && o == retarget(r, x, e);
            lemma_retarget(r, x, e);
            assert(run(m, dl).contains(e));
            lemma_run_origin(m, dl, e);
            let e2 = choose|e2: Item|
                in2(m, dl, e2) && e2.id == e.id && e2.local == e.local && e2.lucina == e.lucina;
            if dl.contains(e2) {
                lemma_last_contains(ord, e2);
            }
            assert(in2(m, ord, e2));
        }
    }
}

/// M4, part 2: a replaced id belongs to a local item of the same role as some incoming item.
proof fn lemma_repl_sound(m: Seq<Item>, ord: Seq<Item>, id: u64)
    requires
        repl(m, ord, id),
    ensures
        exists|x: Item, e: Item|
            #![trigger ord.contains(x), in2(m, ord, e)]
            ord.contains(x) && in2(m, ord, e) && e.id == id && e.local && e.lucina
                == x.lucina && e.id != x.id,
    decreases ord.len(),
{
    let dl = ord.drop_last();
    let x = ord.last();
    let r = run(m, dl);
    if repl(m, dl, id) {
        lemma_repl_sound(m, dl, id);
        let (x2, e2) = choose|x2: Item, e2: Item|
            #![trigger dl.contains(x2), in2(m, dl, e2)]
            dl.contains(x2) && in2(m, dl, e2) && e2.id == id && e2.local && e2.lucina
                == x2.lucina && e2.id != x2.id;
        lemma_last_contains(ord, x2);
        if dl.contains(e2) {
            lemma_last_contains(ord, e2);
        }
        assert(in2(m, ord, e2));
    } else {
        let w = choose|w: Item| r.contains(w) && hit(x, w) && w.id == id;
        lemma_run_origin(m, dl, w);
        let e = choose|e: Item|
            in2(m, dl, e) && e.id == w.id && e.local == w.local && e.lucina == w.lucina;
        lemma_last_contains(ord, x);
        if dl.contains(e) {
            lemma_last_contains(ord, e);
        }
        assert(in2(m, ord, e));
    }
}

/// M4, part 3: a non-local item (locality is a function of the id) is never lost.
proof fn lemma_nonlocal_survives(m: Seq<Item>, ord: Seq<Item>, lid: spec_fn(u64) -> bool, e: Item)
    requires
        forall|a: Item| #[trigger] in2(m, ord, a) ==> a.local == lid(a.id),
        in2(m, ord, e),
        !e.local,
    ensures
        has_id(run(m, ord), e.id),
{
    lemma_nothing_lost(m, ord, e);
    if repl(m, ord, e.id) {
        lemma_repl_sound(m, ord, e.id);
        let (x, e2) = choose|x: Item, e2: Item|
            #![trigger ord.contains(x), in2(m, ord, e2)]
            ord.contains(x) && in2(m, ord, e2) && e2.id == e.id && e2.local && e2.lucina
                == x.lucina && e2.id != x.id;
        assert(e2.local == lid(e2.id));
        assert(e.local == lid(e.id));
    }
}

// ---------------------------------------------------------------- M2 for the whole merge

proof fn lemma_hit_id_ext(m1: Seq<Item>, m2: Seq<Item>, x: Item, id: u64)
    requires
        forall|o: Item| m1.contains(o) <==> m2.contains(o),
    ensures
        hit_id(m1, x, id) <==> hit_id(m2, x, id),
{
    if hit_id(m1, x, id) {
        let e = choose|e: Item| m1.contains(e) && hit(x, e) && e.id == id;
        assert(m2.contains(e));
    }
    if hit_id(m2, x, id) {
        let e = choose|e: Item| m2.contains(e) && hit(x, e) && e.id == id;
        assert(m1.contains(e));
    }
}

proof fn lemma_retarget_ext(m1: Seq<Item>, m2: Seq<Item>, x: Item, e: Item)
    requires
        forall|o: Item| m1.contains(o) <==> m2.contains(o),
    ensures
        retarget(m1, x, e) == retarget(m2, x, e),
{
    if !x.lucina && e.local && e.reply != 0 {
        lemma_hit_id_ext(m1, m2, x, e.reply);
    }
}

proof fn lemma_step_ext(m1: Seq<Item>, m2: Seq<Item>, x: Item)
    requires
        forall|o: Item| m1.contains(o) <==> m2.contains(o),
    ensures
        forall|o: Item| step(m1, x).contains(o) <==> step(m2, x).contains(o),
{
    assert forall|o: Item| step(m1, x).contains(o) <==> step(m2, x).contains(o) by {
        lemma_step(m1, x, o);
        lemma_step(m2, x, o);
        lemma_retarget_ext(m1, m2, x, x);
        if step(m1, x).contains(o) && o != retarget(m1, x, x) {
            let e = choose|e: Item|
                m1.contains(e) && e.id != x.id && !hit(x, e) && o == retarget(m1, x, e);
            lemma_retarget_ext(m1, m2, x, e);
            assert(m2.contains(e));
        }
        if step(m2, x).contains(o) && o != retarget(m2, x, x) {
            let e = choose|e: Item|
                m2.contains(e) && e.id != x.id && !hit(x, e) && o == retarget(m2, x, e);
            lemma_retarget_ext(m1, m2, x, e);
            assert(m1.contains(e));
        }
    }
}

proof fn lemma_run_ext(m1: Seq<Item>, m2: Seq<Item>, ord: Seq<Item>)
    requires
        forall|o: Item| m1.contains(o) <==> m2.contains(o),
    ensures
        forall|o: Item| run(m1, ord).contains(o) <==> run(m2, ord).contains(o),
    decreases ord.len(),
{
    if ord.len() > 0 {
        lemma_run_ext(m1, m2, ord.drop_last());
        lemma_step_ext(run(m1, ord.drop_last()), run(m2, ord.drop_last()), ord.last());
    }
}

/// M1 + M2 of the merge: distinct ids, strictly sorted, the elements of the run.
proof fn lemma_merge_props(cur: Seq<Item>, inc: Seq<Item>)
    requires
        unique_ids(cur),
    ensures
        sorted(merge(cur, inc)),
        unique_ids(merge(cur, inc)),
        forall|o: Item| #[trigger] merge(cur, inc).contains(o) <==> run(cur, ordered(inc)).contains(o),
{
    lemma_run_unique(cur, ordered(inc));
    lemma_sort(run(cur, ordered(inc)));
}

/// M2: the result does not depend on the input order of `current` (or on the order the locals are visited).
proof fn lemma_merge_order_independent(c1: Seq<Item>, c2: Seq<Item>, inc: Seq<Item>)
    requires
        unique_ids(c1),
        unique_ids(c2),
        forall|o: Item| c1.contains(o) <==> c2.contains(o),
    ensures
        merge(c1, inc) == merge(c2, inc),
{
    lemma_run_ext(c1, c2, ordered(inc));
    lemma_merge_props(c1, inc);
    lemma_merge_props(c2, inc);
    assert forall|o: Item| merge(c1, inc).contains(o) <==> merge(c2, inc).contains(o) by {
        assert(merge(c1, inc).contains(o) <==> run(c1, ordered(inc)).contains(o));
        assert(merge(c2, inc).contains(o) <==> run(c2, ordered(inc)).contains(o));
    }
    lemma_sorted_unique(merge(c1, inc), merge(c2, inc));
}

// ---------------------------------------------------------------- M3: idempotence

/// No user item after a Lucina item (what `ordered` produces).
pub open spec fn phased(ord: Seq<Item>) -> bool {
    forall|i: int, j: int|
        0 <= i < j < ord.len() ==> (#[trigger] ord[i]).lucina ==> (#[trigger] ord[j]).lucina
}

pub open spec fn all_nonlocal(ord: Seq<Item>) -> bool {
    forall|k: int| 0 <= k < ord.len() ==> !(#[trigger] ord[k]).local
}

proof fn lemma_drop_last_ord(ord: Seq<Item>)
    requires
        ord.len() > 0,
    ensures
        phased(ord) ==> phased(ord.drop_last()),
        all_nonlocal(ord) ==> all_nonlocal(ord.drop_last()),
        unique_ids(ord) ==> unique_ids(ord.drop_last()),
        forall|k: int| 0 <= k < ord.drop_last().len() ==> (#[trigger] ord.drop_last()[k]) == ord[k],
        ord.last() == ord[ord.len() - 1],
{
    assert forall|k: int| 0 <= k < ord.drop_last().len() implies (#[trigger] ord.drop_last()[k]) == ord[k] by {}
}

/// After the run, no kept local item is replaced by any incoming item.
proof fn lemma_no_hits(c: Seq<Item>, ord: Seq<Item>)
    requires
        phased(ord),
        all_nonlocal(ord),
    ensures
        forall|o: Item, j: int|
            run(c, ord).contains(o) && 0 <= j < ord.len() ==> !(#[trigger] hit(ord[j], o)),
    decreases ord.len(),
{
    if ord.len() > 0 {
        let dl = ord.drop_last();
        let x = ord.last();
        let r = run(c, dl);
        lemma_drop_last_ord(ord);
        lemma_no_hits(c, dl);
        assert forall|o: Item, j: int|
            step(r, x).contains(o) && 0 <= j < ord.len() implies !(#[trigger] hit(ord[j], o)) by {
            lemma_step(r, x, o);
            if o == retarget(r, x, x) {
                lemma_retarget(r, x, x);
                assert(!x.local);
            } else {
                let e = choose|e: Item|
                    r.contains(e) && e.id != x.id && !hit(x, e) && o == retarget(r, x, e);
                lemma_retarget(r, x, e);
                if j == ord.len() - 1 {
                    if x.lucina {
                        assert(o == e);
                    } else {
                        lemma_hit_user(x, e, o);
                    }
                } else {
                    assert(ord[j] == dl[j]);
                    assert(!hit(dl[j], e));
                    if o != e {
                        assert(ord[j].lucina ==> ord[ord.len() - 1].lucina);
                        assert(!ord[j].lucina);
                        lemma_hit_user(ord[j], e, o);
                    }
                }
            }
        }
    }
}

/// Every incoming item is in the result, unchanged (it is not local, so nothing replaces or retargets it).
proof fn lemma_stays(c: Seq<Item>, ord: Seq<Item>)
    requires
        all_nonlocal(ord),
        unique_ids(ord),
    ensures
        forall|k: int| 0 <= k < ord.len() ==> run(c, ord).contains(#[trigger] ord[k]),
    decreases ord.len(),
{
    if ord.len() > 0 {
        let dl = ord.drop_last();
        let x = ord.last();
        let r = run(c, dl);
        lemma_drop_last_ord(ord);
        lemma_stays(c, dl);
        assert forall|k: int| 0 <= k < ord.len() implies step(r, x).contains(#[trigger] ord[k]) by {
            if k == ord.len() - 1 {
                lemma_retarget(r, x, x);
                lemma_step(r, x, x);
            } else {
                assert(ord[k] == dl[k]);
                assert(r.contains(ord[k]));
                assert(ord[k].id != x.id);
                assert(!ord[k].local);
                lemma_retarget(r, x, ord[k]);
                lemma_step(r, x, ord[k]);
            }
        }
    }
}

proof fn lemma_unique_ord(m: Seq<Item>, x: Item)
    requires
        unique_ids(m),
        m.contains(x),
    ensures
        forall|o: Item| m.contains(o) && o.id == x.id ==> o == x,
{
    assert forall|o: Item| m.contains(o) && o.id == x.id implies o == x by {
        lemma_unique_contains(m, o, x);
    }
}

/// Stepping an item that is already in `m` and replaces nothing changes nothing.
proof fn lemma_noop(m: Seq<Item>, x: Item)
    requires
        unique_ids(m),
        m.contains(x),
        forall|o: Item| m.contains(o) ==> !hit(x, o),
    ensures
        forall|o: Item| step(m, x).contains(o) <==> m.contains(o),
{
    assert forall|id: u64| !hit_id(m, x, id) by {
        if hit_id(m, x, id) {
            let e = choose|e: Item| m.contains(e) && hit(x, e) && e.id == id;
            assert(!hit(x, e));
        }
    }
    lemma_unique_ord(m, x);
    assert forall|o: Item| step(m, x).contains(o) <==> m.contains(o) by {
        lemma_step(m, x, o);
        lemma_retarget(m, x, x);
        if step(m, x).contains(o) {
            if o != retarget(m, x, x) {
                let e = choose|e: Item|
                    m.contains(e) && e.id != x.id && !hit(x, e) && o == retarget(m, x, e);
                assert(retarget(m, x, e) == e);
            }
        }
        if m.contains(o) {
            if o.id == x.id {
                assert(o == x);
                assert(retarget(m, x, x) == x);
            } else {
                assert(retarget(m, x, o) == o);
            }
        }
    }
}

/// Running the batch again on its own result changes nothing.
proof fn lemma_idem_run(r1: Seq<Item>, p: Seq<Item>)
    requires
        unique_ids(r1),
        forall|k: int| 0 <= k < p.len() ==> r1.contains(#[trigger] p[k]),
        forall|k: int, o: Item| 0 <= k < p.len() && r1.contains(o) ==> !(#[trigger] hit(p[k], o)),
    ensures
        forall|o: Item| run(r1, p).contains(o) <==> r1.contains(o),
    decreases p.len(),
{
    if p.len() > 0 {
        let dl = p.drop_last();
        let x = p.last();
        lemma_drop_last_ord(p);
        lemma_idem_run(r1, dl);
        let m = run(r1, dl);
        lemma_run_unique(r1, dl);
        assert(p[p.len() - 1] == x);
        assert(r1.contains(x));
        assert forall|o: Item| m.contains(o) implies !hit(x, o) by {
            assert(r1.contains(o));
        }
        lemma_noop(m, x);
    }
}

// ---------------------------------------------------------------- the batch order

proof fn lemma_part(s: Seq<Item>, l: bool)
    ensures
        forall|k: int|
            0 <= k < part(s, l).len() ==> (#[trigger] part(s, l)[k]).lucina == l && s.contains(
                part(s, l)[k],
            ),
        forall|e: Item| s.contains(e) && e.lucina == l ==> #[trigger] part(s, l).contains(e),
        unique_ids(s) ==> unique_ids(part(s, l)),
    decreases s.len(),
{
    if s.len() > 0 {
        let dl = s.drop_last();
        let e0 = s.last();
        let r = part(dl, l);
        lemma_part(dl, l);
        lemma_drop_last_props(s);
        assert forall|e: Item| s.contains(e) <==> (dl.contains(e) || e == e0) by {
            lemma_last_contains(s, e);
        }
        if e0.lucina == l {
            let p = r.push(e0);
            assert forall|k: int| 0 <= k < p.len() implies (#[trigger] p[k]).lucina == l && s.contains(p[k]) by {
                if k < r.len() {
                    assert(p[k] == r[k]);
                    assert(dl.contains(r[k]));
                } else {
                    assert(p[k] == e0);
                }
            }
            assert forall|e: Item| s.contains(e) && e.lucina == l implies #[trigger] p.contains(e) by {
                lemma_push_contains(r, e0, e);
            }
            if unique_ids(s) {
                assert forall|i: int, j: int| 0 <= i < j < p.len() implies (#[trigger] p[i]).id != (
                #[trigger] p[j]).id by {
                    if j == p.len() - 1 {
                        assert(p[j] == e0);
                        assert(p[i] == r[i]);
                        assert(dl.contains(r[i]));
                        let q = choose|q: int| 0 <= q < dl.len() && dl[q] == r[i];
                        assert(dl[q] == s[q]);
                    } else {
                        assert(p[i] == r[i]);
                        assert(p[j] == r[j]);
                    }
                }
            }
        } else {
            assert forall|k: int|
                0 <= k < r.len() implies (#[trigger] r[k]).lucina == l && s.contains(r[k]) by {
                assert(dl.contains(r[k]));
            }
        }
    }
}

proof fn lemma_ordered(inc: Seq<Item>)
    ensures
        phased(ordered(inc)),
        forall|o: Item| #[trigger] ordered(inc).contains(o) <==> inc.contains(o),
        unique_ids(inc) ==> unique_ids(ordered(inc)),
{
    let a = part(inc, false);
    let b = part(inc, true);
    lemma_part(inc, false);
    lemma_part(inc, true);
    let ord = a + b;
    assert forall|i: int, j: int| 0 <= i < j < ord.len() && (#[trigger] ord[i]).lucina implies (
    #[trigger] ord[j]).lucina by {
        if i < a.len() {
            assert(ord[i] == a[i]);
        } else {
            assert(j >= a.len());
            assert(ord[j] == b[j - a.len()]);
        }
    }
    assert forall|o: Item| ordered(inc).contains(o) <==> inc.contains(o) by {
        lemma_add_contains(a, b, o);
        if ordered(inc).contains(o) {
            if a.contains(o) {
                let k = choose|k: int| 0 <= k < a.len() && a[k] == o;
                assert(inc.contains(a[k]));
            } else {
                let k = choose|k: int| 0 <= k < b.len() && b[k] == o;
                assert(inc.contains(b[k]));
            }
        }
    }
    if unique_ids(inc) {
        assert forall|i: int, j: int| 0 <= i < j < ord.len() implies (#[trigger] ord[i]).id != (
        #[trigger] ord[j]).id by {
            if j < a.len() {
                assert(ord[i] == a[i]);
                assert(ord[j] == a[j]);
            } else if i >= a.len() {
                assert(ord[i] == b[i - a.len()]);
                assert(ord[j] == b[j - a.len()]);
            } else {
                assert(ord[i] == a[i]);
                assert(ord[j] == b[j - a.len()]);
                assert(inc.contains(a[i]));
                assert(inc.contains(b[j - a.len()]));
                if a[i].id == b[j - a.len()].id {
                    lemma_unique_contains(inc, a[i], b[j - a.len()]);
                }
            }
        }
    }
}

/// M3: merging the same batch again changes nothing, for a batch of distinct non-local items (a server
/// page, or the in-memory timeline that `hydrate` merges). A batch that carries a local item the same
/// batch's server item replaces is the counterexample: see the header.
proof fn lemma_merge_idempotent(c: Seq<Item>, inc: Seq<Item>)
    requires
        unique_ids(c),
        unique_ids(inc),
        all_nonlocal(inc),
    ensures
        merge(merge(c, inc), inc) == merge(c, inc),
{
    let ord = ordered(inc);
    lemma_ordered(inc);
    assert forall|k: int| 0 <= k < ord.len() implies !(#[trigger] ord[k]).local by {
        assert(ord.contains(ord[k]));
        assert(inc.contains(ord[k]));
        let q = choose|q: int| 0 <= q < inc.len() && inc[q] == ord[k];
    }
    let r1 = run(c, ord);
    lemma_run_unique(c, ord);
    lemma_no_hits(c, ord);
    lemma_stays(c, ord);
    lemma_idem_run(r1, ord);
    lemma_merge_props(c, inc);
    let out1 = merge(c, inc);
    lemma_run_ext(out1, r1, ord);
    lemma_merge_props(out1, inc);
    assert forall|o: Item| merge(out1, inc).contains(o) <==> out1.contains(o) by {
        assert(merge(out1, inc).contains(o) <==> run(out1, ord).contains(o));
        assert(out1.contains(o) <==> r1.contains(o));
    }
    lemma_sorted_unique(merge(out1, inc), out1);
}

// ---------------------------------------------------------------- the sync cursor loop

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Begin,
    Fetching,
    Applying,
    Finished,
    Aborted,
}

#[derive(Clone, Copy)]
pub enum Outcome {
    Started,
    Page { cursor: u64, more: bool },
    Applied,
    Gone,
    Failed,
}

#[derive(Clone, Copy)]
pub enum Action {
    Fetch { after: u64, limit: u32 },
    Apply { reset: bool },
    Done,
    Abort,
}

/// `sync::SyncPlan` with `u64` cursors (0 = none; the interning starts at 1).
#[derive(Clone, Copy)]
pub struct Plan {
    pub phase: Phase,
    pub had_cursor: bool,
    pub fresh: bool,
    pub cursor: u64,
    pub pages: u32,
    pub fetches: u32,
    pub pc: u64,
    pub more: bool,
}

pub open spec fn new_plan(cursor: u64) -> Plan {
    Plan {
        phase: Phase::Begin,
        had_cursor: cursor != 0,
        fresh: false,
        cursor: cursor,
        pages: 0,
        fetches: 0,
        pc: 0,
        more: false,
    }
}

pub open spec fn abort_spec(p: Plan) -> (Plan, Action) {
    (Plan { phase: Phase::Aborted, ..p }, Action::Abort)
}

pub open spec fn fetch_loop_spec(p: Plan) -> (Plan, Action) {
    (
        Plan {
            phase: Phase::Fetching,
            pages: (p.pages + 1) as u32,
            fetches: (p.fetches + 1) as u32,
            ..p
        },
        Action::Fetch { after: p.cursor, limit: 200 },
    )
}

pub open spec fn fetch_fresh_spec(p: Plan) -> (Plan, Action) {
    (
        Plan { phase: Phase::Fetching, fresh: true, fetches: (p.fetches + 1) as u32, ..p },
        Action::Fetch { after: 0, limit: 50 },
    )
}

/// `SyncPlan::next`.
pub open spec fn next_spec(p: Plan, o: Outcome) -> (Plan, Action) {
    if p.phase == Phase::Finished {
        (p, Action::Done)
    } else if p.phase == Phase::Aborted {
        abort_spec(p)
    } else {
        match o {
            Outcome::Failed => abort_spec(p),
            Outcome::Started => {
                if p.phase == Phase::Begin {
                    if p.had_cursor {
                        fetch_loop_spec(p)
                    } else {
                        (
                            Plan { phase: Phase::Fetching, fetches: (p.fetches + 1) as u32, ..p },
                            Action::Fetch { after: 0, limit: 50 },
                        )
                    }
                } else {
                    abort_spec(p)
                }
            },
            Outcome::Page { cursor, more } => {
                if p.phase == Phase::Fetching {
                    (
                        Plan { phase: Phase::Applying, pc: cursor, more: more, ..p },
                        Action::Apply { reset: p.fresh },
                    )
                } else {
                    abort_spec(p)
                }
            },
            Outcome::Gone => {
                if p.phase == Phase::Fetching || p.phase == Phase::Applying {
                    if p.had_cursor && !p.fresh {
                        fetch_fresh_spec(p)
                    } else {
                        abort_spec(p)
                    }
                } else {
                    abort_spec(p)
                }
            },
            Outcome::Applied => {
                if p.phase == Phase::Applying {
                    if p.fresh || !p.had_cursor || !p.more {
                        (Plan { phase: Phase::Finished, ..p }, Action::Done)
                    } else if p.pc == 0 || p.pc == p.cursor {
                        fetch_fresh_spec(p)
                    } else if p.pages >= 10 {
                        fetch_fresh_spec(Plan { cursor: p.pc, ..p })
                    } else {
                        fetch_loop_spec(Plan { cursor: p.pc, ..p })
                    }
                } else {
                    abort_spec(p)
                }
            },
        }
    }
}

/// The same function as the loop runs it.
pub fn next(p: Plan, o: Outcome) -> (r: (Plan, Action))
    requires
        p.pages <= 10,
        p.fetches <= 11,
    ensures
        r == next_spec(p, o),
{
    match p.phase {
        Phase::Finished => (p, Action::Done),
        Phase::Aborted => {
            let mut q = p;
            q.phase = Phase::Aborted;
            (q, Action::Abort)
        },
        _ => match o {
            Outcome::Failed => {
                let mut q = p;
                q.phase = Phase::Aborted;
                (q, Action::Abort)
            },
            Outcome::Started => {
                match p.phase {
                    Phase::Begin => {
                        let mut q = p;
                        q.phase = Phase::Fetching;
                        q.fetches = p.fetches + 1;
                        if p.had_cursor {
                            q.pages = p.pages + 1;
                            (q, Action::Fetch { after: p.cursor, limit: 200 })
                        } else {
                            (q, Action::Fetch { after: 0, limit: 50 })
                        }
                    },
                    _ => {
                        let mut q = p;
                        q.phase = Phase::Aborted;
                        (q, Action::Abort)
                    },
                }
            },
            Outcome::Page { cursor, more } => {
                match p.phase {
                    Phase::Fetching => {
                        let mut q = p;
                        q.phase = Phase::Applying;
                        q.pc = cursor;
                        q.more = more;
                        (q, Action::Apply { reset: p.fresh })
                    },
                    _ => {
                        let mut q = p;
                        q.phase = Phase::Aborted;
                        (q, Action::Abort)
                    },
                }
            },
            Outcome::Gone => {
                match p.phase {
                    Phase::Fetching | Phase::Applying => {
                        let mut q = p;
                        if p.had_cursor && !p.fresh {
                            q.phase = Phase::Fetching;
                            q.fresh = true;
                            q.fetches = p.fetches + 1;
                            (q, Action::Fetch { after: 0, limit: 50 })
                        } else {
                            q.phase = Phase::Aborted;
                            (q, Action::Abort)
                        }
                    },
                    _ => {
                        let mut q = p;
                        q.phase = Phase::Aborted;
                        (q, Action::Abort)
                    },
                }
            },
            Outcome::Applied => {
                match p.phase {
                    Phase::Applying => {
                        let mut q = p;
                        if p.fresh || !p.had_cursor || !p.more {
                            q.phase = Phase::Finished;
                            (q, Action::Done)
                        } else if p.pc == 0 || p.pc == p.cursor {
                            q.phase = Phase::Fetching;
                            q.fresh = true;
                            q.fetches = p.fetches + 1;
                            (q, Action::Fetch { after: 0, limit: 50 })
                        } else if p.pages >= 10 {
                            q.cursor = p.pc;
                            q.phase = Phase::Fetching;
                            q.fresh = true;
                            q.fetches = p.fetches + 1;
                            (q, Action::Fetch { after: 0, limit: 50 })
                        } else {
                            q.cursor = p.pc;
                            q.phase = Phase::Fetching;
                            q.pages = p.pages + 1;
                            q.fetches = p.fetches + 1;
                            (q, Action::Fetch { after: p.pc, limit: 200 })
                        }
                    },
                    _ => {
                        let mut q = p;
                        q.phase = Phase::Aborted;
                        (q, Action::Abort)
                    },
                }
            },
        },
    }
}

// ---------------------------------------------------------------- C3: the fetch budget

/// What every reachable plan satisfies.
pub open spec fn plan_inv(p: Plan) -> bool {
    p.pages <= 10 && p.fetches as int <= p.pages as int + (if p.fresh {
        1int
    } else {
        0int
    }) + (if p.had_cursor {
        0int
    } else {
        1int
    }) && (p.fresh ==> p.had_cursor) && (!p.had_cursor ==> p.pages == 0) && (p.phase
        == Phase::Begin ==> p.fetches == 0 && p.pages == 0 && !p.fresh) && (p.had_cursor
        ==> p.cursor != 0) && (!p.had_cursor ==> p.cursor == 0)
}

proof fn lemma_new_plan(cursor: u64)
    ensures
        plan_inv(new_plan(cursor)),
{
}

proof fn lemma_next_inv(p: Plan, o: Outcome)
    requires
        plan_inv(p),
    ensures
        plan_inv(next_spec(p, o).0),
        // C3: one fetch per Fetch action, none otherwise
        next_spec(p, o).1 is Fetch ==> next_spec(p, o).0.fetches == p.fetches + 1,
        !(next_spec(p, o).1 is Fetch) ==> next_spec(p, o).0.fetches == p.fetches,
{
}

/// C3: at most 11 fetches (10 cursor pages + the fresh window) in a run that starts with a cursor, 1 without.
proof fn lemma_fetch_budget(p: Plan)
    requires
        plan_inv(p),
    ensures
        p.fetches <= 11,
        !p.had_cursor ==> p.fetches <= 1,
{
}

// ---------------------------------------------------------------- C1, C2: the cursor never skips or goes back

/// The server's answer to `fetch(after)`: the next ids of the log after `after`, up to the page's last id `pc`;
/// `more` is whether the log goes on past `pc`. A page never moves the cursor back, and when it says there is
/// more, it moved forward.
pub open spec fn server_ok(log: Seq<u64>, after: u64, pc: u64, more: bool) -> bool {
    pc >= after && (more <==> exists|v: u64| #[trigger] log.contains(v) && v > pc) && (more ==> pc
        > after)
}

/// The log ids a page after `after` up to `pc` carries.
pub open spec fn page_ids(log: Seq<u64>, after: u64, pc: u64) -> spec_fn(u64) -> bool {
    |v: u64| log.contains(v) && after < v && v <= pc
}

/// The run is in the cursor loop (not the first cursor-less fetch, not the fresh window).
pub open spec fn cursor_mode(p: Plan) -> bool {
    p.had_cursor && !p.fresh
}

/// The loop's environment, as the proof sees it: the plan, the action it last returned, `hi` (the cursor of the
/// last page applied with `reset = false`, i.e. the stored cursor) and the log ids applied so far.
pub struct Sys {
    pub plan: Plan,
    pub last: Action,
    pub hi: u64,
    pub c0: u64,
    pub applied: spec_fn(u64) -> bool,
}

pub open spec fn init_sys(c0: u64) -> Sys {
    Sys { plan: new_plan(c0), last: Action::Abort, hi: c0, c0: c0, applied: |v: u64| false }
}

/// What a well-behaved server and client do: a page answers the cursor fetch as `server_ok` says.
pub open spec fn valid(log: Seq<u64>, s: Sys, o: Outcome) -> bool {
    match o {
        Outcome::Page { cursor, more } => cursor_mode(s.plan) ==> server_ok(
            log,
            s.plan.cursor,
            cursor,
            more,
        ),
        _ => true,
    }
}

pub open spec fn sys_step(log: Seq<u64>, s: Sys, o: Outcome) -> Sys {
    let r = next_spec(s.plan, o);
    let applying = s.plan.phase == Phase::Applying && o is Applied && cursor_mode(s.plan);
    Sys {
        plan: r.0,
        last: r.1,
        hi: if applying {
            s.plan.pc
        } else {
            s.hi
        },
        c0: s.c0,
        applied: if applying {
            |v: u64| (s.applied)(v) || (page_ids(log, s.plan.cursor, s.plan.pc))(v)
        } else {
            s.applied
        },
    }
}

/// C1's invariant: every log id in `(c0, hi]` was in an applied page.
pub open spec fn covers(log: Seq<u64>, s: Sys) -> bool {
    forall|v: u64| #[trigger] log.contains(v) && s.c0 < v && v <= s.hi ==> (s.applied)(v)
}

pub open spec fn sys_inv(log: Seq<u64>, s: Sys) -> bool {
    let p = s.plan;
    plan_inv(p) && (p.had_cursor <==> s.c0 != 0) && (p.had_cursor ==> s.hi >= s.c0 && covers(log, s)) && (cursor_mode(p)
        && p.phase != Phase::Finished && p.phase != Phase::Aborted ==> p.cursor == s.hi) && (
    cursor_mode(p) && p.phase == Phase::Applying ==> server_ok(log, p.cursor, p.pc, p.more)) && (
    p.phase == Phase::Fetching ==> match s.last {
        Action::Fetch { after, limit } => ((after == 0) == (p.fresh || !p.had_cursor)) && (after
            != 0 ==> after == p.cursor && limit == 200) && (after == 0 ==> limit == 50),
        _ => false,
    }) && (p.phase == Phase::Applying ==> match s.last {
        Action::Apply { reset } => reset == p.fresh,
        _ => false,
    })
}

proof fn lemma_init(log: Seq<u64>, c0: u64)
    requires
        c0 != 0,
    ensures
        sys_inv(log, init_sys(c0)),
{
    assert forall|v: u64| #[trigger] log.contains(v) && c0 < v && v <= c0 implies (init_sys(c0).applied)(v) by {}
}

/// C1 + C2 + C3 for one step of the loop, for any outcome a conforming server and client can produce.
proof fn lemma_sys_step(log: Seq<u64>, s: Sys, o: Outcome)
    requires
        sys_inv(log, s),
        valid(log, s, o),
    ensures
        sys_inv(log, sys_step(log, s, o)),
        // C2: the stored cursor never moves backwards
        sys_step(log, s, o).hi >= s.hi,
        // a reset is only ever applied for the fresh window, and the fresh window is always applied as a reset
        (s.plan.phase == Phase::Fetching && o is Page) ==> (match (s.last, sys_step(log, s, o).last) {
            (Action::Fetch { after, limit }, Action::Apply { reset }) => reset == (after == 0
                && s.plan.had_cursor),
            _ => false,
        }),
{
    lemma_next_inv(s.plan, o);
    let t = sys_step(log, s, o);
    let p = s.plan;
    let applying = p.phase == Phase::Applying && o is Applied && cursor_mode(p);
    if applying {
        // the page after `cursor` up to `pc` is applied, and `pc >= cursor == hi`
        assert(p.cursor == s.hi);
        assert forall|v: u64| #[trigger] log.contains(v) && s.c0 < v && v <= p.pc implies (t.applied)(v) by {
            if v > s.hi {
                assert((page_ids(log, p.cursor, p.pc))(v));
            } else {
                assert((s.applied)(v));
            }
        }
    }
}


// ---------------------------------------------------------------- whole runs

pub open spec fn sys_run(log: Seq<u64>, s: Sys, outs: Seq<Outcome>) -> Sys
    decreases outs.len(),
{
    if outs.len() == 0 {
        s
    } else {
        sys_run(log, sys_step(log, s, outs[0]), outs.drop_first())
    }
}

pub open spec fn valid_run(log: Seq<u64>, s: Sys, outs: Seq<Outcome>) -> bool
    decreases outs.len(),
{
    if outs.len() == 0 {
        true
    } else {
        valid(log, s, outs[0]) && valid_run(log, sys_step(log, s, outs[0]), outs.drop_first())
    }
}

proof fn lemma_sys_run(log: Seq<u64>, s: Sys, outs: Seq<Outcome>)
    requires
        sys_inv(log, s),
        valid_run(log, s, outs),
    ensures
        sys_inv(log, sys_run(log, s, outs)),
        sys_run(log, s, outs).hi >= s.hi,
    decreases outs.len(),
{
    if outs.len() > 0 {
        lemma_sys_step(log, s, outs[0]);
        lemma_sys_run(log, sys_step(log, s, outs[0]), outs.drop_first());
    }
}

proof fn lemma_run_c0(log: Seq<u64>, s: Sys, outs: Seq<Outcome>)
    ensures
        sys_run(log, s, outs).c0 == s.c0,
    decreases outs.len(),
{
    if outs.len() > 0 {
        lemma_run_c0(log, sys_step(log, s, outs[0]), outs.drop_first());
    }
}

/// C1, C2, C3 for a whole run that starts with the stored cursor `c0`, whatever the server (as long as its
/// pages are `server_ok`), the network (410, other errors) and the apply step do.
proof fn theorem_cursor_run(log: Seq<u64>, c0: u64, outs: Seq<Outcome>)
    requires
        c0 != 0,
        valid_run(log, init_sys(c0), outs),
    ensures
        ({
            let t = sys_run(log, init_sys(c0), outs);
            // C3: at most 10 cursor pages + the fresh window
            &&& t.plan.fetches <= 11
            // C2: the stored cursor never went backwards
            &&& t.hi >= c0
            // C1: a run that ended without a reset applied every log id in (c0, final cursor]
            &&& (t.plan.phase == Phase::Finished && !t.plan.fresh ==> forall|v: u64|
                #[trigger] log.contains(v) && c0 < v && v <= t.hi ==> (t.applied)(v))
        }),
{
    lemma_init(log, c0);
    lemma_sys_run(log, init_sys(c0), outs);
    lemma_run_c0(log, init_sys(c0), outs);
    lemma_fetch_budget(sys_run(log, init_sys(c0), outs).plan);
}

// ---------------------------------------------------------------- the server model

/// The server's log: strictly increasing ids.
pub open spec fn increasing(log: Seq<u64>) -> bool {
    forall|i: int, j: int| 0 <= i < j < log.len() ==> (#[trigger] log[i]) < (#[trigger] log[j])
}

/// A page after `after`: the next `limit` ids of the log.
pub open spec fn next_ids(log: Seq<u64>, after: u64, limit: nat) -> Seq<u64>
    decreases log.len(),
{
    if log.len() == 0 || limit == 0 {
        Seq::empty()
    } else if log[0] > after {
        seq![log[0]] + next_ids(log.drop_first(), after, (limit - 1) as nat)
    } else {
        next_ids(log.drop_first(), after, limit)
    }
}

/// The page's cursor: its last id, or the cursor asked after when the page is empty.
pub open spec fn page_cursor(items: Seq<u64>, after: u64) -> u64 {
    if items.len() == 0 {
        after
    } else {
        items.last()
    }
}

/// The page is the log's ids in `(after, pc]`, and `pc` never goes back.
proof fn lemma_next_ids(log: Seq<u64>, after: u64, limit: nat)
    requires
        increasing(log),
    ensures
        ({
            let items = next_ids(log, after, limit);
            let pc = page_cursor(items, after);
            &&& pc >= after
            &&& items.len() <= limit
            &&& forall|v: u64| #[trigger] items.contains(v) ==> log.contains(v) && after < v && v <= pc
            &&& forall|v: u64| #[trigger] log.contains(v) && after < v && v <= pc ==> items.contains(v)
            &&& (limit > 0 && (exists|v: u64| #[trigger] log.contains(v) && v > after) ==> items.len() > 0)
        }),
    decreases log.len(),
{
    let items = next_ids(log, after, limit);
    if log.len() == 0 || limit == 0 {
        assert(items =~= Seq::<u64>::empty());
        assert forall|v: u64| #[trigger] log.contains(v) && after < v && v <= page_cursor(items, after) implies items.contains(v) by {}
    } else {
        let rest = log.drop_first();
        assert forall|i: int, j: int| 0 <= i < j < rest.len() implies (#[trigger] rest[i]) < (#[trigger] rest[j]) by {
            assert(rest[i] == log[i + 1]);
            assert(rest[j] == log[j + 1]);
        }
        assert forall|k: int| 0 <= k < rest.len() implies log[0] < (#[trigger] rest[k]) by {
            assert(rest[k] == log[k + 1]);
        }
        if log[0] > after {
            let l = (limit - 1) as nat;
            lemma_next_ids(rest, after, l);
            let it = next_ids(rest, after, l);
            let pc1 = page_cursor(it, after);
            assert(items =~= seq![log[0]] + it);
            assert(items[0] == log[0]);
            let pc = page_cursor(items, after);
            if it.len() == 0 {
                assert(items =~= seq![log[0]]);
                assert(pc == log[0]);
            } else {
                assert(items.last() == it.last());
                assert(pc == pc1);
                // it.last() is in rest, so above log[0]
                assert(it.contains(it.last()));
                assert(rest.contains(it.last()));
                let q = choose|q: int| 0 <= q < rest.len() && rest[q] == it.last();
                assert(log[0] < rest[q]);
            }
            assert forall|v: u64| #[trigger] items.contains(v) implies log.contains(v) && after < v && v <= pc by {
                lemma_add_contains(seq![log[0]], it, v);
                lemma_singleton_contains(log[0], v);
                if it.contains(v) {
                    assert(rest.contains(v));
                    lemma_first_contains(log, v);
                }
                if v == log[0] {
                    assert(log.contains(log[0]));
                }
            }
            assert forall|v: u64| #[trigger] log.contains(v) && after < v && v <= pc implies items.contains(v) by {
                lemma_add_contains(seq![log[0]], it, v);
                lemma_singleton_contains(log[0], v);
                lemma_first_contains(log, v);
                if v != log[0] {
                    assert(rest.contains(v));
                    let q = choose|q: int| 0 <= q < rest.len() && rest[q] == v;
                    assert(log[0] < rest[q]);
                    if it.len() > 0 {
                        assert(v <= pc1);
                    }
                }
            }
        } else {
            lemma_next_ids(rest, after, limit);
            assert(items == next_ids(rest, after, limit));
            assert forall|v: u64| #[trigger] items.contains(v) implies log.contains(v) && after < v && v <= page_cursor(items, after) by {
                assert(rest.contains(v));
                lemma_first_contains(log, v);
            }
            assert forall|v: u64| #[trigger] log.contains(v) && after < v && v <= page_cursor(items, after) implies items.contains(v) by {
                lemma_first_contains(log, v);
                assert(v != log[0]);
            }
            if limit > 0 && (exists|v: u64| log.contains(v) && v > after) {
                let v = choose|v: u64| log.contains(v) && v > after;
                lemma_first_contains(log, v);
                assert(rest.contains(v));
            }
        }
    }
}

/// The concrete server (a page = the next `limit` ids after the cursor, `more` = the log goes on past it)
/// is `server_ok`, and the ids it carries are exactly `page_ids`.
proof fn lemma_server_model(log: Seq<u64>, after: u64, limit: nat)
    requires
        increasing(log),
        limit > 0,
    ensures
        ({
            let items = next_ids(log, after, limit);
            let pc = page_cursor(items, after);
            let more = exists|v: u64| #[trigger] log.contains(v) && v > pc;
            &&& server_ok(log, after, pc, more)
            &&& forall|v: u64| #[trigger] items.contains(v) <==> (page_ids(log, after, pc))(v)
        }),
{
    lemma_next_ids(log, after, limit);
    let items = next_ids(log, after, limit);
    let pc = page_cursor(items, after);
    if exists|v: u64| log.contains(v) && v > pc {
        let v = choose|v: u64| log.contains(v) && v > pc;
        assert(v > after);
        assert(items.len() > 0);
        assert(items.contains(items.last()));
        assert(pc == items.last());
        assert(pc > after);
    }
}

} // verus!
fn main() {}
