//! Differential test of `verification/verus/otp_extract.rs` against the real
//! `lucina_phone_core::otp::parse_otp` (lucina-rs c357646).
//!
//! Belongs in `crates/lucina-phone-core/tests/verus_otp_extract.rs`, next to
//! `tests/otp_common/mod.rs` (= `diff/otp_tokenizer.rs`). `diff/otp_run.sh` installs
//! both in a snapshot of lucina-rs and runs `cargo test -p lucina-phone-core`; no new
//! dependency (a built-in xorshift generator instead of proptest).
//!
//! Known bug (amounts and dates near a context word are returned as codes):
//! `*_known_bug_*` tests assert the current behaviour and fail once it is fixed (flip
//! them then); the `#[ignore]`d `amounts_are_never_codes` asserts the fixed behaviour.

mod otp_common;

use lucina_phone_core::otp::parse_otp;
use otp_common::*;

/// Plain-Rust copy of the Verus kernel (`eligible_exec`, `dist_exec`, `select`, `parse_kernel`).
mod kernel {
    use super::otp_common::{Cand, Ctx, MAX_BODY_BYTES, MAX_DISTANCE};

    pub fn eligible(c: Cand, fixed: bool) -> bool {
        c.has_digit
            && !(c.all_digits && c.len > 8)
            && !c.after_plus
            && !c.in_url
            && (!fixed || !(c.all_digits && (c.currency || c.numeric_join)))
    }

    pub fn dist(c: Cand, x: Ctx) -> usize {
        if c.end <= x.start {
            x.start - c.end
        } else if c.start >= x.end {
            c.start - x.end
        } else {
            0
        }
    }

    pub fn select(cands: &[Cand], ctxs: &[Ctx], fixed: bool) -> Option<usize> {
        let mut best = MAX_DISTANCE + 1;
        let mut result = None;
        for x in ctxs {
            for (b, c) in cands.iter().enumerate() {
                if eligible(*c, fixed) {
                    let d = dist(*c, *x);
                    if d < best {
                        best = d;
                        result = Some(b);
                    }
                }
            }
        }
        result
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum Pick {
        None,
        Google,
        Cand(usize),
    }

    pub fn parse_kernel(body_len: usize, google: bool, cands: &[Cand], ctxs: &[Ctx], fixed: bool) -> Pick {
        if body_len > MAX_BODY_BYTES {
            return Pick::None;
        }
        if google {
            return Pick::Google;
        }
        match select(cands, ctxs, fixed) {
            None => Pick::None,
            Some(i) => Pick::Cand(i),
        }
    }
}

/// The kernel's answer as the code string `parse_otp` would return.
fn kernel_parse(sender: &str, body: &str, fixed: bool) -> (Option<String>, Option<Cand>) {
    let t = parse_tokens(sender, body);
    match kernel::parse_kernel(body.len(), t.google.is_some(), &t.cands, &t.ctxs, fixed) {
        kernel::Pick::None => (None, None),
        kernel::Pick::Google => (t.google.clone(), None),
        kernel::Pick::Cand(i) => (Some(t.codes[i].clone()), Some(t.cands[i])),
    }
}

fn is_amount(c: &Cand) -> bool {
    c.all_digits && (c.currency || c.numeric_join)
}

/// The kernel (fixed = false) is parse_otp on every generated body, and its result
/// satisfies X2/X3 checked on the real text.
#[test]
fn kernel_matches_parse_otp() {
    let mut r = Rng::new(0x5eed_0001);
    let (mut some, mut amounts) = (0, 0);
    for case in 0..30_000 {
        let body = body(&mut r, case % 2 == 0);
        let sender = sender(&mut r);
        let real = parse_otp(sender, &body);
        let (k, cand) = kernel_parse(sender, &body, false);
        assert_eq!(real, k, "sender {sender:?} body {body:?}");
        if let Some(c) = cand {
            some += 1;
            let code = real.as_deref().unwrap();
            assert!((4..=10).contains(&code.len()) && code.bytes().any(|b| b.is_ascii_digit()), "{body:?}");
            assert!(!(code.bytes().all(|b| b.is_ascii_digit()) && code.len() > 8), "{body:?}");
            assert!(!c.after_plus && !c.in_url, "{body:?}");
            let t = parse_tokens(sender, &body);
            assert!(t.ctxs.iter().any(|x| kernel::dist(c, *x) <= MAX_DISTANCE), "{body:?}");
            if is_amount(&c) {
                amounts += 1;
            }
        }
    }
    // Non-vacuity: the generator reaches codes and (the known bug) amounts.
    assert!(some > 3_000, "only {some} bodies with a code");
    assert!(amounts > 100, "only {amounts} amounts picked");
    eprintln!("kernel == parse_otp on 30000 bodies; {some} with a code, {amounts} of them an amount (known bug)");
}

/// The fixed kernel never returns an amount, and agrees with parse_otp whenever
/// parse_otp did not return one.
#[test]
fn fixed_kernel_drops_only_amounts() {
    let mut r = Rng::new(0x5eed_0002);
    let mut changed = 0;
    for case in 0..30_000 {
        let body = body(&mut r, case % 2 == 0);
        let sender = sender(&mut r);
        let (real, real_cand) = kernel_parse(sender, &body, false);
        assert_eq!(real, parse_otp(sender, &body));
        let (fixed, fixed_cand) = kernel_parse(sender, &body, true);
        if let Some(c) = fixed_cand {
            assert!(!is_amount(&c), "{body:?}");
        }
        match real_cand {
            Some(c) if is_amount(&c) => changed += 1,
            _ => assert_eq!(fixed, real, "{body:?}"),
        }
    }
    assert!(changed > 100);
}

const AMOUNTS: &[(&str, &str)] = &[
    ("Your verification fee is 1500 DKK.", "1500"),
    ("Security deposit: DKK 1500 due today.", "1500"),
    ("Login from new device. Charged 2499 kr.", "2499"),
    ("Sign in alert: EUR 1250,00 paid", "1250"),
    ("Verification: you paid $1999 today", "1999"),
    ("Your login on 2026-10-25 was successful.", "2026"),
];

/// KNOWN BUG (lucina-rs c357646): an amount or a date near a context word is a code.
#[test]
fn amount_known_bug_parse_otp_returns_amounts() {
    for (body, amount) in AMOUNTS {
        assert_eq!(parse_otp("Shop", body).as_deref(), Some(*amount), "{body}");
        assert_eq!(kernel_parse("Shop", body, true).0, None, "the fixed kernel: {body}");
    }
    // The code still wins when both are there (both kernels and the real function).
    let b = "Your security code is 4821. Total 1299 DKK";
    assert_eq!(parse_otp("Bank", b).as_deref(), Some("4821"));
    assert_eq!(kernel_parse("Bank", b, true).0.as_deref(), Some("4821"));
    // With the fix, the real code next to an amount is still found.
    let b = "Verification fee 1500 DKK. Your verification code is 482913.";
    assert_eq!(kernel_parse("Bank", b, true).0.as_deref(), Some("482913"));
}

/// What the fix must make true. Fails on lucina-rs c357646.
#[test]
#[ignore = "fails on lucina-rs c357646: amounts/dates near a context word are returned as OTPs"]
fn amounts_are_never_codes() {
    for (body, _) in AMOUNTS {
        assert_eq!(parse_otp("Shop", body), None, "{body}");
    }
}
