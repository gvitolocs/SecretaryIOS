//! The Drive's safety kernel: path guards, the gallery/films/drive split, blob dedupe, the prune
//! guard and the thermal pause. The decisions are pure functions whose bodies live in
//! `drive_kernel_shared.rs`, which `verification/verus/drive_kernels.rs` includes with the
//! contracts kept; the helpers here only split strings and map enums onto them.

use std::path::{Component, Path, PathBuf};

macro_rules! verified_kernel {
    ($(pub fn $name:ident($($arg:ident: $ty:ty),*) -> ($ret:ident: $rty:ty)
       requires {$($pre:tt)*} ensures {$($post:tt)*} proof {$($proof:tt)*} body {$($body:tt)*})*) => {
        $(pub fn $name($($arg: $ty),*) -> $rty { $($body)* })*
    };
}
include!("drive_kernel_shared.rs");

/// The longest path inside a Drive folder the API accepts.
pub const MAX_REL_PATH: usize = 4096;

fn seg_ok(seg: &str) -> bool {
    segment_ok(
        seg.len() as u64,
        seg == ".",
        seg == "..",
        seg.bytes().any(|b| b == b'/' || b == b'\\' || b == 0),
    )
}

/// A path inside a Drive folder as the API and the catalog spell it: `a/b/c.pdf`, or `""` for the
/// folder itself. No leading or trailing `/`, no empty, `.` or `..` segment, no `\` or NUL.
pub fn safe_rel(path: &str) -> bool {
    path.is_empty() || (path.len() <= MAX_REL_PATH && path.split('/').all(seg_ok))
}

/// `root` joined with a safe relative path; `None` when the path is not safe. Lexically the result
/// is always inside `root` (DK1: no segment can climb out).
pub fn join_under(root: &Path, rel: &str) -> Option<PathBuf> {
    if !safe_rel(rel) {
        return None;
    }
    let mut p = root.to_path_buf();
    for seg in rel.split('/').filter(|s| !s.is_empty()) {
        p.push(seg);
    }
    Some(p)
}

/// The catalog's relative path of a file found by the walk under `root`: only plain components,
/// UTF-8, each a safe segment. Anything else (a non-UTF-8 name, a path outside the root) is skipped.
pub fn rel_of(root: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(root).ok()?;
    let mut segs = Vec::new();
    for c in rest.components() {
        match c {
            Component::Normal(s) => {
                let s = s.to_str()?;
                if !seg_ok(s) {
                    return None;
                }
                segs.push(s);
            }
            _ => return None,
        }
    }
    (!segs.is_empty()).then(|| segs.join("/"))
}

/// `(dir, name)` of a safe relative path: `"a/b/c.pdf"` → `("a/b", "c.pdf")`.
pub fn split_rel(rel: &str) -> (&str, &str) {
    rel.rsplit_once('/').unwrap_or(("", rel))
}

/// Whether `dir` is `parent` or below it (catalog dirs, `""` is the folder root).
pub fn dir_within(dir: &str, parent: &str) -> bool {
    parent.is_empty()
        || dir == parent
        || (dir.len() > parent.len()
            && dir.starts_with(parent)
            && dir.as_bytes()[parent.len()] == b'/')
}

/// Whether a file or directory name is junk the Drive leaves out (`.git`, `node_modules`, …).
/// Patterns match a whole name; `*` at the start or the end matches any prefix or suffix.
pub fn ignored(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        if let Some(suffix) = p.strip_prefix('*') {
            name.ends_with(suffix)
        } else if let Some(prefix) = p.strip_suffix('*') {
            name.starts_with(prefix)
        } else {
            name == p
        }
    })
}

/// `LUCINA_MEDIA_DRIVE_IGNORE` when unset.
pub fn default_ignore() -> Vec<String> {
    [
        ".git",
        ".hg",
        ".svn",
        "node_modules",
        "target",
        "__pycache__",
        ".DS_Store",
        "Thumbs.db",
        "desktop.ini",
        ".venv",
        "venv",
        ".mypy_cache",
        ".pytest_cache",
        ".gradle",
        ".idea",
        ".cache",
        ".Trash*",
        "*.pyc",
        "*.o",
        "*.swp",
        "~$*",
        ".~lock*",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// The three places of the library (`ListQuery::scope`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Gallery,
    Films,
    Drive,
}

impl Scope {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "gallery" => Some(Scope::Gallery),
            "films" => Some(Scope::Films),
            "drive" => Some(Scope::Drive),
            _ => None,
        }
    }
    fn code(self) -> u8 {
        match self {
            Scope::Gallery => 0,
            Scope::Films => 1,
            Scope::Drive => 2,
        }
    }
    /// DK2: the scope an item belongs to.
    pub fn of(phone: bool, in_film_folder: bool) -> Self {
        match scope_of(phone, in_film_folder) {
            0 => Scope::Gallery,
            1 => Scope::Films,
            _ => Scope::Drive,
        }
    }
    pub fn contains(self, phone: bool, in_film_folder: bool) -> bool {
        in_scope(self.code(), phone, in_film_folder)
    }
}

/// Where a blob's bytes are (`blobs.location`): owned (`store`, `spool`) or indexed in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobLoc {
    Owned,
    External,
}

