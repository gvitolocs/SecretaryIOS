//! Reminders store, compatible with Node's `data/reminders.json`
//! (`{"reminders": [...], "updatedAt": "..."}`, pretty-printed, 2 spaces).
//!
//! Field order follows Node's `createReminder` (and `cancelledAt` appended
//! when a reminder is cancelled), so a read/write round trip of a
//! Node-written file is byte-identical. Unknown fields are kept.

use crate::scheduler::DeliveryOutcome;
use fs4::fs_std::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::atomic::write_atomic;
use crate::timeutil::{iso_ms, now_iso, parse_js_date, ymd_in_tz};
use crate::{Result, StoreError};

/// A scheduler tick claimed the reminder and is sending it (P2b).
pub const STATUS_SENDING: &str = "sending";
/// A claim found after a restart: maybe delivered, never resent.
pub const STATUS_FAILED: &str = "failed";
/// `lastError` of a reminder whose tick died mid-send.
pub const INTERRUPTED: &str = "interrupted; not resent";

/// The next status of a reminder at the start of a tick (pure; the P2b
/// rule, Verus `reminder_claims.rs`): a claim nobody is sending any more
/// becomes `failed`, never `pending` again, so it cannot be sent twice.
/// Ticks run one at a time under the store lock, so at the start of a tick
/// every `sending` claim belongs to a tick that died.
pub fn status_at_tick_start(status: &str) -> &str {
    if status == STATUS_SENDING {
        STATUS_FAILED
    } else {
        status
    }
}

