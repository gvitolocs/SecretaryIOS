//! Expansion and merging: stored items of every source → the events in a
//! window, and free slots between them.

use std::collections::HashMap;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;

use crate::model::{day_start, iso, local_to_utc, short_hash, Event, Item, Moment, Source};
use crate::{google, rrule, MAX_INSTANCES};

/// `[from, to)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// One source's stored items.
pub struct SourceItems<'a> {
    pub source: Source,
    /// ICS subscription id (part of event ids); empty otherwise.
    pub key: &'a str,
    /// Calendar label shown on events (items may carry their own).
    pub label: Option<&'a str>,
    pub items: &'a [Item],
}

fn same_instance(a: &Moment, b: &Moment, tz: Tz) -> bool {
    match (a, b) {
        (Moment::Date(x), Moment::Date(y)) => x == y,
        (Moment::Date(x), other) | (other, Moment::Date(x)) => other.date(tz) == *x,
        _ => a.instant(tz) == b.instant(tz),
    }
}

/// An item's start/end resolved: all-day (dates) or timed (instants).
enum Span {
    Days(NaiveDate, NaiveDate),
    Timed(DateTime<Utc>, DateTime<Utc>),
}

fn span_of(start: &Moment, item: &Item, tz: Tz) -> Span {
    match start {
        Moment::Date(d) => {
            let days = match &item.end {
                Some(Moment::Date(e)) if *e > *d => (*e - *d).num_days(),
                _ => item.duration_secs.map_or(1, |s| (s / 86_400).max(1)),
            };
            Span::Days(*d, *d + Duration::days(days))
        }
        timed => {
            let s = timed.instant(tz);
            let len = match (&item.start, &item.end) {
                (Some(a), Some(b)) if !b.is_date() => {
                    (b.instant(tz) - a.instant(tz)).max(Duration::zero())
                }
                _ => Duration::seconds(item.duration_secs.unwrap_or(0).max(0)),
            };
            Span::Timed(s, s + len)
        }
    }
}

struct Ctx<'a> {
    src: &'a SourceItems<'a>,
    tz: Tz,
    w: Window,
}

impl Ctx<'_> {
    fn event(
        &self,
        item: &Item,
        start: &Moment,
        id_moment: &Moment,
        recurring: bool,
        override_id: bool,
    ) -> Option<Event> {
        let (start_s, end_s, all_day, start_ms, end_ms) = match span_of(start, item, self.tz) {
            Span::Days(a, b) => (
                a.format("%Y-%m-%d").to_string(),
                b.format("%Y-%m-%d").to_string(),
                true,
                day_start(self.tz, a).timestamp_millis(),
                day_start(self.tz, b).timestamp_millis(),
            ),
            Span::Timed(a, b) => (
                iso(a),
                iso(b),
                false,
                a.timestamp_millis(),
                b.timestamp_millis(),
            ),
        };
        let (from, to) = (self.w.from.timestamp_millis(), self.w.to.timestamp_millis());
        let overlaps = if start_ms == end_ms {
            start_ms >= from && start_ms < to
        } else {
            start_ms < to && end_ms > from
        };
        if !overlaps {
            return None;
        }
        let id = match self.src.source {
            Source::Google if override_id || !recurring => format!("google:{}", item.uid),
            Source::Google => format!(
                "google:{}",
                google::instance_id(&item.uid, id_moment, self.tz)
            ),
            Source::Ics => format!(
                "ics:{}:{}",
                self.src.key,
                short_hash(&format!(
                    "{}|{}",
                    item.uid,
                    id_moment.instant(self.tz).timestamp_millis()
                ))
            ),
            Source::Iphone => format!("iphone:{}", item.uid),
            Source::Lucina if override_id || !recurring => format!("lucina:{}", item.uid),
            Source::Lucina => format!(
                "lucina:{}",
                google::instance_id(&item.uid, id_moment, self.tz)
            ),
        };
        Some(Event {
            attendees: item.attendees.iter().take(30).cloned().collect(),
            id,
            source: self.src.source,
            source_id: item.instance_of.clone().unwrap_or_else(|| item.uid.clone()),
            calendar: item
                .calendar
                .clone()
                .or_else(|| self.src.label.map(str::to_owned)),
            title: if item.title.is_empty() {
                "(no title)".into()
            } else {
                item.title.clone()
            },
            start: start_s,
            end: end_s,
            all_day,
            start_ms,
            end_ms,
            location: item.location.clone(),
            description: item.description.clone(),
            url: item.url.clone(),
            conference_url: item.conference_url.clone().or_else(|| {
                [&item.location, &item.description, &item.url]
                    .into_iter()
                    .flatten()
                    .find_map(|t| meeting_url(t))
            }),
            added_by: item.added_by.clone(),
            alarms: item.alarms.clone(),
            alarms_default: item.alarms_default,
            free: item.free,
            travel_min: item.travel_min,
            leave_alert: item.leave_alert,
            recurring,
            editable: matches!(self.src.source, Source::Google | Source::Lucina),
        })
    }
}

