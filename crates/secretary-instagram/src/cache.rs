//! Disk cache under the service's cache directory, with a size cap.
//!
//! - `json/<key>.json`: upstream JSON bodies with the time they were written (short TTLs).
//! - `media/<key>.<ext>`: proxied images and videos. The extension encodes the content type, so
//!   one file per media item is enough.
//!
//! - `profiles/`: the profile store (`store.rs`), never evicted.
//!
//! The cap (`INSTAGRAM_CACHE_MAX_BYTES`, default 2 GiB) covers all of it. When a write pushes the
//! total over the cap, the oldest files (by modification time; a cache hit refreshes it) are
//! deleted until the total is at 90 % of the cap. Pinned media (watched accounts, saved items) is
//! never deleted.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// `(extension, content type)` of every media type the service stores.
pub const MEDIA_TYPES: &[(&str, &str)] = &[
    ("jpg", "image/jpeg"),
    ("png", "image/png"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
    ("mp4", "video/mp4"),
    ("webm", "video/webm"),
];

pub fn ext_for_ctype(ctype: &str) -> Option<&'static str> {
    let c = ctype
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    MEDIA_TYPES.iter().find(|(_, t)| *t == c).map(|(e, _)| *e)
}

pub fn ctype_for_ext(ext: &str) -> Option<&'static str> {
    MEDIA_TYPES.iter().find(|(e, _)| *e == ext).map(|(_, t)| *t)
}

pub struct DiskCache {
    root: PathBuf,
    max_bytes: u64,
    approx_used: AtomicU64,
    evict_lock: Mutex<()>,
    tmp_counter: AtomicU64,
    /// Media keys eviction must leave alone.
    pinned: Mutex<HashSet<String>>,
    /// Pinned files alone are over the cap: nothing more can be evicted.
    full: AtomicBool,
}

#[derive(Serialize, Deserialize)]
struct Stamped<T> {
    at: i64,
    value: T,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn safe_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn walk(dir: &Path, out: &mut Vec<(PathBuf, u64, SystemTime)>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            walk(&p, out);
        } else {
            out.push((p, md.len(), md.modified().unwrap_or(SystemTime::UNIX_EPOCH)));
        }
    }
}

impl DiskCache {
    pub fn new(root: impl Into<PathBuf>, max_bytes: u64) -> std::io::Result<DiskCache> {
        let root = root.into();
        std::fs::create_dir_all(root.join("json"))?;
        std::fs::create_dir_all(root.join("media"))?;
        let c = DiskCache {
            root,
            max_bytes,
            approx_used: AtomicU64::new(0),
            evict_lock: Mutex::new(()),
            tmp_counter: AtomicU64::new(0),
            pinned: Mutex::new(HashSet::new()),
            full: AtomicBool::new(false),
        };
        c.sweep_partials();
        c.approx_used.store(c.total_bytes(), Ordering::Relaxed);
        c.evict_if_needed();
        Ok(c)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Replace the set of media keys eviction must leave alone.
    pub fn set_pinned(&self, keys: HashSet<String>) {
        *self.pinned.lock().unwrap_or_else(|e| e.into_inner()) = keys;
    }

    /// One more media key eviction must leave alone.
    pub fn pin(&self, key: &str) {
        self.pinned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.to_string());
    }

    /// Whether the last eviction could not get under the cap (too much is pinned).
    pub fn is_full(&self) -> bool {
        self.full.load(Ordering::Relaxed)
    }

    /// Exact size of everything under the root.
    pub fn total_bytes(&self) -> u64 {
        let mut files = Vec::new();
        walk(&self.root, &mut files);
        files.iter().map(|(_, n, _)| *n).sum()
    }

