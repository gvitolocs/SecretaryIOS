//! Number formats: what kind of value a cell holds (date, time, duration, number, text) and how
//! Excel would show it. Display only: the file keeps the raw value and its format code.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Timelike};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FmtKind {
    General,
    Number,
    Percent,
    Date,
    Time,
    /// Elapsed time (`[h]:mm`): hours past 24, can be negative.
    Duration,
    DateTime,
    Text,
}

impl FmtKind {
    pub fn is_temporal(self) -> bool {
        matches!(
            self,
            FmtKind::Date | FmtKind::Time | FmtKind::Duration | FmtKind::DateTime
        )
    }
}

/// The built-in format codes (ECMA-376 18.8.30), en-US rendering.
pub fn builtin(id: u32) -> Option<&'static str> {
    Some(match id {
        0 => "General",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        14 => "yyyy-mm-dd",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "yyyy-mm-dd h:mm",
        37 => "#,##0 ;(#,##0)",
        38 => "#,##0 ;[Red](#,##0)",
        39 => "#,##0.00;(#,##0.00)",
        40 => "#,##0.00;[Red](#,##0.00)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mm:ss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

/// Split on `;` outside quotes and escapes.
fn sections(code: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quoted, mut esc) = (0, false, false);
    for (i, c) in code.char_indices() {
        if esc {
            esc = false;
            continue;
        }
        match c {
            '\\' => esc = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                out.push(&code[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&code[start..]);
    out
}

/// The section's letters with literals, escapes and brackets removed (elapsed brackets become
/// `E`), lower-cased.
fn skeleton(sec: &str) -> String {
    let mut out = String::new();
    let mut it = sec.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' => {
                for d in it.by_ref() {
                    if d == '"' {
                        break;
                    }
                }
            }
            '\\' | '_' | '*' => {
                it.next();
            }
            '[' => {
                let mut inner = String::new();
                for d in it.by_ref() {
                    if d == ']' {
                        break;
                    }
                    inner.push(d);
                }
                let l = inner.to_ascii_lowercase();
                if !l.is_empty() && l.chars().all(|c| c == 'h') {
                    out.push('E');
                    out.push('h');
                } else if !l.is_empty() && l.chars().all(|c| c == 'm') {
                    out.push('E');
                    out.push('n');
                } else if !l.is_empty() && l.chars().all(|c| c == 's') {
                    out.push('E');
                    out.push('s');
                } else if l.starts_with("$-f400") {
                    out.push('h');
                } else if l.starts_with("$-f800") {
                    out.push('d');
                }
            }
            c => out.push(c.to_ascii_lowercase()),
        }
    }
    out
}

pub fn classify(code: &str) -> FmtKind {
    let secs = sections(code);
    let first = secs[0];
    if first.trim().eq_ignore_ascii_case("general") || first.trim().is_empty() {
        return FmtKind::General;
    }
    let sk = skeleton(first);
    if sk.contains('E') {
        return FmtKind::Duration;
    }
    let has_time =
        sk.contains('h') || sk.contains('s') || sk.contains("am/pm") || sk.contains("a/p");
    let has_date = sk.contains('y') || sk.contains('d') || (sk.contains('m') && !has_time);
    match (has_date, has_time) {
        (true, true) => FmtKind::DateTime,
        (true, false) => FmtKind::Date,
        (false, true) => FmtKind::Time,
        _ if sk.contains('@') && !sk.contains('0') && !sk.contains('#') => FmtKind::Text,
        _ if sk.contains('%') => FmtKind::Percent,
        _ if sk.contains('0') || sk.contains('#') || sk.contains('?') => FmtKind::Number,
        _ => FmtKind::General,
    }
}

/// Excel's serial day number (1900 system unless `date1904`) as a date and time, rounded to the
/// second.
pub fn serial_to_datetime(v: f64, date1904: bool) -> Option<NaiveDateTime> {
    if !v.is_finite() || v < 0.0 || v > 2_958_466.0 {
        return None;
    }
    let mut days = v.floor() as i64;
    let mut secs = ((v - v.floor()) * 86400.0).round() as i64;
    if secs >= 86400 {
        days += 1;
        secs -= 86400;
    }
    let base = if date1904 {
        NaiveDate::from_ymd_opt(1904, 1, 1)?
    } else if days < 60 {
        // Before Excel's phantom 1900-02-29.
        NaiveDate::from_ymd_opt(1899, 12, 31)?
    } else {
        NaiveDate::from_ymd_opt(1899, 12, 30)?
    };
    let d = base.checked_add_signed(Duration::days(days))?;
    d.and_hms_opt(0, 0, 0)?
        .checked_add_signed(Duration::seconds(secs))
}

pub fn date_to_serial(d: NaiveDate, date1904: bool) -> f64 {
    let base = if date1904 {
        NaiveDate::from_ymd_opt(1904, 1, 1).unwrap()
    } else {
        NaiveDate::from_ymd_opt(1899, 12, 30).unwrap()
    };
    let days = (d - base).num_days();
    if !date1904 && days < 61 {
        // Dates before 1900-03-01 count from 1899-12-31.
        (days - 1) as f64
    } else {
        days as f64
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// `h:mm` from a fraction of a day, rounded to the minute; hours may pass 24; negative gets `-`.
pub fn hhmm(v: f64) -> String {
    let neg = v < 0.0;
    let mins = (v.abs() * 1440.0).round() as i64;
    format!(
        "{}{:02}:{:02}",
        if neg { "-" } else { "" },
        mins / 60,
        mins % 60
    )
}

fn render_temporal(sec: &str, v: f64, date1904: bool, kind: FmtKind) -> Option<String> {
    let neg = v < 0.0;
    if neg && kind != FmtKind::Duration {
        return None;
    }
    let av = v.abs();
    let dt = if kind == FmtKind::Duration {
        None
    } else {
        Some(serial_to_datetime(av, date1904)?)
    };
    let total_secs = (av * 86400.0).round() as i64;
    let ampm = {
        let l = skeleton(sec);
        l.contains("am/pm") || l.contains("a/p")
    };
    let chars: Vec<char> = sec.chars().collect();
    let mut out = String::new();
    if neg {
        out.push('-');
    }
    // Whether the last date/time token was an hour (then `m` is minutes).
    let mut after_hour = false;
    let mut i = 0;
    let run = |i: usize, c: char| {
        let mut j = i;
        while j < chars.len() && chars[j].eq_ignore_ascii_case(&c) {
            j += 1;
        }
        j - i
    };
    let next_is_seconds = |from: usize| {
        let mut j = from;
        while j < chars.len() {
            let c = chars[j].to_ascii_lowercase();
            if c == 's' {
                return true;
            }
            if c.is_ascii_alphabetic() || c == '[' {
                return false;
            }
            j += 1;
        }
        false
    };
    let (h24, mi, se) = match dt {
        Some(d) => (d.hour() as i64, d.minute() as i64, d.second() as i64),
        None => (
            (total_secs / 3600) % 24,
            (total_secs / 60) % 60,
            total_secs % 60,
        ),
    };
    while i < chars.len() {
        let c = chars[i];
        match c.to_ascii_lowercase() {
            '"' => {
                i += 1;
                while i < chars.len() && chars[i] != '"' {
                    out.push(chars[i]);
                    i += 1;
                }
                i += 1;
            }
            '\\' => {
                if let Some(&d) = chars.get(i + 1) {
                    out.push(d);
                }
                i += 2;
            }
            '_' => {
                out.push(' ');
                i += 2;
            }
            '*' => i += 2,
            '[' => {
                let mut j = i + 1;
                let mut inner = String::new();
                while j < chars.len() && chars[j] != ']' {
                    inner.push(chars[j]);
                    j += 1;
                }
                let l = inner.to_ascii_lowercase();
                let w = l.len();
                if !l.is_empty() && l.chars().all(|c| c == 'h') {
                    out.push_str(&format!("{:0w$}", total_secs / 3600));
                    after_hour = true;
                } else if !l.is_empty() && l.chars().all(|c| c == 'm') {
                    out.push_str(&format!("{:0w$}", total_secs / 60));
                } else if !l.is_empty() && l.chars().all(|c| c == 's') {
                    out.push_str(&format!("{:0w$}", total_secs));
                }
                i = j + 1;
            }
            'y' => {
                let n = run(i, 'y');
                let y = dt?.year();
                if n <= 2 {
                    out.push_str(&format!("{:02}", y % 100));
                } else {
                    out.push_str(&format!("{y:04}"));
                }
                i += n;
            }
            'd' => {
                let n = run(i, 'd');
                let d = dt?;
                match n {
                    1 => out.push_str(&d.day().to_string()),
                    2 => out.push_str(&format!("{:02}", d.day())),
                    3 => out.push_str(&DAYS[d.weekday().num_days_from_monday() as usize][..3]),
                    _ => out.push_str(DAYS[d.weekday().num_days_from_monday() as usize]),
                }
                after_hour = false;
                i += n;
            }
            'm' => {
                let n = run(i, 'm');
                if (after_hour || next_is_seconds(i + n)) && n <= 2 {
                    out.push_str(&if n == 1 {
                        mi.to_string()
                    } else {
                        format!("{mi:02}")
                    });
                } else {
                    let m = dt?.month() as usize;
                    match n {
                        1 => out.push_str(&m.to_string()),
                        2 => out.push_str(&format!("{m:02}")),
                        3 => out.push_str(&MONTHS[m - 1][..3]),
                        5 => out.push_str(&MONTHS[m - 1][..1]),
                        _ => out.push_str(MONTHS[m - 1]),
                    }
                }
                after_hour = false;
                i += n;
            }
            'h' => {
                let n = run(i, 'h');
                let h = if ampm { (h24 + 11) % 12 + 1 } else { h24 };
                out.push_str(&if n == 1 {
                    h.to_string()
                } else {
                    format!("{h:02}")
                });
                after_hour = true;
                i += n;
            }
            's' => {
                let n = run(i, 's');
                out.push_str(&if n == 1 {
                    se.to_string()
                } else {
                    format!("{se:02}")
                });
                after_hour = false;
                i += n;
            }
            'a' if chars[i..]
                .iter()
                .take(5)
                .collect::<String>()
                .eq_ignore_ascii_case("am/pm") =>
            {
                out.push_str(if h24 < 12 { "AM" } else { "PM" });
                i += 5;
            }
            'a' if chars[i..]
                .iter()
                .take(3)
                .collect::<String>()
                .eq_ignore_ascii_case("a/p") =>
            {
                out.push(if h24 < 12 { 'A' } else { 'P' });
                i += 3;
            }
            '.' if chars.get(i + 1) == Some(&'0') => {
                // Fractions of a second: not shown.
                i += 1;
                while i < chars.len() && chars[i] == '0' {
                    i += 1;
                }
            }
            '@' => i += 1,
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    Some(out)
}

fn group_thousands(int: &str) -> String {
    let b = int.as_bytes();
    let mut out = String::new();
    for (i, c) in b.iter().enumerate() {
        if i > 0 && (b.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*c as char);
    }
    out
}

fn render_number(sec: &str, v: f64, percent: bool) -> String {
    let sk = skeleton(sec);
    let decimals = sk
        .split_once('.')
        .map(|(_, f)| {
            f.chars()
                .take_while(|c| matches!(c, '0' | '#' | '?'))
                .count()
        })
        .unwrap_or(0);
    let thousands = sk.split('.').next().unwrap_or("").contains(',');
    let x = if percent { v * 100.0 } else { v };
    let s = format!("{:.*}", decimals, x.abs());
    let (int, frac) = s.split_once('.').map_or((s.as_str(), ""), |(a, b)| (a, b));
    let int = if thousands {
        group_thousands(int)
    } else {
        int.to_string()
    };
    let mut out = String::new();
    if x < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0') {
        out.push('-');
    }
    out.push_str(&int);
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    if percent {
        out.push('%');
    }
    out
}

/// Excel's General: up to 11 significant digits, no trailing zeros.
pub fn general(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    if v.abs() >= 1e11 || (v != 0.0 && v.abs() < 1e-9) {
        return format!("{v:.5E}");
    }
    let int_digits = if v.abs() < 1.0 {
        0
    } else {
        v.abs().log10().floor() as usize + 1
    };
    let dec = 10usize.saturating_sub(int_digits);
    let s = format!("{v:.dec$}");
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    if s == "-0" {
        "0".into()
    } else {
        s
    }
}

/// How a number shows with this format code.
pub fn format_number(code: &str, v: f64, date1904: bool) -> String {
    let kind = classify(code);
    let secs = sections(code);
    let sec = if v < 0.0 && secs.len() > 1 && !kind.is_temporal() {
        secs[1]
    } else {
        secs[0]
    };
    let v_abs_for_neg = if v < 0.0 && secs.len() > 1 && !kind.is_temporal() {
        -v
    } else {
        v
    };
    match kind {
        FmtKind::Date | FmtKind::Time | FmtKind::DateTime | FmtKind::Duration => {
            render_temporal(sec, v, date1904, kind).unwrap_or_else(|| general(v))
        }
        FmtKind::Percent => render_number(sec, v_abs_for_neg, true),
        FmtKind::Number => {
            let out = render_number(sec, v_abs_for_neg, false);
            if v < 0.0 && secs.len() > 1 {
                // The negative section carries its own sign or parentheses.
                let sk = sec.replace(['"', '\\'], "");
                if sk.starts_with('(') {
                    return format!("({out})");
                }
                if sk.starts_with('-') {
                    return format!("-{out}");
                }
            }
            out
        }
        _ => general(v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds() {
        assert_eq!(classify("[h]:mm;@"), FmtKind::Duration);
        assert_eq!(classify("hh:mm;@"), FmtKind::Time);
        assert_eq!(classify("h:mm"), FmtKind::Time);
        assert_eq!(classify("ddd\\ dd\\ mmmm\\ yyyy"), FmtKind::Date);
        assert_eq!(classify("mmmm"), FmtKind::Date);
        assert_eq!(classify("[$-F400]h:mm:ss\\ AM/PM"), FmtKind::Time);
        assert_eq!(classify("yyyy-mm-dd h:mm"), FmtKind::DateTime);
        assert_eq!(classify("0.0"), FmtKind::Number);
        assert_eq!(classify("0%"), FmtKind::Percent);
        assert_eq!(classify("@"), FmtKind::Text);
        assert_eq!(classify("General"), FmtKind::General);
        assert_eq!(classify("\"-\"hh:mm"), FmtKind::Time);
    }

    #[test]
    fn dates() {
        // 46327 is 2026-11-01 (the November sheet's first day).
        let d = serial_to_datetime(46327.0, false).unwrap();
        assert_eq!(d.date(), NaiveDate::from_ymd_opt(2026, 11, 1).unwrap());
        assert_eq!(
            date_to_serial(NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(), false),
            46327.0
        );
        assert_eq!(
            date_to_serial(NaiveDate::from_ymd_opt(1900, 1, 1).unwrap(), false),
            1.0
        );
        assert_eq!(
            serial_to_datetime(1.0, false).unwrap().date(),
            NaiveDate::from_ymd_opt(1900, 1, 1).unwrap()
        );
        assert_eq!(
            format_number("ddd\\ dd\\ mmmm\\ yyyy", 46327.0, false),
            "Sun 01 November 2026"
        );
        assert_eq!(format_number("dd", 46327.0, false), "01");
        assert_eq!(format_number("mmmm", 46327.0, false), "November");
        assert_eq!(format_number("yyyy-mm-dd", 46327.0, false), "2026-11-01");
    }

    #[test]
    fn times() {
        assert_eq!(format_number("h:mm", 0.375, false), "9:00");
        assert_eq!(
            format_number("hh:mm;@", 0.30833333333333335, false),
            "07:24"
        );
        assert_eq!(
            format_number("[h]:mm;@", 1.5416666666666667, false),
            "37:00"
        );
        assert_eq!(format_number("[h]:mm;@", -0.25, false), "-6:00");
        assert_eq!(format_number("h:mm AM/PM", 0.75, false), "6:00 PM");
        assert_eq!(format_number("mm:ss", 0.5 / 1440.0, false), "00:30");
        assert_eq!(hhmm(-0.30833333333333335), "-07:24");
    }

    #[test]
    fn numbers() {
        assert_eq!(format_number("0.0", 7.4, false), "7.4");
        assert_eq!(
            format_number("#,##0.00", 1234567.891, false),
            "1,234,567.89"
        );
        assert_eq!(format_number("0%", 0.256, false), "26%");
        assert_eq!(format_number("#,##0 ;(#,##0)", -5.0, false), "(5)");
        assert_eq!(general(37.0), "37");
        assert_eq!(general(7.4), "7.4");
        assert_eq!(general(1.0 / 3.0), "0.3333333333");
    }
}
