//! What the Gallery needs beyond a plain listing: where an item came from (`origin`), the source
//! switcher (`GET /v1/media/sources`) and the `origin=` filter of the item listing.
//!
//! The decisions are small pure functions: the rule that groups a `source` string as chat or
//! Lucina, the origin filter as SQL, the provenance an item shows, and the relative paths behind
//! it. Provenance never shows an absolute host path: imports record the folder under the import
//! root, in place folders the folder under the configured root.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Serialize;

use crate::db::{self, DeviceOut};
use crate::drive::DriveInfo;

/// `items.source` prefixes of photos that came through chat or a share sheet.
pub const CHAT_PREFIXES: &[&str] = &[
    "ios-chat",
    "ios-share",
    "web-chat",
    "web-share",
    "chat",
    "telegram",
    "discord",
];

/// `items.source` prefix of what Lucina made or found herself.
pub const LUCINA_PREFIX: &str = "lucina";

/// [`CHAT_PREFIXES`] as SQL on `items i`, photos and videos only: a document shared from a phone is
/// a Drive file, not a Gallery picture (a test keeps this and the Rust rule in step).
pub const CHAT_SQL: &str = "(i.kind IN ('photo', 'video') AND (i.source LIKE 'ios-chat%' OR i.source LIKE 'ios-share%' OR i.source LIKE 'web-chat%' OR i.source LIKE 'web-share%' OR i.source LIKE 'chat%' OR i.source LIKE 'telegram%' OR i.source LIKE 'discord%'))";

/// [`LUCINA_PREFIX`] as SQL on `items i`, photos and videos only.
pub const LUCINA_SQL: &str = "(i.kind IN ('photo', 'video') AND i.source LIKE 'lucina%')";

/// A source group that is not a device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceGroup {
    Chat,
    Lucina,
}

/// Which group a `source` string belongs to. SQL's `LIKE` ignores ASCII case, so this does too.
pub fn source_group(source: &str) -> Option<SourceGroup> {
    let s = source.to_ascii_lowercase();
    if CHAT_PREFIXES.iter().any(|p| s.starts_with(p)) {
        Some(SourceGroup::Chat)
    } else if s.starts_with(LUCINA_PREFIX) {
        Some(SourceGroup::Lucina)
    } else {
        None
    }
}

/// The Gallery's source switcher value (`items?origin=`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Everything in the Gallery: a device, chat or Lucina (`db::FROM_DEVICE`), not the Drive.
    All,
    Device(String),
    Chat,
    Lucina,
}

impl Origin {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "all" => Some(Self::All),
            "chat" => Some(Self::Chat),
            "lucina" => Some(Self::Lucina),
            _ => s
                .strip_prefix("device:")
                .filter(|d| !d.is_empty() && d.len() <= 64)
                .map(|d| Self::Device(d.to_string())),
        }
    }
}

/// `origin` as a SQL condition on `items i`; a device id goes into `args` (numbered `?N`). An
/// unknown origin matches nothing (the HTTP layer answers 400 before it gets here).
pub fn origin_condition(origin: &str, args: &mut Vec<rusqlite::types::Value>) -> String {
    match Origin::parse(origin) {
        Some(Origin::All) => db::FROM_DEVICE.to_string(),
        Some(Origin::Chat) => CHAT_SQL.to_string(),
        Some(Origin::Lucina) => LUCINA_SQL.to_string(),
        Some(Origin::Device(id)) => {
            args.push(id.into());
            format!("i.device = ?{}", args.len())
        }
        None => "0".to_string(),
    }
}

// ---- provenance ---------------------------------------------------------------------------

/// What the catalog stored about where a file came from (`items.origin_*`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OriginCols {
    /// The original file name.
    pub name: Option<String>,
    /// The folder it was in, relative to the import root.
    pub folder: Option<String>,
    /// The import root's own name (a folder or a Takeout zip), or the configured folder's label.
    pub root: Option<String>,
}

/// `origin` in the item JSON.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct OriginOut {
    pub filename: Option<String>,
    pub folder: Option<String>,
    pub imported_from: Option<String>,
}

/// `source` values of bulk imports: the item came from a folder somebody imported.
const IMPORT_SOURCES: &[&str] = &["import", "nextcloud", "takeout"];

