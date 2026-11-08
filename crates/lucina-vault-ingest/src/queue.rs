//! The durable queue: `queue.jsonl`, an append-only journal of what happened
//! to each item, replayed into the state of every dedup key.
//!
//! - `enqueue` carries the whole item (redacted), so an item stays workable
//!   when its source moves on; a key that is already in the journal is never
//!   queued twice, whatever its state.
//! - `planned` keeps the model's answer before any note is written: a restart
//!   after it writes the same notes again instead of asking the model anew
//!   (whose new wording would make duplicate claims).
//! - `done` / `skipped` are final; `failed` counts attempts and sets the next
//!   try; after [`MAX_ATTEMPTS`] the item waits for a manual retry.
//!
//! Appends happen under `queue.lock` (short); a run holds `run.lock` for its
//! whole length, so the server and the CLI never ingest at the same time.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, FixedOffset, Utc};
use serde::{Deserialize, Serialize};

use crate::plan::Plan;
use crate::IngestError;

pub const QUEUE_FILE: &str = "queue.jsonl";
const QUEUE_LOCK: &str = "queue.lock";
const RUN_LOCK: &str = "run.lock";
/// Failed attempts before an item waits for `retry`.
pub const MAX_ATTEMPTS: u32 = 6;
/// Compact the journal past this size.
const COMPACT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Ops,
    Git,
    Session,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Ops => "ops",
            SourceKind::Git => "git",
            SourceKind::Session => "session",
        }
    }
}

/// How a note links back to where the item came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Backlink {
    Ops {
        id: String,
        /// `Log/YYYY-MM-DD.md`
        log: String,
        incident: Option<String>,
        commit: Option<String>,
    },
    Git {
        repo: String,
        path: String,
        branch: String,
        sha: String,
        merge: bool,
    },
    Session {
        id: String,
    },
}

/// One unit of work: everything the model and the note writer need.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// `ops:<id>`, `git:<repo>:<sha>`, `session:<hash>`.
    pub key: String,
    pub source: SourceKind,
    /// Project slug (`Projects/<project>.md`).
    pub project: String,
    /// The source's own title (record title, commit subject): names the event.
    pub title: String,
    pub at: DateTime<FixedOffset>,
    /// What the model reads (redacted, clipped).
    pub text: String,
    /// The only paths a note may name (absolute, existing).
    pub files: Vec<String>,
    pub backlink: Backlink,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Entry {
    Enqueue {
        item: Item,
        at: DateTime<Utc>,
    },
    Planned {
        key: String,
        plan: Plan,
        at: DateTime<Utc>,
    },
    Done {
        key: String,
        notes: Vec<String>,
        at: DateTime<Utc>,
    },
    Skipped {
        key: String,
        reason: String,
        at: DateTime<Utc>,
    },
    Failed {
        key: String,
        error: String,
        attempts: u32,
        next_try: DateTime<Utc>,
        at: DateTime<Utc>,
    },
    Retry {
        key: String,
        at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum State {
    Pending {
        attempts: u32,
        next_try: Option<DateTime<Utc>>,
        last_error: Option<String>,
    },
    /// Done (`notes` written).
    Done {
        notes: Vec<String>,
    },
    Skipped {
        reason: String,
    },
    /// Gave up after [`MAX_ATTEMPTS`]; `retry` re-arms it.
    Failed {
        attempts: u32,
        last_error: String,
    },
}

/// One key's replayed state.
#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    /// `None` after compaction of a finished item.
    pub item: Option<Item>,
    pub plan: Option<Plan>,
    pub state: State,
    pub enqueued_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Slot {
    /// Waiting for the model or the writer, and due at `now`.
    pub fn due(&self, now: DateTime<Utc>) -> bool {
        match &self.state {
            State::Pending { next_try, .. } => next_try.is_none_or(|t| t <= now),
            _ => false,
        }
    }

    pub fn pending(&self) -> bool {
        matches!(self.state, State::Pending { .. })
    }
}

/// Counts for the status.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Depth {
    /// Waiting (due or backing off).
    pub pending: usize,
    /// Of those, with a stored plan (only the writing is left).
    pub planned: usize,
    pub done: usize,
    pub skipped: usize,
    /// Gave up; need `retry`.
    pub failed: usize,
}

pub struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        // SAFETY: the descriptor is owned by the File for the Lock's life.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn open_lock(path: &Path) -> Result<File, IngestError> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| IngestError::Io(format!("{}: {e}", path.display())))
}

