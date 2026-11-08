//! iCalendar (`.ics`, RFC 5545) reader: events and to-dos, with times
//! resolved to real instants where the file says which zone they are in.

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{Map, Value};

use super::Extracted;

const MAX_EVENTS: usize = 200;
const MAX_TODOS: usize = 200;
const DESCRIPTION_CHARS: usize = 2000;
/// Description shown under each event line in `text`.
const TEXT_DESCRIPTION_CHARS: usize = 300;

/// Reads every `VEVENT` (up to 200) and `VTODO` in an iCalendar file.
pub fn parse_ics(input: &str) -> Extracted {
    let mut out = Extracted::new("calendar");
    let mut warnings = Vec::new();
    let mut events = Vec::new();
    let mut todos = Vec::new();
    let mut event_lines = Vec::new();
    let mut todo_lines = Vec::new();
    let mut first_summary = None;
    let mut skipped_events = 0usize;
    let mut skipped_todos = 0usize;
    let mut stack: Vec<String> = Vec::new();
    let mut current: Vec<Prop> = Vec::new();

    for line in unfold(input) {
        let Some(prop) = parse_line(&line) else {
            continue;
        };
        match prop.name.as_str() {
            "BEGIN" => {
                let comp = prop.value.trim().to_ascii_uppercase();
                if comp == "VEVENT" || comp == "VTODO" {
                    current.clear();
                }
                stack.push(comp);
            }
            "END" => {
                let comp = prop.value.trim().to_ascii_uppercase();
                let Some(pos) = stack.iter().rposition(|c| *c == comp) else {
                    continue;
                };
                stack.truncate(pos);
                match comp.as_str() {
                    "VEVENT" if events.len() < MAX_EVENTS => {
                        let event = build_event(&current, &mut warnings);
                        if first_summary.is_none() {
                            first_summary.clone_from(&event.summary);
                        }
                        event_lines.push(event.line);
                        events.push(event.json);
                    }
                    "VEVENT" => skipped_events += 1,
                    "VTODO" if todos.len() < MAX_TODOS => {
                        let (json, line) = build_todo(&current, &mut warnings);
                        todo_lines.push(line);
                        todos.push(json);
                    }
                    "VTODO" => skipped_todos += 1,
                    _ => {}
                }
            }
            _ => {
                if matches!(stack.last().map(String::as_str), Some("VEVENT" | "VTODO")) {
                    current.push(prop);
                }
            }
        }
    }

    if skipped_events > 0 {
        warnings.push(format!(
            "Only the first {MAX_EVENTS} events were read; {skipped_events} more were skipped."
        ));
    }
    if skipped_todos > 0 {
        warnings.push(format!(
            "Only the first {MAX_TODOS} to-dos were read; {skipped_todos} more were skipped."
        ));
    }
    if events.is_empty() && todos.is_empty() {
        warnings.push("No events or to-dos were found in this calendar file.".into());
    }

    let mut text = vec![count_line(
        events.len() + skipped_events,
        todos.len() + skipped_todos,
    )];
    text.extend(event_lines);
    text.extend(todo_lines);

    out.title = first_summary;
    out.text = text.join("\n");
    out.structured = serde_json::json!({ "events": events, "todos": todos });
    out.warnings = warnings;
    out
}

/// One content line: `NAME;PARAM=VALUE:VALUE`.
struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Prop {
    fn param(&self, key: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .filter(|v| !v.is_empty())
    }

    /// The value as TEXT (escapes undone), `None` when blank.
    fn text(&self) -> Option<String> {
        let v = unescape(self.value.trim());
        (!v.trim().is_empty()).then(|| v.trim().to_string())
    }
}

/// Joins folded lines (a line break followed by a space or tab).
fn unfold(input: &str) -> Vec<String> {
    let input = input.trim_start_matches('\u{feff}');
    let mut lines: Vec<String> = Vec::new();
    for raw in input.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = raw.strip_prefix([' ', '\t']) {
            if let Some(last) = lines.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        if !raw.trim().is_empty() {
            lines.push(raw.to_string());
        }
    }
    lines
}

