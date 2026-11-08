//! The file explorer (Paseo `file_explorer_request`, `fs.file.*`, `/api/files/download`):
//! list, read, write and one-shot download tokens, always inside the workspace. A path
//! that resolves outside it (`..`, an absolute path, a symlink out) is refused with
//! Paseo's message.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use base64::Engine;
use lucina_agents::workspace::{FileContent, FileEntry};

pub const OUTSIDE: &str = "Access outside of workspace is not allowed";
const DEFAULT_MAX_BYTES: u64 = 1024 * 1024;
const TOKEN_TTL: Duration = Duration::from_secs(60);

fn iso(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// `rel` inside `root`, or [`OUTSIDE`]. The deepest existing ancestor is resolved, so a
/// symlink can't lead out, and a path that doesn't exist yet (a write) still resolves.
pub fn resolve(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let rel = Path::new(rel);
    if rel.is_absolute() || rel.components().any(|c| matches!(c, Component::ParentDir | Component::Prefix(_))) {
        return Err(OUTSIDE.into());
    }
    let joined = root.join(rel);
    let mut existing = joined.as_path();
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    while !existing.exists() {
        match (existing.parent(), existing.file_name()) {
            (Some(p), Some(n)) => {
                rest.push(n);
                existing = p;
            }
            _ => return Err(OUTSIDE.into()),
        }
    }
    let mut real = std::fs::canonicalize(existing).map_err(|e| e.to_string())?;
    for n in rest.iter().rev() {
        real.push(n);
    }
    if real.starts_with(&root) {
        Ok(real)
    } else {
        Err(OUTSIDE.into())
    }
}

fn relative(root: &Path, p: &Path) -> String {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    p.strip_prefix(&root).map(|r| r.display().to_string()).unwrap_or_default()
}

pub fn list(root: &Path, rel: &str) -> Result<(String, Vec<FileEntry>), String> {
    let dir = resolve(root, rel)?;
    let read = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    for e in read.flatten() {
        let path = e.path();
        // Follow symlinks for the kind and size; drop dangling ones and links out.
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if resolve(root, &relative(root, &path)).is_err() {
            continue;
        }
        entries.push(FileEntry {
            name: e.file_name().to_string_lossy().to_string(),
            path: relative(root, &path),
            kind: if meta.is_dir() { "directory" } else { "file" }.into(),
            size: meta.len(),
            modified_at: meta.modified().map(iso).unwrap_or_default(),
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let shown = if rel.is_empty() { ".".to_string() } else { rel.to_string() };
    Ok((shown, entries))
}

fn mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        "md" => "text/markdown",
        "html" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "ts" | "tsx" | "rs" | "toml" | "txt" | "yml" | "yaml" | "sh" | "py" | "swift" | "kt" => "text/plain",
        "pdf" => "application/pdf",
        _ => return None,
    })
}

pub fn revision(meta: &std::fs::Metadata) -> String {
    let mtime_ns = meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128;
    format!("{}:{}:{}:{}", meta.dev(), meta.ino(), meta.len(), mtime_ns)
}

pub fn read(root: &Path, rel: &str, max_bytes: Option<u64>, accept_binary: bool) -> Result<FileContent, String> {
    let path = resolve(root, rel)?;
    let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    if meta.is_dir() {
        return Err(format!("{rel} is a directory"));
    }
    let limit = max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    if meta.len() > limit {
        return Err(format!("File is too large ({} bytes, limit {limit})", meta.len()));
    }
    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
    let mime = mime(&path);
    let image = mime.is_some_and(|m| m.starts_with("image/") && m != "image/svg+xml");
    let text = !image && !bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok();
    let b64 = || base64::engine::general_purpose::STANDARD.encode(&bytes);
    let (kind, encoding, content) = if text {
        ("text", "utf-8", Some(String::from_utf8_lossy(&bytes).to_string()))
    } else if image {
        ("image", "base64", Some(b64()))
    } else if accept_binary {
        ("binary", "base64", Some(b64()))
    } else {
        ("binary", "none", None)
    };
    Ok(FileContent {
        path: relative(root, &path),
        kind: kind.into(),
        encoding: encoding.into(),
        content,
        mime_type: Some(mime.unwrap_or(if text { "text/plain" } else { "application/octet-stream" }).into()),
        size: meta.len(),
        modified_at: meta.modified().map(iso).unwrap_or_default(),
        revision: Some(revision(&meta)),
    })
}

/// Write a text file; with `expected_revision`, refuse when it changed since it was read.
pub fn write(root: &Path, rel: &str, content: &str, expected_revision: Option<&str>) -> Result<FileContent, String> {
    let path = resolve(root, rel)?;
    if let Some(expected) = expected_revision {
        let current = std::fs::metadata(&path).map(|m| revision(&m)).ok();
        if current.as_deref() != Some(expected) {
            return Err("The file changed since it was read (stale revision)".into());
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_file_name(format!(".{}.lucina-tmp", path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()));
    std::fs::write(&tmp, content).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    read(root, &relative(root, &path), None, false)
}

/// One-shot download tokens (Paseo: TTL 60 s, consumed on use).
#[derive(Default)]
pub struct Tokens {
    map: Mutex<HashMap<String, (PathBuf, Instant)>>,
}

impl Tokens {
    pub fn create(&self, root: &Path, rel: &str) -> Result<String, String> {
        let path = resolve(root, rel)?;
        if !path.is_file() {
            return Err(format!("Not a file: {rel}"));
        }
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        map.retain(|_, (_, at)| at.elapsed() < TOKEN_TTL);
        map.insert(token.clone(), (path, Instant::now()));
        Ok(token)
    }

    pub fn take(&self, token: &str) -> Option<PathBuf> {
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        let (path, at) = map.remove(token)?;
        (at.elapsed() < TOKEN_TTL).then_some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_inside() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("sub/a.txt"), "hi").unwrap();
        std::os::unix::fs::symlink("/etc", d.path().join("out")).unwrap();
        assert_eq!(resolve(d.path(), "../x").unwrap_err(), OUTSIDE);
        assert_eq!(resolve(d.path(), "/etc/passwd").unwrap_err(), OUTSIDE);
        assert_eq!(resolve(d.path(), "out/passwd").unwrap_err(), OUTSIDE);
        assert!(resolve(d.path(), "sub/new/file.txt").is_ok(), "a path to create");
        let (_, entries) = list(d.path(), "").unwrap();
        assert_eq!(entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["sub"], "the link out is hidden");
        let f = read(d.path(), "sub/a.txt", None, false).unwrap();
        assert_eq!((f.kind.as_str(), f.content.as_deref()), ("text", Some("hi")));
        let rev = f.revision.clone().unwrap();
        assert!(write(d.path(), "sub/a.txt", "x", Some("0:0:0:0")).is_err());
        assert_eq!(write(d.path(), "sub/a.txt", "new", Some(&rev)).unwrap().content.as_deref(), Some("new"));
        let t = Tokens::default();
        let tok = t.create(d.path(), "sub/a.txt").unwrap();
        assert!(t.take(&tok).is_some());
        assert!(t.take(&tok).is_none(), "one use");
    }
}
