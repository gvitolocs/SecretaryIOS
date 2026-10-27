// Dial-number kernel (Lucina Android: what may reach Android Telecom as a number to call).
//
// Production (branch lucina-rs, crates/lucina-android-calls/src/number.rs):
//   call_kernel <-> number::call_number (the dialer, the in-app Call button and Lucina's
//                   `phone_call` tool through the CallProvider all go through it)
//   keep        <-> the `filter` (drop ASCII whitespace, '(', ')', '-')
//   Verdict     <-> Ok(number) / Err("Enter a valid phone number.") / Err("Short service
//                   codes are not voice-call numbers.")
// Characters are Unicode scalar values (`char as u32`); the diff test maps every input
// string to its scalars.
// Differential test: crates/lucina-android-calls/tests/verus_number.rs.
//
// Proven:
//   N1 dial_shape: an accepted number is at most one leading '+' followed only by ASCII
//      digits, so no URI scheme (`tel:`), no MMI/USSD code (`*21*...#`, `#31#`), no pause
//      or wait (',' ';' 'p' 'w'), no extension and no second '+' ever reaches Telecom.
//   N2 dial_length: 6 to 15 digits (E.164), or exactly the emergency numbers 112 / 911.
//   N3 nothing_but_separators_dropped: the accepted number is the input with only
//      ASCII whitespace, '(', ')' and '-' removed (the digits dialled are the digits typed,
//      in order).
//   N4 commands_refused: an input containing '*', '#', ',', ';' or any letter is refused,
//      whatever surrounds it.
//   N5 idempotent: an accepted number is accepted again unchanged (the CallProvider may
//      normalise twice).
//   N6 short_codes_refused: 3 to 5 digits other than 112 / 911 are ServiceCode.
//
// No Stateright model: one pure function of one input string; no state.
use vstd::prelude::*;

