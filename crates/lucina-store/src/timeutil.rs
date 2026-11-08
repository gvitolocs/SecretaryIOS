//! Time and string helpers that match the JavaScript behaviour the Node
//! stores rely on (`Date#toISOString`, `Date.parse`, `String#slice`).

use chrono::{DateTime, LocalResult, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

/// `Date#toISOString()`: always milliseconds and `Z`.
pub fn iso_ms(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

pub fn now_iso() -> String {
    iso_ms(Utc::now())
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// A practical subset of `Date.parse`: RFC 3339 / ISO 8601 with an offset,
/// date-time without offset (UTC, as in the container), and date-only (UTC).
pub fn parse_js_date(value: &str) -> Option<DateTime<Utc>> {
    let s = value.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    // ISO without seconds but with offset: 2026-10-22T09:30+02:00 / 09:30Z
    for fmt in ["%Y-%m-%dT%H:%M%:z", "%Y-%m-%dT%H:%M%#z"] {
        if let Ok(dt) = DateTime::parse_from_str(s, fmt) {
            return Some(dt.with_timezone(&Utc));
        }
    }
    if let Some(stripped) = s.strip_suffix('Z') {
        if let Ok(n) = NaiveDateTime::parse_from_str(stripped, "%Y-%m-%dT%H:%M") {
            return Some(Utc.from_utc_datetime(&n));
        }
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(Utc.from_utc_datetime(&n));
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0)?));
    }
    None
}

/// `YYYY-MM-DD` of `dt` in `tz` (Intl `en-CA`).
pub fn ymd_in_tz(dt: DateTime<Utc>, tz: Tz) -> String {
    dt.with_timezone(&tz).format("%Y-%m-%d").to_string()
}

/// Local wall-clock time in `tz` to UTC. Ambiguous times take the earlier
/// instant; times in a DST gap move forward by the gap (like the Node loop).
pub fn zoned_to_utc(
    tz: Tz,
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<DateTime<Utc>> {
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let naive = date.and_hms_opt(hour, minute, second)?;
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Some(dt.with_timezone(&Utc)),
        LocalResult::Ambiguous(a, _) => Some(a.with_timezone(&Utc)),
        LocalResult::None => {
            let later = naive + chrono::Duration::hours(1);
            tz.from_local_datetime(&later)
                .earliest()
                .map(|dt| dt.with_timezone(&Utc))
        }
    }
}

pub fn parse_tz(name: &str) -> Option<Tz> {
    name.trim().parse::<Tz>().ok()
}

/// `String#slice(0, max)` in UTF-16 code units, without splitting a
/// surrogate pair.
pub fn clip_utf16(s: &str, max: usize) -> String {
    let mut units = 0usize;
    let mut end = 0usize;
    for (i, c) in s.char_indices() {
        let w = c.len_utf16();
        if units + w > max {
            return s[..i].to_owned();
        }
        units += w;
        end = i + c.len_utf8();
    }
    s[..end].to_owned()
}

/// Length in UTF-16 code units (`String#length`).
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// `String#trim()` (JS also trims U+FEFF).
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_and_parse() {
        let dt = parse_js_date("2026-10-22T09:30:00+02:00").unwrap();
        assert_eq!(iso_ms(dt), "2026-10-22T07:30:00.000Z");
        assert_eq!(
            iso_ms(parse_js_date("2026-10-22T07:30:02.118Z").unwrap()),
            "2026-10-22T07:30:02.118Z"
        );
        assert_eq!(
            iso_ms(parse_js_date("2026-10-22").unwrap()),
            "2026-10-22T00:00:00.000Z"
        );
        assert!(parse_js_date("tomorrow").is_none());
    }

    #[test]
    fn zoned() {
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let dt = zoned_to_utc(tz, 2026, 10, 22, 9, 30, 0).unwrap();
        assert_eq!(iso_ms(dt), "2026-10-22T07:30:00.000Z");
        assert_eq!(ymd_in_tz(dt, tz), "2026-10-22");
        // DST gap 2026-03-29 02:30 does not exist in Rome
        assert!(zoned_to_utc(tz, 2026, 3, 29, 2, 30, 0).is_some());
    }

    #[test]
    fn clip() {
        assert_eq!(clip_utf16("abcdef", 3), "abc");
        assert_eq!(clip_utf16("a😀b", 2), "a");
        assert_eq!(clip_utf16("a😀b", 3), "a😀");
        assert_eq!(utf16_len("a😀b"), 4);
    }
}
