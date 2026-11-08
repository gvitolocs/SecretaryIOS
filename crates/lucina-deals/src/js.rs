//! The JavaScript semantics the Node deals engine relies on, so the Rust
//! output is the same text and the same JSON: `Number#toFixed`,
//! `Math.round`, `Number#toString`, `Number(value)`, `Date.parse`,
//! `String#localeCompare` (ICU root collation), UTF-16 lengths and an
//! insertion-ordered JSON value with `JSON.stringify` output.

use std::cmp::Ordering;
use std::fmt::Write as _;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

/// `Number.prototype.toString()` (shortest round-trip digits, JS layout).
pub fn num_to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let sign = if x < 0.0 { "-" } else { "" };
    let sci = format!("{:e}", x.abs());
    let (mantissa, exp) = sci.split_once('e').expect("exponent form");
    let exp: i64 = exp.parse().expect("exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i64;
    let n = exp + 1;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let e = if e < 0 {
            format!("-{}", -e)
        } else {
            format!("+{e}")
        };
        if k == 1 {
            format!("{digits}e{e}")
        } else {
            format!("{}.{}e{e}", &digits[..1], &digits[1..])
        }
    };
    format!("{sign}{body}")
}

/// `Number.prototype.toFixed(digits)`: the exact binary value, ties up.
pub fn to_fixed(x: f64, digits: usize) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() || x.abs() >= 1e21 {
        return num_to_string(x);
    }
    // Rust prints the exact decimal expansion for large precisions; every
    // finite double has at most 1074 fractional digits.
    let exact = format!("{:.1100}", x.abs());
    let (int, frac) = exact.split_once('.').expect("fraction");
    let mut out: Vec<u8> = int.bytes().chain(frac.bytes().take(digits)).collect();
    if frac.as_bytes()[digits] >= b'5' {
        let mut i = out.len();
        loop {
            if i == 0 {
                out.insert(0, b'1');
                break;
            }
            i -= 1;
            if out[i] == b'9' {
                out[i] = b'0';
            } else {
                out[i] += 1;
                break;
            }
        }
    }
    let split = out.len() - digits;
    let mut s = String::new();
    if x < 0.0 {
        s.push('-');
    }
    s.push_str(std::str::from_utf8(&out[..split]).expect("ascii"));
    if digits > 0 {
        s.push('.');
        s.push_str(std::str::from_utf8(&out[split..]).expect("ascii"));
    }
    s
}

/// `Math.round`: nearest integer, ties toward +∞.
pub fn round(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

/// JS `Number(value)` on a JSON value (`undefined` is the caller's `None`).
pub fn to_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => string_to_number(s),
        Value::Array(a) => match a.as_slice() {
            [] => 0.0,
            [one] => string_to_number(&value_text(one)),
            _ => f64::NAN,
        },
        Value::Object(_) => f64::NAN,
    }
}

fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// JS `StringToNumber`.
pub fn string_to_number(s: &str) -> f64 {
    let t = s.trim_matches(is_js_space);
    if t.is_empty() {
        return 0.0;
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return u64::from_str_radix(rest, radix)
                .map(|v| v as f64)
                .unwrap_or(f64::NAN);
        }
    }
    match t {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    // Rust also reads "inf"/"nan", which JS rejects; both end as non-finite.
    if t.bytes()
        .any(|b| b.is_ascii_alphabetic() && b != b'e' && b != b'E')
    {
        return f64::NAN;
    }
    t.parse::<f64>().unwrap_or(f64::NAN)
}

/// The sources' `number(n)`: `null`/`undefined`/non-finite → `None`.
pub fn number(value: Option<&Value>) -> Option<f64> {
    match value {
        None | Some(Value::Null) => None,
        Some(v) => Some(to_number(v)).filter(|n| n.is_finite()),
    }
}

