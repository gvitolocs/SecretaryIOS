//! The Obsidian folder Lucina writes (`Lucina/` in the owner's vault).
//!
//! Every path is relative to the root and confined to it: no `..`, `.`,
//! empty or hidden components, no NUL or backslash, `.md` pages only, and no
//! symlink anywhere from the root down to the target (the root itself may be
//! reached through one: it is canonicalized once at open). Writes are atomic
//! (a hidden temp file in the same directory, fsync, rename, fsync of the
//! directory) and never follow a symlink at the final component.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Where the project-map session writes whole pages ([`Vault::write_page`]).
pub const MAP_DIR: &str = "Map";
/// Ops' own folders.
pub const LOG_DIR: &str = "Log";
pub const PROJECTS_DIR: &str = "Projects";
pub const INCIDENTS_DIR: &str = "Incidents";
pub const CONFLICTS_DIR: &str = "Conflicts";
pub const README: &str = "README.md";
/// A page may be at most this big (UTF-8 bytes).
pub const MAX_PAGE_BYTES: usize = 512 * 1024;
const MAX_REL_BYTES: usize = 1024;
const MAX_COMPONENT_BYTES: usize = 255;

const README_TEXT: &str = "# Lucina\n\n\
This folder is written by Lucina, the keeper of the record of all of Giuseppe's projects. \
Every Claude session, agent and Lucina herself reports deploys, incidents, decisions, status \
and findings here.\n\n\
- `Log/YYYY-MM-DD.md`: every record of the day, in time order.\n\
- `Projects/<project>.md`: one page per project, with a section per kind of record \
(Status, Decisions, Incidents, Deploys, Findings, Open questions). The free text above the \
first `## ` section is yours: Lucina never touches it, and she only adds or trims her own \
entries (the lines that end in a hidden `ops:` comment) inside the sections.\n\
- `Incidents/YYYY-MM-DD-<project>-<slug>.md`: one page per incident.\n\
- `Map/`: written by the project-map session, not by the ops record.\n\
- `Conflicts/`: Syncthing conflict copies Lucina moved out of the way; the entries they \
held are back in the pages.\n\n\
To report something, use the MCP tool `ops_record` (or ask Lucina in chat). \
`ops_search` and Lucina answer \"what changed on X\" and \"why did we decide Y\".\n";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    #[error("invalid vault path {0:?}: {1}")]
    InvalidPath(String, &'static str),
    #[error("vault path {0:?} is not allowed here")]
    NotAllowed(String),
    #[error("symlink refused at {0:?}")]
    SymlinkRefused(String),
    #[error("not a directory: {0:?}")]
    NotADirectory(String),
    #[error("not a regular file: {0:?}")]
    NotAFile(String),
    #[error("page too large ({0} bytes, at most {MAX_PAGE_BYTES})")]
    TooLarge(usize),
    #[error("vault io error: {0}")]
    Io(String),
}

impl VaultError {
    pub fn code(&self) -> &'static str {
        match self {
            VaultError::InvalidPath(..) => "invalid_path",
            VaultError::NotAllowed(_) => "path_not_allowed",
            VaultError::SymlinkRefused(_) => "symlink_refused",
            VaultError::NotADirectory(_) => "not_a_directory",
            VaultError::NotAFile(_) => "not_a_file",
            VaultError::TooLarge(_) => "payload_too_large",
            VaultError::Io(_) => "io_error",
        }
    }
}

fn io(e: std::io::Error) -> VaultError {
    VaultError::Io(e.to_string())
}

/// The components of a valid relative page path.
pub fn check_rel(rel: &str) -> Result<Vec<&str>, VaultError> {
    let bad = |why| {
        Err(VaultError::InvalidPath(
            rel.chars().take(200).collect(),
            why,
        ))
    };
    if rel.is_empty() {
        return bad("empty");
    }
    if rel.len() > MAX_REL_BYTES {
        return bad("too long");
    }
    if rel.contains('\0') {
        return bad("NUL");
    }
    if rel.contains('\\') {
        return bad("backslash");
    }
    if rel.chars().any(char::is_control) {
        return bad("control character");
    }
    if rel.starts_with('/') {
        return bad("absolute");
    }
    let parts: Vec<&str> = rel.split('/').collect();
    for p in &parts {
        if p.is_empty() {
            return bad("empty component");
        }
        if *p == "." || *p == ".." {
            return bad("dot component");
        }
        if p.starts_with('.') {
            return bad("hidden component");
        }
        if p.len() > MAX_COMPONENT_BYTES {
            return bad("component too long");
        }
    }
    let last = parts.last().copied().unwrap_or("");
    if !last.ends_with(".md") || last.len() <= 3 {
        return bad("only .md pages");
    }
    Ok(parts)
}

