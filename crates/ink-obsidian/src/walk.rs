//! Read-only vault walk. Symlinks are never followed, every visited path must
//! stay inside the canonical root, and file count, size and depth are capped.
use crate::{config::VaultConfig, config::MAX_DEPTH, Error, Result};
use std::{
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `/`-separated path relative to the vault root.
    pub rel: String,
    pub mtime_ns: u64,
    pub size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Walk {
    pub root: PathBuf,
    /// Markdown notes, sorted by path.
    pub notes: Vec<Entry>,
    /// Other selected files (link targets such as images and PDFs), names only.
    pub attachments: Vec<String>,
    pub excluded_dirs: usize,
    pub excluded_files: usize,
    pub skipped: usize,
    pub warnings: Vec<String>,
}

pub fn is_markdown(rel: &str) -> bool {
    rel.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("md"))
}

/// True when `path`'s canonical form is inside `root` (itself canonical).
pub fn inside(root: &Path, path: &Path) -> bool {
    fs::canonicalize(path).is_ok_and(|p| p.starts_with(root))
}

pub fn walk(cfg: &VaultConfig) -> Result<Walk> {
    let root = fs::canonicalize(&cfg.root)?;
    if !fs::metadata(&root)?.is_dir() {
        return Err(Error::Invalid("vault root is not a directory".into()));
    }
    let mut w = Walk {
        root: root.clone(),
        ..Default::default()
    };
    let mut stack = vec![(root.clone(), String::new(), 0usize)];
    let mut capped = false;
    while let Some((dir, rel_dir, depth)) = stack.pop() {
        let Ok(mut entries) =
            fs::read_dir(&dir).and_then(|d| d.collect::<std::io::Result<Vec<_>>>())
        else {
            w.skipped += 1;
            w.warnings
                .push(format!("unreadable folder skipped: {}", display(&rel_dir)));
            continue;
        };
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let Some(name) = e.file_name().to_str().map(str::to_owned) else {
                w.skipped += 1;
                w.warnings.push(format!(
                    "skipped a non-UTF-8 file name in {}",
                    display(&rel_dir)
                ));
                continue;
            };
            let rel = if rel_dir.is_empty() {
                name
            } else {
                format!("{rel_dir}/{name}")
            };
            let ft = e.file_type()?;
            if ft.is_symlink() {
                w.skipped += 1;
                w.warnings.push(format!("symlink not followed: {rel}"));
                continue;
            }
            let path = e.path();
            if ft.is_dir() {
                if cfg.excludes_dir(&rel) {
                    w.excluded_dirs += 1;
                } else if depth + 1 > MAX_DEPTH {
                    w.skipped += 1;
                    w.warnings
                        .push(format!("folder deeper than {MAX_DEPTH}: {rel}"));
                } else if !inside(&root, &path) {
                    w.skipped += 1;
                    w.warnings.push(format!("folder outside the vault: {rel}"));
                } else {
                    stack.push((path, rel, depth + 1));
                }
                continue;
            }
            if !ft.is_file() {
                w.skipped += 1;
                continue;
            }
            if !cfg.selects(&rel) {
                w.excluded_files += 1;
                continue;
            }
            if w.notes.len() + w.attachments.len() >= cfg.max_files {
                capped = true;
                w.skipped += 1;
                continue;
            }
            if !inside(&root, &path) {
                w.skipped += 1;
                w.warnings.push(format!("file outside the vault: {rel}"));
                continue;
            }
            if !is_markdown(&rel) {
                w.attachments.push(rel);
                continue;
            }
            let meta = e.metadata()?;
            if meta.len() > cfg.max_file_bytes {
                w.skipped += 1;
                w.warnings.push(format!(
                    "note larger than {} bytes skipped: {rel}",
                    cfg.max_file_bytes
                ));
                continue;
            }
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
                .unwrap_or(0);
            w.notes.push(Entry {
                rel,
                mtime_ns,
                size: meta.len(),
            });
        }
    }
    if capped {
        w.warnings.push(format!(
            "file cap of {} reached; rest skipped",
            cfg.max_files
        ));
    }
    w.notes.sort_by(|a, b| a.rel.cmp(&b.rel));
    w.attachments.sort();
    Ok(w)
}

fn display(rel_dir: &str) -> &str {
    if rel_dir.is_empty() {
        "the vault root"
    } else {
        rel_dir
    }
}

/// Outcome of reading one note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Read {
    Text(String),
    /// Not read; the reason contains no note content.
    Skipped(String),
}

/// Read a note again with the same safety checks; never follows a symlink.
pub fn read_note(root: &Path, rel: &str, max_bytes: u64) -> Result<Read> {
    if rel
        .split('/')
        .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Ok(Read::Skipped(format!("unsafe path: {rel}")));
    }
    let path = root.join(rel);
    // Reject a symlink anywhere below the root, not only at the leaf.
    let mut p = root.to_path_buf();
    for c in rel.split('/') {
        p.push(c);
        if fs::symlink_metadata(&p)?.file_type().is_symlink() {
            return Ok(Read::Skipped(format!("symlink not followed: {rel}")));
        }
    }
    if !inside(root, &path) {
        return Ok(Read::Skipped(format!("file outside the vault: {rel}")));
    }
    let mut bytes = Vec::new();
    fs::File::open(&path)?
        .take(max_bytes + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Ok(Read::Skipped(format!(
            "note larger than {max_bytes} bytes skipped: {rel}"
        )));
    }
    match String::from_utf8(bytes) {
        Ok(s) => Ok(Read::Text(s)),
        Err(_) => Ok(Read::Skipped(format!("non-UTF-8 note skipped: {rel}"))),
    }
}
