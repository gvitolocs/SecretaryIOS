//! Google Calendar API v3 events → [`Item`]s, and the incremental-sync
//! merge.
//!
//! The server lists events with `singleEvents=false`, so recurring events
//! arrive as masters (`recurrence: ["RRULE:…", "EXDATE…"]`) plus exceptions
//! (`recurringEventId` + `originalStartTime`; `status: "cancelled"` for a
//! removed instance), and expands them itself (same code as ICS feeds).

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;

use crate::model::{normalize_alarms, Item, Moment, MAX_TRAVEL_MIN};

/// A Google `EventDateTime` (`{date}` or `{dateTime, timeZone?}`).
pub fn moment(v: &Value, calendar_tz: &str) -> Option<Moment> {
    if let Some(d) = v.get("date").and_then(Value::as_str) {
        return NaiveDate::parse_from_str(d, "%Y-%m-%d")
            .ok()
            .map(Moment::Date);
    }
    let dt = v.get("dateTime").and_then(Value::as_str)?;
    let t = DateTime::parse_from_rfc3339(dt).ok()?.with_timezone(&Utc);
    let zone = v
        .get("timeZone")
        .and_then(Value::as_str)
        .and_then(crate::parse_tz)
        .or_else(|| crate::parse_tz(calendar_tz));
    Some(match zone {
        Some(z) => Moment::Local {
            at: t.with_timezone(&z).naive_local(),
            tz: z.name().to_owned(),
        },
        None => Moment::Utc(t),
    })
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// `hangoutLink`, else the first video entry point of `conferenceData`
/// (https only).
fn conference_url(ev: &Value) -> Option<String> {
    let https = |s: &str| s.starts_with("https://") && s.len() <= 500;
    if let Some(u) = text(ev, "hangoutLink").filter(|u| https(u)) {
        return Some(u);
    }
    ev.pointer("/conferenceData/entryPoints")
        .and_then(Value::as_array)?
        .iter()
        .filter(|p| p.get("entryPointType").and_then(Value::as_str) == Some("video"))
        .find_map(|p| text(p, "uri").filter(|u| https(u)))
}

/// A private extended property (Google stores them as strings).
fn private_prop<'a>(ev: &'a Value, key: &str) -> Option<&'a str> {
    ev.pointer("/extendedProperties/private")?
        .get(key)?
        .as_str()
        .map(str::trim)
}

/// `reminders`: the override minutes, and whether the calendar's defaults
/// apply (also when the event has no `reminders`, as Lucina's older events).
fn reminders(ev: &Value) -> (Vec<i64>, bool) {
    let Some(r) = ev.get("reminders") else {
        return (Vec::new(), true);
    };
    let default = r
        .get("useDefault")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let overrides = r
        .get("overrides")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|o| o.get("minutes").and_then(Value::as_i64))
        .collect();
    (normalize_alarms(overrides), default)
}

/// Google's `defaultReminders` (of an `events.list` response) as minutes.
pub fn default_reminders(list: &Value) -> Option<Vec<i64>> {
    let arr = list.get("defaultReminders")?.as_array()?;
    Some(normalize_alarms(
        arr.iter()
            .filter_map(|o| o.get("minutes").and_then(Value::as_i64))
            .collect(),
    ))
}

