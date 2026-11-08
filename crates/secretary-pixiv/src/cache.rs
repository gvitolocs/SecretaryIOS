//! Disk cache under the service's cache directory, with a size cap.
//!
//! Two kinds of entries share one budget: JSON (`json/<key>`, the normalised upstream answers
//! with the time they were stored) and proxied images (`img/<key>`, the content type on the first
//! line, then the bytes). The cache keeps an in-memory index (path, size, recency) built from the
//! directory at start-up; when a write pushes the total over the cap, the least recently used
//! entries are deleted until it fits again (the entry just written is the newest, so it stays
//! unless it alone is larger than the cap, in which case it is not stored at all).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Stamped<T> {
    at: i64,
    value: T,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[derive(Default)]
struct Index {
    total: u64,
    seq: u64,
    /// rel path ("img/<key>") -> (size, recency)
    files: HashMap<String, (u64, u64)>,
    /// recency -> rel path, oldest first
    order: BTreeMap<u64, String>,
}

impl Index {
    fn touch(&mut self, rel: &str) {
        if let Some((_, seq)) = self.files.get_mut(rel) {
            self.order.remove(seq);
            self.seq += 1;
            *seq = self.seq;
            self.order.insert(self.seq, rel.to_string());
        }
    }

    fn insert(&mut self, rel: &str, size: u64) {
        self.forget(rel);
        self.seq += 1;
        self.files.insert(rel.to_string(), (size, self.seq));
        self.order.insert(self.seq, rel.to_string());
        self.total += size;
    }

    fn forget(&mut self, rel: &str) {
        if let Some((size, seq)) = self.files.remove(rel) {
            self.order.remove(&seq);
            self.total = self.total.saturating_sub(size);
        }
    }
}

pub struct DiskCache {
    root: PathBuf,
    max_bytes: u64,
    idx: Mutex<Index>,
}

