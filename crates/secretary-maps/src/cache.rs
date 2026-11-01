//! Bounded TTL/LRU cache. Only Places entries persist; a private temporary file is renamed
//! over the snapshot so a crash cannot leave half a JSON document.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Serialize, Deserialize, Clone)]
struct Entry {
    value: Value,
    expires: u64,
    used: u64,
    persistent: bool,
}
pub struct Cache {
    path: PathBuf,
    entries: HashMap<String, Entry>,
    tick: u64,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
impl Cache {
    pub fn load(path: PathBuf) -> Self {
        let mut entries: HashMap<String, Entry> = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        entries.retain(|_, e| e.persistent && e.expires > now());
        while entries.len() > 5000 {
            let k = entries
                .iter()
                .min_by_key(|(_, e)| e.used)
                .unwrap()
                .0
                .clone();
            entries.remove(&k);
        }
        let tick = entries.values().map(|e| e.used).max().unwrap_or(0);
        Self {
            path,
            entries,
            tick,
        }
    }
    pub fn get(&mut self, key: &str) -> Option<Value> {
        self.tick = self.tick.saturating_add(1);
        let e = self.entries.get_mut(key)?;
        if e.expires <= now() {
            self.entries.remove(key);
            return None;
        }
        e.used = self.tick;
        Some(e.value.clone())
    }
    pub fn insert(&mut self, key: String, value: Value, ttl: u64, persistent: bool) {
        self.tick = self.tick.saturating_add(1);
        self.entries.retain(|_, e| e.expires > now());
        // Separate quotas keep routing traffic from evicting the Places cache.
        if !self.entries.contains_key(&key)
            && self
                .entries
                .values()
                .filter(|e| e.persistent == persistent)
                .count()
                >= 5000
        {
            if let Some(k) = self
                .entries
                .iter()
                .filter(|(_, e)| e.persistent == persistent)
                .min_by_key(|(_, e)| e.used)
                .map(|(k, _)| k.clone())
            {
                self.entries.remove(&k);
            }
        }
        self.entries.insert(
            key,
            Entry {
                value,
                expires: now().saturating_add(ttl),
                used: self.tick,
                persistent,
            },
        );
        if persistent && self.save().is_err() {
            tracing::warn!("Places cache persistence failed");
        }
    }
    fn save(&self) -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let parent = self.path.parent().unwrap();
        std::fs::create_dir_all(parent)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = self
            .path
            .with_extension(format!("{}.{id}.tmp", std::process::id()));
        let result = (|| {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            let entries: HashMap<_, _> =
                self.entries.iter().filter(|(_, e)| e.persistent).collect();
            f.write_all(&serde_json::to_vec(&entries)?)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &self.path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(tmp);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn ttl_lru_quota_and_private_snapshot() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("state/cache.json");
        let mut c = Cache::load(path.clone());
        c.insert("expired".into(), json!(1), 0, true);
        assert!(c.get("expired").is_none());
        c.insert("places".into(), json!([1]), 3600, true);
        c.insert("route".into(), json!(2), 3600, false);
        assert_eq!(Cache::load(path.clone()).get("places"), Some(json!([1])));
        assert!(Cache::load(path.clone()).get("route").is_none());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Fill without writing thousands of snapshots; insertion must evict the LRU.
        c.entries.clear();
        for n in 0..5000 {
            c.entries.insert(
                n.to_string(),
                Entry {
                    value: json!(n),
                    expires: now() + 3600,
                    used: n,
                    persistent: true,
                },
            );
        }
        c.tick = 5000;
        assert!(c.get("0").is_some());
        c.insert("new".into(), json!(7), 3600, true);
        assert_eq!(c.entries.len(), 5000);
        assert!(c.get("1").is_none());
        assert!(c.get("0").is_some());
        assert_eq!(Cache::load(path.clone()).entries.len(), 5000);
        std::fs::write(&path, "broken JSON").unwrap();
        assert!(Cache::load(path).entries.is_empty());
    }
}