/// JS `String(value)` for a JSON value.
pub fn value_text(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => num_to_string(n.as_f64().unwrap_or(f64::NAN)),
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|v| match v {
                Value::Null => String::new(),
                v => value_text(v),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// `${value}` with `undefined` for a missing value.
pub fn display(value: Option<&Value>) -> String {
    value.map_or_else(|| "undefined".into(), value_text)
}

/// JS truthiness of a JSON value (`undefined` is the caller's `None`).
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// A string field: strings as they are, other values as JS would print them.
pub fn opt_string(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(v @ (Value::Number(_) | Value::Bool(_))) => Some(value_text(v)),
        Some(_) => None,
    }
}

/// A JSON number the way `JSON.stringify` writes it: integers without `.0`.
pub fn num_value(x: f64) -> Value {
    if !x.is_finite() {
        return Value::Null;
    }
    if x.fract() == 0.0 && x.abs() < 9_007_199_254_740_992.0 {
        return Value::from(x as i64);
    }
    serde_json::Number::from_f64(x).map_or(Value::Null, Value::Number)
}

pub fn opt_num_value(x: Option<f64>) -> Value {
    x.map_or(Value::Null, num_value)
}

/// `Date.parse` for the ISO forms the deal sources send
/// (`…Z`, `…+01:00`, `…+0000`, date only). A local date-time without an
/// offset is read as UTC.
pub fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<u32> {
        let part = s.get(from..from + len)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let (y, mo, d) = (num(0, 4)? as i32, num(5, 2)?, num(8, 2)?);
    let date = NaiveDate::from_ymd_opt(y, mo, d)?;
    if b.len() == 10 {
        return Some(
            Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0)?)
                .timestamp_millis(),
        );
    }
    if !(b[10] == b'T' || b[10] == b't' || b[10] == b' ') || b.len() < 16 || b[13] != b':' {
        return None;
    }
    let (h, mi) = (num(11, 2)?, num(14, 2)?);
    let mut i = 16;
    let mut sec = 0;
    let mut ms = 0;
    if b.get(i) == Some(&b':') {
        sec = num(i + 1, 2)?;
        i += 3;
        if matches!(b.get(i), Some(b'.') | Some(b',')) {
            let start = i + 1;
            let mut end = start;
            while end < b.len() && b[end].is_ascii_digit() {
                end += 1;
            }
            if end == start {
                return None;
            }
            let frac = &s[start..end];
            let mut digits: String = frac.chars().take(3).collect();
            while digits.len() < 3 {
                digits.push('0');
            }
            ms = digits.parse().ok()?;
            i = end;
        }
    }
    let rest = s[i..].trim_start();
    let offset_s: i64 = match rest {
        "" | "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let digits: String = rest[1..].chars().filter(|c| *c != ':').collect();
            if digits.len() != 4 || !digits.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let oh: i64 = digits[..2].parse().ok()?;
            let om: i64 = digits[2..].parse().ok()?;
            sign * (oh * 3600 + om * 60)
        }
    };
    if h > 24 || mi > 59 || sec > 59 || (h == 24 && (mi, sec, ms) != (0, 0, 0)) {
        return None;
    }
    let base = Utc
        .from_utc_datetime(&date.and_hms_opt(0, 0, 0)?)
        .timestamp_millis();
    Some(
        base + i64::from(h) * 3_600_000
            + i64::from(mi) * 60_000
            + i64::from(sec) * 1000
            + i64::from(ms)
            - offset_s * 1000,
    )
}

/// `Date.prototype.toISOString`.
pub fn iso_string(ms: i64) -> String {
    let t: DateTime<Utc> = Utc.timestamp_millis_opt(ms).single().expect("valid time");
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `s.slice(0, n)` in UTF-16 units (a split surrogate pair is dropped).
pub fn utf16_prefix(s: &str, n: usize) -> String {
    let mut units = 0;
    let mut out = String::new();
    for c in s.chars() {
        units += c.len_utf16();
        if units > n {
            break;
        }
        out.push(c);
    }
    out
}

// ICU root collation (what `localeCompare` uses in Node's default locale),
// exact for ASCII: whitespace < punctuation < symbols < digits < letters,
// letters case-insensitive at the primary level and lowercase first.
const PUNCTUATION: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

fn collation_elements(s: &str) -> Vec<(u32, u32, u32)> {
    let mut out = Vec::new();
    for c in s.nfd() {
        let cp = c as u32;
        match c {
            '\u{0}'..='\u{8}' | '\u{e}'..='\u{1f}' | '\u{7f}' => {}
            '\t' => out.push((1, 5, 1)),
            '\n' => out.push((2, 5, 1)),
            '\u{b}' => out.push((3, 5, 1)),
            '\u{c}' => out.push((4, 5, 1)),
            '\r' => out.push((5, 5, 1)),
            ' ' => out.push((6, 5, 1)),
            '0'..='9' => out.push((2_000_000 + cp, 5, 1)),
            'a'..='z' => out.push((3_000_000 + (cp - 'a' as u32) * 4, 5, 1)),
            'A'..='Z' => out.push((3_000_000 + (cp - 'A' as u32) * 4, 5, 2)),
            // ICU expands æ to a+e with its own tertiary weight.
            'æ' | 'Æ' => {
                let t = if c == 'æ' { 3 } else { 4 };
                out.push((3_000_000, 5, t));
                out.push((3_000_000 + 4 * 4, 5, t));
            }
            'ø' | 'Ø' => {
                out.push((3_000_000 + 14 * 4, 5, if c == 'ø' { 1 } else { 2 }));
                out.push((0, 9, 0));
            }
            '\u{300}'..='\u{36f}' => out.push((0, 10 + cp - 0x300, 0)),
            _ if c.is_ascii() => {
                let i = PUNCTUATION.find(c).map_or(60, |i| i as u32);
                out.push((10 + i, 5, 1));
            }
            _ if c.is_alphabetic() => {
                let lower = c.to_lowercase().next().unwrap_or(c) as u32;
                out.push((4_000_000 + lower, 5, if c.is_uppercase() { 2 } else { 1 }));
            }
            _ => out.push((1000 + cp, 5, 1)),
        }
    }
    out
}

/// `a.localeCompare(b)`.
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (ea, eb) = (collation_elements(a), collation_elements(b));
    let level = |e: &[(u32, u32, u32)], k: usize| -> Vec<u32> {
        e.iter()
            .map(|w| [w.0, w.1, w.2][k])
            .filter(|w| *w != 0)
            .collect()
    };
    for k in 0..3 {
        match level(&ea, k).cmp(&level(&eb, k)) {
            Ordering::Equal => {}
            other => return other,
        }
    }
    Ordering::Equal
}

