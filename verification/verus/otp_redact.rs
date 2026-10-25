// OTP redaction: `otp_spans` (collect, sort, merge) and `redact_otps`.
//
// Production (branch lucina-rs, c357646): crates/lucina-phone-core/src/otp.rs
//   collect      <-> `otp_spans` before the sort: every GOOGLE match, then (when there is
//                    a context word) every CODES match that is `plausible` and within
//                    MAX_DISTANCE of some context
//   sort_spans   <-> `spans.sort_unstable()` (lexicographic on (start, end))
//   merge        <-> the merge loop (`start < last.1` extends, else push)
//   redact       <-> `redact_otps` (text before each span, then REDACTED, then the tail)
// Differential test: verification/verus/diff/otp_redact.rs.
//
// Modelled: the span logic exactly, over the tokenizer's output on the text being
// redacted (same Cand / Ctx shape as otp_extract.rs: positions in bytes, wf: start < end,
// 4 <= len <= 10, spans inside the text). The output is a sequence of items, Keep(p)
// for the input byte at p and Ph for one REDACTED placeholder: what the code
// concatenates. Abstracted: the regexes (tokenizer), UTF-8 (spans are on char
// boundaries because the regexes match whole chars; not modelled).
//
// Proven:
//   D1 collect: every Google span and every candidate that passes the selection guard
//      against some context is collected, and nothing else is.
//   D2 sort: sorted, same elements.
//   D3 merge: sorted, pairwise disjoint, every input span lies inside one merged span,
//      and a position is covered by the merged spans iff it is covered by an input span.
//   D4 redact: no Keep(p) for a covered p; every uncovered position is kept, in order
//      (text outside spans unchanged); one Ph per merged span; identity with no spans.
//   D5 (theorem otp_spans_hide_selectable): for every candidate that `parse_otp` could
//      select, i.e. eligible (with or without the amount fix) and within MAX_DISTANCE
//      of some context span, no byte of it survives redaction.
// D5 holds when redaction sees the same candidates and contexts as `parse_otp`. On
// lucina-rs it does not: `parse_otp` tokenizes the digit-normalized, URL-stripped text
// while `otp_spans` tokenizes the raw text, so codes in Arabic-Indic / Devanagari /
// full-width digits, and codes moved within 64 bytes of a context only by removing a
// URL, are selected but not redacted (KNOWN BUG, diff/otp_redact.rs).
use vstd::prelude::*;

