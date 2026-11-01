//! The shared data model.

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

/// Where an event comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Google,
    Ics,
    Iphone,
    /// Lucina's own calendar, stored by secretary-server.
    Lucina,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Google => "google",
            Source::Ics => "ics",
            Source::Iphone => "iphone",
            Source::Lucina => "lucina",
        }
    }
}

/// A point in time as the source wrote it.
///
/// - `Date`: an all-day value.
/// - `Utc`: an absolute instant.
/// - `Local`: a wall-clock time in a named zone (recurrences expand in it,
///   so a weekly 09:00 stays at 09:00 across DST changes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Moment {
    Date(NaiveDate),
    Utc(DateTime<Utc>),
    Local { at: NaiveDateTime, tz: String },
}

/// Convert a wall-clock time to UTC: the earlier of two ambiguous times, and
/// one hour later for a time inside a DST gap.
pub fn local_to_utc(tz: Tz, at: NaiveDateTime) -> DateTime<Utc> {
    match tz.from_local_datetime(&at) {
        chrono::LocalResult::Single(t) => t.with_timezone(&Utc),
        chrono::LocalResult::Ambiguous(a, _) => a.with_timezone(&Utc),
        chrono::LocalResult::None => tz
            .from_local_datetime(&(at + Duration::hours(1)))
            .earliest()
            .map_or_else(|| Utc.from_utc_datetime(&at), |t| t.with_timezone(&Utc)),
    }
}

/// Local midnight of `d` in `tz`, as UTC.
pub fn day_start(tz: Tz, d: NaiveDate) -> DateTime<Utc> {
    local_to_utc(tz, d.and_hms_opt(0, 0, 0).expect("midnight"))
}

impl Moment {
    pub fn is_date(&self) -> bool {
        matches!(self, Moment::Date(_))
    }

    /// The zone this moment is written in (`default` for dates and UTC).
    pub fn zone(&self, default: Tz) -> Tz {
        match self {
            Moment::Local { tz, .. } => crate::parse_tz(tz).unwrap_or(default),
            _ => default,
        }
    }

    /// The instant (dates: local midnight in `default`).
    pub fn instant(&self, default: Tz) -> DateTime<Utc> {
        match self {
            Moment::Date(d) => day_start(default, *d),
            Moment::Utc(t) => *t,
            Moment::Local { at, tz } => local_to_utc(crate::parse_tz(tz).unwrap_or(default), *at),
        }
    }

    /// Wall-clock time in `zone` (dates: midnight).
    pub fn wall(&self, zone: Tz) -> NaiveDateTime {
        match self {
            Moment::Date(d) => d.and_hms_opt(0, 0, 0).expect("midnight"),
            Moment::Utc(t) => t.with_timezone(&zone).naive_local(),
            Moment::Local { at, tz } => match crate::parse_tz(tz) {
                Some(z) if z == zone => *at,
                Some(z) => local_to_utc(z, *at).with_timezone(&zone).naive_local(),
                None => *at,
            },
        }
    }

    /// The calendar date (in `default` for instants).
    pub fn date(&self, default: Tz) -> NaiveDate {
        match self {
            Moment::Date(d) => *d,
            other => other.instant(default).with_timezone(&default).date_naive(),
        }
    }
}

/// One stored calendar item: a single event, a recurring master (`rrule`),
/// or an override / cancellation of one instance (`instance_of` +
/// `original`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Item {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<String>,
    /// Provider id: Google event id, ICS UID, iPhone EventKit id.
    pub uid: String,
    /// For overrides: the master's uid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_of: Option<String>,
    /// For overrides: the instance's original start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<Moment>,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<Moment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<Moment>,
    /// Seconds (ICS `DURATION`), used when `end` is missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rrule: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exdates: Vec<Moment>,
    /// Sub-calendar label (an EventKit calendar name, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar: Option<String>,
    /// The video call link (Google Meet, or one found in the text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conference_url: Option<String>,
    /// Who added it ("Lucina", a device name, the Google creator).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<String>,
    /// Alerts, in minutes before the start (Google reminder overrides,
    /// VALARM triggers, EventKit alarms), ascending.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alarms: Vec<i64>,
    /// The calendar's default alerts apply (Google `reminders.useDefault`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alarms_default: bool,
    /// Shown as free rather than busy (`TRANSP:TRANSPARENT`, Google
    /// `transparency: "transparent"`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub free: bool,
    /// Travel time blocked before the start, in minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub travel_min: Option<u32>,
    /// A "time to leave" alert (start minus the travel time).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub leave_alert: bool,
}

