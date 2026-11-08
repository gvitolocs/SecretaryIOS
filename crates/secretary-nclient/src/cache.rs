//! Disk caches under the service's cache directory: vetted JSON entries (`json/<key>.json`,
//! with the time they were written) and proxied images (`img/<gallery>/<name>`). Only values
//! that already passed `safety::vet` are ever written here; a gallery that fails the filter
//! later (a longer blocklist) is purged on its next read.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Default cap on the cache directory: 2 GiB (`NCLIENT_CACHE_MAX_BYTES`).
pub const DEFAULT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub struct DiskCache {
    root: PathBuf,
    /// 0 = no cap.
    max_bytes: u64,
    /// Bytes written since the last scan, plus what that scan found.
    used: AtomicU64,
    evicting: Mutex<()>,
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

impl DiskCache {
    pub fn new(root: impl Into<PathBuf>) -> std::io::Result<DiskCache> {
        DiskCache::with_cap(root, DEFAULT_MAX_BYTES)
    }

    /// `max_bytes == 0` turns the cap off. When the directory grows past the cap the oldest
    /// files (by modification time) are removed until it is at 90 % of it.
    pub fn with_cap(root: impl Into<PathBuf>, max_bytes: u64) -> std::io::Result<DiskCache> {
        let root = root.into();
        std::fs::create_dir_all(root.join("json"))?;
        std::fs::create_dir_all(root.join("img"))?;
        let c = DiskCache {
            root,
            max_bytes,
            used: AtomicU64::new(0),
            evicting: Mutex::new(()),
        };
        if max_bytes > 0 {
            c.evict_if_needed();
        }
        Ok(c)
    }

    /// Total bytes under the cache directory (a scan).
    pub fn size_on_disk(&self) -> u64 {
        files(&self.root).iter().map(|f| f.size).sum()
    }

    fn account(&self, n: usize) {
        if self.max_bytes == 0 {
            return;
        }
        let used = self.used.fetch_add(n as u64, Ordering::Relaxed) + n as u64;
        if used > self.max_bytes {
            self.evict_if_needed();
        }
    }

    fn evict_if_needed(&self) {
        let Ok(_guard) = self.evicting.lock() else {
            return;
        };
        let all = files(&self.root);
        let mut total: u64 = all.iter().map(|f| f.size).sum();
        if total <= self.max_bytes {
            self.used.store(total, Ordering::Relaxed);
            return;
        }
        let target = self.max_bytes / 10 * 9;
        // `.type` sidecars go with their image, never on their own: an image without its
        // sidecar would be served as image/jpeg whatever it is.
        let mut order: Vec<&CacheFile> = all
            .iter()
            .filter(|f| f.path.extension().is_none_or(|e| e != "type"))
            .collect();
        order.sort_by_key(|f| f.modified);
        for f in order {
            if total <= target {
                break;
            }
            let side = f.path.with_extension("type");
            let side_size = std::fs::metadata(&side).map(|m| m.len()).unwrap_or(0);
            if std::fs::remove_file(&f.path).is_ok() {
                total = total.saturating_sub(f.size);
                if side_size > 0 && std::fs::remove_file(&side).is_ok() {
                    total = total.saturating_sub(side_size);
                }
            }
        }
        // Empty gallery directories.
        if let Ok(rd) = std::fs::read_dir(self.root.join("img")) {
            for e in rd.flatten() {
                let _ = std::fs::remove_dir(e.path());
            }
        }
        self.used.store(total, Ordering::Relaxed);
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn json_path(&self, key: &str) -> Option<PathBuf> {
        safe_key(key).then(|| self.root.join("json").join(format!("{key}.json")))
    }

    pub fn get<T: DeserializeOwned>(&self, key: &str, ttl: Duration) -> Option<T> {
        let bytes = std::fs::read(self.json_path(key)?).ok()?;
        let s: Stamped<T> = serde_json::from_slice(&bytes).ok()?;
        (now() - s.at <= ttl.as_secs() as i64).then_some(s.value)
    }

    pub fn put<T: Serialize>(&self, key: &str, value: &T) {
        let Some(path) = self.json_path(key) else {
            return;
        };
        if let Ok(bytes) = serde_json::to_vec(&Stamped { at: now(), value }) {
            write_atomic(&path, &bytes);
            self.account(bytes.len());
        }
    }

    pub fn remove(&self, key: &str) {
        if let Some(p) = self.json_path(key) {
            let _ = std::fs::remove_file(p);
        }
    }

    fn image_path(&self, gallery: i32, name: &str) -> Option<PathBuf> {
        (gallery > 0 && safe_key(name))
            .then(|| self.root.join("img").join(gallery.to_string()).join(name))
    }

    pub fn image(&self, gallery: i32, name: &str) -> Option<(Vec<u8>, String)> {
        let path = self.image_path(gallery, name)?;
        let bytes = std::fs::read(&path).ok()?;
        let ctype = std::fs::read_to_string(path.with_extension("type"))
            .unwrap_or_else(|_| "image/jpeg".into());
        Some((bytes, ctype))
    }

    pub fn put_image(&self, gallery: i32, name: &str, bytes: &[u8], ctype: &str) {
        let Some(path) = self.image_path(gallery, name) else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        write_atomic(&path.with_extension("type"), ctype.as_bytes());
        write_atomic(&path, bytes);
        self.account(bytes.len() + ctype.len());
    }

    /// Every image of one gallery.
    pub fn purge_images(&self, gallery: i32) {
        if gallery > 0 {
            let _ = std::fs::remove_dir_all(self.root.join("img").join(gallery.to_string()));
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

struct CacheFile {
    path: PathBuf,
    size: u64,
    modified: SystemTime,
}

fn files(dir: &Path) -> Vec<CacheFile> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                stack.push(e.path());
            } else {
                out.push(CacheFile {
                    path: e.path(),
                    size: md.len(),
                    modified: md.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cap_removes_the_oldest_files_first() {
        let dir = tempfile::tempdir().unwrap();
        let c = DiskCache::with_cap(dir.path().join("c"), 10_000).unwrap();
        let blob = vec![1u8; 2_000];
        for i in 1..=4 {
            c.put_image(i, "page-0", &blob, "image/png");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(c.image(1, "page-0").is_some());
        c.put_image(5, "page-0", &blob, "image/png");
        std::thread::sleep(Duration::from_millis(20));
        c.put_image(6, "page-0", &blob, "image/png");
        // Over 10 000 bytes: the oldest went, with their sidecars, down to 90 %.
        assert!(c.size_on_disk() <= 9_000, "{}", c.size_on_disk());
        assert!(c.image(1, "page-0").is_none());
        assert!(c.image(2, "page-0").is_none());
        let (_, ctype) = c.image(6, "page-0").unwrap();
        assert_eq!(ctype, "image/png");
    }

    #[test]
    fn zero_means_no_cap() {
        let dir = tempfile::tempdir().unwrap();
        let c = DiskCache::with_cap(dir.path().join("c"), 0).unwrap();
        for i in 1..=5 {
            c.put_image(i, "page-0", &vec![0u8; 5_000], "image/jpeg");
        }
        assert!(c.image(1, "page-0").is_some());
    }
}