verus! {

pub const MAX_DISTANCE: usize = 64;

#[derive(Clone, Copy)]
pub struct Cand {
    pub start: usize,
    pub end: usize,
    pub len: usize,
    pub has_digit: bool,
    pub all_digits: bool,
    pub after_plus: bool,
    pub in_url: bool,
    pub currency: bool,
    pub numeric_join: bool,
}

#[derive(Clone, Copy)]
pub struct Ctx {
    pub start: usize,
    pub end: usize,
}

pub open spec fn dist(c: Cand, x: Ctx) -> int {
    if c.end <= x.start {
        x.start - c.end
    } else if c.start >= x.end {
        c.start - x.end
    } else {
        0
    }
}

/// `plausible` (redaction does not strip URLs).
pub open spec fn plausible(c: Cand) -> bool {
    c.has_digit && !(c.all_digits && c.len > 8) && !c.after_plus
}

/// The selection guard of otp_extract.rs.
pub open spec fn selectable(c: Cand, fixed: bool) -> bool {
    plausible(c) && !c.in_url && (fixed ==> !(c.all_digits && (c.currency || c.numeric_join)))
}

pub open spec fn near(c: Cand, ctxs: Seq<Ctx>) -> bool {
    exists|k: int| 0 <= k < ctxs.len() && #[trigger] dist(c, ctxs[k]) <= MAX_DISTANCE
}

pub open spec fn span_of(c: Cand) -> (usize, usize) {
    (c.start, c.end)
}

pub open spec fn collected(s: (usize, usize), cands: Seq<Cand>, ctxs: Seq<Ctx>, google: Seq<(usize, usize)>) -> bool {
    google.contains(s) || exists|i: int|
        0 <= i < cands.len() && plausible(#[trigger] cands[i]) && near(cands[i], ctxs) && span_of(cands[i]) == s
}

pub open spec fn wf_span(s: (usize, usize), n: nat) -> bool {
    s.0 < s.1 && s.1 <= n
}

pub open spec fn wf(cands: Seq<Cand>, google: Seq<(usize, usize)>, n: nat) -> bool {
    &&& forall|i: int| 0 <= i < cands.len() ==> wf_span(span_of(#[trigger] cands[i]), n)
    &&& forall|i: int| 0 <= i < google.len() ==> wf_span(#[trigger] google[i], n)
}

pub open spec fn in_span(p: int, s: (usize, usize)) -> bool {
    s.0 <= p < s.1
}

pub open spec fn covered(p: int, spans: Seq<(usize, usize)>) -> bool {
    exists|m: int| 0 <= m < spans.len() && in_span(p, #[trigger] spans[m])
}

pub open spec fn le(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1)
}

pub open spec fn sorted(s: Seq<(usize, usize)>) -> bool {
    forall|i: int, j: int| 0 <= i < j < s.len() ==> le(#[trigger] s[i], #[trigger] s[j])
}

pub open spec fn disjoint(s: Seq<(usize, usize)>) -> bool {
    forall|i: int, j: int| 0 <= i < j < s.len() ==> (#[trigger] s[i]).1 <= (#[trigger] s[j]).0
}

// ---------------------------------------------------------------- D1 collect

fn dist_exec(c: Cand, x: Ctx) -> (d: usize)
    ensures
        d as int == dist(c, x),
{
    if c.end <= x.start {
        x.start - c.end
    } else if c.start >= x.end {
        c.start - x.end
    } else {
        0
    }
}

fn near_exec(c: Cand, ctxs: &Vec<Ctx>) -> (b: bool)
    ensures
        b == near(c, ctxs@),
{
    let mut k: usize = 0;
    while k < ctxs.len()
        invariant
            0 <= k <= ctxs.len(),
            forall|j: int| 0 <= j < k ==> #[trigger] dist(c, ctxs@[j]) > MAX_DISTANCE,
        decreases ctxs.len() - k,
    {
        if dist_exec(c, ctxs[k]) <= MAX_DISTANCE {
            assert(dist(c, ctxs@[k as int]) <= MAX_DISTANCE);
            return true;
        }
        k += 1;
    }
    false
}

pub fn collect(cands: &Vec<Cand>, ctxs: &Vec<Ctx>, google: &Vec<(usize, usize)>) -> (out: Vec<(usize, usize)>)
    ensures
        forall|i: int| 0 <= i < google@.len() ==> out@.contains(#[trigger] google@[i]),
        forall|i: int|
            0 <= i < cands@.len() && plausible(#[trigger] cands@[i]) && near(cands@[i], ctxs@) ==> out@.contains(
                span_of(cands@[i]),
            ),
        forall|j: int| 0 <= j < out@.len() ==> collected(#[trigger] out@[j], cands@, ctxs@, google@),
{
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut i: usize = 0;
    while i < google.len()
        invariant
            0 <= i <= google.len(),
            out@ == google@.subrange(0, i as int),
        decreases google.len() - i,
    {
        out.push(google[i]);
        assert(out@ =~= google@.subrange(0, i + 1));
        i += 1;
    }
    assert(out@ =~= google@);
    assert forall|j: int| 0 <= j < out@.len() implies collected(#[trigger] out@[j], cands@, ctxs@, google@) by {
        assert(google@.contains(out@[j]));
    }
    assert forall|j: int| 0 <= j < google@.len() implies out@.contains(#[trigger] google@[j]) by {
        assert(out@[j] == google@[j]);
    }
    if ctxs.len() > 0 {
        let mut b: usize = 0;
        while b < cands.len()
            invariant
                0 <= b <= cands.len(),
                forall|j: int| 0 <= j < google@.len() ==> out@.contains(#[trigger] google@[j]),
                forall|j: int|
                    0 <= j < b && plausible(#[trigger] cands@[j]) && near(cands@[j], ctxs@) ==> out@.contains(
                        span_of(cands@[j]),
                    ),
                forall|j: int| 0 <= j < out@.len() ==> collected(#[trigger] out@[j], cands@, ctxs@, google@),
            decreases cands.len() - b,
        {
            let c = cands[b];
            let ok = c.has_digit && !(c.all_digits && c.len > 8) && !c.after_plus;
            if ok && near_exec(c, ctxs) {
                let ghost before = out@;
                out.push((c.start, c.end));
                assert forall|j: int| 0 <= j < google@.len() implies out@.contains(#[trigger] google@[j]) by {
                    let k = choose|k: int| 0 <= k < before.len() && before[k] == google@[j];
                    assert(out@[k] == google@[j]);
                }
                assert forall|j: int|
                    0 <= j <= b && plausible(#[trigger] cands@[j]) && near(cands@[j], ctxs@) implies out@.contains(
                    span_of(cands@[j]),
                ) by {
                    if j == b {
                        assert(out@[out@.len() - 1] == span_of(cands@[j]));
                    } else {
                        let k = choose|k: int| 0 <= k < before.len() && before[k] == span_of(cands@[j]);
                        assert(out@[k] == span_of(cands@[j]));
                    }
                }
                assert forall|j: int| 0 <= j < out@.len() implies collected(#[trigger] out@[j], cands@, ctxs@, google@) by {
                    if j < before.len() {
                        assert(out@[j] == before[j]);
                    } else {
                        assert(out@[j] == span_of(cands@[b as int]));
                    }
                }
            }
            b += 1;
        }
    } else {
        assert forall|j: int|
            0 <= j < cands@.len() && plausible(#[trigger] cands@[j]) && near(cands@[j], ctxs@) implies out@.contains(
            span_of(cands@[j]),
        ) by {
            assert(!near(cands@[j], ctxs@));
        }
    }
    out
}

// ---------------------------------------------------------------- D2 sort

fn le_exec(a: (usize, usize), b: (usize, usize)) -> (r: bool)
    ensures
        r == le(a, b),
{
    a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1)
}

pub fn sort_spans(v: &Vec<(usize, usize)>) -> (out: Vec<(usize, usize)>)
    ensures
        sorted(out@),
        forall|x: (usize, usize)| #[trigger] out@.contains(x) <==> v@.contains(x),
{
    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut i: usize = 0;
    while i < v.len()
        invariant
            0 <= i <= v.len(),
            sorted(out@),
            forall|x: (usize, usize)| #[trigger] out@.contains(x) <==> v@.subrange(0, i as int).contains(x),
        decreases v.len() - i,
    {
        let x = v[i];
        let mut j: usize = 0;
        while j < out.len() && le_exec(out[j], x)
            invariant
                0 <= j <= out.len(),
                forall|k: int| 0 <= k < j ==> le(#[trigger] out@[k], x),
            decreases out.len() - j,
        {
            j += 1;
        }
        let ghost before = out@;
        out.insert(j, x);
        assert(out@ == before.insert(j as int, x));
        assert forall|a: int, b: int| 0 <= a < b < out@.len() implies le(#[trigger] out@[a], #[trigger] out@[b]) by {
            if a < j && b < j {
                assert(out@[a] == before[a] && out@[b] == before[b]);
            } else if a < j && b == j {
                assert(out@[a] == before[a]);
            } else if a < j {
                assert(out@[a] == before[a] && out@[b] == before[b - 1]);
            } else if a == j {
                // before[j] > x when it exists, and later ones are >= before[j]
                assert(out@[b] == before[b - 1]);
                assert(!le(before[j as int], x));
                if b - 1 > j {
                    assert(le(before[j as int], before[b - 1]));
                }
            } else {
                assert(out@[a] == before[a - 1] && out@[b] == before[b - 1]);
            }
        }
        assert forall|y: (usize, usize)| #[trigger] out@.contains(y) <==> v@.subrange(0, i + 1).contains(y) by {
            let vs = v@.subrange(0, i as int);
            let vs1 = v@.subrange(0, i + 1);
            if out@.contains(y) {
                let k = choose|k: int| 0 <= k < out@.len() && out@[k] == y;
                if k == j {
                    assert(vs1[i as int] == y);
                } else {
                    let k2 = if k < j { k } else { k - 1 };
                    assert(before[k2] == y);
                    assert(before.contains(y));
                    let m = choose|m: int| 0 <= m < vs.len() && vs[m] == y;
                    assert(vs1[m] == y);
                }
            }
            if vs1.contains(y) {
                let m = choose|m: int| 0 <= m < vs1.len() && vs1[m] == y;
                if m == i {
                    assert(out@[j as int] == y);
                } else {
                    assert(vs[m] == y);
                    assert(before.contains(y));
                    let k = choose|k: int| 0 <= k < before.len() && before[k] == y;
                    if k < j {
                        assert(out@[k] == y);
                    } else {
                        assert(out@[k + 1] == y);
                    }
                }
            }
        }
        i += 1;
    }
    assert(v@.subrange(0, v.len() as int) =~= v@);
    out
}

// ---------------------------------------------------------------- D3 merge

proof fn lemma_cover_push(s: Seq<(usize, usize)>, x: (usize, usize))
    ensures
        forall|p: int| #![auto] covered(p, s.push(x)) <==> (covered(p, s) || in_span(p, x)),
{
    let t = s.push(x);
    assert forall|p: int| covered(p, t) <==> (covered(p, s) || in_span(p, x)) by {
        if covered(p, t) {
            let m = choose|m: int| 0 <= m < t.len() && in_span(p, #[trigger] t[m]);
            if m < s.len() {
                assert(t[m] == s[m]);
            }
        }
        if covered(p, s) {
            let m = choose|m: int| 0 <= m < s.len() && in_span(p, #[trigger] s[m]);
            assert(t[m] == s[m]);
        }
        if in_span(p, x) {
            assert(t[s.len() as int] == x);
        }
    }
}

/// Extending the last span to cover `y`, which starts inside it.
proof fn lemma_cover_extend(s: Seq<(usize, usize)>, y: (usize, usize), e: usize)
    requires
        s.len() > 0,
        s.last().0 <= y.0 < s.last().1,
        e >= s.last().1,
        e >= y.1,
        e == s.last().1 || e == y.1,
    ensures
        forall|p: int| covered(p, s.update(s.len() - 1, (s.last().0, e))) <==> (covered(p, s) || in_span(p, y)),
{
    let k = s.len() - 1;
    let t = s.update(k, (s.last().0, e));
    assert forall|p: int| covered(p, t) <==> (covered(p, s) || in_span(p, y)) by {
        if covered(p, t) {
            let m = choose|m: int| 0 <= m < t.len() && in_span(p, #[trigger] t[m]);
            if m < k {
                assert(t[m] == s[m]);
            } else if p < s.last().1 {
                assert(in_span(p, s[k]));
            } else {
                assert(in_span(p, y));
            }
        }
        if covered(p, s) {
            let m = choose|m: int| 0 <= m < s.len() && in_span(p, #[trigger] s[m]);
            if m < k {
                assert(t[m] == s[m]);
            }
            assert(in_span(p, t[m]));
        }
        if in_span(p, y) {
            assert(in_span(p, t[k]));
        }
    }
}

pub fn merge(spans: &Vec<(usize, usize)>, Ghost(n): Ghost<nat>) -> (merged: Vec<(usize, usize)>)
    requires
        sorted(spans@),
        forall|i: int| 0 <= i < spans@.len() ==> wf_span(#[trigger] spans@[i], n),
    ensures
        sorted(merged@),
        disjoint(merged@),
        forall|m: int| 0 <= m < merged@.len() ==> wf_span(#[trigger] merged@[m], n),
        forall|s: int|
            0 <= s < spans@.len() ==> exists|m: int|
                0 <= m < merged@.len() && (#[trigger] merged@[m]).0 <= (#[trigger] spans@[s]).0 && spans@[s].1
                    <= merged@[m].1,
        forall|p: int| covered(p, merged@) <==> covered(p, spans@),
        spans@.len() == 0 ==> merged@.len() == 0,
{
    let mut merged: Vec<(usize, usize)> = Vec::new();
    let mut i: usize = 0;
    while i < spans.len()
        invariant
            0 <= i <= spans.len(),
            sorted(spans@),
            forall|k: int| 0 <= k < spans@.len() ==> wf_span(#[trigger] spans@[k], n),
            i == 0 <==> merged@.len() == 0,
            i > 0 ==> merged@.last().0 <= spans@[i - 1].0,
            sorted(merged@),
            disjoint(merged@),
            forall|m: int| 0 <= m < merged@.len() ==> wf_span(#[trigger] merged@[m], n),
            forall|s: int|
                0 <= s < i ==> exists|m: int|
                    0 <= m < merged@.len() && (#[trigger] merged@[m]).0 <= (#[trigger] spans@[s]).0 && spans@[s].1
                        <= merged@[m].1,
            forall|p: int| #![auto] covered(p, merged@) <==> covered(p, spans@.subrange(0, i as int)),
        decreases spans.len() - i,
    {
        let (start, end) = spans[i];
        let ghost before = merged@;
        let ghost sub = spans@.subrange(0, i as int);
        proof {
            assert(spans@.subrange(0, i + 1) =~= sub.push(spans@[i as int]));
            lemma_cover_push(sub, spans@[i as int]);
        }
        let n_m = merged.len();
        if n_m > 0 && start < merged[n_m - 1].1 {
            let last = merged[n_m - 1];
            let new_end = if last.1 >= end { last.1 } else { end };
            proof {
                assert(le(spans@[i - 1], spans@[i as int]));
                lemma_cover_extend(before, spans@[i as int], new_end);
            }
            merged.set(n_m - 1, (last.0, new_end));
            assert(merged@ == before.update(n_m - 1, (last.0, new_end)));
            assert forall|p: int| covered(p, merged@) <==> covered(p, spans@.subrange(0, i + 1)) by {
                assert(covered(p, merged@) <==> (covered(p, before) || in_span(p, spans@[i as int])));
            }
            assert forall|a: int, b: int| 0 <= a < b < merged@.len() implies le(#[trigger] merged@[a], #[trigger] merged@[b]) by {
                assert(le(before[a], before[b]));
            }
            assert forall|a: int, b: int| 0 <= a < b < merged@.len() implies (#[trigger] merged@[a]).1 <= (#[trigger] merged@[b]).0 by {
                assert(before[a].1 <= before[b].0);
            }
            assert forall|s: int| 0 <= s < i + 1 implies exists|m: int|
                0 <= m < merged@.len() && (#[trigger] merged@[m]).0 <= (#[trigger] spans@[s]).0 && spans@[s].1
                    <= merged@[m].1 by {
                if s == i {
                    assert(merged@[n_m - 1].0 <= spans@[s].0 && spans@[s].1 <= merged@[n_m - 1].1);
                } else {
                    let m = choose|m: int|
                        0 <= m < before.len() && (#[trigger] before[m]).0 <= (#[trigger] spans@[s]).0 && spans@[s].1
                            <= before[m].1;
                    assert(merged@[m].0 <= spans@[s].0 && spans@[s].1 <= merged@[m].1);
                }
            }
        } else {
            proof {
                lemma_cover_push(before, (start, end));
            }
            merged.push((start, end));
            assert(merged@ == before.push((start, end)));
            assert forall|p: int| covered(p, merged@) <==> covered(p, spans@.subrange(0, i + 1)) by {
                assert(covered(p, merged@) <==> (covered(p, before) || in_span(p, spans@[i as int])));
            }
            assert forall|a: int, b: int| 0 <= a < b < merged@.len() implies (#[trigger] merged@[a]).1 <= (#[trigger] merged@[b]).0 by {
                if b == n_m {
                    if a < n_m - 1 {
                        assert(before[a].1 <= before[n_m - 1].0);
                    }
                } else {
                    assert(before[a].1 <= before[b].0);
                }
            }
            assert forall|a: int, b: int| 0 <= a < b < merged@.len() implies le(#[trigger] merged@[a], #[trigger] merged@[b]) by {
                if b == n_m {
                    assert(merged@[a].0 < merged@[a].1);
                    assert(merged@[a].1 <= merged@[b].0);
                } else {
                    assert(le(before[a], before[b]));
                }
            }
            assert forall|s: int| 0 <= s < i + 1 implies exists|m: int|
                0 <= m < merged@.len() && (#[trigger] merged@[m]).0 <= (#[trigger] spans@[s]).0 && spans@[s].1
                    <= merged@[m].1 by {
                if s == i {
                    assert(merged@[n_m as int] == spans@[s]);
                } else {
                    let m = choose|m: int|
                        0 <= m < before.len() && (#[trigger] before[m]).0 <= (#[trigger] spans@[s]).0 && spans@[s].1
                            <= before[m].1;
                    assert(merged@[m] == before[m]);
                }
            }
        }
        i += 1;
    }
    assert(spans@.subrange(0, spans.len() as int) =~= spans@);
    merged
}

// ---------------------------------------------------------------- D4 redact

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Item {
    Keep(usize),
    Ph,
}

pub open spec fn keeps(out: Seq<Item>, p: int) -> bool {
    exists|i: int| 0 <= i < out.len() && #[trigger] out[i] == Item::Keep(p as usize)
}

pub open spec fn keep_ok(out: Seq<Item>, bound: int, merged: Seq<(usize, usize)>) -> bool {
    &&& forall|i: int| 0 <= i < out.len() && (#[trigger] out[i]) is Keep ==> (out[i]->Keep_0 as int) < bound
        && !covered(out[i]->Keep_0 as int, merged)
    &&& forall|i: int, j: int|
        0 <= i < j < out.len() && (#[trigger] out[i]) is Keep && (#[trigger] out[j]) is Keep ==> out[i]->Keep_0
            < out[j]->Keep_0
}

pub open spec fn keeps_all(out: Seq<Item>, bound: int, merged: Seq<(usize, usize)>) -> bool {
    forall|q: int| 0 <= q < bound && !covered(q, merged) ==> keeps(out, q)
}

/// Copies the bytes [from, to), all outside the merged spans.
fn copy_range(out: &mut Vec<Item>, from: usize, to: usize, Ghost(merged): Ghost<Seq<(usize, usize)>>)
    requires
        from <= to,
        forall|q: int| from <= q < to ==> !covered(q, merged),
        keep_ok(old(out)@, from as int, merged),
        keeps_all(old(out)@, from as int, merged),
    ensures
        keep_ok(final(out)@, to as int, merged),
        keeps_all(final(out)@, to as int, merged),
        final(out)@.len() == old(out)@.len() + (to - from),
        forall|i: int| 0 <= i < old(out)@.len() ==> final(out)@[i] == old(out)@[i],
        forall|i: int| old(out)@.len() <= i < final(out)@.len() ==> #[trigger] final(out)@[i] == Item::Keep((from + i - old(out)@.len()) as usize),
{
    let ghost start = out@;
    let mut p = from;
    while p < to
        invariant
            from <= p <= to,
            forall|q: int| from <= q < to ==> !covered(q, merged),
            keep_ok(out@, p as int, merged),
            keeps_all(out@, p as int, merged),
            out@.len() == start.len() + (p - from),
            forall|i: int| #![auto] 0 <= i < start.len() ==> out@[i] == start[i],
            forall|i: int| start.len() <= i < out@.len() ==> #[trigger] out@[i] == Item::Keep((from + i - start.len()) as usize),
        decreases to - p,
    {
        let ghost before = out@;
        out.push(Item::Keep(p));
        assert forall|q: int| #![auto] 0 <= q < p + 1 && !covered(q, merged) implies keeps(out@, q) by {
            if q == p {
                assert(out@[out@.len() - 1] == Item::Keep(p));
            } else {
                let i = choose|i: int| 0 <= i < before.len() && #[trigger] before[i] == Item::Keep(q as usize);
                assert(out@[i] == before[i]);
            }
        }
        assert forall|i: int| 0 <= i < out@.len() && (#[trigger] out@[i]) is Keep implies (out@[i]->Keep_0 as int) < p + 1
            && !covered(out@[i]->Keep_0 as int, merged) by {
            if i < before.len() {
                assert(out@[i] == before[i]);
            }
        }
        assert forall|i: int, j: int|
            0 <= i < j < out@.len() && (#[trigger] out@[i]) is Keep && (#[trigger] out@[j]) is Keep implies out@[i]->Keep_0
                < out@[j]->Keep_0 by {
            assert(out@[i] == before[i]);
            if j < before.len() {
                assert(out@[j] == before[j]);
            }
        }
        p += 1;
    }
}

pub fn redact(n: usize, merged: &Vec<(usize, usize)>) -> (out: Vec<Item>)
    requires
        disjoint(merged@),
        forall|m: int| 0 <= m < merged@.len() ==> wf_span(#[trigger] merged@[m], n as nat),
    ensures
        keep_ok(out@, n as int, merged@),
        keeps_all(out@, n as int, merged@),
        merged@.len() == 0 ==> out@.len() == n && forall|i: int| 0 <= i < n ==> #[trigger] out@[i] == Item::Keep(i as usize),
{
    let mut out: Vec<Item> = Vec::new();
    let mut at: usize = 0;
    let mut m: usize = 0;
    while m < merged.len()
        invariant
            0 <= m <= merged.len(),
            disjoint(merged@),
            forall|k: int| 0 <= k < merged@.len() ==> wf_span(#[trigger] merged@[k], n as nat),
            at <= n,
            m == 0 ==> at == 0,
            0 < m ==> at == merged@[m - 1].1,
            forall|k: int| m <= k < merged@.len() ==> at <= (#[trigger] merged@[k]).0,
            keep_ok(out@, at as int, merged@),
            keeps_all(out@, at as int, merged@),
            merged@.len() == 0 ==> out@.len() == 0,
        decreases merged.len() - m,
    {
        let (stop, next) = merged[m];
        assert forall|q: int| at <= q < stop implies !covered(q, merged@) by {
            if covered(q, merged@) {
                let k = choose|k: int| 0 <= k < merged@.len() && in_span(q, #[trigger] merged@[k]);
                if k < m {
                    if k < m - 1 {
                        assert(merged@[k].1 <= merged@[m - 1].0);
                    }
                } else if k > m {
                    assert(merged@[m as int].1 <= merged@[k].0);
                }
            }
        }
        copy_range(&mut out, at, stop, Ghost(merged@));
        let ghost before = out@;
        out.push(Item::Ph);
        at = next;
        proof {
            assert forall|k: int| m + 1 <= k < merged@.len() implies at <= (#[trigger] merged@[k]).0 by {
                assert(merged@[m as int].1 <= merged@[k].0);
            }
            assert forall|q: int| 0 <= q < at && !covered(q, merged@) implies keeps(out@, q) by {
                assert(!in_span(q, merged@[m as int]));
                assert(keeps(before, q));
                let i = choose|i: int| 0 <= i < before.len() && #[trigger] before[i] == Item::Keep(q as usize);
                assert(out@[i] == before[i]);
            }
            assert forall|i: int| 0 <= i < out@.len() && (#[trigger] out@[i]) is Keep implies (out@[i]->Keep_0 as int) < at
                && !covered(out@[i]->Keep_0 as int, merged@) by {
                assert(out@[i] == before[i]);
            }
            assert forall|i: int, j: int|
                0 <= i < j < out@.len() && (#[trigger] out@[i]) is Keep && (#[trigger] out@[j]) is Keep implies out@[i]->Keep_0
                    < out@[j]->Keep_0 by {
                assert(out@[i] == before[i] && out@[j] == before[j]);
            }
        }
        m += 1;
    }
    assert forall|q: int| at <= q < n implies !covered(q, merged@) by {
        if covered(q, merged@) {
            let k = choose|k: int| 0 <= k < merged@.len() && in_span(q, #[trigger] merged@[k]);
            if k < m - 1 {
                assert(merged@[k].1 <= merged@[m - 1].0);
            }
        }
    }
    copy_range(&mut out, at, n, Ghost(merged@));
    out
}

// ---------------------------------------------------------------- D5

pub fn otp_spans(n: usize, cands: &Vec<Cand>, ctxs: &Vec<Ctx>, google: &Vec<(usize, usize)>) -> (merged: Vec<(usize, usize)>)
    requires
        wf(cands@, google@, n as nat),
    ensures
        disjoint(merged@),
        forall|m: int| 0 <= m < merged@.len() ==> wf_span(#[trigger] merged@[m], n as nat),
        forall|i: int|
             0 <= i < cands@.len() && plausible(#[trigger] cands@[i]) && near(cands@[i], ctxs@) ==> forall|p: int|
                cands@[i].start <= p < cands@[i].end ==> #[trigger] covered(p, merged@),
        forall|i: int| 0 <= i < google@.len() ==> forall|p: int| (#[trigger] google@[i]).0 <= p < google@[i].1 ==> #[trigger] covered(p, merged@),
        forall|p: int| #[trigger] covered(p, merged@) ==> exists|s: (usize, usize)| #![auto]
            collected(s, cands@, ctxs@, google@) && in_span(p, s),
        cands@.len() == 0 && google@.len() == 0 ==> merged@.len() == 0,
{
    let spans = collect(cands, ctxs, google);
    let sorted_spans = sort_spans(&spans);
    assert forall|i: int| 0 <= i < sorted_spans@.len() implies wf_span(#[trigger] sorted_spans@[i], n as nat) by {
        assert(sorted_spans@.contains(sorted_spans@[i]));
        assert(spans@.contains(sorted_spans@[i]));
        let j = choose|j: int| 0 <= j < spans@.len() && spans@[j] == sorted_spans@[i];
        assert(collected(spans@[j], cands@, ctxs@, google@));
        if google@.contains(spans@[j]) {
            let g = choose|g: int| 0 <= g < google@.len() && google@[g] == spans@[j];
            assert(wf_span(google@[g], n as nat));
        } else {
            let c = choose|c: int| 0 <= c < cands@.len() && plausible(#[trigger] cands@[c]) && near(cands@[c], ctxs@) && span_of(cands@[c]) == spans@[j];
            assert(wf_span(span_of(cands@[c]), n as nat));
        }
    }
    let merged = merge(&sorted_spans, Ghost(n as nat));
    proof {
        // A collected span (in spans, so in sorted_spans) is inside one merged span.
        assert forall|s: (usize, usize)| #[trigger] spans@.contains(s) implies forall|p: int| s.0 <= p < s.1 ==> covered(p, merged@) by {
            assert(sorted_spans@.contains(s));
            let k = choose|k: int| 0 <= k < sorted_spans@.len() && sorted_spans@[k] == s;
            assert forall|p: int| s.0 <= p < s.1 implies covered(p, merged@) by {
                assert(in_span(p, sorted_spans@[k]));
                assert(covered(p, sorted_spans@));
            }
        }
        assert forall|i: int|
             0 <= i < cands@.len() && plausible(#[trigger] cands@[i]) && near(cands@[i], ctxs@) implies forall|p: int|
                cands@[i].start <= p < cands@[i].end ==> #[trigger] covered(p, merged@) by {
            assert(spans@.contains(span_of(cands@[i])));
        }
        assert forall|i: int| 0 <= i < google@.len() implies forall|p: int| (#[trigger] google@[i]).0 <= p < google@[i].1 ==> #[trigger] covered(p, merged@) by {
            assert(spans@.contains(google@[i]));
        }
        assert forall|p: int| #[trigger] covered(p, merged@) implies exists|s: (usize, usize)| #![auto]
            collected(s, cands@, ctxs@, google@) && in_span(p, s) by {
            assert(covered(p, sorted_spans@));
            let k = choose|k: int| 0 <= k < sorted_spans@.len() && in_span(p, #[trigger] sorted_spans@[k]);
            assert(sorted_spans@.contains(sorted_spans@[k]));
            assert(spans@.contains(sorted_spans@[k]));
            let j = choose|j: int| 0 <= j < spans@.len() && spans@[j] == sorted_spans@[k];
            assert(collected(spans@[j], cands@, ctxs@, google@));
        }
        if cands@.len() == 0 && google@.len() == 0 {
            if spans@.len() > 0 {
                assert(collected(spans@[0], cands@, ctxs@, google@));
            }
            if sorted_spans@.len() > 0 {
                assert(sorted_spans@.contains(sorted_spans@[0]));
            }
        }
    }
    merged
}

/// `redact_otps`. D5: no byte of a selectable candidate is kept.
pub fn redact_otps(n: usize, cands: &Vec<Cand>, ctxs: &Vec<Ctx>, google: &Vec<(usize, usize)>) -> (out: Vec<Item>)
    requires
        wf(cands@, google@, n as nat),
    ensures
        forall|i: int, j: int|
             0 <= i < cands@.len() && plausible(#[trigger] cands@[i]) && near(cands@[i], ctxs@) && 0 <= j
                < out@.len() && (#[trigger] out@[j]) is Keep ==> !(cands@[i].start <= out@[j]->Keep_0 < cands@[i].end),
        // Outside every collected span the text is kept, in order.
        forall|p: int| #![auto] 0 <= p < n && !(exists|s: (usize, usize)| #![auto] collected(s, cands@, ctxs@, google@) && in_span(p, s)) ==> keeps(out@, p),
        forall|i: int, j: int|
            0 <= i < j < out@.len() && (#[trigger] out@[i]) is Keep && (#[trigger] out@[j]) is Keep ==> out@[i]->Keep_0
                < out@[j]->Keep_0,
        cands@.len() == 0 && google@.len() == 0 ==> out@.len() == n && forall|i: int| 0 <= i < n ==> #[trigger] out@[i] == Item::Keep(i as usize),
{
    let merged = otp_spans(n, cands, ctxs, google);
    let out = redact(n, &merged);
    proof {
        assert forall|i: int, j: int|
             0 <= i < cands@.len() && plausible(#[trigger] cands@[i]) && near(cands@[i], ctxs@) && 0 <= j
                < out@.len() && (#[trigger] out@[j]) is Keep implies !(cands@[i].start <= out@[j]->Keep_0 < cands@[i].end) by {
            let p = out@[j]->Keep_0 as int;
            if cands@[i].start <= p < cands@[i].end {
                assert(covered(p, merged@));
            }
        }
        assert forall|p: int| #![auto] 0 <= p < n && !(exists|s: (usize, usize)| #![auto] collected(s, cands@, ctxs@, google@) && in_span(p, s)) implies keeps(out@, p) by {
            if covered(p, merged@) {
                assert(exists|s: (usize, usize)| #![auto] collected(s, cands@, ctxs@, google@) && in_span(p, s));
            }
        }
    }
    out
}

/// D5 as stated in the header: what `parse_otp` can select (either mode) is plausible,
/// so `redact_otps` (ensures, first clause) keeps none of its bytes.
proof fn lemma_selectable_is_plausible(c: Cand, fixed: bool)
    ensures
        selectable(c, fixed) ==> plausible(c),
{
}

// ---------------------------------------------------------------- witness

/// "Your security code is 682149." (n = 29): bytes 22..28 become one placeholder.
fn witness_code_is_hidden() {
    let mut cands: Vec<Cand> = Vec::new();
    cands.push(Cand { start: 22, end: 28, len: 6, has_digit: true, all_digits: true, after_plus: false, in_url: false, currency: false, numeric_join: false });
    let mut ctxs: Vec<Ctx> = Vec::new();
    ctxs.push(Ctx { start: 5, end: 13 });
    let google: Vec<(usize, usize)> = Vec::new();
    assert(dist(cands@[0], ctxs@[0]) == 9);
    assert(selectable(cands@[0], false) && near(cands@[0], ctxs@));
    let out = redact_otps(29, &cands, &ctxs, &google);
    // Byte 25 (inside the code) is not kept; byte 3 (outside every span) is.
    assert(!keeps(out@, 25)) by {
        if keeps(out@, 25) {
            let j = choose|j: int| 0 <= j < out@.len() && #[trigger] out@[j] == Item::Keep(25);
            assert(out@[j] is Keep);
        }
    }
    assert(keeps(out@, 3)) by {
        assert forall|s: (usize, usize)| #![auto] collected(s, cands@, ctxs@, google@) implies !in_span(3, s) by {
            if !google@.contains(s) {
                let i = choose|i: int| 0 <= i < cands@.len() && plausible(#[trigger] cands@[i]) && near(cands@[i], ctxs@) && span_of(cands@[i]) == s;
                assert(i == 0);
            }
        }
    }
}

fn main() {
}

} // verus!
