//! Durable state: `agents.db` (SQLite) holds each agent record as JSON with its
//! idempotency key; `timelines/<agentId>.jsonl` holds the timeline rows, one per
//! line, append-only. Paseo keeps timelines in memory only; these survive a
//! restart with the same epoch. Projects and workspaces are JSON records in the same
//! database (Paseo: `projects/projects.json`, `projects/workspaces.json`), and so are
//! the commands queued for external agents' owner devices. A provider subagent's
//! descriptor and timeline rows are in `provider_subagents` and
//! `provider_subagent_rows` (an agent's rows are separate from its own).

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use lucina_agents::subagent::Subagent;
use lucina_agents::workspace::{ProjectRecord, WorkspaceRecord};
use lucina_agents::external::AgentCommand;
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
             );
             CREATE TABLE IF NOT EXISTS projects (id TEXT PRIMARY KEY, record TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS workspaces (id TEXT PRIMARY KEY, record TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS commands (
                 id TEXT PRIMARY KEY,
                 agent_id TEXT NOT NULL,
                 device_id TEXT NOT NULL,
                 expires_at_ms INTEGER NOT NULL,
                 state TEXT NOT NULL DEFAULT 'pending',
                 record TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS commands_device ON commands (device_id, state);
             CREATE TABLE IF NOT EXISTS provider_subagents (
                 agent_id TEXT NOT NULL,
                 id TEXT NOT NULL,
                 record TEXT NOT NULL,
                 PRIMARY KEY (agent_id, id)
             );
             CREATE TABLE IF NOT EXISTS provider_subagent_rows (
                 agent_id TEXT NOT NULL,
                 subagent_id TEXT NOT NULL,
                 seq INTEGER NOT NULL,
                 record TEXT NOT NULL,
                 PRIMARY KEY (agent_id, subagent_id, seq)
             );",
        )?;
        Ok(Store { db: Mutex::new(db), timelines })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Run `f` on the database, for the tables other modules own (schedules, finish
    /// subscriptions, the search index).
    pub fn with_db<T>(&self, f: impl FnOnce(&Connection) -> T) -> T {
        f(&self.db())
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
        self.db().execute("DELETE FROM commands WHERE agent_id = ?1", params![id])?;
        self.delete_subagents(id)?;
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

impl Store {
    fn put(&self, table: &str, id: &str, json: String) -> Result<(), StoreError> {
        self.db().execute(
            &format!("INSERT INTO {table} (id, record) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET record = excluded.record"),
            params![id, json],
        )?;
        Ok(())
    }

    fn all<T: serde::de::DeserializeOwned>(&self, table: &str) -> Result<Vec<T>, StoreError> {
        let db = self.db();
        let mut stmt = db.prepare(&format!("SELECT record FROM {table} ORDER BY id"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            out.push(serde_json::from_str(&json?)?);
        }
        Ok(out)
    }

    pub fn save_project(&self, p: &ProjectRecord) -> Result<(), StoreError> {
        self.put("projects", &p.project_id, serde_json::to_string(p)?)
    }

    pub fn load_projects(&self) -> Result<Vec<ProjectRecord>, StoreError> {
        self.all("projects")
    }

    pub fn delete_project(&self, id: &str) -> Result<(), StoreError> {
        self.db().execute("DELETE FROM projects WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn save_workspace(&self, w: &WorkspaceRecord) -> Result<(), StoreError> {
        self.put("workspaces", &w.workspace_id, serde_json::to_string(w)?)
    }

    pub fn load_workspaces(&self) -> Result<Vec<WorkspaceRecord>, StoreError> {
        self.all("workspaces")
    }
}

/// A queued command's state (`commands.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandState {
    Pending,
    Acked,
    Expired,
}

impl CommandState {
    fn as_str(self) -> &'static str {
        match self {
            CommandState::Pending => "pending",
            CommandState::Acked => "acked",
            CommandState::Expired => "expired",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "acked" => CommandState::Acked,
            "expired" => CommandState::Expired,
            _ => CommandState::Pending,
        }
    }
}

/// One stored command with its owner device and state.
#[derive(Debug, Clone)]
pub struct StoredCommand {
    pub command: AgentCommand,
    pub device_id: String,
    pub expires_at_ms: u64,
    pub state: CommandState,
}

impl Store {
    pub fn insert_command(&self, command: &AgentCommand, device_id: &str, expires_at_ms: u64) -> Result<(), StoreError> {
        self.db().execute(
            "INSERT INTO commands (id, agent_id, device_id, expires_at_ms, record) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![command.command_id, command.agent_id, device_id, expires_at_ms as i64, serde_json::to_string(command)?],
        )?;
        Ok(())
    }

    pub fn command(&self, id: &str) -> Result<Option<StoredCommand>, StoreError> {
        let row = self
            .db()
            .query_row(
                "SELECT record, device_id, expires_at_ms, state FROM commands WHERE id = ?1",
                params![id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?)),
            )
            .optional()?;
        row.map(|(json, device_id, exp, state)| {
            Ok(StoredCommand { command: serde_json::from_str(&json)?, device_id, expires_at_ms: exp.max(0) as u64, state: CommandState::parse(&state) })
        })
        .transpose()
    }

    /// Pending commands of one device that have not expired by `now_ms`, oldest first.
    pub fn pending_commands(&self, device_id: &str, now_ms: u64) -> Result<Vec<AgentCommand>, StoreError> {
        let db = self.db();
        let mut stmt = db.prepare(
            "SELECT record FROM commands WHERE device_id = ?1 AND state = 'pending' AND expires_at_ms > ?2 ORDER BY rowid",
        )?;
        let rows = stmt.query_map(params![device_id, now_ms as i64], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for json in rows {
            out.push(serde_json::from_str(&json?)?);
        }
        Ok(out)
    }

    pub fn set_command_state(&self, id: &str, state: CommandState) -> Result<(), StoreError> {
        self.db().execute("UPDATE commands SET state = ?2 WHERE id = ?1", params![id, state.as_str()])?;
        Ok(())
    }

    /// Mark pending commands past their expiry as expired; returns them.
    pub fn expire_commands(&self, now_ms: u64) -> Result<Vec<AgentCommand>, StoreError> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT record FROM commands WHERE state = 'pending' AND expires_at_ms <= ?1 ORDER BY rowid")?;
        let rows = stmt.query_map(params![now_ms as i64], |r| r.get::<_, String>(0))?;
        let mut out: Vec<AgentCommand> = Vec::new();
        for json in rows {
            out.push(serde_json::from_str(&json?)?);
        }
        drop(stmt);
        db.execute("UPDATE commands SET state = 'expired' WHERE state = 'pending' AND expires_at_ms <= ?1", params![now_ms as i64])?;
        // Finished commands are kept a week for inspection, then dropped.
        let week = 7 * 24 * 3_600_000_i64;
        db.execute("DELETE FROM commands WHERE state != 'pending' AND expires_at_ms <= ?1", params![now_ms as i64 - week])?;
        Ok(out)
    }
}

impl Store {
    pub fn save_subagent(&self, s: &Subagent) -> Result<(), StoreError> {
        self.db().execute(
            "INSERT INTO provider_subagents (agent_id, id, record) VALUES (?1, ?2, ?3)
             ON CONFLICT(agent_id, id) DO UPDATE SET record = excluded.record",
            params![s.parent_agent_id, s.id, serde_json::to_string(s)?],
        )?;
        Ok(())
    }

    pub fn append_subagent_row(&self, agent_id: &str, subagent_id: &str, row: &TimelineRow) -> Result<(), StoreError> {
        self.db().execute(
            "INSERT OR REPLACE INTO provider_subagent_rows (agent_id, subagent_id, seq, record) VALUES (?1, ?2, ?3, ?4)",
            params![agent_id, subagent_id, row.seq as i64, serde_json::to_string(row)?],
        )?;
        Ok(())
    }

    /// An agent's provider subagents with their rows, oldest first. An unreadable record is skipped.
    pub fn load_subagents(&self, agent_id: &str) -> Result<Vec<(Subagent, Vec<TimelineRow>)>, StoreError> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT record FROM provider_subagents WHERE agent_id = ?1")?;
        let records = stmt.query_map(params![agent_id], |r| r.get::<_, String>(0))?;
        let mut subs: Vec<Subagent> = records.flatten().filter_map(|j| serde_json::from_str(&j).ok()).collect();
        subs.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        let mut rows_stmt = db.prepare("SELECT record FROM provider_subagent_rows WHERE agent_id = ?1 AND subagent_id = ?2 ORDER BY seq")?;
        let mut out = Vec::new();
        for sub in subs {
            let rows = rows_stmt.query_map(params![agent_id, sub.id], |r| r.get::<_, String>(0))?;
            let rows: Vec<TimelineRow> = rows.flatten().filter_map(|j| serde_json::from_str(&j).ok()).collect();
            out.push((sub, rows));
        }
        Ok(out)
    }

    pub fn delete_subagent(&self, agent_id: &str, subagent_id: &str) -> Result<(), StoreError> {
        self.db().execute("DELETE FROM provider_subagents WHERE agent_id = ?1 AND id = ?2", params![agent_id, subagent_id])?;
        self.db().execute("DELETE FROM provider_subagent_rows WHERE agent_id = ?1 AND subagent_id = ?2", params![agent_id, subagent_id])?;
        Ok(())
    }

    pub fn delete_subagents(&self, agent_id: &str) -> Result<(), StoreError> {
        self.db().execute("DELETE FROM provider_subagents WHERE agent_id = ?1", params![agent_id])?;
        self.db().execute("DELETE FROM provider_subagent_rows WHERE agent_id = ?1", params![agent_id])?;
        Ok(())
    }
}
