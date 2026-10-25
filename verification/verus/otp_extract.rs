// OTP extraction: which candidate `parse_otp` returns.
//
// Production (branch lucina-rs, c357646): crates/lucina-phone-core/src/otp.rs
//   parse_kernel  <-> `parse_otp` (body length cap, the Google path, then the
//                     context x candidate double loop with `distance < best`)
//   eligible      <-> `plausible` (has a digit, not all-digits longer than 8, not after '+')
//                     plus URL removal (a code inside a URL is never a candidate)
//   dist          <-> the distance expression in `parse_otp` (and `otp_spans`)
// Differential test: verification/verus/diff/otp_extract.rs.
//
// Modelled: the selection exactly, over the tokenizer's output. Regexes cannot be
// verified, so the input is what they produce on the URL-stripped, digit-normalized
// text: `ctxs` (CONTEXT matches: start, end) and `cands` (CODES matches: start, end,
// length of the code once '-' and ' ' are removed, has-digit, all-digits, preceded by
// '+', inside a URL), plus whether the Google path matched (`google`).
// The tokenizer contract (`wf`): start < end, 4 <= len <= 10 (what CODES can match),
// all_digits ==> has_digit. Codes inside URLs never reach the real loop (the URL is
// removed first); the kernel takes them with in_url = true and must skip them.
// Two more tokenizer flags are for the proposed fix (FIX below): `currency` (the
// number is next to a currency marker: DKK, EUR, kr, $, ...) and `numeric_join`
// (glued to another number by . , : / -, e.g. 1250,00 or 2026-10-25).
// `fixed = false` is the code on lucina-rs; `fixed = true` adds the amount guard.
//
// Proven (both modes unless noted):
//   X1 the result is None, the Google match, or the index of one candidate;
//   X2 a candidate result has a digit, 4-10 characters, is never all-digits with more
//      than 8 digits, never follows '+', is never inside a URL;
//   X3 it is within MAX_DISTANCE (64 bytes) of some context span; with no context span
//      (and no Google match) the result is None; None only when no eligible candidate
//      is within MAX_DISTANCE of any context;
//   X4 it is a minimum-distance eligible candidate over all (context, candidate) pairs;
//   X5 (fixed only) an all-digit result is never next to a currency marker or glued to
//      another number, so "verification fee 1500 DKK" yields None.
//   bug_amount_is_selected: on lucina-rs (fixed = false) the amount IS the result.
// What X1-X4 do not say: a context word is a keyword, not a sentence. With one near
// (verification, security, login, ...), any 4-8 digit number nearby is a code. That is
// the KNOWN BUG reported with this file; X5 is the fix's guarantee.
use vstd::prelude::*;