impl BlobLoc {
    pub fn from_location(loc: &str) -> Self {
        match loc {
            "store" | "spool" => BlobLoc::Owned,
            _ => BlobLoc::External,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobAction {
    /// No row: place the incoming copy in the store and record it.
    Place,
    /// The library owns these bytes already: the incoming copy goes.
    DropIncoming,
    /// Only an in-place file had these bytes: the incoming copy becomes the owned original.
    Upgrade,
    /// No row: record the in-place file.
    RecordExternal,
    /// Keep the row as it is.
    Keep,
}

/// DK3.
pub fn blob_decision(existing: Option<BlobLoc>, incoming_owned: bool) -> BlobAction {
    let e = match existing {
        None => 0,
        Some(BlobLoc::Owned) => 1,
        Some(BlobLoc::External) => 2,
    };
    match blob_action(e, incoming_owned) {
        0 => BlobAction::Place,
        1 => BlobAction::DropIncoming,
        2 => BlobAction::Upgrade,
        3 => BlobAction::RecordExternal,
        _ => BlobAction::Keep,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_paths_cannot_climb_out() {
        for ok in [
            "",
            "a",
            "a/b/c.pdf",
            "Digital Forensics/L05 slides.pdf",
            "..a",
            "a..",
            ".hidden",
        ] {
            assert!(safe_rel(ok), "{ok:?}");
        }
        for bad in [
            "..", "../x", "a/../b", "a/..", ".", "./a", "a/./b", "/a", "a/", "a//b", "a\\b", "a\0b",
        ] {
            assert!(!safe_rel(bad), "{bad:?}");
            assert!(join_under(Path::new("/r"), bad).is_none());
        }
        assert!(!safe_rel(&"x".repeat(256)));
        assert!(!safe_rel(&"a/".repeat(3000)));
        assert_eq!(
            join_under(Path::new("/r"), "a/b").unwrap(),
            Path::new("/r/a/b")
        );
        assert_eq!(join_under(Path::new("/r"), "").unwrap(), Path::new("/r"));
    }

    #[test]
    fn walk_paths_map_to_catalog_paths() {
        let root = Path::new("/mnt/drive/Lezioni");
        assert_eq!(
            rel_of(root, &root.join("DF/L01.pdf")).as_deref(),
            Some("DF/L01.pdf")
        );
        assert_eq!(rel_of(root, root), None);
        assert_eq!(rel_of(root, Path::new("/elsewhere/x")), None);
        assert_eq!(rel_of(root, &root.join("a/../b")), None);
        assert_eq!(split_rel("a/b/c.pdf"), ("a/b", "c.pdf"));
        assert_eq!(split_rel("c.pdf"), ("", "c.pdf"));
        assert!(dir_within("a/b", "a") && dir_within("a", "a") && dir_within("x", ""));
        assert!(!dir_within("ab", "a") && !dir_within("a", "a/b"));
    }

    #[test]
    fn junk_is_ignored() {
        let p = default_ignore();
        for junk in [
            ".git",
            "node_modules",
            "target",
            "__pycache__",
            ".DS_Store",
            "x.pyc",
            "~$doc.docx",
            ".Trash-1000",
        ] {
            assert!(ignored(junk, &p), "{junk}");
        }
        for keep in ["src", "targets", "notes.md", "git"] {
            assert!(!ignored(keep, &p), "{keep}");
        }
    }

    #[test]
    fn scopes_partition_the_library() {
        for phone in [false, true] {
            for film in [false, true] {
                let s = Scope::of(phone, film);
                let n = [Scope::Gallery, Scope::Films, Scope::Drive]
                    .into_iter()
                    .filter(|x| x.contains(phone, film))
                    .count();
                assert_eq!(n, 1);
                assert!(s.contains(phone, film));
            }
        }
        assert_eq!(Scope::of(true, true), Scope::Gallery);
        assert!(!in_scope(7, false, false));
    }

    #[test]
    fn owned_bytes_are_never_given_up() {
        use BlobAction::*;
        assert_eq!(blob_decision(None, true), Place);
        assert_eq!(blob_decision(Some(BlobLoc::Owned), true), DropIncoming);
        assert_eq!(blob_decision(Some(BlobLoc::External), true), Upgrade);
        assert_eq!(blob_decision(None, false), RecordExternal);
        assert_eq!(blob_decision(Some(BlobLoc::Owned), false), Keep);
        assert_eq!(blob_decision(Some(BlobLoc::External), false), Keep);
    }

    #[test]
    fn prune_needs_a_clean_full_scan() {
        assert!(may_prune(true, true, 0, 10, 12));
        assert!(may_prune(true, true, 0, 0, 0));
        assert!(!may_prune(false, true, 0, 10, 12), "folder missing");
        assert!(!may_prune(true, true, 1, 10, 12), "walk error");
        assert!(!may_prune(true, true, 0, 0, 12), "empty mountpoint");
        assert!(
            !may_prune(true, false, 0, 1, 12),
            "a stray file on the mountpoint's filesystem"
        );
        assert!(may_adopt_fs(true, 0, 12, 0));
        assert!(
            !may_adopt_fs(true, 0, 1, 11),
            "the stray file is not the Drive"
        );
    }

    #[test]
    fn drive_work_pauses_when_hot() {
        assert!(drive_may_work(true, 89, 90, true));
        assert!(!drive_may_work(true, 90, 90, true));
        assert!(!drive_may_work(true, 40, 90, false));
        assert!(drive_may_work(false, 0, 90, true));
    }
}