/// Apply [`status_at_tick_start`]; returns how many claims failed.
fn fail_stale_claims(reminders: &mut [Reminder]) -> usize {
    let mut n = 0;
    for r in reminders.iter_mut() {
        if status_at_tick_start(&r.status) != r.status {
            r.status = STATUS_FAILED.into();
            r.last_error = Some(INTERRUPTED.into());
            r.extra.remove("claimedAt");
            n += 1;
        }
    }
    if n > 0 {
        tracing::warn!(
            count = n,
            "reminders claimed by an interrupted tick: marked failed, not resent"
        );
    }
    n
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoreFile {
    #[serde(default)]
    reminders: Vec<Reminder>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
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

/// Reminder writes are serialized by the in-process mutex and a cross-process flock.
/// Node writers must be stopped or forwarded before local ownership is enabled.
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

    fn file_lock(&self) -> Result<std::fs::File> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut path = self.path.as_os_str().to_os_string();
        path.push(".lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(PathBuf::from(path))?;
        file.lock_exclusive()?;
        Ok(file)
    }

    fn read(&self) -> Result<StoreFile> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StoreFile {
                reminders: Vec::new(),
                updated_at: None,
                extra: Map::new(),
            }),
            Err(e) => Err(e.into()),
        }
    }

    fn write(&self, mut file: StoreFile) -> Result<()> {
        file.updated_at = Some(now_iso());
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
        let _file_guard = self.file_lock()?;
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
            .filter(|r| {
                // A claim in flight or a visible failure is kept too (P2b).
                r.status == "pending"
                    || r.status == STATUS_SENDING
                    || r.status == STATUS_FAILED
                    || r.sent_at.is_some()
                    || r.cancelled_at.is_some()
            })
            .collect();
        let skip = kept.len().saturating_sub(self.max_stored);
        store.reminders = kept.into_iter().skip(skip).collect();
        self.write(store)?;
        Ok((true, reminder))
    }

    /// Cancel a pending reminder, optionally scoped to one chat.
    pub fn cancel(&self, id: &str, chat_id: Option<&str>) -> Result<CancelOutcome> {
        if id.is_empty() {
            return Ok(CancelOutcome::NotFound);
        }
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _file_guard = self.file_lock()?;
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
        self.write(store)?;
        Ok(CancelOutcome::Cancelled(out))
    }

    /// Deliver every pending reminder due at `now` through `send`, at most
    /// once (docs/verification.md, P2b: a restart in the middle of a tick
    /// used to send the whole batch again):
    ///
    /// 1. a `sending` claim left by a tick that never finished (the process
    ///    died mid-send) becomes `failed` with `lastError` "interrupted; not
    ///    resent": it may or may not have reached the owner, and it shows in
    ///    the reminder list instead of being sent twice;
    /// 2. the due reminders are claimed (`sending`) and the claim is written
    ///    **before** the first send;
    /// 3. each one is sent; `sent`, or back to `pending` with `lastError`
    ///    when the sink says it was not delivered (retried next tick). An uncertain
    ///    delivery becomes `failed` and is never resent, to avoid duplicates.
    ///
    /// Returns the due reminders with their outcome.
    pub fn process_due<F>(&self, now: DateTime<Utc>, mut send: F) -> Result<Vec<Reminder>>
    where
        F: FnMut(&Reminder) -> DeliveryOutcome,
    {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _file_guard = self.file_lock()?;
        let mut store = self.read()?;
        let recovered = fail_stale_claims(&mut store.reminders);
        let claimed: Vec<usize> = store
            .reminders
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                r.status == "pending"
                    && (r.persona.is_empty() || r.persona == "Lucina")
                    && r.due().is_some_and(|d| d <= now)
            })
            .map(|(i, _)| i)
            .collect();
        if claimed.is_empty() {
            if recovered > 0 {
                self.write(store)?;
            }
            return Ok(Vec::new());
        }
        let claimed_at = now_iso();
        for &i in &claimed {
            let r = &mut store.reminders[i];
            r.status = STATUS_SENDING.into();
            r.extra
                .insert("claimedAt".into(), Value::String(claimed_at.clone()));
        }
        // The claim is on disk before anything is sent.
        self.write(store.clone())?;
        let mut due = Vec::new();
        for &i in &claimed {
            let reminder = &mut store.reminders[i];
            let outcome = send(reminder);
            reminder.extra.remove("claimedAt");
            match outcome {
                DeliveryOutcome::Delivered => {
                    reminder.status = "sent".into();
                    reminder.sent_at = Some(now_iso());
                    reminder.last_error = None;
                }
                DeliveryOutcome::NotDelivered(e) => {
                    reminder.status = "pending".into();
                    reminder.last_error = Some(e.chars().take(500).collect());
                }
                DeliveryOutcome::Uncertain(e) => {
                    reminder.status = STATUS_FAILED.into();
                    reminder.last_error = Some(format!(
                        "delivery uncertain; not resent: {}",
                        e.chars().take(500).collect::<String>()
                    ));
                }
            }
            due.push(reminder.clone());
        }
        self.write(store)?;
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
mod claim_tests {
    use super::*;

    fn due_input(text: &str) -> CreateReminder {
        CreateReminder {
            text: text.into(),
            due_at: "2020-01-01T00:00:00.000Z".into(),
            chat_id: "42".into(),
            user_peer_id: "giuseppe".into(),
            user_label: String::new(),
            persona: "Lucina".into(),
            time_zone: "Europe/Rome".into(),
            dedupe_key: None,
            source: "secretary".into(),
        }
    }

