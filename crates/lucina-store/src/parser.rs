//! Port of Node's reminder parser (`src/reminders.js`): "alle 21:30 di …",
//! "remind me at 9 to …", "remind me in 2 hours to …", "timer 1 hour and
//! an half", "tra mezz'ora …", "28 August 2026 — …", "2026-08-28 - …".
//!
//! The regexes are the Node ones (fancy-regex for the lookaheads).

use std::sync::LazyLock;

use chrono::{DateTime, Datelike, Duration, Utc};
use chrono_tz::Tz;
use fancy_regex::Regex;

use crate::timeutil::{iso_ms, zoned_to_utc};

fn re(p: &str) -> Regex {
    Regex::new(p).expect("valid reminder regex")
}

/// A parsed reminder.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub text: String,
    pub due_at: DateTime<Utc>,
    /// `YYYY-MM-DD` in the reminder's time zone.
    pub local_date: String,
    pub relative: bool,
}

impl Parsed {
    pub fn due_at_iso(&self) -> String {
        iso_ms(self.due_at)
    }
}

/// Parser failure reason (Node's `error` strings).
pub type ParseError = &'static str;

#[derive(Debug, Clone, Copy)]
pub struct ParseOptions {
    pub tz: Tz,
    pub default_hour: u32,
    pub default_minute: u32,
    pub now: DateTime<Utc>,
}

fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn ymd(y: i32, m: u32, d: u32) -> String {
    format!("{y}-{m:02}-{d:02}")
}

fn today_parts(tz: Tz, now: DateTime<Utc>) -> (i32, u32, u32) {
    let local = now.with_timezone(&tz);
    (local.year(), local.month(), local.day())
}

fn add_days((y, m, d): (i32, u32, u32), days: i64) -> (i32, u32, u32) {
    let date = chrono::NaiveDate::from_ymd_opt(y, m, d).expect("valid date") + Duration::days(days);
    (date.year(), date.month(), date.day())
}

fn month_number(name: &str) -> Option<u32> {
    Some(match name.to_lowercase().as_str() {
        "january" | "jan" => 1,
        "february" | "feb" => 2,
        "march" | "mar" => 3,
        "april" | "apr" => 4,
        "may" => 5,
        "june" | "jun" => 6,
        "july" | "jul" => 7,
        "august" | "aug" => 8,
        "september" | "sep" | "sept" => 9,
        "october" | "oct" => 10,
        "november" | "nov" => 11,
        "december" | "dec" => 12,
        _ => return None,
    })
}

static AT_TIME_1: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)^(?:please\s+)?(?:avvisami|ricordami|remind\s+me(?:\s+to)?|set\s+a\s+reminder(?:\s+(?:for|at))?)\s+(?:(?:oggi|today)\s+)?(?:alle|at|for)\s+(\d{1,2})(?:[:.](\d{2}))?(?:\s+(?:di\s+(?:fare\s+)?|to\s+))?\s*(.*)$",
    )
});
static AT_TIME_2: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)^(?:alle|at)\s+(\d{1,2})[:.](\d{2})\s+(?:di\s+(?:fare\s+)?|to\s+)?\s*(.+)$")
});

