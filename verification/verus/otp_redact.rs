// OTP redaction: `otp_spans` (collect, sort, merge) and `redact_otps`.
//
// Production: crates/lucina-phone-core/src/otp.rs, branch lucina-rs dd7ec9e (F5: 52b9140, merged
// from otp-fixes). Before F5: lucina-rs c357646.
//   collect        <-> `candidate_spans`: every GOOGLE match, then (when there is a
//                      context word) every CODES match that is `plausible` and within
//                      MAX_DISTANCE of some context
//   map_append     <-> F5: `candidate_spans(&view.text)` mapped by `View::original`
//                      (`mapped`), added when `view.text != text` (`differs`)
//   sort_spans     <-> `spans.sort_unstable()` (lexicographic on (start, end))
//   merge          <-> the merge loop (`start < last.1` extends, else push)
//   redact         <-> `redact_otps` (text before each span, then REDACTED, then the tail)
//   otp_spans / redact_otps          the F5 code: raw spans + mapped view spans
//   otp_spans_raw / redact_raw       one text only: the code before F5, and F5's raw half
// Differential test: verification/verus/diff/otp_redact.rs.
//
// Modelled: the span logic exactly, over the tokenizer's output: on the raw text
// (cands, ctxs, google) and on the view `parse_otp` reads (vcands, vctxs, vgoogle:
// digit-normalized, URL-stripped), with `View::origin` (`origin`: for each view byte the
// original char's byte range, in order). Same Cand / Ctx shape as otp_extract.rs (wf:
// start < end, 4 <= len <= 10, spans inside their text). When the view equals the text
// the caller passes the same tokens and the identity origin (`wf_f5`). The output is a
// sequence of items, Keep(p) for the input byte at p and Ph for one REDACTED
// placeholder: what the code concatenates. Abstracted: the regexes, UTF-8 (spans are
// on char boundaries because the regexes match whole chars and `origin` maps whole
// chars; not modelled).
//
// Proven:
//   D1 collect: every Google span and every candidate that passes the selection guard
//      against some context is collected, and nothing else is.
//   D2 sort: sorted, same elements.
//   D3 merge: sorted, pairwise disjoint, every input span lies inside one merged span,
//      and a position is covered by the merged spans iff it is covered by an input span.
//   D4 redact: no Keep(p) for a covered p; every uncovered position is kept, in order
//      (text outside spans unchanged); one Ph per merged span; identity with no spans.
//   D5 (F5) every candidate `parse_otp` can select (in its view: plausible, with or
//      without F4's amount skip, and within MAX_DISTANCE of a context) has none of its
//      original bytes kept, nor has any raw-text candidate near a context (the old
//      guarantee: F5 only adds spans); bytes outside every raw and mapped span are kept.
//   witness_f5_unicode_digits_hidden: a code in Arabic-Indic digits that the raw text
//      does not tokenize is redacted through the view.
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

/// `otp_spans` before F5 (lucina-rs c357646), and F5's `candidate_spans` + sort + merge
/// on one text.
pub fn otp_spans_raw(n: usize, cands: &Vec<Cand>, ctxs: &Vec<Ctx>, google: &Vec<(usize, usize)>) -> (merged: Vec<(usize, usize)>)
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

/// `redact_otps` before F5: D5 on one text (no byte of a selectable candidate is kept).
pub fn redact_raw(n: usize, cands: &Vec<Cand>, ctxs: &Vec<Ctx>, google: &Vec<(usize, usize)>) -> (out: Vec<Item>)
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
    let merged = otp_spans_raw(n, cands, ctxs, google);
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

// ---------------------------------------------------------------- F5: the superset

/// `View::original`: the original bytes of the view's bytes s.0..s.1.
pub open spec fn mapped(s: (usize, usize), origin: Seq<(usize, usize)>) -> (usize, usize) {
    (origin[s.0 as int].0, origin[s.1 - 1].1)
}

