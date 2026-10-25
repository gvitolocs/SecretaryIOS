//! What Lucina read from every shared file, kept so she can answer later
//! ("in the PDF I sent yesterday…"): `attachments.db` next to the shares,
//! with an FTS5 index over the name, text and summary.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;

/// Characters of one attachment's text in the turn context.
const CONTEXT_PER_ITEM: usize = 3_000;
/// Characters of all earlier attachments in the turn context.
const CONTEXT_TOTAL: usize = 9_000;
const RECENT_MS: i64 = 48 * 3600 * 1000;

/// One stored extraction.
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Record {
    pub id: String,
    pub share_id: String,
    pub media_id: Option<String>,
    pub item_id: Option<String>,
    pub name: String,
    pub kind: String,
    pub subtype: Option<String>,
    /// Unix milliseconds.
    pub created_at: i64,
    pub text: String,
    pub summary: Option<String>,
    pub structured: Value,
}

/// Opened on first use, so turns without shares never create the file.
pub struct Memory {
    path: PathBuf,
    conn: OnceLock<Result<Mutex<Connection>, String>>,
}

impl Memory {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            conn: OnceLock::new(),
        }
    }

    /// Open now (and fail now): for tests and callers that need the error.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let m = Self::new(path.to_owned());
        drop(m.conn()?);
        Ok(m)
    }

    fn connect(path: &Path) -> anyhow::Result<Connection> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS attachments (
                 id TEXT PRIMARY KEY, share_id TEXT NOT NULL, media_id TEXT, item_id TEXT,
                 name TEXT NOT NULL, kind TEXT NOT NULL, subtype TEXT, created_at INTEGER NOT NULL,
                 text TEXT NOT NULL, summary TEXT, structured TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS attachments_created ON attachments (created_at);
             CREATE VIRTUAL TABLE IF NOT EXISTS attachments_fts USING fts5(
                 id UNINDEXED, name, text, summary, tokenize = 'unicode61 remove_diacritics 2');",
        )?;
        Ok(conn)
    }

    fn conn(&self) -> anyhow::Result<std::sync::MutexGuard<'_, Connection>> {
        match self
            .conn
            .get_or_init(|| Self::connect(&self.path).map(Mutex::new).map_err(|e| e.to_string()))
        {
            Ok(m) => Ok(m.lock().unwrap_or_else(|e| e.into_inner())),
            Err(e) => anyhow::bail!("attachment memory unavailable: {e}"),
        }
    }

    /// Insert or replace (a reprocessed share keeps its ids).
    pub fn put(&self, r: &Record) -> anyhow::Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR REPLACE INTO attachments
             (id, share_id, media_id, item_id, name, kind, subtype, created_at, text, summary, structured)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                r.id,
                r.share_id,
                r.media_id,
                r.item_id,
                r.name,
                r.kind,
                r.subtype,
                r.created_at,
                r.text,
                r.summary,
                r.structured.to_string()
            ],
        )?;
        tx.execute("DELETE FROM attachments_fts WHERE id = ?1", params![r.id])?;
        tx.execute(
            "INSERT INTO attachments_fts (id, name, text, summary) VALUES (?1, ?2, ?3, ?4)",
            params![r.id, r.name, r.text, r.summary.as_deref().unwrap_or("")],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> anyhow::Result<Option<Record>> {
        Ok(self
            .conn()?
            .query_row(
                &format!("SELECT {COLUMNS} FROM attachments WHERE id = ?1"),
                params![id],
                row,
            )
            .optional()?)
    }

    /// Best matches for free text (any word), newest first among equals.
    /// `kind` and `since_ms` narrow the search.
    pub fn search(
        &self,
        query: &str,
        kind: Option<&str>,
        since_ms: Option<i64>,
        limit: usize,
    ) -> anyhow::Result<Vec<Record>> {
        let fts = fts_query(query);
        let conn = self.conn()?;
        let mut out = Vec::new();
        if fts.is_empty() {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM attachments
                 WHERE (?1 IS NULL OR kind = ?1) AND created_at >= ?2
                 ORDER BY created_at DESC LIMIT ?3"
            ))?;
            let rows = stmt.query_map(
                params![kind, since_ms.unwrap_or(0), limit as i64],
                row,
            )?;
            for r in rows {
                out.push(r?);
            }
            return Ok(out);
        }
        let mut stmt = conn.prepare(&format!(
            "SELECT {PREFIXED} FROM attachments_fts f JOIN attachments a ON a.id = f.id
             WHERE attachments_fts MATCH ?1 AND (?2 IS NULL OR a.kind = ?2) AND a.created_at >= ?3
             ORDER BY bm25(attachments_fts), a.created_at DESC LIMIT ?4"
        ))?;
        let rows = stmt.query_map(
            params![fts, kind, since_ms.unwrap_or(0), limit as i64],
            row,
        )?;
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Attachments shared after `since_ms`, newest first.
    pub fn recent(&self, since_ms: i64, limit: usize) -> anyhow::Result<Vec<Record>> {
        self.search("", None, Some(since_ms), limit)
    }

    pub fn is_empty(&self) -> bool {
        if !self.path.exists() {
            return true;
        }
        let Ok(conn) = self.conn() else {
            return true;
        };
        conn.query_row("SELECT EXISTS (SELECT 1 FROM attachments)", [], |r| {
                r.get::<_, bool>(0)
            })
            .map(|any| !any)
            .unwrap_or(true)
    }

    /// The "Earlier attachments" block for a normal chat turn, or "" when the
    /// message doesn't talk about files or nothing matches.
    pub fn context_block(&self, message: &str, now_ms: i64) -> String {
        if !mentions_attachment(message) || self.is_empty() {
            return String::new();
        }
        let mut picked: Vec<Record> = self.search(message, None, None, 3).unwrap_or_default();
        for r in self.recent(now_ms - RECENT_MS, 3).unwrap_or_default() {
            if !picked.iter().any(|p| p.id == r.id) {
                picked.push(r);
            }
        }
        if picked.is_empty() {
            return String::new();
        }
        let mut out = String::from(
            "Earlier attachments the owner shared (what Lucina read from them; use them when the owner refers to a file):",
        );
        let mut budget = CONTEXT_TOTAL;
        for r in picked {
            if budget < 200 {
                break;
            }
            let body = r.summary.clone().filter(|s| !s.is_empty()).unwrap_or(r.text.clone());
            let body: String = body.chars().take(CONTEXT_PER_ITEM.min(budget)).collect();
            budget = budget.saturating_sub(body.chars().count());
            let when = chrono::DateTime::from_timestamp_millis(r.created_at)
                .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_default();
            out.push_str(&format!(
                "\n- [{}] {} ({}{}), shared {when}:\n{body}",
                r.id,
                r.name,
                r.kind,
                r.subtype.as_deref().map(|s| format!(", {s}")).unwrap_or_default(),
            ));
            if !r.structured.is_null() {
                let s: String = r.structured.to_string().chars().take(800).collect();
                out.push_str(&format!("\n  data: {s}"));
            }
        }
        out
    }
}

