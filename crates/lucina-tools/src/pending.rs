//! Confirmation flow: pending actions.
//!
//! A tool call that needs the owner's yes (External tools, `confirm` tools,
//! or a tool's own per-call rule such as cancelling a reminder) is never run
//! in the turn that asked for it. It is stored here with an id and a
//! 10-minute expiry, and runs only when a **later** turn of the same session
//! is an explicit yes ([`classify_reply`]). The model never gets to run it.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Utc};
use lucina_store::timeutil::{iso_ms, parse_js_date};
use lucina_store::write_atomic;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingAction {
    pub id: String,
    pub session_id: String,
    pub tool: String,
    pub args: Value,
    pub summary: String,
    pub created_at: String,
    pub expires_at: String,
}

impl PendingAction {
    pub fn expired(&self, now: DateTime<Utc>) -> bool {
        parse_js_date(&self.expires_at).is_none_or(|e| e <= now)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PendingFile {
    #[serde(default)]
    pending: Vec<PendingAction>,
}

/// File-backed (`data/lucina-rs/pending-actions.json`, atomic 0600) or
/// in-memory store.
#[derive(Debug)]
pub struct PendingStore {
    path: Option<PathBuf>,
    ttl: Duration,
    mem: Mutex<Vec<PendingAction>>,
}

impl PendingStore {
    pub fn file(path: impl Into<PathBuf>, ttl: Duration) -> Self {
        Self { path: Some(path.into()), ttl, mem: Mutex::new(Vec::new()) }
    }

    pub fn memory(ttl: Duration) -> Self {
        Self { path: None, ttl, mem: Mutex::new(Vec::new()) }
    }

    fn load(&self, guard: &mut Vec<PendingAction>) {
        if let Some(p) = &self.path {
            *guard = std::fs::read(p)
                .ok()
                .and_then(|b| serde_json::from_slice::<PendingFile>(&b).ok())
                .map(|f| f.pending)
                .unwrap_or_default();
        }
    }

    fn save(&self, list: &[PendingAction]) {
        if let Some(p) = &self.path {
            let text = serde_json::to_string_pretty(&serde_json::json!({"pending": list})).unwrap_or_default();
            if let Err(e) = write_atomic(p, format!("{text}\n").as_bytes()) {
                tracing::warn!(error = %e, "pending actions write failed");
            }
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Vec<PendingAction>) -> R) -> R {
        let mut g = self.mem.lock().unwrap_or_else(|e| e.into_inner());
        self.load(&mut g);
        let r = f(&mut g);
        self.save(&g);
        r
    }

    /// Store a new pending action; one per session (a newer one replaces it).
    pub fn create(&self, session_id: &str, tool: &str, args: Value, summary: &str, now: DateTime<Utc>) -> PendingAction {
        let action = PendingAction {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_owned(),
            tool: tool.to_owned(),
            args,
            summary: summary.to_owned(),
            created_at: iso_ms(now),
            expires_at: iso_ms(now + chrono::Duration::from_std(self.ttl).unwrap_or_default()),
        };
        self.with(|list| {
            list.retain(|p| p.session_id != session_id && !p.expired(now));
            list.push(action.clone());
        });
        action
    }

    /// The live pending action of a session.
    pub fn current(&self, session_id: &str, now: DateTime<Utc>) -> Option<PendingAction> {
        self.with(|list| {
            list.retain(|p| !p.expired(now));
            list.iter().rev().find(|p| p.session_id == session_id).cloned()
        })
    }

    /// Remove and return it (it runs at most once).
    pub fn take(&self, id: &str) -> Option<PendingAction> {
        self.with(|list| {
            let pos = list.iter().position(|p| p.id == id)?;
            Some(list.remove(pos))
        })
    }
}

/// How a message answers a pending confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Yes,
    No,
    Other,
}

const YES: &[&str] = &[
    "si", "sì", "sí", "yes", "y", "yep", "yeah", "ok", "okay", "confermo", "conferma", "certo", "procedi", "vai", "fallo",
    "si procedi", "sì procedi", "si confermo", "sì confermo", "si grazie", "sì grazie", "si fallo", "sì fallo", "do it",
    "go ahead", "confirm", "confirmed", "yes please", "yes do it", "ja", "jep", "bekræft", "gør det",
];
const NO: &[&str] = &[
    "no", "n", "nope", "annulla", "lascia stare", "non farlo", "no grazie", "cancel", "stop", "don't", "dont", "nej", "lad være",
];

/// Strict: the whole message (lowercased, punctuation and emoji trimmed)
/// must be an explicit yes or no. Anything else is `Other`.
pub fn classify_reply(text: &str) -> Reply {
    let cleaned: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '\'' { c } else { ' ' })
        .collect();
    let norm = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if YES.contains(&norm.as_str()) {
        Reply::Yes
    } else if NO.contains(&norm.as_str()) {
        Reply::No
    } else {
        Reply::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies() {
        assert_eq!(classify_reply("Sì!"), Reply::Yes);
        assert_eq!(classify_reply("ok 👍"), Reply::Yes);
        assert_eq!(classify_reply("yes, do it"), Reply::Yes);
        assert_eq!(classify_reply("No."), Reply::No);
        assert_eq!(classify_reply("sì ma prima dimmi il meteo"), Reply::Other);
        assert_eq!(classify_reply("che tempo fa?"), Reply::Other);
    }

    #[test]
    fn expiry_and_single_use() {
        let dir = tempfile::tempdir().unwrap();
        let s = PendingStore::file(dir.path().join("p.json"), DEFAULT_TTL);
        let now = Utc::now();
        let p = s.create("sess", "reminders", serde_json::json!({"action": "cancel"}), "cancel x", now);
        assert_eq!(s.current("sess", now).unwrap().id, p.id);
        assert!(s.current("other", now).is_none());
        assert!(s.current("sess", now + chrono::Duration::minutes(11)).is_none());
        let p = s.create("sess", "reminders", serde_json::json!({}), "y", now);
        assert!(s.take(&p.id).is_some());
        assert!(s.take(&p.id).is_none());
    }
}
