//! On-disk state of the notifier, all in the data dir and all 0600:
//!
//! - `notifications.json`: the inbox, oldest first, at most [`MAX_ITEMS`];
//! - `notification-settings.json`: [`Settings`];
//! - `notify-state.json`: the last timeline event id and the phone's
//!   upcoming calendar events.
//!
//! Only the server process writes these files, so each lives in memory
//! behind a mutex and is written through atomically (temp file, fsync,
//! rename) on every change. A file that cannot be parsed is moved aside
//! (`<name>.corrupt-<ms>`) and replaced by an empty one, with a warning.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::model::{now_ms, Category, Notification};
use super::settings::Settings;
use crate::devices::save_json_atomic;

pub const INBOX_FILE: &str = "notifications.json";
pub const SETTINGS_FILE: &str = "notification-settings.json";
pub const STATE_FILE: &str = "notify-state.json";
/// Inbox size; the oldest read items go first, then the oldest unread.
pub const MAX_ITEMS: usize = 500;
pub const MAX_PAGE: usize = 200;
pub const DEFAULT_PAGE: usize = 50;

/// Load `dir/name` as JSON; missing → default; unparsable → moved aside.
pub fn load_or_default<T: DeserializeOwned + Default>(dir: &Path, name: &str) -> T {
    let path = dir.join(name);
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                let aside = dir.join(format!("{name}.corrupt-{}", now_ms()));
                tracing::warn!(file = name, error = %e, "unreadable notification file; moving it aside");
                let _ = std::fs::rename(&path, aside);
                T::default()
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => T::default(),
        Err(e) => {
            tracing::warn!(file = name, error = %e, "cannot read notification file");
            T::default()
        }
    }
}

/// A JSON document kept in memory and written through on change.
pub struct JsonFile<T> {
    dir: PathBuf,
    name: &'static str,
    value: Mutex<T>,
}

impl<T: Serialize + DeserializeOwned + Default + Clone> JsonFile<T> {
    pub fn open(dir: &Path, name: &'static str) -> Self {
        Self {
            dir: dir.to_owned(),
            name,
            value: Mutex::new(load_or_default(dir, name)),
        }
    }

    fn lock(&self) -> MutexGuard<'_, T> {
        self.value.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn get(&self) -> T {
        self.lock().clone()
    }

    /// Read without cloning.
    pub fn read<R>(&self, f: impl FnOnce(&T) -> R) -> R {
        f(&self.lock())
    }

    /// Change and persist. The in-memory value changes only if the write
    /// succeeds.
    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> R) -> io::Result<R> {
        let mut guard = self.lock();
        let mut next = guard.clone();
        let r = f(&mut next);
        save_json_atomic(&self.dir, self.name, &next)?;
        *guard = next;
        Ok(r)
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(self.name)
    }
}

/// The phone's next events (from `POST /v1/device/state`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEvent {
    pub id: String,
    pub title: String,
    /// Epoch ms.
    pub start: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyState {
    /// Last timeline item the ingester handled (resume point).
    #[serde(default)]
    pub last_event_id: Option<String>,
    /// Device id → upcoming timed events.
    #[serde(default)]
    pub calendar: BTreeMap<String, Vec<CalendarEvent>>,
}

/// One page of the inbox.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub items: Vec<Notification>,
    pub unread: usize,
    pub next_before: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResult {
    pub updated: usize,
    pub unread: usize,
}

/// The inbox (`notifications.json`).
pub struct Inbox {
    file: JsonFile<Vec<Notification>>,
}

impl Inbox {
    pub fn open(dir: &Path) -> Self {
        Self {
            file: JsonFile::open(dir, INBOX_FILE),
        }
    }

    pub fn add(&self, n: Notification) -> io::Result<()> {
        self.file.update(|items| {
            items.push(n);
            items.sort_by(|a, b| a.id.cmp(&b.id));
            while items.len() > MAX_ITEMS {
                let victim = items.iter().position(|i| i.read).unwrap_or(0);
                items.remove(victim);
            }
        })
    }

    /// Drop every notification whose dedup key is `key`; returns how many.
    pub fn remove_key(&self, key: &str) -> io::Result<usize> {
        self.file.update(|items| {
            let before = items.len();
            items.retain(|n| n.dedup_key.as_deref() != Some(key));
            before - items.len()
        })
    }

    /// Whether any notification carries one of `keys` (dedup key or `tl:<id>`).
    pub fn has_any_key(&self, keys: &[String]) -> bool {
        if keys.is_empty() {
            return false;
        }
        self.file.read(|items| {
            items.iter().any(|n| {
                n.dedup_key.as_ref().is_some_and(|k| keys.contains(k))
                    || n.timeline_id
                        .as_ref()
                        .is_some_and(|t| keys.iter().any(|k| k.strip_prefix("tl:") == Some(t)))
            })
        })
    }

    pub fn unread(&self) -> usize {
        self.file
            .read(|items| items.iter().filter(|n| !n.read).count())
    }

    /// Unread notifications of `category` created at or after `since_ms`.
    pub fn unread_since(&self, category: Category, since_ms: i64) -> usize {
        self.file.read(|items| {
            items
                .iter()
                .filter(|n| !n.read && n.category == category && n.ts >= since_ms)
                .count()
        })
    }

