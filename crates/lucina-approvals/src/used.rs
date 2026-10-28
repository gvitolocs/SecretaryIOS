//! The single-use registry: the grant ids this host already used, in a
//! mode 0600 JSON-lines file (`{"id","exp"}`), pruned once a grant is past
//! its `exp` (an expired grant fails the `exp` check anyway). Shared by
//! `lucina-rs` and `lucina-root`: every update runs under an exclusive
//! `flock` on the file.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::grant::GrantError;
use crate::ApprovalError;

#[derive(Debug, Clone)]
pub struct UsedRegistry {
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    id: String,
    exp: i64,
}

struct Locked(std::fs::File);

impl Drop for Locked {
    fn drop(&mut self) {
        // SAFETY: the fd is open for the life of `self`.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl UsedRegistry {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn open_locked(&self) -> Result<Locked, ApprovalError> {
        if let Some(dir) = self.path.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir).map_err(io("create the used-ids directory"))?;
            }
        }
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.path)
            .map_err(io("open the used-ids file"))?;
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
        // SAFETY: a valid fd; blocks until the lock is ours.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(ApprovalError::Io(format!(
                "lock the used-ids file: {}",
                std::io::Error::last_os_error().kind()
            )));
        }
        Ok(Locked(f))
    }

    /// Record `id` as used. `Err(Grant(Replay))` when it already was.
    /// `now` and `exp` are unix seconds; entries past their `exp` are pruned.
    pub fn consume(&self, id: &str, exp: i64, now: i64) -> Result<(), ApprovalError> {
        let mut lock = self.open_locked()?;
        let f = &mut lock.0;
        let mut text = String::new();
        f.read_to_string(&mut text)
            .map_err(io("read the used-ids file"))?;
        let mut entries: Vec<Entry> = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
            .filter(|e| e.exp > now)
            .collect();
        if entries.iter().any(|e| e.id == id) {
            return Err(ApprovalError::Grant(GrantError::Replay));
        }
        entries.push(Entry {
            id: id.to_owned(),
            exp,
        });
        let mut out = String::new();
        for e in &entries {
            out.push_str(&serde_json::to_string(e).unwrap_or_default());
            out.push('\n');
        }
        f.set_len(0).map_err(io("truncate the used-ids file"))?;
        f.seek(SeekFrom::Start(0))
            .map_err(io("rewind the used-ids file"))?;
        f.write_all(out.as_bytes())
            .map_err(io("write the used-ids file"))?;
        f.sync_all().map_err(io("sync the used-ids file"))?;
        Ok(())
    }

    /// Whether `id` is recorded (and not yet past `exp`).
    pub fn contains(&self, id: &str, now: i64) -> bool {
        std::fs::read_to_string(&self.path)
            .map(|t| {
                t.lines()
                    .filter_map(|l| serde_json::from_str::<Entry>(l).ok())
                    .any(|e| e.id == id && e.exp > now)
            })
            .unwrap_or(false)
    }
}

fn io(what: &'static str) -> impl Fn(std::io::Error) -> ApprovalError {
    move |e| ApprovalError::Io(format!("{what}: {}", e.kind()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_use_is_a_replay_and_expired_entries_are_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let r = UsedRegistry::new(dir.path().join("sub/used.jsonl"));
        r.consume("apr_a", 200, 100).unwrap();
        assert!(matches!(
            r.consume("apr_a", 200, 150),
            Err(ApprovalError::Grant(GrantError::Replay))
        ));
        r.consume("apr_b", 300, 150).unwrap();
        assert!(r.contains("apr_a", 150));
        // past apr_a's exp it is pruned on the next write
        r.consume("apr_c", 400, 250).unwrap();
        let text = std::fs::read_to_string(r.path()).unwrap();
        assert!(!text.contains("apr_a") && text.contains("apr_b") && text.contains("apr_c"));
        let mode = std::fs::metadata(r.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