fn flock(f: &File, nonblocking: bool) -> std::io::Result<bool> {
    let op = libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 };
    // SAFETY: flock on a descriptor we own.
    if unsafe { libc::flock(f.as_raw_fd(), op) } == 0 {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    if nonblocking && e.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(e)
    }
}

#[derive(Debug)]
pub struct Queue {
    dir: PathBuf,
}

impl Queue {
    pub fn open(dir: &Path) -> Result<Self, IngestError> {
        fs::create_dir_all(dir).map_err(|e| IngestError::Io(format!("{}: {e}", dir.display())))?;
        Ok(Self {
            dir: dir.to_owned(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file(&self) -> PathBuf {
        self.dir.join(QUEUE_FILE)
    }

    fn lock(&self) -> Result<Lock, IngestError> {
        let f = open_lock(&self.dir.join(QUEUE_LOCK))?;
        flock(&f, false).map_err(|e| IngestError::Io(format!("queue lock: {e}")))?;
        Ok(Lock(f))
    }

    /// The run lock, or `None` when another run holds it.
    pub fn try_run_lock(&self) -> Result<Option<Lock>, IngestError> {
        let f = open_lock(&self.dir.join(RUN_LOCK))?;
        match flock(&f, true) {
            Ok(true) => Ok(Some(Lock(f))),
            Ok(false) => Ok(None),
            Err(e) => Err(IngestError::Io(format!("run lock: {e}"))),
        }
    }

    fn read_entries(&self) -> Result<Vec<Entry>, IngestError> {
        let f = match File::open(self.file()) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(IngestError::Io(format!("queue: {e}"))),
        };
        let mut out = Vec::new();
        for (n, line) in BufReader::new(f).lines().enumerate() {
            let line = line.map_err(|e| IngestError::Io(format!("queue: {e}")))?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Entry>(&line) {
                Ok(e) => out.push(e),
                // A torn last line (crash mid-append) is skipped, not fatal.
                Err(e) => {
                    tracing::warn!(line = n + 1, error = %e, "vault-ingest: bad queue line skipped")
                }
            }
        }
        Ok(out)
    }

    /// Every key's state, in enqueue order.
    pub fn slots(&self) -> Result<BTreeMap<String, Slot>, IngestError> {
        Ok(replay(self.read_entries()?).0)
    }

    /// Slots in the order they were queued (oldest first).
    pub fn ordered(&self) -> Result<Vec<(String, Slot)>, IngestError> {
        let (mut slots, order) = replay(self.read_entries()?);
        Ok(order
            .into_iter()
            .filter_map(|k| slots.remove(&k).map(|s| (k, s)))
            .collect())
    }

    fn append(&self, _lock: &Lock, entries: &[Entry]) -> Result<(), IngestError> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut buf = String::new();
        for e in entries {
            buf.push_str(&serde_json::to_string(e).map_err(|e| IngestError::Io(e.to_string()))?);
            buf.push('\n');
        }
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.file())
            .map_err(|e| IngestError::Io(format!("queue: {e}")))?;
        f.write_all(buf.as_bytes())
            .and_then(|_| f.sync_data())
            .map_err(|e| IngestError::Io(format!("queue: {e}")))
    }

    /// Queue the items whose keys are new. Returns how many were added.
    pub fn enqueue(&self, items: Vec<Item>, now: DateTime<Utc>) -> Result<usize, IngestError> {
        if items.is_empty() {
            return Ok(0);
        }
        let lock = self.lock()?;
        let known = self.slots()?;
        let mut seen = std::collections::HashSet::new();
        let fresh: Vec<Entry> = items
            .into_iter()
            .filter(|i| !known.contains_key(&i.key) && seen.insert(i.key.clone()))
            .map(|item| Entry::Enqueue { item, at: now })
            .collect();
        let n = fresh.len();
        self.append(&lock, &fresh)?;
        Ok(n)
    }

    pub fn planned(&self, key: &str, plan: &Plan, now: DateTime<Utc>) -> Result<(), IngestError> {
        let lock = self.lock()?;
        self.append(
            &lock,
            &[Entry::Planned {
                key: key.into(),
                plan: plan.clone(),
                at: now,
            }],
        )
    }

    pub fn done(
        &self,
        key: &str,
        notes: Vec<String>,
        now: DateTime<Utc>,
    ) -> Result<(), IngestError> {
        let lock = self.lock()?;
        self.append(
            &lock,
            &[Entry::Done {
                key: key.into(),
                notes,
                at: now,
            }],
        )
    }

    pub fn skipped(&self, key: &str, reason: &str, now: DateTime<Utc>) -> Result<(), IngestError> {
        let lock = self.lock()?;
        self.append(
            &lock,
            &[Entry::Skipped {
                key: key.into(),
                reason: reason.into(),
                at: now,
            }],
        )
    }

    /// One more failed attempt: back off 5 min × 2^(attempts-1), at most 6 h.
    /// Returns the new attempt count.
    pub fn failed(&self, key: &str, error: &str, now: DateTime<Utc>) -> Result<u32, IngestError> {
        let lock = self.lock()?;
        let attempts = match self.slots()?.get(key).map(|s| &s.state) {
            Some(State::Pending { attempts, .. }) => attempts + 1,
            _ => 1,
        };
        let wait = Duration::minutes((5i64 << (attempts - 1).min(7)).min(360));
        self.append(
            &lock,
            &[Entry::Failed {
                key: key.into(),
                error: crate::clip(error, 500),
                attempts,
                next_try: now + wait,
                at: now,
            }],
        )?;
        Ok(attempts)
    }

    /// Re-arm failed (or, with `any`, skipped and done) items. Returns how many.
    pub fn retry(
        &self,
        keys: &[String],
        any: bool,
        now: DateTime<Utc>,
    ) -> Result<usize, IngestError> {
        let lock = self.lock()?;
        let slots = self.slots()?;
        let entries: Vec<Entry> = keys
            .iter()
            .filter(|k| {
                slots.get(k.as_str()).is_some_and(|s| {
                    s.item.is_some()
                        && match s.state {
                            State::Failed { .. } => true,
                            State::Pending { .. } => false,
                            State::Done { .. } | State::Skipped { .. } => any,
                        }
                })
            })
            .map(|k| Entry::Retry {
                key: k.clone(),
                at: now,
            })
            .collect();
        let n = entries.len();
        self.append(&lock, &entries)?;
        Ok(n)
    }

    /// Keys that gave up.
    pub fn failed_keys(&self) -> Result<Vec<String>, IngestError> {
        Ok(self
            .slots()?
            .into_iter()
            .filter(|(_, s)| matches!(s.state, State::Failed { .. }))
            .map(|(k, _)| k)
            .collect())
    }

    pub fn depth(&self) -> Result<(Depth, Option<DateTime<Utc>>), IngestError> {
        let mut d = Depth::default();
        let mut oldest: Option<DateTime<Utc>> = None;
        for s in self.slots()?.values() {
            match &s.state {
                State::Pending { .. } => {
                    d.pending += 1;
                    if s.plan.is_some() {
                        d.planned += 1;
                    }
                    oldest = Some(oldest.map_or(s.enqueued_at, |o| o.min(s.enqueued_at)));
                }
                State::Done { .. } => d.done += 1,
                State::Skipped { .. } => d.skipped += 1,
                State::Failed { .. } => d.failed += 1,
            }
        }
        Ok((d, oldest))
    }

    /// Rewrite the journal with one line per finished key (no payload) and
    /// the full history of the rest, once it is bigger than 4 MiB.
    pub fn compact_if_big(&self) -> Result<bool, IngestError> {
        let big = fs::metadata(self.file())
            .map(|m| m.len() > COMPACT_BYTES)
            .unwrap_or(false);
        if !big {
            return Ok(false);
        }
        let _lock = self.lock()?;
        let entries = self.read_entries()?;
        let (slots, order) = replay(entries.clone());
        let mut keep = Vec::new();
        for key in &order {
            let Some(s) = slots.get(key) else { continue };
            match &s.state {
                State::Done { notes } => keep.push(Entry::Done {
                    key: key.clone(),
                    notes: notes.clone(),
                    at: s.updated_at,
                }),
                State::Skipped { reason } => keep.push(Entry::Skipped {
                    key: key.clone(),
                    reason: reason.clone(),
                    at: s.updated_at,
                }),
                _ => keep.extend(entries.iter().filter(|e| entry_key(e) == key).cloned()),
            }
        }
        let tmp = self.dir.join(format!(".{QUEUE_FILE}.tmp"));
        let mut buf = String::new();
        for e in &keep {
            buf.push_str(&serde_json::to_string(e).map_err(|e| IngestError::Io(e.to_string()))?);
            buf.push('\n');
        }
        fs::write(&tmp, buf)
            .and_then(|_| File::open(&tmp)?.sync_all())
            .and_then(|_| fs::rename(&tmp, self.file()))
            .map_err(|e| IngestError::Io(format!("queue compaction: {e}")))?;
        Ok(true)
    }
}

fn entry_key(e: &Entry) -> &str {
    match e {
        Entry::Enqueue { item, .. } => &item.key,
        Entry::Planned { key, .. }
        | Entry::Done { key, .. }
        | Entry::Skipped { key, .. }
        | Entry::Failed { key, .. }
        | Entry::Retry { key, .. } => key,
    }
}

fn replay(entries: Vec<Entry>) -> (BTreeMap<String, Slot>, Vec<String>) {
    let mut slots: BTreeMap<String, Slot> = BTreeMap::new();
    let mut order = Vec::new();
    let fresh = || State::Pending {
        attempts: 0,
        next_try: None,
        last_error: None,
    };
    for e in entries {
        let key = entry_key(&e).to_owned();
        if !slots.contains_key(&key) {
            order.push(key.clone());
        }
        match e {
            Entry::Enqueue { item, at } => {
                slots.entry(key).or_insert(Slot {
                    item: Some(item),
                    plan: None,
                    state: fresh(),
                    enqueued_at: at,
                    updated_at: at,
                });
            }
            Entry::Planned { plan, at, .. } => {
                if let Some(s) = slots.get_mut(&key) {
                    s.plan = Some(plan);
                    s.updated_at = at;
                }
            }
            Entry::Done { notes, at, .. } => {
                let s = slots.entry(key).or_insert_with(|| Slot {
                    item: None,
                    plan: None,
                    state: State::Done { notes: Vec::new() },
                    enqueued_at: at,
                    updated_at: at,
                });
                s.state = State::Done { notes };
                s.updated_at = at;
            }
            Entry::Skipped { reason, at, .. } => {
                let s = slots.entry(key).or_insert_with(|| Slot {
                    item: None,
                    plan: None,
                    state: State::Skipped {
                        reason: String::new(),
                    },
                    enqueued_at: at,
                    updated_at: at,
                });
                s.state = State::Skipped { reason };
                s.updated_at = at;
            }
            Entry::Failed {
                error,
                attempts,
                next_try,
                at,
                ..
            } => {
                if let Some(s) = slots.get_mut(&key) {
                    s.state = if attempts >= MAX_ATTEMPTS {
                        State::Failed {
                            attempts,
                            last_error: error,
                        }
                    } else {
                        State::Pending {
                            attempts,
                            next_try: Some(next_try),
                            last_error: Some(error),
                        }
                    };
                    s.updated_at = at;
                }
            }
            Entry::Retry { at, .. } => {
                if let Some(s) = slots.get_mut(&key) {
                    // A retried item is planned anew.
                    s.plan = None;
                    s.state = fresh();
                    s.updated_at = at;
                }
            }
        }
    }
    order.retain(|k| slots.contains_key(k));
    (slots, order)
}
