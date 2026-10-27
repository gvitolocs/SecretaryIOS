//! Reports on disk plus a SQLite index.
//!
//! ```text
//! <root>/index.sqlite                         the index (rebuildable: `lucina-diagnostics reindex`)
//! <root>/.staging/<id>/                       a report being written (cleared at start)
//! <root>/<app>/<YYYY-MM-DD>/<id>/report.json  the report (date = received, UTC)
//! <root>/<app>/<YYYY-MM-DD>/<id>/<file>       its attachments
//! ```
//!
//! A report is written completely in `.staging/<id>/`, then moved into place
//! with one rename, then indexed; a crash in between leaves at most an
//! unindexed directory, which the retention sweep removes once it is old.
//! The dedupe decision (same fingerprint already stored → count it) and the
//! insert happen under the index lock, so two copies of one crash arriving
//! together still give one report.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Context;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};

use crate::model::{
    first_line, valid_id, AppName, Attachment, Kind, Occurrence, Report, Summary,
    RECENT_OCCURRENCES,
};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS reports(
    id TEXT PRIMARY KEY,
    app TEXT NOT NULL,
    app_version TEXT NOT NULL,
    os TEXT NOT NULL,
    device TEXT NOT NULL,
    kind TEXT NOT NULL,
    message TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    received_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    count INTEGER NOT NULL,
    attachments INTEGER NOT NULL,
    dir TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS reports_fingerprint ON reports(fingerprint);
CREATE INDEX IF NOT EXISTS reports_last_seen ON reports(last_seen_at);
CREATE TABLE IF NOT EXISTS client_ids(
    client_id TEXT PRIMARY KEY,
    report_id TEXT NOT NULL
);
";

/// What happened to an incoming report.
#[derive(Debug, Clone, PartialEq)]
pub enum Ingest {
    /// A new report (with its attachments).
    Created(Box<Report>),
    /// The same signature was already stored: its counter went up, the new
    /// copy and its attachments were dropped.
    Duplicate { id: String, count: u64 },
    /// The client sent this `client_id` before: nothing changed.
    Replay { id: String, count: u64 },
}

/// Filters of the list.
#[derive(Debug, Clone, Default)]
pub struct ListQuery {
    pub apps: Vec<AppName>,
    pub kinds: Vec<Kind>,
    /// Last seen at or after.
    pub since: Option<DateTime<Utc>>,
    /// Last seen before.
    pub until: Option<DateTime<Utc>>,
    pub device: Option<String>,
    pub fingerprint: Option<String>,
    pub limit: usize,
}

pub struct Store {
    root: PathBuf,
    db: Mutex<Connection>,
}

fn ms(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

fn from_ms(v: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(v).single().unwrap_or_default()
}

/// Writes `bytes` to `path` through a temporary file and a rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

fn read_report(dir: &Path) -> anyhow::Result<Report> {
    let p = dir.join("report.json");
    let bytes = std::fs::read(&p).with_context(|| format!("read {}", p.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parse {}", p.display()))
}

fn write_report(dir: &Path, r: &Report) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(r)?;
    write_atomic(&dir.join("report.json"), &bytes)?;
    Ok(())
}

fn summary_of(row: &rusqlite::Row<'_>) -> rusqlite::Result<Summary> {
    let app: String = row.get(1)?;
    let kind: String = row.get(5)?;
    Ok(Summary {
        id: row.get(0)?,
        app: AppName::parse(&app).unwrap_or(AppName::Server),
        app_version: row.get(2)?,
        os: row.get(3)?,
        device: row.get(4)?,
        kind: Kind::parse(&kind).unwrap_or(Kind::Error),
        message: row.get(6)?,
        fingerprint: row.get(7)?,
        occurred_at: from_ms(row.get(8)?),
        received_at: from_ms(row.get(9)?),
        last_seen_at: from_ms(row.get(10)?),
        count: row.get::<_, i64>(11)? as u64,
        attachments: row.get::<_, i64>(12)? as u32,
    })
}

const SUMMARY_COLS: &str = "id, app, app_version, os, device, kind, message, fingerprint, occurred_at, received_at, last_seen_at, count, attachments";

impl Store {
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(root).with_context(|| format!("create {}", root.display()))?;
        let staging = root.join(".staging");
        if staging.exists() {
            std::fs::remove_dir_all(&staging).ok();
        }
        std::fs::create_dir_all(&staging)?;
        let db = Connection::open(root.join("index.sqlite")).context("open index.sqlite")?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch(SCHEMA)?;
        Ok(Self {
            root: root.to_owned(),
            db: Mutex::new(db),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// An empty staging directory for a report being received.
    pub fn stage(&self, id: &str) -> std::io::Result<PathBuf> {
        let d = self.root.join(".staging").join(id);
        std::fs::create_dir_all(&d)?;
        Ok(d)
    }

    fn rel_dir(r: &Report) -> String {
        format!(
            "{}/{}/{}",
            r.app.as_str(),
            r.received_at.format("%Y-%m-%d"),
            r.id
        )
    }

    /// Stores `report` (its attachments already in `staged`), or counts it
    /// as a repeat. `staged` is moved or removed in every case.
    pub fn commit(&self, report: Report, staged: &Path) -> anyhow::Result<Ingest> {
        let out = self.commit_locked(report, staged);
        if staged.exists() {
            std::fs::remove_dir_all(staged).ok();
        }
        out
    }

    fn commit_locked(&self, report: Report, staged: &Path) -> anyhow::Result<Ingest> {
        let db = self.db();
        if let Some(cid) = &report.client_id {
            let known: Option<(String, i64)> = db
                .query_row(
                    "SELECT c.report_id, r.count FROM client_ids c JOIN reports r ON r.id = c.report_id WHERE c.client_id = ?1",
                    [cid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((id, count)) = known {
                return Ok(Ingest::Replay {
                    id,
                    count: count as u64,
                });
            }
        }
        if report.kind.dedupes() {
            let existing: Option<(String, String)> = db
                .query_row(
                    "SELECT id, dir FROM reports WHERE fingerprint = ?1 ORDER BY received_at LIMIT 1",
                    [&report.fingerprint],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((id, dir)) = existing {
                let dir = self.root.join(dir);
                let mut known = read_report(&dir)?;
                known.count += 1;
                known.last_seen_at = known.last_seen_at.max(report.received_at);
                known.last_occurred_at = known.last_occurred_at.max(report.occurred_at);
                known.recent.push(Occurrence {
                    occurred_at: report.occurred_at,
                    received_at: report.received_at,
                    app_version: report.app_version.clone(),
                    os: report.os.clone(),
                    device: report.device.clone(),
                });
                let excess = known.recent.len().saturating_sub(RECENT_OCCURRENCES);
                known.recent.drain(..excess);
                write_report(&dir, &known)?;
                db.execute(
                    "UPDATE reports SET count = ?2, last_seen_at = ?3 WHERE id = ?1",
                    params![id, known.count as i64, ms(known.last_seen_at)],
                )?;
                if let Some(cid) = &report.client_id {
                    db.execute(
                        "INSERT OR IGNORE INTO client_ids(client_id, report_id) VALUES (?1, ?2)",
                        params![cid, id],
                    )?;
                }
                return Ok(Ingest::Duplicate {
                    id,
                    count: known.count,
                });
            }
        }
        write_report(staged, &report)?;
        let rel = Self::rel_dir(&report);
        let dest = self.root.join(&rel);
        std::fs::create_dir_all(dest.parent().expect("dated dir"))?;
        std::fs::rename(staged, &dest).with_context(|| format!("move into {}", dest.display()))?;
        db.execute(
            &format!("INSERT INTO reports({SUMMARY_COLS}, dir) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"),
            params![
                report.id,
                report.app.as_str(),
                report.app_version,
                report.os,
                report.device,
                report.kind.as_str(),
                first_line(&report.message, 300),
                report.fingerprint,
                ms(report.occurred_at),
                ms(report.received_at),
                ms(report.last_seen_at),
                report.count as i64,
                report.attachments.len() as i64,
                rel,
            ],
        )?;
        if let Some(cid) = &report.client_id {
            db.execute(
                "INSERT OR IGNORE INTO client_ids(client_id, report_id) VALUES (?1, ?2)",
                params![cid, report.id],
            )?;
        }
        Ok(Ingest::Created(Box::new(report)))
    }

    /// Newest activity first.
    pub fn list(&self, q: &ListQuery) -> anyhow::Result<Vec<Summary>> {
        let mut sql = format!("SELECT {SUMMARY_COLS} FROM reports WHERE 1=1");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        let mut any_of = |sql: &mut String, col: &str, vals: Vec<String>| {
            if vals.is_empty() {
                return;
            }
            sql.push_str(&format!(
                " AND {col} IN ({})",
                vec!["?"; vals.len()].join(",")
            ));
            args.extend(vals.into_iter().map(rusqlite::types::Value::Text));
        };
        any_of(
            &mut sql,
            "app",
            q.apps.iter().map(|a| a.as_str().to_owned()).collect(),
        );
        any_of(
            &mut sql,
            "kind",
            q.kinds.iter().map(|k| k.as_str().to_owned()).collect(),
        );
        any_of(&mut sql, "device", q.device.iter().cloned().collect());
        any_of(
            &mut sql,
            "fingerprint",
            q.fingerprint.iter().cloned().collect(),
        );
        if let Some(s) = q.since {
            sql.push_str(" AND last_seen_at >= ?");
            args.push(rusqlite::types::Value::Integer(ms(s)));
        }
        if let Some(u) = q.until {
            sql.push_str(" AND last_seen_at < ?");
            args.push(rusqlite::types::Value::Integer(ms(u)));
        }
        sql.push_str(" ORDER BY last_seen_at DESC, id DESC LIMIT ?");
        args.push(rusqlite::types::Value::Integer(q.limit.clamp(1, 500) as i64));
        let db = self.db();
        let mut st = db.prepare(&sql)?;
        let rows = st.query_map(params_from_iter(args), summary_of)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn count(&self) -> anyhow::Result<u64> {
        Ok(self
            .db()
            .query_row("SELECT COUNT(*) FROM reports", [], |r| r.get::<_, i64>(0))?
            as u64)
    }

    /// The report and its directory.
    pub fn get(&self, id: &str) -> anyhow::Result<Option<(Report, PathBuf)>> {
        if !valid_id(id) {
            return Ok(None);
        }
        let dir: Option<String> = self
            .db()
            .query_row("SELECT dir FROM reports WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        let Some(dir) = dir else { return Ok(None) };
        let dir = self.root.join(dir);
        Ok(Some((read_report(&dir)?, dir)))
    }

    /// An attachment of a report and its path.
    pub fn file(&self, id: &str, name: &str) -> anyhow::Result<Option<(Attachment, PathBuf)>> {
        let Some((r, dir)) = self.get(id)? else {
            return Ok(None);
        };
        Ok(r.attachments
            .into_iter()
            .find(|a| a.name == name && a.name != "report.json" && !a.name.contains(['/', '\\']))
            .map(|a| {
                let p = dir.join(&a.name);
                (a, p)
            }))
    }

    /// Deletes reports last seen before `cutoff` and stray directories of
    /// days before it; returns how many reports went.
    pub fn purge(&self, cutoff: DateTime<Utc>) -> anyhow::Result<usize> {
        let db = self.db();
        let old: Vec<(String, String)> = {
            let mut st = db.prepare("SELECT id, dir FROM reports WHERE last_seen_at < ?1")?;
            let rows = st.query_map([ms(cutoff)], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<Result<_, _>>()?
        };
        for (id, dir) in &old {
            let p = self.root.join(dir);
            if p.exists() {
                std::fs::remove_dir_all(&p).with_context(|| format!("remove {}", p.display()))?;
            }
            db.execute("DELETE FROM client_ids WHERE report_id = ?1", [id])?;
            db.execute("DELETE FROM reports WHERE id = ?1", [id])?;
        }
        let mut removed = old.len();
        // Directories the index does not know (a crash before indexing).
        let cutoff_day = cutoff.date_naive();
        for app in AppName::ALL {
            let app_dir = self.root.join(app.as_str());
            let Ok(days) = std::fs::read_dir(&app_dir) else {
                continue;
            };
            for day in days.flatten() {
                let name = day.file_name().to_string_lossy().into_owned();
                let Ok(date) = NaiveDate::parse_from_str(&name, "%Y-%m-%d") else {
                    continue;
                };
                if date >= cutoff_day {
                    continue;
                }
                for entry in std::fs::read_dir(day.path())?.flatten() {
                    let id = entry.file_name().to_string_lossy().into_owned();
                    let indexed: bool = db
                        .query_row("SELECT 1 FROM reports WHERE id = ?1", [&id], |_| Ok(true))
                        .optional()?
                        .unwrap_or(false);
                    if !indexed {
                        std::fs::remove_dir_all(entry.path()).ok();
                        removed += 1;
                    }
                }
                // Gone when empty (a still-indexed, still-seen report keeps it).
                std::fs::remove_dir(day.path()).ok();
            }
        }
        Ok(removed)
    }

    /// Rebuilds the index from every `report.json` on disk.
    pub fn reindex(&self) -> anyhow::Result<usize> {
        let mut n = 0;
        let db = self.db();
        db.execute_batch("DELETE FROM reports; DELETE FROM client_ids;")?;
        for app in AppName::ALL {
            let Ok(days) = std::fs::read_dir(self.root.join(app.as_str())) else {
                continue;
            };
            for day in days.flatten() {
                for entry in std::fs::read_dir(day.path())?.flatten() {
                    let Ok(r) = read_report(&entry.path()) else {
                        continue;
                    };
                    let rel = format!(
                        "{}/{}/{}",
                        app.as_str(),
                        day.file_name().to_string_lossy(),
                        entry.file_name().to_string_lossy()
                    );
                    db.execute(
                        &format!("INSERT OR REPLACE INTO reports({SUMMARY_COLS}, dir) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"),
                        params![
                            r.id,
                            r.app.as_str(),
                            r.app_version,
                            r.os,
                            r.device,
                            r.kind.as_str(),
                            first_line(&r.message, 300),
                            r.fingerprint,
                            ms(r.occurred_at),
                            ms(r.received_at),
                            ms(r.last_seen_at),
                            r.count as i64,
                            r.attachments.len() as i64,
                            rel,
                        ],
                    )?;
                    if let Some(cid) = &r.client_id {
                        db.execute(
                            "INSERT OR IGNORE INTO client_ids(client_id, report_id) VALUES (?1, ?2)",
                            params![cid, r.id],
                        )?;
                    }
                    n += 1;
                }
            }
        }
        Ok(n)
    }
}
