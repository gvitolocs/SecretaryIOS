//! Differential test: `number::call_number` against a plain-Rust copy of the
//! Verus kernel `verification/verus/dial_number.rs`. Inputs are random
//! strings over an alphabet that mixes digits, '+', the separators the code
//! drops, MMI/USSD and pause characters, letters, non-ASCII digits and
//! non-ASCII whitespace (U+00A0, U+2007, U+000B), plus every string of up to
//! four characters over a smaller alphabet. The verdict (accepted, invalid,
//! service code) and the accepted number must agree; the kernel's theorems
//! N1/N2 are re-checked on every accepted number.

use lucina_calls::number::call_number;

/// Plain-Rust copy of the Verus kernel (exec part).
mod kernel {
    pub const MAX_INPUT_CHARS: usize = 60;
    pub const PLUS: u32 = 43;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Verdict {
        Invalid,
        ServiceCode,
        Ok,
    }

    fn keep_exec(c: u32) -> bool {
        !(c == 32 || c == 9 || c == 10 || c == 12 || c == 13) && c != 40 && c != 41 && c != 45
    }

    pub fn call_kernel(input: &[u32]) -> (Verdict, Vec<u32>) {
        if input.len() > MAX_INPUT_CHARS {
            return (Verdict::Invalid, Vec::new());
        }
        let mut s: Vec<u32> = Vec::new();
        for &c in input {
            if keep_exec(c) {
                s.push(c);
            }
        }
        let start: usize = if !s.is_empty() && s[0] == PLUS { 1 } else { 0 };
        let n = s.len() - start;
        for &c in &s[start..] {
            if !(48..=57).contains(&c) {
                return (Verdict::Invalid, Vec::new());
            }
        }
        if !(3..=15).contains(&n) {
            return (Verdict::Invalid, Vec::new());
        }
        if n < 6 {
            let (a, b, c) = (s[start], s[start + 1], s[start + 2]);
            let sos =
                n == 3 && ((a == 49 && b == 49 && c == 50) || (a == 57 && b == 49 && c == 49));
            if !sos {
                return (Verdict::ServiceCode, Vec::new());
            }
        }
        (Verdict::Ok, s)
    }
}

use kernel::Verdict;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const ALPHABET: &[char] = &[
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', '0', '1', '1', '2', '9', '+', ' ', '\t',
    '\n', '\r', '\u{0c}', '\u{0b}', '(', ')', '-', '*', '#', ',', ';', 'p', 'w', 'P', 'W', 't',
    'e', 'l', ':', '.', '/', '\u{a0}', '\u{2007}', '\u{0661}', '\u{ff11}', 'é', '$',
];

fn real(input: &str) -> (Verdict, Vec<u32>) {
    match call_number(input) {
        Ok(n) => (Verdict::Ok, n.chars().map(|c| c as u32).collect()),
        Err(e) if e.contains("service codes") => (Verdict::ServiceCode, Vec::new()),
        Err(_) => (Verdict::Invalid, Vec::new()),
    }
}

fn check(input: &str) {
    let scalars: Vec<u32> = input.chars().map(|c| c as u32).collect();
    let want = kernel::call_kernel(&scalars);
    let got = real(input);
    assert_eq!(got, want, "input {input:?}");
    if got.0 == Verdict::Ok {
        // N1: '+'? digit+; N2: 6..=15 digits or 112 / 911.
        let n: String = got.1.iter().map(|&c| char::from_u32(c).unwrap()).collect();
        let digits = n.strip_prefix('+').unwrap_or(&n);
        assert!(digits.bytes().all(|b| b.is_ascii_digit()), "{n:?}");
        assert!(
            (6..=15).contains(&digits.len()) || digits == "112" || digits == "911",
            "{n:?}"
        );
        // N5: normalising twice changes nothing.
        assert_eq!(call_number(&n).as_deref(), Ok(n.as_str()));
    }
}

#[test]
fn random_strings_agree_with_the_kernel() {
    let mut rng = Rng(0x5eed_d1a1);
    let mut accepted = 0;
    let mut service = 0;
    for _ in 0..200_000 {
        let len = rng.below(70);
        let mut s = String::new();
        // Bias towards number-shaped input so the Ok branch is well covered.
        let digits_only = rng.below(3) == 0;
        for _ in 0..len {
            let c = if digits_only && rng.below(8) != 0 {
                char::from(b'0' + rng.below(10) as u8)
            } else {
                ALPHABET[rng.below(ALPHABET.len())]
            };
            s.push(c);
        }
        check(&s);
        match real(&s).0 {
            Verdict::Ok => accepted += 1,
            Verdict::ServiceCode => service += 1,
            Verdict::Invalid => {}
        }
    }
    // Not vacuous: every branch was taken many times.
    assert!(accepted > 1_000, "accepted {accepted}");
    assert!(service > 100, "service codes {service}");
}

#[test]
fn every_short_string_agrees_with_the_kernel() {
    let small = ['1', '2', '9', '+', ' ', '-', '*', '#', 'p'];
    let mut stack = vec![String::new()];
    while let Some(s) = stack.pop() {
        check(&s);
        if s.chars().count() < 4 {
            for c in small {
                let mut t = s.clone();
                t.push(c);
                stack.push(t);
            }
        }
    }
    for s in [
        "112",
        "911",
        "+112",
        " 1 1 2 ",
        "(911)",
        "1-1-2",
        "113",
        "+45 66 77 87 02",
    ] {
        check(s);
    }
}

#[test]
fn length_boundaries_agree_with_the_kernel() {
    for n in 0..=20 {
        check(&"7".repeat(n));
        check(&format!("+{}", "7".repeat(n)));
    }
    // 60 characters before normalisation, with separators making up the rest.
    for pad in 0..=50 {
        check(&format!("{}{}", " ".repeat(pad), "4512345678"));
    }
}