/// "alle 21:30 di …", "remind me at 9 to …" (today, or tomorrow when past).
pub fn parse_at_time(text: &str, o: &ParseOptions) -> Result<Parsed, ParseError> {
    let raw = normalize_ws(text);
    if raw.is_empty() {
        return Err("empty_message");
    }
    let caps = match AT_TIME_1.captures(&raw).ok().flatten() {
        Some(c) => c,
        None => match AT_TIME_2.captures(&raw).ok().flatten() {
            Some(c) => c,
            None => return Err("unparsed_time"),
        },
    };
    let hour: u32 = caps
        .get(1)
        .and_then(|m| m.as_str().parse().ok())
        .unwrap_or(99);
    let minute: u32 = match caps.get(2) {
        Some(m) => m.as_str().parse().unwrap_or(99),
        None => o.default_minute,
    };
    let body = normalize_ws(caps.get(3).map(|m| m.as_str()).unwrap_or(""));
    if body.is_empty() {
        return Err("missing_body");
    }
    if hour > 23 || minute > 59 {
        return Err("invalid_time");
    }
    let mut parts = today_parts(o.tz, o.now);
    let mut due =
        zoned_to_utc(o.tz, parts.0, parts.1, parts.2, hour, minute, 0).ok_or("invalid_date")?;
    if due <= o.now {
        parts = add_days(parts, 1);
        due =
            zoned_to_utc(o.tz, parts.0, parts.1, parts.2, hour, minute, 0).ok_or("invalid_date")?;
    }
    Ok(Parsed {
        text: body,
        due_at: due,
        local_date: ymd(parts.0, parts.1, parts.2),
        relative: false,
    })
}

/// "2026-08-28", "28 August 2026", "August 28, 2026" at the default hour.
pub fn parse_date_part(
    date_part: &str,
    o: &ParseOptions,
) -> Option<Result<(DateTime<Utc>, String), ParseError>> {
    static ISO: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d{4})-(\d{2})-(\d{2})$"));
    static NAMED: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d{1,2})\s+([A-Za-z]+)\s+(\d{4})$"));
    static US: LazyLock<Regex> = LazyLock::new(|| re(r"^([A-Za-z]+)\s+(\d{1,2}),?\s+(\d{4})$"));
    let s = normalize_ws(date_part);
    let num = |c: &fancy_regex::Captures<'_, str>, i: usize| {
        c.get(i).and_then(|m| m.as_str().parse::<u32>().ok())
    };
    let parts = if let Some(c) = ISO.captures(s.as_str()).ok().flatten() {
        Some((num(&c, 1)? as i32, num(&c, 2)?, num(&c, 3)?))
    } else if let Some(c) = NAMED.captures(s.as_str()).ok().flatten() {
        month_number(c.get(2)?.as_str()).and_then(|m| Some((num(&c, 3)? as i32, m, num(&c, 1)?)))
    } else if let Some(c) = US.captures(s.as_str()).ok().flatten() {
        month_number(c.get(1)?.as_str()).and_then(|m| Some((num(&c, 3)? as i32, m, num(&c, 2)?)))
    } else {
        None
    }?;
    let (y, m, d) = parts;
    Some(
        zoned_to_utc(o.tz, y, m, d, o.default_hour, o.default_minute, 0)
            .map(|due| (due, ymd(y, m, d)))
            .ok_or("invalid_date"),
    )
}

// ---- relative durations

const UNIT_S: f64 = 1000.0;
const UNIT_M: f64 = 60_000.0;
const UNIT_H: f64 = 3_600_000.0;
const UNIT_D: f64 = 86_400_000.0;

fn unit_ms(word: &str) -> Option<f64> {
    Some(match word.to_lowercase().as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" | "secondo" | "secondi" => UNIT_S,
        "m" | "min" | "mins" | "minute" | "minutes" | "minuto" | "minuti" => UNIT_M,
        "h" | "hr" | "hrs" | "hour" | "hours" | "ora" | "oretta" | "orette" | "ore" => UNIT_H,
        "d" | "day" | "days" | "giorno" | "giorni" => UNIT_D,
        _ => return None,
    })
}

fn number_word(word: &str) -> Option<f64> {
    Some(match word.to_lowercase().as_str() {
        "a" | "an" | "one" | "un" | "una" | "uno" => 1.0,
        "due" | "two" => 2.0,
        "tre" | "three" => 3.0,
        "quattro" | "four" => 4.0,
        "cinque" | "five" => 5.0,
        "sei" | "six" => 6.0,
        "sette" | "seven" => 7.0,
        "otto" | "eight" => 8.0,
        "nove" | "nine" => 9.0,
        "dieci" | "ten" => 10.0,
        "dodici" | "twelve" => 12.0,
        "quindici" | "fifteen" => 15.0,
        "venti" | "twenty" => 20.0,
        "trenta" | "thirty" => 30.0,
        "quaranta" | "forty" => 40.0,
        "cinquanta" | "fifty" => 50.0,
        _ => return None,
    })
}

