// Which files the secret keeper may read: the pure check on a path.
//
// Production: crates/lucina-secrets/src/source_path.rs `allowed`, `clean`, `key_refused`.
// Differential test with planted bugs: crates/lucina-secrets/tests/verus_source_path.rs.
//
// Modelled: the path and all lists as bytes; ASCII lowercasing for the name lists.
// Not modelled: the filesystem. Symlinks are resolved and the result checked again by the caller,
// which then opens it with RESOLVE_NO_SYMLINKS (lucina-server secret_keep/files.rs), tested there.
//
// Proven:
//   P1 an allowed path is clean: absolute, no NUL, no empty, `.` or `..` component.
//   P2 an allowed path stands below a root at a component boundary, with a folder between the root
//      and the file; `/a/bc/d/e` is not below root `/a/b`.
//   P3 an allowed path is neither a denied tree nor below one, at a component boundary.
//   P4 an allowed path has no denied name as a component, in any ASCII case; `.SSH` is `.ssh`.
//   P5 an allowed path has no component starting with a denied start, and no denied substring.
//   X  allowed_exec equals the spec on every input, so these hold for the code, and it refuses for these reasons only.
//   K1 key_refused (spec only): a key holding a refused term, in any ASCII case and with `-` for `_`, is refused.
//      The Rust is tied to it by the differential test.
//   Non-vacuity: witness_allowed; witness_dotdot, witness_sibling and witness_case are refused.
use vstd::prelude::*;

