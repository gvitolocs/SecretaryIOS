//! Favorites and reading progress, per owner, in one JSON file each under the data directory
//! (`<owner>.json`, 0600). Never shared with other users: the owner id comes from the
//! service's configuration, not from the request.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::service::Summary;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Favorite {
    pub gallery: Summary,
    pub added_at: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Progress {
    /// Zero-based page last shown.
    pub page: i32,
    pub pages: i32,
    pub updated_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OwnerData {
    favorites: BTreeMap<i32, Favorite>,
    progress: BTreeMap<i32, Progress>,
}

pub struct Store {
    path: PathBuf,
    data: Mutex<OwnerData>,
}

const MAX_PROGRESS: usize = 2000;

impl Store {
    pub fn open(dir: &std::path::Path, owner: &str) -> std::io::Result<Store> {
        std::fs::create_dir_all(dir)?;
        let safe: String = owner
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        let path = dir.join(format!(
            "{}.json",
            if safe.is_empty() { "owner" } else { &safe }
        ));
        let data = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Ok(Store {
            path,
            data: Mutex::new(data),
        })
    }

    fn save(&self, data: &OwnerData) {
        let Ok(bytes) = serde_json::to_vec(data) else {
            return;
        };
        let tmp = self.path.with_extension("tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
            }
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    pub fn favorites(&self) -> Vec<Favorite> {
        let mut v: Vec<Favorite> = self
            .data
            .lock()
            .unwrap()
            .favorites
            .values()
            .cloned()
            .collect();
        v.sort_by_key(|f| std::cmp::Reverse(f.added_at));
        v
    }

    pub fn is_favorite(&self, id: i32) -> bool {
        self.data.lock().unwrap().favorites.contains_key(&id)
    }

    pub fn add_favorite(&self, gallery: Summary) {
        let mut d = self.data.lock().unwrap();
        let added_at = d
            .favorites
            .get(&gallery.id)
            .map(|f| f.added_at)
            .unwrap_or_else(|| chrono::Utc::now().timestamp());
        d.favorites
            .insert(gallery.id, Favorite { gallery, added_at });
        self.save(&d);
    }

    pub fn remove_favorite(&self, id: i32) {
        let mut d = self.data.lock().unwrap();
        if d.favorites.remove(&id).is_some() {
            self.save(&d);
        }
    }

    pub fn progress(&self, id: i32) -> Option<Progress> {
        self.data.lock().unwrap().progress.get(&id).copied()
    }

    pub fn all_progress(&self) -> BTreeMap<i32, Progress> {
        self.data.lock().unwrap().progress.clone()
    }

    pub fn set_progress(&self, id: i32, page: i32, pages: i32) {
        let mut d = self.data.lock().unwrap();
        d.progress.insert(
            id,
            Progress {
                page: page.max(0),
                pages: pages.max(0),
                updated_at: chrono::Utc::now().timestamp(),
            },
        );
        if d.progress.len() > MAX_PROGRESS {
            if let Some((&oldest, _)) = d.progress.iter().min_by_key(|(_, p)| p.updated_at) {
                d.progress.remove(&oldest);
            }
        }
        self.save(&d);
    }

    /// A gallery the filter now drops disappears from the owner's lists too.
    pub fn forget(&self, id: i32) {
        let mut d = self.data.lock().unwrap();
        let a = d.favorites.remove(&id).is_some();
        let b = d.progress.remove(&id).is_some();
        if a || b {
            self.save(&d);
        }
    }
}
