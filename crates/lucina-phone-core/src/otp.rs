//! OTP detection and redaction.
//!
//! A code is a 4–8 digit number, a `123-456` pair or a 4–10 character
//! upper-case alphanumeric token with at least one digit, closest to an OTP
//! context word ("verification", "codice di verifica", "engangskode",
//! "验证码", …). Links are removed first (a code inside a URL is not a code),
//! as are numbers right after `+` (phone numbers), all-digit runs longer
//! than eight, and amounts and dates (F4): an all-digit number next to a
//! currency marker (`1500 DKK`, `DKK 1500`, `$1999`) or joined to another
//! number by `. , : / -` (`1250,00`, `2026-10-25`). Google's `G-123456` has
//! its own pattern.
//!
//! Redaction (F5) tokenizes the same digit-normalized, link-stripped view of
//! the text as [`parse_otp`] (and the raw text, as before) and maps the spans
//! back to the original bytes, so every code `parse_otp` can return is
//! covered. Kernels: `verification/verus/otp_extract.rs` (X5) and
//! `otp_redact.rs` (D5) on branch `otp-verify`.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

/// Bodies longer than this are not parsed (an OTP SMS is short; this bounds
/// the work on the receiver thread).
pub const MAX_BODY_BYTES: usize = 8192;

/// Maximum distance, in bytes, between a code and its context word.
const MAX_DISTANCE: usize = 64;

static CONTEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?iu)(?:\b(?:otp|one[ -]time|verification|verify|security|authentication|authentification|login|sign[ -]?in|access",
        r"|verifica|sicurezza|accesso|autenticazione",
        r"|bestätigung|bestatigung|anmelde|sicherheits",
        r"|bekræft|bekraeft|engangs|verifikations",
        r"|vérification|connexion|sécurité",
        r"|seguridad|verificación|autenticación|confirmação",
        r"|подтверждения|проверки|входа)\p{L}*",
        r"|验证码|驗證碼|認証コード|인증",
        r"|(?:your|votre|tuo|tu|din|dit|dein)\s+(?:code|codice|código|codigo|kode)\b)",
    ))
    .expect("OTP context pattern")
});
static CODES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:[0-9]{3}[- ][0-9]{3}|[0-9]{4,8}|[A-Z0-9]{4,10})\b").expect("OTP code pattern")
});
static GOOGLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bG[-\s\x{2011}\x{2013}]([0-9]{6})\b").expect("Google code pattern")
});
static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"https?://\S+").expect("URL pattern"));

/// Currency codes and words that make a number next to them an amount
/// (whole words, case-insensitive, surrounding punctuation ignored).
const CURRENCY_WORDS: &[&str] = &[
    "dkk", "kr", "kr.", "sek", "nok", "eur", "euro", "euros", "usd", "gbp", "chf", "pln", "inr",
    "rs", "rs.",
];
/// Currency symbols right before or after a number.
const CURRENCY_SYMBOLS: &[char] = &['€', '$', '£', '¥', '₹'];

/// Maps a digit of the common Unicode decimal digit blocks (Arabic-Indic,
/// Extended Arabic-Indic, Devanagari, full-width) to ASCII; other characters
/// are kept.
fn ascii_digit(c: char) -> char {
    let n = c as u32;
    [0x660, 0x6f0, 0x966, 0xff10]
        .into_iter()
        .find(|zero| n >= *zero && n < *zero + 10)
        .map_or(c, |zero| char::from_u32(48 + n - zero).unwrap_or(c))
}

/// What [`parse_otp`] reads: the text with ASCII digits and without links,
/// plus, for each of its bytes, the byte range of the character of the
/// original text it comes from.
struct View {
    text: String,
    origin: Vec<(usize, usize)>,
}

impl View {
    fn new(body: &str) -> Self {
        let mut normalized = String::with_capacity(body.len());
        let mut from = Vec::with_capacity(body.len());
        for (i, c) in body.char_indices() {
            let before = normalized.len();
            normalized.push(ascii_digit(c));
            from.resize(normalized.len(), (i, i + c.len_utf8()));
            debug_assert!(normalized.len() > before);
        }
        let mut text = String::with_capacity(normalized.len());
        let mut origin = Vec::with_capacity(from.len());
        let mut at = 0;
        for m in URL.find_iter(&normalized) {
            text.push_str(&normalized[at..m.start()]);
            origin.extend_from_slice(&from[at..m.start()]);
            at = m.end();
        }
        text.push_str(&normalized[at..]);
        origin.extend_from_slice(&from[at..]);
        Self { text, origin }
    }

