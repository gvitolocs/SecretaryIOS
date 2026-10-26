//! Disk caches under the service's cache directory: vetted JSON entries (`json/<key>.json`,
//! with the time they were written) and proxied images (`img/<gallery>/<name>`). Only values
//! that already passed `safety::vet` are ever written here; a gallery that fails the filter
//! later (a longer blocklist) is purged on its next read.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub struct DiskCache {
    root: PathBuf,
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
        let root = root.into();
        std::fs::create_dir_all(root.join("json"))?;
        std::fs::create_dir_all(root.join("img"))?;
        Ok(DiskCache { root })
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