fn format_duration_short(ms: f64) -> String {
    let ms = ms.max(0.0);
    let days = (ms / UNIT_D).floor() as i64;
    let hours = ((ms % UNIT_D) / UNIT_H).floor() as i64;
    let minutes = ((ms % UNIT_H) / UNIT_M).floor() as i64;
    let seconds = ((ms % UNIT_M) / UNIT_S).floor() as i64;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}g"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 && parts.is_empty() {
        parts.push(format!("{seconds}s"));
    }
    if parts.is_empty() {
        "0s".into()
    } else {
        parts.into_iter().take(2).collect::<Vec<_>>().join(" ")
    }
}

/// Byte offset of `chars` characters into `s` (the Node code indexes the
/// lowercased string by UTF-16 units; inputs here are matched on `&str`).
struct Duration2 {
    ms: f64,
    /// Bytes of the lowercased input consumed.
    consumed: usize,
}

static RE_CONNECTOR: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^[\s,]*(?:e|ed|and|\+)[\s,]+"));
static RE_MEZZ: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^mezz(?:['’] ?or[ae]|orett[ae])(?![a-z])"));
static RE_TRE_QUARTI: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^tre\s+quarti\s+d['’ ]?ora(?![a-z])"));
static RE_QUARTO: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^(?:un\s+)?quarto\s+d['’ ]?ora(?![a-z])"));
static RE_HALF_HOUR: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^half\s+an?\s+hour(?![a-z])"));
static RE_HALF: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^(?:a\s+|an\s+|un\s+|una\s+)?(?:half|mezza|mezzo|met[àa])(?![a-z])"));
static RE_QTY: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(\d+(?:[.,]\d+)?)"));
static RE_NUMBER_WORD: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)^(?:uno|una|un|one|a|an|due|two|tre|three|quattro|four|cinque|five|sei|six|sette|seven|otto|eight|nove|nine|dieci|ten|dodici|twelve|quindici|fifteen|venti|twenty|trenta|thirty|quaranta|forty|cinquanta|fifty)(?![a-z])",
    )
});
static RE_UNIT: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)^['’ ]?(seconds?|secondi|secondo|secs?|minutes?|minuti|minuto|mins?|min|hours?|hrs?|hr|orett[ae]|oras?|ora|ore|days?|giorn[oi]|[smhd])(?![a-z])",
    )
});

fn find_at<'a>(r: &Regex, s: &'a str) -> Option<fancy_regex::Match<'a>> {
    r.find(s).ok().flatten()
}

