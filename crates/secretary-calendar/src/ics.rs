//! RFC 5545 (iCalendar) text → [`Item`]s. VEVENTs only; nested components
//! (VALARM) and other components (VTODO, VTIMEZONE) are skipped. Time zones
//! come from `TZID` (IANA, vendor-prefixed or common Windows names), else
//! `X-WR-TIMEZONE`, else the caller's default; VTIMEZONE bodies are not
//! interpreted.

use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};

use crate::model::{Item, Moment};

/// Most VEVENTs read from one feed.
pub const MAX_EVENTS: usize = 20_000;

/// A parsed feed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Feed {
    /// `X-WR-CALNAME`.
    pub name: Option<String>,
    pub items: Vec<Item>,
}

/// Unfold continuation lines (CRLF or LF followed by a space or tab).
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(rest) = line.strip_prefix([' ', '\t']) {
            if let Some(last) = lines.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        lines.push(line.to_owned());
    }
    lines
}

/// A content line: name, parameters, value.
struct Prop {
    name: String,
    params: Vec<(String, String)>,
    value: String,
}

impl Prop {
    fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

fn parse_line(line: &str) -> Option<Prop> {
    // Split at the first ':' outside double quotes.
    let mut in_q = false;
    let mut split = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_q = !in_q,
            ':' if !in_q => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let i = split?;
    let (head, value) = (&line[..i], &line[i + 1..]);
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    for c in head.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                cur.push(c);
            }
            ';' if !in_q => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    let name = parts.first()?.trim().to_ascii_uppercase();
    let params = parts[1..]
        .iter()
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((
                k.trim().to_ascii_uppercase(),
                v.trim().trim_matches('"').to_owned(),
            ))
        })
        .collect();
    Some(Prop {
        name,
        params,
        value: value.to_owned(),
    })
}

/// TEXT unescaping (`\n`, `\,`, `\;`, `\\`).
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// One DATE or DATE-TIME value.
fn moment(
    value: &str,
    tzid: Option<&str>,
    value_type: Option<&str>,
    cal_tz: &str,
) -> Option<Moment> {
    let v = value.trim();
    let is_date = value_type.is_some_and(|t| t.eq_ignore_ascii_case("DATE")) || v.len() == 8;
    if is_date {
        return NaiveDate::parse_from_str(&v[..8.min(v.len())], "%Y%m%d")
            .ok()
            .map(Moment::Date);
    }
    if let Some(u) = v.strip_suffix('Z') {
        let at = NaiveDateTime::parse_from_str(u, "%Y%m%dT%H%M%S").ok()?;
        return Some(Moment::Utc(Utc.from_utc_datetime(&at)));
    }
    let at = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()?;
    let tz = tzid
        .and_then(crate::parse_tz)
        .map(|z| z.name().to_owned())
        .unwrap_or_else(|| cal_tz.to_owned());
    Some(Moment::Local { at, tz })
}

/// `DURATION` (`P1D`, `PT1H30M`, `-PT15M`, `P2W`) in seconds.
pub fn duration_secs(v: &str) -> Option<i64> {
    let v = v.trim();
    let (sign, v) = match v.strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, v.strip_prefix('+').unwrap_or(v)),
    };
    let v = v.strip_prefix('P')?;
    let mut total: i64 = 0;
    let mut num = String::new();
    let mut in_time = false;
    for c in v.chars() {
        match c {
            'T' => in_time = true,
            '0'..='9' => num.push(c),
            _ => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += n * match (c, in_time) {
                    ('W', false) => 7 * 86_400,
                    ('D', false) => 86_400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
            }
        }
    }
    if !num.is_empty() {
        return None;
    }
    Some(sign * total)
}