/// `View::origin`: one original char range per view byte, in order, inside the text.
pub open spec fn wf_origin(origin: Seq<(usize, usize)>, m: nat, n: nat) -> bool {
    &&& origin.len() == m
    &&& forall|j: int| 0 <= j < m ==> (#[trigger] origin[j]).0 < origin[j].1 <= n
    &&& forall|i: int, j: int| 0 <= i <= j < m ==> (#[trigger] origin[i]).0 <= (#[trigger] origin[j]).0
}

pub open spec fn identity_origin(origin: Seq<(usize, usize)>, n: nat) -> bool {
    origin.len() == n && forall|j: int| 0 <= j < n ==> #[trigger] origin[j] == (j as usize, (j + 1) as usize)
}

/// What F5 redaction may cover: a raw-text span or a mapped view span.
pub open spec fn collected_f5(
    s: (usize, usize),
    cands: Seq<Cand>, ctxs: Seq<Ctx>, google: Seq<(usize, usize)>,
    vcands: Seq<Cand>, vctxs: Seq<Ctx>, vgoogle: Seq<(usize, usize)>,
    origin: Seq<(usize, usize)>, differs: bool,
) -> bool {
    collected(s, cands, ctxs, google) || (differs && exists|v: (usize, usize)| #![auto]
        collected(v, vcands, vctxs, vgoogle) && mapped(v, origin) == s)
}

/// The view's span plus the original text: what `otp_spans` must accept from the caller.
pub open spec fn wf_f5(
    n: nat, cands: Seq<Cand>, ctxs: Seq<Ctx>, google: Seq<(usize, usize)>,
    m: nat, vcands: Seq<Cand>, vctxs: Seq<Ctx>, vgoogle: Seq<(usize, usize)>,
    origin: Seq<(usize, usize)>, differs: bool,
) -> bool {
    &&& wf(cands, google, n)
    &&& wf(vcands, vgoogle, m)
    &&& wf_origin(origin, m, n)
    // `if view.text != text`: otherwise the view is the text.
    &&& !differs ==> vcands == cands && vctxs == ctxs && vgoogle == google && m == n && identity_origin(origin, n)
}

proof fn lemma_mapped_wf(s: (usize, usize), origin: Seq<(usize, usize)>, m: nat, n: nat)
    requires
        wf_span(s, m),
        wf_origin(origin, m, n),
    ensures
        wf_span(mapped(s, origin), n),
        forall|p: int| s.0 <= p < s.1 ==> mapped(s, origin).0 <= (#[trigger] origin[p]).0,
{
    assert(origin[s.0 as int].0 <= origin[s.1 - 1].0);
}

/// `spans.extend(candidate_spans(&view.text).map(|(s, e)| view.original(s, e)))`.
fn map_append(spans: &mut Vec<(usize, usize)>, v: &Vec<(usize, usize)>, origin: &Vec<(usize, usize)>, Ghost(m): Ghost<nat>, Ghost(n): Ghost<nat>)
    requires
        forall|j: int| 0 <= j < v@.len() ==> wf_span(#[trigger] v@[j], m),
        wf_origin(origin@, m, n),
    ensures
        final(spans)@.len() == old(spans)@.len() + v@.len(),
        forall|i: int| 0 <= i < old(spans)@.len() ==> #[trigger] final(spans)@[i] == old(spans)@[i],
        forall|j: int| 0 <= j < v@.len() ==> #[trigger] final(spans)@[old(spans)@.len() + j] == mapped(v@[j], origin@),
{
    let ghost start = spans@;
    let mut j: usize = 0;
    while j < v.len()
        invariant
            0 <= j <= v.len(),
            forall|k: int| 0 <= k < v@.len() ==> wf_span(#[trigger] v@[k], m),
            wf_origin(origin@, m, n),
            spans@.len() == start.len() + j,
            forall|i: int| 0 <= i < start.len() ==> #[trigger] spans@[i] == start[i],
            forall|k: int| 0 <= k < j ==> #[trigger] spans@[start.len() + k] == mapped(v@[k], origin@),
        decreases v.len() - j,
    {
        let (s, e) = v[j];
        assert(wf_span(v@[j as int], m));
        let a = origin[s].0;
        let b = origin[e - 1].1;
        spans.push((a, b));
        j += 1;
    }
}

/// `otp_spans` on lucina-rs dd7ec9e (F5, 52b9140): the raw-text candidate spans, plus (when the
/// view differs from the text) the view's candidate spans mapped back; sorted, merged.
pub fn otp_spans(
    n: usize, cands: &Vec<Cand>, ctxs: &Vec<Ctx>, google: &Vec<(usize, usize)>,
    Ghost(m): Ghost<nat>, vcands: &Vec<Cand>, vctxs: &Vec<Ctx>, vgoogle: &Vec<(usize, usize)>,
    origin: &Vec<(usize, usize)>, differs: bool,
) -> (merged: Vec<(usize, usize)>)
    requires
        wf_f5(n as nat, cands@, ctxs@, google@, m, vcands@, vctxs@, vgoogle@, origin@, differs),
    ensures
        disjoint(merged@),
        forall|k: int| 0 <= k < merged@.len() ==> wf_span(#[trigger] merged@[k], n as nat),
        // the raw-text guarantee, as before F5
        forall|i: int|
            0 <= i < cands@.len() && plausible(#[trigger] cands@[i]) && near(cands@[i], ctxs@) ==> forall|p: int|
                cands@[i].start <= p < cands@[i].end ==> #[trigger] covered(p, merged@),
        // F5: every candidate parse_otp can select, in original bytes
        forall|i: int|
            0 <= i < vcands@.len() && plausible(#[trigger] vcands@[i]) && near(vcands@[i], vctxs@) ==> forall|p: int|
                mapped(span_of(vcands@[i]), origin@).0 <= p < mapped(span_of(vcands@[i]), origin@).1 ==> #[trigger] covered(p, merged@),
        forall|i: int| 0 <= i < vgoogle@.len() ==> forall|p: int|
            mapped(#[trigger] vgoogle@[i], origin@).0 <= p < mapped(vgoogle@[i], origin@).1 ==> #[trigger] covered(p, merged@),
        forall|i: int| 0 <= i < google@.len() ==> forall|p: int| (#[trigger] google@[i]).0 <= p < google@[i].1 ==> #[trigger] covered(p, merged@),
        // nothing else
        forall|p: int| #[trigger] covered(p, merged@) ==> exists|s: (usize, usize)| #![auto]
            collected_f5(s, cands@, ctxs@, google@, vcands@, vctxs@, vgoogle@, origin@, differs) && in_span(p, s),
{
    let mut spans = collect(cands, ctxs, google);
    let ghost raw = spans@;
    let ghost mut v: Seq<(usize, usize)> = Seq::empty();
    if differs {
        let vs = collect(vcands, vctxs, vgoogle);
        proof {
            v = vs@;
            assert forall|j: int| 0 <= j < vs@.len() implies wf_span(#[trigger] vs@[j], m) by {
                assert(collected(vs@[j], vcands@, vctxs@, vgoogle@));
                if vgoogle@.contains(vs@[j]) {
                    let g = choose|g: int| 0 <= g < vgoogle@.len() && vgoogle@[g] == vs@[j];
                    assert(wf_span(vgoogle@[g], m));
                } else {
                    let c = choose|c: int| 0 <= c < vcands@.len() && plausible(#[trigger] vcands@[c]) && near(vcands@[c], vctxs@) && span_of(vcands@[c]) == vs@[j];
                    assert(wf_span(span_of(vcands@[c]), m));
                }
            }
        }
        map_append(&mut spans, &vs, origin, Ghost(m), Ghost(n as nat));
        proof {
            // the view's collected spans, mapped, are in spans
            assert forall|x: (usize, usize)| #[trigger] v.contains(x) implies spans@.contains(mapped(x, origin@)) by {
                let j = choose|j: int| 0 <= j < v.len() && v[j] == x;
                assert(spans@[raw.len() + j] == mapped(x, origin@));
            }
        }
    }
    proof {
        assert forall|x: (usize, usize)| #[trigger] raw.contains(x) implies spans@.contains(x) by {
            let j = choose|j: int| 0 <= j < raw.len() && raw[j] == x;
            assert(spans@[j] == x);
        }
        // every span is a raw collected one or a mapped view one
        assert forall|j: int| 0 <= j < spans@.len() implies
            collected_f5(#[trigger] spans@[j], cands@, ctxs@, google@, vcands@, vctxs@, vgoogle@, origin@, differs)
            && wf_span(spans@[j], n as nat) by {
            if j < raw.len() {
                assert(spans@[j] == raw[j]);
                assert(collected(raw[j], cands@, ctxs@, google@));
                if google@.contains(raw[j]) {
                    let g = choose|g: int| 0 <= g < google@.len() && google@[g] == raw[j];
                    assert(wf_span(google@[g], n as nat));
                } else {
                    let c = choose|c: int| 0 <= c < cands@.len() && plausible(#[trigger] cands@[c]) && near(cands@[c], ctxs@) && span_of(cands@[c]) == raw[j];
                    assert(wf_span(span_of(cands@[c]), n as nat));
                }
            } else {
                let k = j - raw.len();
                assert(differs);
                assert(spans@[raw.len() + k] == mapped(v[k], origin@));
                assert(collected(v[k], vcands@, vctxs@, vgoogle@));
                assert(wf_span(v[k], m)) by {
                    if vgoogle@.contains(v[k]) {
                        let g = choose|g: int| 0 <= g < vgoogle@.len() && vgoogle@[g] == v[k];
                        assert(wf_span(vgoogle@[g], m));
                    } else {
                        let c = choose|c: int| 0 <= c < vcands@.len() && plausible(#[trigger] vcands@[c]) && near(vcands@[c], vctxs@) && span_of(vcands@[c]) == v[k];
                        assert(wf_span(span_of(vcands@[c]), m));
                    }
                }
                lemma_mapped_wf(v[k], origin@, m, n as nat);
            }
        }
    }
    let sorted_spans = sort_spans(&spans);
    proof {
        assert forall|i: int| 0 <= i < sorted_spans@.len() implies wf_span(#[trigger] sorted_spans@[i], n as nat) by {
            assert(sorted_spans@.contains(sorted_spans@[i]));
            let j = choose|j: int| 0 <= j < spans@.len() && spans@[j] == sorted_spans@[i];
            assert(wf_span(spans@[j], n as nat));
        }
    }
    let merged = merge(&sorted_spans, Ghost(n as nat));
    proof {
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
            assert(raw.contains(span_of(cands@[i])));
            assert(spans@.contains(span_of(cands@[i])));
        }
        assert forall|i: int| 0 <= i < google@.len() implies forall|p: int| (#[trigger] google@[i]).0 <= p < google@[i].1 ==> #[trigger] covered(p, merged@) by {
            assert(raw.contains(google@[i]));
            assert(spans@.contains(google@[i]));
        }
        assert forall|i: int|
            0 <= i < vcands@.len() && plausible(#[trigger] vcands@[i]) && near(vcands@[i], vctxs@) implies forall|p: int|
                mapped(span_of(vcands@[i]), origin@).0 <= p < mapped(span_of(vcands@[i]), origin@).1 ==> #[trigger] covered(p, merged@) by {
            let x = span_of(vcands@[i]);
            if differs {
                assert(v.contains(x));
                assert(spans@.contains(mapped(x, origin@)));
            } else {
                assert(cands@[i] == vcands@[i]);
                assert(origin@[x.0 as int] == (x.0, (x.0 + 1) as usize));
                assert(origin@[x.1 - 1] == ((x.1 - 1) as usize, x.1));
                assert(mapped(x, origin@) == x);
                assert(raw.contains(x));
                assert(spans@.contains(x));
            }
        }
        assert forall|i: int| 0 <= i < vgoogle@.len() implies forall|p: int|
            mapped(#[trigger] vgoogle@[i], origin@).0 <= p < mapped(vgoogle@[i], origin@).1 ==> #[trigger] covered(p, merged@) by {
            let x = vgoogle@[i];
            if differs {
                assert(v.contains(x));
                assert(spans@.contains(mapped(x, origin@)));
            } else {
                assert(google@[i] == x);
                assert(origin@[x.0 as int] == (x.0, (x.0 + 1) as usize));
                assert(origin@[x.1 - 1] == ((x.1 - 1) as usize, x.1));
                assert(mapped(x, origin@) == x);
                assert(raw.contains(x));
                assert(spans@.contains(x));
            }
        }
        assert forall|p: int| #[trigger] covered(p, merged@) implies exists|s: (usize, usize)| #![auto]
            collected_f5(s, cands@, ctxs@, google@, vcands@, vctxs@, vgoogle@, origin@, differs) && in_span(p, s) by {
            assert(covered(p, sorted_spans@));
            let k = choose|k: int| 0 <= k < sorted_spans@.len() && in_span(p, #[trigger] sorted_spans@[k]);
            assert(sorted_spans@.contains(sorted_spans@[k]));
            let j = choose|j: int| 0 <= j < spans@.len() && spans@[j] == sorted_spans@[k];
            assert(collected_f5(spans@[j], cands@, ctxs@, google@, vcands@, vctxs@, vgoogle@, origin@, differs));
        }
    }
    merged
}

/// `redact_otps` on lucina-rs dd7ec9e (F5). D5: no original byte of a code `parse_otp` can
/// return is kept; bytes outside every raw and mapped span are kept, in order.
pub fn redact_otps(
    n: usize, cands: &Vec<Cand>, ctxs: &Vec<Ctx>, google: &Vec<(usize, usize)>,
    Ghost(m): Ghost<nat>, vcands: &Vec<Cand>, vctxs: &Vec<Ctx>, vgoogle: &Vec<(usize, usize)>,
    origin: &Vec<(usize, usize)>, differs: bool,
) -> (out: Vec<Item>)
    requires
        wf_f5(n as nat, cands@, ctxs@, google@, m, vcands@, vctxs@, vgoogle@, origin@, differs),
    ensures
        forall|i: int, j: int|
            0 <= i < vcands@.len() && plausible(#[trigger] vcands@[i]) && near(vcands@[i], vctxs@) && 0 <= j
                < out@.len() && (#[trigger] out@[j]) is Keep ==> !(mapped(span_of(vcands@[i]), origin@).0 <= out@[j]->Keep_0
                < mapped(span_of(vcands@[i]), origin@).1),
        forall|i: int, j: int|
            0 <= i < cands@.len() && plausible(#[trigger] cands@[i]) && near(cands@[i], ctxs@) && 0 <= j
                < out@.len() && (#[trigger] out@[j]) is Keep ==> !(cands@[i].start <= out@[j]->Keep_0 < cands@[i].end),
        forall|p: int| #![auto] 0 <= p < n && !(exists|s: (usize, usize)| #![auto]
            collected_f5(s, cands@, ctxs@, google@, vcands@, vctxs@, vgoogle@, origin@, differs) && in_span(p, s)) ==> keeps(out@, p),
        forall|i: int, j: int|
            0 <= i < j < out@.len() && (#[trigger] out@[i]) is Keep && (#[trigger] out@[j]) is Keep ==> out@[i]->Keep_0
                < out@[j]->Keep_0,
{
    let merged = otp_spans(n, cands, ctxs, google, Ghost(m), vcands, vctxs, vgoogle, origin, differs);
    let out = redact(n, &merged);
    proof {
        assert forall|i: int, j: int|
            0 <= i < vcands@.len() && plausible(#[trigger] vcands@[i]) && near(vcands@[i], vctxs@) && 0 <= j
                < out@.len() && (#[trigger] out@[j]) is Keep implies !(mapped(span_of(vcands@[i]), origin@).0 <= out@[j]->Keep_0
                < mapped(span_of(vcands@[i]), origin@).1) by {
            let p = out@[j]->Keep_0 as int;
            if mapped(span_of(vcands@[i]), origin@).0 <= p < mapped(span_of(vcands@[i]), origin@).1 {
                assert(covered(p, merged@));
            }
        }
        assert forall|i: int, j: int|
            0 <= i < cands@.len() && plausible(#[trigger] cands@[i]) && near(cands@[i], ctxs@) && 0 <= j
                < out@.len() && (#[trigger] out@[j]) is Keep implies !(cands@[i].start <= out@[j]->Keep_0 < cands@[i].end) by {
            let p = out@[j]->Keep_0 as int;
            if cands@[i].start <= p < cands@[i].end {
                assert(covered(p, merged@));
            }
        }
        assert forall|p: int| #![auto] 0 <= p < n && !(exists|s: (usize, usize)| #![auto]
            collected_f5(s, cands@, ctxs@, google@, vcands@, vctxs@, vgoogle@, origin@, differs) && in_span(p, s)) implies keeps(out@, p) by {
            if covered(p, merged@) {
                assert(exists|s: (usize, usize)| #![auto]
                    collected_f5(s, cands@, ctxs@, google@, vcands@, vctxs@, vgoogle@, origin@, differs) && in_span(p, s));
            }
        }
    }
    out
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
    let out = redact_raw(29, &cands, &ctxs, &google);
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

/// F5: "OTP " + four Arabic-Indic digits (12 bytes; the view "OTP 1234" is 8). The raw
/// text has no candidate; the view's code maps back to bytes 4..12, none of them kept.
fn witness_f5_unicode_digits_hidden() {
    let cands: Vec<Cand> = Vec::new();
    let google: Vec<(usize, usize)> = Vec::new();
    let mut ctxs: Vec<Ctx> = Vec::new();
    ctxs.push(Ctx { start: 0, end: 3 });
    let mut vctxs: Vec<Ctx> = Vec::new();
    vctxs.push(Ctx { start: 0, end: 3 });
    let mut vcands: Vec<Cand> = Vec::new();
    vcands.push(Cand { start: 4, end: 8, len: 4, has_digit: true, all_digits: true, after_plus: false, in_url: false, currency: false, numeric_join: false });
    let vgoogle: Vec<(usize, usize)> = Vec::new();
    let mut origin: Vec<(usize, usize)> = Vec::new();
    origin.push((0, 1));
    origin.push((1, 2));
    origin.push((2, 3));
    origin.push((3, 4));
    origin.push((4, 6));
    origin.push((6, 8));
    origin.push((8, 10));
    origin.push((10, 12));
    assert(wf_origin(origin@, 8, 12));
    assert(dist(vcands@[0], vctxs@[0]) == 1);
    assert(plausible(vcands@[0]) && near(vcands@[0], vctxs@));
    assert(mapped(span_of(vcands@[0]), origin@) == (4usize, 12usize));
    let out = redact_otps(12, &cands, &ctxs, &google, Ghost(8), &vcands, &vctxs, &vgoogle, &origin, true);
    assert(!keeps(out@, 6) && !keeps(out@, 11)) by {
        if keeps(out@, 6) {
            let j = choose|j: int| 0 <= j < out@.len() && #[trigger] out@[j] == Item::Keep(6);
            assert(out@[j] is Keep);
        }
        if keeps(out@, 11) {
            let j = choose|j: int| 0 <= j < out@.len() && #[trigger] out@[j] == Item::Keep(11);
            assert(out@[j] is Keep);
        }
    }
}

fn main() {
}

} // verus!