/// Splits `NAME;P=V;P="a:b":value` on the first colon outside quotes.
fn parse_line(line: &str) -> Option<Prop> {
    let mut in_quotes = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let mut parts = split_quoted(&line[..colon], ';').into_iter();
    let name = parts.next()?.trim().to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = parts
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (
                k.trim().to_ascii_uppercase(),
                v.trim().trim_matches('"').to_string(),
            ),
            None => (p.trim().to_ascii_uppercase(), String::new()),
        })
        .collect();
    Some(Prop {
        name,
        params,
        value: line[colon + 1..].to_string(),
    })
}

fn split_quoted(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_quotes = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if c == sep && !in_quotes {
            parts.push(&s[start..i]);
            start = i + c.len_utf8();
        }
    }
    parts.push(&s[start..]);
    parts
}

/// Undoes TEXT escapes: `\n` `\N` `\,` `\;` `\\`.
fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn first<'a>(props: &'a [Prop], name: &str) -> Option<&'a Prop> {
    props.iter().find(|p| p.name == name)
}

fn first_text(props: &[Prop], name: &str) -> Option<String> {
    first(props, name).and_then(Prop::text)
}

/// A DTSTART/DTEND/DUE value.
#[derive(Clone, Copy, Debug, PartialEq)]
enum When {
    /// `VALUE=DATE`: an all-day date.
    Date(NaiveDate),
    /// `...Z`.
    UtcTime(DateTime<Utc>),
    /// With a known `TZID`.
    Zoned(DateTime<Tz>),
    /// No zone (or a zone we could not resolve): wall-clock time as written.
    Floating(NaiveDateTime),
}

impl When {
    fn iso(&self) -> String {
        match self {
            When::Date(d) => d.format("%Y-%m-%d").to_string(),
            When::UtcTime(t) => t.to_rfc3339_opts(SecondsFormat::Secs, true),
            When::Zoned(t) => t.to_rfc3339_opts(SecondsFormat::Secs, false),
            When::Floating(t) => t.format("%Y-%m-%dT%H:%M:%S").to_string(),
        }
    }

    fn plus(&self, d: Duration) -> Option<When> {
        Some(match *self {
            When::Date(x) => When::Date(x.checked_add_signed(d)?),
            When::UtcTime(t) => When::UtcTime(t.checked_add_signed(d)?),
            When::Zoned(t) => When::Zoned(t.checked_add_signed(d)?),
            When::Floating(t) => When::Floating(t.checked_add_signed(d)?),
        })
    }

    /// Wall-clock time in the value's own zone.
    fn wall(&self) -> NaiveDateTime {
        match self {
            When::Date(d) => d.and_time(chrono::NaiveTime::MIN),
            When::UtcTime(t) => t.naive_utc(),
            When::Zoned(t) => t.naive_local(),
            When::Floating(t) => *t,
        }
    }

    /// Wall-clock time of `self` seen from `start`'s zone.
    fn wall_from(&self, start: &When) -> NaiveDateTime {
        match (start, self) {
            (When::Zoned(s), When::Zoned(e)) => e.with_timezone(&s.timezone()).naive_local(),
            (When::Zoned(s), When::UtcTime(e)) => e.with_timezone(&s.timezone()).naive_local(),
            (When::UtcTime(_), When::Zoned(e)) => e.naive_utc(),
            _ => self.wall(),
        }
    }
}

fn push_warning(warnings: &mut Vec<String>, w: String) {
    if !warnings.contains(&w) {
        warnings.push(w);
    }
}

/// Parses a date or date-time property, warning when it cannot.
fn prop_when(prop: &Prop, warnings: &mut Vec<String>) -> Option<When> {
    let date_param = prop
        .param("VALUE")
        .is_some_and(|v| v.eq_ignore_ascii_case("DATE"));
    let when = parse_when(&prop.value, date_param, prop.param("TZID"), warnings);
    if when.is_none() {
        push_warning(
            warnings,
            format!(
                "Could not read the {} value \"{}\".",
                prop.name,
                prop.value.trim()
            ),
        );
    }
    when
}

