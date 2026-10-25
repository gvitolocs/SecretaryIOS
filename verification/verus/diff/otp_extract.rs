//! Differential test of `verification/verus/otp_extract.rs` against the real
//! `lucina_phone_core::otp::parse_otp` (REV of `otp_run.sh`, default lucina-rs dd7ec9e, which has
//! F4).
//!
//! Belongs in `crates/lucina-phone-core/tests/verus_otp_extract.rs`, next to
//! `tests/otp_common/mod.rs` (= `diff/otp_tokenizer.rs`). `diff/otp_run.sh` installs
//! both in a snapshot of REV and runs `cargo test -p lucina-phone-core`; no new
//! dependency (a built-in xorshift generator instead of proptest).
//!
//! F4 (amounts and dates near a context word are not codes) is the default kernel;
//! `parse_before_f4` (`f4 = false`) is the kernel of the old code, kept to show what
//! F4 changed.

mod otp_common;

use lucina_phone_core::otp::parse_otp;
use otp_common::*;

/// Plain-Rust copy of the Verus kernel (`eligible_exec`, `dist_exec`, `select`, `parse_mode`:
/// `f4 = true` is `parse_kernel`, `false` is `parse_before_f4`).
mod kernel {
    use super::otp_common::{Cand, Ctx, MAX_BODY_BYTES, MAX_DISTANCE};

    pub fn eligible(c: Cand, f4: bool) -> bool {
        c.has_digit
            && !(c.all_digits && c.len > 8)
            && !c.after_plus
            && !c.in_url
            && (!f4 || !(c.all_digits && (c.currency || c.numeric_join)))
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

    pub fn select(cands: &[Cand], ctxs: &[Ctx], f4: bool) -> Option<usize> {
        let mut best = MAX_DISTANCE + 1;
        let mut result = None;
        for x in ctxs {
            for (b, c) in cands.iter().enumerate() {
                if eligible(*c, f4) {
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

    pub fn parse_kernel(body_len: usize, google: bool, cands: &[Cand], ctxs: &[Ctx], f4: bool) -> Pick {
        if body_len > MAX_BODY_BYTES {
            return Pick::None;
        }
        if google {
            return Pick::Google;
        }
        match select(cands, ctxs, f4) {
            None => Pick::None,
            Some(i) => Pick::Cand(i),
        }
    }
}

/// The kernel's answer as the code string `parse_otp` would return.
fn kernel_parse(sender: &str, body: &str, f4: bool) -> (Option<String>, Option<Cand>) {
    let t = parse_tokens(sender, body);
    match kernel::parse_kernel(body.len(), t.google.is_some(), &t.cands, &t.ctxs, f4) {
        kernel::Pick::None => (None, None),
        kernel::Pick::Google => (t.google.clone(), None),
        kernel::Pick::Cand(i) => (Some(t.codes[i].clone()), Some(t.cands[i])),
    }
}

fn is_amount(c: &Cand) -> bool {
    c.all_digits && (c.currency || c.numeric_join)
}

/// The kernel (F4) is parse_otp on every generated body, and its result satisfies
/// X2, X3 and X5 checked on the real text.
#[test]
fn kernel_matches_parse_otp() {
    let mut r = Rng::new(0x5eed_0001);
    let (mut some, mut old_amounts) = (0, 0);
    for case in 0..30_000 {
        let body = body(&mut r, case % 2 == 0);
        let sender = sender(&mut r);
        let real = parse_otp(sender, &body);
        let (k, cand) = kernel_parse(sender, &body, true);
        assert_eq!(real, k, "sender {sender:?} body {body:?}");
        if let Some(c) = cand {
            some += 1;
            let code = real.as_deref().unwrap();
            assert!((4..=10).contains(&code.len()) && code.bytes().any(|b| b.is_ascii_digit()), "{body:?}");
            assert!(!(code.bytes().all(|b| b.is_ascii_digit()) && code.len() > 8), "{body:?}");
            assert!(!c.after_plus && !c.in_url && !is_amount(&c), "{body:?}");
            let t = parse_tokens(sender, &body);
            assert!(t.ctxs.iter().any(|x| kernel::dist(c, *x) <= MAX_DISTANCE), "{body:?}");
        }
        if let (_, Some(c)) = kernel_parse(sender, &body, false) {
            if is_amount(&c) {
                old_amounts += 1;
            }
        }
    }
    // Non-vacuity: the generator reaches codes, and amounts the old code returned.
    assert!(some > 3_000, "only {some} bodies with a code");
    assert!(old_amounts > 100, "only {old_amounts} amounts");
    eprintln!("kernel == parse_otp (F4) on 30000 bodies; {some} with a code; {old_amounts} where the pre-F4 kernel returns an amount");
}

/// F4 changes only answers that were amounts: where the pre-F4 kernel did not return
/// an amount, both kernels agree.
#[test]
fn f4_changes_only_amount_picks() {
    let mut r = Rng::new(0x5eed_0002);
    let mut changed = 0;
    for case in 0..30_000 {
        let body = body(&mut r, case % 2 == 0);
        let sender = sender(&mut r);
        let (old, old_cand) = kernel_parse(sender, &body, false);
        let (new, new_cand) = kernel_parse(sender, &body, true);
        assert_eq!(new, parse_otp(sender, &body));
        if let Some(c) = new_cand {
            assert!(!is_amount(&c), "{body:?}");
        }
        match old_cand {
            Some(c) if is_amount(&c) => changed += 1,
            _ => assert_eq!(new, old, "{body:?}"),
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

/// F4: an amount or a date near a context word is not a code (it was before F4,
/// as the pre-F4 kernel still shows); a real code next to one still is.
#[test]
fn amounts_are_never_codes() {
    for (body, amount) in AMOUNTS {
        assert_eq!(parse_otp("Shop", body), None, "{body}");
        assert_eq!(kernel_parse("Shop", body, true).0, None, "the kernel: {body}");
        assert_eq!(kernel_parse("Shop", body, false).0.as_deref(), Some(*amount), "pre-F4: {body}");
    }
    for (body, want) in [
        ("Your security code is 4821. Total 1299 DKK", "4821"),
        ("Verification fee 1500 DKK. Your verification code is 482913.", "482913"),
        ("Your security code is 294-728.", "294728"),
    ] {
        assert_eq!(parse_otp("Bank", body).as_deref(), Some(want), "{body}");
        assert_eq!(kernel_parse("Bank", body, true).0.as_deref(), Some(want), "{body}");
    }
}