/// Whether a title reads as a file name: `IMG_0001.JPG`, not `Beach day`.
fn looks_like_filename(t: &str) -> bool {
    match t.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && (1..=5).contains(&ext.len())
                && ext.bytes().all(|b| b.is_ascii_alphanumeric())
                && !t.contains('/')
        }
        None => false,
    }
}

/// The provenance an item shows. A Drive file says so from its Drive info; everything else from
/// what was stored when it arrived. Items stored before provenance was recorded fall back to a
/// title that reads as a file name, and to the import source (`takeout`, `nextcloud`, `import`).
pub fn origin_of(
    cols: &OriginCols,
    title: &str,
    source: &str,
    drive: Option<&DriveInfo>,
) -> OriginOut {
    if let Some(d) = drive {
        return OriginOut {
            filename: Some(d.name.clone()),
            folder: (!d.dir.is_empty()).then(|| d.dir.clone()),
            imported_from: Some(d.folder.clone()),
        };
    }
    OriginOut {
        filename: cols
            .name
            .clone()
            .or_else(|| looks_like_filename(title).then(|| title.to_string())),
        folder: cols.folder.clone(),
        imported_from: cols
            .root
            .clone()
            .or_else(|| IMPORT_SOURCES.contains(&source).then(|| source.to_string())),
    }
}

/// A folder as the client or an import states it, made safe to show: `/` separators, no `.` or empty
/// segments. Absolute paths, `..` and NUL are refused (`None`), so a host path never gets in.
pub fn clean_folder(s: &str) -> Option<String> {
    let s = s.replace('\\', "/");
    if s.starts_with('/')
        || s.as_bytes().get(1) == Some(&b':')
        || s.contains('\0')
        || s.len() > 1024
    {
        return None;
    }
    let segs: Vec<&str> = s
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if segs.is_empty() || segs.contains(&"..") {
        return None;
    }
    Some(segs.join("/"))
}

/// A single name (an import root's, a file's): no separators, not empty, bounded.
pub fn clean_name(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty() && s.len() <= 255 && !s.contains(['/', '\\', '\0'])).then(|| s.to_string())
}

/// `path` under one of the configured folder roots `(label, root)`: the label and the folder
/// between the root and the file (`""` at the top, so `None`). `None` when no root holds it.
pub fn relative_to_roots(
    roots: &[(String, PathBuf)],
    path: &Path,
) -> Option<(String, Option<String>)> {
    roots.iter().find_map(|(label, root)| {
        let rel = crate::drive_kernel::rel_of(root, path)?;
        let (dir, _) = crate::drive_kernel::split_rel(&rel);
        Some((label.clone(), (!dir.is_empty()).then(|| dir.to_string())))
    })
}

/// The folder of `file` under the import `root`, as the catalog spells it, and the root's own name.
pub fn import_origin(root: &Path, file: &Path) -> (Option<String>, Option<String>) {
    let folder = file
        .strip_prefix(root)
        .ok()
        .and_then(Path::parent)
        .and_then(|p| clean_folder(&p.to_string_lossy()));
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(clean_name);
    (folder, name)
}

