// MCP scopes and attempt-limiter kernel (secretary-mcp: what a token may do, how often a
// login / registration / token request may be tried).
//
// Production (branch lucina-rs, crates/secretary-mcp/src/):
//   Mask (bit 1 read, 2 chat, 4 write, 8 computer) <-> scope::Scopes (sorted, deduplicated Vec<Scope>)
//   parse_lenient / parse_list         <-> Scopes::parse_lenient / Scopes::parse_list over
//                                          the names split on ' ' and ',' (a name is a
//                                          token id: 1 read, 2 chat, 3 write, 4 computer,
//                                          0 unknown)
//   intersect / subset / has           <-> Scopes::intersect / is_subset_of / contains
//   authorize_scopes                   <-> oauth.rs `authorize`: requested = parse_lenient,
//                                          absent or empty -> all
//   consent_scopes                     <-> oauth.rs consent: ticked boxes ∩ requested
//   refresh_scopes                     <-> oauth.rs refresh: parse_lenient(scope) must be a
//                                          non-empty subset of the grant, else invalid_scope;
//                                          no scope -> the grant's
//   check                              <-> ratelimit::Limiter::check for one key: drop hits
//                                          with now - t >= window, refuse when max remain,
//                                          else record now. Times are abstract ticks; the
//                                          code's Instant only grows, as `now` does here.
// Differential test: crates/secretary-mcp/tests/verus_scopes.rs.
//
// Proven:
//   M1 unknown_names_add_nothing: parse_lenient ignores unknown names; parse_list refuses
//      them; both only ever yield read/chat/write/computer.
//   M2 consent_within_request: the scopes the owner grants are within what the client
//      asked for and within the ticked boxes; an empty pick is refused.
//   M3 refresh_never_widens: a refreshed token's scopes are within its grant, so along any
//      chain of refreshes the scopes only narrow (refresh_chain).
//   M4 subset_order: subset is reflexive, transitive, antisymmetric; intersect is the
//      greatest lower bound.
//   M5 window_bound (Limiter, inductive over every check): for each key, the attempts
//      accepted in any window (now - window, now] ending at a check are at most `max`;
//      the stored hits are exactly the accepted attempts still inside the window (a
//      suffix of all accepted attempts).
// Not modelled: the `hits.len() > 10_000` sweep (it drops only keys whose newest hit has
// expired, whose hits the next check of that key would drop anyway), and the Retry-After
// value (whole seconds, truncated: up to one second early, see docs/verification-gaps.md).
//
// No Stateright model: scopes are pure set algebra; the limiter is one mutex-guarded map
// whose per-key step is the relation proven here.
use vstd::prelude::*;

