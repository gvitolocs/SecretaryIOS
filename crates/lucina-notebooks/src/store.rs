//! The notebooks database: one SQLite file (`notebooks.db`) with the rows, the passages
//! (`chunks`), their full-text index (`chunks_fts`, FTS5) and their vectors (`chunks.vec`,
//! little-endian f32, unit length).
//!
//! Notes are not stored here: a notebook keeps the slugs of Keep notes on p1
//! (`notebook_notes`) and their passages, so the one note store stays the only copy.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ids::now;
use crate::Error;

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS notebooks (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    archived INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sources (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    origin TEXT NOT NULL,
    status TEXT NOT NULL,
    error TEXT,
    text TEXT NOT NULL DEFAULT '',
    pages INTEGER,
    duration_s REAL,
    file_name TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    transformations TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS notebook_sources (
    notebook_id TEXT NOT NULL REFERENCES notebooks(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    context TEXT NOT NULL DEFAULT 'full',
    added_at TEXT NOT NULL,
    PRIMARY KEY (notebook_id, source_id)
);
CREATE TABLE IF NOT EXISTS notebook_notes (
    notebook_id TEXT NOT NULL REFERENCES notebooks(id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    excerpt TEXT NOT NULL DEFAULT '',
    note_updated_at TEXT,
    synced_at TEXT,
    added_at TEXT NOT NULL,
    PRIMARY KEY (notebook_id, slug)
);
CREATE TABLE IF NOT EXISTS chunks (
    rowid INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    owner TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    text TEXT NOT NULL,
    page INTEGER,
    start_s REAL,
    vec BLOB
);
CREATE INDEX IF NOT EXISTS chunks_owner ON chunks(owner, ordinal);
CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
    text, content='chunks', content_rowid='rowid', tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
    INSERT INTO chunks_fts(rowid, text) VALUES (new.rowid, new.text);
END;
CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
    INSERT INTO chunks_fts(chunks_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
END;
CREATE TABLE IF NOT EXISTS chats (
    id TEXT PRIMARY KEY,
    notebook_id TEXT NOT NULL REFERENCES notebooks(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    text TEXT NOT NULL,
    citations TEXT NOT NULL DEFAULT '[]',
    model TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS messages_chat ON messages(chat_id, created_at);
CREATE TABLE IF NOT EXISTS transformations (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    prompt TEXT NOT NULL,
    apply_default INTEGER NOT NULL DEFAULT 0,
    builtin INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS insights (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    transformation_id TEXT NOT NULL,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    error TEXT,
    text TEXT NOT NULL DEFAULT '',
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS podcasts (
    id TEXT PRIMARY KEY,
    notebook_id TEXT NOT NULL REFERENCES notebooks(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    episode_profile TEXT NOT NULL,
    speaker_profile TEXT NOT NULL,
    briefing TEXT NOT NULL,
    minutes REAL NOT NULL,
    status TEXT NOT NULL,
    error TEXT,
    outline TEXT NOT NULL DEFAULT '[]',
    transcript TEXT NOT NULL DEFAULT '[]',
    duration_s REAL,
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
"#;

pub struct Store {
    conn: Mutex<Connection>,
}

// ------------------------------------------------------------------ rows

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Counts {
    pub sources: i64,
    pub notes: i64,
    pub chats: i64,
    pub podcasts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Notebook {
    pub id: String,
    pub name: String,
    pub description: String,
    pub archived: bool,
    pub created_at: String,
    pub updated_at: String,
    pub counts: Counts,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Source {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub origin: String,
    pub status: String,
    pub error: Option<String>,
    pub chars: i64,
    pub chunks: i64,
    pub pages: Option<i64>,
    pub duration_s: Option<f64>,
    pub has_file: bool,
    pub created_at: String,
    pub updated_at: String,
    pub notebooks: Vec<String>,
    /// In a notebook listing: `full`, `summary` or `off`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip)]
    pub file_name: Option<String>,
    #[serde(skip)]
    pub transformations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoteLink {
    pub slug: String,
    pub title: String,
    pub excerpt: String,
    pub updated_at: Option<String>,
    pub indexed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Chat {
    pub id: String,
    pub notebook_id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub messages: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Citation {
    #[serde(rename = "ref")]
    pub reference: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insight_id: Option<String>,
    pub title: String,
    pub excerpt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_s: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub text: String,
    pub citations: Vec<Citation>,
    pub model: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Transformation {
    pub id: String,
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub apply_default: bool,
    pub builtin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Insight {
    pub id: String,
    pub source_id: String,
    pub transformation_id: String,
    pub title: String,
    pub status: String,
    pub error: Option<String>,
    pub text: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Podcast {
    pub id: String,
    pub notebook_id: String,
    pub title: String,
    pub episode_profile: String,
    pub speaker_profile: String,
    pub briefing: String,
    pub minutes: f64,
    pub status: String,
    pub error: Option<String>,
    pub outline: Value,
    pub transcript: Value,
    pub duration_s: Option<f64>,
    pub audio: bool,
    pub created_at: String,
}

/// One passage, as stored and as search returns it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Chunk {
    pub id: String,
    pub owner: String,
    pub ordinal: i64,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_s: Option<f64>,
}

/// A passage to store: text plus where it came from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewChunk {
    pub text: String,
    pub page: Option<i64>,
    pub start_s: Option<f64>,
}

pub const SOURCE_FINAL: [&str; 2] = ["ready", "failed"];

fn source_row(r: &Row) -> rusqlite::Result<Source> {
    let transformations: String = r.get("transformations")?;
    let file_name: Option<String> = r.get("file_name")?;
    Ok(Source {
        id: r.get("id")?,
        kind: r.get("kind")?,
        title: r.get("title")?,
        origin: r.get("origin")?,
        status: r.get("status")?,
        error: r.get("error")?,
        chars: r.get("chars")?,
        chunks: r.get("nchunks")?,
        pages: r.get("pages")?,
        duration_s: r.get("duration_s")?,
        has_file: file_name.is_some(),
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
        notebooks: Vec::new(),
        context: None,
        text: None,
        file_name,
        transformations: serde_json::from_str(&transformations).unwrap_or_default(),
    })
}

const SOURCE_COLS: &str = "s.id, s.kind, s.title, s.origin, s.status, s.error, length(s.text) AS chars, \
     (SELECT count(*) FROM chunks c WHERE c.owner = s.id) AS nchunks, s.pages, s.duration_s, s.file_name, \
     s.transformations, s.created_at, s.updated_at";

fn insight_row(r: &Row) -> rusqlite::Result<Insight> {
    Ok(Insight {
        id: r.get("id")?,
        source_id: r.get("source_id")?,
        transformation_id: r.get("transformation_id")?,
        title: r.get("title")?,
        status: r.get("status")?,
        error: r.get("error")?,
        text: r.get("text")?,
        created_at: r.get("created_at")?,
    })
}

fn podcast_row(r: &Row) -> rusqlite::Result<Podcast> {
    let outline: String = r.get("outline")?;
    let transcript: String = r.get("transcript")?;
    let status: String = r.get("status")?;
    Ok(Podcast {
        id: r.get("id")?,
        notebook_id: r.get("notebook_id")?,
        title: r.get("title")?,
        episode_profile: r.get("episode_profile")?,
        speaker_profile: r.get("speaker_profile")?,
        briefing: r.get("briefing")?,
        minutes: r.get("minutes")?,
        audio: status == "ready",
        status,
        error: r.get("error")?,
        outline: serde_json::from_str(&outline).unwrap_or(Value::Array(vec![])),
        transcript: serde_json::from_str(&transcript).unwrap_or(Value::Array(vec![])),
        duration_s: r.get("duration_s")?,
        created_at: r.get("created_at")?,
    })
}

fn transformation_row(r: &Row) -> rusqlite::Result<Transformation> {
    Ok(Transformation {
        id: r.get("id")?,
        name: r.get("name")?,
        description: r.get("description")?,
        prompt: r.get("prompt")?,
        apply_default: r.get::<_, i64>("apply_default")? != 0,
        builtin: r.get::<_, i64>("builtin")? != 0,
    })
}

fn chunk_row(r: &Row) -> rusqlite::Result<Chunk> {
    Ok(Chunk {
        id: r.get("id")?,
        owner: r.get("owner")?,
        ordinal: r.get("ordinal")?,
        text: r.get("text")?,
        page: r.get("page")?,
        start_s: r.get("start_s")?,
    })
}

pub fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn blob_to_vec(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, Error> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn in_memory() -> Result<Self, Error> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, Error> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ------------------------------------------------------------ notebooks

    pub fn create_notebook(&self, id: &str, name: &str, description: &str) -> Result<Notebook, Error> {
        let t = now();
        self.conn().execute(
            "INSERT INTO notebooks (id, name, description, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, name, description, t],
        )?;
        self.notebook(id)
    }

    pub fn notebook(&self, id: &str) -> Result<Notebook, Error> {
        self.notebooks_where("n.id = ?1", params![id])?
            .into_iter()
            .next()
            .ok_or(Error::NotFound("notebook"))
    }

    /// `archived`: `Some(false)` only active, `Some(true)` only archived, `None` both.
    pub fn notebooks(&self, archived: Option<bool>) -> Result<Vec<Notebook>, Error> {
        match archived {
            Some(a) => self.notebooks_where("n.archived = ?1", params![a as i64]),
            None => self.notebooks_where("1 = 1", params![]),
        }
    }

    fn notebooks_where(&self, cond: &str, p: &[&dyn rusqlite::ToSql]) -> Result<Vec<Notebook>, Error> {
        let conn = self.conn();
        let sql = format!(
            "SELECT n.*, \
             (SELECT count(*) FROM notebook_sources x WHERE x.notebook_id = n.id) AS ns, \
             (SELECT count(*) FROM notebook_notes x WHERE x.notebook_id = n.id) AS nn, \
             (SELECT count(*) FROM chats x WHERE x.notebook_id = n.id) AS nc, \
             (SELECT count(*) FROM podcasts x WHERE x.notebook_id = n.id) AS np \
             FROM notebooks n WHERE {cond} ORDER BY n.updated_at DESC"
        );
        let mut st = conn.prepare(&sql)?;
        let rows = st.query_map(p, |r| {
            Ok(Notebook {
                id: r.get("id")?,
                name: r.get("name")?,
                description: r.get("description")?,
                archived: r.get::<_, i64>("archived")? != 0,
                created_at: r.get("created_at")?,
                updated_at: r.get("updated_at")?,
                counts: Counts {
                    sources: r.get("ns")?,
                    notes: r.get("nn")?,
                    chats: r.get("nc")?,
                    podcasts: r.get("np")?,
                },
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn update_notebook(
        &self,
        id: &str,
        name: Option<&str>,
        description: Option<&str>,
        archived: Option<bool>,
    ) -> Result<Notebook, Error> {
        let n = self.conn().execute(
            "UPDATE notebooks SET name = coalesce(?2, name), description = coalesce(?3, description), \
             archived = coalesce(?4, archived), updated_at = ?5 WHERE id = ?1",
            params![id, name, description, archived.map(|a| a as i64), now()],
        )?;
        if n == 0 {
            return Err(Error::NotFound("notebook"));
        }
        self.notebook(id)
    }

    pub fn touch_notebook(&self, id: &str) -> Result<(), Error> {
        self.conn()
            .execute("UPDATE notebooks SET updated_at = ?2 WHERE id = ?1", params![id, now()])?;
        Ok(())
    }

    /// Sources only in this notebook (they go with it when asked).
    pub fn exclusive_sources(&self, notebook_id: &str) -> Result<Vec<String>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT source_id FROM notebook_sources a WHERE notebook_id = ?1 AND NOT EXISTS \
             (SELECT 1 FROM notebook_sources b WHERE b.source_id = a.source_id AND b.notebook_id != ?1)",
        )?;
        let ids = st.query_map(params![notebook_id], |r| r.get(0))?;
        Ok(ids.collect::<Result<_, _>>()?)
    }

    pub fn delete_notebook(&self, id: &str) -> Result<(), Error> {
        let n = self.conn().execute("DELETE FROM notebooks WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(Error::NotFound("notebook"));
        }
        Ok(())
    }

    // ------------------------------------------------------------ sources

    #[allow(clippy::too_many_arguments)]
    pub fn create_source(
        &self,
        id: &str,
        kind: &str,
        title: &str,
        origin: &str,
        status: &str,
        text: &str,
        file_name: Option<&str>,
        transformations: &[String],
    ) -> Result<(), Error> {
        let t = now();
        self.conn().execute(
            "INSERT INTO sources (id, kind, title, origin, status, text, file_name, transformations, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
            params![
                id,
                kind,
                title,
                origin,
                status,
                text,
                file_name,
                serde_json::to_string(transformations).unwrap_or_else(|_| "[]".into()),
                t
            ],
        )?;
        Ok(())
    }

    pub fn link_source(&self, notebook_id: &str, source_id: &str) -> Result<(), Error> {
        self.conn().execute(
            "INSERT OR IGNORE INTO notebook_sources (notebook_id, source_id, added_at) VALUES (?1, ?2, ?3)",
            params![notebook_id, source_id, now()],
        )?;
        self.touch_notebook(notebook_id)
    }

    /// Unlinks; deletes the source when no notebook has it any more. Returns whether it was deleted.
    pub fn unlink_source(&self, notebook_id: &str, source_id: &str) -> Result<bool, Error> {
        let conn = self.conn();
        let n = conn.execute(
            "DELETE FROM notebook_sources WHERE notebook_id = ?1 AND source_id = ?2",
            params![notebook_id, source_id],
        )?;
        if n == 0 {
            return Err(Error::NotFound("source"));
        }
        let left: i64 = conn.query_row(
            "SELECT count(*) FROM notebook_sources WHERE source_id = ?1",
            params![source_id],
            |r| r.get(0),
        )?;
        drop(conn);
        if left == 0 {
            self.delete_source(source_id)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn delete_source(&self, source_id: &str) -> Result<(), Error> {
        let conn = self.conn();
        let insights: Vec<String> = {
            let mut st = conn.prepare("SELECT id FROM insights WHERE source_id = ?1")?;
            let ids = st.query_map(params![source_id], |r| r.get(0))?;
            ids.collect::<Result<_, _>>()?
        };
        for i in insights {
            conn.execute("DELETE FROM chunks WHERE owner = ?1", params![i])?;
        }
        conn.execute("DELETE FROM chunks WHERE owner = ?1", params![source_id])?;
        conn.execute("DELETE FROM sources WHERE id = ?1", params![source_id])?;
        Ok(())
    }

    pub fn source(&self, id: &str, with_text: bool) -> Result<Source, Error> {
        let conn = self.conn();
        let mut s = conn
            .query_row(
                &format!("SELECT {SOURCE_COLS} FROM sources s WHERE s.id = ?1"),
                params![id],
                source_row,
            )
            .optional()?
            .ok_or(Error::NotFound("source"))?;
        let mut st = conn.prepare("SELECT notebook_id FROM notebook_sources WHERE source_id = ?1 ORDER BY added_at")?;
        s.notebooks = st
            .query_map(params![id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        if with_text {
            s.text = Some(conn.query_row("SELECT text FROM sources WHERE id = ?1", params![id], |r| r.get(0))?);
        }
        Ok(s)
    }

    pub fn source_text(&self, id: &str) -> Result<String, Error> {
        self.conn()
            .query_row("SELECT text FROM sources WHERE id = ?1", params![id], |r| r.get(0))
            .optional()?
            .ok_or(Error::NotFound("source"))
    }

    pub fn notebook_sources(&self, notebook_id: &str) -> Result<Vec<Source>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT {SOURCE_COLS}, ns.context FROM notebook_sources ns JOIN sources s ON s.id = ns.source_id \
             WHERE ns.notebook_id = ?1 ORDER BY ns.added_at DESC"
        ))?;
        let rows = st.query_map(params![notebook_id], |r| {
            let mut s = source_row(r)?;
            s.context = Some(r.get("context")?);
            Ok(s)
        })?;
        let mut out: Vec<Source> = rows.collect::<Result<_, _>>()?;
        let mut nb = conn.prepare("SELECT notebook_id FROM notebook_sources WHERE source_id = ?1 ORDER BY added_at")?;
        for s in &mut out {
            s.notebooks = nb.query_map(params![s.id], |r| r.get(0))?.collect::<Result<_, _>>()?;
        }
        Ok(out)
    }

    pub fn set_context(&self, notebook_id: &str, source_id: &str, context: &str) -> Result<(), Error> {
        let n = self.conn().execute(
            "UPDATE notebook_sources SET context = ?3 WHERE notebook_id = ?1 AND source_id = ?2",
            params![notebook_id, source_id, context],
        )?;
        if n == 0 {
            return Err(Error::NotFound("source"));
        }
        Ok(())
    }

    pub fn set_source_status(&self, id: &str, status: &str, error: Option<&str>) -> Result<(), Error> {
        self.conn().execute(
            "UPDATE sources SET status = ?2, error = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, status, error, now()],
        )?;
        Ok(())
    }

    pub fn bump_source_attempts(&self, id: &str) -> Result<i64, Error> {
        let conn = self.conn();
        conn.execute("UPDATE sources SET attempts = attempts + 1 WHERE id = ?1", params![id])?;
        Ok(conn.query_row("SELECT attempts FROM sources WHERE id = ?1", params![id], |r| r.get(0))?)
    }

    pub fn reset_source_attempts(&self, id: &str) -> Result<(), Error> {
        self.conn()
            .execute("UPDATE sources SET attempts = 0 WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn set_source_content(
        &self,
        id: &str,
        title: Option<&str>,
        text: &str,
        pages: Option<i64>,
        duration_s: Option<f64>,
    ) -> Result<(), Error> {
        self.conn().execute(
            "UPDATE sources SET title = coalesce(?2, title), text = ?3, pages = ?4, duration_s = ?5, updated_at = ?6 WHERE id = ?1",
            params![id, title, text, pages, duration_s, now()],
        )?;
        Ok(())
    }

    pub fn set_source_file(&self, id: &str, file_name: &str) -> Result<(), Error> {
        self.conn()
            .execute("UPDATE sources SET file_name = ?2 WHERE id = ?1", params![id, file_name])?;
        Ok(())
    }

    /// Sources that still have work to do (queued, or cut off by a restart).
    pub fn pending_sources(&self) -> Result<Vec<String>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id FROM sources WHERE status NOT IN ('ready', 'failed') ORDER BY created_at",
        )?;
        let ids = st.query_map([], |r| r.get(0))?;
        Ok(ids.collect::<Result<_, _>>()?)
    }

    // ------------------------------------------------------------ chunks

    /// Replaces an owner's passages.
    pub fn replace_chunks(&self, owner: &str, chunks: &[NewChunk]) -> Result<(), Error> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM chunks WHERE owner = ?1", params![owner])?;
        {
            let mut st = tx.prepare(
                "INSERT INTO chunks (id, owner, ordinal, text, page, start_s) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for (i, c) in chunks.iter().enumerate() {
                st.execute(params![format!("{owner}-{i}"), owner, i as i64, c.text, c.page, c.start_s])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_chunks(&self, owner: &str) -> Result<(), Error> {
        self.conn().execute("DELETE FROM chunks WHERE owner = ?1", params![owner])?;
        Ok(())
    }

    pub fn chunks_of(&self, owner: &str, from: i64, limit: i64) -> Result<Vec<Chunk>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, owner, ordinal, text, page, start_s FROM chunks WHERE owner = ?1 AND ordinal >= ?2 \
             ORDER BY ordinal LIMIT ?3",
        )?;
        let rows = st.query_map(params![owner, from, limit], chunk_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn chunk(&self, id: &str) -> Result<Option<Chunk>, Error> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, owner, ordinal, text, page, start_s FROM chunks WHERE id = ?1",
                params![id],
                chunk_row,
            )
            .optional()?)
    }

    /// Passages without a vector (the embedder was down, or they are new).
    pub fn unembedded(&self, owner: Option<&str>, limit: i64) -> Result<Vec<Chunk>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, owner, ordinal, text, page, start_s FROM chunks WHERE vec IS NULL \
             AND (?1 IS NULL OR owner = ?1) ORDER BY rowid LIMIT ?2",
        )?;
        let rows = st.query_map(params![owner, limit], chunk_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_vectors(&self, vectors: &[(String, Vec<f32>)]) -> Result<(), Error> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut st = tx.prepare("UPDATE chunks SET vec = ?2 WHERE id = ?1")?;
            for (id, v) in vectors {
                st.execute(params![id, vec_to_blob(v)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// FTS5 hits among `owners`: (chunk, bm25 rank; lower is better).
    pub fn fts(&self, query: &str, owners: &[String], limit: i64) -> Result<Vec<(Chunk, f64)>, Error> {
        if owners.is_empty() || query.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let list = serde_json::to_string(owners).unwrap_or_else(|_| "[]".into());
        let mut st = conn.prepare(
            "SELECT c.id, c.owner, c.ordinal, c.text, c.page, c.start_s, bm25(chunks_fts) AS rank \
             FROM chunks_fts JOIN chunks c ON c.rowid = chunks_fts.rowid \
             WHERE chunks_fts MATCH ?1 AND c.owner IN (SELECT value FROM json_each(?2)) \
             ORDER BY rank LIMIT ?3",
        )?;
        let rows = st.query_map(params![query, list, limit], |r| Ok((chunk_row(r)?, r.get::<_, f64>("rank")?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every vector among `owners`.
    pub fn vectors(&self, owners: &[String]) -> Result<Vec<(Chunk, Vec<f32>)>, Error> {
        if owners.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let list = serde_json::to_string(owners).unwrap_or_else(|_| "[]".into());
        let mut st = conn.prepare(
            "SELECT id, owner, ordinal, text, page, start_s, vec FROM chunks \
             WHERE vec IS NOT NULL AND owner IN (SELECT value FROM json_each(?1))",
        )?;
        let rows = st.query_map(params![list], |r| {
            let b: Vec<u8> = r.get("vec")?;
            Ok((chunk_row(r)?, blob_to_vec(&b)))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn count_chunks(&self) -> Result<(i64, i64), Error> {
        Ok(self.conn().query_row(
            "SELECT count(*), count(vec) FROM chunks",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// The passage owners a notebook searches: its sources in `full` context, the insights
    /// of its sources not `off`, and its notes (`note:<slug>`).
    pub fn scope(&self, notebook_id: &str) -> Result<Vec<String>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT source_id FROM notebook_sources WHERE notebook_id = ?1 AND context = 'full' \
             UNION ALL SELECT i.id FROM insights i JOIN notebook_sources ns ON ns.source_id = i.source_id \
               WHERE ns.notebook_id = ?1 AND ns.context != 'off' AND i.status = 'ready' \
             UNION ALL SELECT 'note:' || slug FROM notebook_notes WHERE notebook_id = ?1",
        )?;
        let ids = st.query_map(params![notebook_id], |r| r.get(0))?;
        Ok(ids.collect::<Result<_, _>>()?)
    }

    /// Every owner (the search across all notebooks).
    pub fn all_owners(&self) -> Result<Vec<String>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT DISTINCT owner FROM chunks")?;
        let ids = st.query_map([], |r| r.get(0))?;
        Ok(ids.collect::<Result<_, _>>()?)
    }

    /// The notebooks an owner belongs to (for global search hits).
    pub fn notebooks_of_owner(&self, owner: &str) -> Result<Vec<String>, Error> {
        let conn = self.conn();
        let sql = if let Some(slug) = owner.strip_prefix("note:") {
            let mut st = conn.prepare("SELECT notebook_id FROM notebook_notes WHERE slug = ?1")?;
            let ids = st.query_map(params![slug], |r| r.get(0))?;
            return Ok(ids.collect::<Result<_, _>>()?);
        } else if owner.starts_with('i') {
            "SELECT ns.notebook_id FROM insights i JOIN notebook_sources ns ON ns.source_id = i.source_id WHERE i.id = ?1"
        } else {
            "SELECT notebook_id FROM notebook_sources WHERE source_id = ?1"
        };
        let mut st = conn.prepare(sql)?;
        let ids = st.query_map(params![owner], |r| r.get(0))?;
        Ok(ids.collect::<Result<_, _>>()?)
    }

    /// A title for an owner: the source's, the insight's ("Dense Summary · <source>") or the note's.
    pub fn owner_title(&self, owner: &str) -> Result<String, Error> {
        let conn = self.conn();
        let t: Option<String> = if let Some(slug) = owner.strip_prefix("note:") {
            conn.query_row(
                "SELECT title FROM notebook_notes WHERE slug = ?1 AND title != '' LIMIT 1",
                params![slug],
                |r| r.get(0),
            )
            .optional()?
            .or_else(|| Some(slug.to_owned()))
        } else if owner.starts_with('i') {
            conn.query_row(
                "SELECT i.title || ' · ' || s.title FROM insights i JOIN sources s ON s.id = i.source_id WHERE i.id = ?1",
                params![owner],
                |r| r.get(0),
            )
            .optional()?
        } else {
            conn.query_row("SELECT title FROM sources WHERE id = ?1", params![owner], |r| r.get(0))
                .optional()?
        };
        Ok(t.unwrap_or_default())
    }

    // ------------------------------------------------------------ notes

    pub fn link_note(&self, notebook_id: &str, slug: &str, title: &str, excerpt: &str, updated: Option<&str>) -> Result<(), Error> {
        self.conn().execute(
            "INSERT INTO notebook_notes (notebook_id, slug, title, excerpt, note_updated_at, synced_at, added_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
             ON CONFLICT(notebook_id, slug) DO UPDATE SET title = excluded.title, excerpt = excluded.excerpt, \
             note_updated_at = excluded.note_updated_at, synced_at = excluded.synced_at",
            params![notebook_id, slug, title, excerpt, updated, now()],
        )?;
        self.touch_notebook(notebook_id)
    }

    pub fn unlink_note(&self, notebook_id: &str, slug: &str) -> Result<(), Error> {
        let conn = self.conn();
        let n = conn.execute(
            "DELETE FROM notebook_notes WHERE notebook_id = ?1 AND slug = ?2",
            params![notebook_id, slug],
        )?;
        if n == 0 {
            return Err(Error::NotFound("note"));
        }
        let left: i64 = conn.query_row("SELECT count(*) FROM notebook_notes WHERE slug = ?1", params![slug], |r| r.get(0))?;
        if left == 0 {
            conn.execute("DELETE FROM chunks WHERE owner = ?1", params![format!("note:{slug}")])?;
        }
        Ok(())
    }

    pub fn notes(&self, notebook_id: &str) -> Result<Vec<NoteLink>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT slug, title, excerpt, note_updated_at, \
             EXISTS (SELECT 1 FROM chunks c WHERE c.owner = 'note:' || slug) AS indexed \
             FROM notebook_notes WHERE notebook_id = ?1 ORDER BY added_at DESC",
        )?;
        let rows = st.query_map(params![notebook_id], |r| {
            Ok(NoteLink {
                slug: r.get(0)?,
                title: r.get(1)?,
                excerpt: r.get(2)?,
                updated_at: r.get(3)?,
                indexed: r.get::<_, i64>(4)? != 0,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Notes of a notebook whose copy was synced before `before` (or never).
    pub fn stale_notes(&self, notebook_id: &str, before: &str) -> Result<Vec<String>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT slug FROM notebook_notes WHERE notebook_id = ?1 AND (synced_at IS NULL OR synced_at < ?2 \
             OR NOT EXISTS (SELECT 1 FROM chunks c WHERE c.owner = 'note:' || slug))",
        )?;
        let ids = st.query_map(params![notebook_id, before], |r| r.get(0))?;
        Ok(ids.collect::<Result<_, _>>()?)
    }

    pub fn note_synced(&self, slug: &str, title: &str, excerpt: &str, updated: Option<&str>) -> Result<(), Error> {
        self.conn().execute(
            "UPDATE notebook_notes SET title = ?2, excerpt = ?3, note_updated_at = ?4, synced_at = ?5 WHERE slug = ?1",
            params![slug, title, excerpt, updated, now()],
        )?;
        Ok(())
    }

    /// The note's stored update time, to skip re-indexing an unchanged note.
    pub fn note_updated(&self, slug: &str) -> Result<Option<String>, Error> {
        Ok(self
            .conn()
            .query_row(
                "SELECT note_updated_at FROM notebook_notes WHERE slug = ?1 LIMIT 1",
                params![slug],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    // ------------------------------------------------------------ chats

    pub fn create_chat(&self, id: &str, notebook_id: &str, title: &str) -> Result<Chat, Error> {
        let t = now();
        self.conn().execute(
            "INSERT INTO chats (id, notebook_id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
            params![id, notebook_id, title, t],
        )?;
        self.touch_notebook(notebook_id)?;
        self.chat(id)
    }

    pub fn chat(&self, id: &str) -> Result<Chat, Error> {
        self.chats_where("c.id = ?1", id)?
            .into_iter()
            .next()
            .ok_or(Error::NotFound("chat"))
    }

    pub fn chats(&self, notebook_id: &str) -> Result<Vec<Chat>, Error> {
        self.chats_where("c.notebook_id = ?1", notebook_id)
    }

    fn chats_where(&self, cond: &str, arg: &str) -> Result<Vec<Chat>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT c.*, (SELECT count(*) FROM messages m WHERE m.chat_id = c.id) AS nm FROM chats c \
             WHERE {cond} ORDER BY c.updated_at DESC"
        ))?;
        let rows = st.query_map(params![arg], |r| {
            Ok(Chat {
                id: r.get("id")?,
                notebook_id: r.get("notebook_id")?,
                title: r.get("title")?,
                created_at: r.get("created_at")?,
                updated_at: r.get("updated_at")?,
                messages: r.get("nm")?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn rename_chat(&self, id: &str, title: &str) -> Result<Chat, Error> {
        let n = self.conn().execute(
            "UPDATE chats SET title = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, title, now()],
        )?;
        if n == 0 {
            return Err(Error::NotFound("chat"));
        }
        self.chat(id)
    }

    pub fn delete_chat(&self, id: &str) -> Result<(), Error> {
        let n = self.conn().execute("DELETE FROM chats WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(Error::NotFound("chat"));
        }
        Ok(())
    }

    pub fn add_message(&self, chat_id: &str, m: &Message) -> Result<(), Error> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO messages (id, chat_id, role, text, citations, model, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                m.id,
                chat_id,
                m.role,
                m.text,
                serde_json::to_string(&m.citations).unwrap_or_else(|_| "[]".into()),
                m.model,
                m.created_at
            ],
        )?;
        conn.execute("UPDATE chats SET updated_at = ?2 WHERE id = ?1", params![chat_id, m.created_at])?;
        Ok(())
    }

    pub fn messages(&self, chat_id: &str) -> Result<Vec<Message>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, role, text, citations, model, created_at FROM messages WHERE chat_id = ?1 ORDER BY created_at, rowid",
        )?;
        let rows = st.query_map(params![chat_id], |r| {
            let c: String = r.get(3)?;
            Ok(Message {
                id: r.get(0)?,
                role: r.get(1)?,
                text: r.get(2)?,
                citations: serde_json::from_str(&c).unwrap_or_default(),
                model: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn message(&self, chat_id: &str, id: &str) -> Result<Message, Error> {
        self.messages(chat_id)?
            .into_iter()
            .find(|m| m.id == id)
            .ok_or(Error::NotFound("message"))
    }

    // ------------------------------------------------------------ transformations

    pub fn upsert_builtin(&self, t: &Transformation) -> Result<(), Error> {
        let n = now();
        self.conn().execute(
            "INSERT INTO transformations (id, name, description, prompt, apply_default, builtin, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6) \
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, description = excluded.description, \
             prompt = excluded.prompt, builtin = 1",
            params![t.id, t.name, t.description, t.prompt, t.apply_default as i64, n],
        )?;
        Ok(())
    }

    pub fn create_transformation(&self, t: &Transformation) -> Result<Transformation, Error> {
        let n = now();
        let r = self.conn().execute(
            "INSERT INTO transformations (id, name, description, prompt, apply_default, builtin, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?6)",
            params![t.id, t.name, t.description, t.prompt, t.apply_default as i64, n],
        );
        match r {
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
                return Err(Error::Conflict("a transformation with that name exists".into()))
            }
            r => {
                r?;
            }
        }
        self.transformation(&t.id)
    }

    pub fn transformation(&self, id: &str) -> Result<Transformation, Error> {
        self.conn()
            .query_row("SELECT * FROM transformations WHERE id = ?1", params![id], transformation_row)
            .optional()?
            .ok_or(Error::NotFound("transformation"))
    }

    pub fn transformations(&self) -> Result<Vec<Transformation>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT * FROM transformations ORDER BY builtin DESC, name")?;
        let rows = st.query_map([], transformation_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn update_transformation(
        &self,
        id: &str,
        name: Option<&str>,
        description: Option<&str>,
        prompt: Option<&str>,
        apply_default: Option<bool>,
    ) -> Result<Transformation, Error> {
        let n = self.conn().execute(
            "UPDATE transformations SET name = coalesce(?2, name), description = coalesce(?3, description), \
             prompt = coalesce(?4, prompt), apply_default = coalesce(?5, apply_default), updated_at = ?6 WHERE id = ?1",
            params![id, name, description, prompt, apply_default.map(|b| b as i64), now()],
        )?;
        if n == 0 {
            return Err(Error::NotFound("transformation"));
        }
        self.transformation(id)
    }

    pub fn delete_transformation(&self, id: &str) -> Result<(), Error> {
        if self.transformation(id)?.builtin {
            return Err(Error::Conflict("built-in transformations can't be deleted".into()));
        }
        self.conn().execute("DELETE FROM transformations WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ------------------------------------------------------------ insights

    pub fn create_insight(&self, id: &str, source_id: &str, t: &Transformation) -> Result<Insight, Error> {
        let n = now();
        self.conn().execute(
            "INSERT INTO insights (id, source_id, transformation_id, title, status, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?5)",
            params![id, source_id, t.id, t.name, n],
        )?;
        self.insight(id)
    }

    pub fn insight(&self, id: &str) -> Result<Insight, Error> {
        self.conn()
            .query_row("SELECT * FROM insights WHERE id = ?1", params![id], insight_row)
            .optional()?
            .ok_or(Error::NotFound("insight"))
    }

    pub fn insights(&self, source_id: &str) -> Result<Vec<Insight>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT * FROM insights WHERE source_id = ?1 ORDER BY created_at")?;
        let rows = st.query_map(params![source_id], insight_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_insight(&self, id: &str, status: &str, text: Option<&str>, error: Option<&str>) -> Result<(), Error> {
        self.conn().execute(
            "UPDATE insights SET status = ?2, text = coalesce(?3, text), error = ?4, updated_at = ?5 WHERE id = ?1",
            params![id, status, text, error, now()],
        )?;
        Ok(())
    }

    pub fn bump_insight_attempts(&self, id: &str) -> Result<i64, Error> {
        let conn = self.conn();
        conn.execute("UPDATE insights SET attempts = attempts + 1 WHERE id = ?1", params![id])?;
        Ok(conn.query_row("SELECT attempts FROM insights WHERE id = ?1", params![id], |r| r.get(0))?)
    }

    pub fn delete_insight(&self, id: &str) -> Result<(), Error> {
        let conn = self.conn();
        let n = conn.execute("DELETE FROM insights WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(Error::NotFound("insight"));
        }
        conn.execute("DELETE FROM chunks WHERE owner = ?1", params![id])?;
        Ok(())
    }

    pub fn pending_insights(&self) -> Result<Vec<String>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT id FROM insights WHERE status NOT IN ('ready', 'failed') ORDER BY created_at")?;
        let ids = st.query_map([], |r| r.get(0))?;
        Ok(ids.collect::<Result<_, _>>()?)
    }

    // ------------------------------------------------------------ podcasts

    #[allow(clippy::too_many_arguments)]
    pub fn create_podcast(
        &self,
        id: &str,
        notebook_id: &str,
        title: &str,
        episode_profile: &str,
        speaker_profile: &str,
        briefing: &str,
        minutes: f64,
    ) -> Result<Podcast, Error> {
        let n = now();
        self.conn().execute(
            "INSERT INTO podcasts (id, notebook_id, title, episode_profile, speaker_profile, briefing, minutes, status, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'queued', ?8, ?8)",
            params![id, notebook_id, title, episode_profile, speaker_profile, briefing, minutes, n],
        )?;
        self.touch_notebook(notebook_id)?;
        self.podcast(id)
    }

    pub fn podcast(&self, id: &str) -> Result<Podcast, Error> {
        self.conn()
            .query_row("SELECT * FROM podcasts WHERE id = ?1", params![id], podcast_row)
            .optional()?
            .ok_or(Error::NotFound("podcast"))
    }

    pub fn podcasts(&self, notebook_id: &str) -> Result<Vec<Podcast>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT * FROM podcasts WHERE notebook_id = ?1 ORDER BY created_at DESC")?;
        let rows = st.query_map(params![notebook_id], podcast_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn set_podcast_status(&self, id: &str, status: &str, error: Option<&str>) -> Result<(), Error> {
        self.conn().execute(
            "UPDATE podcasts SET status = ?2, error = ?3, updated_at = ?4 WHERE id = ?1",
            params![id, status, error, now()],
        )?;
        Ok(())
    }

    pub fn set_podcast_script(&self, id: &str, title: Option<&str>, outline: &Value, transcript: &Value) -> Result<(), Error> {
        self.conn().execute(
            "UPDATE podcasts SET title = coalesce(?2, title), outline = ?3, transcript = ?4, updated_at = ?5 WHERE id = ?1",
            params![id, title, outline.to_string(), transcript.to_string(), now()],
        )?;
        Ok(())
    }

    pub fn set_podcast_duration(&self, id: &str, duration_s: f64) -> Result<(), Error> {
        self.conn()
            .execute("UPDATE podcasts SET duration_s = ?2 WHERE id = ?1", params![id, duration_s])?;
        Ok(())
    }

    pub fn bump_podcast_attempts(&self, id: &str) -> Result<i64, Error> {
        let conn = self.conn();
        conn.execute("UPDATE podcasts SET attempts = attempts + 1 WHERE id = ?1", params![id])?;
        Ok(conn.query_row("SELECT attempts FROM podcasts WHERE id = ?1", params![id], |r| r.get(0))?)
    }

    pub fn delete_podcast(&self, id: &str) -> Result<(), Error> {
        let n = self.conn().execute("DELETE FROM podcasts WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(Error::NotFound("podcast"));
        }
        Ok(())
    }

    pub fn pending_podcasts(&self) -> Result<Vec<String>, Error> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT id FROM podcasts WHERE status NOT IN ('ready', 'failed') ORDER BY created_at")?;
        let ids = st.query_map([], |r| r.get(0))?;
        Ok(ids.collect::<Result<_, _>>()?)
    }

    pub fn count_sources(&self) -> Result<i64, Error> {
        Ok(self.conn().query_row("SELECT count(*) FROM sources", [], |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notebook_sources_notes_and_scope() {
        let s = Store::in_memory().unwrap();
        let nb = s.create_notebook("nb1", "CNS", "").unwrap();
        assert_eq!(nb.counts, Counts::default());
        s.create_source("s1", "text", "One", "text", "ready", "alpha beta", None, &[]).unwrap();
        s.create_source("s2", "text", "Two", "text", "ready", "gamma", None, &[]).unwrap();
        s.link_source("nb1", "s1").unwrap();
        s.link_source("nb1", "s2").unwrap();
        s.link_note("nb1", "lecture-1", "Lecture 1", "x", None).unwrap();
        s.replace_chunks("s1", &[NewChunk { text: "DHCP snooping blocks rogue servers".into(), page: Some(3), start_s: None }]).unwrap();
        s.replace_chunks("s2", &[NewChunk { text: "VLAN hopping with double tagging".into(), ..Default::default() }]).unwrap();
        s.set_context("nb1", "s2", "off").unwrap();
        let scope = s.scope("nb1").unwrap();
        assert!(scope.contains(&"s1".to_string()));
        assert!(!scope.contains(&"s2".to_string()));
        assert!(scope.contains(&"note:lecture-1".to_string()));
        let hits = s.fts("\"rogue\"", &scope, 5).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.id, "s1-0");
        assert_eq!(hits[0].0.page, Some(3));
        let nb = s.notebook("nb1").unwrap();
        assert_eq!(nb.counts.sources, 2);
        assert_eq!(nb.counts.notes, 1);
    }

    #[test]
    fn unlinking_the_last_notebook_deletes_the_source_and_its_passages() {
        let s = Store::in_memory().unwrap();
        s.create_notebook("a", "A", "").unwrap();
        s.create_notebook("b", "B", "").unwrap();
        s.create_source("s1", "text", "One", "text", "ready", "x", None, &[]).unwrap();
        s.link_source("a", "s1").unwrap();
        s.link_source("b", "s1").unwrap();
        s.replace_chunks("s1", &[NewChunk { text: "x y".into(), ..Default::default() }]).unwrap();
        assert_eq!(s.exclusive_sources("a").unwrap(), Vec::<String>::new());
        assert!(!s.unlink_source("a", "s1").unwrap());
        assert!(s.unlink_source("b", "s1").unwrap());
        assert!(matches!(s.source("s1", false), Err(Error::NotFound(_))));
        assert_eq!(s.count_chunks().unwrap(), (0, 0));
        // The FTS index forgot it too.
        assert!(s.fts("\"x\"", &["s1".into()], 5).unwrap().is_empty());
    }

    #[test]
    fn vectors_round_trip() {
        let s = Store::in_memory().unwrap();
        s.create_source("s1", "text", "One", "text", "ready", "x", None, &[]).unwrap();
        s.replace_chunks("s1", &[NewChunk { text: "x".into(), ..Default::default() }]).unwrap();
        assert_eq!(s.unembedded(None, 10).unwrap().len(), 1);
        s.set_vectors(&[("s1-0".into(), vec![0.6, 0.8])]).unwrap();
        assert!(s.unembedded(None, 10).unwrap().is_empty());
        let v = s.vectors(&["s1".into()]).unwrap();
        assert_eq!(v[0].1, vec![0.6, 0.8]);
    }
}