/// "1 hour and an half", "un'ora e mezza", "20 min", "1h30m", "mezz'ora".
fn parse_duration_expression(value: &str) -> Option<Duration2> {
    let raw = normalize_ws(value);
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_lowercase();
    let mut i = 0usize;
    let mut total = 0f64;
    let mut segments = 0u32;
    let mut last_unit = 0f64;
    while i < lower.len() {
        let before_segment = i;
        if segments > 0 {
            if let Some(m) = find_at(&RE_CONNECTOR, &lower[i..]) {
                i += m.end();
            }
        }
        let rest = &lower[i..];
        let fixed: Option<(usize, f64)> = [
            (&*RE_MEZZ, 30.0 * UNIT_M),
            (&*RE_TRE_QUARTI, 45.0 * UNIT_M),
            (&*RE_QUARTO, 15.0 * UNIT_M),
            (&*RE_HALF_HOUR, 30.0 * UNIT_M),
        ]
        .into_iter()
        .find_map(|(r, ms)| find_at(r, rest).map(|m| (m.end(), ms)));
        if let Some((len, ms)) = fixed {
            total += ms;
            last_unit = UNIT_M;
            i += len;
            segments += 1;
            continue;
        }
        if let Some(m) = find_at(&RE_HALF, rest) {
            if last_unit == 0.0 {
                return None;
            }
            total += last_unit / 2.0;
            i += m.end();
            segments += 1;
            continue;
        }
        let mut qty = None;
        if let Some(c) = RE_QTY.captures(rest).ok().flatten() {
            let m = c.get(1).expect("group");
            qty = m.as_str().replace(',', ".").parse::<f64>().ok();
            i += m.end();
        } else if let Some(m) = find_at(&RE_NUMBER_WORD, rest) {
            qty = number_word(m.as_str());
            i += m.end();
        }
        let Some(qty) = qty.filter(|q| q.is_finite()) else {
            if segments > 0 {
                i = before_segment;
                break;
            }
            return None;
        };
        let rest = &lower[i..];
        let caps = RE_UNIT.captures(rest).ok().flatten()?;
        let unit = unit_ms(caps.get(1)?.as_str())?;
        i += caps.get(0)?.end();
        total += qty * unit;
        last_unit = unit;
        segments += 1;
    }
    if segments < 1 || !(5_000.0..=180.0 * UNIT_D).contains(&total) {
        return None;
    }
    Some(Duration2 {
        ms: total,
        consumed: i,
    })
}

static TIMER_PREFIX_STRIP: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        re(r"(?i)^(?:please|per\s+favore|perfavore)[\s,!.]*"),
        re(
            r"(?i)^(?:ciao|hey|hi|hello|salve|ei|yo|ok|okay)(?:[\s,!.]*(?:lucina|flar?e|hermes))?[\s,!.]*",
        ),
        re(r"(?i)^(?:can\s+you|could\s+you|puoi|potresti|mi\s+puoi)[\s,!]*"),
        re(r"(?i)^(?:lucina|hermes)[\s,!.]*"),
        re(
            r"(?i)^(?:set|start|metti|avvia|imposta|attiva)\s+(?:a\s+|un\s+|una\s+)?(?:new\s+|nuovo\s+)?",
        ),
    ]
});
static TIMER_KEYWORD: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^timer(?![a-z])(?:\s+(?:for|of|di)\b)?[\s:：]*"));
static REMINDER_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)^(remind\s+me(?:\s+to)?|ricordami(?:\s+di|a)?|avvisami(?:\s+di|a)?|svegliami|wake\s+me(?:\s+up)?|set\s+a\s+reminder|mandami\s+un\s+promemoria)[\s,!:]*\s*",
    )
});
static RELATIVE_PREP: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(?:in|after|tra|fra)\s+"));
static TIMER_QUESTION: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)^(?:how|what|when|where|why|who|quanto|quanta|quanti|quante|quando|come|che|cosa|perche'|perché)[\s?!.,]",
    )
});
static BODY_LEAD: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^(?:to|di|per|che)\s+"));
static BODY_POLITE_TAIL: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\s*(?:please|per\s+favore|perfavore)[\s.!]*$"));

fn replace_first(r: &Regex, s: &str) -> String {
    r.replace(s, "").into_owned()
}

