// OTP extraction: which candidate `parse_otp` returns.
//
// Production: crates/lucina-phone-core/src/otp.rs, branch lucina-rs dd7ec9e (F4: 52b9140, merged
// from otp-fixes). Before F4: lucina-rs c357646.
//   parse_kernel     <-> `parse_otp` (body length cap, the Google path, then the
//                        context x candidate double loop with `distance < best`)
//   eligible(_, true)<-> `plausible` (has a digit, not all-digits longer than 8, not
//                        after '+') + URL removal (a code inside a URL is never a
//                        candidate) + F4's `amount` skip
//   dist             <-> the distance expression in `parse_otp` (and `otp_spans`)
//   parse_before_f4  <-> `parse_otp` at c357646 (no amount skip), kept as the witness
// Differential test: verification/verus/diff/otp_extract.rs.
//
// Modelled: the selection exactly, over the tokenizer's output. Regexes cannot be
// verified, so the input is what they produce on `View::text` (digit-normalized,
// URL-stripped): `ctxs` (CONTEXT matches: start, end) and `cands` (CODES matches:
// start, end, length of the code once '-' and ' ' are removed, has-digit, all-digits,
// preceded by '+', inside a URL, next to a currency marker, glued to another number),
// plus whether the Google path matched (`google`). `currency` and `numeric_join` are
// F4's `currency_adjacent` and `numeric_join`.
// The tokenizer contract (`wf`): start < end, 4 <= len <= 10 (what CODES can match),
// all_digits ==> has_digit. Codes inside URLs never reach the real loop (the URL is
// removed first); the kernel takes them with in_url = true and must skip them.
//
// Proven for parse_kernel (X1-X4 also for parse_before_f4):
//   X1 the result is None, the Google match, or the index of one candidate;
//   X2 a candidate result has a digit, 4-10 characters, is never all-digits with more
//      than 8 digits, never follows '+', is never inside a URL;
//   X3 it is within MAX_DISTANCE (64 bytes) of some context span; with no context span
//      (and no Google match) the result is None; None only when no eligible candidate
//      is within MAX_DISTANCE of any context;
//   X4 it is a minimum-distance eligible candidate over all (context, candidate) pairs;
//   X5 an all-digit result is never next to a currency marker or glued to another
//      number, so "verification fee 1500 DKK" yields None.
//   witness_f4_skips_the_amount: parse_before_f4 returns that amount, parse_kernel None.
// What X1-X5 do not say: a context word is a keyword, not a sentence; a 4-8 character
// code-shaped token near one that is not an amount or a date is still a code.
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
    /// F4: next to a currency marker.
    pub currency: bool,
    /// F4: glued to another number by . , : / -
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

