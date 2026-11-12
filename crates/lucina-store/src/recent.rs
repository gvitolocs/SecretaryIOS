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

    /// Take a secret out of the turns (it was saved in Infisical): how many turns changed.
    pub fn scrub(&self, needles: &[String], replacement: &str) -> usize {
        let (n, snapshot) = self.with(|map| {
            let mut n = 0;
            for turn in map.values_mut().flatten() {
                if let Some(new) = crate::scrub::scrub_str(&turn.text, needles, replacement) {
                    turn.text = new;
                    n += 1;
                }
            }
            (
                n,
                (n > 0)
                    .then(|| serde_json::to_string_pretty(map).ok())
                    .flatten(),
            )
        });
        if let (Some(p), Some(text)) = (&self.path, snapshot) {
            if let Err(e) = write_atomic(p, format!("{text}\n").as_bytes()) {
                tracing::warn!(error = %e.kind(), "recent chat persist failed");
            }
        }
        n
    }

    /// Drop one buffer's turns (a self-test starting clean).
    pub fn clear(&self, chat_id: &str) {
        let snapshot = self.with(|map| {
            map.remove(chat_id)
                .and_then(|_| serde_json::to_string_pretty(map).ok())
        });
        if let (Some(p), Some(text)) = (&self.path, snapshot) {
            if let Err(e) = write_atomic(p, format!("{text}\n").as_bytes()) {
                tracing::warn!(error = %e.kind(), "recent chat persist failed");
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
        self.render_turns(&self.turns(chat_id), tz)
    }

    /// The "Recent chat turns" block over several buffers: `chat_id`'s own turns plus the turns of `also`
    /// (another buffer of the same person, e.g. the app's share-sheet session) that are newer than
    /// `since`, merged by time, the last TURNS_LIMIT kept. Turns without a time from `also` are left out.
    pub fn render_with(
        &self,
        chat_id: &str,
        also: &[&str],
        since: DateTime<Utc>,
        tz: Tz,
    ) -> String {
        let mut merged: Vec<(Option<DateTime<Utc>>, Turn)> = self
            .turns(chat_id)
            .into_iter()
            .map(|t| (t.at.as_deref().and_then(parse_js_date), t))
            .collect();
        for key in also {
            if *key == chat_id {
                continue;
            }
            for t in self.turns(key) {
                if let Some(at) = t.at.as_deref().and_then(parse_js_date) {
                    if at >= since {
                        merged.push((Some(at), t));
                    }
                }
            }
        }
        // Stable sort: untimed turns of `chat_id` count as the oldest and keep their order.
        merged.sort_by_key(|(at, _)| at.map(|d| d.timestamp_millis()).unwrap_or(i64::MIN));
        let turns: Vec<Turn> = merged
            .into_iter()
            .rev()
            .take(TURNS_LIMIT)
            .rev()
            .map(|(_, t)| t)
            .collect();
        self.render_turns(&turns, tz)
    }

    /// Render a slice of turns as the "Recent chat turns" block.
    fn render_turns(&self, turns: &[Turn], tz: Tz) -> String {
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
    // What she knows about him is never clipped away: a casual reply needs it most.
    if let Some(start) = memory.find(crate::about::SUMMARY_MARKER) {
        let end = memory[start..]
            .find("\n\n")
            .map_or(memory.len(), |e| start + e);
        let pinned: String = memory[start..end]
            .chars()
            .take(crate::about::SUMMARY_CHARS + 200)
            .collect();
        let rest = format!(
            "{}{}",
            &memory[..start],
            memory[end..].trim_start_matches('\n')
        );
        let rest = clip_memory_for_reply(rest.trim(), max_chars);
        return if rest.is_empty() {
            pinned
        } else {
            format!("{pinned}\n\n{rest}")
        };
    }
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

    #[test]
    fn render_with_merges_also_by_time() {
        let dir = tempfile::tempdir().unwrap();
        let r = RecentChat::new(Some(dir.path().join("recent.json")));
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap();
        r.append("main", "user", "first", t0);
        r.append(
            "main",
            "assistant",
            "third",
            t0 + chrono::Duration::seconds(20),
        );
        r.append(
            "share",
            "user",
            "second",
            t0 + chrono::Duration::seconds(10),
        );
        r.append(
            "share",
            "user",
            "too old",
            t0 - chrono::Duration::seconds(10),
        );
        let since = t0;
        let block = r.render_with("main", &["share"], since, tz);
        let body = block.lines().skip(3).collect::<Vec<_>>().join("\n");
        assert_eq!(
            body,
            "[21/10 17:00 Europe/Rome] user: first\n[21/10 17:00 Europe/Rome] user: second\n[21/10 17:00 Europe/Rome] assistant: third"
        );
    }

    #[test]
    fn render_with_skips_also_older_than_since() {
        let dir = tempfile::tempdir().unwrap();
        let r = RecentChat::new(Some(dir.path().join("recent.json")));
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap();
        r.append("main", "user", "mine", t0);
        r.append("share", "user", "old", t0 - chrono::Duration::seconds(1));
        let block = r.render_with("main", &["share"], t0, tz);
        assert!(block.contains("user: mine"));
        assert!(!block.contains("user: old"));
    }

    #[test]
    fn render_with_ignores_chat_id_in_also() {
        let dir = tempfile::tempdir().unwrap();
        let r = RecentChat::new(Some(dir.path().join("recent.json")));
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap();
        r.append("main", "user", "once", t0);
        let block = r.render_with("main", &["main"], t0, tz);
        assert_eq!(block.matches("once").count(), 1);
    }

    #[test]
    fn render_with_empty_also_equals_render() {
        let dir = tempfile::tempdir().unwrap();
        let r = RecentChat::new(Some(dir.path().join("recent.json")));
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap();
        r.append("main", "user", "a", t0);
        r.append("main", "assistant", "b", t0 + chrono::Duration::seconds(1));
        assert_eq!(r.render_with("main", &[], t0, tz), r.render("main", tz));
    }

    #[test]
    fn render_with_keeps_newest_turns_limit() {
        let dir = tempfile::tempdir().unwrap();
        let r = RecentChat::new(Some(dir.path().join("recent.json")));
        let tz: Tz = "Europe/Rome".parse().unwrap();
        let t0 = Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap();
        for i in 0..TURNS_LIMIT {
            r.append(
                "main",
                "user",
                &format!("m{i}"),
                t0 + chrono::Duration::seconds(i as i64),
            );
        }
        for i in 0..5 {
            r.append(
                "share",
                "user",
                &format!("s{i}"),
                t0 + chrono::Duration::seconds(100 + i as i64),
            );
        }
        let block = r.render_with("main", &["share"], t0, tz);
        let body = block.lines().skip(3).collect::<Vec<_>>().join("\n");
        assert!(!body.contains("m0"));
        assert!(!body.contains("m4"));
        assert!(body.contains("m5"));
        assert!(body.contains("s4"));
        assert_eq!(body.lines().count(), TURNS_LIMIT);
    }

    #[test]
    fn clipping_keeps_what_she_knows_about_him() {
        let about = format!(
            "{} (what you know):\n- [af-1] (work) He works 12 h/week",
            crate::about::SUMMARY_MARKER
        );
        let recent = format!(
            "{MARKER} (most recent last):\n{}",
            "user: hello\n".repeat(300)
        );
        let memory = format!("meta line\n\n{about}\n\nNotes: x\n\n{recent}");
        let out = clip_memory_for_reply(&memory, 1600);
        assert!(out.starts_with(&about));
        assert!(out.contains(MARKER));
        assert_eq!(clip_memory_for_reply("short", 1600), "short");
    }
}