    /// Newest first; `before` is an id cursor (only older items).
    pub fn page(&self, before: Option<&str>, limit: usize, unread_only: bool) -> Page {
        let limit = limit.clamp(1, MAX_PAGE);
        self.file.read(|items| {
            let mut out: Vec<Notification> = items
                .iter()
                .rev()
                .filter(|n| before.is_none_or(|b| n.id.as_str() < b))
                .filter(|n| !unread_only || !n.read)
                .take(limit + 1)
                .cloned()
                .collect();
            let more = out.len() > limit;
            out.truncate(limit);
            Page {
                next_before: if more {
                    out.last().map(|n| n.id.clone())
                } else {
                    None
                },
                unread: items.iter().filter(|n| !n.read).count(),
                items: out,
            }
        })
    }

    /// Mark `ids` (or everything with `all`) read.
    pub fn mark_read(&self, ids: &[String], all: bool, now: i64) -> io::Result<ReadResult> {
        self.file.update(|items| {
            let mut updated = 0;
            for n in items.iter_mut() {
                if !n.read && (all || ids.contains(&n.id)) {
                    n.read = true;
                    n.read_at = Some(now);
                    n.deferred = false;
                    updated += 1;
                }
            }
            ReadResult {
                updated,
                unread: items.iter().filter(|n| !n.read).count(),
            }
        })
    }

    /// The unread notifications held back for the digest; clears the flag.
    pub fn take_deferred(&self) -> io::Result<Vec<Notification>> {
        if !self.file.read(|items| items.iter().any(|n| n.deferred)) {
            return Ok(Vec::new());
        }
        self.file.update(|items| {
            let mut out = Vec::new();
            for n in items.iter_mut().filter(|n| n.deferred) {
                n.deferred = false;
                if !n.read {
                    out.push(n.clone());
                }
            }
            out
        })
    }

    pub fn path(&self) -> PathBuf {
        self.file.path()
    }
}

/// Settings (`notification-settings.json`).
pub type SettingsFile = JsonFile<Settings>;
/// Ingest and calendar state (`notify-state.json`).
pub type StateFile = JsonFile<NotifyState>;

#[cfg(test)]
mod tests {
    use super::*;

    fn n(id: &str, cat: Category, read: bool) -> Notification {
        Notification {
            id: id.into(),
            ts: 1,
            category: cat,
            title: None,
            body: "b".into(),
            route: "/".into(),
            channel: None,
            timeline_id: Some(format!("T{id}")),
            dedup_key: Some(format!("k:{id}")),
            data: None,
            read,
            read_at: None,
            deferred: false,
        }
    }

    #[test]
    fn page_read_cap_and_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path());
        for i in 0..5 {
            inbox
                .add(n(&format!("n_{i:02}"), Category::LucinaMessage, false))
                .unwrap();
        }
        let p = inbox.page(None, 2, false);
        assert_eq!(
            p.items.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["n_04", "n_03"]
        );
        assert_eq!(p.next_before.as_deref(), Some("n_03"));
        assert_eq!(p.unread, 5);
        let p2 = inbox.page(p.next_before.as_deref(), 10, false);
        assert_eq!(p2.items.len(), 3);
        assert!(p2.next_before.is_none());
        let r = inbox
            .mark_read(&["n_01".into(), "zz".into()], false, 9)
            .unwrap();
        assert_eq!(
            r,
            ReadResult {
                updated: 1,
                unread: 4
            }
        );
        assert_eq!(inbox.page(None, 50, true).items.len(), 4);
        assert!(inbox.has_any_key(&["k:n_02".into()]));
        assert!(inbox.has_any_key(&["tl:Tn_03".into()]));
        assert!(!inbox.has_any_key(&["tl:nope".into()]));

        // Reopen from disk: same content, file is 0600.
        let again = Inbox::open(dir.path());
        assert_eq!(again.unread(), 4);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(again.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        let r = again.mark_read(&[], true, 10).unwrap();
        assert_eq!(
            r,
            ReadResult {
                updated: 4,
                unread: 0
            }
        );
    }

    #[test]
    fn cap_drops_read_first_and_deferred_digest() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = Inbox::open(dir.path());
        inbox.add(n("n_000", Category::Reminder, false)).unwrap();
        inbox.add(n("n_001", Category::Reminder, true)).unwrap();
        for i in 2..=MAX_ITEMS {
            let mut x = n(&format!("n_{i:03}"), Category::LucinaMessage, false);
            x.deferred = i % 100 == 0;
            inbox.add(x).unwrap();
        }
        let all = inbox.page(None, MAX_PAGE, false);
        assert_eq!(all.unread, MAX_ITEMS);
        assert!(inbox.has_any_key(&["k:n_000".into()]), "unread kept");
        assert!(
            !inbox.has_any_key(&["k:n_001".into()]),
            "read dropped first"
        );
        let d = inbox.take_deferred().unwrap();
        assert_eq!(d.len(), 5);
        assert!(inbox.take_deferred().unwrap().is_empty());
        assert_eq!(inbox.unread_since(Category::Reminder, 0), 1);
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(INBOX_FILE), b"{not json").unwrap();
        let inbox = Inbox::open(dir.path());
        assert_eq!(inbox.unread(), 0);
        let aside = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains(".corrupt-"));
        assert!(aside);
    }
}
