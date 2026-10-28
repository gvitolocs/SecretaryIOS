//! What was saved to the Gallery, per account (the plugin's "Archive" tab): an index of
//! lucina-media item ids. The pictures and videos live in the Gallery, not here.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    /// lucina-media's item id.
    pub media_id: String,
    /// Instagram's id for the story or post, when known (auto-saves).
    pub ig_id: Option<String>,
    pub kind: String,
    pub mime: String,
    pub taken_at: Option<i64>,
    pub saved_at: i64,
    pub caption: Option<String>,
    /// Saved by the watcher, not by the owner's tap.
    pub auto: bool,
}

pub struct Archive {
    path: Option<PathBuf>,
    users: Mutex<BTreeMap<String, Vec<Saved>>>,
}

impl Archive {
    pub fn open(path: Option<PathBuf>) -> Archive {
        let users = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Archive {
            path,
            users: Mutex::new(users),
        }
    }

    /// Record a saved item (newest first). A media id already listed is not listed twice.
    pub fn add(&self, user: &str, item: Saved) {
        let mut g = self.users.lock().unwrap_or_else(|e| e.into_inner());
        let list = g.entry(user.to_string()).or_default();
        if list.iter().any(|s| s.media_id == item.media_id) {
            return;
        }
        list.insert(0, item);
        if let (Some(path), Ok(bytes)) = (&self.path, serde_json::to_vec(&*g)) {
            crate::cache::write_private_atomic(path, &bytes);
        }
    }

    /// A page of `limit` items from `offset` (newest saved first), and the next offset.
    pub fn page(&self, user: &str, offset: usize, limit: usize) -> (Vec<Saved>, Option<usize>) {
        let g = self.users.lock().unwrap_or_else(|e| e.into_inner());
        let list = g.get(user).map(Vec::as_slice).unwrap_or(&[]);
        let items: Vec<Saved> = list.iter().skip(offset).take(limit).cloned().collect();
        let next = (offset + items.len() < list.len()).then_some(offset + items.len());
        (items, next)
    }

    pub fn count(&self, user: &str) -> usize {
        self.users
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(user)
            .map_or(0, Vec::len)
    }

    /// The saved item with this lucina-media id, for serving it back.
    pub fn find(&self, media_id: &str) -> Option<Saved> {
        let g = self.users.lock().unwrap_or_else(|e| e.into_inner());
        g.values()
            .flatten()
            .find(|s| s.media_id == media_id)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(id: &str, at: i64) -> Saved {
        Saved {
            media_id: id.into(),
            ig_id: None,
            kind: "image".into(),
            mime: "image/jpeg".into(),
            taken_at: None,
            saved_at: at,
            caption: None,
            auto: true,
        }
    }

    #[test]
    fn newest_first_paged_unique_and_persistent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("archive.json");
        let a = Archive::open(Some(p.clone()));
        for i in 0..5 {
            a.add("x", saved(&format!("m{i}"), i));
        }
        a.add("x", saved("m2", 99));
        assert_eq!(a.count("x"), 5);
        let (first, next) = a.page("x", 0, 2);
        assert_eq!(
            first
                .iter()
                .map(|s| s.media_id.as_str())
                .collect::<Vec<_>>(),
            ["m4", "m3"]
        );
        assert_eq!(next, Some(2));
        let (last, next) = a.page("x", 4, 2);
        assert_eq!(last.len(), 1);
        assert_eq!(next, None);
        assert!(a.page("nobody", 0, 2).0.is_empty());
        assert!(Archive::open(Some(p)).find("m1").is_some());
    }
}