/// Longest alert offset accepted (four weeks, Google's limit).
pub const MAX_ALARM_MIN: i64 = 40_320;
/// Most alerts per event (Google allows five overrides).
pub const MAX_ALARMS: usize = 5;
/// Longest travel time accepted.
pub const MAX_TRAVEL_MIN: u32 = 1440;

/// Alerts cleaned up: in range, ascending, no duplicates, at most
/// [`MAX_ALARMS`].
pub fn normalize_alarms(mut v: Vec<i64>) -> Vec<i64> {
    v.retain(|m| (0..=MAX_ALARM_MIN).contains(m));
    v.sort_unstable();
    v.dedup();
    v.truncate(MAX_ALARMS);
    v
}

/// An expanded event as the clients see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attendees: Vec<String>,
    /// Unique across sources: `google:<event or instance id>`,
    /// `ics:<subscription>:<hash>`, `iphone:<id>`, `lucina:<id>`.
    pub id: String,
    pub source: Source,
    /// The provider's id of the event (or of its recurring master).
    pub source_id: String,
    /// Which calendar: "Google", the ICS subscription's label, the iPhone
    /// calendar name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calendar: Option<String>,
    pub title: String,
    /// RFC 3339 UTC for timed events; `YYYY-MM-DD` for all-day events.
    pub start: String,
    /// Exclusive end, same format as `start`.
    pub end: String,
    pub all_day: bool,
    pub start_ms: i64,
    pub end_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conference_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_by: Option<String>,
    /// Alerts in minutes before the start, ascending (for Google events
    /// with `alarmsDefault`, the calendar's defaults once the server knows
    /// them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alarms: Vec<i64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub alarms_default: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub free: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub travel_min: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub leave_alert: bool,
    pub recurring: bool,
    /// Whether the server can change it (Lucina's own and Google events).
    pub editable: bool,
}

/// RFC 3339 with milliseconds and `Z`.
pub fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// FNV-1a 64-bit, hex: short stable ids for ICS instances.
pub fn short_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dst_gap_and_ambiguity() {
        let rome = chrono_tz::Europe::Rome;
        // 2026-03-29 02:30 does not exist in Rome: one hour later (03:30 CEST = 01:30Z).
        let gap = NaiveDate::from_ymd_opt(2026, 3, 29)
            .unwrap()
            .and_hms_opt(2, 30, 0)
            .unwrap();
        assert_eq!(iso(local_to_utc(rome, gap)), "2026-03-29T01:30:00.000Z");
        // 2026-10-25 02:30 happens twice: the first (CEST, 00:30Z).
        let amb = NaiveDate::from_ymd_opt(2026, 10, 25)
            .unwrap()
            .and_hms_opt(2, 30, 0)
            .unwrap();
        assert_eq!(iso(local_to_utc(rome, amb)), "2026-10-25T00:30:00.000Z");
    }

    #[test]
    fn moment_serde_roundtrip() {
        let m = Moment::Local {
            at: NaiveDate::from_ymd_opt(2026, 10, 22)
                .unwrap()
                .and_hms_opt(9, 0, 0)
                .unwrap(),
            tz: "Europe/Rome".into(),
        };
        let s = serde_json::to_string(&m).unwrap();
        assert_eq!(
            s,
            r#"{"local":{"at":"2026-10-22T09:00:00","tz":"Europe/Rome"}}"#
        );
        assert_eq!(serde_json::from_str::<Moment>(&s).unwrap(), m);
        assert_eq!(iso(m.instant(chrono_tz::UTC)), "2026-10-22T07:00:00.000Z");
        assert_ne!(short_hash("a"), short_hash("b"));
    }
}
