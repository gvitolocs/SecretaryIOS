//! The audit log: every resolve (secret name, tool, time, outcome), never a
//! value. JSON lines in a 0600 file plus the recent entries in memory.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

pub const DEFAULT_KEEP: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    /// RFC 3339, UTC.
    pub ts: String,
    /// The reference (`secret://lucina/prod/lucina#KEY`).
    #[serde(rename = "ref")]
    pub reference: String,
    /// The key alone.
    pub name: String,
    /// The tool (or component) that asked.
    pub tool: String,
    pub ok: bool,
    /// Served from the in-memory cache (no Infisical call).
    #[serde(default)]
    pub cached: bool,
    /// An error code (`denied`, `not_found`, ...) when not ok.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub struct AuditLog {
    path: Option<PathBuf>,
    keep: usize,
    recent: Mutex<VecDeque<AuditEntry>>,
}

impl std::fmt::Debug for AuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditLog")
            .field("path", &self.path)
            .finish()
    }
}

fn open_append(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut o = std::fs::OpenOptions::new();
    o.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

impl AuditLog {
    /// `path: None` keeps the log in memory only (tests).
    pub fn new(path: Option<PathBuf>, keep: usize) -> Self {
        let keep = keep.max(1);
        let mut recent = VecDeque::new();
        if let Some(p) = &path {
            if let Ok(text) = std::fs::read_to_string(p) {
                for line in text.lines() {
                    if let Ok(e) = serde_json::from_str::<AuditEntry>(line) {
                        recent.push_back(e);
                        if recent.len() > keep {
                            recent.pop_front();
                        }
                    }
                }
            }
        }
        Self {
            path,
            keep,
            recent: Mutex::new(recent),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn record(&self, entry: AuditEntry) {
        if let Some(p) = &self.path {
            let line = serde_json::to_string(&entry).unwrap_or_default();
            let res = open_append(p).and_then(|mut f| f.write_all(format!("{line}\n").as_bytes()));
            if let Err(e) = res {
                tracing::warn!(error = %e.kind(), "secrets audit write failed");
            }
        }
        let mut r = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        r.push_back(entry);
        while r.len() > self.keep {
            r.pop_front();
        }
    }

    /// Newest first; `name` filters by key (case-insensitive), `tool` by tool.
    pub fn entries(&self, limit: usize, name: Option<&str>, tool: Option<&str>) -> Vec<AuditEntry> {
        let r = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        r.iter()
            .rev()
            .filter(|e| name.is_none_or(|n| e.name.eq_ignore_ascii_case(n)))
            .filter(|e| tool.is_none_or(|t| e.tool.eq_ignore_ascii_case(t)))
            .take(limit)
            .cloned()
            .collect()
    }

    /// The last successful use of a key.
    pub fn last_use(&self, name: &str) -> Option<AuditEntry> {
        let r = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        r.iter().rev().find(|e| e.ok && e.name == name).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, tool: &str, ok: bool) -> AuditEntry {
        AuditEntry {
            ts: "2026-10-22T12:00:00Z".into(),
            reference: format!("secret://lucina/prod/lucina#{name}"),
            name: name.into(),
            tool: tool.into(),
            ok,
            cached: false,
            error: (!ok).then(|| "denied".into()),
        }
    }

    #[test]
    fn file_and_memory_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state/secrets-audit.jsonl");
        let log = AuditLog::new(Some(p.clone()), 3);
        log.record(e("A_KEY", "weather", true));
        log.record(e("B_KEY", "todoist", false));
        log.record(e("A_KEY", "weather", true));
        log.record(e("C_KEY", "github", true));
        let all = log.entries(10, None, None);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].name, "C_KEY");
        assert_eq!(log.entries(10, Some("a_key"), None).len(), 1);
        assert!(log.last_use("B_KEY").is_none());
        // reloaded from disk
        let again = AuditLog::new(Some(p.clone()), 10);
        assert_eq!(again.entries(10, None, None).len(), 4);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("\"ref\":\"secret://lucina/prod/lucina#A_KEY\""));
    }
}