    /// Half-written downloads left by a crash.
    fn sweep_partials(&self) {
        let mut files = Vec::new();
        walk(&self.root.join("media"), &mut files);
        for (p, _, _) in files {
            if p.extension().is_some_and(|e| e == "part") {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    // ---- JSON ----

    fn json_path(&self, key: &str) -> Option<PathBuf> {
        safe_key(key).then(|| self.root.join("json").join(format!("{key}.json")))
    }

    pub fn get<T: DeserializeOwned>(&self, key: &str, ttl: Duration) -> Option<T> {
        let bytes = std::fs::read(self.json_path(key)?).ok()?;
        let s: Stamped<T> = serde_json::from_slice(&bytes).ok()?;
        (now() - s.at <= ttl.as_secs() as i64).then_some(s.value)
    }

    pub fn put<T: Serialize>(&self, key: &str, value: &T) {
        self.put_aged(key, value, Duration::ZERO);
    }

    /// `put`, stamped `age` ago (tests: an entry written by an earlier visit or warmer pass).
    pub fn put_aged<T: Serialize>(&self, key: &str, value: &T, age: Duration) {
        let Some(path) = self.json_path(key) else {
            return;
        };
        let at = now() - age.as_secs() as i64;
        if let Ok(bytes) = serde_json::to_vec(&Stamped { at, value }) {
            write_atomic(&path, &bytes);
            self.account(bytes.len() as u64);
        }
    }

    // ---- media ----

    /// Whether a media file is on disk (its age stays).
    pub fn has_media(&self, key: &str) -> bool {
        safe_key(key)
            && MEDIA_TYPES.iter().any(|(ext, _)| {
                self.root
                    .join("media")
                    .join(format!("{key}.{ext}"))
                    .is_file()
            })
    }

    /// Path of a cached media file, refreshing its age. `None` when it isn't cached.
    pub fn find_media(&self, key: &str) -> Option<(PathBuf, &'static str)> {
        if !safe_key(key) {
            return None;
        }
        for (ext, ctype) in MEDIA_TYPES {
            let p = self.root.join("media").join(format!("{key}.{ext}"));
            if p.is_file() {
                touch(&p);
                return Some((p, ctype));
            }
        }
        None
    }

    /// A fresh temporary file path for a download of `key` (not counted until committed).
    pub fn tmp_media(&self, key: &str) -> PathBuf {
        let n = self.tmp_counter.fetch_add(1, Ordering::Relaxed);
        self.root
            .join("media")
            .join(format!("{key}.{}.{n}.part", std::process::id()))
    }

    /// Move a finished download into place and enforce the cap.
    pub fn commit_media(&self, tmp: &Path, key: &str, ext: &str) -> std::io::Result<PathBuf> {
        let dest = self.root.join("media").join(format!("{key}.{ext}"));
        let len = std::fs::metadata(tmp)?.len();
        std::fs::rename(tmp, &dest)?;
        self.account(len);
        Ok(dest)
    }

    fn account(&self, added: u64) {
        let used = self.approx_used.fetch_add(added, Ordering::Relaxed) + added;
        if used > self.max_bytes {
            self.evict_if_needed();
        }
    }

    /// Delete the oldest files until the total is at 90 % of the cap. Returns how many went.
    pub fn evict_if_needed(&self) -> usize {
        let _g = self.evict_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut files = Vec::new();
        walk(&self.root, &mut files);
        let mut total: u64 = files.iter().map(|(_, n, _)| *n).sum();
        if total <= self.max_bytes {
            self.full.store(false, Ordering::Relaxed);
            self.approx_used.store(total, Ordering::Relaxed);
            return 0;
        }
        let target = self.max_bytes / 10 * 9;
        let pinned = self
            .pinned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let store = self.root.join("profiles");
        files.sort_by_key(|(_, _, m)| *m);
        let mut removed = 0;
        for (p, n, _) in files {
            if total <= target {
                break;
            }
            // Never delete a download that is still being written, the profile store or a file
            // that must stay.
            if p.extension().is_some_and(|e| e == "part")
                || p.starts_with(&store)
                || p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| pinned.contains(s))
            {
                continue;
            }
            if std::fs::remove_file(&p).is_ok() {
                total = total.saturating_sub(n);
                removed += 1;
            }
        }
        let full = total > self.max_bytes;
        if full && !self.full.load(Ordering::Relaxed) {
            tracing::warn!(
                total,
                cap = self.max_bytes,
                "instagram cache: what must stay is over the cap; downloads pause"
            );
        }
        self.full.store(full, Ordering::Relaxed);
        self.approx_used.store(total, Ordering::Relaxed);
        removed
    }
}

fn touch(p: &Path) {
    if let Ok(f) = std::fs::OpenOptions::new().append(true).open(p) {
        let _ = f.set_modified(SystemTime::now());
    }
}

/// Write a file that only the owner may read (state files: watch list, archive index).
pub fn write_private_atomic(path: &Path, bytes: &[u8]) {
    let tmp = path.with_extension("tmp");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp);
        if let Ok(mut f) = f {
            if f.write_all(bytes).is_ok() {
                let _ = std::fs::rename(&tmp, path);
            }
        }
    }
    #[cfg(not(unix))]
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8]) {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_media(c: &DiskCache, key: &str, n: usize) {
        let tmp = c.tmp_media(key);
        std::fs::write(&tmp, vec![1u8; n]).unwrap();
        c.commit_media(&tmp, key, "jpg").unwrap();
    }

    #[test]
    fn evicts_oldest_first_when_over_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let c = DiskCache::new(dir.path(), 3_000).unwrap();
        put_media(&c, "a", 1_000);
        std::thread::sleep(Duration::from_millis(20));
        put_media(&c, "b", 1_000);
        std::thread::sleep(Duration::from_millis(20));
        put_media(&c, "c", 1_000);
        assert!(
            c.find_media("a").is_some(),
            "exactly at the cap: nothing evicted"
        );
        std::thread::sleep(Duration::from_millis(20));
        put_media(&c, "d", 1_000); // 4000 > 3000: evict down to 2700 -> drops the two oldest
        assert!(c.total_bytes() <= 3_000);
        assert!(c.find_media("d").is_some());
        assert!(c.find_media("b").is_none());
    }

    #[test]
    fn a_hit_refreshes_the_age() {
        let dir = tempfile::tempdir().unwrap();
        let c = DiskCache::new(dir.path(), 2_500).unwrap();
        put_media(&c, "old", 1_000);
        std::thread::sleep(Duration::from_millis(30));
        put_media(&c, "mid", 1_000);
        std::thread::sleep(Duration::from_millis(30));
        assert!(c.find_media("old").is_some()); // touch: now the newest
        std::thread::sleep(Duration::from_millis(30));
        put_media(&c, "new", 1_000);
        assert!(c.find_media("old").is_some());
        assert!(c.find_media("mid").is_none());
    }

    #[test]
    fn pinned_media_and_the_profile_store_are_never_evicted() {
        let dir = tempfile::tempdir().unwrap();
        let c = DiskCache::new(dir.path(), 3_000).unwrap();
        std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
        std::fs::write(dir.path().join("profiles/doc.json"), vec![b'x'; 200]).unwrap();
        c.set_pinned(HashSet::from(["keep".to_string()]));
        put_media(&c, "keep", 1_000);
        std::thread::sleep(Duration::from_millis(20));
        put_media(&c, "b", 1_000);
        std::thread::sleep(Duration::from_millis(20));
        put_media(&c, "c", 1_000);
        std::thread::sleep(Duration::from_millis(20));
        put_media(&c, "d", 1_000);
        assert!(c.has_media("keep"), "the oldest file, but pinned");
        assert!(!c.has_media("b"));
        assert!(dir.path().join("profiles/doc.json").exists());
        assert!(!c.is_full());
        // Only pinned files left and still over the cap: said, and nothing is deleted.
        let all = ["keep", "d", "e", "f"];
        c.set_pinned(all.iter().map(|k| k.to_string()).collect());
        put_media(&c, "e", 1_000);
        put_media(&c, "f", 1_000);
        assert!(all.iter().all(|k| c.has_media(k)));
        assert!(c.is_full());
    }

    #[test]
    fn json_entries_expire() {
        let dir = tempfile::tempdir().unwrap();
        let c = DiskCache::new(dir.path(), 1 << 20).unwrap();
        c.put("k", &"v".to_string());
        assert_eq!(
            c.get::<String>("k", Duration::from_secs(60)),
            Some("v".into())
        );
        assert_eq!(c.get::<String>("nope", Duration::from_secs(60)), None);
        assert_eq!(c.get::<String>("../x", Duration::from_secs(60)), None);
    }

    #[test]
    fn startup_removes_partials_and_trims_to_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        {
            let c = DiskCache::new(dir.path(), 10_000).unwrap();
            put_media(&c, "x", 4_000);
            put_media(&c, "y", 4_000);
            std::fs::write(dir.path().join("media/zzz.1.0.part"), b"half").unwrap();
        }
        let c = DiskCache::new(dir.path(), 5_000).unwrap();
        assert!(!dir.path().join("media/zzz.1.0.part").exists());
        assert!(c.total_bytes() <= 5_000);
    }
}