fn random_hex() -> String {
    let mut b = [0u8; 8];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The vault root and its confined writes.
#[derive(Debug, Clone)]
pub struct Vault {
    root: PathBuf,
}

impl Vault {
    /// Open the vault at `root` (it must exist and be a directory). Writes
    /// `README.md` when missing.
    pub fn open(root: &Path) -> Result<Self, VaultError> {
        let root = root
            .canonicalize()
            .map_err(|e| VaultError::Io(format!("vault root {}: {e}", root.display())))?;
        if !root.is_dir() {
            return Err(VaultError::NotADirectory(root.display().to_string()));
        }
        let v = Self { root };
        if v.read(README)?.is_none() {
            v.write(README, README_TEXT)?;
        }
        Ok(v)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Replace a whole page under `Map/` (the project-map session's pages),
    /// redacted. Identical content is left alone (no Syncthing churn).
    /// Returns whether the page changed.
    pub fn write_page(&self, rel: &str, content: &str) -> Result<bool, VaultError> {
        let parts = check_rel(rel)?;
        if parts.len() < 2 || parts[0] != MAP_DIR {
            return Err(VaultError::NotAllowed(rel.to_owned()));
        }
        let content = lucina_secrets::redactor().redact(content);
        if self.read(rel)?.as_deref() == Some(content.as_ref()) {
            return Ok(false);
        }
        self.write(rel, &content)?;
        Ok(true)
    }

    /// Walk from the root to the parent of `rel`, refusing symlinks and
    /// non-directories, creating missing directories when `create`. `None`:
    /// a directory is missing (and `create` is off).
    fn parent_dir(&self, parts: &[&str], create: bool) -> Result<Option<PathBuf>, VaultError> {
        let mut dir = self.root.clone();
        let mut shown = String::new();
        for p in &parts[..parts.len() - 1] {
            dir.push(p);
            if !shown.is_empty() {
                shown.push('/');
            }
            shown.push_str(p);
            match fs::symlink_metadata(&dir) {
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(VaultError::SymlinkRefused(shown))
                }
                Ok(m) if !m.is_dir() => return Err(VaultError::NotADirectory(shown)),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if !create {
                        return Ok(None);
                    }
                    match fs::DirBuilder::new().mode(0o755).create(&dir) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(e) => return Err(io(e)),
                    }
                    let m = fs::symlink_metadata(&dir).map_err(io)?;
                    if m.file_type().is_symlink() {
                        return Err(VaultError::SymlinkRefused(shown));
                    }
                    if !m.is_dir() {
                        return Err(VaultError::NotADirectory(shown));
                    }
                }
                Err(e) => return Err(io(e)),
            }
        }
        Ok(Some(dir))
    }

    /// The final component must not be a symlink or a directory.
    fn check_target(path: &Path, rel: &str) -> Result<bool, VaultError> {
        match fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_symlink() => Err(VaultError::SymlinkRefused(rel.to_owned())),
            Ok(m) if !m.is_file() => Err(VaultError::NotAFile(rel.to_owned())),
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io(e)),
        }
    }

    /// The page's text, `None` when it does not exist.
    pub fn read(&self, rel: &str) -> Result<Option<String>, VaultError> {
        let parts = check_rel(rel)?;
        let Some(dir) = self.parent_dir(&parts, false)? else {
            return Ok(None);
        };
        let path = dir.join(parts[parts.len() - 1]);
        if !Self::check_target(&path, rel)? {
            return Ok(None);
        }
        let mut f = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
                return Err(VaultError::SymlinkRefused(rel.to_owned()))
            }
            Err(e) => return Err(io(e)),
        };
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).map_err(io)?;
        Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
    }

    pub fn exists(&self, rel: &str) -> Result<bool, VaultError> {
        let parts = check_rel(rel)?;
        let Some(dir) = self.parent_dir(&parts, false)? else {
            return Ok(false);
        };
        Self::check_target(&dir.join(parts[parts.len() - 1]), rel)
    }

    /// Atomic whole-page write (any confined path: ops' own folders).
    pub(crate) fn write(&self, rel: &str, content: &str) -> Result<(), VaultError> {
        if content.len() > MAX_PAGE_BYTES {
            return Err(VaultError::TooLarge(content.len()));
        }
        let parts = check_rel(rel)?;
        let dir = self
            .parent_dir(&parts, true)?
            .ok_or_else(|| VaultError::Io(format!("{rel}: directory missing")))?;
        let name = parts[parts.len() - 1];
        let target = dir.join(name);
        Self::check_target(&target, rel)?;
        let tmp = dir.join(format!(".{name}.tmp-{}", random_hex()));
        let result = (|| {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o644)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&tmp)?;
            f.write_all(content.as_bytes())?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, &target)?;
            File::open(&dir)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result.map_err(io)
    }

    /// Move a page inside the vault (same filesystem). An existing
    /// destination gets `-2`, `-3`, … before `.md`. Returns the destination.
    pub(crate) fn rename(&self, from: &str, to: &str) -> Result<String, VaultError> {
        let fparts = check_rel(from)?;
        let fdir = self
            .parent_dir(&fparts, false)?
            .ok_or_else(|| VaultError::Io(format!("{from}: missing")))?;
        let src = fdir.join(fparts[fparts.len() - 1]);
        if !Self::check_target(&src, from)? {
            return Err(VaultError::Io(format!("{from}: missing")));
        }
        let stem = to.strip_suffix(".md").unwrap_or(to);
        for n in 1..1000 {
            let dest = if n == 1 {
                to.to_owned()
            } else {
                format!("{stem}-{n}.md")
            };
            let tparts = check_rel(&dest)?;
            let tdir = self
                .parent_dir(&tparts, true)?
                .ok_or_else(|| VaultError::Io(format!("{dest}: directory missing")))?;
            let target = tdir.join(tparts[tparts.len() - 1]);
            match fs::symlink_metadata(&target) {
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io(e)),
            }
            fs::rename(&src, &target).map_err(io)?;
            File::open(&tdir).and_then(|d| d.sync_all()).map_err(io)?;
            File::open(&fdir).and_then(|d| d.sync_all()).map_err(io)?;
            return Ok(dest);
        }
        Err(VaultError::Io(format!("{to}: no free name")))
    }

    /// Entries of a vault directory (`""` = the root): `(name, kind)`,
    /// refusing to enter a symlinked directory. `None` when it does not exist.
    pub(crate) fn list_dir(
        &self,
        rel_dir: &str,
    ) -> Result<Option<Vec<(String, Entry)>>, VaultError> {
        let mut dir = self.root.clone();
        if !rel_dir.is_empty() {
            for p in rel_dir.split('/') {
                if p.is_empty() || p == "." || p == ".." || p.starts_with('.') || p.contains('\\') {
                    return Err(VaultError::InvalidPath(rel_dir.to_owned(), "bad directory"));
                }
                dir.push(p);
                match fs::symlink_metadata(&dir) {
                    Ok(m) if m.file_type().is_symlink() => {
                        return Err(VaultError::SymlinkRefused(rel_dir.to_owned()))
                    }
                    Ok(m) if !m.is_dir() => {
                        return Err(VaultError::NotADirectory(rel_dir.to_owned()))
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                    Err(e) => return Err(io(e)),
                }
            }
        }
        let mut out = Vec::new();
        for e in fs::read_dir(&dir).map_err(io)? {
            let e = e.map_err(io)?;
            let Ok(name) = e.file_name().into_string() else {
                continue;
            };
            let ft = e.file_type().map_err(io)?;
            let kind = if ft.is_symlink() {
                Entry::Symlink
            } else if ft.is_dir() {
                Entry::Dir
            } else if ft.is_file() {
                Entry::File
            } else {
                Entry::Other
            };
            out.push((name, kind));
        }
        out.sort();
        Ok(Some(out))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Entry {
    Dir,
    File,
    Symlink,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rel_rules() {
        for ok in ["Map/x.md", "Projects/lucina.md", "README.md", "a/b/c-d.md"] {
            assert!(check_rel(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "/etc/x.md",
            "../x.md",
            "Map/../x.md",
            "Map/./x.md",
            "Map//x.md",
            ".x.md",
            "Map/.obsidian/x.md",
            "Map\\x.md",
            "Map/x\0.md",
            "Map/x.txt",
            "Map/x",
            ".md",
            "Map/x.md/",
            "Map/a\nb.md",
        ] {
            assert!(check_rel(bad).is_err(), "{bad:?}");
        }
    }
}
