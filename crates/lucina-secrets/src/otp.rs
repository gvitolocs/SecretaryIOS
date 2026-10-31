//! The OTP pass: one-time codes in outbound text (model prompts, logs).
//!
//! A code near an OTP context word ("verification code 482917", "codice di
//! verifica", `G-123456`, ...) becomes `[otp]`, using the phone bridge's own
//! detector (`lucina_phone_core::otp_spans`), so the redactor and the phone
//! tools agree on what a code is.
//!
//! It is **not** part of [`Redactor::redact`], which every sink uses:
//!
//! - the Secretary chat reply that delivers a code to the owner goes through
//!   the API sink, and must keep it;
//! - the detector is fuzzy by design (any 4–8 digit number within 64 bytes
//!   of "login", "security", "access", ...), so in the timeline, recent turns
//!   and API replies it would hide ordinary numbers the owner reads
//!   ("security review in 2026").
//!
//! So it runs only where a code is never wanted and nobody reads the text as
//! the owner's own content: prompts to the external model
//! (`lucina-server::secrets::RedactingModel`) and the log sink
//! ([`crate::log`]), through [`Redactor::redact_outbound`].
//!
//! `[otp]` keeps the known-value pass well formed (`wf`, see
//! [`crate::redact::admissible`]): it is shorter than
//! [`crate::redact::MIN_PATTERN_LEN`] and brackets are never part of a
//! registered pattern, so no registered value can occur in it. Existing
//! `[otp]` and `[secret:…]` placeholders are left alone, so a second pass
//! does not treat the `otp` in `[otp]` as a context word.

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::Regex;

use crate::redact::Redactor;

/// What a masked code becomes (the phone bridge's placeholder).
pub const OTP_PLACEHOLDER: &str = lucina_phone_core::otp::REDACTED;

fn placeholders() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\[[^\[\]\s]{1,96}\]").expect("placeholder regex"))
}

/// `text` with every OTP-looking code near an OTP context word replaced by
/// [`OTP_PLACEHOLDER`]. Placeholders already in the text are kept as they are.
pub fn mask_otps(text: &str) -> Cow<'_, str> {
    // Hide our own placeholders from the detector (same length, so the
    // spans still index `text`): `[otp]` is not a context word, and a
    // `[secret:S3KEY]` is not a code.
    let kept: Vec<(usize, usize)> = placeholders()
        .find_iter(text)
        .map(|m| (m.start(), m.end()))
        .collect();
    let probe: Cow<'_, str> = if kept.is_empty() {
        Cow::Borrowed(text)
    } else {
        let mut bytes = text.as_bytes().to_vec();
        for &(s, e) in &kept {
            // ASCII brackets around non-space bytes: `_` keeps it one word
            // that is neither a code nor a context word.
            bytes[s + 1..e - 1].fill(b'_');
        }
        // Replacing whole UTF-8 sequences with ASCII keeps it valid UTF-8.
        Cow::Owned(String::from_utf8(bytes).unwrap_or_default())
    };
    let spans: Vec<(usize, usize)> = lucina_phone_core::otp_spans(&probe)
        .into_iter()
        .filter(|&(s, e)| !kept.iter().any(|&(ks, ke)| s < ke && ks < e))
        .collect();
    if spans.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end) in spans {
        out.push_str(&text[at..start]);
        out.push_str(OTP_PLACEHOLDER);
        at = end;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
}

impl Redactor {
    /// [`Redactor::redact`], then the OTP pass ([`mask_otps`]): for text
    /// leaving Lucina where a one-time code is never wanted (model prompts,
    /// logs).
    pub fn redact_outbound<'a>(&self, text: &'a str) -> Cow<'a, str> {
        match self.redact(text) {
            Cow::Borrowed(t) => mask_otps(t),
            Cow::Owned(s) => Cow::Owned(mask_otps(&s).into_owned()),
        }
    }

    /// [`Redactor::redact_outbound`] in place; `true` when something changed.
    pub fn redact_outbound_string(&self, text: &mut String) -> bool {
        let changed = match self.redact_outbound(text) {
            Cow::Borrowed(_) => None,
            Cow::Owned(s) => Some(s),
        };
        match changed {
            Some(s) => {
                *text = s;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_codes_near_context_words() {
        assert_eq!(
            mask_otps("Your verification code is 482917."),
            "Your verification code is [otp]."
        );
        assert_eq!(
            mask_otps("Il tuo codice di verifica è 294728"),
            "Il tuo codice di verifica è [otp]"
        );
        assert_eq!(mask_otps("G-123456 from Google"), "[otp] from Google");
        let plain = "Ho comprato 12 uova e 2 litri di latte.";
        assert!(matches!(mask_otps(plain), Cow::Borrowed(_)));
    }

    #[test]
    fn keeps_placeholders_and_is_stable() {
        let r = Redactor::new();
        r.register("S3KEY", "Zp9qL2vX8rT5wY1u");
        let once = r
            .redact_outbound("access key Zp9qL2vX8rT5wY1u, login code 4829")
            .into_owned();
        assert_eq!(once, "access key [secret:S3KEY], login code [otp]");
        assert_eq!(r.redact_outbound(&once), once);
        // `[otp]` itself is not a context word for the next code.
        let far = format!("[otp]{}9999", " ".repeat(10));
        assert_eq!(mask_otps(&far), far);
    }

    #[test]
    fn plain_redact_keeps_codes() {
        // The owner's own delivery goes through the API sink: `redact` must
        // not touch it.
        let r = Redactor::new();
        let reply = "OTP from Bank: 482917\nCheck it matches the service you requested.";
        assert_eq!(r.redact(reply), reply);
        assert!(!r.redact_outbound(reply).contains("482917"));
    }

    #[test]
    fn unicode_text_survives() {
        let text = "验证码：294728，请勿泄露 [secret:令牌]";
        let out = mask_otps(text);
        assert_eq!(out, "验证码：[otp]，请勿泄露 [secret:令牌]");
    }
}
