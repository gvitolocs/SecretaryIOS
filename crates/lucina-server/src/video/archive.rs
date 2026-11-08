//! The MyBook archive: `<LUCINA_VIDEOS_DIR>/<YYYY>/<YYYY-MM-DD> <title> [<id>]/`, one folder
//! per video id, and `index.json` (id → folder, note) for dedup and later lookup.
//!
//! The date is the day the owner shared the video (not the upload date), in the owner's time
//! zone. A video that is shared again gets its old folder back, found through the index or,
//! when the index lost it, by the ` [<id>]` suffix of a folder name.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use super::VideoMeta;

/// Longest title part of a folder name, in characters.
const TITLE_MAX_CHARS: usize = 80;

const INDEX: &str = "index.json";

/// Characters that cannot (or should not) be in a folder name on the MyBook disk.
const FORBIDDEN: &str = "/\\:*?\"<>|";

pub struct Archive {
    base: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub url: String,
    /// Absolute path of the video's folder.
    pub folder: PathBuf,
    /// The Keep note with the report, once there is one.
    pub note_slug: Option<String>,
    /// started | done
    pub status: String,
    pub updated_at: String,
}

impl Entry {
    /// The job began: the folder exists, the report does not.
    pub fn started(meta: &VideoMeta, folder: &Path) -> Self {
        Self {
            id: meta.id.clone(),
            title: meta.title.clone(),
            channel: meta.channel.clone(),
            url: meta.webpage_url.clone(),
            folder: folder.to_path_buf(),
            note_slug: None,
            status: "started".into(),
            updated_at: lucina_store::timeutil::now_iso(),
        }
    }

    /// The report is saved as the Keep note `note_slug`.
    pub fn finished(meta: &VideoMeta, folder: &Path, note_slug: &str) -> Self {
        Self {
            note_slug: Some(note_slug.to_owned()),
            status: "done".into(),
            ..Self::started(meta, folder)
        }
    }
}

/// One writer at a time inside this process (the read-modify-write of `index.json`).
fn index_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Default::default)
}