    /// The original byte range of the view's bytes `start..end`.
    fn original(&self, start: usize, end: usize) -> (usize, usize) {
        (self.origin[start].0, self.origin[end - 1].1)
    }
}

fn currency_word(w: &str) -> bool {
    let w = w.trim_matches(|c: char| matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | ')' | '('));
    CURRENCY_WORDS.iter().any(|c| w.eq_ignore_ascii_case(c))
}

/// `text[start..end]` is next to a currency marker: a symbol right before
/// (spaces allowed) or right after it, or a currency word just before or
/// after it.
fn currency_adjacent(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].trim_end();
    let after = text[end..].trim_start();
    if before.ends_with(CURRENCY_SYMBOLS) || text[end..].starts_with(CURRENCY_SYMBOLS) {
        return true;
    }
    let next = after.split_whitespace().next().unwrap_or("");
    let prev = before.split_whitespace().last().unwrap_or("");
    currency_word(next) || currency_word(prev)
}

/// `text[start..end]` is glued to another number by `. , : / -`
/// (`1250,00`, `2026-10-25`, `12:30`).
fn numeric_join(text: &str, start: usize, end: usize) -> bool {
    let b = text.as_bytes();
    let sep = |c: u8| matches!(c, b'.' | b',' | b':' | b'/' | b'-');
    (end + 1 < b.len() && sep(b[end]) && b[end + 1].is_ascii_digit())
        || (start >= 2 && sep(b[start - 1]) && b[start - 2].is_ascii_digit())
}

/// F4: an all-digit candidate that is an amount, a price or a date.
fn amount(text: &str, start: usize, end: usize, code: &str) -> bool {
    code.bytes().all(|b| b.is_ascii_digit())
        && (currency_adjacent(text, start, end) || numeric_join(text, start, end))
}

/// A code candidate that is not obviously something else.
fn plausible(text: &str, start: usize, raw: &str) -> Option<String> {
    let code = raw.replace(['-', ' '], "");
    let has_digit = code.bytes().any(|b| b.is_ascii_digit());
    let all_digits = code.bytes().all(|b| b.is_ascii_digit());
    if !has_digit || (all_digits && code.len() > 8) || text[..start].ends_with('+') {
        return None;
    }
    Some(code)
}

/// The OTP in an SMS or notification, if there is one.
///
/// `sender` only matters for Google (`G-123456`). The result is 4–10 ASCII
/// alphanumerics ([`crate::proto::valid_code`] holds).
pub fn parse_otp(sender: &str, body: &str) -> Option<String> {
    if body.len() > MAX_BODY_BYTES {
        return None;
    }
    let view = View::new(body);
    let text = view.text.as_str();
    if sender.eq_ignore_ascii_case("google") || text.to_lowercase().contains("google") {
        if let Some(m) = GOOGLE.captures(text) {
            return Some(m[1].to_owned());
        }
    }
    let mut best = MAX_DISTANCE + 1;
    let mut result = None;
    for context in CONTEXT.find_iter(text) {
        for m in CODES.find_iter(text) {
            let Some(code) = plausible(text, m.start(), m.as_str()) else {
                continue;
            };
            if amount(text, m.start(), m.end(), &code) {
                continue;
            }
            let distance = if m.end() <= context.start() {
                context.start() - m.end()
            } else {
                m.start().saturating_sub(context.end())
            };
            if distance < best {
                best = distance;
                result = Some(code);
            }
        }
    }
    result
}

/// Replacement for a redacted code.
pub const REDACTED: &str = "[otp]";

/// `text` with every OTP-looking code near an OTP context word (and every
/// Google `G-123456`) replaced by [`REDACTED`].
///
/// For text that may reach a model, a log or shared memory (phone tool
/// output, notification dumps). It errs towards redacting: any code-shaped
/// token within [`MAX_DISTANCE`] bytes of a context word goes (amounts
/// too), found in the raw text and in [`parse_otp`]'s view of it (Unicode
/// digits as ASCII, links removed), so every code `parse_otp` returns is
/// covered. Only the spans change; other text is kept as it is.
pub fn redact_otps(text: &str) -> Cow<'_, str> {
    let spans = otp_spans(text);
    if spans.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end) in spans {
        out.push_str(&text[at..start]);
        out.push_str(REDACTED);
        at = end;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
}