verus! {

pub const READ: u8 = 1;
pub const CHAT: u8 = 2;
pub const WRITE: u8 = 4;
pub const COMPUTER: u8 = 8;
pub const ALL: u8 = 15;

// ---------------------------------------------------------------- scopes

pub open spec fn wf(m: u8) -> bool {
    m <= 15
}

pub open spec fn bit(name: u8) -> u8 {
    if name == 1 {
        READ
    } else if name == 2 {
        CHAT
    } else if name == 3 {
        WRITE
    } else if name == 4 {
        COMPUTER
    } else {
        0
    }
}

pub open spec fn has(m: u8, s: u8) -> bool {
    m & s == s
}

pub open spec fn subset(a: u8, b: u8) -> bool {
    a & b == a
}

pub open spec fn lenient(names: Seq<u8>) -> u8
    decreases names.len(),
{
    if names.len() == 0 {
        0
    } else {
        lenient(names.drop_last()) | bit(names.last())
    }
}

pub open spec fn all_known(names: Seq<u8>) -> bool {
    forall|i: int| 0 <= i < names.len() ==> 1 <= #[trigger] names[i] <= 4
}

proof fn lemma_lenient_wf(names: Seq<u8>)
    ensures
        wf(lenient(names)),
    decreases names.len(),
{
    if names.len() > 0 {
        lemma_lenient_wf(names.drop_last());
        let a = lenient(names.drop_last());
        let b = bit(names.last());
        assert(a <= 15 && b <= 15 ==> a | b <= 15) by (bit_vector);
    }
}

/// M1: a scope is in the result exactly when one of its names is in the list.
pub proof fn unknown_names_add_nothing(names: Seq<u8>, s: u8)
    requires
        s == READ || s == CHAT || s == WRITE || s == COMPUTER,
    ensures
        has(lenient(names), s) <==> exists|i: int| 0 <= i < names.len() && bit(#[trigger] names[i])
            == s,
        wf(lenient(names)),
    decreases names.len(),
{
    lemma_lenient_wf(names);
    if names.len() == 0 {
        assert(0u8 & s != s) by (bit_vector)
            requires
                s == 1 || s == 2 || s == 4 || s == 8,
        ;
    } else {
        let p = names.drop_last();
        unknown_names_add_nothing(p, s);
        let a = lenient(p);
        let b = bit(names.last());
        assert((a | b) & s == s <==> (a & s == s || b == s)) by (bit_vector)
            requires
                s == 1 || s == 2 || s == 4 || s == 8,
                b == 0 || b == 1 || b == 2 || b == 4 || b == 8,
        ;
        if exists|i: int| 0 <= i < p.len() && bit(#[trigger] p[i]) == s {
            let i = choose|i: int| 0 <= i < p.len() && bit(#[trigger] p[i]) == s;
            assert(names[i] == p[i]);
        }
        if exists|i: int| 0 <= i < names.len() && bit(#[trigger] names[i]) == s {
            let i = choose|i: int| 0 <= i < names.len() && bit(#[trigger] names[i]) == s;
            if i < p.len() {
                assert(p[i] == names[i]);
            }
        }
    }
}

/// `authorize`: parse_lenient(scope), absent or empty -> all.
pub open spec fn authorize_scopes(present: bool, names: Seq<u8>) -> u8 {
    if !present || lenient(names) == 0 {
        ALL
    } else {
        lenient(names)
    }
}

/// consent: the ticked boxes intersected with the request (`None`: "pick at least one").
pub open spec fn consent_scopes(ticked: u8, requested: u8) -> Option<u8> {
    if ticked & requested == 0 {
        None
    } else {
        Some(ticked & requested)
    }
}

/// refresh: `None` = invalid_scope.
pub open spec fn refresh_scopes(present: bool, names: Seq<u8>, grant: u8) -> Option<u8> {
    if !present {
        Some(grant)
    } else if lenient(names) == 0 || !subset(lenient(names), grant) {
        None
    } else {
        Some(lenient(names))
    }
}

/// M2.
pub proof fn consent_within_request(ticked: u8, requested: u8)
    ensures
        consent_scopes(ticked, requested) matches Some(g) ==> subset(g, requested) && subset(
            g,
            ticked,
        ) && g != 0,
{
    let g = ticked & requested;
    assert(g & requested == g && g & ticked == g) by (bit_vector)
        requires
            g == ticked & requested,
    ;
}

/// M3, one step.
pub proof fn refresh_never_widens(present: bool, names: Seq<u8>, grant: u8)
    ensures
        refresh_scopes(present, names, grant) matches Some(g) ==> subset(g, grant),
{
    assert(grant & grant == grant) by (bit_vector);
}

/// M4.
pub proof fn subset_order(a: u8, b: u8, c: u8)
    ensures
        subset(a, a),
        subset(a, b) && subset(b, c) ==> subset(a, c),
        subset(a, b) && subset(b, a) ==> a == b,
        subset(a & b, a) && subset(a & b, b),
        subset(c, a) && subset(c, b) ==> subset(c, a & b),
{
    assert(a & a == a) by (bit_vector);
    assert((a & b == a && b & c == b) ==> a & c == a) by (bit_vector);
    assert((a & b == a && b & a == b) ==> a == b) by (bit_vector);
    assert((a & b) & a == a & b && (a & b) & b == a & b) by (bit_vector);
    assert((c & a == c && c & b == c) ==> c & (a & b) == c) by (bit_vector);
}

/// M3, along a chain: `refreshes[i]` is the scope request of the i-th refresh (`None`:
/// absent). The scopes after every successful refresh stay within the first grant.
pub open spec fn chain(grant: u8, refreshes: Seq<Option<Seq<u8>>>) -> u8
    decreases refreshes.len(),
{
    if refreshes.len() == 0 {
        grant
    } else {
        let g = chain(grant, refreshes.drop_last());
        let r = match refreshes.last() {
            None => refresh_scopes(false, Seq::empty(), g),
            Some(n) => refresh_scopes(true, n, g),
        };
        match r {
            Some(h) => h,
            None => g,  // the refresh failed: the client keeps its current token

        }
    }
}

pub proof fn refresh_chain(grant: u8, refreshes: Seq<Option<Seq<u8>>>)
    ensures
        subset(chain(grant, refreshes), grant),
    decreases refreshes.len(),
{
    if refreshes.len() == 0 {
        subset_order(grant, grant, grant);
    } else {
        let g = chain(grant, refreshes.drop_last());
        refresh_chain(grant, refreshes.drop_last());
        match refreshes.last() {
            None => refresh_never_widens(false, Seq::empty(), g),
            Some(n) => refresh_never_widens(true, n, g),
        }
        let h = chain(grant, refreshes);
        subset_order(h, g, grant);
    }
}

// exec copies of the code's operations

pub fn bit_exec(name: u8) -> (b: u8)
    ensures
        b == bit(name),
{
    if name == 1 {
        READ
    } else if name == 2 {
        CHAT
    } else if name == 3 {
        WRITE
    } else if name == 4 {
        COMPUTER
    } else {
        0
    }
}

pub fn parse_lenient(names: &Vec<u8>) -> (m: u8)
    ensures
        m == lenient(names@),
{
    let mut m: u8 = 0;
    let mut i: usize = 0;
    while i < names.len()
        invariant
            i <= names.len(),
            m == lenient(names@.subrange(0, i as int)),
        decreases names.len() - i,
    {
        proof {
            assert(names@.subrange(0, i as int + 1).drop_last() =~= names@.subrange(0, i as int));
        }
        m = m | bit_exec(names[i]);
        i += 1;
    }
    assert(names@.subrange(0, names.len() as int) =~= names@);
    m
}

/// `parse_list`: `None` on any unknown name.
pub fn parse_list(names: &Vec<u8>) -> (r: Option<u8>)
    ensures
        r is Some <==> all_known(names@),
        r matches Some(m) ==> m == lenient(names@),
{
    let mut i: usize = 0;
    while i < names.len()
        invariant
            i <= names.len(),
            forall|j: int| 0 <= j < i ==> 1 <= #[trigger] names@[j] <= 4,
        decreases names.len() - i,
    {
        if names[i] < 1 || names[i] > 4 {
            return None;
        }
        i += 1;
    }
    Some(parse_lenient(names))
}

pub fn intersect(a: u8, b: u8) -> (r: u8)
    ensures
        r == a & b,
{
    a & b
}

pub fn is_subset_of(a: u8, b: u8) -> (r: bool)
    ensures
        r == subset(a, b),
{
    a & b == a
}

// ---------------------------------------------------------------- the limiter

pub open spec fn nondecreasing(s: Seq<u64>) -> bool {
    forall|i: int, j: int| 0 <= i <= j < s.len() ==> s[i] <= s[j]
}

/// `now.duration_since(t) < window`.
pub open spec fn live(t: u64, now: u64, window: u64) -> bool {
    now - t < window
}

/// The per-key state: the hits are the suffix acc[k..] of every accepted attempt, and
/// the attempts before k have expired by `last` (the latest check).
pub open spec fn lim_inv(v: Seq<u64>, acc: Seq<u64>, k: int, last: u64, window: u64, max: nat) -> bool {
    &&& 0 <= k <= acc.len()
    &&& v =~= acc.subrange(k, acc.len() as int)
    &&& nondecreasing(acc)
    &&& forall|i: int| 0 <= i < acc.len() ==> #[trigger] acc[i] <= last
    &&& forall|i: int| 0 <= i < k ==> !live(#[trigger] acc[i], last, window)
    &&& v.len() <= max || max == 0 && v.len() == 0

}

/// The attempts accepted within (now - window, now].
pub open spec fn recent(acc: Seq<u64>, now: u64, window: u64) -> nat
    decreases acc.len(),
{
    if acc.len() == 0 {
        0
    } else {
        recent(acc.drop_last(), now, window) + if acc.last() <= now && live(
            acc.last(),
            now,
            window,
        ) {
            1nat
        } else {
            0nat
        }
    }
}

proof fn lemma_recent_le(acc: Seq<u64>, k: int, now: u64, window: u64)
    requires
        0 <= k <= acc.len(),
        forall|i: int| 0 <= i < k ==> !live(#[trigger] acc[i], now, window) || acc[i] > now,
    ensures
        recent(acc, now, window) <= acc.len() - k,
    decreases acc.len(),
{
    if acc.len() > k {
        lemma_recent_le(acc.drop_last(), k, now, window);
    } else if acc.len() > 0 {
        assert(acc.len() == k);
        assert forall|i: int| 0 <= i < acc.drop_last().len() implies !live(
            #[trigger] acc.drop_last()[i],
            now,
            window,
        ) || acc.drop_last()[i] > now by {
            assert(acc.drop_last()[i] == acc[i]);
        }
        lemma_recent_le(acc.drop_last(), k - 1, now, window);
        assert(!live(acc[k - 1], now, window) || acc[k - 1] > now);
    }
}

/// `Limiter::check` for one key. Returns (accepted, new hits, new k); `acc` grows by
/// `now` when accepted.
pub fn check(
    v: &Vec<u64>,
    now: u64,
    window: u64,
    max: u64,
    Ghost(acc): Ghost<Seq<u64>>,
    Ghost(k): Ghost<int>,
    Ghost(last): Ghost<u64>,
) -> (r: (bool, Vec<u64>, Ghost<int>))
    requires
        lim_inv(v@, acc, k, last, window, max as nat),
        last <= now,
    ensures
        ({
            let acc2 = if r.0 {
                acc.push(now)
            } else {
                acc
            };
            &&& lim_inv(r.1@, acc2, r.2@, now, window, max as nat)
            &&& recent(acc2, now, window) <= max
        }),
{
    // v.retain(|t| now.duration_since(*t) < window)
    let mut kept: Vec<u64> = Vec::new();
    let mut i: usize = 0;
    let ghost mut k2: int = k;
    while i < v.len()
        invariant
            lim_inv(v@, acc, k, last, window, max as nat),
            last <= now,
            i <= v.len(),
            k <= k2 <= k + i,
            kept@ =~= acc.subrange(k2, k + i),
            forall|j: int| 0 <= j < k2 ==> !live(#[trigger] acc[j], now, window),
            kept@.len() == 0 ==> k2 == k + i,
            forall|j: int| 0 <= j < kept@.len() ==> live(#[trigger] kept@[j], now, window),
        decreases v.len() - i,
    {
        let t = v[i];
        proof {
            assert(t == acc[k + i]);
        }
        if now - t < window {
            kept.push(t);
        } else {
            proof {
                // Later hits are not older; an expired hit after a live one cannot exist.
                if kept@.len() > 0 {
                    let t0 = kept@[0];
                    assert(t0 == acc[k2]);
                    assert(acc[k2] <= acc[k + i]);
                    assert(live(t0, now, window));
                    assert(acc[k + i] <= last);
                }
                assert(kept@.len() == 0);
                k2 = k + i + 1;
                assert forall|j: int| 0 <= j < k2 implies !live(#[trigger] acc[j], now, window) by {
                    if j >= k && j < k + i {
                        // dropped earlier in this loop (kept was empty then)
                    }
                }
            }
        }
        i += 1;
    }
    proof {
        assert(kept@ =~= acc.subrange(k2, acc.len() as int));
        assert(forall|j: int| 0 <= j < k2 ==> !live(#[trigger] acc[j], now, window));
    }
    if kept.len() as u64 >= max {
        proof {
            lemma_recent_le(acc, k2, now, window);
        }
        return (false, kept, Ghost(k2));
    }
    kept.push(now);
    proof {
        let acc2 = acc.push(now);
        assert(kept@ =~= acc2.subrange(k2, acc2.len() as int));
        assert forall|j: int| 0 <= j < k2 implies !live(#[trigger] acc2[j], now, window) by {
            assert(acc2[j] == acc[j]);
        }
        assert forall|a: int, b: int| 0 <= a <= b < acc2.len() implies acc2[a] <= acc2[b] by {
            if b < acc.len() {
            } else {
                if a < acc.len() {
                    assert(acc[a] <= last);
                }
            }
        }
        lemma_recent_le(acc2, k2, now, window);
    }
    (true, kept, Ghost(k2))
}

/// Not vacuous: with max 2, a third attempt in the window is refused.
pub proof fn witness_third_refused() {
    let acc = seq![10u64, 11];
    assert(recent(acc, 12, 60) == 2) by {
        reveal_with_fuel(recent, 3);
    }
}

} // verus!

fn main() {}