/// Parse a feed. `default_tz` is used for floating times when the feed
/// names no zone.
pub fn parse(text: &str, default_tz: &str) -> Feed {
    let mut feed = Feed::default();
    let mut cal_tz = default_tz.to_owned();
    let mut cur: Option<Item> = None;
    // Depth of components nested in the current VEVENT (VALARM …).
    let mut nested = 0usize;
    // Inside a non-VEVENT component (VTODO, VTIMEZONE, …).
    let mut other = 0usize;
    for line in unfold(text) {
        let Some(p) = parse_line(&line) else {
            continue;
        };
        match p.name.as_str() {
            "BEGIN" => {
                let comp = p.value.trim().to_ascii_uppercase();
                if cur.is_some() {
                    nested += 1;
                } else if comp == "VEVENT" && other == 0 {
                    cur = Some(Item::default());
                } else if comp != "VCALENDAR" {
                    other += 1;
                }
                continue;
            }
            "END" => {
                let comp = p.value.trim().to_ascii_uppercase();
                if cur.is_some() {
                    if nested > 0 {
                        nested -= 1;
                    } else if comp == "VEVENT" {
                        if let Some(item) = cur.take() {
                            if item.start.is_some() && feed.items.len() < MAX_EVENTS {
                                feed.items.push(item);
                            }
                        }
                    }
                } else if comp != "VCALENDAR" {
                    other = other.saturating_sub(1);
                }
                continue;
            }
            _ => {}
        }
        let Some(item) = cur.as_mut() else {
            if other == 0 {
                match p.name.as_str() {
                    "X-WR-CALNAME" => feed.name = Some(unescape(&p.value)),
                    "X-WR-TIMEZONE" => {
                        if let Some(tz) = crate::parse_tz(&p.value) {
                            cal_tz = tz.name().to_owned();
                        }
                    }
                    _ => {}
                }
            }
            continue;
        };
        if nested > 0 {
            continue;
        }
        let tzid = p.param("TZID");
        let vtype = p.param("VALUE");
        match p.name.as_str() {
            "UID" => item.uid = p.value.trim().to_owned(),
            "SUMMARY" => item.title = unescape(&p.value),
            "LOCATION" => item.location = Some(unescape(&p.value)).filter(|s| !s.is_empty()),
            "DESCRIPTION" => item.description = Some(unescape(&p.value)).filter(|s| !s.is_empty()),
            "URL" => item.url = Some(p.value.trim().to_owned()).filter(|s| !s.is_empty()),
            "DTSTART" => item.start = moment(&p.value, tzid, vtype, &cal_tz),
            "DTEND" => item.end = moment(&p.value, tzid, vtype, &cal_tz),
            "DURATION" => item.duration_secs = duration_secs(&p.value),
            "RRULE" => item.rrule = Some(p.value.trim().to_owned()),
            "EXDATE" => {
                for v in p.value.split(',') {
                    if let Some(m) = moment(v, tzid, vtype, &cal_tz) {
                        item.exdates.push(m);
                    }
                }
            }
            "RECURRENCE-ID" => item.original = moment(&p.value, tzid, vtype, &cal_tz),
            "STATUS" => item.cancelled = p.value.trim().eq_ignore_ascii_case("CANCELLED"),
            _ => {}
        }
    }
    // Overrides point at their master by UID.
    for item in &mut feed.items {
        if item.original.is_some() {
            item.instance_of = Some(item.uid.clone());
        }
        if item.uid.is_empty() {
            item.uid = crate::model::short_hash(&format!("{}|{:?}", item.title, item.start));
        }
    }
    feed
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nX-WR-CALNAME:Lavoro\r\nX-WR-TIMEZONE:Europe/Rome\r\nBEGIN:VTIMEZONE\r\nTZID:Europe/Rome\r\nBEGIN:DAYLIGHT\r\nDTSTART:19700329T020000\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\nBEGIN:VEVENT\r\nUID:standup@x\r\nSUMMARY:Stand-up\\, team\r\nDTSTART;TZID=Europe/Rome:20261019T093000\r\nDTEND;TZID=Europe/Rome:20261019T094500\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE\r\nEXDATE;TZID=Europe/Rome:20261021T093000\r\nDESCRIPTION:Line one\\nline two that is long and\r\n  folded\r\nBEGIN:VALARM\r\nTRIGGER:-PT15M\r\nDESCRIPTION:alarm\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:standup@x\r\nRECURRENCE-ID;TZID=Europe/Rome:20261026T093000\r\nSUMMARY:Stand-up (moved)\r\nDTSTART;TZID=Europe/Rome:20261026T110000\r\nDTEND;TZID=Europe/Rome:20261026T111500\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:holiday@x\r\nSUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20261101\r\nDTEND;VALUE=DATE:20261102\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:call@x\r\nSUMMARY:Call\r\nDTSTART:20261022T150000Z\r\nDURATION:PT30M\r\nSTATUS:CONFIRMED\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:todo\r\nSUMMARY:not an event\r\nDTSTART:20261022T150000Z\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";

    #[test]
    fn parses_events_overrides_and_folding() {
        let f = parse(FEED, "UTC");
        assert_eq!(f.name.as_deref(), Some("Lavoro"));
        assert_eq!(f.items.len(), 4);
        let s = &f.items[0];
        assert_eq!(s.title, "Stand-up, team");
        assert_eq!(
            s.description.as_deref(),
            Some("Line one\nline two that is long and folded")
        );
        assert_eq!(s.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO,WE"));
        assert_eq!(s.exdates.len(), 1);
        assert!(matches!(&s.start, Some(Moment::Local { tz, .. }) if tz == "Europe/Rome"));
        let o = &f.items[1];
        assert_eq!(o.instance_of.as_deref(), Some("standup@x"));
        assert!(o.original.is_some());
        assert!(matches!(f.items[2].start, Some(Moment::Date(_))));
        assert_eq!(f.items[3].duration_secs, Some(1800));
        assert!(matches!(f.items[3].start, Some(Moment::Utc(_))));
    }

    #[test]
    fn durations_and_lines() {
        assert_eq!(duration_secs("P1D"), Some(86_400));
        assert_eq!(duration_secs("PT1H30M"), Some(5400));
        assert_eq!(duration_secs("-PT15M"), Some(-900));
        assert_eq!(duration_secs("P2W"), Some(1_209_600));
        assert_eq!(duration_secs("P1H"), None);
        assert_eq!(duration_secs("garbage"), None);
        let p = parse_line("ATTENDEE;CN=\"Doe; John\":mailto:j@x").unwrap();
        assert_eq!(p.param("CN"), Some("Doe; John"));
        assert_eq!(p.value, "mailto:j@x");
        assert_eq!(unescape("a\\;b\\\\c"), "a;b\\c");
    }

    #[test]
    fn floating_times_use_the_calendar_zone() {
        let f = parse(
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:a\nDTSTART:20261022T090000\nEND:VEVENT\nEND:VCALENDAR\n",
            "Europe/Copenhagen",
        );
        assert!(
            matches!(&f.items[0].start, Some(Moment::Local { tz, .. }) if tz == "Europe/Copenhagen")
        );
    }
}