/// The byte ranges [`redact_otps`] replaces: sorted, non-overlapping
/// (overlapping candidates are merged) and on `char` boundaries.
///
/// For redactors that write their own placeholder (`lucina-secrets`).
pub fn otp_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = candidate_spans(text);
    // F5: the candidates parse_otp sees, mapped back to the original bytes.
    let view = View::new(text);
    if view.text != text {
        spans.extend(
            candidate_spans(&view.text)
                .into_iter()
                .map(|(start, end)| view.original(start, end)),
        );
    }
    spans.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start < last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

/// Every Google match and every plausible code near a context word in
/// `text`, unsorted.
fn candidate_spans(text: &str) -> Vec<(usize, usize)> {
    let contexts: Vec<(usize, usize)> = CONTEXT
        .find_iter(text)
        .map(|m| (m.start(), m.end()))
        .collect();
    let mut spans: Vec<(usize, usize)> = GOOGLE
        .find_iter(text)
        .map(|m| (m.start(), m.end()))
        .collect();
    if !contexts.is_empty() {
        for m in CODES.find_iter(text) {
            if plausible(text, m.start(), m.as_str()).is_none() {
                continue;
            }
            let near = contexts.iter().any(|&(start, end)| {
                let distance = if m.end() <= start {
                    start - m.end()
                } else {
                    m.start().saturating_sub(end)
                };
                distance <= MAX_DISTANCE
            });
            if near {
                spans.push((m.start(), m.end()));
            }
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_languages_and_non_codes() {
        for (sender, body, want) in [
            ("Google", "G-123456", "123456"),
            (
                "Bank",
                "Your security code is 682149. Expires in 5 minutes.",
                "682149",
            ),
            ("Apple", "Il tuo codice di verifica è 294728.", "294728"),
            ("Service", "Код подтверждения: 294728.", "294728"),
            ("Service", "验证码：294728", "294728"),
            ("Service", "Din engangskode er 294728.", "294728"),
            ("Service", "Your login code: X8H2K9.", "X8H2K9"),
            ("Service", "Votre code de sécurité est 294728.", "294728"),
            ("Service", "Ihr Sicherheitscode lautet 294728.", "294728"),
            ("Service", "Tu código de verificación es 294728.", "294728"),
            ("Service", "Your security code is 294-728.", "294728"),
            ("Service", "Your verification code: ٢٩٤٧٢٨.", "294728"),
        ] {
            assert_eq!(parse_otp(sender, body).as_deref(), Some(want), "{body}");
        }
        for body in [
            "Invoice 123456, order code 9999.",
            "Call +4512345678 at 12:30.",
            "Your balance is 123456 euros.",
            "Your verification code is at https://example.test/123456",
            "G-1234567",
        ] {
            assert_eq!(parse_otp("Shop", body), None, "{body}");
        }
        assert_eq!(parse_otp("Bank", &"x".repeat(MAX_BODY_BYTES + 1)), None);
    }

    #[test]
    fn redaction_hides_codes_and_keeps_other_text() {
        let text = "Row: 0 address=Bank, body=Your security code is 682149. Order 123456 shipped yesterday to the depot in town.";
        let out = redact_otps(text);
        assert!(!out.contains("682149"), "{out}");
        assert!(out.contains("address=Bank"));
        assert_eq!(
            redact_otps("G-123456 is your Google verification code"),
            "[otp] is your Google verification code"
        );
        assert_eq!(
            redact_otps("Il tuo codice di verifica è 294728."),
            "Il tuo codice di verifica è [otp]."
        );
        let plain = "Battery 87%, build 20261024, nothing secret here.";
        assert!(matches!(redact_otps(plain), Cow::Borrowed(_)));
        assert_eq!(
            redact_otps("Call +4512345678 to verify"),
            "Call +4512345678 to verify"
        );
    }

    #[test]
    fn spans_match_redaction() {
        let text = "Your security code is 682149 (or G-123456). Order 123456 shipped.";
        let spans = otp_spans(text);
        assert_eq!(spans.len(), 3, "{spans:?}");
        assert!(spans.windows(2).all(|w| w[0].1 <= w[1].0));
        assert_eq!(&text[spans[0].0..spans[0].1], "682149");
        let mut rebuilt = String::new();
        let mut at = 0;
        for (s, e) in &spans {
            rebuilt.push_str(&text[at..*s]);
            rebuilt.push_str(REDACTED);
            at = *e;
        }
        rebuilt.push_str(&text[at..]);
        assert_eq!(redact_otps(text), rebuilt);
        assert!(otp_spans("Battery 87%, nothing secret.").is_empty());
    }

    /// F4 (Verus `otp_extract.rs` X5): amounts and dates near a context word
    /// are not codes; a real code next to one still is.
    #[test]
    fn amounts_and_dates_are_not_codes() {
        for body in [
            "Your verification fee is 1500 DKK.",
            "Security deposit: DKK 1500 due today.",
            "Login from new device. Charged 2499 kr.",
            "Sign in alert: EUR 1250,00 paid",
            "Verification: you paid $1999 today",
            "Verification: you paid $ 1999 today",
            "Your login on 2026-10-25 was successful.",
            "Your login at 10/25/2026 was successful.",
            "Security alert: 1999€ charged",
            "Login: Rs. 4500 debited",
        ] {
            assert_eq!(parse_otp("Shop", body), None, "{body}");
        }
        for (body, want) in [
            ("Your security code is 4821. Total 1299 DKK", "4821"),
            (
                "Verification fee 1500 DKK. Your verification code is 482913.",
                "482913",
            ),
            ("Your security code is 294-728.", "294728"),
            ("Your security code is 294 728.", "294728"),
            ("Your login code: X8H2K9. Total 1299 DKK", "X8H2K9"),
            ("Your verification code: 482913. Expires 12:30.", "482913"),
            ("OTP 4821 for your payment of EUR 12,50", "4821"),
        ] {
            assert_eq!(parse_otp("Shop", body).as_deref(), Some(want), "{body}");
        }
        // Redaction still errs towards hiding amounts near a context word.
        assert_eq!(
            redact_otps("Your verification fee is 1500 DKK."),
            "Your verification fee is [otp] DKK."
        );
    }

    /// F5 (Verus `otp_redact.rs` D5): every code parse_otp returns is
    /// redacted in the original text, which is otherwise unchanged.
    #[test]
    fn every_parsed_code_is_redacted() {
        let pad = format!("https://example.test/{}", "a".repeat(70));
        for (body, want) in [
            (
                "Your verification code: \u{662}\u{669}\u{664}\u{667}\u{662}\u{668}.".to_owned(),
                "Your verification code: [otp].".to_owned(),
            ),
            (
                "Your verification code: \u{ff14}\u{ff18}\u{ff12}\u{ff19}\u{ff11}\u{ff13}".to_owned(),
                "Your verification code: [otp]".to_owned(),
            ),
            (
                "Google: G-\u{662}\u{669}\u{664}\u{667}\u{662}\u{668}".to_owned(),
                "Google: [otp]".to_owned(),
            ),
            (
                "\u{915}\u{94b}\u{921} OTP \u{967}\u{968}\u{969}\u{96a}\u{96b}\u{96c} \u{939}\u{948}".to_owned(),
                "\u{915}\u{94b}\u{921} OTP [otp] \u{939}\u{948}".to_owned(),
            ),
            (
                format!("Your verification code {pad} 482913"),
                format!("Your verification code {pad} [otp]"),
            ),
            (
                format!("Your verification code is at https://example.test/123456 or {pad} 482913."),
                format!("Your verification code is at https://example.test/[otp] or {pad} [otp]."),
            ),
        ] {
            let code = parse_otp("Shop", &body).expect("parsed");
            assert!(code.len() >= 4, "{body}");
            assert_eq!(redact_otps(&body), want, "{body}");
            let spans = otp_spans(&body);
            assert!(spans.iter().all(|&(s, e)| body.is_char_boundary(s) && body.is_char_boundary(e)));
            assert!(spans.windows(2).all(|w| w[0].1 <= w[1].0));
        }
    }

    #[test]
    fn view_is_what_parse_otp_reads() {
        let body = "A \u{ff11}\u{ff12} https://x.test/9 b \u{662}";
        let v = View::new(body);
        assert_eq!(v.text, "A 12  b 2");
        assert_eq!(v.origin.len(), v.text.len());
        assert_eq!(v.original(2, 4), (2, 8));
        assert_eq!(v.original(8, 9), (body.len() - 2, body.len()));
    }
}
