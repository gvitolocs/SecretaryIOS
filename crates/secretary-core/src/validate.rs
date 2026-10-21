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

// Limits mirror Lucina's (docs/secretary-api.md in Lucina), so the app gets
// a 400 from us instead of a round trip.

/// Max characters of a chat message.
pub const MAX_MESSAGE_CHARS: usize = 4_000;
/// Max characters of reminder text / natural-language reminder input.
pub const MAX_TEXT_CHARS: usize = 500;
/// Max characters of a shopping `rawName` / `product`.
pub const MAX_SHOPPING_NAME_CHARS: usize = 200;
/// Max length of a chat `sessionId` (`[A-Za-z0-9_-]`).
pub const MAX_SESSION_ID_LEN: usize = 64;
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

/// A chat session id: 1..=64 bytes of `[A-Za-z0-9_-]` (Lucina's rule).
pub fn session_id(field: &'static str, s: &str) -> Result<()> {
    if s.is_empty()
        || s.len() > MAX_SESSION_ID_LEN
        || !s
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(invalid(
            field,
            format!("must be 1..={MAX_SESSION_ID_LEN} characters of A-Z a-z 0-9 _ -"),
        ));
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

/// Max page size of `GET /timeline`.
pub const MAX_TIMELINE_LIMIT: u32 = 200;

/// Client surfaces a chat message may come from.
pub const CHAT_CHANNELS: &[&str] = &["web", "ios"];

/// A decimal integer in `min..=max` (no sign, no leading `+`).
pub fn bounded_int(field: &'static str, s: &str, min: u32, max: u32) -> Result<u32> {
    let n = if !s.is_empty() && s.len() <= 10 && s.bytes().all(|b| b.is_ascii_digit()) {
        s.parse::<u32>().ok()
    } else {
        None
    };
    match n {
        Some(n) if (min..=max).contains(&n) => Ok(n),
        _ => Err(invalid(field, format!("must be an integer {min}..={max}"))),
    }
}

/// A chat channel: one of [`CHAT_CHANNELS`].
pub fn chat_channel(field: &'static str, s: &str) -> Result<()> {
    if CHAT_CHANNELS.contains(&s) {
        Ok(())
    } else {
        Err(invalid(
            field,
            format!("must be one of {}", CHAT_CHANNELS.join(", ")),
        ))
    }
}

/// A JSON object (shopping bodies are passed through, but must be objects).
pub fn object(field: &'static str, v: &Value) -> Result<()> {
    match v {
        Value::Object(_) => Ok(()),
        _ => Err(invalid(field, "must be a JSON object")),
    }
}

/// `POST /shopping` body: object with string `rawName` (required) and
/// optional string `product`, each 1..=200 characters. Other fields pass.
pub fn shopping_item(v: &Value) -> Result<()> {
    object("body", v)?;
    match v.get("rawName") {
        Some(Value::String(s)) => text("rawName", s, MAX_SHOPPING_NAME_CHARS)?,
        _ => return Err(invalid("rawName", "is required and must be a string")),
    }
    match v.get("product") {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) => text("product", s, MAX_SHOPPING_NAME_CHARS)?,
        Some(_) => return Err(invalid("product", "must be a string")),
    }
    Ok(())
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
        assert!(shopping_item(&json!({"rawName": "latte", "product": "milk"})).is_ok());
        assert!(shopping_item(&json!({"name": "milk"})).is_err());
        assert!(shopping_item(&json!({"rawName": "x", "product": 3})).is_err());
        assert!(session_id("sessionId", "iphone_1-a").is_ok());
        assert!(session_id("sessionId", "a.b").is_err());
        assert_eq!(bounded_int("limit", "50", 1, 200).unwrap(), 50);
        for bad in ["0", "201", "-1", "+5", "abc", "", "1e2", "99999999999"] {
            assert!(bounded_int("limit", bad, 1, 200).is_err(), "{bad:?}");
        }
        assert!(chat_channel("channel", "ios").is_ok());
        assert!(chat_channel("channel", "telegram").is_err());
    }
}
