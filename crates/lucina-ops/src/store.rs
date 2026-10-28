//! The source of truth: `<state_dir>/records.jsonl` (one [`StoredRecord`]
//! per line, mode 0600, directory 0700), and the exclusive advisory lock
//! `<state_dir>/ops.lock` every writer (the server and the CLI) takes around
//! its writes.
//!
//! The records are cached in memory and reloaded whenever the file changed
//! (inode, size or mtime): another process may have appended or rewritten it.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::record::StoredRecord;
use crate::OpsError;

pub const RECORDS_FILE: &str = "records.jsonl";
pub const LOCK_FILE: &str = "ops.lock";

fn io(what: &str, e: std::io::Error) -> OpsError {
    OpsError::Io(format!("{what}: {e}"))
}

/// `(dev, inode, size, mtime ns)` of the records file.
type Stamp = (u64, u64, u64, i128);

#[derive(Default)]
struct Cache {
    stamp: Option<Stamp>,
    records: Vec<StoredRecord>,
    by_id: HashMap<String, usize>,
}

pub struct Store {
    dir: PathBuf,
    cache: Mutex<Cache>,
}

/// The held lock (released on drop).
pub struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn stamp(path: &Path) -> Option<Stamp> {
    let m = fs::metadata(path).ok()?;
    Some((
        m.dev(),
        m.ino(),
        m.size(),
        m.mtime() as i128 * 1_000_000_000 + m.mtime_nsec() as i128,
    ))
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self, OpsError> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| io("state dir", e))?;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        let s = Self {
            dir: dir.to_owned(),
            cache: Mutex::default(),
        };
        s.refresh()?;
        Ok(s)
    }

    pub fn path(&self) -> PathBuf {
        self.dir.join(RECORDS_FILE)
    }

    /// Take the exclusive lock (blocks while the other process writes).
    pub fn lock(&self) -> Result<Lock, OpsError> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(self.dir.join(LOCK_FILE))
            .map_err(|e| io("lock file", e))?;
        f.lock().map_err(|e| io("lock", e))?;
        Ok(Lock(f))
    }

    /// Reload the cache when the file changed.
    pub fn refresh(&self) -> Result<(), OpsError> {
        let path = self.path();
        let now = stamp(&path);
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if c.stamp == now && now.is_some() {
            return Ok(());
        }
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(io("records", e)),
        };
        let mut records = Vec::new();
        let mut by_id = HashMap::new();
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<StoredRecord>(line) {
                Ok(r) => {
                    if !by_id.contains_key(&r.id) {
                        by_id.insert(r.id.clone(), records.len());
                        records.push(r);
                    }
                }
                Err(e) => {
                    tracing::warn!(line = n + 1, error = %e, "ops store: unreadable line skipped")
                }
            }
        }
        *c = Cache {
            stamp: now,
            records,
            by_id,
        };
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<StoredRecord> {
        let c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        c.by_id.get(id).map(|&i| c.records[i].clone())
    }

    pub fn all(&self) -> Vec<StoredRecord> {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .records
            .clone()
    }

    /// Append one record (under the lock).
    pub fn append(&self, _lock: &Lock, r: &StoredRecord) -> Result<(), OpsError> {
        let path = self.path();
        let mut line = serde_json::to_string(r).map_err(|e| OpsError::Io(e.to_string()))?;
        line.push('\n');
        let mut f = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| io("records", e))?;
        f.write_all(line.as_bytes())
            .and_then(|_| f.sync_data())
            .map_err(|e| io("records", e))?;
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if !c.by_id.contains_key(&r.id) {
            let i = c.records.len();
            c.by_id.insert(r.id.clone(), i);
            c.records.push(r.clone());
        }
        c.stamp = stamp(&path);
        Ok(())
    }

    /// Set `honcho_synced` on `ids` and rewrite the file atomically (under
    /// the lock, after a refresh).
    pub fn mark_synced(&self, _lock: &Lock, ids: &[String]) -> Result<(), OpsError> {
        if ids.is_empty() {
            return Ok(());
        }
        self.refresh()?;
        let path = self.path();
        let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = false;
        for id in ids {
            if let Some(&i) = c.by_id.get(id) {
                if !c.records[i].honcho_synced {
                    c.records[i].honcho_synced = true;
                    changed = true;
                }
            }
        }
        if !changed {
            return Ok(());
        }
        let mut text = String::new();
        for r in &c.records {
            text.push_str(&serde_json::to_string(r).map_err(|e| OpsError::Io(e.to_string()))?);
            text.push('\n');
        }
        let tmp = self
            .dir
            .join(format!(".{RECORDS_FILE}.tmp-{}", std::process::id()));
        let result = (|| {
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, &path)?;
            File::open(&self.dir)?.sync_all()
        })();
        if let Err(e) = result {
            let _ = fs::remove_file(&tmp);
            return Err(io("records rewrite", e));
        }
        c.stamp = stamp(&path);
        Ok(())
    }
}