// ---- the source switcher -----------------------------------------------------------------

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SourceOut {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub photos: i64,
    pub videos: i64,
    pub latest_taken_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AllOut {
    pub photos: i64,
    pub videos: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Sources {
    pub sources: Vec<SourceOut>,
    pub all: AllOut,
}

/// Photos, videos and the latest taken date of the visible items matching `cond` (on `items i`).
fn count_where(c: &Connection, cond: &str) -> rusqlite::Result<(i64, i64, Option<String>)> {
    c.query_row(
        &format!(
            "SELECT COALESCE(SUM(i.kind = 'photo'), 0), COALESCE(SUM(i.kind = 'video'), 0), MAX(i.taken_at) \
             FROM items i WHERE i.trashed_at IS NULL AND i.hidden = 0 AND i.archived = 0 AND {cond}"
        ),
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
}

/// The registered devices (most recently seen first), then Chat & shares and Lucina when they hold
/// anything, and the Gallery's total.
pub fn list_sources(c: &Connection) -> rusqlite::Result<Sources> {
    let mut sources: Vec<SourceOut> = db::list_devices(c)?
        .into_iter()
        .map(|d: DeviceOut| SourceOut {
            id: format!("device:{}", d.id),
            label: d.name,
            kind: d.kind,
            photos: d.photos,
            videos: d.videos,
            latest_taken_at: d.latest_taken_at,
        })
        .collect();
    for (id, label, cond) in [
        ("chat", "Chat & shares", CHAT_SQL),
        ("lucina", "Lucina", LUCINA_SQL),
    ] {
        let (photos, videos, latest) = count_where(c, cond)?;
        if photos + videos > 0 {
            sources.push(SourceOut {
                id: id.into(),
                label: label.into(),
                kind: id.into(),
                photos,
                videos,
                latest_taken_at: latest,
            });
        }
    }
    let (photos, videos, _) = count_where(c, db::FROM_DEVICE)?;
    Ok(Sources {
        sources,
        all: AllOut { photos, videos },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_group_as_chat_or_lucina() {
        for s in [
            "ios-chat",
            "ios-share",
            "web-chat",
            "web-share",
            "chat",
            "telegram",
            "discord",
            "iOS-Chat-2",
        ] {
            assert_eq!(source_group(s), Some(SourceGroup::Chat), "{s}");
        }
        assert_eq!(source_group("lucina"), Some(SourceGroup::Lucina));
        assert_eq!(source_group("lucina-pick"), Some(SourceGroup::Lucina));
        for s in [
            "ios-selftest-1",
            "phone-ios",
            "folder",
            "upload",
            "takeout",
            "",
        ] {
            assert_eq!(source_group(s), None, "{s}");
        }
    }

    #[test]
    fn the_sql_groups_agree_with_the_rust_rule() {
        let c = Connection::open_in_memory().unwrap();
        for s in [
            "ios-chat",
            "ios-share-x",
            "web-chat",
            "web-share",
            "chat",
            "chatgpt",
            "telegram",
            "discord",
            "lucina",
            "lucina-picks",
            "ios-selftest-9",
            "phone-ios",
            "folder",
            "upload",
            "mac",
            "takeout",
        ] {
            for kind in ["photo", "video", "audio", "file"] {
                let q = |cond: &str| -> bool {
                    c.query_row(
                        &format!(
                            "SELECT {cond} FROM (SELECT ?1 AS source, ?2 AS kind, NULL AS device) i"
                        ),
                        [s, kind],
                        |r| r.get(0),
                    )
                    .unwrap()
                };
                let media = matches!(kind, "photo" | "video");
                let group = source_group(s);
                let chat = media && group == Some(SourceGroup::Chat);
                let lucina = media && group == Some(SourceGroup::Lucina);
                assert_eq!(q(CHAT_SQL), chat, "chat {s} {kind}");
                assert_eq!(q(LUCINA_SQL), lucina, "lucina {s} {kind}");
                assert_eq!(
                    q(db::FROM_DEVICE),
                    chat || lucina || s.starts_with("phone"),
                    "gallery {s} {kind}"
                );
            }
        }
    }

    #[test]
    fn origin_values_parse() {
        assert_eq!(Origin::parse("all"), Some(Origin::All));
        assert_eq!(Origin::parse("chat"), Some(Origin::Chat));
        assert_eq!(Origin::parse("lucina"), Some(Origin::Lucina));
        assert_eq!(
            Origin::parse("device:ios-1"),
            Some(Origin::Device("ios-1".into()))
        );
        assert_eq!(Origin::parse("device:"), None);
        assert_eq!(Origin::parse("device"), None);
        assert_eq!(Origin::parse("drive"), None);
        assert_eq!(Origin::parse(""), None);
    }

    #[test]
    fn the_origin_filter_becomes_sql() {
        let mut args = Vec::new();
        assert_eq!(origin_condition("chat", &mut args), CHAT_SQL);
        assert_eq!(origin_condition("lucina", &mut args), LUCINA_SQL);
        assert_eq!(origin_condition("all", &mut args), db::FROM_DEVICE);
        assert!(args.is_empty());
        assert_eq!(origin_condition("device:mac-1", &mut args), "i.device = ?1");
        assert_eq!(args.len(), 1);
        assert_eq!(origin_condition("nonsense", &mut args), "0");
        assert_eq!(args.len(), 1, "an unknown origin binds nothing");
    }

    #[test]
    fn folders_are_relative_and_safe() {
        assert_eq!(clean_folder("DCIM/2019"), Some("DCIM/2019".into()));
        assert_eq!(clean_folder("DCIM\\2019\\"), Some("DCIM/2019".into()));
        assert_eq!(clean_folder("./a//b/."), Some("a/b".into()));
        assert_eq!(clean_folder("/home/nez/Pictures"), None);
        assert_eq!(clean_folder("C:\\Users\\x"), None);
        assert_eq!(clean_folder("a/../b"), None);
        assert_eq!(clean_folder(".."), None);
        assert_eq!(clean_folder(""), None);
        assert_eq!(clean_name("Takeout"), Some("Takeout".into()));
        assert_eq!(clean_name("a/b"), None);
        assert_eq!(clean_name("  "), None);
    }

    #[test]
    fn imports_record_the_folder_under_their_root() {
        let root = Path::new("/mnt/old/Backup");
        assert_eq!(
            import_origin(root, Path::new("/mnt/old/Backup/DCIM/2019/IMG_1.JPG")),
            (Some("DCIM/2019".into()), Some("Backup".into()))
        );
        // A file at the top of the root has no folder.
        assert_eq!(
            import_origin(root, Path::new("/mnt/old/Backup/IMG_2.JPG")),
            (None, Some("Backup".into()))
        );
        // A file outside the root never leaks its path.
        assert_eq!(
            import_origin(root, Path::new("/etc/passwd")),
            (None, Some("Backup".into()))
        );
    }

    #[test]
    fn configured_roots_give_a_label_and_a_relative_folder() {
        let roots = vec![
            ("Film".to_string(), PathBuf::from("/data/film")),
            ("Photos".to_string(), PathBuf::from("/data/photos")),
        ];
        assert_eq!(
            relative_to_roots(&roots, Path::new("/data/photos/2020/trip/a.jpg")),
            Some(("Photos".into(), Some("2020/trip".into())))
        );
        assert_eq!(
            relative_to_roots(&roots, Path::new("/data/film/m.mkv")),
            Some(("Film".into(), None))
        );
        assert_eq!(relative_to_roots(&roots, Path::new("/tmp/x.jpg")), None);
    }

    #[test]
    fn origin_falls_back_for_items_stored_before_provenance() {
        let none = OriginCols::default();
        let o = origin_of(&none, "IMG_0001.JPG", "phone-ios", None);
        assert_eq!(o.filename.as_deref(), Some("IMG_0001.JPG"));
        assert_eq!((o.folder, o.imported_from), (None, None));
        let o = origin_of(&none, "Beach day", "takeout", None);
        assert_eq!(o.filename, None);
        assert_eq!(o.imported_from.as_deref(), Some("takeout"));
        let cols = OriginCols {
            name: Some("a.jpg".into()),
            folder: Some("DCIM/2019".into()),
            root: Some("Backup".into()),
        };
        let o = origin_of(&cols, "Beach day", "import", None);
        assert_eq!(o.filename.as_deref(), Some("a.jpg"));
        assert_eq!(o.folder.as_deref(), Some("DCIM/2019"));
        assert_eq!(o.imported_from.as_deref(), Some("Backup"));
    }

    #[test]
    fn a_drive_file_names_its_drive_folder() {
        let d = DriveInfo {
            folder: "Documents".into(),
            path: "a/b/c.pdf".into(),
            dir: "a/b".into(),
            name: "c.pdf".into(),
            ext: "pdf".into(),
            doc_type: "pdf".into(),
            preview: "pdf".into(),
            language: None,
            modified: String::new(),
            text: "ready".into(),
            row: 1,
        };
        let o = origin_of(&OriginCols::default(), "c", "folder", Some(&d));
        assert_eq!(
            (
                o.filename.as_deref(),
                o.folder.as_deref(),
                o.imported_from.as_deref()
            ),
            (Some("c.pdf"), Some("a/b"), Some("Documents"))
        );
    }

    #[test]
    fn titles_that_read_as_file_names() {
        for t in ["IMG_0001.JPG", "a.b.png", "clip.mov"] {
            assert!(looks_like_filename(t), "{t}");
        }
        for t in ["Beach day", ".hidden", "Trip. Day one", "x/y.jpg", "noext."] {
            assert!(!looks_like_filename(t), "{t}");
        }
    }
}