verus! {

pub const SLASH: u8 = 47;
pub const DOT: u8 = 46;
pub const DASH: u8 = 45;
pub const UNDERSCORE: u8 = 95;

pub open spec fn low(c: u8) -> u8 {
    if 65 <= c <= 90 { (c + 32) as u8 } else { c }
}

pub open spec fn lower_all(p: Seq<u8>) -> Seq<u8> {
    Seq::new(p.len(), |i: int| low(p[i]))
}

pub open spec fn at(h: Seq<u8>, t: Seq<u8>, i: int) -> bool {
    0 <= i && i + t.len() <= h.len() && h.subrange(i, i + t.len()) =~= t
}

// ------------------------------------------------------------------ clean

pub open spec fn slash_ok(p: Seq<u8>, i: int) -> bool {
    let n = p.len() as int;
    &&& i + 1 < n
    &&& p[i + 1] != SLASH
    &&& !(p[i + 1] == DOT && (i + 2 == n || p[i + 2] == SLASH))
    &&& !(p[i + 1] == DOT && i + 2 < n && p[i + 2] == DOT && (i + 3 == n || p[i + 3] == SLASH))
}

pub open spec fn clean(p: Seq<u8>) -> bool {
    &&& p.len() > 0
    &&& p[0] == SLASH
    &&& forall|i: int| 0 <= i < p.len() ==> #[trigger] p[i] != 0
    &&& forall|i: int| 0 <= i < p.len() && #[trigger] p[i] == SLASH ==> slash_ok(p, i)
}

// ------------------------------------------------------------------ roots and denied trees

pub open spec fn under(p: Seq<u8>, d: Seq<u8>) -> bool {
    &&& d.len() > 0
    &&& d.last() != SLASH
    &&& p.len() > d.len()
    &&& at(p, d, 0)
    &&& p[d.len() as int] == SLASH
}

pub open spec fn deep(p: Seq<u8>, root: Seq<u8>) -> bool {
    &&& under(p, root)
    &&& exists|j: int| root.len() + 1 <= j < p.len() && #[trigger] p[j] == SLASH
}

// ------------------------------------------------------------------ names

pub open spec fn component_at(lp: Seq<u8>, c: Seq<u8>, i: int) -> bool {
    &&& 0 <= i < lp.len()
    &&& lp[i] == SLASH
    &&& at(lp, c, i + 1)
    &&& (i + 1 + c.len() == lp.len() || lp[i + 1 + c.len()] == SLASH)
}

pub open spec fn name_start_at(lp: Seq<u8>, c: Seq<u8>, i: int) -> bool {
    &&& 0 <= i < lp.len()
    &&& lp[i] == SLASH
    &&& at(lp, c, i + 1)
}

pub open spec fn component_hit(lp: Seq<u8>, c: Seq<u8>) -> bool {
    exists|i: int| component_at(lp, c, i)
}

pub open spec fn name_start_hit(lp: Seq<u8>, c: Seq<u8>) -> bool {
    exists|i: int| name_start_at(lp, c, i)
}

pub open spec fn contains(lp: Seq<u8>, c: Seq<u8>) -> bool {
    exists|i: int| at(lp, c, i)
}

// ------------------------------------------------------------------ the rule

pub open spec fn any_root(p: Seq<u8>, roots: Seq<Vec<u8>>) -> bool {
    exists|k: int| 0 <= k < roots.len() && deep(p, #[trigger] roots[k]@)
}

pub open spec fn denied_tree(p: Seq<u8>, ds: Seq<Vec<u8>>) -> bool {
    exists|k: int| 0 <= k < ds.len() && (p =~= #[trigger] ds[k]@ || under(p, ds[k]@))
}

pub open spec fn any_component(lp: Seq<u8>, cs: Seq<Vec<u8>>) -> bool {
    exists|k: int| 0 <= k < cs.len() && component_hit(lp, #[trigger] cs[k]@)
}

pub open spec fn any_start(lp: Seq<u8>, cs: Seq<Vec<u8>>) -> bool {
    exists|k: int| 0 <= k < cs.len() && name_start_hit(lp, #[trigger] cs[k]@)
}

pub open spec fn any_contains(lp: Seq<u8>, cs: Seq<Vec<u8>>) -> bool {
    exists|k: int| 0 <= k < cs.len() && contains(lp, #[trigger] cs[k]@)
}

pub open spec fn allowed(
    p: Seq<u8>,
    roots: Seq<Vec<u8>>,
    prefixes: Seq<Vec<u8>>,
    components: Seq<Vec<u8>>,
    starts: Seq<Vec<u8>>,
    substrings: Seq<Vec<u8>>,
) -> bool {
    let lp = lower_all(p);
    &&& clean(p)
    &&& any_root(p, roots)
    &&& !denied_tree(p, prefixes)
    &&& !any_component(lp, components)
    &&& !any_start(lp, starts)
    &&& !any_contains(lp, substrings)
}

// ------------------------------------------------------------------ what that means

/// P1: no component of an allowed path is `..`, `.` or empty.
pub proof fn p1_clean(p: Seq<u8>, i: int)
    requires
        clean(p),
        0 <= i < p.len(),
        p[i] == SLASH,
    ensures
        i + 1 < p.len(),
        p[i + 1] != SLASH,
        !(p[i + 1] == DOT && (i + 2 == p.len() || p[i + 2] == SLASH)),
        !(p[i + 1] == DOT && i + 2 < p.len() && p[i + 2] == DOT && (i + 3 == p.len() || p[i + 3] == SLASH)),
{
    assert(slash_ok(p, i));
}

/// P2: the root's bytes, then a slash, then a folder and a file.
pub proof fn p2_below_a_root(p: Seq<u8>, root: Seq<u8>)
    requires
        deep(p, root),
    ensures
        p.len() > root.len() > 0,
        p.subrange(0, root.len() as int) =~= root,
        p[root.len() as int] == SLASH,
        exists|j: int| root.len() + 1 <= j < p.len() && #[trigger] p[j] == SLASH,
{
}

/// P2, a look-alike sibling: `<root>x...` is not below the root.
pub proof fn p2_sibling_out(p: Seq<u8>, root: Seq<u8>)
    requires
        root.len() > 0,
        p.len() > root.len(),
        p[root.len() as int] != SLASH,
    ensures
        !under(p, root),
        !deep(p, root),
{
}

/// P3 to P5 for one list entry.
pub proof fn p3_to_p5(
    p: Seq<u8>,
    roots: Seq<Vec<u8>>,
    prefixes: Seq<Vec<u8>>,
    components: Seq<Vec<u8>>,
    starts: Seq<Vec<u8>>,
    substrings: Seq<Vec<u8>>,
)
    requires
        allowed(p, roots, prefixes, components, starts, substrings),
    ensures
        forall|k: int| 0 <= k < prefixes.len() ==> !(p =~= #[trigger] prefixes[k]@) && !under(p, prefixes[k]@),
        forall|k: int, i: int| #![trigger component_at(lower_all(p), components[k]@, i)] 0 <= k < components.len() ==> !component_at(lower_all(p), components[k]@, i),
        forall|k: int, i: int| #![trigger name_start_at(lower_all(p), starts[k]@, i)] 0 <= k < starts.len() ==> !name_start_at(lower_all(p), starts[k]@, i),
        forall|k: int, i: int| #![trigger at(lower_all(p), substrings[k]@, i)] 0 <= k < substrings.len() ==> !at(lower_all(p), substrings[k]@, i),
{
    let lp = lower_all(p);
    assert forall|k: int| 0 <= k < prefixes.len() implies !(p =~= #[trigger] prefixes[k]@) && !under(p, prefixes[k]@) by {
        if p =~= prefixes[k]@ || under(p, prefixes[k]@) {
            assert(denied_tree(p, prefixes));
        }
    }
    assert forall|k: int, i: int| #![trigger component_at(lp, components[k]@, i)] 0 <= k < components.len() implies !component_at(lp, components[k]@, i) by {
        if component_at(lp, components[k]@, i) {
            assert(component_hit(lp, components[k]@));
            assert(any_component(lp, components));
        }
    }
    assert forall|k: int, i: int| #![trigger name_start_at(lp, starts[k]@, i)] 0 <= k < starts.len() implies !name_start_at(lp, starts[k]@, i) by {
        if name_start_at(lp, starts[k]@, i) {
            assert(name_start_hit(lp, starts[k]@));
            assert(any_start(lp, starts));
        }
    }
    assert forall|k: int, i: int| #![trigger at(lp, substrings[k]@, i)] 0 <= k < substrings.len() implies !at(lp, substrings[k]@, i) by {
        if at(lp, substrings[k]@, i) {
            assert(contains(lp, substrings[k]@));
            assert(any_contains(lp, substrings));
        }
    }
}

// ------------------------------------------------------------------ the code

fn low_exec(c: u8) -> (r: u8)
    ensures
        r == low(c),
{
    if 65 <= c && c <= 90 {
        c + 32
    } else {
        c
    }
}

fn lower_exec(p: &Vec<u8>) -> (r: Vec<u8>)
    ensures
        r@ =~= lower_all(p@),
{
    let mut out: Vec<u8> = Vec::new();
    let mut i: usize = 0;
    while i < p.len()
        invariant
            i <= p.len(),
            out@.len() == i,
            forall|j: int| 0 <= j < i ==> #[trigger] out@[j] == low(p@[j]),
        decreases p.len() - i,
    {
        out.push(low_exec(p[i]));
        i += 1;
    }
    out
}

fn at_exec(h: &Vec<u8>, t: &Vec<u8>, i: usize) -> (b: bool)
    requires
        h@.len() < usize::MAX / 2,
        t@.len() < usize::MAX / 2,
        i < usize::MAX / 2,
    ensures
        b == at(h@, t@, i as int),
{
    if i + t.len() > h.len() {
        return false;
    }
    let mut k: usize = 0;
    while k < t.len()
        invariant
            k <= t.len(),
            i + t.len() <= h.len(),
            forall|m: int| 0 <= m < k ==> h@[i + m] == t@[m],
        decreases t.len() - k,
    {
        if h[i + k] != t[k] {
            proof {
                assert(h@.subrange(i as int, i + t.len())[k as int] != t@[k as int]);
            }
            return false;
        }
        k += 1;
    }
    proof {
        assert(h@.subrange(i as int, i + t.len()) =~= t@);
    }
    true
}

fn clean_exec(p: &Vec<u8>) -> (b: bool)
    requires
        p@.len() < usize::MAX / 2,
    ensures
        b == clean(p@),
{
    let n = p.len();
    if n == 0 || p[0] != SLASH {
        return false;
    }
    let mut i: usize = 0;
    while i < n
        invariant
            n == p@.len(),
            n < usize::MAX / 2,
            i <= n,
            p@[0] == SLASH,
            forall|j: int| 0 <= j < i ==> #[trigger] p@[j] != 0,
            forall|j: int| 0 <= j < i && #[trigger] p@[j] == SLASH ==> slash_ok(p@, j),
        decreases n - i,
    {
        if p[i] == 0 {
            return false;
        }
        if p[i] == SLASH {
            if i + 1 == n || p[i + 1] == SLASH {
                proof {
                    assert(!slash_ok(p@, i as int));
                }
                return false;
            }
            if p[i + 1] == DOT {
                if i + 2 == n || p[i + 2] == SLASH {
                    proof {
                        assert(!slash_ok(p@, i as int));
                    }
                    return false;
                }
                if p[i + 2] == DOT && (i + 3 == n || p[i + 3] == SLASH) {
                    proof {
                        assert(!slash_ok(p@, i as int));
                    }
                    return false;
                }
            }
        }
        i += 1;
    }
    true
}

fn under_exec(p: &Vec<u8>, d: &Vec<u8>) -> (b: bool)
    requires
        p@.len() < usize::MAX / 2,
        d@.len() < usize::MAX / 2,
    ensures
        b == under(p@, d@),
{
    if d.len() == 0 || d[d.len() - 1] == SLASH || p.len() <= d.len() {
        return false;
    }
    at_exec(p, d, 0) && p[d.len()] == SLASH
}

fn deep_exec(p: &Vec<u8>, root: &Vec<u8>) -> (b: bool)
    requires
        p@.len() < usize::MAX / 2,
        root@.len() < usize::MAX / 2,
    ensures
        b == deep(p@, root@),
{
    if !under_exec(p, root) {
        return false;
    }
    let mut j: usize = root.len() + 1;
    while j < p.len()
        invariant
            root@.len() + 1 <= j <= p@.len(),
            under(p@, root@),
            forall|m: int| root@.len() + 1 <= m < j ==> #[trigger] p@[m] != SLASH,
        decreases p.len() - j,
    {
        if p[j] == SLASH {
            proof {
                assert(root@.len() + 1 <= j as int && (j as int) < p@.len() && p@[j as int] == SLASH);
            }
            return true;
        }
        j += 1;
    }
    false
}

fn component_exec(lp: &Vec<u8>, c: &Vec<u8>) -> (b: bool)
    requires
        lp@.len() < usize::MAX / 4,
        c@.len() < usize::MAX / 4,
    ensures
        b == component_hit(lp@, c@),
{
    let mut i: usize = 0;
    while i < lp.len()
        invariant
            i <= lp@.len(),
            lp@.len() < usize::MAX / 4,
            c@.len() < usize::MAX / 4,
            forall|j: int| 0 <= j < i ==> !component_at(lp@, c@, j),
        decreases lp.len() - i,
    {
        if lp[i] == SLASH && at_exec(lp, c, i + 1) && (i + 1 + c.len() == lp.len() || lp[i + 1 + c.len()] == SLASH) {
            proof {
                assert(component_at(lp@, c@, i as int));
            }
            return true;
        }
        i += 1;
    }
    proof {
        assert forall|j: int| !component_at(lp@, c@, j) by {
            if 0 <= j < i {
            }
        }
    }
    false
}

fn start_exec(lp: &Vec<u8>, c: &Vec<u8>) -> (b: bool)
    requires
        lp@.len() < usize::MAX / 4,
        c@.len() < usize::MAX / 4,
    ensures
        b == name_start_hit(lp@, c@),
{
    let mut i: usize = 0;
    while i < lp.len()
        invariant
            i <= lp@.len(),
            lp@.len() < usize::MAX / 4,
            c@.len() < usize::MAX / 4,
            forall|j: int| 0 <= j < i ==> !name_start_at(lp@, c@, j),
        decreases lp.len() - i,
    {
        if lp[i] == SLASH && at_exec(lp, c, i + 1) {
            proof {
                assert(name_start_at(lp@, c@, i as int));
            }
            return true;
        }
        i += 1;
    }
    proof {
        assert forall|j: int| !name_start_at(lp@, c@, j) by {
            if 0 <= j < i {
            }
        }
    }
    false
}

fn contains_exec(lp: &Vec<u8>, c: &Vec<u8>) -> (b: bool)
    requires
        lp@.len() < usize::MAX / 4,
        c@.len() < usize::MAX / 4,
    ensures
        b == contains(lp@, c@),
{
    let mut i: usize = 0;
    while i <= lp.len()
        invariant
            i <= lp@.len() + 1,
            lp@.len() < usize::MAX / 4,
            c@.len() < usize::MAX / 4,
            forall|j: int| 0 <= j < i ==> !at(lp@, c@, j),
        decreases lp.len() + 1 - i,
    {
        if at_exec(lp, c, i) {
            return true;
        }
        i += 1;
    }
    proof {
        assert forall|j: int| !at(lp@, c@, j) by {
            if 0 <= j < i {
            }
        }
    }
    false
}

fn same_exec(a: &Vec<u8>, b: &Vec<u8>) -> (r: bool)
    ensures
        r == (a@ =~= b@),
{
    if a.len() != b.len() {
        return false;
    }
    let mut i: usize = 0;
    while i < a.len()
        invariant
            a@.len() == b@.len(),
            i <= a@.len(),
            forall|j: int| 0 <= j < i ==> a@[j] == b@[j],
        decreases a.len() - i,
    {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    proof {
        assert(a@ =~= b@);
    }
    true
}

pub open spec fn short(v: Seq<Vec<u8>>) -> bool {
    forall|k: int| 0 <= k < v.len() ==> (#[trigger] v[k])@.len() < usize::MAX / 4
}

fn any_root_exec(p: &Vec<u8>, roots: &Vec<Vec<u8>>) -> (b: bool)
    requires
        p@.len() < usize::MAX / 4,
        short(roots@),
    ensures
        b == any_root(p@, roots@),
{
    let mut k: usize = 0;
    while k < roots.len()
        invariant
            k <= roots@.len(),
            p@.len() < usize::MAX / 4,
            short(roots@),
            forall|m: int| 0 <= m < k ==> !deep(p@, (#[trigger] roots@[m])@),
        decreases roots.len() - k,
    {
        proof {
            assert(roots@[k as int]@.len() < usize::MAX / 4);
        }
        if deep_exec(p, &roots[k]) {
            return true;
        }
        k += 1;
    }
    false
}

/// The check. Every result equals the spec; P1 to P5 above say what the spec means.
pub fn allowed_exec(
    p: &Vec<u8>,
    roots: &Vec<Vec<u8>>,
    prefixes: &Vec<Vec<u8>>,
    components: &Vec<Vec<u8>>,
    starts: &Vec<Vec<u8>>,
    substrings: &Vec<Vec<u8>>,
) -> (b: bool)
    requires
        p@.len() < usize::MAX / 4,
        short(roots@),
        short(prefixes@),
        short(components@),
        short(starts@),
        short(substrings@),
    ensures
        b == allowed(p@, roots@, prefixes@, components@, starts@, substrings@),
{
    if !clean_exec(p) {
        return false;
    }
    if !any_root_exec(p, roots) {
        return false;
    }
    let mut k: usize = 0;
    while k < prefixes.len()
        invariant
            k <= prefixes@.len(),
            p@.len() < usize::MAX / 4,
            short(prefixes@),
            forall|m: int| 0 <= m < k ==> !(p@ =~= (#[trigger] prefixes@[m])@) && !under(p@, prefixes@[m]@),
        decreases prefixes.len() - k,
    {
        proof {
            assert(prefixes@[k as int]@.len() < usize::MAX / 4);
        }
        if same_exec(p, &prefixes[k]) || under_exec(p, &prefixes[k]) {
            proof {
                assert(denied_tree(p@, prefixes@));
            }
            return false;
        }
        k += 1;
    }
    let lp = lower_exec(p);
    let mut k: usize = 0;
    while k < components.len()
        invariant
            k <= components@.len(),
            lp@ =~= lower_all(p@),
            lp@.len() < usize::MAX / 4,
            short(components@),
            forall|m: int| 0 <= m < k ==> !component_hit(lp@, (#[trigger] components@[m])@),
        decreases components.len() - k,
    {
        proof {
            assert(components@[k as int]@.len() < usize::MAX / 4);
        }
        if component_exec(&lp, &components[k]) {
            proof {
                assert(any_component(lp@, components@));
            }
            return false;
        }
        k += 1;
    }
    let mut k: usize = 0;
    while k < starts.len()
        invariant
            k <= starts@.len(),
            lp@ =~= lower_all(p@),
            lp@.len() < usize::MAX / 4,
            short(starts@),
            forall|m: int| 0 <= m < k ==> !name_start_hit(lp@, (#[trigger] starts@[m])@),
        decreases starts.len() - k,
    {
        proof {
            assert(starts@[k as int]@.len() < usize::MAX / 4);
        }
        if start_exec(&lp, &starts[k]) {
            proof {
                assert(any_start(lp@, starts@));
            }
            return false;
        }
        k += 1;
    }
    let mut k: usize = 0;
    while k < substrings.len()
        invariant
            k <= substrings@.len(),
            lp@ =~= lower_all(p@),
            lp@.len() < usize::MAX / 4,
            short(substrings@),
            forall|m: int| 0 <= m < k ==> !contains(lp@, (#[trigger] substrings@[m])@),
        decreases substrings.len() - k,
    {
        proof {
            assert(substrings@[k as int]@.len() < usize::MAX / 4);
        }
        if contains_exec(&lp, &substrings[k]) {
            proof {
                assert(any_contains(lp@, substrings@));
            }
            return false;
        }
        k += 1;
    }
    true
}

// ------------------------------------------------------------------ keys

pub open spec fn norm_key(c: u8) -> u8 {
    if c == DASH { UNDERSCORE } else { low(c) }
}

pub open spec fn norm_all(k: Seq<u8>) -> Seq<u8> {
    Seq::new(k.len(), |i: int| norm_key(k[i]))
}

pub open spec fn key_refused(key: Seq<u8>, terms: Seq<Vec<u8>>) -> bool {
    any_contains(norm_all(key), terms)
}

/// K1: a refused term anywhere in the normalised key refuses it.
pub proof fn k1_refused(key: Seq<u8>, terms: Seq<Vec<u8>>, k: int, i: int)
    requires
        0 <= k < terms.len(),
        at(norm_all(key), terms[k]@, i),
    ensures
        key_refused(key, terms),
{
    assert(contains(norm_all(key), terms[k]@));
}

// ------------------------------------------------------------------ non-vacuity

/// `/r/a/k` with root `/r`, and empty deny lists: allowed.
pub proof fn witness_allowed() {
    let p = seq![47u8, 114u8, 47u8, 97u8, 47u8, 107u8];
    assert(p.len() == 6);
    assert forall|i: int| 0 <= i < p.len() implies #[trigger] p[i] != 0 by {}
    assert(p[0] == SLASH);
    assert(p[2] == SLASH);
    assert(p[4] == SLASH);
    assert forall|i: int| 0 <= i < p.len() && #[trigger] p[i] == SLASH implies slash_ok(p, i) by {
        assert(i == 0 || i == 2 || i == 4);
    }
    assert(clean(p));
    let root = seq![47u8, 114u8];
    assert(p.subrange(0, 2) =~= root);
    assert(under(p, root));
    assert(p[4] == SLASH);
    assert(deep(p, root));
}

/// `/r/../k`: not clean.
pub proof fn witness_dotdot() {
    let p = seq![47u8, 114u8, 47u8, 46u8, 46u8, 47u8, 107u8];
    assert(p[2] == SLASH);
    assert(!slash_ok(p, 2));
    assert(!clean(p));
}

/// `/rx/a/k` is not below root `/r`.
pub proof fn witness_sibling() {
    let p = seq![47u8, 114u8, 120u8, 47u8, 97u8, 47u8, 107u8];
    let root = seq![47u8, 114u8];
    assert(p[2] != SLASH);
    assert(!under(p, root));
}

/// `/A` lowercased holds the component `a`.
pub proof fn witness_case() {
    let p = seq![47u8, 65u8];
    let lp = lower_all(p);
    let c = seq![97u8];
    assert(lp[0] == SLASH);
    assert(lp[1] == 97u8);
    assert(lp.subrange(1, 2) =~= c);
    assert(component_at(lp, c, 0));
    assert(component_hit(lp, c));
}

fn main() {
}

} // verus!
