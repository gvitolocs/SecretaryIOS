//! Input validation for everything the app sends before it reaches Lucina.
//!
//! The rules are deliberately small and strict: ids are opaque tokens from a
//! safe alphabet (so they can never smuggle a path segment), free text is
//! length-bounded and free of control characters, timestamps are RFC 3339,
//! dates are `YYYY-MM-DD`.

use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;

use crate::error::{Error, Result};

/// Max characters of a chat message.
pub const MAX_MESSAGE_CHARS: usize = 8_000;
/// Max characters of reminder text / natural-language reminder input.
pub const MAX_TEXT_CHARS: usize = 1_000;
/// Max length of any id (job, reminder, shopping item, session, client message).
pub const MAX_ID_LEN: usize = 128;

fn invalid(field: &'static str, message: impl Into<String>) -> Error {
    Error::Invalid {
        field,
        message: message.into(),
    }
}

/// An opaque id: 1..=128 bytes of `[A-Za-z0-9_.:-]`, not `.` or `..`.
pub fn id(field: &'static str, s: &str) -> Result<()> {
    if s.is_empty() || s.len() > MAX_ID_LEN {
        return Err(invalid(
            field,
            format!("must be 1..={MAX_ID_LEN} characters"),
        ));
    }
    if s == "." || s == ".." {
        return Err(invalid(field, "must not be a dot segment"));
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
    {
        return Err(invalid(field, "may only contain A-Z a-z 0-9 _ - . :"));
    }
    Ok(())
}

/// Optional id: absent is fine, present must pass [`id`].
pub fn opt_id(field: &'static str, s: Option<&str>) -> Result<()> {
    s.map_or(Ok(()), |s| id(field, s))
}

/// Free text: not blank, at most `max` characters, no control characters
/// other than newline and tab.
pub fn text(field: &'static str, s: &str, max: usize) -> Result<()> {
    if s.trim().is_empty() {
        return Err(invalid(field, "must not be empty"));
    }
    if s.chars().count() > max {
        return Err(invalid(field, format!("must be at most {max} characters")));
    }
    if s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(invalid(field, "must not contain control characters"));
    }
    Ok(())
}

/// An RFC 3339 timestamp with offset, e.g. `2026-10-22T09:00:00+02:00`.
pub fn timestamp(field: &'static str, s: &str) -> Result<()> {
    time::OffsetDateTime::parse(s, &Rfc3339)
        .map(|_| ())
        .map_err(|_| invalid(field, "must be an RFC 3339 timestamp with offset"))
}

/// A calendar date `YYYY-MM-DD`.
pub fn date(field: &'static str, s: &str) -> Result<()> {
    time::Date::parse(s, format_description!("[year]-[month]-[day]"))
        .map(|_| ())
        .map_err(|_| invalid(field, "must be a date YYYY-MM-DD"))
}

/// A boolean query flag: `0|1|true|false`, normalised to `0|1`.
pub fn flag(field: &'static str, s: &str) -> Result<&'static str> {
    match s {
        "0" | "false" => Ok("0"),
        "1" | "true" => Ok("1"),
        _ => Err(invalid(field, "must be 0 or 1")),
    }
}

/// A JSON object (shopping bodies are passed through, but must be objects).
pub fn object(field: &'static str, v: &Value) -> Result<()> {
    match v {
        Value::Object(_) => Ok(()),
        _ => Err(invalid(field, "must be a JSON object")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ids() {
        assert!(id("id", "r_1-a.b:c").is_ok());
        for bad in ["", ".", "..", "a/b", "a b", "a%2Fb", &"x".repeat(129)] {
            assert!(id("id", bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn texts() {
        assert!(text("t", "buy milk\nand eggs", 100).is_ok());
        assert!(text("t", "   ", 100).is_err());
        assert!(text("t", "a\u{0}b", 100).is_err());
        assert!(text("t", &"é".repeat(5), 5).is_ok());
        assert!(text("t", &"é".repeat(6), 5).is_err());
    }

    #[test]
    fn times_and_flags() {
        assert!(timestamp("dueAt", "2026-10-22T09:00:00+02:00").is_ok());
        assert!(timestamp("dueAt", "2026-10-22 09:00").is_err());
        assert!(date("date", "2026-02-28").is_ok());
        assert!(date("date", "2026-02-30").is_err());
        assert_eq!(flag("f", "true").unwrap(), "1");
        assert!(flag("f", "yes").is_err());
        assert!(object("body", &json!({"name": "milk"})).is_ok());
        assert!(object("body", &json!(["milk"])).is_err());
    }
}
