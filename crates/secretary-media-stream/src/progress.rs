//! The server's copy of resume positions: one JSON file, last writer wins by
//! `updated_at` (the playing device's clock), same rule as the apps' core.
use secretary_player::progress::Entry;
use std::{collections::BTreeMap, path::PathBuf};
use tokio::sync::Mutex;

pub struct Store {
    path: PathBuf,
    entries: Mutex<BTreeMap<String, Entry>>,
}

impl Store {
    pub fn open(path: PathBuf) -> Result<Self, String> {
        let entries: BTreeMap<String, Entry> = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(Self { path, entries: Mutex::new(entries) })
    }

    pub async fn get(&self, id: &str) -> Option<Entry> {
        self.entries.lock().await.get(id).cloned()
    }

    /// Entries changed after `since` (ms), oldest first.
    pub async fn since(&self, since: u64) -> Vec<Entry> {
        let mut v: Vec<Entry> = self.entries.lock().await.values().filter(|e| e.updated_at > since).cloned().collect();
        v.sort_by_key(|e| e.updated_at);
        v
    }

    /// Stores the entries that are newer than what the server has. Returns
    /// the server's entries that are newer than the submitted ones, so the
    /// client can adopt them.
    pub async fn put(&self, incoming: Vec<Entry>) -> Result<Vec<Entry>, String> {
        let mut entries = self.entries.lock().await;
        let mut newer = Vec::new();
        let mut changed = false;
        for e in incoming {
            match entries.get(&e.id) {
                Some(cur) if cur.updated_at >= e.updated_at => {
                    if cur.updated_at > e.updated_at {
                        newer.push(cur.clone());
                    }
                }
                _ => {
                    entries.insert(e.id.clone(), e);
                    changed = true;
                }
            }
        }
        if changed {
            let bytes = serde_json::to_vec(&*entries).map_err(|e| e.to_string())?;
            let tmp = self.path.with_extension("tmp");
            if let Some(dir) = self.path.parent() {
                tokio::fs::create_dir_all(dir).await.map_err(|e| e.to_string())?;
            }
            tokio::fs::write(&tmp, bytes).await.map_err(|e| e.to_string())?;
            tokio::fs::rename(&tmp, &self.path).await.map_err(|e| e.to_string())?;
        }
        Ok(newer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: &str, pos: u64, at: u64) -> Entry {
        Entry { id: id.into(), position_ms: pos, duration_ms: 100_000, finished: false, updated_at: at, device: None }
    }

    #[tokio::test]
    async fn last_writer_wins_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("progress.json");
        let s = Store::open(path.clone()).unwrap();
        assert!(s.put(vec![e("a", 10, 5)]).await.unwrap().is_empty());
        let newer = s.put(vec![e("a", 99, 4)]).await.unwrap();
        assert_eq!(newer, vec![e("a", 10, 5)]);
        s.put(vec![e("a", 20, 6), e("b", 1, 7)]).await.unwrap();
        let s = Store::open(path).unwrap();
        assert_eq!(s.get("a").await.unwrap().position_ms, 20);
        assert_eq!(s.since(6).await, vec![e("b", 1, 7)]);
    }
}
