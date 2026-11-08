// Secrets policy kernel (lucina-secrets: which secret references Lucina may resolve).
//
// Production (branch lucina-rs, crates/lucina-secrets/src/):
//   in_folder_exec   <-> reference::SecretRef::in_folder (folder scope, segment-aware)
//   hard_denied_exec <-> policy::hard_denied (HARD_DENY substrings of the lowercased
//                        "key|path", and of it with every non-alphanumeric removed,
//                        compared with the term without '_')
//   decide           <-> policy::Policy::check (scope, then the hard list, then the exact
//                        allow names, then the deny terms)
// Strings are bytes: SecretRef::parse admits only ASCII in keys and paths, so the
// lowercasing and the alphanumeric test are the ASCII ones. The deny-term match
// (`tokens` + the >= 6 character substring rule) is an input of `decide` (`deny_hit`),
// computed in the diff test with the real `policy::tokens`.
// Differential test: crates/lucina-secrets/tests/verus_policy.rs.
//
// Proven:
//   F1 folder_boundary: in a non-root folder f, a path is in scope only if it is f or
//      continues f with '/' (a segment boundary).
//   F2 sibling_out: f ++ x with x not starting with '/' is out of scope ("/lucina-evil",
//      "/lucinax" are not in "/lucina"); parent_out: a strict prefix of f is out.
//   F3 trailing_slashes: "/lucina/" and "/lucina//" scope the same as "/lucina".
//   F4 fail_closed: a configured folder without its leading '/' ("lucina") admits
//      nothing (refs always carry a leading '/').
//   H1 hard_case_and_separators: if the key, lowercased and stripped of everything but
//      letters and digits, contains a hard term without its '_' ("sudo", "sshpass",
//      "loginpassword"), the ref is hard-denied: case and separators ("S-u-D_o",
//      "Login.Password") do not get around it. H2: the same for the path.
//   P1 out_of_scope_never_ok, P2 hard_beats_allow (an allow name never lifts a hard
//      term), P3 ok_iff: Ok exactly when in scope, not hard-denied, and allowed by exact
//      name or not hitting a deny term.
// Known gap kept visible (witness test in the diff test, not a proof here): digits glue
// to letters in `tokens` ("api-key2" -> "key2"), so "BANK_PIN2" or "PIN1" hit no deny
// term ("pin" is matched only as a whole token); see docs/verification-gaps.md, S1.
//
// No Stateright model: a pure decision per reference; the policy is immutable config.
use vstd::prelude::*;

