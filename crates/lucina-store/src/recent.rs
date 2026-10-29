//! Recent chat turns (`src/recent-chat-context.js`) and turn metadata
//! (`src/turn-metadata.js`).
//!
//! Same format as Node's `recent-telegram-chat.json` (`{chatId: [{role, at,
//! text}]}`, last 20 turns per chat), but in phase 1 Rust writes its own
//! file, `data/lucina-rs/recent-chat.json` (secretary keys only), because
//! Node rewrites its file whole from memory.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::atomic::write_atomic;
use crate::timeutil::{clip_utf16, iso_ms, parse_js_date};

pub const TURNS_LIMIT: usize = 20;
pub const CHARS_LIMIT: usize = 5000;
pub const MARKER: &str = "Recent chat turns";
pub const IDLE_BOUNDARY_MS: i64 = 90 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    pub text: String,
}

#[derive(Debug)]
pub struct RecentChat {
    path: Option<PathBuf>,
    turns: Mutex<Option<BTreeMap<String, Vec<Turn>>>>,
}

impl RecentChat {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            turns: Mutex::new(None),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut BTreeMap<String, Vec<Turn>>) -> R) -> R {
        let mut g = self.turns.lock().unwrap_or_else(|e| e.into_inner());
        let map = g.get_or_insert_with(|| {
            self.path
                .as_ref()
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|b| serde_json::from_slice::<BTreeMap<String, Vec<Turn>>>(&b).ok())
                .unwrap_or_default()
        });
        f(map)
    }

    /// Import sessions starting with `prefix` from another buffer file (the
    /// Node Lucina's `recent-telegram-chat.json`, read only) when this buffer
    /// does not have them yet. Returns how many sessions were imported.
    pub fn seed_from(&self, path: &std::path::Path, prefix: &str) -> usize {
        let Some(seed) = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<BTreeMap<String, Vec<Turn>>>(&b).ok())
        else {
            return 0;
        };
        self.with(|map| {
            let mut n = 0;
            for (k, v) in seed {
                if k.starts_with(prefix) && !map.contains_key(&k) {
                    map.insert(k, v);
                    n += 1;
                }
            }
            n
        })
    }

    pub fn append(&self, chat_id: &str, role: &str, text: &str, at: DateTime<Utc>) {
        if chat_id.is_empty() || text.trim().is_empty() {
            return;
        }
        // Recent turns feed prompts and a file: redact first.
        let text = lucina_secrets::redactor().redact(text);
        let text: &str = &text;
        let snapshot = self.with(|map| {
            let turns = map.entry(chat_id.to_owned()).or_default();
            turns.push(Turn {
                role: if role == "assistant" {
                    "assistant".into()
                } else {
                    "user".into()
                },
                at: Some(iso_ms(at)),
                text: clip_utf16(text.trim(), 1000),
            });
            let skip = turns.len().saturating_sub(TURNS_LIMIT);
            turns.drain(..skip);
            serde_json::to_string_pretty(map).ok()
        });
        if let (Some(p), Some(text)) = (&self.path, snapshot) {
            if let Err(e) = write_atomic(p, format!("{text}\n").as_bytes()) {
                tracing::warn!(error = %e, "recent chat persist failed");
            }
        }
    }

    /// Forget the loaded turns: the next use reads the file again (another
    /// writer may have changed it, e.g. Node before a lease handover).
    pub fn reload(&self) {
        *self.turns.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn turns(&self, chat_id: &str) -> Vec<Turn> {
        self.with(|m| m.get(chat_id).cloned().unwrap_or_default())
    }

    /// The "Recent chat turns" block (Node `renderRecentChatContext`).
    pub fn render(&self, chat_id: &str, tz: Tz) -> String {
        let turns = self.turns(chat_id);
        if turns.is_empty() {
            return String::new();
        }
        let lines: Vec<String> = turns
            .iter()
            .map(|t| {
                let stamp =
                    t.at.as_deref()
                        .and_then(parse_js_date)
                        .map(|d| {
                            format!(
                                "[{} {}]",
                                d.with_timezone(&tz).format("%d/%m %H:%M"),
                                tz.name()
                            )
                        })
                        .unwrap_or_else(|| "[time unknown]".into());
                format!("{stamp} {}: {}", t.role, t.text)
            })
            .collect();
        let joined = lines.join("\n");
        let n = joined.chars().count();
        let rendered: String = if n > CHARS_LIMIT {
            joined.chars().skip(n - CHARS_LIMIT).collect()
        } else {
            joined
        };
        [
            format!("{MARKER} (most recent last; stamps show when each turn happened):"),
            "If the user asks you to reread what you sent, use these turns before older Honcho summaries or peer observations.".into(),
            "Turns older than today are history, not the current conversation.".into(),
            rendered,
        ]
        .join("\n")
    }

    /// The "Turn metadata" block (Node `buildTurnMetadata` + `renderTurnMetadataBlock`).
    pub fn metadata_block(&self, chat_id: &str, tz: Tz, now: DateTime<Utc>) -> String {
        let turns = self.turns(chat_id);
        let valid = |t: &Turn| {
            t.at.as_deref()
                .and_then(parse_js_date)
                .filter(|d| *d <= now + chrono::Duration::minutes(5))
        };
        let last_user = turns
            .iter()
            .rev()
            .find(|t| t.role == "user")
            .and_then(valid);
        let last_asst = turns
            .iter()
            .rev()
            .find(|t| t.role == "assistant")
            .and_then(valid);
        let last_activity = match (last_user, last_asst) {
            (Some(u), Some(a)) => Some(u.max(a)),
            (u, a) => u.or(a),
        };
        let elapsed = last_activity.map(|d| {
            ((now - d).num_milliseconds() as f64 / 1000.0)
                .round()
                .max(0.0) as i64
        });
        let first_after_idle = elapsed.is_none_or(|s| s * 1000 >= IDLE_BOUNDARY_MS);
        let local = now.with_timezone(&tz);
        let current_time = format!(
            "{} {} ({})",
            local.format("%Y-%m-%d %H:%M:%S"),
            local.format("%Z"),
            tz.name()
        );
        let mut lines = vec![
            "Turn metadata (computed by code; authoritative for date/time):".to_owned(),
            format!("- current time: {current_time}"),
            format!("- current date: {}", &current_time[..10]),
        ];
        if let Some(s) = elapsed {
            lines.push(format!("- since last chat activity: {s}s"));
        }
        lines.push(format!(
            "- first turn after idle: {}",
            if first_after_idle { "yes" } else { "no" }
        ));
        if let Some(u) = last_user {
            lines.push(format!("- last user message: {}", iso_ms(u)));
        }
        lines.push(format!("- turn index (recent window): {}", turns.len() + 1));
        lines.push(
            "- Dates before the current date are in the past; do not treat them as upcoming."
                .into(),
        );
        lines.join("\n")
    }
}

