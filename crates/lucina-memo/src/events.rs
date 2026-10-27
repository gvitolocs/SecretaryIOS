//! Calendar context, classification and local location matching.
use crate::{
    kernel::{self, Cand},
    model::{EventKind, EventLink, Memo},
    search::fold,
    App,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::Duration,
};

const LECTURE: &[&str] = &[
    "lecture",
    "lezione",
    "corso",
    "course",
    "forelæsning",
    "seminar",
    "seminario",
    "lab",
    "laboratorio",
    "esercitazione",
    "exercise class",
    "tutorial",
    "øvelse",
    "undervisning",
    "class",
    "lesson",
];
/// Calendar labels of a university timetable ("Aarhus Universitet - Personal timetable: …").
const LECTURE_CALENDARS: &[&str] = &[
    "uni",
    "dtu",
    "university",
    "universitet",
    "università",
    "universita",
    "universität",
    "corsi",
    "courses",
    "skema",
    "timetable",
    "orario",
];
const MEETING: &[&str] = &[
    "meeting",
    "riunione",
    "call",
    "møde",
    "sync",
    "standup",
    "stand-up",
    "1:1",
    "one-on-one",
    "interview",
    "colloquio",
    "review",
    "retro",
];
/// Words a course's events add to its name: "Algorithms – Lecture 5", "Algoritmi, lezione 3".
const SERIES_NOISE: &[&str] = &[
    "lecture",
    "lezione",
    "lesson",
    "forelæsning",
    "session",
    "sessione",
    "week",
    "settimana",
    "uge",
    "part",
    "parte",
    "del",
    "no",
    "nr",
    "n",
];

/// Whether any of `words` (folded, so accents and punctuation don't matter) is a whole word
/// or phrase of `text`.
fn has_word(text: &str, words: &[&str]) -> bool {
    let text = format!(
        " {} ",
        fold(text).split_whitespace().collect::<Vec<_>>().join(" ")
    );
    words.iter().any(|w| {
        let w = fold(w).split_whitespace().collect::<Vec<_>>().join(" ");
        text.contains(&format!(" {w} "))
    })
}

/// Lecture, meeting or other, from the event's words, its calendar and its attendees.
pub fn kind(
    title: &str,
    calendar: Option<&str>,
    description: Option<&str>,
    attendees: usize,
) -> EventKind {
    let text = format!("{title} {}", description.unwrap_or_default());
    if has_word(&text, LECTURE) || calendar.is_some_and(|c| has_word(c, LECTURE_CALENDARS)) {
        EventKind::Lecture
    } else if attendees >= 2 || has_word(&text, MEETING) {
        EventKind::Meeting
    } else {
        EventKind::Other
    }
}

fn noise(word: &str) -> bool {
    let f = fold(word);
    let f = f.trim();
    f.is_empty() || f.chars().any(|c| c.is_ascii_digit()) || SERIES_NOISE.contains(&f)
}

/// The name a series of events shares: the title without trailing numbers, dates and words
/// like "Lecture" ("Algorithms – Lecture 5" → "Algorithms").
pub fn series_title(title: &str) -> String {
    let mut words: Vec<&str> = title.split_whitespace().collect();
    while words.len() > 1 && words.last().is_some_and(|w| noise(w)) {
        words.pop();
    }
    let t = words.join(" ");
    let t = t
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_owned();
    if t.is_empty() {
        "Event".into()
    } else {
        t
    }
}