/// Every event of `sources` overlapping `w`, sorted by start, at most
/// [`MAX_INSTANCES`]. Recurring items expand in their own zone; all-day
/// events are placed in `tz` (the owner's zone).
pub fn window(sources: &[SourceItems<'_>], w: Window, tz: Tz) -> Vec<Event> {
    let mut out: Vec<Event> = Vec::new();
    for src in sources {
        let ctx = Ctx { src, tz, w };
        let masters: HashMap<&str, &Item> = src
            .items
            .iter()
            .filter(|i| i.instance_of.is_none())
            .map(|i| (i.uid.as_str(), i))
            .collect();
        let mut overrides: HashMap<&str, Vec<&Item>> = HashMap::new();
        for i in src.items.iter() {
            if let Some(m) = i.instance_of.as_deref() {
                if masters.contains_key(m) {
                    overrides.entry(m).or_default().push(i);
                }
            }
        }
        for item in src.items.iter() {
            let Some(start) = item.start.as_ref() else {
                continue;
            };
            if item.cancelled {
                continue;
            }
            let is_override = item
                .instance_of
                .as_deref()
                .is_some_and(|m| masters.contains_key(m));
            if is_override {
                let id_m = item.original.clone().unwrap_or_else(|| start.clone());
                out.extend(ctx.event(item, start, &id_m, true, true));
                continue;
            }
            let rule = item.rrule.as_deref().and_then(|r| rrule::parse(r).ok());
            let Some(rule) = rule else {
                out.extend(ctx.event(item, start, start, false, false));
                continue;
            };
            let zone = start.zone(tz);
            let wall_start = start.wall(zone);
            let horizon = (w.to + Duration::days(1))
                .with_timezone(&zone)
                .naive_local();
            let occ = rrule::expand(&rule, wall_start, horizon, MAX_INSTANCES * 4, |t| {
                local_to_utc(zone, t).naive_utc()
            });
            let ovs = overrides.get(item.uid.as_str());
            for t in occ {
                let m = if start.is_date() {
                    Moment::Date(t.date())
                } else {
                    Moment::Local {
                        at: t,
                        tz: zone.name().to_owned(),
                    }
                };
                if item.exdates.iter().any(|x| same_instance(x, &m, zone)) {
                    continue;
                }
                if ovs.is_some_and(|v| {
                    v.iter().any(|o| {
                        o.original
                            .as_ref()
                            .is_some_and(|x| same_instance(x, &m, zone))
                    })
                }) {
                    continue;
                }
                out.extend(ctx.event(item, &m, &m, true, false));
                if out.len() > MAX_INSTANCES * 2 {
                    break;
                }
            }
        }
    }
    out.sort_by(|a, b| {
        (a.start_ms, !a.all_day, &a.title, &a.id).cmp(&(b.start_ms, !b.all_day, &b.title, &b.id))
    });
    out.dedup_by(|a, b| a.id == b.id);
    out.truncate(MAX_INSTANCES);
    out
}

/// Hosts of video-call links.
const MEETING_HOSTS: [&str; 6] = [
    "meet.google.com",
    "zoom.us",
    "teams.microsoft.com",
    "teams.live.com",
    "webex.com",
    "whereby.com",
];

/// The first video-call link (Meet, Zoom, Teams, Webex, Whereby) in `text`.
pub fn meeting_url(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '(' | ')'))
        .filter_map(|w| w.find("https://").map(|i| &w[i..]))
        .map(|w| w.trim_end_matches(['.', ',', ';', ':', '!', '?', ']']))
        .find(|w| {
            let host = w["https://".len()..]
                .split(['/', '?', '#'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            w.len() <= 500
                && MEETING_HOSTS
                    .iter()
                    .any(|h| host == *h || host.ends_with(&format!(".{h}")))
        })
        .map(str::to_owned)
}

/// Drop the iPhone's copies of Google events: once Google is connected the
/// phone's calendar mirrors the same events, with EventKit ids that contain
/// `<google id>@google.com`.
pub fn drop_mirrored(events: &mut Vec<Event>) {
    let google: Vec<(String, i64)> = events
        .iter()
        .filter(|e| e.source == Source::Google)
        .map(|e| (format!(":{}@google.com", e.source_id), e.start_ms))
        .collect();
    if google.is_empty() {
        return;
    }
    events.retain(|e| {
        e.source != Source::Iphone
            || !google
                .iter()
                .any(|(needle, start)| *start == e.start_ms && e.id.contains(needle.as_str()))
    });
}

/// What [`free_slots`] looks for.
#[derive(Debug, Clone, Copy)]
pub struct SlotQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub minutes: i64,
    pub day_start_hour: u32,
    pub day_end_hour: u32,
    /// At most this many slots.
    pub max: usize,
}