verus! {

pub const MAX_DISTANCE: usize = 64;
pub const MAX_BODY_BYTES: usize = 8192;

#[derive(Clone, Copy)]
pub struct Cand {
    pub start: usize,
    pub end: usize,
    /// Length of the code after removing '-' and ' '.
    pub len: usize,
    pub has_digit: bool,
    pub all_digits: bool,
    pub after_plus: bool,
    pub in_url: bool,
    /// FIX: next to a currency marker.
    pub currency: bool,
    /// FIX: glued to another number by . , : / -
    pub numeric_join: bool,
}

#[derive(Clone, Copy)]
pub struct Ctx {
    pub start: usize,
    pub end: usize,
}

pub open spec fn wf_cand(c: Cand) -> bool {
    c.start < c.end && 4 <= c.len <= 10 && (c.all_digits ==> c.has_digit)
}

pub open spec fn wf(cands: Seq<Cand>, ctxs: Seq<Ctx>) -> bool {
    &&& forall|i: int| 0 <= i < cands.len() ==> wf_cand(#[trigger] cands[i])
    &&& forall|k: int| 0 <= k < ctxs.len() ==> (#[trigger] ctxs[k]).start < ctxs[k].end
}

pub open spec fn is_amount(c: Cand) -> bool {
    c.all_digits && (c.currency || c.numeric_join)
}

/// `plausible` + "not inside a URL" (+ the FIX when `fixed`).
pub open spec fn eligible(c: Cand, fixed: bool) -> bool {
    c.has_digit && !(c.all_digits && c.len > 8) && !c.after_plus && !c.in_url && (fixed ==> !is_amount(c))
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

fn eligible_exec(c: Cand, fixed: bool) -> (b: bool)
    ensures
        b == eligible(c, fixed),
{
    c.has_digit && !(c.all_digits && c.len > 8) && !c.after_plus && !c.in_url && (!fixed || !(c.all_digits
        && (c.currency || c.numeric_join)))
}

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

/// The double loop of `parse_otp` (contexts outer, candidates inner, strict `<`).
pub fn select(cands: &Vec<Cand>, ctxs: &Vec<Ctx>, fixed: bool) -> (r: Option<usize>)
    ensures
        r is None ==> forall|i: int, k: int|
            0 <= i < cands@.len() && 0 <= k < ctxs@.len() && eligible(#[trigger] cands@[i], fixed) ==> dist(
                cands@[i],
                #[trigger] ctxs@[k],
            ) > MAX_DISTANCE,
        r is Some ==> ({
            let i = r->Some_0 as int;
            &&& 0 <= i < cands@.len()
            &&& eligible(cands@[i], fixed)
            &&& exists|k: int|
                0 <= k < ctxs@.len() && #[trigger] dist(cands@[i], ctxs@[k]) <= MAX_DISTANCE && forall|j: int, k2: int|
                    0 <= j < cands@.len() && 0 <= k2 < ctxs@.len() && eligible(#[trigger] cands@[j], fixed) ==> dist(
                        cands@[i],
                        ctxs@[k],
                    ) <= dist(cands@[j], #[trigger] ctxs@[k2])
        }),
{
    let mut best: usize = MAX_DISTANCE + 1;
    let mut result: Option<usize> = None;
    let ghost mut bk: int = 0;
    let mut a: usize = 0;
    while a < ctxs.len()
        invariant
            0 <= a <= ctxs.len(),
            best <= MAX_DISTANCE + 1,
            result is None <==> best == MAX_DISTANCE + 1,
            result is Some ==> ({
                let i = result->Some_0 as int;
                0 <= i < cands@.len() && eligible(cands@[i], fixed) && 0 <= bk < ctxs@.len() && dist(
                    cands@[i],
                    ctxs@[bk],
                ) == best as int
            }),
            forall|j: int, k2: int|
                0 <= j < cands@.len() && 0 <= k2 < a && eligible(#[trigger] cands@[j], fixed) ==> best as int
                    <= dist(cands@[j], #[trigger] ctxs@[k2]),
        decreases ctxs.len() - a,
    {
        let x = ctxs[a];
        let mut b: usize = 0;
        while b < cands.len()
            invariant
                0 <= a < ctxs.len(),
                x == ctxs@[a as int],
                0 <= b <= cands.len(),
                best <= MAX_DISTANCE + 1,
                result is None <==> best == MAX_DISTANCE + 1,
                result is Some ==> ({
                    let i = result->Some_0 as int;
                    0 <= i < cands@.len() && eligible(cands@[i], fixed) && 0 <= bk < ctxs@.len() && dist(
                        cands@[i],
                        ctxs@[bk],
                    ) == best as int
                }),
                forall|j: int, k2: int|
                    0 <= j < cands@.len() && 0 <= k2 < a && eligible(#[trigger] cands@[j], fixed) ==> best
                        as int <= dist(cands@[j], #[trigger] ctxs@[k2]),
                forall|j: int|
                    0 <= j < b && eligible(#[trigger] cands@[j], fixed) ==> best as int <= dist(
                        cands@[j],
                        ctxs@[a as int],
                    ),
            decreases cands.len() - b,
        {
            let c = cands[b];
            if eligible_exec(c, fixed) {
                let d = dist_exec(c, x);
                if d < best {
                    best = d;
                    result = Some(b);
                    proof {
                        bk = a as int;
                    }
                }
            }
            b += 1;
        }
        a += 1;
    }
    proof {
        if result is Some {
            let i = result->Some_0 as int;
            assert(dist(cands@[i], ctxs@[bk]) <= MAX_DISTANCE);
            assert forall|j: int, k2: int|
                0 <= j < cands@.len() && 0 <= k2 < ctxs@.len() && eligible(#[trigger] cands@[j], fixed) implies dist(
                cands@[i],
                ctxs@[bk],
            ) <= dist(cands@[j], #[trigger] ctxs@[k2]) by {}
        }
    }
    result
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Pick {
    None,
    /// The Google `G-123456` path (6 digits; `sender` or text mentions google).
    Google,
    Cand(usize),
}

/// `parse_otp`.
pub fn parse_kernel(body_len: usize, google: bool, cands: &Vec<Cand>, ctxs: &Vec<Ctx>, fixed: bool) -> (r: Pick)
    requires
        wf(cands@, ctxs@),
    ensures
        // X1
        r is Cand ==> (r->Cand_0 as int) < cands@.len(),
        body_len > MAX_BODY_BYTES ==> r == Pick::None,
        r == Pick::Google ==> google,
        // X2
        r is Cand ==> ({
            let c = cands@[r->Cand_0 as int];
            &&& c.has_digit
            &&& 4 <= c.len <= 10
            &&& !(c.all_digits && c.len > 8)
            &&& !c.after_plus
            &&& !c.in_url
        }),
        // X3
        r is Cand ==> exists|k: int|
            0 <= k < ctxs@.len() && #[trigger] dist(cands@[r->Cand_0 as int], ctxs@[k]) <= MAX_DISTANCE,
        ctxs@.len() == 0 && !google ==> r == Pick::None,
        r == Pick::None && body_len <= MAX_BODY_BYTES ==> !google && forall|i: int, k: int|
            0 <= i < cands@.len() && 0 <= k < ctxs@.len() && eligible(#[trigger] cands@[i], fixed) ==> dist(
                cands@[i],
                #[trigger] ctxs@[k],
            ) > MAX_DISTANCE,
        // X4
        r is Cand ==> exists|k: int|
            0 <= k < ctxs@.len() && forall|j: int, k2: int|
                0 <= j < cands@.len() && 0 <= k2 < ctxs@.len() && eligible(#[trigger] cands@[j], fixed) ==> #[trigger] dist(
                    cands@[r->Cand_0 as int],
                    ctxs@[k],
                ) <= dist(cands@[j], #[trigger] ctxs@[k2]),
        // X5
        fixed && r is Cand ==> !is_amount(cands@[r->Cand_0 as int]),
{
    if body_len > MAX_BODY_BYTES {
        return Pick::None;
    }
    if google {
        return Pick::Google;
    }
    match select(cands, ctxs, fixed) {
        None => Pick::None,
        Some(i) => {
            assert(wf_cand(cands@[i as int]));
            Pick::Cand(i)
        },
    }
}

// ---------------------------------------------------------------- witnesses

fn cand(start: usize, end: usize, len: usize, all_digits: bool, currency: bool) -> (c: Cand)
    ensures
        c == (Cand {
            start,
            end,
            len,
            has_digit: true,
            all_digits,
            after_plus: false,
            in_url: false,
            currency,
            numeric_join: false,
        }),
{
    Cand { start, end, len, has_digit: true, all_digits, after_plus: false, in_url: false, currency, numeric_join: false }
}

/// "Your security code is 4821. Total 1299 DKK": the code, not the amount (both modes).
fn witness_code_beats_amount() {
    let mut cands: Vec<Cand> = Vec::new();
    cands.push(cand(22, 26, 4, true, false));
    cands.push(cand(34, 38, 4, true, true));
    let mut ctxs: Vec<Ctx> = Vec::new();
    ctxs.push(Ctx { start: 5, end: 13 });  // "security"
    let r = parse_kernel(42, false, &cands, &ctxs, false);
    assert(dist(cands@[0], ctxs@[0]) == 9 && dist(cands@[1], ctxs@[0]) == 21);
    assert(r == Pick::Cand(0));
    let f = parse_kernel(42, false, &cands, &ctxs, true);
    assert(f == Pick::Cand(0));
}

/// KNOWN BUG on lucina-rs: "Your verification fee is 1500 DKK." returns "1500".
/// With the fix it returns None.
fn witness_bug_amount_is_selected() {
    let mut cands: Vec<Cand> = Vec::new();
    cands.push(cand(25, 29, 4, true, true));
    let mut ctxs: Vec<Ctx> = Vec::new();
    ctxs.push(Ctx { start: 5, end: 17 });  // "verification"
    let r = parse_kernel(34, false, &cands, &ctxs, false);
    assert(dist(cands@[0], ctxs@[0]) == 8);
    assert(r == Pick::Cand(0));
    assert(is_amount(cands@[0]));
    let f = parse_kernel(34, false, &cands, &ctxs, true);
    assert(f == Pick::None);
}

/// A number with no context word, a phone number and a far number are never codes.
fn witness_no_context_and_phone() {
    let mut cands: Vec<Cand> = Vec::new();
    cands.push(cand(8, 14, 6, true, false));
    let ctxs: Vec<Ctx> = Vec::new();
    let r0 = parse_kernel(20, false, &cands, &ctxs, false);
    assert(r0 == Pick::None);
    let mut phone: Vec<Cand> = Vec::new();
    phone.push(Cand { start: 6, end: 16, len: 10, has_digit: true, all_digits: true, after_plus: true, in_url: false, currency: false, numeric_join: false });
    let mut near: Vec<Ctx> = Vec::new();
    near.push(Ctx { start: 20, end: 26 });  // "verify"
    let r1 = parse_kernel(30, false, &phone, &near, false);
    assert(r1 == Pick::None);
    let mut far: Vec<Cand> = Vec::new();
    far.push(cand(100, 106, 6, true, false));
    assert(dist(far@[0], near@[0]) == 74);
    let r2 = parse_kernel(120, false, &far, &near, false);
    assert(r2 == Pick::None);
}

fn main() {
}

} // verus!