/// One JS comparator term (`a - b || …`): `None` when it is 0 or NaN.
pub fn cmp_num(diff: f64) -> Option<Ordering> {
    if diff > 0.0 {
        Some(Ordering::Greater)
    } else if diff < 0.0 {
        Some(Ordering::Less)
    } else {
        None
    }
}

/// An insertion-ordered JSON value (what `JSON.parse` gives Node), so files
/// Node wrote are read and written back byte for byte.
#[derive(Debug, Clone, PartialEq)]
pub enum JsValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsValue>),
    Object(Vec<(String, JsValue)>),
}

impl JsValue {
    pub fn get(&self, key: &str) -> Option<&JsValue> {
        match self {
            JsValue::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// JS `Number(value)`.
    pub fn to_number(&self) -> f64 {
        match self {
            JsValue::Null => 0.0,
            JsValue::Bool(b) => f64::from(u8::from(*b)),
            JsValue::Number(n) => *n,
            JsValue::String(s) => string_to_number(s),
            JsValue::Array(a) if a.is_empty() => 0.0,
            JsValue::Array(a) if a.len() == 1 => match &a[0] {
                JsValue::Null => 0.0,
                JsValue::String(s) => string_to_number(s),
                other => other.to_number(),
            },
            _ => f64::NAN,
        }
    }

    /// `JSON.stringify(value)` (`indent` 0) or `JSON.stringify(value, null, indent)`.
    pub fn stringify(&self, indent: usize) -> String {
        let mut out = String::new();
        self.write(&mut out, indent, 0);
        out
    }

    fn write(&self, out: &mut String, indent: usize, depth: usize) {
        let pad = |out: &mut String, depth: usize| {
            if indent > 0 {
                out.push('\n');
                out.push_str(&" ".repeat(indent * depth));
            }
        };
        match self {
            JsValue::Null => out.push_str("null"),
            JsValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            JsValue::Number(n) if n.is_finite() => out.push_str(&num_to_string(*n)),
            JsValue::Number(_) => out.push_str("null"),
            JsValue::String(s) => quote(out, s),
            JsValue::Array(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    pad(out, depth + 1);
                    item.write(out, indent, depth + 1);
                }
                pad(out, depth);
                out.push(']');
            }
            JsValue::Object(entries) => {
                if entries.is_empty() {
                    out.push_str("{}");
                    return;
                }
                out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    pad(out, depth + 1);
                    quote(out, k);
                    out.push(':');
                    if indent > 0 {
                        out.push(' ');
                    }
                    v.write(out, indent, depth + 1);
                }
                pad(out, depth);
                out.push('}');
            }
        }
    }

    pub fn parse(text: &str) -> serde_json::Result<JsValue> {
        serde_json::from_str(text)
    }
}

