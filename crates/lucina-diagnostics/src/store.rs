//! Reports on disk plus a SQLite index.
//!
//! ```text
//! <root>/index.sqlite
//! <root>/<app>/<YYYY-MM-DD>/<id>/report.json
//! <root>/<app>/<YYYY-MM-DD>/<id>/<attachment>…
//! ```
//!
//! A report directory is written under `<root>/.incoming/` and renamed into
//! place before its row is inserted, so the index never points at half a
//! report. Repeats of a fingerprint only bump `count`, `last_seen_at` and
//! `last_device` of the stored report.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Duration, TimeZone, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use crate::model::{Attachment, Kind, Report, Summary};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("index: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub struct Store {
    root: PathBuf,
    db: Mutex<Connection>,
}

/// A report ready to store (scrubbed, with its attachments in memory).
pub struct Prepared {
    pub report: Report,
    pub files: Vec<(Attachment, Vec<u8>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ingested {
    /// The stored report (the earlier one for a repeat).
    pub id: String,
    pub duplicate: bool,
    pub count: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub app: Option<String>,
    pub kind: Option<String>,
    pub device: Option<String>,
    /// Reports seen (first or again) at or after this time.
    pub since: Option<DateTime<Utc>>,
    /// Pagination: the `next` cursor of the previous page.
    pub before: Option<String>,
    pub limit: usize,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS reports (
    id TEXT PRIMARY KEY,
    app TEXT NOT NULL,
    app_version TEXT NOT NULL,
    os TEXT NOT NULL,
    device TEXT NOT NULL,
    kind TEXT NOT NULL,
    message TEXT NOT NULL,
    top_frame TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    received_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    count INTEGER NOT NULL DEFAULT 1,
    last_device TEXT NOT NULL,
    attachments INTEGER NOT NULL,
    bytes INTEGER NOT NULL,
    dir TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS reports_fingerprint ON reports(fingerprint);
CREATE INDEX IF NOT EXISTS reports_last_seen ON reports(last_seen_at);
CREATE INDEX IF NOT EXISTS reports_app_last_seen ON reports(app, last_seen_at);
";

/// Report ids: `dg_` and 16 lowercase hex digits.
pub fn new_id() -> String {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).expect("system randomness");
    format!(
        "dg_{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 19
        && id.starts_with("dg_")
        && id[3..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// An attachment name safe as a file name: `[A-Za-z0-9._-]`, at most 100
/// bytes, never hidden, never `report.json`.
pub fn safe_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    while s.starts_with('.') {
        s.remove(0);
    }
    if s.len() > 100 {
        s = s[s.len() - 100..].to_owned();
    }
    if s.is_empty() {
        s = "attachment".into();
    }
    if s == "report.json" {
        s = "attachment-report.json".into();
    }
    s
}

fn ms(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

fn at(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap_or_default()
}

fn row_summary(r: &rusqlite::Row<'_>) -> rusqlite::Result<(Summary, String)> {
    Ok((
        Summary {
            id: r.get(0)?,
            app: r.get(1)?,
            app_version: r.get(2)?,
            os: r.get(3)?,
            device: r.get(4)?,
            kind: r.get(5)?,
            message: r.get(6)?,
            top_frame: r.get(7)?,
            fingerprint: r.get(8)?,
            occurred_at: at(r.get(9)?),
            received_at: at(r.get(10)?),
            last_seen_at: at(r.get(11)?),
            count: r.get(12)?,
            last_device: r.get(13)?,
            attachments: r.get(14)?,
            bytes: r.get(15)?,
        },
        r.get(16)?,
    ))
}

const COLUMNS: &str = "id, app, app_version, os, device, kind, message, top_frame, fingerprint, \
     occurred_at, received_at, last_seen_at, count, last_device, attachments, bytes, dir, rowid";

/// Page cursors: `<last_seen_at ms>_<rowid>` of the last report of a page.
fn parse_cursor(c: &str) -> Option<(i64, i64)> {
    let (a, b) = c.split_once('_')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root.join(".incoming"))?;
        let db = Connection::open(root.join("index.sqlite"))?;
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

    /// Stores a report, or counts it as a repeat of a stored one with the same
    /// fingerprint (user reports are never merged).
    pub fn ingest(&self, p: Prepared) -> Result<Ingested> {
        let db = self.db();
        let r = &p.report;
        if r.kind != Kind::UserReport {
            let hit: Option<(String, i64)> = db
                .query_row(
                    "SELECT id, count FROM reports WHERE fingerprint = ?1 AND kind = ?2 \
                     ORDER BY received_at LIMIT 1",
                    params![r.fingerprint, r.kind.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((id, count)) = hit {
                db.execute(
                    "UPDATE reports SET count = count + 1, last_device = ?2, \
                     last_seen_at = MAX(last_seen_at, ?3) WHERE id = ?1",
                    params![id, r.device, ms(r.received_at)],
                )?;
                return Ok(Ingested {
                    id,
                    duplicate: true,
                    count: count + 1,
                });
            }
        }
        let rel = format!(
            "{}/{}/{}",
            r.app.as_str(),
            r.received_at.format("%Y-%m-%d"),
            r.id
        );
        let tmp = self.root.join(".incoming").join(&r.id);
        if tmp.exists() {
            std::fs::remove_dir_all(&tmp)?;
        }
        std::fs::create_dir_all(&tmp)?;
        for (a, bytes) in &p.files {
            std::fs::write(tmp.join(&a.name), bytes)?;
        }
        std::fs::write(tmp.join("report.json"), serde_json::to_vec_pretty(r)?)?;
        let dest = self.root.join(&rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&tmp, &dest)?;
        let bytes: u64 = p.files.iter().map(|(a, _)| a.size).sum();
        let top_frame = r.top_frames.first().cloned().unwrap_or_default();
        let message: String = r.message.chars().take(500).collect();
        let res = db.execute(
            "INSERT INTO reports (id, app, app_version, os, device, kind, message, top_frame, \
             fingerprint, occurred_at, received_at, last_seen_at, count, last_device, \
             attachments, bytes, dir) VALUES \
             (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13, ?14, ?15, ?16)",
            params![
                r.id,
                r.app.as_str(),
                r.app_version,
                r.os,
                r.device,
                r.kind.as_str(),
                message,
                top_frame,
                r.fingerprint,
                ms(r.occurred_at),
                ms(r.received_at),
                ms(r.received_at),
                r.device,
                p.files.len() as i64,
                bytes as i64,
                rel,
            ],
        );
        if let Err(e) = res {
            let _ = std::fs::remove_dir_all(&dest);
            return Err(e.into());
        }
        Ok(Ingested {
            id: r.id.clone(),
            duplicate: false,
            count: 1,
        })
    }

    /// One page of reports, newest `last_seen_at` first, and the cursor of
    /// the next page. A malformed cursor reads as the first page.
    pub fn list(&self, f: &Filter) -> Result<(Vec<Summary>, Option<String>)> {
        let mut sql = format!("SELECT {COLUMNS} FROM reports WHERE 1=1");
        let mut args: Vec<rusqlite::types::Value> = Vec::new();
        let mut push = |cond: &str, v: rusqlite::types::Value| {
            args.push(v);
            sql.push_str(&format!(" AND {cond} ?{}", args.len()));
        };
        if let Some(a) = &f.app {
            push("app =", a.clone().into());
        }
        if let Some(k) = &f.kind {
            push("kind =", k.clone().into());
        }
        if let Some(d) = &f.device {
            push("last_device =", d.clone().into());
        }
        if let Some(s) = f.since {
            push("last_seen_at >=", ms(s).into());
        }
        if let Some((t, rowid)) = f.before.as_deref().and_then(parse_cursor) {
            args.push(t.into());
            args.push(rowid.into());
            let (a, b) = (args.len() - 1, args.len());
            sql.push_str(&format!(
                " AND (last_seen_at < ?{a} OR (last_seen_at = ?{a} AND rowid < ?{b}))"
            ));
        }
        let limit = f.limit.clamp(1, 500);
        sql.push_str(&format!(
            " ORDER BY last_seen_at DESC, rowid DESC LIMIT {}",
            limit + 1
        ));
        let db = self.db();
        let mut st = db.prepare(&sql)?;
        let mut rows = st
            .query_map(rusqlite::params_from_iter(args), |r| {
                Ok((row_summary(r)?.0, r.get::<_, i64>(17)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next = (rows.len() > limit).then(|| {
            format!(
                "{}_{}",
                ms(rows[limit - 1].0.last_seen_at),
                rows[limit - 1].1
            )
        });
        rows.truncate(limit);
        Ok((rows.into_iter().map(|(s, _)| s).collect(), next))
    }

    fn row(&self, id: &str) -> Result<Option<(Summary, String)>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {COLUMNS} FROM reports WHERE id = ?1"),
                params![id],
                row_summary,
            )
            .optional()?)
    }

    /// The report and its index row.
    pub fn get(&self, id: &str) -> Result<Option<(Report, Summary)>> {
        if !valid_id(id) {
            return Ok(None);
        }
        let Some((summary, dir)) = self.row(id)? else {
            return Ok(None);
        };
        let path = self.root.join(dir).join("report.json");
        let report: Report = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        Ok(Some((report, summary)))
    }

    /// Where an attachment of a report is, by its stored name.
    pub fn file(&self, id: &str, name: &str) -> Result<Option<(PathBuf, Attachment)>> {
        let Some((report, _)) = self.get(id)? else {
            return Ok(None);
        };
        let Some(a) = report.attachments.into_iter().find(|a| a.name == name) else {
            return Ok(None);
        };
        let Some((_, dir)) = self.row(id)? else {
            return Ok(None);
        };
        Ok(Some((self.root.join(dir).join(&a.name), a)))
    }

    pub fn count(&self) -> Result<(i64, i64)> {
        Ok(self.db().query_row(
            "SELECT COUNT(*), COALESCE(SUM(count), 0) FROM reports",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// Deletes reports not seen for `days` days (rows and directories), and
    /// leftovers of interrupted writes. Returns how many reports went.
    pub fn prune(&self, now: DateTime<Utc>, days: i64) -> Result<usize> {
        let cutoff = ms(now - Duration::days(days.max(1)));
        let old: Vec<(String, String)> = {
            let db = self.db();
            let mut st = db.prepare("SELECT id, dir FROM reports WHERE last_seen_at < ?1")?;
            let rows = st
                .query_map(params![cutoff], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows
        };
        for (id, dir) in &old {
            let path = self.root.join(dir);
            if path.exists() {
                std::fs::remove_dir_all(&path)?;
            }
            self.db()
                .execute("DELETE FROM reports WHERE id = ?1", params![id])?;
            for parent in [path.parent(), path.parent().and_then(Path::parent)]
                .into_iter()
                .flatten()
            {
                // Only empty day/app directories go; a full one fails harmlessly.
                if parent != self.root {
                    let _ = std::fs::remove_dir(parent);
                }
            }
        }
        // Interrupted writes: anything in .incoming older than an hour.
        if let Ok(rd) = std::fs::read_dir(self.root.join(".incoming")) {
            for e in rd.flatten() {
                let stale = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|d| d.as_secs() > 3600);
                if stale {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        Ok(old.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_names() {
        let id = new_id();
        assert!(valid_id(&id), "{id}");
        assert!(!valid_id("dg_0123"));
        assert!(!valid_id("dg_0123456789ABCDEF"));
        assert!(!valid_id("../../etc/passwd00"));
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("C:\\logs\\main log.txt"), "main_log.txt");
        assert_eq!(safe_name(".hidden"), "hidden");
        assert_eq!(safe_name("report.json"), "attachment-report.json");
        assert_eq!(safe_name(""), "attachment");
        assert_eq!(safe_name("..."), "attachment");
    }
}