fn safe_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl DiskCache {
    pub fn new(root: impl Into<PathBuf>, max_bytes: u64) -> std::io::Result<DiskCache> {
        let root = root.into();
        std::fs::create_dir_all(root.join("json"))?;
        std::fs::create_dir_all(root.join("img"))?;
        let mut found: Vec<(std::time::SystemTime, String, u64)> = Vec::new();
        for dir in ["json", "img"] {
            for e in std::fs::read_dir(root.join(dir))?.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let Ok(meta) = e.metadata() else { continue };
                if !meta.is_file() {
                    continue;
                }
                if name.contains(".tmp") {
                    let _ = std::fs::remove_file(e.path());
                    continue;
                }
                let at = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                found.push((at, format!("{dir}/{name}"), meta.len()));
            }
        }
        found.sort();
        let mut idx = Index::default();
        for (_, rel, size) in found {
            idx.insert(&rel, size);
        }
        let cache = DiskCache {
            root,
            max_bytes: max_bytes.max(1),
            idx: Mutex::new(idx),
        };
        cache.evict();
        Ok(cache)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Bytes currently kept (index view).
    pub fn total_bytes(&self) -> u64 {
        self.idx.lock().unwrap().total
    }

    fn path(&self, dir: &str, key: &str) -> Option<(String, PathBuf)> {
        safe_key(key).then(|| {
            let rel = format!("{dir}/{key}");
            let p = self.root.join(&rel);
            (rel, p)
        })
    }

    fn store(&self, rel: &str, path: &Path, bytes: &[u8]) {
        if bytes.len() as u64 > self.max_bytes {
            return;
        }
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, bytes).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        if std::fs::rename(&tmp, path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        self.idx.lock().unwrap().insert(rel, bytes.len() as u64);
        self.evict();
    }

    fn evict(&self) {
        let mut victims = Vec::new();
        {
            let mut idx = self.idx.lock().unwrap();
            while idx.total > self.max_bytes {
                let Some((_, rel)) = idx.order.iter().next().map(|(k, v)| (*k, v.clone())) else {
                    break;
                };
                idx.forget(&rel);
                victims.push(rel);
            }
        }
        for rel in victims {
            let _ = std::fs::remove_file(self.root.join(rel));
        }
    }

    // ---- JSON ----------------------------------------------------------------------------

    pub fn get_json<T: DeserializeOwned>(&self, key: &str, ttl: Duration) -> Option<T> {
        let (rel, path) = self.path("json", key)?;
        let bytes = std::fs::read(path).ok()?;
        let s: Stamped<T> = serde_json::from_slice(&bytes).ok()?;
        if now() - s.at > ttl.as_secs() as i64 {
            return None;
        }
        self.idx.lock().unwrap().touch(&rel);
        Some(s.value)
    }

    pub fn put_json<T: Serialize>(&self, key: &str, value: &T) {
        let Some((rel, path)) = self.path("json", key) else {
            return;
        };
        if let Ok(bytes) = serde_json::to_vec(&Stamped { at: now(), value }) {
            self.store(&rel, &path, &bytes);
        }
    }

    // ---- images --------------------------------------------------------------------------

    pub fn get_image(&self, key: &str) -> Option<(Vec<u8>, String)> {
        let (rel, path) = self.path("img", key)?;
        let mut bytes = std::fs::read(path).ok()?;
        let nl = bytes.iter().position(|b| *b == b'\n')?;
        let ctype = String::from_utf8_lossy(&bytes[..nl]).into_owned();
        bytes.drain(..=nl);
        self.idx.lock().unwrap().touch(&rel);
        Some((bytes, ctype))
    }

    pub fn put_image(&self, key: &str, bytes: &[u8], ctype: &str) {
        let Some((rel, path)) = self.path("img", key) else {
            return;
        };
        let mut all = Vec::with_capacity(bytes.len() + ctype.len() + 1);
        all.extend_from_slice(ctype.replace('\n', " ").as_bytes());
        all.push(b'\n');
        all.extend_from_slice(bytes);
        self.store(&rel, &path, &all);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used_first() {
        let dir = tempfile::tempdir().unwrap();
        let c = DiskCache::new(dir.path(), 1000).unwrap();
        c.put_image("a", &[1; 400], "image/jpeg");
        c.put_image("b", &[2; 400], "image/jpeg");
        assert!(c.get_image("a").is_some()); // a is now newer than b
        c.put_image("c", &[3; 400], "image/jpeg"); // 3 * ~412 > 1000
        assert!(c.get_image("b").is_none());
        assert!(c.get_image("a").is_some());
        assert!(c.get_image("c").is_some());
        assert!(c.total_bytes() <= 1000);
    }

    #[test]
    fn an_entry_bigger_than_the_cap_is_not_stored() {
        let dir = tempfile::tempdir().unwrap();
        let c = DiskCache::new(dir.path(), 100).unwrap();
        c.put_image("big", &[0; 500], "image/png");
        assert!(c.get_image("big").is_none());
        assert_eq!(c.total_bytes(), 0);
    }

    #[test]
    fn the_index_is_rebuilt_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        {
            let c = DiskCache::new(dir.path(), 10_000).unwrap();
            c.put_image("a", &[1; 300], "image/jpeg");
            c.put_json("j", &serde_json::json!({"x": 1}));
        }
        let c = DiskCache::new(dir.path(), 10_000).unwrap();
        assert!(c.total_bytes() >= 300);
        assert_eq!(c.get_image("a").unwrap().0.len(), 300);
        let v: serde_json::Value = c.get_json("j", Duration::from_secs(60)).unwrap();
        assert_eq!(v["x"], 1);
        // shrinking the cap evicts at start-up
        drop(c);
        let c = DiskCache::new(dir.path(), 200).unwrap();
        assert!(c.total_bytes() <= 200);
    }

    #[test]
    fn unsafe_keys_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let c = DiskCache::new(dir.path(), 1000).unwrap();
        c.put_image("../x", &[1; 10], "image/jpeg");
        assert!(c.get_image("../x").is_none());
    }
}