/// One Google event → an item (`None` without an id).
pub fn to_item(ev: &Value, calendar_tz: &str) -> Option<Item> {
    let uid = text(ev, "id")?;
    let (alarms, alarms_default) = reminders(ev);
    let mut item = Item {
        attendees: ev
            .get("attendees")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|a| text(a, "displayName").or_else(|| text(a, "email")))
            .take(30)
            .collect(),
        uid,
        cancelled: ev.get("status").and_then(Value::as_str) == Some("cancelled"),
        title: text(ev, "summary").unwrap_or_default(),
        location: text(ev, "location"),
        description: text(ev, "description"),
        url: text(ev, "htmlLink"),
        start: ev.get("start").and_then(|s| moment(s, calendar_tz)),
        end: ev.get("end").and_then(|s| moment(s, calendar_tz)),
        instance_of: text(ev, "recurringEventId"),
        original: ev
            .get("originalStartTime")
            .and_then(|s| moment(s, calendar_tz)),
        conference_url: conference_url(ev),
        added_by: ev
            .pointer("/extendedProperties/private/addedBy")
            .and_then(Value::as_str)
            .or_else(|| ev.pointer("/creator/displayName").and_then(Value::as_str))
            .or_else(|| ev.pointer("/creator/email").and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.chars().take(120).collect()),
        alarms,
        alarms_default,
        free: ev.get("transparency").and_then(Value::as_str) == Some("transparent"),
        travel_min: private_prop(ev, "travelMin")
            .and_then(|m| m.parse::<u32>().ok())
            .filter(|m| (1..=MAX_TRAVEL_MIN).contains(m)),
        leave_alert: matches!(private_prop(ev, "leaveAlert"), Some("1" | "true")),
        ..Item::default()
    };
    for line in ev
        .get("recurrence")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("RRULE:") {
            item.rrule = Some(line[6..].to_owned());
        } else if upper.starts_with("EXDATE") {
            // EXDATE;TZID=Europe/Rome:20261022T090000,20261029T090000 / EXDATE;VALUE=DATE:20261022
            let Some((head, values)) = line.split_once(':') else {
                continue;
            };
            let tzid = head
                .split(';')
                .find_map(|p| p.strip_prefix("TZID="))
                .map(str::to_owned);
            let tz = tzid.as_deref().unwrap_or(calendar_tz);
            for v in values.split(',') {
                let v = v.trim();
                let m = if v.len() == 8 {
                    NaiveDate::parse_from_str(v, "%Y%m%d")
                        .ok()
                        .map(Moment::Date)
                } else if let Some(u) = v.strip_suffix('Z') {
                    chrono::NaiveDateTime::parse_from_str(u, "%Y%m%dT%H%M%S")
                        .ok()
                        .map(|t| Moment::Utc(t.and_utc()))
                } else {
                    chrono::NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S")
                        .ok()
                        .map(|at| Moment::Local {
                            at,
                            tz: crate::parse_tz(tz)
                                .map_or_else(|| tz.to_owned(), |z| z.name().to_owned()),
                        })
                };
                item.exdates.extend(m);
            }
        }
    }
    Some(item)
}

/// Merge one page of `events.list` results into the stored items.
///
/// - a cancelled master (no `recurringEventId`) removes the event and its
///   stored exceptions;
/// - a cancelled exception is kept (it hides that instance);
/// - anything else is inserted or replaced.
///
/// Returns how many entries changed.
pub fn apply_page(
    store: &mut BTreeMap<String, Item>,
    events: &[Value],
    calendar_tz: &str,
) -> usize {
    let mut changed = 0;
    for ev in events {
        let Some(item) = to_item(ev, calendar_tz) else {
            continue;
        };
        changed += 1;
        if item.cancelled && item.instance_of.is_none() {
            let id = item.uid.clone();
            store.remove(&id);
            store.retain(|_, v| v.instance_of.as_deref() != Some(id.as_str()));
            continue;
        }
        store.insert(item.uid.clone(), item);
    }
    changed
}