/// A folder-safe title: forbidden characters and control characters dropped, whitespace
/// collapsed, dots and spaces trimmed, at most 80 characters. Empty becomes `fallback`.
pub fn safe_title(title: &str, fallback: &str) -> String {
    let spaced: String = title
        .chars()
        .filter(|c| !FORBIDDEN.contains(*c))
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect();
    let collapsed = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped: String = collapsed.chars().take(TITLE_MAX_CHARS).collect();
    let trimmed = capped.trim_matches(|c| c == '.' || c == ' ');
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `2026-10-28 Title [id]`.
fn folder_name(meta: &VideoMeta, day: &str) -> String {
    let id = safe_title(&meta.id, "video");
    format!("{day} {} [{id}]", safe_title(&meta.title, &id))
}

impl Archive {
    pub fn new(base: &Path) -> Self {
        Self {
            base: base.to_path_buf(),
        }
    }

    fn index_path(&self) -> PathBuf {
        self.base.join(INDEX)
    }

    /// The index, and whether the file was there but unreadable.
    fn read_index(&self) -> (BTreeMap<String, Entry>, bool) {
        let text = match std::fs::read_to_string(self.index_path()) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (BTreeMap::new(), false),
            Err(e) => {
                tracing::warn!(error = %e, "video archive index unreadable");
                return (BTreeMap::new(), true);
            }
        };
        match serde_json::from_str(&text) {
            Ok(map) => (map, false),
            Err(e) => {
                tracing::warn!(error = %e, "video archive index is corrupt");
                (BTreeMap::new(), true)
            }
        }
    }

    /// The index entry for a video id.
    pub fn find(&self, id: &str) -> Option<Entry> {
        self.read_index().0.remove(id)
    }

    /// The folder of a video id that is already in the archive (index first, then the
    /// ` [<id>]` suffix of a folder name under any year).
    fn existing_folder(&self, id: &str) -> Option<PathBuf> {
        if let Some(e) = self.find(id) {
            if e.folder.is_dir() {
                return Some(e.folder);
            }
        }
        let suffix = format!(" [{}]", safe_title(id, "video"));
        for year in std::fs::read_dir(&self.base).ok()?.flatten() {
            if !year.path().is_dir() {
                continue;
            }
            let Ok(children) = std::fs::read_dir(year.path()) else {
                continue;
            };
            for child in children.flatten() {
                let name = child.file_name();
                if child.path().is_dir() && name.to_string_lossy().ends_with(&suffix) {
                    return Some(child.path());
                }
            }
        }
        None
    }

    /// The video's folder: the existing one for this id, else a new
    /// `<YYYY>/<YYYY-MM-DD> <title> [<id>]` for the day `now` falls on in `tz`. The base
    /// directory is created only when its parent exists (the MyBook mount); a missing mount is
    /// an error rather than a silent write to the system disk.
    pub fn folder_for(
        &self,
        meta: &VideoMeta,
        now: DateTime<Utc>,
        tz: Tz,
    ) -> anyhow::Result<PathBuf> {
        if meta.id.trim().is_empty() {
            anyhow::bail!("the video has no id");
        }
        self.ensure_base()?;
        if let Some(found) = self.existing_folder(&meta.id) {
            return Ok(found);
        }
        let local = now.with_timezone(&tz);
        let year = local.format("%Y").to_string();
        let day = local.format("%Y-%m-%d").to_string();
        let folder = self.base.join(year).join(folder_name(meta, &day));
        std::fs::create_dir_all(&folder)
            .with_context(|| format!("creating {}", folder.display()))?;
        Ok(folder)
    }

    fn ensure_base(&self) -> anyhow::Result<()> {
        if self.base.is_dir() {
            return Ok(());
        }
        let parent_exists = self.base.parent().is_some_and(|p| p.is_dir());
        if !parent_exists {
            return Err(anyhow!(
                "the MyBook archive is not mounted ({} does not exist)",
                self.base.display()
            ));
        }
        std::fs::create_dir(&self.base)
            .with_context(|| format!("creating the archive directory {}", self.base.display()))
    }

    /// Saves `e` in the index under its id (atomic: temp file + rename). A corrupt index is
    /// kept as `index.json.bak` and replaced by a fresh one. A `started` entry keeps the note
    /// of an earlier entry for the same id, so a redo that fails midway does not lose the
    /// report it started from; `finished` sets the new slug.
    pub fn put(&self, mut e: Entry) -> anyhow::Result<()> {
        let _guard = index_lock().lock().unwrap_or_else(|p| p.into_inner());
        self.ensure_base()?;
        let (mut map, corrupt) = self.read_index();
        if corrupt {
            let bak = self.base.join("index.json.bak");
            if let Err(err) = std::fs::copy(self.index_path(), &bak) {
                tracing::warn!(error = %err, "could not back up the corrupt video index");
            }
        }
        if e.note_slug.is_none() {
            e.note_slug = map.get(&e.id).and_then(|old| old.note_slug.clone());
        }
        map.insert(e.id.clone(), e);
        let json = serde_json::to_vec_pretty(&map)?;
        let tmp = self
            .base
            .join(format!(".index.json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, self.index_path()).with_context(|| "replacing the video index")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn meta(id: &str, title: &str) -> VideoMeta {
        VideoMeta {
            id: id.into(),
            title: title.into(),
            channel: "Chan".into(),
            webpage_url: format!("https://www.youtube.com/watch?v={id}"),
            ..VideoMeta::default()
        }
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 28, 23, 30, 0).unwrap()
    }

    #[test]
    fn safe_title_rules() {
        assert_eq!(
            safe_title("AC/DC: Back <in> \"Black\"?", "id"),
            "ACDC Back in Black"
        );
        assert_eq!(safe_title("  a \t\n b   c  ", "id"), "a b c");
        assert_eq!(safe_title("..hidden..", "id"), "hidden");
        assert_eq!(safe_title("///", "abc123"), "abc123");
        assert_eq!(safe_title("tab\u{7}bell", "id"), "tabbell");
        assert_eq!(
            safe_title("Rust 🦀 e la città più bella", "id"),
            "Rust 🦀 e la città più bella"
        );
        let long = "è".repeat(200);
        let cut = safe_title(&long, "id");
        assert_eq!(cut.chars().count(), 80);
        assert!(cut.chars().all(|c| c == 'è'));
        let dotted = format!("{}.{}", "a".repeat(79), "b".repeat(20));
        assert_eq!(safe_title(&dotted, "id"), "a".repeat(79));
    }

    #[test]
    fn new_folder_uses_the_owner_day_and_year() {
        let dir = tempfile::tempdir().unwrap();
        let a = Archive::new(dir.path());
        let m = meta("FBs--2nSPLQ", "I challenged Rust Developers");
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let f = a.folder_for(&m, now(), tz).unwrap();
        // 23:30 UTC is already the 29th in Rome.
        assert_eq!(
            f,
            dir.path()
                .join("2026")
                .join("2026-10-29 I challenged Rust Developers [FBs--2nSPLQ]")
        );
        assert!(f.is_dir());
        let utc = a
            .folder_for(&meta("zzz", "x"), now(), "UTC".parse().unwrap())
            .unwrap();
        assert!(utc.ends_with("2026/2026-10-28 x [zzz]"));
    }

    #[test]
    fn empty_title_falls_back_to_the_id() {
        let dir = tempfile::tempdir().unwrap();
        let a = Archive::new(dir.path());
        let f = a
            .folder_for(&meta("abc", "///"), now(), chrono_tz::UTC)
            .unwrap();
        assert!(f.ends_with("2026-10-28 abc [abc]"), "{f:?}");
    }

    #[test]
    fn an_existing_folder_is_reused_by_index_and_by_scan() {
        let dir = tempfile::tempdir().unwrap();
        let a = Archive::new(dir.path());
        let m = meta("vid1", "First title");
        let first = a.folder_for(&m, now(), chrono_tz::UTC).unwrap();

        // Found by the folder name alone (no index yet), even on another day and title.
        let later = Utc.with_ymd_and_hms(2027, 1, 2, 9, 0, 0).unwrap();
        let again = a
            .folder_for(&meta("vid1", "Renamed title"), later, chrono_tz::UTC)
            .unwrap();
        assert_eq!(first, again);

        // Found by the index, even when the folder was renamed by hand elsewhere.
        let moved = dir.path().join("2026").join("my own name");
        std::fs::rename(&first, &moved).unwrap();
        a.put(Entry::started(&m, &moved)).unwrap();
        assert_eq!(a.folder_for(&m, later, chrono_tz::UTC).unwrap(), moved);

        // A different id gets its own folder.
        let other = a
            .folder_for(&meta("vid2", "First title"), now(), chrono_tz::UTC)
            .unwrap();
        assert_ne!(other, moved);
    }

    #[test]
    fn missing_mount_is_a_clear_error_but_a_missing_leaf_is_created() {
        let dir = tempfile::tempdir().unwrap();
        let unmounted = Archive::new(&dir.path().join("mnt").join("mybook").join("lucina-videos"));
        let e = unmounted
            .folder_for(&meta("a", "b"), now(), chrono_tz::UTC)
            .unwrap_err();
        assert!(
            e.to_string().contains("MyBook archive is not mounted"),
            "{e}"
        );

        let leaf = dir.path().join("lucina-videos");
        let a = Archive::new(&leaf);
        let f = a
            .folder_for(&meta("a", "b"), now(), chrono_tz::UTC)
            .unwrap();
        assert!(f.starts_with(&leaf) && f.is_dir());
    }

    #[test]
    fn index_roundtrip_and_note_kept_by_a_redo() {
        let dir = tempfile::tempdir().unwrap();
        let a = Archive::new(dir.path());
        let m = meta("vid1", "T");
        let folder = a.folder_for(&m, now(), chrono_tz::UTC).unwrap();
        assert!(a.find("vid1").is_none());

        a.put(Entry::started(&m, &folder)).unwrap();
        let e = a.find("vid1").unwrap();
        assert_eq!((e.status.as_str(), e.note_slug.clone()), ("started", None));
        assert_eq!(e.folder, folder);
        assert_eq!(e.url, m.webpage_url);
        assert!(!e.updated_at.is_empty());

        a.put(Entry::finished(&m, &folder, "youtube-t")).unwrap();
        let e = a.find("vid1").unwrap();
        assert_eq!(
            (e.status.as_str(), e.note_slug.as_deref()),
            ("done", Some("youtube-t"))
        );

        // A redo starts again but still knows the old note until the new one is saved.
        a.put(Entry::started(&m, &folder)).unwrap();
        let e = a.find("vid1").unwrap();
        assert_eq!(
            (e.status.as_str(), e.note_slug.as_deref()),
            ("started", Some("youtube-t"))
        );
        a.put(Entry::finished(&m, &folder, "youtube-t-2")).unwrap();
        assert_eq!(
            a.find("vid1").unwrap().note_slug.as_deref(),
            Some("youtube-t-2")
        );

        // Other ids are untouched, and no temp file is left behind.
        a.put(Entry::started(&meta("vid2", "U"), &folder)).unwrap();
        assert!(a.find("vid1").is_some() && a.find("vid2").is_some());
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn corrupt_index_starts_empty_and_keeps_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(INDEX), "{ not json").unwrap();
        let a = Archive::new(dir.path());
        assert!(a.find("x").is_none());
        let m = meta("x", "T");
        a.put(Entry::started(&m, dir.path())).unwrap();
        assert_eq!(a.find("x").unwrap().id, "x");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("index.json.bak")).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn concurrent_puts_do_not_lose_entries() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().to_path_buf();
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let base = base.clone();
                std::thread::spawn(move || {
                    let a = Archive::new(&base);
                    a.put(Entry::started(&meta(&format!("v{i}"), "T"), &base))
                        .unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let a = Archive::new(&base);
        for i in 0..8 {
            assert!(a.find(&format!("v{i}")).is_some(), "v{i} lost");
        }
    }
}
