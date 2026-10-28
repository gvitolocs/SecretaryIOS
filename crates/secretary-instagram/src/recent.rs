//! The profiles the owner opened, newest first, kept server side in a 0600 file.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const MAX_RECENT: usize = 30;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RecentEntry {
    pub username: String,
    pub full_name: String,
    /// The Instagram CDN URL of the profile picture (signed again on every read).
    pub pic_cdn: Option<String>,
    pub verified: bool,
    pub opened_at: i64,
}

pub struct Recent {
    path: PathBuf,
    entries: Mutex<Vec<RecentEntry>>,
}

impl Recent {
    pub fn open(path: PathBuf) -> Recent {
        let entries = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Recent {
            path,
            entries: Mutex::new(entries),
        }
    }

    pub fn list(&self) -> Vec<RecentEntry> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn add(&self, entry: RecentEntry) {
        let mut g = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|e| e.username != entry.username);
        g.insert(0, entry);
        g.truncate(MAX_RECENT);
        self.save(&g);
    }

    pub fn remove(&self, username: &str) {
        let mut g = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|e| e.username != username);
        self.save(&g);
    }

    fn save(&self, entries: &[RecentEntry]) {
        let Ok(bytes) = serde_json::to_vec(entries) else {
            return;
        };
        let tmp = self.path.with_extension("tmp");
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
                if f.write_all(&bytes).is_ok() {
                    let _ = std::fs::rename(&tmp, &self.path);
                }
            }
        }
        #[cfg(not(unix))]
        {
            if std::fs::write(&tmp, &bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(u: &str, t: i64) -> RecentEntry {
        RecentEntry {
            username: u.into(),
            full_name: String::new(),
            pic_cdn: None,
            verified: false,
            opened_at: t,
        }
    }

    #[test]
    fn newest_first_deduplicated_capped_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("recent.json");
        let r = Recent::open(p.clone());
        for i in 0..40 {
            r.add(e(&format!("u{i}"), i));
        }
        r.add(e("u5", 99));
        let l = r.list();
        assert_eq!(l.len(), MAX_RECENT);
        assert_eq!(l[0].username, "u5");
        assert_eq!(l.iter().filter(|x| x.username == "u5").count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(Recent::open(p).list(), l);
    }
}