/// "timer 20 minutes", "in 20 minutes …", "tra mezz'ora …", "after 2 days …".
pub fn parse_relative(text: &str, o: &ParseOptions) -> Result<Parsed, ParseError> {
    let raw = normalize_ws(text);
    if raw.is_empty() {
        return Err("empty_message");
    }
    let mut work = raw;
    let mut is_timer = false;
    let mut is_wake = false;
    for _ in 0..6 {
        let mut stripped = work.clone();
        for r in TIMER_PREFIX_STRIP.iter() {
            stripped = replace_first(r, &stripped);
        }
        if let Some(m) = find_at(&TIMER_KEYWORD, &stripped) {
            is_timer = true;
            stripped = stripped[m.end()..].to_owned();
        } else if let Some(c) = REMINDER_PREFIX.captures(&stripped).ok().flatten() {
            let word = c
                .get(1)
                .map(|m| m.as_str().to_lowercase())
                .unwrap_or_default();
            if word.starts_with("sveglia") || word.starts_with("wake") {
                is_wake = true;
            }
            let end = c.get(0).map(|m| m.end()).unwrap_or(0);
            stripped = stripped[end..].to_owned();
        }
        if stripped == work {
            break;
        }
        work = stripped;
    }
    let work = work.trim().to_owned();
    if TIMER_QUESTION.is_match(&work).unwrap_or(false) {
        return Err("question_not_timer");
    }
    let expr = if let Some(m) = find_at(&RELATIVE_PREP, &work) {
        work[m.end()..].to_owned()
    } else if is_timer || is_wake {
        work.clone()
    } else {
        return Err("no_duration");
    };
    let duration = parse_duration_expression(&expr).ok_or("unparsed_time")?;
    // `consumed` indexes the lowercased, whitespace-normalised expression;
    // `expr` is already normalised and lowercasing keeps ASCII byte lengths.
    let expr_norm = normalize_ws(&expr);
    let consumed = if expr_norm.to_lowercase().len() == expr_norm.len() {
        duration.consumed.min(expr_norm.len())
    } else {
        // Non-ASCII case changes: map by characters.
        let chars = expr_norm.to_lowercase()[..duration.consumed]
            .chars()
            .count();
        expr_norm
            .char_indices()
            .nth(chars)
            .map(|(b, _)| b)
            .unwrap_or(expr_norm.len())
    };
    let mut body = normalize_ws(&expr_norm[consumed..]);
    body = replace_first(&BODY_LEAD, &body);
    body = replace_first(&BODY_POLITE_TAIL, &body);
    if body.is_empty() {
        if !is_timer && !is_wake {
            return Err("missing_body");
        }
        body = format!(
            "{} ({})",
            if is_wake && !is_timer {
                "Sveglia"
            } else {
                "Timer"
            },
            format_duration_short(duration.ms)
        );
    }
    let due = o.now + Duration::milliseconds(duration.ms.round() as i64);
    let local = due.with_timezone(&o.tz);
    Ok(Parsed {
        text: body,
        due_at: due,
        local_date: ymd(local.year(), local.month(), local.day()),
        relative: true,
    })
}

static REMIND_PREFIX_DATE: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)^(?:please\s+)?(?:remind me(?:\s+to)?|set a reminder(?:\s+for)?|ricordami(?:\s+di)?)\s+(?:on\s+|for\s+|il\s+|per\s+il\s+)?(.+)$",
    )
});

fn split_date_and_body(text: &str) -> (String, String) {
    let raw = normalize_ws(text);
    for sep in [" — ", " – ", " - ", " —", " –", " -"] {
        if let Some(idx) = raw.find(sep) {
            if idx > 0 {
                return (
                    raw[..idx].trim().to_owned(),
                    raw[idx + sep.len()..].trim().to_owned(),
                );
            }
        }
    }
    (raw, String::new())
}