    /// P2b (docs/verification.md): the scheduler sent the whole batch and
    /// wrote `sent` once at the end, so a restart mid-tick sent it again.
    /// Now: claim, then send; a claim found after the restart fails visibly.
    #[test]
    fn a_restart_mid_tick_never_resends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reminders.json");
        let store = ReminderStore::new(&path, 100);
        store.create(due_input("first")).unwrap();
        store.create(due_input("second")).unwrap();
        let sends = std::sync::atomic::AtomicUsize::new(0);
        // The process dies while sending the first one (after it went out).
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store.process_due(Utc::now(), |_| {
                // The claim reached the disk before this send.
                let on_disk = ReminderStore::new(&path, 100).list(true).unwrap();
                assert!(on_disk.iter().all(|r| r.status == STATUS_SENDING));
                sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                panic!("SIGKILL mid-send");
            })
        }));
        assert!(crashed.is_err());
        // The restarted scheduler's first tick.
        let restarted = ReminderStore::new(&path, 100);
        let due = restarted
            .process_due(Utc::now(), |_| {
                sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                DeliveryOutcome::Delivered
            })
            .unwrap();
        assert!(due.is_empty(), "nothing is due again");
        assert_eq!(sends.load(std::sync::atomic::Ordering::SeqCst), 1);
        let all = restarted.list(true).unwrap();
        assert!(all
            .iter()
            .all(|r| r.status == STATUS_FAILED && r.last_error.as_deref() == Some(INTERRUPTED)));
        assert!(all.iter().all(|r| !r.extra.contains_key("claimedAt")));
        // Still listed after the next create (kept, visible).
        restarted.create(due_input("third")).unwrap();
        assert_eq!(restarted.list(true).unwrap().len(), 3);
    }

    #[test]
    fn a_failed_send_stays_pending_and_is_retried() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReminderStore::new(dir.path().join("r.json"), 100);
        store.create(due_input("x")).unwrap();
        let due = store
            .process_due(Utc::now(), |_| {
                DeliveryOutcome::NotDelivered("telegram down".into())
            })
            .unwrap();
        assert_eq!(due[0].status, "pending");
        assert_eq!(due[0].last_error.as_deref(), Some("telegram down"));
        let due = store
            .process_due(Utc::now(), |_| DeliveryOutcome::Delivered)
            .unwrap();
        assert_eq!(due[0].status, "sent");
        assert!(store
            .process_due(Utc::now(), |_| DeliveryOutcome::Delivered)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn uncertain_delivery_is_failed_and_never_retried() {
        let dir = tempfile::tempdir().unwrap();
        let store = ReminderStore::new(dir.path().join("r.json"), 100);
        store.create(due_input("x")).unwrap();
        let due = store
            .process_due(Utc::now(), |_| {
                DeliveryOutcome::Uncertain("read timeout".into())
            })
            .unwrap();
        assert_eq!(due[0].status, "failed");
        assert_eq!(
            due[0].last_error.as_deref(),
            Some("delivery uncertain; not resent: read timeout")
        );
        assert!(due[0].sent_at.is_none());
        assert!(store
            .process_due(Utc::now(), |_| panic!("must not resend"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn separate_stores_serialize_writers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.json");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        std::thread::scope(|scope| {
            for thread in 0..2 {
                let path = path.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    let store = ReminderStore::new(path, 1000);
                    barrier.wait();
                    for n in 0..50 {
                        store.create(due_input(&format!("{thread}-{n}"))).unwrap();
                    }
                });
            }
        });
        assert_eq!(
            ReminderStore::new(path, 1000).list(true).unwrap().len(),
            100
        );
    }

    #[test]
    fn separate_schedulers_do_not_interleave_claims() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.json");
        ReminderStore::new(&path, 100)
            .create(due_input("x"))
            .unwrap();
        let sends = std::sync::atomic::AtomicUsize::new(0);
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let path = &path;
                let sends = &sends;
                let barrier = &barrier;
                scope.spawn(move || {
                    let store = ReminderStore::new(path, 100);
                    barrier.wait();
                    store
                        .process_due(Utc::now(), |_| {
                            sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(10));
                            DeliveryOutcome::Delivered
                        })
                        .unwrap();
                });
            }
        });
        assert_eq!(sends.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            ReminderStore::new(&path, 100).list(true).unwrap()[0].status,
            "sent"
        );
    }

    #[test]
    fn tick_start_rule() {
        assert_eq!(status_at_tick_start(STATUS_SENDING), STATUS_FAILED);
        for s in ["pending", "sent", "cancelled", STATUS_FAILED] {
            assert_eq!(status_at_tick_start(s), s);
        }
    }
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
        let due = s
            .process_due(Utc::now(), |_| DeliveryOutcome::Delivered)
            .unwrap();
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
