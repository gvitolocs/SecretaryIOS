//! Reminders store, compatible with Node's `data/reminders.json`
//! (`{"reminders": [...], "updatedAt": "..."}`, pretty-printed, 2 spaces).
//!
//! Field order follows Node's `createReminder` (and `cancelledAt` appended
//! when a reminder is cancelled), so a read/write round trip of a
//! Node-written file is byte-identical. Unknown fields are kept.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::atomic::write_atomic;
use crate::timeutil::{iso_ms, now_iso, parse_js_date, ymd_in_tz};
use crate::{Result, StoreError};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    pub id: String,
    #[serde(default)]
    pub dedupe_key: String,
    pub text: String,
    pub due_at: String,
    #[serde(default)]
    pub time_zone: String,
    #[serde(default)]
    pub chat_id: String,
    #[serde(default)]
    pub user_peer_id: String,
    #[serde(default)]
    pub user_label: String,
    #[serde(default)]
    pub persona: String,
    #[serde(default)]
    pub source: Value,
    pub status: String,
    #[serde(default)]
    pub created_at: Value,
    #[serde(default)]
    pub sent_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Reminder {
    pub fn due(&self) -> Option<DateTime<Utc>> {
        parse_js_date(&self.due_at)
    }

    /// The public object of `docs/secretary-api.md`.
    pub fn public(&self) -> Value {
        fn or_null(v: Option<&str>) -> Value {
            match v {
                Some(s) if !s.is_empty() => Value::String(s.to_owned()),
                _ => Value::Null,
            }
        }
        serde_json::json!({
            "id": self.id,
            "text": self.text,
            "dueAt": self.due_at,
            "timeZone": self.time_zone,
            "status": self.status,
            "source": or_null(self.source.as_str()),
            "createdAt": or_null(self.created_at.as_str()),
            "sentAt": or_null(self.sent_at.as_deref()),
            "cancelledAt": or_null(self.cancelled_at.as_deref()),
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreFile {
    #[serde(default)]
    reminders: Vec<Reminder>,
    #[serde(default)]
    updated_at: Option<String>,
}

/// Input of [`ReminderStore::create`].
#[derive(Debug, Clone)]
pub struct CreateReminder {
    pub text: String,
    /// ISO 8601; part of the dedupe key exactly as given (Node).
    pub due_at: String,
    pub chat_id: String,
    pub user_peer_id: String,
    pub user_label: String,
    pub persona: String,
    pub time_zone: String,
    pub dedupe_key: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CancelOutcome {
    Cancelled(Reminder),
    AlreadyCancelled(Reminder),
    AlreadySent(Reminder),
    NotFound,
}

/// The reminders store. One process-wide lock serializes writes; Node
/// does not lock across processes, so only one Lucina may write a file.
#[derive(Debug)]
pub struct ReminderStore {
    path: PathBuf,
    max_stored: usize,
    lock: Mutex<()>,
}

impl ReminderStore {
    pub fn new(path: impl Into<PathBuf>, max_stored: usize) -> Self {
        Self {
            path: path.into(),
            max_stored: max_stored.max(1),
            lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<StoreFile> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoreFile {
                reminders: Vec::new(),
                updated_at: None,
            }),
            Err(e) => Err(e.into()),
        }
    }

    fn write(&self, reminders: Vec<Reminder>) -> Result<()> {
        let file = StoreFile {
            reminders,
            updated_at: Some(now_iso()),
        };
        let mut text = serde_json::to_string_pretty(&file)?;
        text.push('\n');
        write_atomic(&self.path, text.as_bytes())?;
        Ok(())
    }

    /// Every reminder (`include_sent`) or only pending ones.
    pub fn list(&self, include_sent: bool) -> Result<Vec<Reminder>> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self
            .read()?
            .reminders
            .into_iter()
            .filter(|r| include_sent || r.status == "pending")
            .collect())
    }

    /// Create (or return the identical pending one): `(created, reminder)`.
    pub fn create(&self, input: CreateReminder) -> Result<(bool, Reminder)> {
        let due = parse_js_date(&input.due_at)
            .ok_or_else(|| StoreError::Invalid("Invalid reminder dueAt".into()))?;
        let text = input.text.trim().to_owned();
        if text.is_empty() {
            return Err(StoreError::Invalid("Reminder text is required".into()));
        }
        if input.chat_id.is_empty() {
            return Err(StoreError::Invalid("Reminder chatId is required".into()));
        }
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut store = self.read()?;
        let key = input
            .dedupe_key
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| {
                format!("{}:{}:{}", input.chat_id, input.due_at, text.to_lowercase())
            });
        if let Some(existing) = store
            .reminders
            .iter()
            .find(|r| r.dedupe_key == key && r.status == "pending")
        {
            return Ok((false, existing.clone()));
        }
        let reminder = Reminder {
            id: uuid::Uuid::new_v4().to_string(),
            dedupe_key: key,
            text,
            due_at: iso_ms(due),
            time_zone: if input.time_zone.is_empty() {
                "UTC".into()
            } else {
                input.time_zone
            },
            chat_id: input.chat_id,
            user_peer_id: input.user_peer_id,
            user_label: input.user_label,
            persona: if input.persona.is_empty() {
                "Lucina".into()
            } else {
                input.persona
            },
            source: Value::String(input.source),
            status: "pending".into(),
            created_at: Value::String(now_iso()),
            sent_at: None,
            last_error: None,
            cancelled_at: None,
            extra: Map::new(),
        };
        store.reminders.push(reminder.clone());
        let kept: Vec<Reminder> = store
            .reminders
            .into_iter()
            .filter(|r| r.status == "pending" || r.sent_at.is_some() || r.cancelled_at.is_some())
            .collect();
        let skip = kept.len().saturating_sub(self.max_stored);
        self.write(kept.into_iter().skip(skip).collect())?;
        Ok((true, reminder))
    }

    /// Cancel a pending reminder, optionally scoped to one chat.
    pub fn cancel(&self, id: &str, chat_id: Option<&str>) -> Result<CancelOutcome> {
        if id.is_empty() {
            return Ok(CancelOutcome::NotFound);
        }
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut store = self.read()?;
        let Some(reminder) = store
            .reminders
            .iter_mut()
            .find(|r| r.id == id && chat_id.is_none_or(|c| r.chat_id == c))
        else {
            return Ok(CancelOutcome::NotFound);
        };
        match reminder.status.as_str() {
            "sent" => return Ok(CancelOutcome::AlreadySent(reminder.clone())),
            "cancelled" => return Ok(CancelOutcome::AlreadyCancelled(reminder.clone())),
            _ => {}
        }
        reminder.status = "cancelled".into();
        reminder.cancelled_at = Some(now_iso());
        let out = reminder.clone();
        self.write(store.reminders)?;
        Ok(CancelOutcome::Cancelled(out))
    }

    /// Deliver every pending reminder due at `now` through `send`, marking
    /// it `sent` (or recording `lastError`). Returns the due reminders.
    pub fn process_due<F>(&self, now: DateTime<Utc>, mut send: F) -> Result<Vec<Reminder>>
    where
        F: FnMut(&Reminder) -> std::result::Result<(), String>,
    {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut store = self.read()?;
        let mut due = Vec::new();
        for reminder in store.reminders.iter_mut() {
            if reminder.status != "pending" || reminder.due().is_none_or(|d| d > now) {
                continue;
            }
            match send(reminder) {
                Ok(()) => {
                    reminder.status = "sent".into();
                    reminder.sent_at = Some(now_iso());
                    reminder.last_error = None;
                }
                Err(e) => reminder.last_error = Some(e.chars().take(500).collect()),
            }
            due.push(reminder.clone());
        }
        if !due.is_empty() {
            self.write(store.reminders)?;
        }
        Ok(due)
    }

    /// Raw round trip (tests and migrations): parse and re-serialize.
    pub fn reserialize(bytes: &[u8]) -> Result<String> {
        let file: StoreFile = serde_json::from_slice(bytes)?;
        let mut text = serde_json::to_string_pretty(&file)?;
        text.push('\n');
        Ok(text)
    }
}

/// Pending reminders, optionally only those due today in `tz`.
pub fn filter_for_scope(
    reminders: &[Reminder],
    today_only: bool,
    tz: Tz,
    now: DateTime<Utc>,
) -> Vec<Reminder> {
    let today = ymd_in_tz(now, tz);
    reminders
        .iter()
        .filter(|r| r.status == "pending")
        .filter(|r| !today_only || r.due().is_some_and(|d| ymd_in_tz(d, tz) == today))
        .cloned()
        .collect()
}

const IT_MONTHS: [&str; 12] = [
    "gen", "feb", "mar", "apr", "mag", "giu", "lug", "ago", "set", "ott", "nov", "dic",
];

/// `Intl.DateTimeFormat('it-IT', {dateStyle: 'medium', timeStyle: 'short'})`.
pub fn format_it_medium(dt: DateTime<Utc>, tz: Tz) -> String {
    use chrono::{Datelike, Timelike};
    let l = dt.with_timezone(&tz);
    format!(
        "{} {} {}, {:02}:{:02}",
        l.day(),
        IT_MONTHS[l.month0() as usize],
        l.year(),
        l.hour(),
        l.minute()
    )
}

/// "Reminder set for Tuesday 21 October 2026 at 18:30 (Europe/Rome): text"
pub fn format_confirmation(reminder: &Reminder, tz: Tz) -> String {
    let when = reminder
        .due()
        .map(|d| {
            d.with_timezone(&tz)
                // Intl en-GB {dateStyle: "full", timeStyle: "short"}.
                .format("%A, %-d %B %Y at %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| reminder.due_at.clone());
    format!("Reminder set for {when} ({}): {}", tz.name(), reminder.text)
}

/// Node's `formatReminderList` (Italian).
pub fn format_list(
    reminders: &[Reminder],
    tz: Tz,
    persona: &str,
    today_only: bool,
    now: DateTime<Utc>,
) -> String {
    let mut pending = filter_for_scope(reminders, today_only, tz, now);
    if pending.is_empty() {
        return if today_only {
            format!(
                "{persona}: non hai promemoria in programma per oggi ({}).",
                tz.name()
            )
        } else {
            format!("{persona}: non hai promemoria futuri in coda.")
        };
    }
    pending.sort_by_key(|r| r.due());
    let lines: Vec<String> = pending
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let when = r.due().map(|d| format_it_medium(d, tz)).unwrap_or_default();
            format!("{}. {when} — {}", i + 1, r.text)
        })
        .collect();
    format!(
        "{persona}: promemoria {} ({}, {}):\n{}",
        if today_only { "per oggi" } else { "futuri" },
        pending.len(),
        tz.name(),
        lines.join("\n")
    )
}

/// Node's `buildRecentDeliveredRemindersContext`: reminders delivered to
/// `chat_id` in the last 48 h, newest first, at most 6.
pub fn delivered_context(
    reminders: &[Reminder],
    chat_id: &str,
    tz: Tz,
    now: DateTime<Utc>,
) -> String {
    let cutoff = now - chrono::Duration::hours(48);
    let mut sent: Vec<(&Reminder, DateTime<Utc>)> = reminders
        .iter()
        .filter(|r| r.chat_id == chat_id && r.status == "sent")
        .filter_map(|r| r.sent_at.as_deref().and_then(parse_js_date).map(|d| (r, d)))
        .filter(|(_, d)| *d >= cutoff)
        .collect();
    if sent.is_empty() {
        return String::new();
    }
    sent.sort_by_key(|s| std::cmp::Reverse(s.1));
    let lines: Vec<String> = sent
        .iter()
        .take(6)
        .enumerate()
        .map(|(i, (r, d))| {
            format!(
                "{}. [delivered {}] {}",
                i + 1,
                format_it_medium(*d, tz),
                r.text.trim()
            )
        })
        .collect();
    format!(
        "Recently delivered reminders in this chat:\n{}",
        lines.join("\n")
    )
}

/// `⏰ Reminder\n\n<text>`
pub fn format_delivery(reminder: &Reminder) -> String {
    format!("⏰ Reminder\n\n{}", reminder.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn input(text: &str, due: &str) -> CreateReminder {
        CreateReminder {
            text: text.into(),
            due_at: due.into(),
            chat_id: "42".into(),
            user_peer_id: "giuseppe".into(),
            user_label: "Giuseppe".into(),
            persona: "Lucina".into(),
            time_zone: "Europe/Rome".into(),
            dedupe_key: None,
            source: "secretary".into(),
        }
    }

    #[test]
    fn create_dedupe_cancel_and_deliver() {
        let dir = tempfile::tempdir().unwrap();
        let s = ReminderStore::new(dir.path().join("reminders.json"), 500);
        let (created, r) = s.create(input("Call", "2030-01-01T09:00:00.000Z")).unwrap();
        assert!(created);
        let (again, same) = s
            .create(input(" call ", "2030-01-01T09:00:00.000Z"))
            .unwrap();
        assert!(!again);
        assert_eq!(same.id, r.id);
        assert_eq!(s.list(false).unwrap().len(), 1);
        match s.cancel(&r.id, Some("42")).unwrap() {
            CancelOutcome::Cancelled(c) => assert!(c.cancelled_at.is_some()),
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            s.cancel(&r.id, None).unwrap(),
            CancelOutcome::AlreadyCancelled(_)
        ));
        assert_eq!(s.cancel(&r.id, Some("7")).unwrap(), CancelOutcome::NotFound);
        let (_, r2) = s.create(input("Due", "2020-01-01T09:00:00.000Z")).unwrap();
        let due = s.process_due(Utc::now(), |_| Ok(())).unwrap();
        assert_eq!(due.len(), 1);
        assert!(matches!(
            s.cancel(&r2.id, None).unwrap(),
            CancelOutcome::AlreadySent(_)
        ));
        // file layout: pretty JSON, Node key order, cancelledAt last
        let text = std::fs::read_to_string(s.path()).unwrap();
        assert!(text.ends_with("}\n"));
        let first = text.find("\"id\"").unwrap();
        assert!(first < text.find("\"dedupeKey\"").unwrap());
        assert!(text.find("\"lastError\"").unwrap() < text.find("\"cancelledAt\"").unwrap());
        assert_eq!(ReminderStore::reserialize(text.as_bytes()).unwrap(), text);
    }

    #[test]
    fn formats() {
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let now = Utc.with_ymd_and_hms(2026, 10, 21, 10, 0, 0).unwrap();
        let mut r: Reminder = serde_json::from_value(serde_json::json!({
            "id": "a", "text": "chiamare la mamma", "dueAt": "2026-10-21T16:30:00.000Z",
            "timeZone": "Europe/Rome", "status": "pending"
        }))
        .unwrap();
        assert_eq!(
            format_confirmation(&r, tz),
            "Reminder set for Wednesday, 21 October 2026 at 18:30 (Europe/Rome): chiamare la mamma"
        );
        assert_eq!(
            format_list(std::slice::from_ref(&r), tz, "Lucina", true, now),
            "Lucina: promemoria per oggi (1, Europe/Rome):\n1. 21 ott 2026, 18:30 — chiamare la mamma"
        );
        r.status = "sent".into();
        assert_eq!(
            format_list(&[r], tz, "Lucina", false, now),
            "Lucina: non hai promemoria futuri in coda."
        );
    }
}
