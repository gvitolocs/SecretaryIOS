//! Voice-call numbers.
//!
//! The dialer, the in-app Call button and Lucina's `phone_call` tool (through
//! the `CallProvider`) all pass their input through [`call_number`] before
//! Telecom sees it, so nothing but a plain number is ever dialled: no `tel:`
//! URIs, no USSD or MMI codes (`*21*…#`), no pauses or extensions.

use crate::Result;

/// The longest input accepted, before normalisation.
pub const MAX_INPUT_CHARS: usize = 60;

/// Normalise a voice-call number: spaces, `(`, `)` and `-` are dropped, one
/// leading `+` is kept, and 3–15 digits must remain (E.164 allows 15).
///
/// Numbers shorter than six digits are service codes, which are refused,
/// except the emergency numbers 112 and 911.
pub fn call_number(input: &str) -> Result<String> {
    if input.chars().count() > MAX_INPUT_CHARS {
        return Err("Enter a valid phone number.".into());
    }
    let s: String = input
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && !['(', ')', '-'].contains(c))
        .collect();
    let digits = s.strip_prefix('+').unwrap_or(&s);
    if !digits.bytes().all(|b| b.is_ascii_digit()) || !(3..=15).contains(&digits.len()) {
        return Err("Enter a valid phone number.".into());
    }
    if digits.len() < 6 && !["112", "911"].contains(&digits) {
        return Err("Short service codes are not voice-call numbers.".into());
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_owner_number_and_no_commands() {
        assert_eq!(call_number("+45 66 77 87 02").unwrap(), "+4566778702");
        assert_eq!(call_number("(+45) 66-77-87-02").unwrap(), "+4566778702");
        for s in [
            "tel:+4566778702",
            "*21*12345#",
            "+45;reboot",
            "$(touch x)",
            "++4512345678",
            "+45 66 77 87 02,123",
            "",
            "+",
        ] {
            assert!(call_number(s).is_err(), "{s}");
        }
    }

    #[test]
    fn service_codes_are_refused_but_emergency_numbers_are_not() {
        assert_eq!(call_number("112").unwrap(), "112");
        assert_eq!(call_number("911").unwrap(), "911");
        assert!(call_number("1234").unwrap_err().contains("service codes"));
        assert!(call_number("1234567890123456").is_err());
        assert!(call_number(&"1".repeat(61)).is_err());
    }
}
