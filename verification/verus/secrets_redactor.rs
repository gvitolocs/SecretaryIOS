// Secrets redactor kernel: the known-values pass, on bytes.
//
// Production: crates/lucina-secrets/src/redact.rs (worktree SecretaryIOS-secrets, branch
// lucina-secrets, not committed yet when this was written):
//   pats / phs        <-> the registered encodings of every secret (Redactor::register:
//                         raw, JSON-escaped once and twice, URL-encoded, base64 at three
//                         alignments in two alphabets; at least MIN_PATTERN_LEN = 6 bytes)
//                         and their placeholders `[secret:<NAME>]` (placeholder())
//   best / redact     <-> Redactor::find_known (all hits, sorted by start then longest
//                         first, kept when they start at or after the previous kept end)
//                         + Redactor::redact_known (each kept hit becomes its placeholder).
//                         Taking the longest match at the leftmost unconsumed position is
//                         the same greedy choice.
//   redact_exec       <-> the plain loop the differential test runs against redact_known
// Differential test: verification/verus/diff/lucina_secrets_redact.rs (to go to
// crates/lucina-secrets/tests/ once that crate is committed).
//
// Proven, for well-formed inputs (wf: every pattern non-empty without '[' or ']' and
// not inside any placeholder; every placeholder is '[' ... ']'):
//   R1 no_secret_survives: the output contains no occurrence of any pattern.
//   R2 redact_idempotent: redacting the output again changes nothing.
//   R3 length bound: min_len * |out| <= max(min_len, max_placeholder) * |in|
//      (production: |out| <= ceil(73/6) * |in| with names up to 64 bytes).
//   R4 redact_exec == redact (the loop the test copies computes the spec).
//
// Without wf both R1 and R2 fail on production (see bad_* below and the report):
//   - a secret that is part of a placeholder: value "secret" (or a name such as "GITHUB")
//     -> "[secret:X]" still contains it, and a second pass rewrites the placeholder;
//   - a secret ending a placeholder: X = "aaaaaa", Y = "X]bbbbbb"; "aaaaaabbbbbb" ->
//     "[secret:X]bbbbbb", which contains Y's value though the input did not.
use vstd::prelude::*;

