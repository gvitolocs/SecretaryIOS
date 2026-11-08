//! The owner's local bookmarks (the core has no login, so "bookmark" is a list kept here): one
//! JSON file per owner under the data directory, mode 0600, never shared. The owner id comes
//! from the service's configuration, not from the request.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::model::Summary;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub illust: Summary,
    pub added_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct OwnerData {
    bookmarks: BTreeMap<String, Bookmark>,
}

pub struct Store {
    path: PathBuf,
    data: Mutex<OwnerData>,
    settings: Mutex<Settings>,
}

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
        let settings = match std::fs::read(dir.join("settings.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(std::io::Error::other)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => return Err(e),
        };
        Ok(Store {
            settings: Mutex::new(settings),
            path,
            data: Mutex::new(data),
        })
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn update_settings(&self, patch: &serde_json::Value) -> Result<Settings, SettingsError> {
        let fields = patch.as_object().ok_or(SettingsError::Invalid)?;
        let mut slot = self.settings.lock().unwrap();
        let mut next = slot.clone();
        for (key, value) in fields {
            let s = value.as_str().ok_or(SettingsError::Invalid)?;
            match key.as_str() {
                "defaultContent" if matches!(s, "safe" | "all" | "r18") => {
                    next.default_content = s.into()
                }
                "defaultSort" if matches!(s, "date_d" | "popular") => next.default_sort = s.into(),
                _ => return Err(SettingsError::Invalid),
            }
        }
        let path = self.path.parent().unwrap().join("settings.json");
        let tmp = path.with_extension("tmp");
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)?;
            file.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
            file.write_all(&serde_json::to_vec(&next)?)?;
            file.sync_all()?;
            std::fs::rename(&tmp, &path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return Err(SettingsError::Io);
        }
        *slot = next.clone();
        Ok(next)
    }

    fn save(&self, data: &OwnerData) {
        let Ok(bytes) = serde_json::to_vec(data) else {
            return;
        };
        let tmp = self.path.with_extension("tmp");
        let written = {
            use std::io::Write;
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            o.open(&tmp).and_then(|mut f| f.write_all(&bytes))
        };
        if written.is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        } else {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    pub fn list(&self) -> Vec<Bookmark> {
        let mut v: Vec<Bookmark> = self
            .data
            .lock()
            .unwrap()
            .bookmarks
            .values()
            .cloned()
            .collect();
        v.sort_by_key(|b| std::cmp::Reverse(b.added_at));
        v
    }

    pub fn contains(&self, id: &str) -> bool {
        self.data.lock().unwrap().bookmarks.contains_key(id)
    }

    pub fn add(&self, illust: Summary) {
        let mut d = self.data.lock().unwrap();
        let added_at = d
            .bookmarks
            .get(&illust.id)
            .map(|b| b.added_at)
            .unwrap_or_else(|| chrono::Utc::now().timestamp());
        d.bookmarks
            .insert(illust.id.clone(), Bookmark { illust, added_at });
        self.save(&d);
    }

    pub fn remove(&self, id: &str) {
        let mut d = self.data.lock().unwrap();
        if d.bookmarks.remove(id).is_some() {
            self.save(&d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sum(id: &str) -> Summary {
        Summary {
            id: id.into(),
            title: "t".into(),
            author: "a".into(),
            author_id: "1".into(),
            tags: vec![],
            thumb: "https://i.pximg.net/x.jpg".into(),
            pages: 1,
            r18: false,
        }
    }

    #[test]
    fn persists_with_0600() {
        let dir = tempfile::tempdir().unwrap();
        {
            let s = Store::open(dir.path(), "owner").unwrap();
            s.add(sum("1"));
            s.add(sum("2"));
            s.remove("1");
        }
        let s = Store::open(dir.path(), "owner").unwrap();
        assert!(!s.contains("1"));
        assert!(s.contains("2"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("owner.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub default_content: String,
    pub default_sort: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            default_content: "safe".into(),
            default_sort: "date_d".into(),
        }
    }
}
#[derive(Debug)]
pub enum SettingsError {
    Invalid,
    Io,
}