/// Gaps of at least `minutes` between timed events, inside working hours
/// (`day_start_hour..day_end_hour`, local time in `tz`) and `[from, to)`.
/// All-day events do not block time. At most `max` slots, earliest first.
pub fn free_slots(events: &[Event], q: &SlotQuery, tz: Tz) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    let SlotQuery {
        from,
        to,
        minutes,
        day_start_hour,
        day_end_hour,
        max,
    } = *q;
    let need = Duration::minutes(minutes.max(1));
    let mut busy: Vec<(i64, i64)> = events
        .iter()
        .filter(|e| !e.all_day && e.end_ms > e.start_ms)
        .map(|e| (e.start_ms, e.end_ms))
        .collect();
    busy.sort_unstable();
    let mut slots = Vec::new();
    let mut day = from.with_timezone(&tz).date_naive();
    let last = to.with_timezone(&tz).date_naive();
    let (h0, h1) = (
        day_start_hour.min(23),
        day_end_hour.clamp(day_start_hour.min(23) + 1, 24),
    );
    while day <= last && slots.len() < max {
        let open = local_to_utc(tz, day.and_hms_opt(h0, 0, 0).expect("hour")).max(from);
        let close = if h1 == 24 {
            day_start(tz, day + Duration::days(1))
        } else {
            local_to_utc(tz, day.and_hms_opt(h1, 0, 0).expect("hour"))
        }
        .min(to);
        let mut cursor = open;
        for &(s, e) in &busy {
            if cursor >= close {
                break;
            }
            let (s, e) = (
                DateTime::<Utc>::from_timestamp_millis(s).unwrap_or(cursor),
                DateTime::<Utc>::from_timestamp_millis(e).unwrap_or(cursor),
            );
            if e <= cursor {
                continue;
            }
            if s >= close {
                break;
            }
            if s > cursor && s.min(close) - cursor >= need {
                slots.push((cursor, s.min(close)));
                if slots.len() >= max {
                    return slots;
                }
            }
            cursor = cursor.max(e);
        }
        if cursor < close && close - cursor >= need {
            slots.push((cursor, close));
        }
        day += Duration::days(1);
    }
    slots.truncate(max);
    slots
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ics;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    const ROME: Tz = chrono_tz::Europe::Rome;

    #[test]
    fn ics_recurrence_exdate_override_across_dst() {
        let feed = ics::parse(
            "BEGIN:VCALENDAR\nBEGIN:VEVENT\nUID:s\nSUMMARY:Standup\nDTSTART;TZID=Europe/Rome:20261019T093000\nDTEND;TZID=Europe/Rome:20261019T094500\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE\nEXDATE;TZID=Europe/Rome:20261021T093000\nEND:VEVENT\nBEGIN:VEVENT\nUID:s\nRECURRENCE-ID;TZID=Europe/Rome:20261026T093000\nSUMMARY:Standup (moved)\nDTSTART;TZID=Europe/Rome:20261026T110000\nDTEND;TZID=Europe/Rome:20261026T111500\nEND:VEVENT\nEND:VCALENDAR\n",
            "UTC",
        );
        let src = SourceItems {
            source: Source::Ics,
            key: "sub1",
            label: Some("Lavoro"),
            items: &feed.items,
        };
        let w = Window {
            from: utc("2026-10-19T00:00:00Z"),
            to: utc("2026-11-01T00:00:00Z"),
        };
        let ev = window(&[src], w, ROME);
        let got: Vec<(String, String)> = ev
            .iter()
            .map(|e| (e.start.clone(), e.title.clone()))
            .collect();
        let want = [
            ("2026-10-19T07:30:00.000Z", "Standup"),
            // 10-21 is an EXDATE; 10-26 is after the DST change and moved to 11:00 CET
            ("2026-10-26T10:00:00.000Z", "Standup (moved)"),
            ("2026-10-28T08:30:00.000Z", "Standup"),
        ];
        assert_eq!(
            got,
            want.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect::<Vec<_>>()
        );
        assert!(ev
            .iter()
            .all(|e| e.calendar.as_deref() == Some("Lavoro") && e.id.starts_with("ics:sub1:")));
        assert!(ev.iter().all(|e| e.recurring && !e.editable));
    }

    #[test]
    fn google_items_all_day_and_instance_ids() {
        let mut store = BTreeMap::new();
        google::apply_page(
            &mut store,
            &[
                json!({"id": "g1", "summary": "Gym", "start": {"dateTime": "2026-10-19T18:00:00+02:00", "timeZone": "Europe/Rome"},
                       "end": {"dateTime": "2026-10-19T19:00:00+02:00", "timeZone": "Europe/Rome"}, "recurrence": ["RRULE:FREQ=WEEKLY;COUNT=3"]}),
                json!({"id": "g1_20261026T170000Z", "recurringEventId": "g1", "status": "cancelled",
                       "originalStartTime": {"dateTime": "2026-10-26T18:00:00+01:00", "timeZone": "Europe/Rome"}}),
                json!({"id": "trip", "summary": "Trip", "start": {"date": "2026-10-20"}, "end": {"date": "2026-10-23"}}),
            ],
            "Europe/Rome",
        );
        let items: Vec<Item> = store.into_values().collect();
        let src = SourceItems {
            source: Source::Google,
            key: "",
            label: Some("Google"),
            items: &items,
        };
        let w = Window {
            from: utc("2026-10-18T22:00:00Z"),
            to: utc("2026-11-10T00:00:00Z"),
        };
        let ev = window(&[src], w, ROME);
        let ids: Vec<&str> = ev.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "google:g1_20261019T160000Z",
                "google:trip",
                "google:g1_20261102T170000Z"
            ]
        );
        let trip = &ev[1];
        assert!(trip.all_day && trip.start == "2026-10-20" && trip.end == "2026-10-23");
        assert!(trip.editable && !trip.recurring);
        assert_eq!(ev[0].source_id, "g1");
    }

    #[test]
    fn window_overlap_and_free_slots() {
        let items = vec![
            Item {
                uid: "a".into(),
                title: "A".into(),
                start: Some(Moment::Utc(utc("2026-10-22T07:00:00Z"))),
                end: Some(Moment::Utc(utc("2026-10-22T08:00:00Z"))),
                ..Item::default()
            },
            Item {
                uid: "b".into(),
                title: "B".into(),
                start: Some(Moment::Utc(utc("2026-10-22T07:30:00Z"))),
                end: Some(Moment::Utc(utc("2026-10-22T09:00:00Z"))),
                ..Item::default()
            },
            Item {
                uid: "c".into(),
                title: "C".into(),
                start: Some(Moment::Utc(utc("2026-10-22T12:00:00Z"))),
                end: Some(Moment::Utc(utc("2026-10-22T13:00:00Z"))),
                ..Item::default()
            },
            Item {
                uid: "old".into(),
                title: "Old".into(),
                start: Some(Moment::Utc(utc("2026-10-20T07:00:00Z"))),
                end: Some(Moment::Utc(utc("2026-10-20T08:00:00Z"))),
                ..Item::default()
            },
        ];
        let src = SourceItems {
            source: Source::Iphone,
            key: "",
            label: None,
            items: &items,
        };
        let day = Window {
            from: utc("2026-10-21T22:00:00Z"),
            to: utc("2026-10-22T22:00:00Z"),
        };
        let ev = window(&[src], day, ROME);
        assert_eq!(
            ev.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["iphone:a", "iphone:b", "iphone:c"]
        );
        // Working hours 09-18 Rome = 07-16Z: busy 07-09Z and 12-13Z.
        let q = SlotQuery {
            from: day.from,
            to: day.to,
            minutes: 60,
            day_start_hour: 9,
            day_end_hour: 18,
            max: 5,
        };
        let slots = free_slots(&ev, &q, ROME);
        assert_eq!(
            slots
                .iter()
                .map(|(a, b)| (iso(*a), iso(*b)))
                .collect::<Vec<_>>(),
            vec![
                (
                    "2026-10-22T09:00:00.000Z".to_string(),
                    "2026-10-22T12:00:00.000Z".to_string()
                ),
                (
                    "2026-10-22T13:00:00.000Z".to_string(),
                    "2026-10-22T16:00:00.000Z".to_string()
                ),
            ]
        );
        // 4-hour slot: none that day
        assert!(free_slots(&ev, &SlotQuery { minutes: 240, ..q }, ROME).is_empty());
    }

    #[test]
    fn meeting_links_are_found() {
        assert_eq!(
            meeting_url("Join: https://meet.google.com/abc-defg-hij.").as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
        assert_eq!(
            meeting_url("Zoom (https://us02web.zoom.us/j/123?pwd=x)").as_deref(),
            Some("https://us02web.zoom.us/j/123?pwd=x")
        );
        assert_eq!(
            meeting_url("<a href=\"https://teams.microsoft.com/l/meetup-join/1\">Teams</a>").as_deref(),
            Some("https://teams.microsoft.com/l/meetup-join/1")
        );
        assert_eq!(meeting_url("https://example.com/zoom.us and http://meet.google.com/x"), None);
        assert_eq!(meeting_url("https://notzoom.us/j/1"), None);
    }

    fn ev(id: &str, source: Source, source_id: &str, start_ms: i64) -> Event {
        Event {
            attendees: vec![],
            id: id.into(),
            source,
            source_id: source_id.into(),
            calendar: None,
            title: "x".into(),
            start: String::new(),
            end: String::new(),
            all_day: false,
            start_ms,
            end_ms: start_ms + 1,
            location: None,
            description: None,
            url: None,
            conference_url: None,
            added_by: None,
            alarms: vec![],
            alarms_default: false,
            free: false,
            travel_min: None,
            leave_alert: false,
            recurring: false,
            editable: false,
        }
    }

    #[test]
    fn iphone_copies_of_google_events_are_dropped() {
        let mut v = vec![
            ev("google:abc", Source::Google, "abc", 1000),
            ev("iphone:CAL:abc@google.com@1000", Source::Iphone, "CAL:abc@google.com@1000", 1000),
            // Another instance of the same series (different start) stays.
            ev("iphone:CAL:abc@google.com@2000", Source::Iphone, "CAL:abc@google.com@2000", 2000),
            // A holiday calendar the server does not sync stays.
            ev("iphone:HOL:hol1@google.com@1000", Source::Iphone, "HOL:hol1@google.com@1000", 1000),
            ev("iphone:CAL:local@1000", Source::Iphone, "CAL:local@1000", 1000),
            ev("lucina:ev_1", Source::Lucina, "ev_1", 1000),
        ];
        drop_mirrored(&mut v);
        let ids: Vec<&str> = v.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "google:abc",
                "iphone:CAL:abc@google.com@2000",
                "iphone:HOL:hol1@google.com@1000",
                "iphone:CAL:local@1000",
                "lucina:ev_1"
            ]
        );
    }

    #[test]
    fn lucina_events_are_editable_and_keep_their_fields() {
        let items = vec![Item {
            uid: "ev_1".into(),
            title: "Call".into(),
            start: Some(Moment::Utc(utc("2026-10-30T10:00:00Z"))),
            end: Some(Moment::Utc(utc("2026-10-30T11:00:00Z"))),
            description: Some("link https://meet.google.com/aaa-bbbb-ccc".into()),
            added_by: Some("Lucina".into()),
            ..Item::default()
        }];
        let w = Window {
            from: utc("2026-10-30T00:00:00Z"),
            to: utc("2026-10-31T00:00:00Z"),
        };
        let out = window(
            &[SourceItems {
                source: Source::Lucina,
                key: "",
                label: Some("Lucina"),
                items: &items,
            }],
            w,
            chrono_tz::Europe::Copenhagen,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "lucina:ev_1");
        assert!(out[0].editable);
        assert_eq!(out[0].calendar.as_deref(), Some("Lucina"));
        assert_eq!(out[0].added_by.as_deref(), Some("Lucina"));
        assert_eq!(
            out[0].conference_url.as_deref(),
            Some("https://meet.google.com/aaa-bbbb-ccc")
        );
    }
}