verus! {

pub const MAX_INPUT_CHARS: usize = 60;

pub const PLUS: u32 = 43;

#[derive(PartialEq, Eq)]
pub enum Verdict {
    Invalid,
    ServiceCode,
    Ok,
}

// ---------------------------------------------------------------- spec model

/// `char::is_ascii_whitespace`: U+0020, U+0009, U+000A, U+000C, U+000D (not U+000B).
pub open spec fn ascii_ws(c: u32) -> bool {
    c == 32 || c == 9 || c == 10 || c == 12 || c == 13
}

/// The filter of `call_number`: drop ASCII whitespace, '(', ')' and '-'.
pub open spec fn keep(c: u32) -> bool {
    !ascii_ws(c) && c != 40 && c != 41 && c != 45
}

pub open spec fn is_digit(c: u32) -> bool {
    48 <= c <= 57
}

/// The input without the separators, in order.
pub open spec fn strip(s: Seq<u32>) -> Seq<u32>
    decreases s.len(),
{
    if s.len() == 0 {
        Seq::empty()
    } else {
        let p = strip(s.drop_last());
        if keep(s.last()) {
            p.push(s.last())
        } else {
            p
        }
    }
}

/// `s.strip_prefix('+').unwrap_or(&s)`.
pub open spec fn digits_part(s: Seq<u32>) -> Seq<u32> {
    if s.len() > 0 && s[0] == PLUS {
        s.subrange(1, s.len() as int)
    } else {
        s
    }
}

pub open spec fn all_digits(s: Seq<u32>) -> bool {
    forall|i: int| 0 <= i < s.len() ==> is_digit(#[trigger] s[i])
}

pub open spec fn emergency(d: Seq<u32>) -> bool {
    d =~= seq![49u32, 49, 50] || d =~= seq![57u32, 49, 49]
}

pub open spec fn verdict(input: Seq<u32>) -> Verdict {
    let s = strip(input);
    let d = digits_part(s);
    if input.len() > MAX_INPUT_CHARS || !all_digits(d) || d.len() < 3 || d.len() > 15 {
        Verdict::Invalid
    } else if d.len() < 6 && !emergency(d) {
        Verdict::ServiceCode
    } else {
        Verdict::Ok
    }
}

// ---------------------------------------------------------------- lemmas about strip

proof fn lemma_strip_len(s: Seq<u32>)
    ensures
        strip(s).len() <= s.len(),
    decreases s.len(),
{
    if s.len() > 0 {
        lemma_strip_len(s.drop_last());
    }
}

/// Everything strip keeps passes `keep`.
proof fn lemma_strip_kept(s: Seq<u32>)
    ensures
        forall|i: int| 0 <= i < strip(s).len() ==> keep(#[trigger] strip(s)[i]),
    decreases s.len(),
{
    if s.len() > 0 {
        let p = strip(s.drop_last());
        lemma_strip_kept(s.drop_last());
        if keep(s.last()) {
            assert forall|i: int| 0 <= i < strip(s).len() implies keep(#[trigger] strip(s)[i]) by {
                if i < p.len() {
                    assert(strip(s)[i] == p[i]);
                }
            }
        }
    }
}

/// A character of the input that passes `keep` survives into strip.
proof fn lemma_strip_keeps(s: Seq<u32>, j: int)
    requires
        0 <= j < s.len(),
        keep(s[j]),
    ensures
        exists|i: int| 0 <= i < strip(s).len() && strip(s)[i] == s[j],
    decreases s.len(),
{
    let p = strip(s.drop_last());
    if j == s.len() - 1 {
        assert(strip(s) == p.push(s.last()));
        assert(strip(s)[p.len() as int] == s[j]);
    } else {
        assert(s.drop_last()[j] == s[j]);
        lemma_strip_keeps(s.drop_last(), j);
        let i = choose|i: int| 0 <= i < p.len() && p[i] == s[j];
        if keep(s.last()) {
            assert(strip(s)[i] == p[i]);
        }
    }
}

/// Strip of a sequence whose characters all pass `keep` is the sequence itself.
proof fn lemma_strip_fixed(s: Seq<u32>)
    requires
        forall|i: int| 0 <= i < s.len() ==> keep(#[trigger] s[i]),
    ensures
        strip(s) == s,
    decreases s.len(),
{
    if s.len() > 0 {
        let t = s.drop_last();
        assert forall|i: int| 0 <= i < t.len() implies keep(#[trigger] t[i]) by {
            assert(t[i] == s[i]);
        }
        lemma_strip_fixed(t);
        assert(keep(s[s.len() - 1]));
        assert(t.push(s.last()) =~= s);
    }
}

// ---------------------------------------------------------------- theorems

/// N1: '+'? digit*.
pub proof fn dial_shape(input: Seq<u32>)
    requires
        verdict(input) == Verdict::Ok,
    ensures
        forall|i: int|
            0 <= i < strip(input).len() ==> (is_digit(#[trigger] strip(input)[i]) || (i == 0
                && strip(input)[i] == PLUS)),
{
    let s = strip(input);
    let d = digits_part(s);
    assert forall|i: int| 0 <= i < s.len() implies (is_digit(#[trigger] s[i]) || (i == 0 && s[i]
        == PLUS)) by {
        if s.len() > 0 && s[0] == PLUS {
            if i > 0 {
                assert(d[i - 1] == s[i]);
            }
        } else {
            assert(d[i] == s[i]);
        }
    }
}

/// N2: 6..=15 digits or exactly 112 / 911.
pub proof fn dial_length(input: Seq<u32>)
    requires
        verdict(input) == Verdict::Ok,
    ensures
        (6 <= digits_part(strip(input)).len() <= 15) || emergency(digits_part(strip(input))),
{
}

/// N3: what is dialled is the input minus the separators (the exec function returns
/// exactly `strip(input)`, see `call_kernel`), and every kept character is still there.
pub proof fn nothing_but_separators_dropped(input: Seq<u32>, j: int)
    requires
        0 <= j < input.len(),
        keep(input[j]),
    ensures
        exists|i: int| 0 <= i < strip(input).len() && strip(input)[i] == input[j],
{
    lemma_strip_keeps(input, j);
}

/// N4: '*', '#', ',', ';' and letters are never accepted.
pub proof fn commands_refused(input: Seq<u32>, j: int)
    requires
        0 <= j < input.len(),
        input[j] == 42 || input[j] == 35 || input[j] == 44 || input[j] == 59 || (65 <= input[j]
            <= 90) || (97 <= input[j] <= 122),
    ensures
        verdict(input) != Verdict::Ok,
{
    lemma_strip_keeps(input, j);
    let s = strip(input);
    let i = choose|i: int| 0 <= i < s.len() && s[i] == input[j];
    if verdict(input) == Verdict::Ok {
        dial_shape(input);
        assert(is_digit(s[i]) || (i == 0 && s[i] == PLUS));
    }
}

/// N5: an accepted number normalises to itself.
pub proof fn idempotent(input: Seq<u32>)
    requires
        verdict(input) == Verdict::Ok,
    ensures
        strip(strip(input)) == strip(input),
        verdict(strip(input)) == Verdict::Ok,
{
    lemma_strip_kept(input);
    lemma_strip_fixed(strip(input));
    lemma_strip_len(input);
}

/// N6: 3-5 digits other than 112 / 911 are service codes.
pub proof fn short_codes_refused(input: Seq<u32>)
    requires
        input.len() <= MAX_INPUT_CHARS,
        all_digits(digits_part(strip(input))),
        3 <= digits_part(strip(input)).len() < 6,
        !emergency(digits_part(strip(input))),
    ensures
        verdict(input) == Verdict::ServiceCode,
{
}

/// Witness (not vacuous): "+4566778702" is accepted, "*21*12345#" is not, "112" is,
/// "1234" is a service code.
pub proof fn witnesses() {
    let ok = seq![43u32, 52, 53, 54, 54, 55, 55, 56, 55, 48, 50];
    assert forall|i: int| 0 <= i < ok.len() implies keep(#[trigger] ok[i]) by {}
    lemma_strip_fixed(ok);
    assert(digits_part(ok) =~= ok.subrange(1, 11));
    assert(all_digits(digits_part(ok)));
    assert(verdict(ok) == Verdict::Ok);
    let mmi = seq![42u32, 50, 49, 42, 49, 50, 51, 52, 53, 35];
    commands_refused(mmi, 0);
    let sos = seq![49u32, 49, 50];
    assert forall|i: int| 0 <= i < sos.len() implies keep(#[trigger] sos[i]) by {}
    lemma_strip_fixed(sos);
    assert(emergency(sos));
    assert(verdict(sos) == Verdict::Ok);
    let code = seq![49u32, 50, 51, 52];
    assert forall|i: int| 0 <= i < code.len() implies keep(#[trigger] code[i]) by {}
    lemma_strip_fixed(code);
    assert(!(code =~= seq![49u32, 49, 50]));
    assert(!(code =~= seq![57u32, 49, 49]));
    assert(verdict(code) == Verdict::ServiceCode);
}

// ---------------------------------------------------------------- exec (the code's steps)

fn keep_exec(c: u32) -> (b: bool)
    ensures
        b == keep(c),
{
    !(c == 32 || c == 9 || c == 10 || c == 12 || c == 13) && c != 40 && c != 41 && c != 45
}

/// `call_number`: `(verdict, number)`; the number is meaningful when the verdict is Ok.
pub fn call_kernel(input: &Vec<u32>) -> (r: (Verdict, Vec<u32>))
    ensures
        r.0 == verdict(input@),
        r.0 == Verdict::Ok ==> r.1@ == strip(input@),
{
    if input.len() > MAX_INPUT_CHARS {
        return (Verdict::Invalid, Vec::new());
    }
    let mut s: Vec<u32> = Vec::new();
    let mut i: usize = 0;
    while i < input.len()
        invariant
            i <= input.len(),
            s@ == strip(input@.subrange(0, i as int)),
        decreases input.len() - i,
    {
        let c = input[i];
        proof {
            let t = input@.subrange(0, i as int + 1);
            assert(t.drop_last() =~= input@.subrange(0, i as int));
            assert(t.last() == c);
        }
        if keep_exec(c) {
            s.push(c);
        }
        i += 1;
    }
    assert(input@.subrange(0, input.len() as int) =~= input@);
    let start: usize = if s.len() > 0 && s[0] == PLUS {
        1
    } else {
        0
    };
    let n = s.len() - start;
    let mut k: usize = start;
    while k < s.len()
        invariant
            start <= k <= s.len(),
            start == 0 || start == 1,
            (start == 1) == (s@.len() > 0 && s@[0] == PLUS),
            s@ == strip(input@),
            forall|j: int| start <= j < k ==> is_digit(#[trigger] s@[j]),
        decreases s.len() - k,
    {
        if !(48 <= s[k] && s[k] <= 57) {
            proof {
                let d = digits_part(s@);
                assert(d =~= s@.subrange(start as int, s@.len() as int));
                assert(d[k - start] == s@[k as int]);
            }
            return (Verdict::Invalid, Vec::new());
        }
        k += 1;
    }
    proof {
        let d = digits_part(s@);
        assert(d =~= s@.subrange(start as int, s@.len() as int));
        assert forall|j: int| 0 <= j < d.len() implies is_digit(#[trigger] d[j]) by {
            assert(d[j] == s@[j + start]);
        }
    }
    if n < 3 || n > 15 {
        return (Verdict::Invalid, Vec::new());
    }
    if n < 6 {
        let a = s[start];
        let b = s[start + 1];
        let c = s[start + 2];
        let sos = n == 3 && ((a == 49 && b == 49 && c == 50) || (a == 57 && b == 49 && c == 49));
        proof {
            let d = digits_part(s@);
            assert(d[0] == a && d[1] == b && d[2] == c);
            if n == 3 && a == 49 && b == 49 && c == 50 {
                assert(d =~= seq![49u32, 49, 50]);
            }
            if n == 3 && a == 57 && b == 49 && c == 49 {
                assert(d =~= seq![57u32, 49, 49]);
            }
            if emergency(d) {
                assert(d.len() == 3);
                assert(d[0] == a && d[1] == b && d[2] == c);
            }
        }
        if !sos {
            return (Verdict::ServiceCode, Vec::new());
        }
    }
    (Verdict::Ok, s)
}

} // verus!

fn main() {}
