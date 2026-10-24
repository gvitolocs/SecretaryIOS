//! OTP detection and redaction.
//!
//! A code is a 4–8 digit number, a `123-456` pair or a 4–10 character
//! upper-case alphanumeric token with at least one digit, closest to an OTP
//! context word ("verification", "codice di verifica", "engangskode",
//! "验证码", …). Links are removed first (a code inside a URL is not a code),
//! as are numbers right after `+` (phone numbers) and all-digit runs longer
//! than eight. Google's `G-123456` has its own pattern.

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

/// Maps the common Unicode decimal digit blocks (Arabic-Indic, Extended
/// Arabic-Indic, Devanagari, full-width) to ASCII; other text is kept.
fn ascii_digits(body: &str) -> String {
    body.chars()
        .map(|c| {
            let n = c as u32;
            [0x660, 0x6f0, 0x966, 0xff10]
                .into_iter()
                .find(|zero| n >= *zero && n < *zero + 10)
                .map_or(c, |zero| char::from_u32(48 + n - zero).unwrap_or(c))
        })
        .collect()
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
    let normalized = ascii_digits(body);
    let text = URL.replace_all(&normalized, "");
    if sender.eq_ignore_ascii_case("google") || text.to_lowercase().contains("google") {
        if let Some(m) = GOOGLE.captures(&text) {
            return Some(m[1].to_owned());
        }
    }
    let mut best = MAX_DISTANCE + 1;
    let mut result = None;
    for context in CONTEXT.find_iter(&text) {
        for m in CODES.find_iter(&text) {
            let Some(code) = plausible(&text, m.start(), m.as_str()) else {
                continue;
            };
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
/// token within [`MAX_DISTANCE`] bytes of a context word goes. Unicode digits
/// are not normalized here, so redaction never changes other text.
pub fn redact_otps(text: &str) -> Cow<'_, str> {
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
    if spans.is_empty() {
        return Cow::Borrowed(text);
    }
    spans.sort_unstable();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end) in spans {
        if start < at {
            at = at.max(end);
            continue;
        }
        out.push_str(&text[at..start]);
        out.push_str(REDACTED);
        at = end;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
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
}
