//! Durable state: `agents.db` (SQLite) holds each agent record as JSON with its
//! idempotency key; `timelines/<agentId>.jsonl` holds the timeline rows, one per
//! line, append-only. Paseo keeps timelines in memory only; these survive a
//! restart with the same epoch.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lucina_agents::{AgentRecord, TimelineRow};
use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct Store {
    db: Mutex<Connection>,
    timelines: PathBuf,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        fs::create_dir_all(dir)?;
        let timelines = dir.join("timelines");
        fs::create_dir_all(&timelines)?;
        let db = Connection::open(dir.join("agents.db"))?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS agents (
                 id TEXT PRIMARY KEY,
                 idempotency_key TEXT UNIQUE,
                 created_at TEXT NOT NULL,
                 record TEXT NOT NULL
             );",
        )?;
        Ok(Store { db: Mutex::new(db), timelines })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn save(&self, record: &AgentRecord) -> Result<(), StoreError> {
        let json = serde_json::to_string(record)?;
        self.db().execute(
            "INSERT INTO agents (id, idempotency_key, created_at, record) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET record = excluded.record",
            params![record.id, record.idempotency_key, record.created_at, json],
        )?;
        Ok(())
    }

    pub fn load_all(&self) -> Result<Vec<AgentRecord>, StoreError> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT record FROM agents ORDER BY created_at")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            out.push(serde_json::from_str(&json?)?);
        }
        Ok(out)
    }

    pub fn find_by_idempotency_key(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .db()
            .query_row("SELECT id FROM agents WHERE idempotency_key = ?1", params![key], |r| r.get(0))
            .optional()?)
    }

    pub fn delete(&self, id: &str) -> Result<(), StoreError> {
        self.db().execute("DELETE FROM agents WHERE id = ?1", params![id])?;
        if valid_id(id) {
            match fs::remove_file(self.timelines.join(format!("{id}.jsonl"))) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            }
        }
        Ok(())
    }

    fn timeline_path(&self, id: &str) -> io::Result<PathBuf> {
        if !valid_id(id) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad agent id"));
        }
        Ok(self.timelines.join(format!("{id}.jsonl")))
    }

    pub fn append_row(&self, id: &str, row: &TimelineRow) -> Result<(), StoreError> {
        let mut line = serde_json::to_string(row)?;
        line.push('\n');
        let mut f = OpenOptions::new().create(true).append(true).open(self.timeline_path(id)?)?;
        f.write_all(line.as_bytes())?;
        Ok(())
    }

    /// The stored rows; a torn last line (crash mid-write) is dropped.
    pub fn load_rows(&self, id: &str) -> Result<Vec<TimelineRow>, StoreError> {
        let path = self.timeline_path(id)?;
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut rows = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<TimelineRow>(&line) {
                Ok(row) => rows.push(row),
                Err(e) => tracing::warn!(agent = id, error = %e, "skipping unreadable timeline row"),
            }
        }
        Ok(rows)
    }
}