const COLUMNS: &str =
    "id, share_id, media_id, item_id, name, kind, subtype, created_at, text, summary, structured";
const PREFIXED: &str = "a.id, a.share_id, a.media_id, a.item_id, a.name, a.kind, a.subtype, a.created_at, a.text, a.summary, a.structured";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Record> {
    Ok(Record {
        id: r.get(0)?,
        share_id: r.get(1)?,
        media_id: r.get(2)?,
        item_id: r.get(3)?,
        name: r.get(4)?,
        kind: r.get(5)?,
        subtype: r.get(6)?,
        created_at: r.get(7)?,
        text: r.get(8)?,
        summary: r.get(9)?,
        structured: serde_json::from_str(&r.get::<_, String>(10)?).unwrap_or(Value::Null),
    })
}

/// Words of three or more letters, quoted and OR-ed (FTS5 syntax can't leak in).
fn fts_query(text: &str) -> String {
    let mut words: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3 && !STOP_WORDS.contains(&w.to_lowercase().as_str()))
        .map(|w| format!("\"{}\"", w.to_lowercase()))
        .collect();
    words.dedup();
    words.truncate(24);
    words.join(" OR ")
}

const STOP_WORDS: &[&str] = &[
    "the", "and", "that", "this", "what", "with", "from", "for", "you", "your", "sent", "che", "per",
    "con", "del", "della", "nel", "una", "uno", "cosa", "come", "sono", "hai", "ieri", "mio", "mia",
    "det", "der", "som", "jeg", "har", "til", "hvad", "med", "yesterday", "file", "send",
];

