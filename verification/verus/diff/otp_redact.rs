//! Differential test of `verification/verus/otp_redact.rs` against the real
//! `otp_spans` / `redact_otps` (lucina-rs c357646).
//!
//! Belongs in `crates/lucina-phone-core/tests/verus_otp_redact.rs`, next to
//! `tests/otp_common/mod.rs` (= `diff/otp_tokenizer.rs`); `diff/otp_run.sh` runs it.
//!
//! Known bug: `parse_otp` reads the digit-normalized, URL-stripped text but
//! `otp_spans` the raw text, so a code `parse_otp` returns can survive `redact_otps`
//! (Unicode digits; a URL between the context word and the code). The
//! `*_known_bug_*` tests assert today's behaviour; the `#[ignore]`d
//! `every_parsed_code_is_redacted` asserts the fixed one.

mod otp_common;

use lucina_phone_core::otp::{otp_spans, parse_otp, redact_otps, REDACTED};
use otp_common::*;

/// Plain-Rust copy of the Verus kernel (`collect`, `sort_spans`, `merge`, `redact`).
mod kernel {
    use super::otp_common::{Cand, Ctx, MAX_DISTANCE};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Item {
        Keep(usize),
        Ph,
    }

    fn dist(c: Cand, x: Ctx) -> usize {
        if c.end <= x.start {
            x.start - c.end
        } else if c.start >= x.end {
            c.start - x.end
        } else {
            0
        }
    }

    pub fn near(c: Cand, ctxs: &[Ctx]) -> bool {
        ctxs.iter().any(|x| dist(c, *x) <= MAX_DISTANCE)
    }

    pub fn collect(cands: &[Cand], ctxs: &[Ctx], google: &[(usize, usize)]) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = google.to_vec();
        if !ctxs.is_empty() {
            for c in cands {
                let ok = c.has_digit && !(c.all_digits && c.len > 8) && !c.after_plus;
                if ok && near(*c, ctxs) {
                    out.push((c.start, c.end));
                }
            }
        }
        out
    }

    fn le(a: (usize, usize), b: (usize, usize)) -> bool {
        a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1)
    }

    pub fn sort_spans(v: &[(usize, usize)]) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for &x in v {
            let mut j = 0;
            while j < out.len() && le(out[j], x) {
                j += 1;
            }
            out.insert(j, x);
        }
        out
    }

    pub fn merge(spans: &[(usize, usize)]) -> Vec<(usize, usize)> {
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for &(start, end) in spans {
            let n = merged.len();
            if n > 0 && start < merged[n - 1].1 {
                let last = merged[n - 1];
                merged[n - 1] = (last.0, if last.1 >= end { last.1 } else { end });
            } else {
                merged.push((start, end));
            }
        }
        merged
    }

    pub fn redact(n: usize, merged: &[(usize, usize)]) -> Vec<Item> {
        let mut out = Vec::new();
        let mut at = 0;
        for &(stop, next) in merged {
            out.extend((at..stop).map(Item::Keep));
            out.push(Item::Ph);
            at = next;
        }
        out.extend((at..n).map(Item::Keep));
        out
    }

    pub fn otp_spans(cands: &[Cand], ctxs: &[Ctx], google: &[(usize, usize)]) -> Vec<(usize, usize)> {
        merge(&sort_spans(&collect(cands, ctxs, google)))
    }
}

/// The kernel pipeline on the raw text, rendered like `redact_otps`.
fn kernel_redact(text: &str) -> (Vec<(usize, usize)>, String) {
    let t = raw_tokens(text);
    let spans = kernel::otp_spans(&t.cands, &t.ctxs, &t.google);
    let mut out: Vec<u8> = Vec::new();
    for item in kernel::redact(text.len(), &spans) {
        match item {
            kernel::Item::Keep(p) => out.push(text.as_bytes()[p]),
            kernel::Item::Ph => out.extend_from_slice(REDACTED.as_bytes()),
        }
    }
    (spans, String::from_utf8(out).expect("spans on char boundaries"))
}

/// The bytes of the candidate `parse_otp` selects, when parse_otp's text is the raw
/// text (ASCII digits, no URL): then both see the same tokens.
fn selected_span(sender: &str, body: &str) -> Option<(usize, usize)> {
    let t = parse_tokens(sender, body);
    if t.too_long || t.google.is_some() {
        return None;
    }
    let mut best = MAX_DISTANCE + 1;
    let mut result = None;
    for x in &t.ctxs {
        for c in &t.cands {
            if c.has_digit && !(c.all_digits && c.len > 8) && !c.after_plus && !c.in_url {
                let d = if c.end <= x.start { x.start - c.end } else { c.start.saturating_sub(x.end) };
                if d < best {
                    best = d;
                    result = Some((c.start, c.end));
                }
            }
        }
    }
    result
}

#[test]
fn kernel_matches_otp_spans_and_redact_otps() {
    let mut r = Rng::new(0x5eed_0101);
    let mut redacted = 0;
    for case in 0..30_000 {
        let text = body(&mut r, case % 2 == 0);
        let (spans, out) = kernel_redact(&text);
        assert_eq!(otp_spans(&text), spans, "{text:?}");
        assert_eq!(redact_otps(&text), out, "{text:?}");
        if spans.is_empty() {
            assert_eq!(out, text);
        } else {
            redacted += 1;
        }
    }
    assert!(redacted > 3_000, "only {redacted} texts redacted");
}

/// D5 on the real functions where parse_otp and otp_spans see the same text.
#[test]
fn parsed_code_is_redacted_on_ascii_text_without_urls() {
    let mut r = Rng::new(0x5eed_0102);
    let mut checked = 0;
    for _ in 0..30_000 {
        let text = body(&mut r, true);
        let sender = sender(&mut r);
        let Some((s, e)) = selected_span(sender, &text) else { continue };
        assert_eq!(parse_otp(sender, &text).as_deref(), Some(text[s..e].replace(['-', ' '], "").as_str()));
        let spans = otp_spans(&text);
        assert!(spans.iter().any(|&(a, b)| a <= s && e <= b), "{text:?}: {s}..{e} not in {spans:?}");
        checked += 1;
    }
    assert!(checked > 3_000, "only {checked} checked");
}

const URL_PAD: &str = "https://example.test/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn missed() -> Vec<(String, &'static str)> {
    vec![
        ("Your verification code: \u{662}\u{669}\u{664}\u{667}\u{662}\u{668}.".to_owned(), "294728"),
        ("Your verification code: \u{ff14}\u{ff18}\u{ff12}\u{ff19}\u{ff11}\u{ff13}".to_owned(), "482913"),
        ("Google: G-\u{662}\u{669}\u{664}\u{667}\u{662}\u{668}".to_owned(), "294728"),
        (format!("Your verification code {URL_PAD} 482913"), "482913"),
    ]
}

/// KNOWN BUG (lucina-rs c357646): parse_otp returns these codes, redact_otps keeps them.
#[test]
fn redaction_known_bug_parsed_code_survives() {
    for (body, code) in missed() {
        assert_eq!(parse_otp("Shop", &body).as_deref(), Some(code), "{body}");
        assert_eq!(redact_otps(&body), body.as_str(), "{body}");
    }
}

/// What the fix must make true. Fails on lucina-rs c357646.
#[test]
#[ignore = "fails on lucina-rs c357646: Unicode-digit codes and codes behind a URL survive redaction"]
fn every_parsed_code_is_redacted() {
    for (body, _) in missed() {
        assert_ne!(redact_otps(&body), body.as_str(), "{body}");
    }
}