/// The full parser: at-time, then relative, then "date — body".
pub fn parse_reminder_message(text: &str, o: &ParseOptions) -> Result<Parsed, ParseError> {
    let raw = normalize_ws(text);
    if raw.is_empty() {
        return Err("empty_message");
    }
    if let Ok(p) = parse_at_time(&raw, o) {
        return Ok(p);
    }
    if let Ok(p) = parse_relative(&raw, o) {
        return Ok(p);
    }
    let (date_part, body) = match REMIND_PREFIX_DATE.captures(&raw).ok().flatten() {
        Some(c) => split_date_and_body(c.get(1).map(|m| m.as_str().trim()).unwrap_or("")),
        None => split_date_and_body(&raw),
    };
    if body.is_empty() {
        return Err("missing_body");
    }
    let (due, local_date) = match parse_date_part(&date_part, o) {
        Some(Ok(v)) => v,
        Some(Err(e)) => return Err(e),
        None => return Err("unparsed_date"),
    };
    if due <= o.now {
        return Err("due_in_past");
    }
    Ok(Parsed {
        text: body,
        due_at: due,
        local_date,
        relative: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn opts() -> ParseOptions {
        ParseOptions {
            tz: "Europe/Rome".parse().unwrap(),
            default_hour: 9,
            default_minute: 0,
            // Tuesday 2026-10-21 17:00 in Rome
            now: Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap(),
        }
    }

    #[test]
    fn at_time() {
        let p =
            parse_reminder_message("Ricordami alle 18:30 di chiamare la mamma", &opts()).unwrap();
        assert_eq!(p.text, "chiamare la mamma");
        assert_eq!(p.due_at_iso(), "2026-10-21T16:30:00.000Z");
        let p = parse_reminder_message("alle 9:30 di fare la doccia", &opts()).unwrap();
        assert_eq!(p.due_at_iso(), "2026-10-22T07:30:00.000Z");
        assert_eq!(p.local_date, "2026-10-22");
        let p = parse_reminder_message("remind me at 21 to stretch", &opts()).unwrap();
        assert_eq!(p.text, "stretch");
        assert_eq!(p.due_at_iso(), "2026-10-21T19:00:00.000Z");
        assert_eq!(
            parse_at_time("ricordami alle 25 di x", &opts()),
            Err("invalid_time")
        );
    }

    #[test]
    fn relative() {
        let o = opts();
        let p = parse_reminder_message("remind me in 2 hours to call Bob", &o).unwrap();
        assert_eq!(p.text, "call Bob");
        assert_eq!(p.due_at, o.now + Duration::hours(2));
        let p = parse_reminder_message("Timer 1 hour and an half please", &o).unwrap();
        assert_eq!(p.text, "Timer (1h 30m)");
        assert_eq!(p.due_at, o.now + Duration::minutes(90));
        let p = parse_reminder_message("tra mezz'ora di girare la pasta", &o).unwrap();
        assert_eq!(p.text, "girare la pasta");
        assert_eq!(p.due_at, o.now + Duration::minutes(30));
        let p = parse_reminder_message("ricordami tra un'ora e mezza di uscire", &o).unwrap();
        assert_eq!(p.text, "uscire");
        assert_eq!(p.due_at, o.now + Duration::minutes(90));
        let p = parse_reminder_message("svegliami tra 20 minuti", &o).unwrap();
        assert_eq!(p.text, "Sveglia (20m)");
        assert_eq!(
            parse_relative("how long until the timer ends?", &o),
            Err("question_not_timer")
        );
        assert_eq!(parse_relative("timer 3", &o), Err("unparsed_time"));
    }

    #[test]
    fn dates() {
        let o = opts();
        let p = parse_reminder_message("28 November 2026 — unsubscribe from Google AI Plus", &o)
            .unwrap();
        assert_eq!(p.text, "unsubscribe from Google AI Plus");
        assert_eq!(p.due_at_iso(), "2026-11-28T08:00:00.000Z");
        let p = parse_reminder_message("remind me on 2026-12-01 - pay rent", &o).unwrap();
        assert_eq!(p.due_at_iso(), "2026-12-01T08:00:00.000Z");
        assert_eq!(
            parse_reminder_message("28 August 2026 — late", &o),
            Err("due_in_past")
        );
        assert_eq!(
            parse_reminder_message("domani forse", &o),
            Err("missing_body")
        );
        assert_eq!(
            parse_reminder_message("blah - blah", &o),
            Err("unparsed_date")
        );
    }
}
