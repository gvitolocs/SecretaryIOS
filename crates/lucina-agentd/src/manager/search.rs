//! Agent search (Paseo `agent-history-search.js`, `timeline-prompt-index.js`), plus
//! full text over what was said (Lucina: Paseo only matches names).
//!
//! - Names: Paseo's typo-tolerant ranking ([`lucina_agents::text_match`]) over the
//!   workspace name, the agent's title, the branch and the project.
//! - Timelines: SQLite FTS5 (`search_fts`) over the owner's prompts, the agents'
//!   answers and agent-to-agent messages, one row per projected entry. An agent is
//!   indexed when it is not running (so streamed answers are whole), lazily before
//!   each search and every 30 s; `search_state` keeps how far.
//!
//! Results: name matches first (best score first), then the agents whose timelines
//! match (best FTS rank first); ties go to the most recently updated.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use lucina_agents::jsstr;
use lucina_agents::projection::project_rows;
use lucina_agents::text_match::{compare, score_text_fields, tokenize, Score};
use lucina_agents::{AgentStatus, TimelineItem};
use rusqlite::params;
use serde::Serialize;

use super::{Manager, ManagerError};
use crate::store::{Store, StoreError};

pub(super) fn init(store: &Store) -> Result<(), StoreError> {
    store.with_db(|db| {
        db.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS search_fts USING fts5(
                 text, agent_id UNINDEXED, seq UNINDEXED, kind UNINDEXED,
                 tokenize = 'unicode61 remove_diacritics 2'
             );
             CREATE TABLE IF NOT EXISTS search_state (agent_id TEXT PRIMARY KEY, indexed_seq INTEGER NOT NULL);",
        )
    })?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMatch {
    pub field: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub agent_id: String,
    pub title: Option<String>,
    pub provider: String,
    pub status: AgentStatus,
    pub cwd: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// Lower is better: name tier sum, or 100 + the FTS rank for timeline-only hits.
    pub score: f64,
    pub matches: Vec<SearchMatch>,
}

struct IndexEntry {
    seq: u64,
    kind: &'static str,
    text: String,
}

/// An agent's rows to index, up to `last`.
struct Pending {
    agent: String,
    last: u64,
    entries: Vec<IndexEntry>,
}

struct FtsHit {
    agent: String,
    seq: i64,
    kind: String,
    snippet: String,
    rank: f64,
}

/// One prompt in Paseo's prompt index.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptEntry {
    pub seq: u64,
    pub timestamp: String,
    pub preview: String,
    /// Lucina only: the message's id, to rewind to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

const PREVIEW_MAX: usize = 120;

/// Paseo `promptPreview`.
pub fn prompt_preview(text: &str) -> String {
    let collapsed = jsstr::collapse_spaces(text);
    let collapsed = jsstr::trim(&collapsed);
    if jsstr::len(collapsed) <= PREVIEW_MAX {
        return collapsed.to_string();
    }
    format!("{}…", jsstr::slice(collapsed, 0, PREVIEW_MAX - 1))
}

/// An FTS5 query: every token as a quoted prefix, all required.
fn fts_query(q: &str) -> Option<String> {
    // The unicode61 tokenizer splits on anything but letters and digits; so do we.
    let tokens: Vec<String> = tokenize(q)
        .iter()
        .flat_map(|t| t.split(|c: char| !c.is_alphanumeric()).map(str::to_string).collect::<Vec<_>>())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();
    (!tokens.is_empty()).then(|| tokens.join(" AND "))
}