/// `plausible` + "not inside a URL" (+ F4: not an amount, when `f4`).
pub open spec fn eligible(c: Cand, f4: bool) -> bool {
    c.has_digit && !(c.all_digits && c.len > 8) && !c.after_plus && !c.in_url && (f4 ==> !is_amount(c))
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

fn eligible_exec(c: Cand, f4: bool) -> (b: bool)
    ensures
        b == eligible(c, f4),
{
    c.has_digit && !(c.all_digits && c.len > 8) && !c.after_plus && !c.in_url && (!f4 || !(c.all_digits
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
pub fn select(cands: &Vec<Cand>, ctxs: &Vec<Ctx>, f4: bool) -> (r: Option<usize>)
    ensures
        r is None ==> forall|i: int, k: int|
            0 <= i < cands@.len() && 0 <= k < ctxs@.len() && eligible(#[trigger] cands@[i], f4) ==> dist(
                cands@[i],
                #[trigger] ctxs@[k],
            ) > MAX_DISTANCE,
        r is Some ==> ({
            let i = r->Some_0 as int;
            &&& 0 <= i < cands@.len()
            &&& eligible(cands@[i], f4)
            &&& exists|k: int|
                0 <= k < ctxs@.len() && #[trigger] dist(cands@[i], ctxs@[k]) <= MAX_DISTANCE && forall|j: int, k2: int|
                    0 <= j < cands@.len() && 0 <= k2 < ctxs@.len() && eligible(#[trigger] cands@[j], f4) ==> dist(
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
                0 <= i < cands@.len() && eligible(cands@[i], f4) && 0 <= bk < ctxs@.len() && dist(
                    cands@[i],
                    ctxs@[bk],
                ) == best as int
            }),
            forall|j: int, k2: int|
                0 <= j < cands@.len() && 0 <= k2 < a && eligible(#[trigger] cands@[j], f4) ==> best as int
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
                    0 <= i < cands@.len() && eligible(cands@[i], f4) && 0 <= bk < ctxs@.len() && dist(
                        cands@[i],
                        ctxs@[bk],
                    ) == best as int
                }),
                forall|j: int, k2: int|
                    0 <= j < cands@.len() && 0 <= k2 < a && eligible(#[trigger] cands@[j], f4) ==> best
                        as int <= dist(cands@[j], #[trigger] ctxs@[k2]),
                forall|j: int|
                    0 <= j < b && eligible(#[trigger] cands@[j], f4) ==> best as int <= dist(
                        cands@[j],
                        ctxs@[a as int],
                    ),
            decreases cands.len() - b,
        {
            let c = cands[b];
            if eligible_exec(c, f4) {
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
                0 <= j < cands@.len() && 0 <= k2 < ctxs@.len() && eligible(#[trigger] cands@[j], f4) implies dist(
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

/// X1-X5: everything proven about one `parse_otp` answer. `f4` adds X5 and the amount
/// guard to `eligible`.
pub open spec fn parse_post(r: Pick, body_len: usize, google: bool, cands: Seq<Cand>, ctxs: Seq<Ctx>, f4: bool) -> bool {
    // X1
    &&& r is Cand ==> (r->Cand_0 as int) < cands.len()
    &&& body_len > MAX_BODY_BYTES ==> r == Pick::None
    &&& r == Pick::Google ==> google
    // X2
    &&& r is Cand ==> ({
        let c = cands[r->Cand_0 as int];
        &&& c.has_digit
        &&& 4 <= c.len <= 10
        &&& !(c.all_digits && c.len > 8)
        &&& !c.after_plus
        &&& !c.in_url
    })
    // X3
    &&& r is Cand ==> exists|k: int|
        0 <= k < ctxs.len() && #[trigger] dist(cands[r->Cand_0 as int], ctxs[k]) <= MAX_DISTANCE
    &&& ctxs.len() == 0 && !google ==> r == Pick::None
    &&& r == Pick::None && body_len <= MAX_BODY_BYTES ==> !google && forall|i: int, k: int|
        0 <= i < cands.len() && 0 <= k < ctxs.len() && eligible(#[trigger] cands[i], f4) ==> dist(
            cands[i],
            #[trigger] ctxs[k],
        ) > MAX_DISTANCE
    // X4
    &&& r is Cand ==> exists|k: int|
        0 <= k < ctxs.len() && forall|j: int, k2: int|
            0 <= j < cands.len() && 0 <= k2 < ctxs.len() && eligible(#[trigger] cands[j], f4) ==> #[trigger] dist(
                cands[r->Cand_0 as int],
                ctxs[k],
            ) <= dist(cands[j], #[trigger] ctxs[k2])
    // X5
    &&& f4 && r is Cand ==> !is_amount(cands[r->Cand_0 as int])
}

/// `parse_otp` with or without the F4 amount guard.
pub fn parse_mode(body_len: usize, google: bool, cands: &Vec<Cand>, ctxs: &Vec<Ctx>, f4: bool) -> (r: Pick)
    requires
        wf(cands@, ctxs@),
    ensures
        parse_post(r, body_len, google, cands@, ctxs@, f4),
{
    if body_len > MAX_BODY_BYTES {
        return Pick::None;
    }
    if google {
        return Pick::Google;
    }
    match select(cands, ctxs, f4) {
        None => Pick::None,
        Some(i) => {
            assert(wf_cand(cands@[i as int]));
            Pick::Cand(i)
        },
    }
}

/// `parse_otp` on lucina-rs dd7ec9e (F4, 52b9140): amounts and dates are skipped.
pub fn parse_kernel(body_len: usize, google: bool, cands: &Vec<Cand>, ctxs: &Vec<Ctx>) -> (r: Pick)
    requires
        wf(cands@, ctxs@),
    ensures
        parse_post(r, body_len, google, cands@, ctxs@, true),
{
    parse_mode(body_len, google, cands, ctxs, true)
}

/// `parse_otp` before F4 (lucina-rs c357646): the old behaviour, kept as the witness
/// of the bug F4 f4.
pub fn parse_before_f4(body_len: usize, google: bool, cands: &Vec<Cand>, ctxs: &Vec<Ctx>) -> (r: Pick)
    requires
        wf(cands@, ctxs@),
    ensures
        parse_post(r, body_len, google, cands@, ctxs@, false),
{
    parse_mode(body_len, google, cands, ctxs, false)
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
    let r = parse_before_f4(42, false, &cands, &ctxs);
    assert(dist(cands@[0], ctxs@[0]) == 9 && dist(cands@[1], ctxs@[0]) == 21);
    assert(r == Pick::Cand(0));
    let f = parse_kernel(42, false, &cands, &ctxs);
    assert(f == Pick::Cand(0));
}

/// The bug F4 f4: before F4, "Your verification fee is 1500 DKK." returned "1500";
/// now it returns None.
fn witness_f4_skips_the_amount() {
    let mut cands: Vec<Cand> = Vec::new();
    cands.push(cand(25, 29, 4, true, true));
    let mut ctxs: Vec<Ctx> = Vec::new();
    ctxs.push(Ctx { start: 5, end: 17 });  // "verification"
    let r = parse_before_f4(34, false, &cands, &ctxs);
    assert(dist(cands@[0], ctxs@[0]) == 8);
    assert(r == Pick::Cand(0));
    assert(is_amount(cands@[0]));
    let f = parse_kernel(34, false, &cands, &ctxs);
    assert(f == Pick::None);
}

/// A number with no context word, a phone number and a far number are never codes.
fn witness_no_context_and_phone() {
    let mut cands: Vec<Cand> = Vec::new();
    cands.push(cand(8, 14, 6, true, false));
    let ctxs: Vec<Ctx> = Vec::new();
    let r0 = parse_kernel(20, false, &cands, &ctxs);
    assert(r0 == Pick::None);
    let mut phone: Vec<Cand> = Vec::new();
    phone.push(Cand { start: 6, end: 16, len: 10, has_digit: true, all_digits: true, after_plus: true, in_url: false, currency: false, numeric_join: false });
    let mut near: Vec<Ctx> = Vec::new();
    near.push(Ctx { start: 20, end: 26 });  // "verify"
    let r1 = parse_kernel(30, false, &phone, &near);
    assert(r1 == Pick::None);
    let mut far: Vec<Cand> = Vec::new();
    far.push(cand(100, 106, 6, true, false));
    assert(dist(far@[0], near@[0]) == 74);
    let r2 = parse_kernel(120, false, &far, &near);
    assert(r2 == Pick::None);
}

fn main() {
}

} // verus!