/// Whether a chat message talks about something the owner shared.
pub fn mentions_attachment(message: &str) -> bool {
    let m = message.to_lowercase();
    WORDS.iter().any(|w| {
        m.split(|c: char| !c.is_alphanumeric())
            .any(|token| token == *w || (w.len() >= 5 && token.starts_with(w)))
    })
}

const WORDS: &[&str] = &[
    // it
    "pdf", "foto", "fotografia", "immagine", "screenshot", "allegato", "allegati", "file",
    "documento", "fattura", "bolletta", "scontrino", "ricevuta", "video", "audio", "vocale",
    "link", "articolo", "contratto", "tabella", "foglio", "condiviso", "mandato", "inviato",
    // en
    "photo", "picture", "image", "attachment", "document", "invoice", "receipt", "bill",
    "spreadsheet", "recording", "voice", "shared", "screenshots", "slides",
    // da
    "billede", "vedhæft", "dokument", "faktura", "kvittering", "regning", "optagelse",
    "delte", "artikel",
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rec(id: &str, name: &str, kind: &str, text: &str, at: i64) -> Record {
        Record {
            id: id.into(),
            share_id: "s".into(),
            name: name.into(),
            kind: kind.into(),
            created_at: at,
            text: text.into(),
            structured: json!({"total": "84,30"}),
            ..Record::default()
        }
    }

    #[test]
    fn stores_searches_and_builds_context() {
        let dir = tempfile::tempdir().unwrap();
        let m = Memory::open(&dir.path().join("attachments.db")).unwrap();
        assert!(m.is_empty());
        assert_eq!(m.context_block("cosa dice il pdf?", 10), "");
        let day = 24 * 3600 * 1000;
        m.put(&rec("a1", "bolletta-enel.pdf", "pdf", "Totale da pagare € 84,30 Scadenza 15/11/2026", 10 * day))
            .unwrap();
        m.put(&rec("a2", "IMG_1.jpg", "image", "A cat on a sofa", 11 * day)).unwrap();
        m.put(&rec("a3", "old.docx", "document", "Contratto di affitto, canone mensile", day))
            .unwrap();
        assert!(!m.is_empty());
        // Replacing keeps one row and one index entry.
        m.put(&rec("a2", "IMG_1.jpg", "image", "A grey cat on a sofa", 11 * day)).unwrap();
        assert_eq!(m.search("cat", None, None, 10).unwrap().len(), 1);

        let hits = m.search("quanto devo pagare la bolletta?", None, None, 5).unwrap();
        assert_eq!(hits[0].id, "a1");
        assert!(m.search("contratto", Some("pdf"), None, 5).unwrap().is_empty());
        assert_eq!(m.search("contratto", Some("document"), None, 5).unwrap()[0].id, "a3");
        // Quotes and FTS operators in the message are just words.
        assert!(m.search("\"NEAR( OR * -", None, None, 5).is_ok());

        let ctx = m.context_block("nel PDF che ti ho mandato ieri, quanto devo pagare?", 12 * day);
        assert!(ctx.contains("bolletta-enel.pdf") && ctx.contains("84,30"), "{ctx}");
        // The recent photo comes along (last 48 h), the old contract does not.
        assert!(ctx.contains("IMG_1.jpg") && !ctx.contains("old.docx"), "{ctx}");
        assert_eq!(m.context_block("come stai oggi?", 12 * day), "");
        assert_eq!(m.get("a1").unwrap().unwrap().structured["total"], "84,30");
    }

    #[test]
    fn attachment_words() {
        for s in ["Il PDF di ieri?", "the invoice I sent", "den vedhæftede faktura", "quella fotografia"] {
            assert!(mentions_attachment(s), "{s}");
        }
        for s in ["ciao Lucina", "remind me at 9", "hvordan går det"] {
            assert!(!mentions_attachment(s), "{s}");
        }
    }
}