impl Manager {
    /// Paseo `buildTimelinePromptIndex`.
    pub fn prompts(&self, id: &str) -> Result<(String, Vec<PromptEntry>), ManagerError> {
        let agents = self.lock();
        let a = agents.get(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        let prompts = a
            .rows
            .iter()
            .filter_map(|r| match &r.item {
                TimelineItem::UserMessage { text, message_id, .. } => {
                    Some(PromptEntry { seq: r.seq, timestamp: r.timestamp.clone(), preview: prompt_preview(text), message_id: message_id.clone() })
                }
                _ => None,
            })
            .collect();
        Ok((a.record.epoch.clone(), prompts))
    }

    /// Drop what is indexed of an agent whose timeline was replaced (a rewind), so the
    /// next pass indexes the new one.
    pub(super) fn forget_index(&self, id: &str) {
        let r = self.inner.store.with_db(|db| -> rusqlite::Result<()> {
            db.execute("DELETE FROM search_fts WHERE agent_id = ?1", params![id])?;
            db.execute("DELETE FROM search_state WHERE agent_id = ?1", params![id])?;
            Ok(())
        });
        if let Err(e) = r {
            tracing::warn!(agent = id, error = %e, "search index reset failed");
        }
    }

    /// Index the timeline rows of idle agents that aren't indexed yet.
    pub fn index_pending(&self) {
        let done: HashMap<String, u64> = self
            .inner
            .store
            .with_db(|db| -> rusqlite::Result<HashMap<String, u64>> {
                let mut stmt = db.prepare("SELECT agent_id, indexed_seq FROM search_state")?;
                let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))?;
                Ok(rows.flatten().collect())
            })
            .unwrap_or_default();
        let mut batch: Vec<Pending> = Vec::new();
        {
            let agents = self.lock();
            for a in agents.values() {
                if a.record.internal || matches!(a.k.status, AgentStatus::Running | AgentStatus::Initializing) {
                    continue;
                }
                let from = done.get(&a.record.id).copied().unwrap_or(0);
                let Some(last) = a.rows.last().map(|r| r.seq) else { continue };
                if last <= from {
                    continue;
                }
                let rows: Vec<_> = a.rows.iter().filter(|r| r.seq > from).cloned().collect();
                let entries = project_rows(&rows)
                    .into_iter()
                    .filter_map(|e| {
                        let (kind, text) = match e.item {
                            TimelineItem::UserMessage { text, .. } => ("user_message", text),
                            TimelineItem::AssistantMessage { text, .. } if !text.starts_with(lucina_agents::events::SYSTEM_ERROR_PREFIX) => ("assistant_message", text),
                            TimelineItem::ToolCall(t) => match t.agent_message {
                                Some(m) => ("agent_message", m.text),
                                None => return None,
                            },
                            _ => return None,
                        };
                        (!text.trim().is_empty()).then_some(IndexEntry { seq: e.seq_start, kind, text })
                    })
                    .collect();
                batch.push(Pending { agent: a.record.id.clone(), last, entries });
            }
        }
        let known: Vec<String> = self.lock().keys().cloned().collect();
        let gone: Vec<&String> = done.keys().filter(|k| !known.contains(k)).collect();
        let r = self.inner.store.with_db(|db| -> rusqlite::Result<()> {
            let tx = db.unchecked_transaction()?;
            for p in &batch {
                for e in &p.entries {
                    tx.execute("INSERT INTO search_fts (text, agent_id, seq, kind) VALUES (?1, ?2, ?3, ?4)", params![e.text, p.agent, e.seq as i64, e.kind])?;
                }
                tx.execute(
                    "INSERT INTO search_state (agent_id, indexed_seq) VALUES (?1, ?2) ON CONFLICT(agent_id) DO UPDATE SET indexed_seq = excluded.indexed_seq",
                    params![p.agent, p.last as i64],
                )?;
            }
            for g in &gone {
                tx.execute("DELETE FROM search_fts WHERE agent_id = ?1", params![g])?;
                tx.execute("DELETE FROM search_state WHERE agent_id = ?1", params![g])?;
            }
            tx.commit()
        });
        if let Err(e) = r {
            tracing::warn!(error = %e, "search index update failed");
        }
    }

    pub fn spawn_indexer(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut closing = manager.closing();
            loop {
                manager.index_pending();
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                    _ = closing.wait_for(|c| *c) => return,
                }
            }
        });
    }

    /// Search agents by name and by what was said (module docs).
    pub fn search(&self, q: &str, limit: usize, include_archived: bool) -> Vec<SearchResult> {
        if q.trim().is_empty() {
            return Vec::new();
        }
        self.index_pending();
        let limit = limit.clamp(1, 100);
        struct Hit {
            name: Option<Score>,
            fts: Option<f64>,
            matches: Vec<SearchMatch>,
        }
        let mut hits: BTreeMap<String, Hit> = BTreeMap::new();
        // Each visible agent: its result (no matches yet) and its workspace.
        let visible: HashMap<String, (SearchResult, Option<String>)> = self
            .lock()
            .values()
            .filter(|a| !a.record.internal && (include_archived || a.record.archived_at.is_none()))
            .map(|a| {
                let r = &a.record;
                let base = SearchResult {
                    agent_id: r.id.clone(),
                    title: r.title.clone(),
                    provider: r.provider.clone(),
                    status: a.k.status,
                    cwd: r.cwd.clone(),
                    updated_at: r.updated_at.clone(),
                    archived_at: r.archived_at.clone(),
                    score: 0.0,
                    matches: Vec::new(),
                };
                (r.id.clone(), (base, r.workspace_id.clone()))
            })
            .collect();
        for (id, (base, ws)) in &visible {
            let w = ws.as_deref().and_then(|w| self.inner.workspaces.get(w));
            let workspace = w.as_ref().map(|w| w.title.clone().unwrap_or_else(|| w.display_name.clone())).unwrap_or_default();
            let branch = w.as_ref().and_then(|w| w.branch.clone()).unwrap_or_default();
            let project = w.as_ref().map(|w| self.inner.workspaces.project_name(w)).unwrap_or_default();
            let title = base.title.clone().unwrap_or_default();
            let fields = [workspace.as_str(), title.as_str(), branch.as_str(), project.as_str()];
            if let Some(score) = score_text_fields(q, &fields, true) {
                let mut matches = Vec::new();
                for (name, value) in [("title", &title), ("workspace", &workspace), ("branch", &branch), ("project", &project)] {
                    if !value.is_empty() && score_text_fields(q, &[value.as_str()], true).is_some() {
                        matches.push(SearchMatch { field: name, seq: None, kind: None, snippet: value.clone() });
                    }
                }
                hits.insert(id.clone(), Hit { name: Some(score), fts: None, matches });
            }
        }
        if let Some(fts) = fts_query(q) {
            let rows = self
                .inner
                .store
                .with_db(|db| -> rusqlite::Result<Vec<FtsHit>> {
                    let mut stmt = db.prepare(
                        "SELECT agent_id, seq, kind, snippet(search_fts, 0, '[', ']', '…', 16), bm25(search_fts)
                         FROM search_fts WHERE search_fts MATCH ?1 ORDER BY bm25(search_fts) LIMIT 500",
                    )?;
                    let rows = stmt.query_map(params![fts], |r| Ok(FtsHit { agent: r.get(0)?, seq: r.get(1)?, kind: r.get(2)?, snippet: r.get(3)?, rank: r.get(4)? }))?;
                    Ok(rows.flatten().collect())
                })
                .unwrap_or_default();
            for FtsHit { agent, seq, kind, snippet, rank } in rows {
                if !visible.contains_key(&agent) {
                    continue;
                }
                let hit = hits.entry(agent).or_insert(Hit { name: None, fts: None, matches: Vec::new() });
                hit.fts = Some(hit.fts.map_or(rank, |r| r.min(rank)));
                if hit.matches.iter().filter(|m| m.field == "timeline").count() < 3 {
                    hit.matches.push(SearchMatch { field: "timeline", seq: Some(seq as u64), kind: Some(kind), snippet });
                }
            }
        }
        let mut out: Vec<(Option<Score>, f64, SearchResult)> = hits
            .into_iter()
            .filter_map(|(id, h)| {
                let mut result = visible.get(&id)?.0.clone();
                result.score = match h.name {
                    Some(s) => s.tier as f64,
                    None => 100.0 + h.fts.unwrap_or(0.0),
                };
                result.matches = h.matches;
                Some((h.name, h.fts.unwrap_or(0.0), result))
            })
            .collect();
        out.sort_by(|a, b| match (&a.0, &b.0) {
            (Some(x), Some(y)) => compare(x, y).then_with(|| b.2.updated_at.cmp(&a.2.updated_at)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| b.2.updated_at.cmp(&a.2.updated_at)),
        });
        out.into_iter().take(limit).map(|(_, _, r)| r).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_and_queries() {
        assert_eq!(prompt_preview("  fix\n\n the   bug "), "fix the bug");
        let long = "a".repeat(130);
        let p = prompt_preview(&long);
        assert_eq!(jsstr::len(&p), 120);
        assert!(p.ends_with('…'));
        assert_eq!(fts_query("scroll \"anchor\" x-y").as_deref(), Some("\"scroll\"* AND \"anchor\"* AND \"x\"* AND \"y\"*"));
        assert_eq!(fts_query("  \"\" "), None);
    }
}