/// `JSON.stringify` string quoting.
pub fn quote(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

impl<'de> Deserialize<'de> for JsValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = JsValue;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_unit<E>(self) -> Result<JsValue, E> {
                Ok(JsValue::Null)
            }
            fn visit_none<E>(self) -> Result<JsValue, E> {
                Ok(JsValue::Null)
            }
            fn visit_bool<E>(self, v: bool) -> Result<JsValue, E> {
                Ok(JsValue::Bool(v))
            }
            fn visit_i64<E>(self, v: i64) -> Result<JsValue, E> {
                Ok(JsValue::Number(v as f64))
            }
            fn visit_u64<E>(self, v: u64) -> Result<JsValue, E> {
                Ok(JsValue::Number(v as f64))
            }
            fn visit_f64<E>(self, v: f64) -> Result<JsValue, E> {
                Ok(JsValue::Number(v))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<JsValue, E> {
                Ok(JsValue::String(v.to_owned()))
            }
            fn visit_string<E>(self, v: String) -> Result<JsValue, E> {
                Ok(JsValue::String(v))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<JsValue, A::Error> {
                let mut items = Vec::new();
                while let Some(v) = seq.next_element()? {
                    items.push(v);
                }
                Ok(JsValue::Array(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<JsValue, A::Error> {
                let mut entries: Vec<(String, JsValue)> = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, JsValue>()? {
                    // JSON.parse: a repeated key keeps its first position, last value.
                    match entries.iter_mut().find(|(old, _)| *old == k) {
                        Some(slot) => slot.1 = v,
                        None => entries.push((k, v)),
                    }
                }
                Ok(JsValue::Object(entries))
            }
        }
        deserializer.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_print_like_js() {
        assert_eq!(num_to_string(3.0), "3");
        assert_eq!(num_to_string(19.95), "19.95");
        assert_eq!(num_to_string(0.115466), "0.115466");
        assert_eq!(num_to_string(1e21), "1e+21");
        assert_eq!(num_to_string(1e-7), "1e-7");
        assert_eq!(num_to_string(123e-20), "1.23e-18");
        assert_eq!(num_to_string(0.000001), "0.000001");
        assert_eq!(num_to_string(-2.5), "-2.5");
        assert_eq!(to_fixed(1.005, 2), "1.00");
        assert_eq!(to_fixed(0.125, 2), "0.13");
        assert_eq!(to_fixed(2.5, 0), "3");
        assert_eq!(to_fixed(9.995, 2), "9.99");
        assert_eq!(to_fixed(99.999, 2), "100.00");
        assert_eq!(to_fixed(-0.004, 2), "-0.00");
        assert_eq!(to_fixed(0.56, 1), "0.6");
        assert_eq!(to_fixed(285.7142857, 2), "285.71");
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(-2.5), -2.0);
        assert_eq!(round(0.49999999999999994), 0.0);
        assert_eq!(string_to_number(" 12.5 "), 12.5);
        assert_eq!(string_to_number(""), 0.0);
        assert_eq!(string_to_number("0x10"), 16.0);
        assert!(string_to_number("inf").is_nan());
    }

    #[test]
    fn dates_parse_like_v8() {
        assert_eq!(
            parse_date("2026-10-24T09:30:00Z"),
            parse_date("2026-10-24T11:30:00+0200")
        );
        assert_eq!(
            parse_date("2026-10-28T23:00:00+0000"),
            parse_date("2026-10-28T23:00:00.000Z")
        );
        assert_eq!(
            iso_string(parse_date("2026-10-24T09:30:00Z").unwrap()),
            "2026-10-24T09:30:00.000Z"
        );
        assert_eq!(parse_date("nope"), None);
    }

    #[test]
    fn locale_compare_is_icu_like() {
        use Ordering::*;
        assert_eq!(locale_compare("a", "B"), Less);
        assert_eq!(locale_compare("a", "A"), Less);
        assert_eq!(locale_compare("_", "-"), Less);
        assert_eq!(locale_compare("-", "0"), Less);
        assert_eq!(locale_compare("tjek:a_", "tjek:a-"), Less);
        assert_eq!(locale_compare("tjek:9", "tjek:A"), Less);
        assert_eq!(locale_compare("fw:1:2", "fw:12"), Less);
    }

    #[test]
    fn ordered_json_round_trips() {
        let text = "{\n  \"b\": {},\n  \"a\": [\n    1,\n    \"x\\n\"\n  ],\n  \"c\": null\n}";
        let v = JsValue::parse(text).unwrap();
        assert_eq!(v.stringify(2), text);
        assert_eq!(v.stringify(0), "{\"b\":{},\"a\":[1,\"x\\n\"],\"c\":null}");
    }
}