/// `clipMemoryForReply`: keep the newest recent turns when clipping.
pub fn clip_memory_for_reply(memory: &str, max_chars: usize) -> String {
    let limit = max_chars.max(200);
    let chars: Vec<char> = memory.chars().collect();
    if chars.len() <= limit {
        return memory.to_owned();
    }
    if let Some(idx) = memory.rfind(MARKER) {
        let recent: Vec<char> = memory[idx..].chars().collect();
        if recent.len() >= limit {
            let tail: String = recent[recent.len() - (limit - 28)..].iter().collect();
            return format!("{MARKER} (tail)\n{tail}");
        }
        let head_budget = limit as i64 - recent.len() as i64 - 5;
        let head = if head_budget > 80 {
            format!(
                "{}\n…\n",
                chars[..head_budget as usize].iter().collect::<String>()
            )
        } else {
            String::new()
        };
        return format!("{head}{}", recent.iter().collect::<String>());
    }
    chars[chars.len() - limit..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn renders_like_node() {
        let dir = tempfile::tempdir().unwrap();
        let r = RecentChat::new(Some(dir.path().join("recent.json")));
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap();
        r.append("secretary_app", "user", "ciao", t0);
        r.append(
            "secretary_app",
            "assistant",
            "ciao!",
            t0 + chrono::Duration::seconds(5),
        );
        let block = r.render("secretary_app", tz);
        assert!(block.starts_with("Recent chat turns (most recent last"));
        assert!(block.ends_with(
            "[21/10 17:00 Europe/Rome] user: ciao\n[21/10 17:00 Europe/Rome] assistant: ciao!"
        ));
        let meta = r.metadata_block("secretary_app", tz, t0 + chrono::Duration::seconds(65));
        assert!(
            meta.contains("- current time: 2026-10-21 17:01:05 CEST (Europe/Rome)"),
            "{meta}"
        );
        assert!(meta.contains("- since last chat activity: 60s"));
        assert!(meta.contains("- first turn after idle: no"));
        assert!(meta.contains("- turn index (recent window): 3"));
        let again = RecentChat::new(Some(dir.path().join("recent.json")));
        assert_eq!(again.turns("secretary_app").len(), 2);
        for i in 0..30 {
            r.append("x", "user", &format!("m{i}"), t0);
        }
        assert_eq!(r.turns("x").len(), TURNS_LIMIT);
    }
}
