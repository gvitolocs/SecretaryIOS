//! Differential test of `verification/verus/otp_redact.rs` against the real
//! `otp_spans` / `redact_otps` (REV of `otp_run.sh`, default lucina-rs dd7ec9e with F5).
//!
//! Belongs in `crates/lucina-phone-core/tests/verus_otp_redact.rs`, next to
//! `tests/otp_common/mod.rs` (= `diff/otp_tokenizer.rs`); `diff/otp_run.sh` runs it.
//!
//! F5: redaction tokenizes the raw text and `parse_otp`'s view (digits normalized,
//! links removed) and maps the view's spans back, so every code `parse_otp` returns is
//! redacted. Before F5 the view half was missing (Unicode digits, a URL between the
//! context word and the code); `every_parsed_code_is_redacted` holds those cases.

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

    /// `otp_spans_raw`: one text (the code before F5).
    pub fn otp_spans_raw(cands: &[Cand], ctxs: &[Ctx], google: &[(usize, usize)]) -> Vec<(usize, usize)> {
        merge(&sort_spans(&collect(cands, ctxs, google)))
    }

    /// `otp_spans` (F5): raw spans, plus the view's spans mapped back when the view differs.
    #[allow(clippy::too_many_arguments)]
    pub fn otp_spans(
        cands: &[Cand], ctxs: &[Ctx], google: &[(usize, usize)],
        vcands: &[Cand], vctxs: &[Ctx], vgoogle: &[(usize, usize)],
        origin: &[(usize, usize)], differs: bool,
    ) -> Vec<(usize, usize)> {
        let mut spans = collect(cands, ctxs, google);
        if differs {
            let v = collect(vcands, vctxs, vgoogle);
            for (s, e) in v {
                spans.push((origin[s].0, origin[e - 1].1));
            }
        }
        merge(&sort_spans(&spans))
    }
}

/// The kernel pipeline (F5), rendered like `redact_otps`.
fn kernel_redact(text: &str) -> (Vec<(usize, usize)>, String) {
    let t = f5_tokens(text);
    let spans = kernel::otp_spans(
        &t.raw.cands, &t.raw.ctxs, &t.raw.google, &t.view.cands, &t.view.ctxs, &t.view.google, &t.origin, t.differs,
    );
    let mut out: Vec<u8> = Vec::new();
    for item in kernel::redact(text.len(), &spans) {
        match item {
            kernel::Item::Keep(p) => out.push(text.as_bytes()[p]),
            kernel::Item::Ph => out.extend_from_slice(REDACTED.as_bytes()),
        }
    }
    (spans, String::from_utf8(out).expect("spans on char boundaries"))
}

/// The original bytes of the candidate `parse_otp` (F4) selects: its view span mapped
/// back by `View::original`.
fn selected_span(sender: &str, body: &str) -> Option<(usize, usize)> {
    let t = parse_tokens(sender, body);
    if t.too_long || t.google.is_some() {
        return None;
    }
    let mut best = MAX_DISTANCE + 1;
    let mut result = None;
    for x in &t.ctxs {
        for c in &t.cands {
            let amount = c.all_digits && (c.currency || c.numeric_join);
            if c.has_digit && !(c.all_digits && c.len > 8) && !c.after_plus && !c.in_url && !amount {
                let d = if c.end <= x.start { x.start - c.end } else { c.start.saturating_sub(x.end) };
                if d < best {
                    best = d;
                    result = Some((c.start, c.end));
                }
            }
        }
    }
    result.map(|(s, e)| View::new(body).original(s, e))
}

#[test]
fn kernel_matches_otp_spans_and_redact_otps() {
    let mut r = Rng::new(0x5eed_0101);
    let (mut redacted, mut through_view) = (0, 0);
    for case in 0..30_000 {
        let text = body(&mut r, case % 2 == 0);
        let (spans, out) = kernel_redact(&text);
        let t = raw_tokens(&text);
        if kernel::otp_spans_raw(&t.cands, &t.ctxs, &t.google) != spans {
            through_view += 1;
        }
        assert_eq!(otp_spans(&text), spans, "{text:?}");
        assert_eq!(redact_otps(&text), out, "{text:?}");
        if spans.is_empty() {
            assert_eq!(out, text);
        } else {
            redacted += 1;
        }
    }
    assert!(redacted > 3_000, "only {redacted} texts redacted");
    assert!(through_view > 300, "only {through_view} texts where the view adds spans");
    eprintln!("kernel == otp_spans/redact_otps (F5) on 30000 texts; {redacted} redacted, {through_view} with spans only the view finds");
}

/// D5 on the real functions, for every generated body (Unicode digits and URLs too).
#[test]
fn parsed_code_is_redacted() {
    let mut r = Rng::new(0x5eed_0102);
    let mut checked = 0;
    for case in 0..30_000 {
        let text = body(&mut r, case % 3 == 0);
        let sender = sender(&mut r);
        let Some((s, e)) = selected_span(sender, &text) else { continue };
        let code = ascii_digits(&text[s..e]).replace(['-', ' '], "");
        assert_eq!(parse_otp(sender, &text).as_deref(), Some(code.as_str()), "{text:?}");
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

/// F5: the codes the raw text alone missed (before F5 `redact_otps` kept these
/// bodies unchanged) are redacted, and the old raw-only kernel still misses them.
#[test]
fn every_parsed_code_is_redacted() {
    for (body, code) in missed() {
        assert_eq!(parse_otp("Shop", &body).as_deref(), Some(code), "{body}");
        let (s, e) = selected_span("Shop", &body).unwrap_or_else(|| {
            // the Google path: G-<digits>
            let v = View::new(&body);
            let m = GOOGLE.find(&v.text).expect("google");
            v.original(m.start(), m.end())
        });
        let out = redact_otps(&body);
        assert!(!out.contains(&body[s..e]), "{body} -> {out}");
        assert_eq!(kernel_redact(&body).1, out, "{body}");
        let t = raw_tokens(&body);
        assert!(kernel::otp_spans_raw(&t.cands, &t.ctxs, &t.google).iter().all(|&(a, b)| !(a <= s && e <= b)), "{body}");
    }
}