fn parse_when(
    raw: &str,
    date_param: bool,
    tzid: Option<&str>,
    warnings: &mut Vec<String>,
) -> Option<When> {
    let v = raw.trim();
    if date_param || (v.len() == 8 && v.bytes().all(|b| b.is_ascii_digit())) {
        return NaiveDate::parse_from_str(v.get(..8)?, "%Y%m%d")
            .ok()
            .map(When::Date);
    }
    let (body, utc) = match v.strip_suffix(['Z', 'z']) {
        Some(b) => (b, true),
        None => (v, false),
    };
    let naive = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S")
        .or_else(|_| NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M"))
        .ok()?;
    if utc {
        return Some(When::UtcTime(Utc.from_utc_datetime(&naive)));
    }
    if let Some(name) = tzid {
        return match resolve_tz(name) {
            Some(tz) => Some(When::Zoned(localize(tz, naive))),
            None => {
                push_warning(
                    warnings,
                    format!("Unknown time zone \"{name}\"; its times are shown as written."),
                );
                Some(When::Floating(naive))
            }
        };
    }
    Some(When::Floating(naive))
}

/// Local time in `tz`; a time inside a DST gap moves forward an hour.
fn localize(tz: Tz, naive: NaiveDateTime) -> DateTime<Tz> {
    tz.from_local_datetime(&naive)
        .earliest()
        .or_else(|| {
            tz.from_local_datetime(&(naive + Duration::hours(1)))
                .earliest()
        })
        .unwrap_or_else(|| tz.from_utc_datetime(&naive))
}

/// IANA names, `/vendor/prefix/Europe/Rome`-style ids and common Windows names.
fn resolve_tz(name: &str) -> Option<Tz> {
    let name = name.trim().trim_matches('"').trim();
    if let Ok(tz) = name.parse::<Tz>() {
        return Some(tz);
    }
    for (i, c) in name.char_indices() {
        if c == '/' {
            if let Ok(tz) = name[i + 1..].parse::<Tz>() {
                return Some(tz);
            }
        }
    }
    let iana = match name {
        "W. Europe Standard Time" => "Europe/Berlin",
        "Romance Standard Time" => "Europe/Paris",
        "Central Europe Standard Time" => "Europe/Budapest",
        "Central European Standard Time" => "Europe/Warsaw",
        "GMT Standard Time" => "Europe/London",
        "Greenwich Standard Time" => "Atlantic/Reykjavik",
        "Eastern Standard Time" => "America/New_York",
        "Central Standard Time" => "America/Chicago",
        "Mountain Standard Time" => "America/Denver",
        "Pacific Standard Time" => "America/Los_Angeles",
        "Coordinated Universal Time" => "UTC",
        _ => return None,
    };
    iana.parse().ok()
}

/// ISO 8601 / RFC 5545 duration: `P1W`, `P1DT2H`, `-PT15M`.
fn parse_duration(raw: &str) -> Option<Duration> {
    let s = raw.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix(['P', 'p'])?;
    let mut total: i64 = 0;
    let mut num = String::new();
    let mut in_time = false;
    let mut any = false;
    for c in s.chars() {
        match c {
            '0'..='9' => num.push(c),
            'T' | 't' if num.is_empty() => in_time = true,
            _ => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                let unit = match (c.to_ascii_uppercase(), in_time) {
                    ('W', false) => 604_800,
                    ('D', false) => 86_400,
                    ('H', true) => 3_600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
                total = total.checked_add(n.checked_mul(unit)?)?;
                any = true;
            }
        }
    }
    if !num.is_empty() || !any {
        return None;
    }
    Duration::try_seconds(if neg { -total } else { total })
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn put(map: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(v) = value {
        map.insert(key.into(), Value::String(v));
    }
}

struct Event {
    json: Value,
    line: String,
    summary: Option<String>,
}

fn build_event(props: &[Prop], warnings: &mut Vec<String>) -> Event {
    let summary = first_text(props, "SUMMARY");
    let description = first_text(props, "DESCRIPTION");
    let location = first_text(props, "LOCATION");
    let start_prop = first(props, "DTSTART");
    let start = start_prop.and_then(|p| prop_when(p, warnings));
    let duration_raw = first(props, "DURATION").map(|p| p.value.trim().to_string());
    let end = match first(props, "DTEND") {
        Some(p) => prop_when(p, warnings),
        None => match (&start, &duration_raw) {
            (Some(s), Some(d)) => match parse_duration(d) {
                Some(dur) => s.plus(dur),
                None => {
                    push_warning(warnings, format!("Could not read the duration \"{d}\"."));
                    None
                }
            },
            _ => None,
        },
    };
    let all_day = matches!(start, Some(When::Date(_)));
    let tzid = match &start {
        Some(When::Zoned(t)) => Some(t.timezone().name().to_string()),
        _ => start_prop
            .and_then(|p| p.param("TZID"))
            .map(|t| t.trim_matches('"').to_string()),
    };
    let status = first(props, "STATUS").map(|p| p.value.trim().to_ascii_uppercase());
    let rrule = first(props, "RRULE").map(|p| p.value.trim().to_string());

    let mut map = Map::new();
    put(&mut map, "summary", summary.clone());
    put(
        &mut map,
        "description",
        description.as_deref().map(|d| clip(d, DESCRIPTION_CHARS)),
    );
    put(&mut map, "location", location.clone());
    put(&mut map, "start", start.as_ref().map(When::iso));
    put(&mut map, "end", end.as_ref().map(When::iso));
    map.insert("all_day".into(), Value::Bool(all_day));
    put(&mut map, "tzid", tzid.clone());
    put(&mut map, "duration", duration_raw);
    if let Some(org) = first(props, "ORGANIZER") {
        let mut o = Map::new();
        put(&mut o, "name", org.param("CN").map(str::to_string));
        put(&mut o, "email", mail_address(&org.value));
        if !o.is_empty() {
            map.insert("organizer".into(), Value::Object(o));
        }
    }
    put(
        &mut map,
        "url",
        first(props, "URL").map(|p| p.value.trim().to_string()),
    );
    put(&mut map, "rrule", rrule.clone());
    put(&mut map, "status", status.clone());
    put(
        &mut map,
        "uid",
        first(props, "UID").map(|p| p.value.trim().to_string()),
    );

    let mut parts = Vec::new();
    if let Some(s) = &start {
        parts.push(span_text(s, end.as_ref(), tzid.as_deref()));
    }
    let title = summary.clone().unwrap_or_else(|| "(untitled event)".into());
    parts.push(match status.as_deref() {
        Some("CANCELLED") => format!("CANCELLED: {title}"),
        _ => title,
    });
    if let Some(l) = &location {
        parts.push(l.replace('\n', ", "));
    }
    if let Some(r) = &rrule {
        parts.push(repeat_text(r));
    }
    let mut line = parts.join(" · ");
    if let Some(d) = &description {
        let flat = d.split_whitespace().collect::<Vec<_>>().join(" ");
        line.push_str("\n  ");
        line.push_str(&clip(&flat, TEXT_DESCRIPTION_CHARS));
    }
    Event {
        json: Value::Object(map),
        line,
        summary,
    }
}

fn build_todo(props: &[Prop], warnings: &mut Vec<String>) -> (Value, String) {
    let summary = first_text(props, "SUMMARY");
    let due = first(props, "DUE").and_then(|p| prop_when(p, warnings));
    let status = first(props, "STATUS").map(|p| p.value.trim().to_ascii_uppercase());
    let mut map = Map::new();
    put(&mut map, "summary", summary.clone());
    put(&mut map, "due", due.as_ref().map(When::iso));
    put(&mut map, "status", status.clone());
    let mut line = format!(
        "To-do: {}",
        summary.unwrap_or_else(|| "(untitled to-do)".into())
    );
    if let Some(d) = &due {
        line.push_str(&format!(" (due {})", when_text(d)));
    }
    if status.as_deref() == Some("COMPLETED") {
        line.push_str(" [done]");
    }
    (Value::Object(map), line)
}

fn mail_address(value: &str) -> Option<String> {
    let v = value.trim();
    let v = match v.get(..7) {
        Some(p) if p.eq_ignore_ascii_case("mailto:") => &v[7..],
        _ => v,
    };
    (!v.is_empty()).then(|| v.to_string())
}

fn count_line(events: usize, todos: usize) -> String {
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut s = format!("Calendar: {}", plural(events, "event", "events"));
    if todos > 0 {
        s.push_str(&format!(", {}", plural(todos, "to-do", "to-dos")));
    }
    s.push('.');
    s
}

const DAY_FMT: &str = "%a %-d %b %Y";

fn zone_label(w: &When, tzid: Option<&str>) -> Option<String> {
    match w {
        When::Zoned(t) => Some(t.timezone().name().to_string()),
        When::UtcTime(_) => Some("UTC".into()),
        When::Floating(_) => tzid.map(str::to_string),
        When::Date(_) => None,
    }
}

fn when_text(w: &When) -> String {
    match w {
        When::Date(d) => d.format(DAY_FMT).to_string(),
        _ => {
            let mut s = w.wall().format("%a %-d %b %Y %H:%M").to_string();
            if let Some(z) = zone_label(w, None) {
                s.push_str(&format!(" ({z})"));
            }
            s
        }
    }
}

/// `Fri 14 Nov 2026 19:00–21:00 (Europe/Copenhagen)`.
fn span_text(start: &When, end: Option<&When>, tzid: Option<&str>) -> String {
    if let When::Date(d) = start {
        let first = d.format(DAY_FMT).to_string();
        // DTEND of an all-day event is exclusive.
        let last = match end {
            Some(When::Date(e)) => e.pred_opt().filter(|l| l > d),
            _ => None,
        };
        return match last {
            Some(l) => format!("{first} – {} (all day)", l.format(DAY_FMT)),
            None => format!("{first} (all day)"),
        };
    }
    let s = start.wall();
    let mut out = s.format("%a %-d %b %Y %H:%M").to_string();
    if let Some(e) = end {
        let e = e.wall_from(start);
        if e.date() == s.date() {
            out.push_str(&format!("–{}", e.format("%H:%M")));
        } else {
            out.push_str(&format!(" – {}", e.format("%a %-d %b %Y %H:%M")));
        }
    }
    if let Some(z) = zone_label(start, tzid) {
        out.push_str(&format!(" ({z})"));
    }
    out
}

fn repeat_text(rrule: &str) -> String {
    let freq = rrule.split(';').find_map(|part| {
        let (k, v) = part.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("FREQ")
            .then(|| v.trim().to_ascii_uppercase())
    });
    match freq.as_deref() {
        Some("DAILY") => "repeats daily".into(),
        Some("WEEKLY") => "repeats weekly".into(),
        Some("MONTHLY") => "repeats monthly".into(),
        Some("YEARLY") => "repeats yearly".into(),
        _ => "repeats".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOGLE: &str = include_str!("../../../tests/fixtures/attachments/google-export.ics");

    fn events(x: &Extracted) -> &Vec<Value> {
        x.structured["events"].as_array().expect("events array")
    }

    #[test]
    fn google_export_with_tzid() {
        let x = parse_ics(&GOOGLE.replace('\n', "\r\n"));
        assert_eq!(x.kind, "calendar");
        assert!(x.warnings.is_empty(), "{:?}", x.warnings);
        let ev = events(&x);
        assert_eq!(ev.len(), 3);
        let dinner = &ev[0];
        assert_eq!(dinner["summary"], "Dinner at Noma");
        assert_eq!(dinner["start"], "2026-11-14T19:00:00+01:00");
        assert_eq!(dinner["end"], "2026-11-14T21:00:00+01:00");
        assert_eq!(dinner["tzid"], "Europe/Copenhagen");
        assert_eq!(dinner["all_day"], false);
        assert_eq!(dinner["location"], "Refshalevej 96, 1432 København");
        assert_eq!(dinner["organizer"]["name"], "Giuseppe Vitolo");
        assert_eq!(dinner["organizer"]["email"], "owner@example.com");
        assert_eq!(dinner["status"], "CONFIRMED");
        assert_eq!(dinner["uid"], "7kukuqrfedlm2f9t0vr5bq3h1a@google.com");
        assert_eq!(x.title.as_deref(), Some("Dinner at Noma"));
        assert!(
            x.text.contains(
                "Sat 14 Nov 2026 19:00–21:00 (Europe/Copenhagen) · Dinner at Noma · Refshalevej 96, 1432 København"
            ),
            "{}",
            x.text
        );
        assert!(x.text.starts_with("Calendar: 3 events, 1 to-do."));
        // The VALARM inside the event does not leak its DESCRIPTION.
        assert!(!dinner["description"]
            .as_str()
            .unwrap_or_default()
            .contains("Reminder"));
        let weekly = &ev[2];
        assert_eq!(weekly["rrule"], "FREQ=WEEKLY;BYDAY=TU");
        assert!(x.text.contains("repeats weekly"));
        let todos = x.structured["todos"].as_array().expect("todos");
        assert_eq!(todos[0]["summary"], "Book the table");
        assert_eq!(todos[0]["due"], "2026-11-10");
    }

    #[test]
    fn folded_long_description_and_escaped_commas() {
        let x = parse_ics(GOOGLE);
        let dinner = &events(&x)[0];
        let desc = dinner["description"].as_str().expect("description");
        assert!(desc.starts_with("Tasting menu, wine pairing; bring the gift card."));
        assert!(desc.contains("\nTable for 2, ask for the window seat"));
        assert!(desc.ends_with("cancellation fee applies after 48 hours."));
        assert!(!desc.contains('\\'));
    }

    #[test]
    fn all_day_event() {
        let x = parse_ics(GOOGLE);
        let trip = &events(&x)[1];
        assert_eq!(trip["all_day"], true);
        assert_eq!(trip["start"], "2026-12-23");
        assert_eq!(trip["end"], "2026-12-27");
        assert!(x
            .text
            .contains("Wed 23 Dec 2026 – Sat 26 Dec 2026 (all day) · Christmas in Naples"));
    }

    #[test]
    fn utc_event() {
        let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nDTSTART:20261201T083000Z\nDTEND:20261201T090000Z\nSUMMARY:Standup\nEND:VEVENT\nEND:VCALENDAR\n";
        let x = parse_ics(ics);
        let e = &events(&x)[0];
        assert_eq!(e["start"], "2026-12-01T08:30:00Z");
        assert_eq!(e["end"], "2026-12-01T09:00:00Z");
        assert!(e.get("tzid").is_none());
        assert!(x
            .text
            .contains("Tue 1 Dec 2026 08:30–09:00 (UTC) · Standup"));
    }

    #[test]
    fn duration_event_and_floating_time() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nDTSTART:20261105T223000\r\nDURATION:PT1H45M\r\nSUMMARY:Night train\\, Rome\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let x = parse_ics(ics);
        let e = &events(&x)[0];
        assert_eq!(e["summary"], "Night train, Rome");
        assert_eq!(e["start"], "2026-11-05T22:30:00");
        assert_eq!(e["end"], "2026-11-06T00:15:00");
        assert_eq!(e["duration"], "PT1H45M");
        assert!(x
            .text
            .contains("Thu 5 Nov 2026 22:30 – Fri 6 Nov 2026 00:15 · Night train, Rome"));
    }

    #[test]
    fn zoned_duration_and_unknown_zone() {
        let ics = "BEGIN:VEVENT\nDTSTART;TZID=Europe/Rome:20260329T013000\nDURATION:P1D\nSUMMARY:Across DST\nEND:VEVENT\nBEGIN:VEVENT\nDTSTART;TZID=\"Customized Time Zone\":20261110T090000\nSUMMARY:Outlook\nEND:VEVENT\n";
        let x = parse_ics(ics);
        let ev = events(&x);
        assert_eq!(ev[0]["start"], "2026-03-29T01:30:00+01:00");
        assert_eq!(ev[0]["end"], "2026-03-30T02:30:00+02:00");
        assert_eq!(ev[1]["start"], "2026-11-10T09:00:00");
        assert_eq!(ev[1]["tzid"], "Customized Time Zone");
        assert!(x
            .warnings
            .iter()
            .any(|w| w.contains("Customized Time Zone")));
    }

    #[test]
    fn helpers() {
        assert_eq!(parse_duration("P1W"), Duration::try_seconds(604_800));
        assert_eq!(parse_duration("-PT15M"), Duration::try_seconds(-900));
        assert_eq!(parse_duration("P1DT2H3M4S"), Duration::try_seconds(93_784));
        assert_eq!(parse_duration("PT"), None);
        assert_eq!(parse_duration("P1H"), None);
        assert_eq!(
            resolve_tz("/mozilla.org/20050126_1/Europe/Rome"),
            Some(chrono_tz::Europe::Rome)
        );
        assert_eq!(
            resolve_tz("W. Europe Standard Time"),
            Some(chrono_tz::Europe::Berlin)
        );
        assert_eq!(unescape(r"a\, b\; c\\d\ne"), "a, b; c\\d\ne");
    }

    #[test]
    fn empty_and_garbage_input() {
        let x = parse_ics("not a calendar at all");
        assert!(events(&x).is_empty());
        assert_eq!(x.warnings.len(), 1);
        assert!(x.title.is_none());
    }
}
