//! The tracked list, persisted as JSON at a path the caller gives (temp file + rename, inside a
//! mutex). A missing or unreadable file is an empty list.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::policy::Status;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParcelState {
    Active,
    Delivered,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Parcel {
    pub id: String,
    pub number: String,
    pub carrier: Option<String>,
    pub label: Option<String>,
    pub added_at: DateTime<Utc>,
    pub state: ParcelState,
    pub status: Option<Status>,
    pub last_checked: Option<DateTime<Utc>>,
    pub last_change: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub final_sent: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    parcels: Vec<Parcel>,
    /// The local date (`YYYY-MM-DD`) of the last daily digest.
    #[serde(default)]
    digest_sent: Option<String>,
}

pub struct AddResult {
    pub parcel: Parcel,
    /// `false`: the number was already tracked.
    pub created: bool,
}

struct Inner {
    file: File,
    /// The file's modified time when `file` was read or written.
    seen: Option<SystemTime>,
}

pub struct Store {
    path: PathBuf,
    inner: Mutex<Inner>,
}

/// Trimmed, spaces removed, upper-cased.
pub fn normalize_number(s: &str) -> String {
    s.split_whitespace().collect::<String>().to_uppercase()
}

/// `pc-` and eight lowercase hex digits.
pub fn valid_id(id: &str) -> bool {
    id.strip_prefix("pc-")
        .is_some_and(|h| h.len() == 8 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

fn new_id() -> String {
    let mut b = [0u8; 4];
    getrandom::fill(&mut b).expect("the OS has randomness");
    format!(
        "pc-{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

fn read(path: &Path) -> File {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
            tracing::warn!(path = %path.display(), error = %e, "parcels: the list is unreadable, starting empty");
            File::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => File::default(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "parcels: the list can't be read, starting empty");
            File::default()
        }
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl Store {
    pub fn open(path: impl Into<PathBuf>) -> Store {
        let path = path.into();
        let inner = Inner {
            file: read(&path),
            seen: mtime(&path),
        };
        Store {
            path,
            inner: Mutex::new(inner),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The list, re-read first when another process (the CLI) changed the file.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let now = mtime(&self.path);
        if now != g.seen {
            g.file = read(&self.path);
            g.seen = now;
        }
        g
    }

    fn save(&self, g: &mut Inner) {
        let write = || -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension("json.tmp");
            std::fs::write(
                &tmp,
                serde_json::to_vec_pretty(&g.file).map_err(std::io::Error::other)?,
            )?;
            std::fs::rename(&tmp, &self.path)
        };
        if let Err(e) = write() {
            tracing::warn!(path = %self.path.display(), error = %e, "parcels: the list was not saved");
        }
        g.seen = mtime(&self.path);
    }

    pub fn list(&self) -> Vec<Parcel> {
        self.lock().file.parcels.clone()
    }

    /// By id, number or label (case-insensitive).
    pub fn get(&self, key: &str) -> Option<Parcel> {
        find(&self.lock().file.parcels, key).cloned()
    }

    /// Adds a parcel; a number that is already tracked returns the existing one (a stopped one
    /// is reactivated).
    pub fn add(
        &self,
        number: &str,
        carrier: Option<&str>,
        label: Option<&str>,
        now: DateTime<Utc>,
    ) -> AddResult {
        let number = normalize_number(number);
        let clean = |s: Option<&str>| {
            s.map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let (carrier, label) = (clean(carrier), clean(label));
        let mut g = self.lock();
        if let Some(p) = g.file.parcels.iter_mut().find(|p| p.number == number) {
            if p.state == ParcelState::Stopped {
                p.state = ParcelState::Active;
                p.added_at = now;
                p.last_change = None;
            }
            p.carrier = p.carrier.take().or(carrier);
            p.label = p.label.take().or(label);
            let parcel = p.clone();
            self.save(&mut g);
            return AddResult {
                parcel,
                created: false,
            };
        }
        let parcel = Parcel {
            id: new_id(),
            number,
            carrier,
            label,
            added_at: now,
            state: ParcelState::Active,
            status: None,
            last_checked: None,
            last_change: None,
            last_error: None,
            final_sent: false,
        };
        g.file.parcels.push(parcel.clone());
        self.save(&mut g);
        AddResult {
            parcel,
            created: true,
        }
    }

    /// Changes one parcel under the lock; `None`: no such id.
    pub fn update(&self, id: &str, change: impl FnOnce(&mut Parcel)) -> Option<Parcel> {
        let mut g = self.lock();
        let p = g.file.parcels.iter_mut().find(|p| p.id == id)?;
        change(p);
        let parcel = p.clone();
        self.save(&mut g);
        Some(parcel)
    }

    /// By id only (the apps' key).
    pub fn get_id(&self, id: &str) -> Option<Parcel> {
        self.lock().file.parcels.iter().find(|p| p.id == id).cloned()
    }

    pub fn remove(&self, key: &str) -> Option<Parcel> {
        let id = find(&self.lock().file.parcels, key)?.id.clone();
        self.remove_id(&id)
    }

    /// By id only (the apps' key).
    pub fn remove_id(&self, id: &str) -> Option<Parcel> {
        let mut g = self.lock();
        let i = g.file.parcels.iter().position(|p| p.id == id)?;
        let gone = g.file.parcels.remove(i);
        self.save(&mut g);
        Some(gone)
    }

    pub fn digest_sent(&self) -> Option<String> {
        self.lock().file.digest_sent.clone()
    }

    pub fn set_digest_sent(&self, date: &str) {
        let mut g = self.lock();
        g.file.digest_sent = Some(date.to_owned());
        self.save(&mut g);
    }
}

fn find<'a>(parcels: &'a [Parcel], key: &str) -> Option<&'a Parcel> {
    let key = key.trim();
    let number = normalize_number(key);
    parcels.iter().find(|p| {
        p.id.eq_ignore_ascii_case(key)
            || p.number == number
            || p.label
                .as_deref()
                .is_some_and(|l| l.eq_ignore_ascii_case(key))
    })
}
