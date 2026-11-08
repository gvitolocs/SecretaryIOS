//! Which vault files are read. Privacy by default: app state, backups,
//! templates, temporary folders, macOS litter and private/journal folders are
//! excluded unless the owner replaces `exclude`.
//!
//! Patterns use `/` separators relative to the vault root. `*` matches within
//! one path segment, `**` across segments, `?` one character.
//! - `name/` matches a folder with that name at any depth (and everything below);
//!   `a/b/` (another `/` inside) matches that folder path from the root.
//! - `name` matches a file name at any depth; `a/*.md` matches a path from the root.
use std::path::PathBuf;

pub const MAX_FILES: usize = 20_000;
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
pub const MAX_DEPTH: usize = 32;

pub const DEFAULT_EXCLUDES: &[&str] = &[
    ".obsidian/",
    ".trash/",
    ".backups/",
    ".git/",
    "Templates/",
    "_tmp*/",
    "_tmp*",
    "._*",
    ".DS_Store",
    "Private/",
    "private/",
    "Journal/",
    "journal/",
    "Diary/",
    "diary/",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultConfig {
    pub root: PathBuf,
    /// When non-empty, only files matching one of these patterns are indexed.
    pub include: Vec<String>,
    /// Always wins over `include`.
    pub exclude: Vec<String>,
    pub max_files: usize,
    pub max_file_bytes: u64,
}

impl VaultConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            include: vec![],
            exclude: DEFAULT_EXCLUDES.iter().map(|s| (*s).to_owned()).collect(),
            max_files: MAX_FILES,
            max_file_bytes: MAX_FILE_BYTES,
        }
    }
    /// A folder (relative path) whose contents are never visited.
    pub fn excludes_dir(&self, rel: &str) -> bool {
        self.exclude.iter().any(|p| dir_matches(p, rel))
    }
    /// A file excluded by name/path or by any of its folders.
    pub fn excludes_file(&self, rel: &str) -> bool {
        self.exclude
            .iter()
            .any(|p| file_matches(p, rel) || ancestors(rel).any(|d| dir_matches(p, d)))
    }
    pub fn includes_file(&self, rel: &str) -> bool {
        self.include.is_empty()
            || self
                .include
                .iter()
                .any(|p| file_matches(p, rel) || ancestors(rel).any(|d| dir_matches(p, d)))
    }
    pub fn selects(&self, rel: &str) -> bool {
        self.includes_file(rel) && !self.excludes_file(rel)
    }
}

fn ancestors(rel: &str) -> impl Iterator<Item = &str> {
    rel.match_indices('/').map(move |(i, _)| &rel[..i])
}
fn basename(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}
fn dir_matches(pattern: &str, dir: &str) -> bool {
    let Some(p) = pattern.strip_suffix('/') else {
        return false;
    };
    if p.contains('/') {
        glob(p, dir)
    } else {
        glob(p, basename(dir))
    }
}
fn file_matches(pattern: &str, rel: &str) -> bool {
    if pattern.ends_with('/') {
        false
    } else if pattern.contains('/') {
        glob(pattern, rel)
    } else {
        glob(pattern, basename(rel))
    }
}

/// Glob match on whole strings; see the module docs for the syntax.
pub fn glob(pattern: &str, s: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = s.chars().collect();
    glob_chars(&p, &s)
}
fn glob_chars(p: &[char], s: &[char]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some('*') if p.get(1) == Some(&'*') => {
            let rest = &p[2..];
            // `**/` may also match zero folders.
            if rest.first() == Some(&'/') && glob_chars(&rest[1..], s) {
                return true;
            }
            (0..=s.len()).any(|i| glob_chars(rest, &s[i..]))
        }
        Some('*') => {
            for i in 0..=s.len() {
                if glob_chars(&p[1..], &s[i..]) {
                    return true;
                }
                if i < s.len() && s[i] == '/' {
                    break;
                }
            }
            false
        }
        Some('?') => s.first().is_some_and(|c| *c != '/') && glob_chars(&p[1..], &s[1..]),
        Some(c) => s.first() == Some(c) && glob_chars(&p[1..], &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn globs() {
        assert!(glob("_tmp*", "_tmp_old"));
        assert!(glob("._*", "._Note.md"));
        assert!(!glob("*.md", "a/b.md"));
        assert!(glob("**/*.md", "a/b.md"));
        assert!(glob("**/*.md", "b.md"));
        assert!(glob("a/?.md", "a/b.md"));
        assert!(!glob("a/?.md", "a/bc.md"));
    }
    #[test]
    fn default_privacy_excludes() {
        let c = VaultConfig::new("/v");
        for rel in [
            ".obsidian/app.json",
            "Templates/Book.md",
            "Work/Templates/Day.md",
            "Work/Private/x.md",
            "Journal/2026.md",
            "._Note.md",
            "Work/._Note.md",
            "_tmp_import/a.md",
            ".backups/a.md",
        ] {
            assert!(c.excludes_file(rel), "{rel}");
        }
        for rel in [
            "Work/Permanent/Idea.md",
            "Privateer.md",
            "Work/MOC/Books.md",
        ] {
            assert!(!c.excludes_file(rel), "{rel}");
        }
        assert!(c.excludes_dir("Work/Private"));
        assert!(!c.excludes_dir("Work/Sources"));
    }
    #[test]
    fn include_and_override() {
        let mut c = VaultConfig::new("/v");
        c.include = vec!["Work/".into(), "Inbox.md".into()];
        assert!(c.selects("Work/a/b.md"));
        assert!(c.selects("Inbox.md"));
        assert!(!c.selects("Home/a.md"));
        assert!(!c.selects("Work/Private/a.md"));
        c.exclude.clear();
        assert!(c.selects("Work/Private/a.md"));
        c.include = vec!["Work/a/".into()];
        assert!(c.selects("Work/a/b.md"));
        assert!(!c.selects("Other/a/b.md"));
    }
}