/// The folded key of [`series_title`] (dashes, case and accents ignored).
pub fn series_key(title: &str) -> String {
    fold(&series_title(title))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEvent {
    pub id: String,
    pub source: String,
    pub source_id: String,
    pub calendar: Option<String>,
    pub title: String,
    pub start: String,
    pub end: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub all_day: bool,
    pub location: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub attendees: Vec<String>,
    pub recurring: bool,
}
/// Memos of the same course or recurring meeting share this: the recurring master's id when
/// the calendar has one (Google instances are `<master>_<start>`), else [`series_key`].
pub fn series(e: &CalendarEvent) -> String {
    if e.recurring {
        if e.source == "google" {
            return e
                .source_id
                .rsplit_once('_')
                .filter(|(_, suffix)| suffix.starts_with(|c: char| c.is_ascii_digit()))
                .map_or_else(|| e.source_id.clone(), |(master, _)| master.to_owned());
        }
        return e.source_id.clone();
    }
    series_key(&e.title)
}

impl CalendarEvent {
    pub fn link(&self) -> EventLink {
        EventLink {
            id: self.id.clone(),
            source: self.source.clone(),
            calendar: self.calendar.clone(),
            title: self.title.clone(),
            start: self.start.clone(),
            end: self.end.clone(),
            start_ms: self.start_ms,
            end_ms: self.end_ms,
            location: self.location.clone(),
            attendees: self.attendees.iter().take(30).cloned().collect(),
            recurring: self.recurring,
            series: series(self),
            kind: kind(
                &self.title,
                self.calendar.as_deref(),
                self.description.as_deref(),
                self.attendees.len(),
            ),
        }
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub event: EventLink,
    pub overlap_ms: i64,
    pub loc_match: bool,
    #[serde(skip)]
    pub cand: Cand,
}

pub fn configured(app: &App) -> bool {
    app.cfg.calendar_url.is_some() && app.cfg.notify.service_token_file.is_some()
}
/// secretary-server's events (Google, ICS, iPhone calendars) between `from` and `to`.
pub async fn fetch(
    app: &App,
    from: chrono::DateTime<chrono::Utc>,
    to: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<CalendarEvent>, String> {
    let base = app
        .cfg
        .calendar_url
        .as_ref()
        .ok_or("calendar not configured")?;
    let token = std::fs::read_to_string(
        app.cfg
            .notify
            .service_token_file
            .as_ref()
            .ok_or("calendar token not configured")?,
    )
    .map_err(|e| e.to_string())?;
    #[derive(Deserialize)]
    struct Reply {
        events: Vec<CalendarEvent>,
    }
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())?;
    let reply: Reply = http
        .get(format!("{}/v1/calendar/events", base.trim_end_matches('/')))
        .bearer_auth(token.trim())
        .query(&[("from", from.to_rfc3339()), ("to", to.to_rfc3339())])
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?
        .error_for_status()
        .map_err(|e| e.without_url().to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(reply.events)
}

pub async fn candidates(app: &App, m: &Memo, minutes: i64) -> Result<Vec<Candidate>, String> {
    let events = fetch(
        app,
        m.recorded_at - chrono::Duration::minutes(minutes),
        m.recording_end() + chrono::Duration::minutes(minutes),
    )
    .await?;
    let mut out = Vec::new();
    for e in events {
        let loc_match = match (&m.location, &app.cfg.geocode_url, &e.location) {
            (Some(fix), Some(url), Some(location)) => geocode_match(url, location, fix).await,
            _ => false,
        };
        out.push(Candidate {
            overlap_ms: kernel::overlap_ms(
                m.recorded_at.timestamp_millis(),
                m.recording_end().timestamp_millis(),
                e.start_ms,
                e.end_ms,
            ),
            loc_match,
            cand: Cand {
                start_ms: e.start_ms,
                end_ms: e.end_ms,
                all_day: e.all_day,
                loc_match,
            },
            event: e.link(),
        });
    }
    // Repeated selection preserves the exact K8 ordering; remaining ineligible events follow.
    let mut sorted = Vec::new();
    while let Some(i) = kernel::pick_event(
        m.recorded_at.timestamp_millis(),
        m.recording_end().timestamp_millis(),
        &out.iter().map(|c| c.cand).collect::<Vec<_>>(),
    ) {
        sorted.push(out.remove(i));
    }
    out.sort_by_key(|c| {
        (
            std::cmp::Reverse(c.loc_match),
            std::cmp::Reverse(c.overlap_ms),
            c.event.end_ms as i128 - c.event.start_ms as i128,
            c.event.start_ms,
        )
    });
    sorted.extend(out);
    Ok(sorted)
}
type Geocodes = HashMap<String, Vec<(f64, f64)>>;
async fn geocode_match(url: &str, location: &str, fix: &crate::model::GeoFix) -> bool {
    static CACHE: OnceLock<Mutex<Geocodes>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    let key = format!("{url}|{location}|{}|{}", fix.lat, fix.lon);
    let cached = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned();
    let points = if let Some(p) = cached {
        p
    } else {
        #[derive(Deserialize)]
        struct Point {
            lat: f64,
            lng: f64,
        }
        #[derive(Deserialize)]
        struct Reply {
            results: Vec<Point>,
        }
        let result = async {
            reqwest::Client::new()
                .get(format!("{}/geocode", url.trim_end_matches('/')))
                .timeout(Duration::from_secs(3))
                .query(&[
                    ("q", location.to_owned()),
                    ("lat", fix.lat.to_string()),
                    ("lng", fix.lon.to_string()),
                    ("limit", "3".into()),
                ])
                .send()
                .await?
                .error_for_status()?
                .json::<Reply>()
                .await
        }
        .await;
        let points: Vec<_> = result
            .map(|r| r.results.into_iter().map(|p| (p.lat, p.lng)).collect())
            .unwrap_or_default();
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, points.clone());
        points
    };
    points.into_iter().any(|(lat, lon)| {
        distance(fix.lat, fix.lon, lat, lon) <= 400.0 + fix.accuracy_m.unwrap_or(0.0)
    })
}
fn distance(a: f64, b: f64, c: f64, d: f64) -> f64 {
    let h = ((c - a).to_radians() / 2.0).sin().powi(2)
        + a.to_radians().cos() * c.to_radians().cos() * ((d - b).to_radians() / 2.0).sin().powi(2);
    6_371_000.0 * 2.0 * h.clamp(0.0, 1.0).sqrt().asin()
}
/// K10: a linked memo is named after its event and date ("Algorithms · 27 Oct 2026"),
/// unless the owner named it.
pub fn apply_title(m: &mut Memo, tz: chrono_tz::Tz) {
    if let Some(e) = &m.event {
        if kernel::title_from_event(kernel::TitleSource::parse(&m.title_source)) {
            let tz = m
                .timezone
                .as_deref()
                .and_then(|s| s.parse::<chrono_tz::Tz>().ok())
                .unwrap_or(tz);
            m.title = format!(
                "{} · {}",
                e.title,
                m.recorded_at.with_timezone(&tz).format("%-d %b %Y")
            );
            m.title_source = "event".into();
        }
    }
}

/// Earlier memos of the same series (course, recurring meeting), newest first.
pub fn previous(app: &App, m: &Memo) -> Vec<Memo> {
    let Some(e) = &m.event else {
        return vec![];
    };
    let mut memos: Vec<_> = app
        .store
        .list()
        .into_iter()
        .filter(|p| {
            p.recorded_at < m.recorded_at && p.event.as_ref().is_some_and(|v| v.series == e.series)
        })
        .collect();
    memos.sort_by_key(|p| std::cmp::Reverse(p.recorded_at));
    memos
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(title: &str, source: &str, source_id: &str, recurring: bool) -> CalendarEvent {
        CalendarEvent {
            id: format!("{source}:{source_id}"),
            source: source.into(),
            source_id: source_id.into(),
            calendar: None,
            title: title.into(),
            start: String::new(),
            end: String::new(),
            start_ms: 0,
            end_ms: 0,
            all_day: false,
            location: None,
            description: None,
            attendees: vec![],
            recurring,
        }
    }

    #[test]
    fn kinds() {
        assert_eq!(
            kind("Algorithms – Lecture 5", None, None, 0),
            EventKind::Lecture
        );
        assert_eq!(
            kind("Algoritmi, lezione 3", None, None, 0),
            EventKind::Lecture
        );
        assert_eq!(
            kind("Forelæsning: Statistik", None, None, 0),
            EventKind::Lecture
        );
        assert_eq!(
            kind("02105 Algorithms", Some("DTU skema"), None, 0),
            EventKind::Lecture
        );
        // The owner's real timetable (iPhone calendar of Aarhus University).
        assert_eq!(
            kind(
                "Communications and Network Security",
                Some("Aarhus Universitet - Personal timetable: 810677"),
                Some("5125-140 Seminarrum"),
                0
            ),
            EventKind::Lecture
        );
        assert_eq!(kind("Riunione progetto", None, None, 0), EventKind::Meeting);
        assert_eq!(kind("Sprint review", None, None, 0), EventKind::Meeting);
        assert_eq!(kind("Lunch", None, None, 3), EventKind::Meeting);
        assert_eq!(kind("1:1 with Anna", None, None, 0), EventKind::Meeting);
        assert_eq!(kind("Dentist", None, None, 1), EventKind::Other);
        // Whole words only: "Community" is not "uni", "labour" is not "lab", "recall" is not "call".
        assert_eq!(kind("Gym", Some("Community"), None, 0), EventKind::Other);
        assert_eq!(kind("Labour day", None, None, 0), EventKind::Other);
        assert_eq!(kind("Recall the plumber", None, None, 0), EventKind::Other);
    }

    #[test]
    fn series_of_a_course() {
        assert_eq!(series_title("Algorithms – Lecture 5"), "Algorithms");
        assert_eq!(series_title("Algoritmi, lezione 3"), "Algoritmi");
        assert_eq!(series_title("Team sync"), "Team sync");
        assert_eq!(series_title("2026"), "2026");
        assert_eq!(series_title("   "), "Event");
        assert_eq!(
            series_key("Algorithms – Lecture 5"),
            series_key("algorithms - lecture 6")
        );
        assert_ne!(series_key("Algorithms"), series_key("Statistics"));
        // Recurring: the master's id (Google instances carry `_<start>`).
        let g = event("Algorithms", "google", "abc123_20261027T080000Z", true);
        assert_eq!(series(&g), "abc123");
        let ics = event("Algorithms", "ics", "uid-77@dtu.dk", true);
        assert_eq!(series(&ics), "uid-77@dtu.dk");
        assert_eq!(
            series(&event("Algorithms – Lecture 5", "iphone", "x", false)),
            "algorithms"
        );
    }

    #[test]
    fn titles_follow_the_event_unless_the_owner_named_it() {
        let mut m = crate::obsidian::tests::memo();
        m.recorded_at = "2026-10-27T08:15:00Z".parse().unwrap();
        m.timezone = Some("Europe/Copenhagen".into());
        m.title_source = "lucina".into();
        m.event = Some(event("Algorithms – Lecture 5", "google", "a_1", true).link());
        apply_title(&mut m, chrono_tz::Europe::Rome);
        assert_eq!(m.title, "Algorithms – Lecture 5 · 27 Oct 2026");
        assert_eq!(m.title_source, "event");
        m.title = "Mine".into();
        m.title_source = "owner".into();
        apply_title(&mut m, chrono_tz::Europe::Rome);
        assert_eq!(m.title, "Mine");
    }
}
