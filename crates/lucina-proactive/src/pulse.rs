//! The Daily Pulse: one short morning message at `pulse.time` (08:00
//! local), with up to three important things for today, what got
//! completed, and what to keep an eye on. Silent when there is nothing, or
//! when everything in it was already in the previous Pulse. Never inside
//! quiet hours or while he is gaming: it waits, until `pulse.deadline`.
//!
//! - important: the day's best digest candidates (by score), today's
//!   remaining timed calendar events, reminders due today;
//! - completed: digest candidates of completion kinds (a followed thread
//!   resolved, a service back up);
//! - keep an eye on: open threads, deadlines with no plan, pinned notes for
//!   upcoming events, deadline-like reminders due in the next days.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use lucina_store::Reminder;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::config::Config;
use crate::model::{Candidate, Event, Kind};
use crate::secretary::CalEvent;
use crate::sources::reminders::looks_like_deadline;
use crate::text;

#[derive(Debug, Clone, PartialEq)]
pub struct PulseItem {
    /// Stable across days for the same thing (the "is it new?" check).
    pub key: String,
    pub line: String,
    pub candidate: Option<Uuid>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PulseDraft {
    pub important: Vec<PulseItem>,
    pub completed: Vec<PulseItem>,
    pub watch: Vec<PulseItem>,
}

impl PulseDraft {
    pub fn is_empty(&self) -> bool {
        self.important.is_empty() && self.completed.is_empty() && self.watch.is_empty()
    }
    pub fn items(&self) -> impl Iterator<Item = &PulseItem> {
        self.important
            .iter()
            .chain(&self.completed)
            .chain(&self.watch)
    }
    pub fn keys(&self) -> Vec<String> {
        self.items().map(|i| i.key.clone()).collect()
    }
    /// Items not in the previous Pulse.
    pub fn new_items(&self, previous: &[String]) -> usize {
        self.items().filter(|i| !previous.contains(&i.key)).count()
    }
    pub fn candidates(&self) -> Vec<Uuid> {
        self.items().filter_map(|i| i.candidate).collect()
    }
    pub fn facts(&self) -> Value {
        let lines = |v: &[PulseItem]| {
            v.iter()
                .map(|i| json!({"line": i.line}))
                .collect::<Vec<_>>()
        };
        json!({
            "important": lines(&self.important),
            "completed": lines(&self.completed),
            "watch": lines(&self.watch),
        })
    }
}

fn hhmm(t: DateTime<Utc>, tz: Tz) -> String {
    t.with_timezone(&tz).format("%H:%M").to_string()
}

fn day(t: DateTime<Utc>, tz: Tz) -> String {
    t.with_timezone(&tz).format("%d/%m").to_string()
}

/// A stable key for a candidate's thing (not its per-day dedup key).
fn candidate_key(e: &Event) -> String {
    let parts: Vec<&str> = e.dedup_key.split(':').collect();
    match e.kind {
        // thread_open:<anchor>:<date> → thread:<anchor>
        Kind::ThreadOpen | Kind::ThreadResolved => {
            format!("thread:{}:{}", e.kind, parts.get(1).unwrap_or(&""))
        }
        // due_no_plan:<id>:<bucket> → due:<id>
        Kind::DueNoPlan => format!("due:{}", parts.get(1).unwrap_or(&"")),
        _ => e.dedup_key.clone(),
    }
}

fn candidate_line(e: &Event, tz: Tz) -> String {
    let p = &e.payload;
    let s = |ptr: &str| {
        p.pointer(ptr)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let line = match e.kind {
        Kind::ThreadOpen | Kind::ThreadResolved => {
            let sum = s("/summary");
            if sum.is_empty() {
                e.subject.clone()
            } else {
                sum
            }
        }
        Kind::DueNoPlan => {
            let due = p
                .pointer("/reminder/dueAt")
                .and_then(Value::as_str)
                .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                .map(|d| day(d.with_timezone(&Utc), tz))
                .unwrap_or_default();
            format!("{} ({due})", e.subject)
        }
        Kind::NoteForEvent => {
            let start = p
                .pointer("/event/start")
                .and_then(Value::as_str)
                .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                .map(|d| {
                    format!(
                        "{} {}",
                        day(d.with_timezone(&Utc), tz),
                        hhmm(d.with_timezone(&Utc), tz)
                    )
                })
                .unwrap_or_default();
            format!(
                "for \"{}\" ({start}) the pinned note \"{}\" is ready",
                s("/event/title"),
                s("/note/slug")
            )
        }
        Kind::MeetingPrep => {
            let start = p
                .pointer("/event/start")
                .and_then(Value::as_str)
                .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                .map(|d| hhmm(d.with_timezone(&Utc), tz))
                .unwrap_or_default();
            format!("{start} {}", e.subject)
        }
        _ => e.subject.clone(),
    };
    text::clip(&line, 160)
}

pub struct PulseInput<'a> {
    pub digests: &'a [(Candidate, Event)],
    /// Calendar events from now to the end of the local day.
    pub today_events: &'a [CalEvent],
    pub reminders: &'a [Reminder],
}

pub fn build(cfg: &Config, now: DateTime<Utc>, input: &PulseInput<'_>) -> PulseDraft {
    let tz = cfg.tz();
    let p = &cfg.pulse;
    let today = now.with_timezone(&tz).date_naive();
    let mut d = PulseDraft::default();

    let mut digests: Vec<&(Candidate, Event)> = input.digests.iter().collect();
    digests.sort_by(|a, b| b.0.score.total_cmp(&a.0.score));

    let mut seen = std::collections::BTreeSet::new();
    let mut push = |list: &mut Vec<PulseItem>, max: usize, item: PulseItem| {
        if list.len() < max && seen.insert(item.key.clone()) {
            list.push(item);
        }
    };

    for (c, e) in &digests {
        let item = PulseItem {
            key: candidate_key(e),
            line: candidate_line(e, tz),
            candidate: Some(c.id),
        };
        if e.kind.is_completion() {
            push(&mut d.completed, p.max_completed, item);
        } else if e.kind.is_watch() {
            push(&mut d.watch, p.max_watch, item);
        } else if e.kind != Kind::MeetingPrep {
            // A meeting without prep is in the calendar lines below.
            push(&mut d.important, p.max_important, item);
        }
    }
    for e in input.today_events.iter().filter(|e| !e.all_day) {
        let Some(start) = e.starts() else { continue };
        if start < now || start.with_timezone(&tz).date_naive() != today {
            continue;
        }
        push(
            &mut d.important,
            p.max_important,
            PulseItem {
                key: format!("cal:{}", e.id),
                line: text::clip(&format!("{} {}", hhmm(start, tz), e.title), 160),
                candidate: None,
            },
        );
    }
    let mut pending: Vec<(&Reminder, DateTime<Utc>)> = input
        .reminders
        .iter()
        .filter(|r| r.status == "pending")
        .filter_map(|r| r.due().map(|d| (r, d)))
        .filter(|(_, d)| *d >= now)
        .collect();
    pending.sort_by_key(|(_, d)| *d);
    for (r, due) in &pending {
        if due.with_timezone(&tz).date_naive() == today {
            push(
                &mut d.important,
                p.max_important,
                PulseItem {
                    key: format!("rem:{}", r.id),
                    line: text::clip(&format!("{} {}", hhmm(*due, tz), r.text), 160),
                    candidate: None,
                },
            );
        } else if *due <= now + chrono::Duration::days(p.watch_reminder_days)
            && looks_like_deadline(&r.text)
        {
            push(
                &mut d.watch,
                p.max_watch,
                PulseItem {
                    key: format!("due:{}", r.id),
                    line: text::clip(&format!("{} ({})", r.text, day(*due, tz)), 160),
                    candidate: None,
                },
            );
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Decision, Priority, Scores};

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn reminder(id: &str, due: &str, text: &str) -> Reminder {
        serde_json::from_value(json!({"id": id, "text": text, "dueAt": due, "status": "pending"}))
            .unwrap()
    }

    fn digest(
        kind: Kind,
        key: &str,
        subject: &str,
        score: f32,
        payload: Value,
    ) -> (Candidate, Event) {
        let now = at("2026-10-22T05:00:00Z");
        let e = Event::new("t", kind, key, now, subject).payload(payload);
        let c = Candidate {
            id: Uuid::new_v4(),
            event_id: e.id,
            kind,
            scores: Scores::default(),
            score,
            decision: Decision::Digest,
            reason: String::new(),
            priority: Priority::Low,
            created_at: now,
            decided_at: now,
            hold_until: None,
            expires_at: None,
            pulse_id: None,
        };
        (c, e)
    }

    #[test]
    fn empty_day_is_silent_and_repeats_are_not_new() {
        let cfg = Config::default();
        let now = at("2026-10-22T06:00:00Z"); // 08:00 Copenhagen
        let none = build(
            &cfg,
            now,
            &PulseInput {
                digests: &[],
                today_events: &[],
                reminders: &[],
            },
        );
        assert!(none.is_empty());

        let reminders = vec![
            reminder("r1", "2026-10-22T16:00:00.000Z", "Chiama il commercialista"),
            reminder(
                "r2",
                "2026-10-24T08:00:00.000Z",
                "Scadenza IVA: invia la dichiarazione",
            ),
            reminder("r3", "2026-11-30T08:00:00.000Z", "Scadenza lontana"),
            reminder("r4", "2026-10-23T08:00:00.000Z", "Compra il latte"),
        ];
        let digests = vec![
            digest(
                Kind::ThreadOpen,
                "thread_open:01AB:2026-10-22",
                "x",
                0.47,
                json!({"summary": "Il rimborso non è ancora arrivato."}),
            ),
            digest(
                Kind::ServiceRecovered,
                "service_recovered:pokoin.com:1",
                "pokoin.com back up",
                0.46,
                json!({}),
            ),
        ];
        let d = build(
            &cfg,
            now,
            &PulseInput {
                digests: &digests,
                today_events: &[],
                reminders: &reminders,
            },
        );
        assert_eq!(d.important.len(), 1);
        assert_eq!(d.important[0].line, "18:00 Chiama il commercialista");
        assert_eq!(d.completed[0].line, "pokoin.com back up");
        let watch: Vec<&str> = d.watch.iter().map(|i| i.line.as_str()).collect();
        assert_eq!(
            watch,
            vec![
                "Il rimborso non è ancora arrivato.",
                "Scadenza IVA: invia la dichiarazione (24/10)"
            ]
        );
        assert_eq!(d.new_items(&[]), 4);
        // Tomorrow the same open thread (new per-day dedup key) is not new.
        assert_eq!(d.new_items(&d.keys()), 0);
        assert_eq!(candidate_key(&digests[0].1), "thread:thread_open:01AB");
    }
}