verus! {

pub const SLASH: u8 = 47;
pub const BAR: u8 = 124;
pub const UNDERSCORE: u8 = 95;

// ---------------------------------------------------------------- folder scope

/// `folder.trim_end_matches('/')`.
pub open spec fn trim_end(f: Seq<u8>) -> Seq<u8>
    decreases f.len(),
{
    if f.len() > 0 && f.last() == SLASH {
        trim_end(f.drop_last())
    } else {
        f
    }
}

pub open spec fn starts_with(p: Seq<u8>, q: Seq<u8>) -> bool {
    q.len() <= p.len() && p.subrange(0, q.len() as int) =~= q
}

/// `in_folder`: `f.is_empty() || path == f || path.starts_with(&format!("{f}/"))`.
pub open spec fn in_folder(p: Seq<u8>, folder: Seq<u8>) -> bool {
    let f = trim_end(folder);
    f.len() == 0 || p =~= f || starts_with(p, f.push(SLASH))
}

proof fn lemma_trim_end(f: Seq<u8>)
    ensures
        trim_end(f).len() <= f.len(),
        trim_end(f) =~= f.subrange(0, trim_end(f).len() as int),
        trim_end(f).len() == 0 || trim_end(f).last() != SLASH,
    decreases f.len(),
{
    if f.len() > 0 && f.last() == SLASH {
        lemma_trim_end(f.drop_last());
        assert(trim_end(f) =~= f.drop_last().subrange(0, trim_end(f).len() as int));
    }
}

/// F1.
pub proof fn folder_boundary(p: Seq<u8>, folder: Seq<u8>)
    requires
        in_folder(p, folder),
        trim_end(folder).len() > 0,
    ensures
        ({
            let f = trim_end(folder);
            p =~= f || (starts_with(p, f) && p.len() > f.len() && p[f.len() as int] == SLASH)
        }),
{
    let f = trim_end(folder);
    if !(p =~= f) {
        assert(starts_with(p, f.push(SLASH)));
        assert(p.subrange(0, f.len() as int + 1)[f.len() as int] == SLASH);
        assert(p.subrange(0, f.len() as int) =~= p.subrange(0, f.len() as int + 1).subrange(
            0,
            f.len() as int,
        ));
    }
}

/// F2: a sibling sharing the folder's name as a prefix is out.
pub proof fn sibling_out(f: Seq<u8>, x: Seq<u8>)
    requires
        f.len() > 0,
        f.last() != SLASH,
        x.len() > 0,
        x[0] != SLASH,
    ensures
        !in_folder(f + x, f),
{
    lemma_trim_end(f);
    assert(trim_end(f) =~= f);
    let p = f + x;
    assert(!(p =~= f)) by {
        assert(p.len() != f.len());
    }
    if starts_with(p, f.push(SLASH)) {
        assert(p.subrange(0, f.len() as int + 1)[f.len() as int] == f.push(SLASH)[f.len() as int]);
        assert(p[f.len() as int] == x[0]);
    }
}

/// F2: a parent of the folder is out.
pub proof fn parent_out(p: Seq<u8>, f: Seq<u8>)
    requires
        f.len() > 0,
        f.last() != SLASH,
        p.len() < f.len(),
    ensures
        !in_folder(p, f),
{
    lemma_trim_end(f);
    assert(trim_end(f) =~= f);
}

/// F3.
pub proof fn trailing_slashes(p: Seq<u8>, f: Seq<u8>)
    ensures
        in_folder(p, f.push(SLASH)) == in_folder(p, f),
        in_folder(p, f.push(SLASH).push(SLASH)) == in_folder(p, f),
{
    assert(f.push(SLASH).drop_last() =~= f);
    assert(f.push(SLASH).push(SLASH).drop_last() =~= f.push(SLASH));
}

/// F4: a folder configured without its leading '/' admits no ref path.
pub proof fn fail_closed(p: Seq<u8>, folder: Seq<u8>)
    requires
        p.len() > 0,
        p[0] == SLASH,
        trim_end(folder).len() > 0,
        trim_end(folder)[0] != SLASH,
    ensures
        !in_folder(p, folder),
{
    let f = trim_end(folder);
    if starts_with(p, f.push(SLASH)) {
        assert(p.subrange(0, f.len() as int + 1)[0] == p[0]);
        assert(f.push(SLASH)[0] == f[0]);
    }
    if p =~= f {
        assert(p[0] == f[0]);
    }
}

fn trim_end_exec(f: &Vec<u8>) -> (n: usize)
    ensures
        n <= f.len(),
        n as int == trim_end(f@).len(),
        trim_end(f@) =~= f@.subrange(0, n as int),
{
    let mut n = f.len();
    proof {
        assert(f@.subrange(0, n as int) =~= f@);
    }
    while n > 0 && f[n - 1] == SLASH
        invariant
            n <= f.len(),
            trim_end(f@) == trim_end(f@.subrange(0, n as int)),
        decreases n,
    {
        proof {
            assert(f@.subrange(0, n as int).drop_last() =~= f@.subrange(0, n - 1));
        }
        n -= 1;
    }
    proof {
        let g = f@.subrange(0, n as int);
        assert(!(g.len() > 0 && g.last() == SLASH));
    }
    n
}

pub fn in_folder_exec(p: &Vec<u8>, folder: &Vec<u8>) -> (b: bool)
    ensures
        b == in_folder(p@, folder@),
{
    let n = trim_end_exec(folder);
    let ghost f = trim_end(folder@);
    if n == 0 {
        return true;
    }
    // path == f
    let mut eq = p.len() == n;
    // path starts with f + "/"
    let mut pre = p.len() > n && p[n] == SLASH;
    let mut i: usize = 0;
    while i < n
        invariant
            i <= n,
            n == f.len(),
            f =~= folder@.subrange(0, n as int),
            n <= folder.len(),
            eq ==> p.len() == n,
            pre ==> (p.len() > n && p@[n as int] == SLASH),
            eq == (p.len() == n && forall|j: int| 0 <= j < i ==> #[trigger] p@[j] == f[j]),
            pre == (p.len() > n && p@[n as int] == SLASH && forall|j: int|
                0 <= j < i ==> #[trigger] p@[j] == f[j]),
        decreases n - i,
    {
        proof {
            assert(f[i as int] == folder@[i as int]);
        }
        if i < p.len() && p[i] == folder[i] {
        } else {
            eq = false;
            pre = false;
        }
        i += 1;
    }
    proof {
        if eq {
            assert(p@ =~= f);
        }
        if p@ =~= f {
            assert(eq);
        }
        let g = f.push(SLASH);
        if pre {
            assert(p@.subrange(0, g.len() as int) =~= g);
        }
        if starts_with(p@, g) {
            assert forall|j: int| 0 <= j < n implies #[trigger] p@[j] == f[j] by {
                assert(p@.subrange(0, g.len() as int)[j] == g[j]);
            }
            assert(p@.subrange(0, g.len() as int)[n as int] == g[n as int]);
        }
    }
    eq || pre
}

// ---------------------------------------------------------------- hard list

pub open spec fn lower(c: u8) -> u8 {
    if 65 <= c <= 90 {
        (c + 32) as u8
    } else {
        c
    }
}

pub open spec fn alnum(c: u8) -> bool {
    (48 <= c <= 57) || (65 <= c <= 90) || (97 <= c <= 122)
}

pub open spec fn lower_all(s: Seq<u8>) -> Seq<u8> {
    s.map_values(|c: u8| lower(c))
}

/// Keep letters and digits (`filter(|c| c.is_alphanumeric())`).
pub open spec fn squash(s: Seq<u8>) -> Seq<u8>
    decreases s.len(),
{
    if s.len() == 0 {
        Seq::empty()
    } else {
        let p = squash(s.drop_last());
        if alnum(s.last()) {
            p.push(s.last())
        } else {
            p
        }
    }
}

/// Drop '_' (`t.replace('_', "")`).
pub open spec fn no_underscore(s: Seq<u8>) -> Seq<u8>
    decreases s.len(),
{
    if s.len() == 0 {
        Seq::empty()
    } else {
        let p = no_underscore(s.drop_last());
        if s.last() != UNDERSCORE {
            p.push(s.last())
        } else {
            p
        }
    }
}

pub open spec fn contains_at(h: Seq<u8>, t: Seq<u8>, i: int) -> bool {
    0 <= i && i + t.len() <= h.len() && h.subrange(i, i + t.len()) =~= t
}

pub open spec fn contains(h: Seq<u8>, t: Seq<u8>) -> bool {
    exists|i: int| contains_at(h, t, i)
}

pub open spec fn hay(key: Seq<u8>, path: Seq<u8>) -> Seq<u8> {
    lower_all(key).push(BAR) + lower_all(path)
}

pub open spec fn hit(key: Seq<u8>, path: Seq<u8>, t: Seq<u8>) -> bool {
    contains(hay(key, path), t) || contains(squash(hay(key, path)), no_underscore(t))
}

pub open spec fn hard(key: Seq<u8>, path: Seq<u8>, terms: Seq<Seq<u8>>) -> bool {
    exists|k: int| 0 <= k < terms.len() && hit(key, path, #[trigger] terms[k])
}

proof fn lemma_squash_append(a: Seq<u8>, b: Seq<u8>)
    ensures
        squash(a + b) =~= squash(a) + squash(b),
    decreases b.len(),
{
    if b.len() == 0 {
        assert(a + b =~= a);
    } else {
        lemma_squash_append(a, b.drop_last());
        assert((a + b).drop_last() =~= a + b.drop_last());
        assert((a + b).last() == b.last());
    }
}

proof fn lemma_contains_extend(h: Seq<u8>, t: Seq<u8>, x: Seq<u8>, y: Seq<u8>)
    requires
        contains(h, t),
    ensures
        contains(x + h + y, t),
{
    let i = choose|i: int| contains_at(h, t, i);
    let j = i + x.len();
    assert((x + h + y).subrange(j, j + t.len()) =~= h.subrange(i, i + t.len()));
    assert(contains_at(x + h + y, t, j));
}

/// H1: separators and case in the key do not get around a hard term.
pub proof fn hard_case_and_separators(
    key: Seq<u8>,
    path: Seq<u8>,
    terms: Seq<Seq<u8>>,
    k: int,
)
    requires
        0 <= k < terms.len(),
        contains(squash(lower_all(key)), no_underscore(terms[k])),
    ensures
        hard(key, path, terms),
{
    let a = lower_all(key).push(BAR);
    lemma_squash_append(lower_all(key), seq![BAR]);
    assert(lower_all(key) + seq![BAR] =~= a);
    lemma_squash_append(a, lower_all(path));
    reveal_with_fuel(squash, 2);
    assert(squash(seq![BAR]) =~= Seq::<u8>::empty());
    assert(squash(a) =~= squash(lower_all(key)));
    lemma_contains_extend(
        squash(lower_all(key)),
        no_underscore(terms[k]),
        Seq::empty(),
        squash(lower_all(path)),
    );
    assert(Seq::<u8>::empty() + squash(lower_all(key)) + squash(lower_all(path)) =~= squash(
        hay(key, path),
    ));
    assert(hit(key, path, terms[k]));
}

/// H2: the same for the folder path.
pub proof fn hard_in_path(key: Seq<u8>, path: Seq<u8>, terms: Seq<Seq<u8>>, k: int)
    requires
        0 <= k < terms.len(),
        contains(squash(lower_all(path)), no_underscore(terms[k])),
    ensures
        hard(key, path, terms),
{
    let a = lower_all(key).push(BAR);
    lemma_squash_append(a, lower_all(path));
    lemma_contains_extend(
        squash(lower_all(path)),
        no_underscore(terms[k]),
        squash(a),
        Seq::empty(),
    );
    assert(squash(a) + squash(lower_all(path)) + Seq::<u8>::empty() =~= squash(hay(key, path)));
    assert(hit(key, path, terms[k]));
}

fn lower_exec(c: u8) -> (r: u8)
    ensures
        r == lower(c),
{
    if 65 <= c && c <= 90 {
        c + 32
    } else {
        c
    }
}

fn contains_exec(h: &Vec<u8>, t: &Vec<u8>) -> (b: bool)
    ensures
        b == contains(h@, t@),
{
    if t.len() > h.len() {
        proof {
            assert forall|i: int| !contains_at(h@, t@, i) by {}
        }
        return false;
    }
    let mut i: usize = 0;
    while i <= h.len() - t.len()
        invariant
            t.len() <= h.len(),
            i <= h.len() - t.len() + 1,
            forall|j: int| 0 <= j < i ==> !contains_at(h@, t@, j),
        decreases h.len() - i,
    {
        let mut k: usize = 0;
        let mut same = true;
        while k < t.len()
            invariant
                k <= t.len(),
                i + t.len() <= h.len(),
                same == forall|m: int| 0 <= m < k ==> h@[i + m] == t@[m],
            decreases t.len() - k,
        {
            if h[i + k] != t[k] {
                same = false;
            }
            k += 1;
        }
        if same {
            proof {
                assert(h@.subrange(i as int, i + t.len()) =~= t@);
                assert(contains_at(h@, t@, i as int));
            }
            return true;
        }
        proof {
            if contains_at(h@, t@, i as int) {
                let m = choose|m: int| 0 <= m < t.len() && h@[i + m] != t@[m];
                assert(h@.subrange(i as int, i + t.len())[m] == h@[i + m]);
            }
        }
        i += 1;
    }
    proof {
        assert forall|j: int| !contains_at(h@, t@, j) by {
            if 0 <= j && j < i {
            }
        }
    }
    false
}

fn hay_exec(key: &Vec<u8>, path: &Vec<u8>) -> (h: Vec<u8>)
    ensures
        h@ =~= hay(key@, path@),
{
    let mut h: Vec<u8> = Vec::new();
    let mut i: usize = 0;
    while i < key.len()
        invariant
            i <= key.len(),
            h@ =~= lower_all(key@.subrange(0, i as int)),
        decreases key.len() - i,
    {
        h.push(lower_exec(key[i]));
        i += 1;
    }
    assert(key@.subrange(0, key.len() as int) =~= key@);
    h.push(BAR);
    let ghost base = h@;
    let mut j: usize = 0;
    while j < path.len()
        invariant
            j <= path.len(),
            base =~= lower_all(key@).push(BAR),
            h@ =~= base + lower_all(path@.subrange(0, j as int)),
        decreases path.len() - j,
    {
        h.push(lower_exec(path[j]));
        j += 1;
    }
    assert(path@.subrange(0, path.len() as int) =~= path@);
    h
}

fn squash_exec(s: &Vec<u8>) -> (r: Vec<u8>)
    ensures
        r@ =~= squash(s@),
{
    let mut r: Vec<u8> = Vec::new();
    let mut i: usize = 0;
    while i < s.len()
        invariant
            i <= s.len(),
            r@ =~= squash(s@.subrange(0, i as int)),
        decreases s.len() - i,
    {
        let c = s[i];
        proof {
            let t = s@.subrange(0, i as int + 1);
            assert(t.drop_last() =~= s@.subrange(0, i as int));
        }
        if (48 <= c && c <= 57) || (65 <= c && c <= 90) || (97 <= c && c <= 122) {
            r.push(c);
        }
        i += 1;
    }
    assert(s@.subrange(0, s.len() as int) =~= s@);
    r
}

fn no_underscore_exec(s: &Vec<u8>) -> (r: Vec<u8>)
    ensures
        r@ =~= no_underscore(s@),
{
    let mut r: Vec<u8> = Vec::new();
    let mut i: usize = 0;
    while i < s.len()
        invariant
            i <= s.len(),
            r@ =~= no_underscore(s@.subrange(0, i as int)),
        decreases s.len() - i,
    {
        proof {
            let t = s@.subrange(0, i as int + 1);
            assert(t.drop_last() =~= s@.subrange(0, i as int));
        }
        if s[i] != UNDERSCORE {
            r.push(s[i]);
        }
        i += 1;
    }
    assert(s@.subrange(0, s.len() as int) =~= s@);
    r
}

pub open spec fn views(terms: Seq<Vec<u8>>) -> Seq<Seq<u8>> {
    terms.map_values(|t: Vec<u8>| t@)
}

/// `hard_denied(key, path).is_some()`.
pub fn hard_denied_exec(key: &Vec<u8>, path: &Vec<u8>, terms: &Vec<Vec<u8>>) -> (b: bool)
    ensures
        b == hard(key@, path@, views(terms@)),
{
    let ghost ts = views(terms@);
    let h = hay_exec(key, path);
    let sq = squash_exec(&h);
    let mut k: usize = 0;
    while k < terms.len()
        invariant
            k <= terms.len(),
            ts == views(terms@),
            ts.len() == terms.len(),
            forall|m: int| 0 <= m < terms.len() ==> #[trigger] ts[m] == terms@[m]@,
            h@ =~= hay(key@, path@),
            sq@ =~= squash(hay(key@, path@)),
            forall|m: int| 0 <= m < k ==> !hit(key@, path@, #[trigger] ts[m]),
        decreases terms.len() - k,
    {
        let t = &terms[k];
        let tu = no_underscore_exec(t);
        if contains_exec(&h, t) || contains_exec(&sq, &tu) {
            proof {
                assert(h@ == hay(key@, path@));
                assert(sq@ == squash(hay(key@, path@)));
                assert(tu@ == no_underscore(t@));
                assert(ts[k as int] == t@);
                assert(hit(key@, path@, ts[k as int]));
                assert(0 <= k < ts.len());
                assert(hard(key@, path@, ts));
            }
            return true;
        }
        proof {
            assert(ts[k as int] == t@);
        }
        k += 1;
    }
    false
}

// ---------------------------------------------------------------- the decision

#[derive(PartialEq, Eq)]
pub enum Decision {
    Ok,
    OutOfScope,
    HardDenied,
    Denied,
}

/// `Policy::check`'s order.
pub fn decide(scope_ok: bool, hard_hit: bool, allowed: bool, deny_hit: bool) -> (d: Decision)
    ensures
        !scope_ok ==> d == Decision::OutOfScope,
        scope_ok && hard_hit ==> d == Decision::HardDenied,
        (d == Decision::Ok) == (scope_ok && !hard_hit && (allowed || !deny_hit)),
        d == Decision::Denied ==> scope_ok && !hard_hit && !allowed && deny_hit,
{
    if !scope_ok {
        return Decision::OutOfScope;
    }
    if hard_hit {
        return Decision::HardDenied;
    }
    if allowed {
        return Decision::Ok;
    }
    if deny_hit {
        return Decision::Denied;
    }
    Decision::Ok
}

/// P2: whatever the allow list says, a hard hit is never Ok.
pub proof fn hard_beats_allow(scope_ok: bool, hard_hit: bool, allowed: bool, deny_hit: bool)
    requires
        hard_hit,
    ensures
        !(scope_ok && !hard_hit && (allowed || !deny_hit)),
{
}

/// Witnesses (not vacuous): "/lucina/github" is in "/lucina", "/lucinax" is not, "SuDo"
/// is hard-denied.
pub proof fn witnesses() {
    let f = seq![47u8, 108, 117, 99, 105, 110, 97];  // "/lucina"
    let sub = f + seq![47u8, 103, 104];  // "/lucina/gh"
    lemma_trim_end(f);
    assert(trim_end(f) =~= f);
    assert(sub.subrange(0, 8) =~= f.push(SLASH));
    assert(in_folder(sub, f));
    sibling_out(f, seq![120u8]);  // "/lucinax"
    let key = seq![83u8, 117, 68, 111];  // "SuDo"
    let sudo = seq![115u8, 117, 100, 111];
    reveal_with_fuel(squash, 5);
    reveal_with_fuel(no_underscore, 5);
    assert(lower_all(key) =~= sudo);
    assert(squash(sudo) =~= sudo);
    assert(no_underscore(sudo) =~= sudo);
    assert(contains_at(squash(lower_all(key)), no_underscore(sudo), 0));
    hard_case_and_separators(key, f, seq![sudo], 0);
}

} // verus!

fn main() {}