/// The id Google gives instance `start` of recurring event `master`
/// (`<id>_20261022T070000Z`, `<id>_20261022` for all-day events).
pub fn instance_id(master: &str, start: &Moment, default: chrono_tz::Tz) -> String {
    match start {
        Moment::Date(d) => format!("{master}_{}", d.format("%Y%m%d")),
        other => format!(
            "{master}_{}",
            other.instant(default).format("%Y%m%dT%H%M%SZ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_masters_exceptions_and_cancellations() {
        let master = json!({
            "id": "abc", "status": "confirmed", "summary": "Gym", "htmlLink": "https://cal/x",
            "start": {"dateTime": "2026-10-19T18:00:00+02:00", "timeZone": "Europe/Rome"},
            "end": {"dateTime": "2026-10-19T19:00:00+02:00", "timeZone": "Europe/Rome"},
            "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=MO", "EXDATE;TZID=Europe/Rome:20261026T180000"]
        });
        let it = to_item(&master, "UTC").unwrap();
        assert_eq!(it.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO"));
        assert_eq!(it.exdates.len(), 1);
        assert!(matches!(&it.start, Some(Moment::Local{tz, ..}) if tz == "Europe/Rome"));

        let mut store = BTreeMap::new();
        let moved = json!({"id": "abc_20261102T170000Z", "recurringEventId": "abc", "status": "confirmed",
            "summary": "Gym (late)", "originalStartTime": {"dateTime": "2026-11-02T18:00:00+01:00", "timeZone": "Europe/Rome"},
            "start": {"dateTime": "2026-11-02T20:00:00+01:00"}, "end": {"dateTime": "2026-11-02T21:00:00+01:00"}});
        let gone = json!({"id": "abc_20261109T170000Z", "recurringEventId": "abc", "status": "cancelled",
            "originalStartTime": {"dateTime": "2026-11-09T18:00:00+01:00", "timeZone": "Europe/Rome"}});
        let single = json!({"id": "one", "summary": "Dentist", "start": {"date": "2026-10-23"}, "end": {"date": "2026-10-24"}});
        assert_eq!(
            apply_page(&mut store, &[master, moved, gone, single], "Europe/Rome"),
            4
        );
        assert_eq!(store.len(), 4);
        assert!(store["abc_20261109T170000Z"].cancelled);

        // the master is deleted: its exceptions go too
        apply_page(
            &mut store,
            &[json!({"id": "abc", "status": "cancelled"})],
            "UTC",
        );
        assert_eq!(store.keys().collect::<Vec<_>>(), vec!["one"]);
    }

    #[test]
    fn instance_ids() {
        let m = Moment::Local {
            at: chrono::NaiveDate::from_ymd_opt(2026, 10, 22)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap(),
            tz: "Europe/Rome".into(),
        };
        assert_eq!(
            instance_id("abc", &m, chrono_tz::UTC),
            "abc_20261022T070000Z"
        );
        let d = Moment::Date(chrono::NaiveDate::from_ymd_opt(2026, 10, 22).unwrap());
        assert_eq!(instance_id("abc", &d, chrono_tz::UTC), "abc_20261022");
    }

    #[test]
    fn attendees() {
        let ev = json!({
            "id": "m1", "status": "confirmed", "summary": "Sync",
            "start": {"dateTime": "2026-10-27T09:00:00+01:00"}, "end": {"dateTime": "2026-10-27T10:00:00+01:00"},
            "attendees": [{"displayName": "Anna Rossi", "email": "anna@x.it"}, {"email": "bo@y.dk"}, {}]
        });
        let item = to_item(&ev, "Europe/Copenhagen").unwrap();
        assert_eq!(
            item.attendees,
            vec!["Anna Rossi".to_owned(), "bo@y.dk".to_owned()]
        );
        let none = to_item(
            &json!({"id": "m2", "summary": "Gym", "start": {"date": "2026-10-27"}}),
            "UTC",
        )
        .unwrap();
        assert!(none.attendees.is_empty());
    }

    #[test]
    fn maps_alerts_show_as_and_travel() {
        let tz = "Europe/Copenhagen";
        let e = json!({"id": "a", "summary": "x", "start": {"date": "2026-11-02"},
            "reminders": {"useDefault": false, "overrides": [
                {"method": "popup", "minutes": 1440}, {"method": "email", "minutes": 60},
                {"method": "popup", "minutes": 60}, {"method": "popup", "minutes": -5}]},
            "transparency": "transparent",
            "extendedProperties": {"private": {"travelMin": "25", "leaveAlert": "1"}}});
        let i = to_item(&e, tz).unwrap();
        assert_eq!(i.alarms, vec![60, 1440]);
        assert!(!i.alarms_default);
        assert!(i.free);
        assert_eq!(i.travel_min, Some(25));
        assert!(i.leave_alert);

        let plain = to_item(
            &json!({"id": "b", "start": {"date": "2026-11-02"},
            "reminders": {"useDefault": true}, "transparency": "opaque",
            "extendedProperties": {"private": {"travelMin": "99999"}}}),
            tz,
        )
        .unwrap();
        assert!(plain.alarms.is_empty() && plain.alarms_default && !plain.free);
        assert_eq!(plain.travel_min, None);
        // Lucina's older events have no `reminders`: the defaults apply.
        assert!(
            to_item(&json!({"id": "c", "start": {"date": "2026-11-02"}}), tz)
                .unwrap()
                .alarms_default
        );
        assert_eq!(
            default_reminders(
                &json!({"defaultReminders": [{"method": "popup", "minutes": 30}, {"method": "email", "minutes": 10}]})
            ),
            Some(vec![10, 30])
        );
    }

    #[test]
    fn maps_video_link_and_who_added_it() {
        let tz = "Europe/Copenhagen";
        let base = json!({"id": "a", "summary": "x", "start": {"date": "2026-11-01"}});
        let mut e = base.clone();
        e["hangoutLink"] = json!("https://meet.google.com/abc-defg-hij");
        e["creator"] = json!({"email": "owner@example.com"});
        let i = to_item(&e, tz).unwrap();
        assert_eq!(i.conference_url.as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        assert_eq!(i.added_by.as_deref(), Some("owner@example.com"));

        let mut e = base.clone();
        e["conferenceData"] = json!({"entryPoints": [
            {"entryPointType": "phone", "uri": "tel:+45"},
            {"entryPointType": "video", "uri": "https://zoom.us/j/1"}
        ]});
        e["creator"] = json!({"email": "owner@example.com", "displayName": "Giuseppe"});
        e["extendedProperties"] = json!({"private": {"addedBy": "Lucina"}});
        let i = to_item(&e, tz).unwrap();
        assert_eq!(i.conference_url.as_deref(), Some("https://zoom.us/j/1"));
        assert_eq!(i.added_by.as_deref(), Some("Lucina"));

        let mut e = base;
        e["hangoutLink"] = json!("javascript:alert(1)");
        let i = to_item(&e, tz).unwrap();
        assert_eq!(i.conference_url, None);
        assert_eq!(i.added_by, None);
    }
}