verus! {

pub const LB: u8 = 91; // '['
pub const RB: u8 = 93; // ']'

pub open spec fn matches_at(t: Seq<u8>, i: int, p: Seq<u8>) -> bool {
    0 <= i && i + p.len() <= t.len() && t.subrange(i, i + p.len()) == p
}

pub open spec fn occurs(t: Seq<u8>, p: Seq<u8>) -> bool {
    exists|j: int| matches_at(t, j, p)
}

/// The longest pattern among pats[0..k) that matches at the start of t (first on ties).
pub open spec fn best_upto(t: Seq<u8>, pats: Seq<Seq<u8>>, k: int) -> Option<int>
    decreases k,
{
    if k <= 0 {
        None
    } else {
        let prev = best_upto(t, pats, k - 1);
        if matches_at(t, 0, pats[k - 1]) && (prev is None || pats[k - 1].len() > pats[prev->0].len()) {
            Some(k - 1)
        } else {
            prev
        }
    }
}

pub open spec fn best(t: Seq<u8>, pats: Seq<Seq<u8>>) -> Option<int> {
    best_upto(t, pats, pats.len() as int)
}

/// How many bytes the step at the start of t consumes, and what it emits.
pub open spec fn hit_len(t: Seq<u8>, pats: Seq<Seq<u8>>) -> int {
    match best(t, pats) {
        Some(k) => if 0 <= k < pats.len() && 0 < pats[k].len() <= t.len() {
            pats[k].len() as int
        } else {
            0
        },
        None => 0,
    }
}

pub open spec fn redact(t: Seq<u8>, pats: Seq<Seq<u8>>, phs: Seq<Seq<u8>>) -> Seq<u8>
    decreases t.len(),
{
    if t.len() == 0 {
        Seq::empty()
    } else {
        let n = hit_len(t, pats);
        if n > 0 {
            phs[best(t, pats)->0] + redact(t.skip(n), pats, phs)
        } else {
            seq![t[0]] + redact(t.skip(1), pats, phs)
        }
    }
}

pub open spec fn no_brackets(p: Seq<u8>) -> bool {
    forall|i: int| 0 <= i < p.len() ==> #[trigger] p[i] != LB && p[i] != RB
}

pub open spec fn wf(pats: Seq<Seq<u8>>, phs: Seq<Seq<u8>>) -> bool {
    &&& pats.len() == phs.len()
    &&& forall|k: int| 0 <= k < pats.len() ==> (#[trigger] pats[k]).len() >= 1 && no_brackets(pats[k])
    &&& forall|k: int|
        0 <= k < phs.len() ==> (#[trigger] phs[k]).len() >= 2 && phs[k][0] == LB && phs[k][phs[k].len()
            - 1] == RB
    &&& forall|k: int, j: int| 0 <= k < pats.len() && 0 <= j < phs.len() ==> !occurs(#[trigger] phs[j], #[trigger] pats[k])
}

// ---------------------------------------------------------------- best

proof fn lemma_best_upto(t: Seq<u8>, pats: Seq<Seq<u8>>, k: int)
    requires
        0 <= k <= pats.len(),
    ensures
        match best_upto(t, pats, k) {
            Some(b) => 0 <= b < k && matches_at(t, 0, pats[b]) && forall|m: int|
                0 <= m < k && matches_at(t, 0, #[trigger] pats[m]) ==> pats[m].len() <= pats[b].len(),
            None => forall|m: int| 0 <= m < k ==> !matches_at(t, 0, #[trigger] pats[m]),
        },
    decreases k,
{
    if k > 0 {
        lemma_best_upto(t, pats, k - 1);
    }
}

proof fn lemma_best(t: Seq<u8>, pats: Seq<Seq<u8>>, phs: Seq<Seq<u8>>)
    requires
        wf(pats, phs),
        t.len() > 0,
    ensures
        (hit_len(t, pats) > 0) == (best(t, pats) is Some),
        best(t, pats) is Some ==> {
            let k = best(t, pats)->0;
            &&& 0 <= k < pats.len()
            &&& matches_at(t, 0, pats[k])
            &&& hit_len(t, pats) == pats[k].len()
        },
        best(t, pats) is None ==> forall|m: int| 0 <= m < pats.len() ==> !matches_at(t, 0, #[trigger] pats[m]),
{
    lemma_best_upto(t, pats, pats.len() as int);
    if best(t, pats) is Some {
        let k = best(t, pats)->0;
        assert(pats[k].len() >= 1);
    }
}

// ---------------------------------------------------------------- R1

/// Before the first '[' the output is a copy of the input.
proof fn lemma_prefix_raw(u: Seq<u8>, pats: Seq<Seq<u8>>, phs: Seq<Seq<u8>>, c: int)
    requires
        wf(pats, phs),
        0 <= c <= redact(u, pats, phs).len(),
        forall|i: int| 0 <= i < c ==> #[trigger] redact(u, pats, phs)[i] != LB,
    ensures
        c <= u.len(),
        redact(u, pats, phs).subrange(0, c) == u.subrange(0, c),
    decreases u.len(),
{
    let out = redact(u, pats, phs);
    if c == 0 {
        assert(out.subrange(0, 0) =~= u.subrange(0, 0));
        return;
    }
    if u.len() == 0 {
        return;
    }
    lemma_best(u, pats, phs);
    let n = hit_len(u, pats);
    if n > 0 {
        let k = best(u, pats)->0;
        assert(out[0] == phs[k][0]);
        assert(false);
    } else {
        let rest = u.skip(1);
        let r = redact(rest, pats, phs);
        assert(out == seq![u[0]] + r);
        assert forall|i: int| 0 <= i < c - 1 implies #[trigger] r[i] != LB by {
            assert(out[i + 1] == r[i]);
        }
        lemma_prefix_raw(rest, pats, phs, c - 1);
        assert(out.subrange(0, c) =~= seq![u[0]] + r.subrange(0, c - 1));
        assert(u.subrange(0, c) =~= seq![u[0]] + rest.subrange(0, c - 1));
    }
}

/// R1: no pattern occurs in the output.
pub proof fn no_secret_survives(t: Seq<u8>, pats: Seq<Seq<u8>>, phs: Seq<Seq<u8>>, m: int, j: int)
    requires
        wf(pats, phs),
        0 <= m < pats.len(),
    ensures
        !matches_at(redact(t, pats, phs), j, pats[m]),
    decreases t.len(),
{
    let out = redact(t, pats, phs);
    let p = pats[m];
    if !matches_at(out, j, p) {
        return;
    }
    if t.len() == 0 {
        return;
    }
    lemma_best(t, pats, phs);
    let n = hit_len(t, pats);
    if n > 0 {
        let k = best(t, pats)->0;
        let ph = phs[k];
        let rest = t.skip(n);
        let r = redact(rest, pats, phs);
        assert(out == ph + r);
        if j >= ph.len() {
            assert(r.subrange(j - ph.len(), j - ph.len() + p.len()) =~= out.subrange(j, j + p.len()));
            no_secret_survives(rest, pats, phs, m, j - ph.len());
        } else if j + p.len() <= ph.len() {
            assert(ph.subrange(j, j + p.len()) =~= out.subrange(j, j + p.len()));
            assert(matches_at(ph, j, p));
            assert(!occurs(phs[k], pats[m]));
        } else {
            let i = ph.len() - 1 - j;
            assert(out[ph.len() - 1] == RB);
            assert(out.subrange(j, j + p.len())[i] == out[ph.len() - 1]);
            assert(p[i] == RB);
            assert(no_brackets(pats[m]));
        }
    } else {
        let rest = t.skip(1);
        let r = redact(rest, pats, phs);
        assert(out == seq![t[0]] + r);
        if j >= 1 {
            assert(r.subrange(j - 1, j - 1 + p.len()) =~= out.subrange(j, j + p.len()));
            no_secret_survives(rest, pats, phs, m, j - 1);
        } else {
            // j == 0: the output starts with p, and p has no '[', so the input did too.
            assert(no_brackets(pats[m]));
            let c = p.len() - 1;
            assert forall|i: int| 0 <= i < c implies #[trigger] r[i] != LB by {
                assert(out.subrange(0, p.len() as int)[i + 1] == out[i + 1]);
                assert(out[i + 1] == r[i]);
                assert(p[i + 1] != LB);
            }
            lemma_prefix_raw(rest, pats, phs, c);
            assert(t.subrange(0, p.len() as int) =~= seq![t[0]] + rest.subrange(0, c));
            assert(out.subrange(0, p.len() as int) =~= seq![t[0]] + r.subrange(0, c));
            assert(matches_at(t, 0, pats[m]));
        }
    }
}

// ---------------------------------------------------------------- R2

proof fn lemma_clean_is_fixed(u: Seq<u8>, pats: Seq<Seq<u8>>, phs: Seq<Seq<u8>>)
    requires
        wf(pats, phs),
        forall|m: int, j: int| 0 <= m < pats.len() ==> !#[trigger] matches_at(u, j, pats[m]),
    ensures
        redact(u, pats, phs) == u,
    decreases u.len(),
{
    if u.len() > 0 {
        lemma_best(u, pats, phs);
        let rest = u.skip(1);
        assert forall|m: int, j: int| 0 <= m < pats.len() implies !#[trigger] matches_at(rest, j, pats[m]) by {
            if matches_at(rest, j, pats[m]) {
                assert(u.subrange(j + 1, j + 1 + pats[m].len()) =~= rest.subrange(j, j + pats[m].len()));
                assert(matches_at(u, j + 1, pats[m]));
            }
        }
        lemma_clean_is_fixed(rest, pats, phs);
        assert(seq![u[0]] + rest =~= u);
    }
}

/// R2: redaction is idempotent.
pub proof fn redact_idempotent(t: Seq<u8>, pats: Seq<Seq<u8>>, phs: Seq<Seq<u8>>)
    requires
        wf(pats, phs),
    ensures
        redact(redact(t, pats, phs), pats, phs) == redact(t, pats, phs),
{
    let out = redact(t, pats, phs);
    assert forall|m: int, j: int| 0 <= m < pats.len() implies !#[trigger] matches_at(out, j, pats[m]) by {
        no_secret_survives(t, pats, phs, m, j);
    }
    lemma_clean_is_fixed(out, pats, phs);
}

// ---------------------------------------------------------------- R3

/// R3: min_len * |out| <= cap * |in|, cap >= min_len and cap >= every placeholder length.
pub proof fn length_bound(t: Seq<u8>, pats: Seq<Seq<u8>>, phs: Seq<Seq<u8>>, min_len: int, cap: int)
    requires
        wf(pats, phs),
        1 <= min_len <= cap,
        forall|k: int| 0 <= k < pats.len() ==> (#[trigger] pats[k]).len() >= min_len,
        forall|k: int| 0 <= k < phs.len() ==> (#[trigger] phs[k]).len() <= cap,
    ensures
        min_len * redact(t, pats, phs).len() <= cap * t.len(),
    decreases t.len(),
{
    if t.len() > 0 {
        lemma_best(t, pats, phs);
        let n = hit_len(t, pats);
        let out = redact(t, pats, phs);
        if n > 0 {
            let k = best(t, pats)->0;
            let rest = t.skip(n);
            length_bound(rest, pats, phs, min_len, cap);
            let a = phs[k].len() as int;
            let rl = redact(rest, pats, phs).len() as int;
            assert(out.len() == a + rl);
            assert(rest.len() == t.len() - n);
            assert(min_len * a <= cap * n) by (nonlinear_arith)
                requires
                    0 <= a <= cap,
                    1 <= min_len <= n,
            ;
            assert(min_len * (a + rl) == min_len * a + min_len * rl) by (nonlinear_arith);
            assert(cap * t.len() == cap * n + cap * rest.len()) by (nonlinear_arith)
                requires
                    rest.len() == t.len() - n,
            ;
        } else {
            let rest = t.skip(1);
            length_bound(rest, pats, phs, min_len, cap);
            let rl = redact(rest, pats, phs).len() as int;
            assert(out.len() == 1 + rl);
            assert(min_len * (1 + rl) == min_len + min_len * rl) by (nonlinear_arith);
            assert(cap * t.len() == cap + cap * rest.len()) by (nonlinear_arith)
                requires
                    rest.len() == t.len() - 1,
            ;
        }
    }
}

// ---------------------------------------------------------------- the bugs without wf

/// "secret" inside its own placeholder.
pub proof fn bad_secret_inside_placeholder()
    ensures
        ({
            // pattern "secret", placeholder "[secret:X]"
            let p = seq![115u8, 101u8, 99u8, 114u8, 101u8, 116u8];
            let ph = seq![91u8, 115u8, 101u8, 99u8, 114u8, 101u8, 116u8, 58u8, 88u8, 93u8];
            matches_at(ph, 1, p)
        }),
{
    let p = seq![115u8, 101u8, 99u8, 114u8, 101u8, 116u8];
    let ph = seq![91u8, 115u8, 101u8, 99u8, 114u8, 101u8, 116u8, 58u8, 88u8, 93u8];
    assert(ph.subrange(1, 7) =~= p);
}

// ---------------------------------------------------------------- R4: the loop

pub fn bytes_match_at(t: &Vec<u8>, i: usize, p: &Vec<u8>) -> (r: bool)
    ensures
        r == matches_at(t@, i as int, p@),
{
    if p.len() > t.len() || i > t.len() - p.len() {
        return false;
    }
    let mut j: usize = 0;
    while j < p.len()
        invariant
            i + p.len() <= t.len(),
            0 <= j <= p.len(),
            forall|q: int| 0 <= q < j ==> t@[i + q] == p@[q],
        decreases p.len() - j,
    {
        if t[i + j] != p[j] {
            assert(t@.subrange(i as int, i + p.len())[j as int] != p@[j as int]);
            return false;
        }
        j += 1;
    }
    assert(t@.subrange(i as int, i + p.len()) =~= p@);
    true
}

pub open spec fn wf_exec(pats: Seq<Vec<u8>>, phs: Seq<Vec<u8>>) -> bool {
    wf(pats.map_values(|v: Vec<u8>| v@), phs.map_values(|v: Vec<u8>| v@))
}

/// The longest match at position i (first on ties), or None.
fn best_at(t: &Vec<u8>, i: usize, pats: &Vec<Vec<u8>>) -> (r: Option<usize>)
    requires
        i <= t.len(),
    ensures
        ({
            let ps = pats@.map_values(|v: Vec<u8>| v@);
            match best(t@.skip(i as int), ps) {
                Some(k) => r is Some && r->0 as int == k,
                None => r is None,
            }
        }),
{
    let ghost ps = pats@.map_values(|v: Vec<u8>| v@);
    let ghost u = t@.skip(i as int);
    let mut cur: Option<usize> = None;
    let mut k: usize = 0;
    while k < pats.len()
        invariant
            0 <= k <= pats.len(),
            i <= t.len(),
            ps == pats@.map_values(|v: Vec<u8>| v@),
            u == t@.skip(i as int),
            match best_upto(u, ps, k as int) {
                Some(b) => cur is Some && cur->0 as int == b && 0 <= b < k,
                None => cur is None,
            },
        decreases pats.len() - k,
    {
        let m = bytes_match_at(t, i, &pats[k]);
        proof {
            assert(ps[k as int] == pats@[k as int]@);
            if m {
                assert(u.subrange(0, ps[k as int].len() as int) =~= t@.subrange(i as int, i + ps[k as int].len()));
            } else if matches_at(u, 0, ps[k as int]) {
                assert(u.subrange(0, ps[k as int].len() as int) =~= t@.subrange(i as int, i + ps[k as int].len()));
            }
        }
        if m {
            let better = match cur {
                None => true,
                Some(b) => pats[k].len() > pats[b].len(),
            };
            if better {
                cur = Some(k);
            }
        }
        k += 1;
    }
    cur
}

/// R4: the loop computes redact.
pub fn redact_exec(t: &Vec<u8>, pats: &Vec<Vec<u8>>, phs: &Vec<Vec<u8>>) -> (out: Vec<u8>)
    requires
        wf_exec(pats@, phs@),
    ensures
        out@ == redact(t@, pats@.map_values(|v: Vec<u8>| v@), phs@.map_values(|v: Vec<u8>| v@)),
{
    let ghost ps = pats@.map_values(|v: Vec<u8>| v@);
    let ghost hs = phs@.map_values(|v: Vec<u8>| v@);
    let mut out: Vec<u8> = Vec::new();
    let mut i: usize = 0;
    proof {
        assert(t@.skip(0) =~= t@);
        assert(out@ + redact(t@, ps, hs) =~= redact(t@, ps, hs));
    }
    while i < t.len()
        invariant
            0 <= i <= t.len(),
            ps == pats@.map_values(|v: Vec<u8>| v@),
            hs == phs@.map_values(|v: Vec<u8>| v@),
            wf(ps, hs),
            out@ + redact(t@.skip(i as int), ps, hs) == redact(t@, ps, hs),
        decreases t.len() - i,
    {
        let ghost u = t@.skip(i as int);
        proof {
            lemma_best(u, ps, hs);
        }
        let b = best_at(t, i, pats);
        match b {
            Some(k) => {
                let n = pats[k].len();
                proof {
                    assert(ps[k as int] == pats@[k as int]@);
                    assert(hs[k as int] == phs@[k as int]@);
                    assert(best(u, ps) is Some);
                    assert(best(u, ps)->0 == k as int);
                    assert(matches_at(u, 0, ps[k as int]));
                    assert(n <= u.len());
                    assert(hit_len(u, ps) == n);
                    assert(u.skip(n as int) =~= t@.skip(i + n));
                }
                let mut q: usize = 0;
                let ghost out0 = out@;
                while q < phs[k].len()
                    invariant
                        k < phs.len(),
                        0 <= q <= phs[k as int].len(),
                        out@ == out0 + phs[k as int]@.subrange(0, q as int),
                    decreases phs[k as int].len() - q,
                {
                    out.push(phs[k][q]);
                    q += 1;
                    proof {
                        assert(phs[k as int]@.subrange(0, q as int) =~= phs[k as int]@.subrange(0, q - 1)
                            .push(phs[k as int]@[q - 1]));
                    }
                }
                proof {
                    assert(phs[k as int]@.subrange(0, q as int) =~= phs[k as int]@);
                    assert(out0 + (hs[k as int] + redact(u.skip(n as int), ps, hs)) =~= out@ + redact(
                        t@.skip(i + n),
                        ps,
                        hs,
                    ));
                }
                i = i + n;
            },
            None => {
                out.push(t[i]);
                proof {
                    assert(u.skip(1) =~= t@.skip(i + 1));
                    assert(u[0] == t@[i as int]);
                }
                i = i + 1;
            },
        }
    }
    proof {
        assert(t@.skip(i as int).len() == 0);
        assert(out@ + Seq::<u8>::empty() =~= out@);
    }
    out
}

} // verus!
fn main() {}
