//! The AI Lab: "Edit with Lucina…". A photo, a prompt and optionally a reference photo go to a small
//! local image-editing model that runs one job at a time (see [`crate::lab_run`]); the result waits in
//! the Lab until the owner accepts or discards it.
//!
//! - The queue is the `lab_jobs` table. A job never touches the library until it is accepted.
//! - Inputs, the result and scratch files live in `<data_dir>/lab/<job>/`, never in the store.
//! - **Accept makes a new item**, not a new version of the original: the original keeps its bytes, its
//!   version number and its device asset (a phone re-upload of the original must still find it), and
//!   the edit becomes its own picture with `links.derived_from` pointing back; the original's
//!   `links.lab_edits` lists the edits made from it. Source `lucina-lab` puts it in the Gallery's
//!   Lucina group, so it shows in All and is never offered to a device for deletion.
//! - No foreign keys: purging an original from the Trash must not be blocked by an old job. A job whose
//!   picture is gone simply has no "before" image.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};

use crate::config::Config;
use crate::db::{self, now_str, Item};
use crate::ingest::{self, Library};
use crate::meta::ClientMeta;
use crate::thumbs;

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS lab_jobs (
    id TEXT PRIMARY KEY,
    item_id TEXT NOT NULL,
    reference_item TEXT,
    prompt TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'done', 'failed', 'accepted', 'discarded')),
    phase TEXT,
    progress INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    seed INTEGER,
    steps INTEGER,
    model TEXT,
    result_hash TEXT,
    result_bytes INTEGER,
    result_w INTEGER,
    result_h INTEGER,
    accepted_item_id TEXT,
    notified_at TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT
);
CREATE INDEX IF NOT EXISTS lab_jobs_status ON lab_jobs(status, created_at);
CREATE INDEX IF NOT EXISTS lab_jobs_item ON lab_jobs(item_id);
"#;

/// Idempotent schema, called once from [`crate::db::Db::open`].
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)
}

pub const MAX_PROMPT_CHARS: usize = 1000;
/// Queued jobs the owner can have waiting at once.
pub const MAX_QUEUED: i64 = 30;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Job {
    pub id: String,
    pub item_id: String,
    pub reference_item: Option<String>,
    pub prompt: String,
    pub status: String,
    /// While running: `waiting` for the GPU, then `generating`.
    pub phase: Option<String>,
    pub progress: i64,
    pub error: Option<String>,
    pub seed: Option<i64>,
    pub steps: Option<i64>,
    pub model: Option<String>,
    pub result_hash: Option<String>,
    pub result_bytes: Option<i64>,
    pub result_w: Option<i64>,
    pub result_h: Option<i64>,
    pub accepted_item_id: Option<String>,
    pub notified_at: Option<String>,
    pub attempts: i64,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

const JOB_COLS: &str = "id, item_id, reference_item, prompt, status, phase, progress, error, seed, steps, model, \
    result_hash, result_bytes, result_w, result_h, accepted_item_id, notified_at, attempts, created_at, started_at, finished_at";

fn job_from_row(r: &rusqlite::Row) -> rusqlite::Result<Job> {
    Ok(Job {
        id: r.get(0)?,
        item_id: r.get(1)?,
        reference_item: r.get(2)?,
        prompt: r.get(3)?,
        status: r.get(4)?,
        phase: r.get(5)?,
        progress: r.get(6)?,
        error: r.get(7)?,
        seed: r.get(8)?,
        steps: r.get(9)?,
        model: r.get(10)?,
        result_hash: r.get(11)?,
        result_bytes: r.get(12)?,
        result_w: r.get(13)?,
        result_h: r.get(14)?,
        accepted_item_id: r.get(15)?,
        notified_at: r.get(16)?,
        attempts: r.get(17)?,
        created_at: r.get(18)?,
        started_at: r.get(19)?,
        finished_at: r.get(20)?,
    })
}

/// A job id is `lab_` plus 32 hex digits; anything else is not looked up (it ends up in a path).
pub fn id_ok(id: &str) -> bool {
    id.strip_prefix("lab_")
        .is_some_and(|h| h.len() == 32 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

pub fn job_dir(cfg: &Config, id: &str) -> PathBuf {
    cfg.data_dir.join("lab").join(id)
}

pub fn result_path(cfg: &Config, id: &str) -> PathBuf {
    job_dir(cfg, id).join("result.png")
}

#[derive(Clone, Debug, Default)]
pub struct NewJob {
    pub item_id: String,
    pub reference_item: Option<String>,
    pub prompt: String,
    pub steps: Option<i64>,
    pub seed: Option<i64>,
}

pub fn insert_job(c: &Connection, n: &NewJob) -> rusqlite::Result<Job> {
    let id = db::new_id("lab");
    c.execute(
        "INSERT INTO lab_jobs (id, item_id, reference_item, prompt, status, seed, steps, created_at)
         VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7)",
        params![id, n.item_id, n.reference_item, n.prompt, n.seed, n.steps, now_str()],
    )?;
    Ok(get_job(c, &id)?.expect("just inserted"))
}

pub fn get_job(c: &Connection, id: &str) -> rusqlite::Result<Option<Job>> {
    c.query_row(
        &format!("SELECT {JOB_COLS} FROM lab_jobs WHERE id = ?1"),
        [id],
        job_from_row,
    )
    .optional()
}

/// Jobs for the Lab view: running first, then the queue in order, then the rest newest first.
/// `statuses` empty = everything except discarded jobs.
pub fn list_jobs(
    c: &Connection,
    statuses: &[String],
    item_id: Option<&str>,
    limit: i64,
) -> rusqlite::Result<Vec<Job>> {
    let mut sql = format!("SELECT {JOB_COLS} FROM lab_jobs WHERE 1 = 1");
    let mut args: Vec<rusqlite::types::Value> = Vec::new();
    if statuses.is_empty() {
        sql.push_str(" AND status != 'discarded'");
    } else {
        let marks = vec!["?"; statuses.len()].join(",");
        sql.push_str(&format!(" AND status IN ({marks})"));
        args.extend(statuses.iter().cloned().map(rusqlite::types::Value::from));
    }
    if let Some(i) = item_id {
        sql.push_str(" AND item_id = ?");
        args.push(i.to_string().into());
    }
    sql.push_str(
        " ORDER BY CASE status WHEN 'running' THEN 0 WHEN 'queued' THEN 1 ELSE 2 END,
          CASE WHEN status = 'queued' THEN created_at END ASC, CASE WHEN status = 'queued' THEN rowid END ASC,
          created_at DESC, rowid DESC LIMIT ?",
    );
    args.push(limit.clamp(1, 500).into());
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params_from_iter(args), job_from_row)?;
    rows.collect()
}

pub fn count_queued(c: &Connection) -> rusqlite::Result<i64> {
    c.query_row(
        "SELECT COUNT(*) FROM lab_jobs WHERE status = 'queued'",
        [],
        |r| r.get(0),
    )
}

/// 1 for the job that runs next; `None` when the job is not queued.
pub fn queue_position(c: &Connection, job: &Job) -> rusqlite::Result<Option<i64>> {
    if job.status != "queued" {
        return Ok(None);
    }
    let ahead: i64 = c.query_row(
        "SELECT COUNT(*) FROM lab_jobs WHERE status = 'queued' AND (created_at < ?1 OR (created_at = ?1 AND rowid < (SELECT rowid FROM lab_jobs WHERE id = ?2)))",
        params![job.created_at, job.id],
        |r| r.get(0),
    )?;
    let running: i64 = c.query_row(
        "SELECT COUNT(*) FROM lab_jobs WHERE status = 'running'",
        [],
        |r| r.get(0),
    )?;
    Ok(Some(ahead + running + 1))
}

/// Take the oldest queued job and mark it running. One worker calls this, one job at a time.
pub fn claim_next(c: &mut Connection) -> rusqlite::Result<Option<Job>> {
    let tx = c.transaction()?;
    let id: Option<String> = tx
        .query_row(
            "SELECT id FROM lab_jobs WHERE status = 'queued' ORDER BY created_at, rowid LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = id else {
        return Ok(None);
    };
    tx.execute(
        "UPDATE lab_jobs SET status = 'running', phase = 'waiting', progress = 0, error = NULL, started_at = ?2,
            attempts = attempts + 1 WHERE id = ?1",
        params![id, now_str()],
    )?;
    let job = get_job(&tx, &id)?;
    tx.commit()?;
    Ok(job)
}

/// Jobs left `running` by a restart go back to the queue.
pub fn requeue_interrupted(c: &Connection) -> rusqlite::Result<usize> {
    c.execute(
        "UPDATE lab_jobs SET status = 'queued', phase = NULL, progress = 0 WHERE status = 'running'",
        [],
    )
}

pub fn set_progress(
    c: &Connection,
    id: &str,
    progress: i64,
    phase: Option<&str>,
) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE lab_jobs SET progress = ?2, phase = COALESCE(?3, phase) WHERE id = ?1 AND status = 'running'",
        params![id, progress.clamp(0, 100), phase],
    )?;
    Ok(())
}

pub fn set_model(c: &Connection, id: &str, model: &str) -> rusqlite::Result<()> {
    c.execute(
        "UPDATE lab_jobs SET model = ?2 WHERE id = ?1",
        params![id, model.chars().take(64).collect::<String>()],
    )?;
    Ok(())
}

pub struct ResultInfo {
    pub hash: String,
    pub bytes: i64,
    pub width: i64,
    pub height: i64,
}

pub fn finish_done(c: &Connection, id: &str, r: &ResultInfo) -> rusqlite::Result<bool> {
    let n = c.execute(
        "UPDATE lab_jobs SET status = 'done', phase = NULL, progress = 100, error = NULL, result_hash = ?2,
            result_bytes = ?3, result_w = ?4, result_h = ?5, finished_at = ?6
         WHERE id = ?1 AND status = 'running'",
        params![id, r.hash, r.bytes, r.width, r.height, now_str()],
    )?;
    Ok(n == 1)
}

pub fn finish_failed(c: &Connection, id: &str, error: &str) -> rusqlite::Result<bool> {
    let n = c.execute(
        "UPDATE lab_jobs SET status = 'failed', phase = NULL, error = ?2, finished_at = ?3
         WHERE id = ?1 AND status = 'running'",
        params![id, error.chars().take(400).collect::<String>(), now_str()],
    )?;
    Ok(n == 1)
}

/// Mark the owner as told; `false` when they already were (a job is announced once).
pub fn mark_notified(c: &Connection, id: &str) -> rusqlite::Result<bool> {
    let n = c.execute(
        "UPDATE lab_jobs SET notified_at = ?2 WHERE id = ?1 AND notified_at IS NULL",
        params![id, now_str()],
    )?;
    Ok(n == 1)
}

/// A failed job goes back to the queue (same prompt, same seed).
pub fn retry(c: &Connection, id: &str) -> rusqlite::Result<bool> {
    let n = c.execute(
        "UPDATE lab_jobs SET status = 'queued', phase = NULL, progress = 0, error = NULL, finished_at = NULL,
            started_at = NULL, notified_at = NULL, created_at = ?2 WHERE id = ?1 AND status = 'failed'",
        params![id, now_str()],
    )?;
    Ok(n == 1)
}

/// Remove a job that is not working and holds nothing to decide on (queued, failed, discarded).
pub fn delete_idle(c: &Connection, id: &str) -> rusqlite::Result<bool> {
    let n = c.execute(
        "DELETE FROM lab_jobs WHERE id = ?1 AND status IN ('queued', 'failed', 'discarded')",
        [id],
    )?;
    Ok(n == 1)
}

fn bytes_of_png(path: &Path) -> anyhow::Result<(i64, i64)> {
    let (w, h) = image::ImageReader::open(path)?
        .with_guessed_format()?
        .into_dimensions()?;
    Ok((i64::from(w), i64::from(h)))
}

/// Validate a runner's output (a decodable image) and describe it.
pub fn inspect_result(path: &Path) -> anyhow::Result<ResultInfo> {
    let (w, h) = bytes_of_png(path)?;
    if w == 0 || h == 0 {
        anyhow::bail!("the result image is empty");
    }
    Ok(ResultInfo {
        hash: crate::store::hash_file(path)?,
        bytes: std::fs::metadata(path)?.len() as i64,
        width: w,
        height: h,
    })
}

/// What an edit starts from: the picture as the owner sees it (EXIF orientation and the saved rotation
/// applied), scaled down to `max_side` on its long side. Falls back to the stored 1600 px preview when
/// the original cannot be decoded here (HEIC).
pub fn display_image(
    lib: &Library,
    item: &Item,
    max_side: u32,
) -> anyhow::Result<image::DynamicImage> {
    let mut img = None;
    if let Some((path, _)) = ingest::original_path(lib, &item.id)? {
        img = thumbs::load_oriented(&path).ok();
    }
    let mut img = match img {
        Some(i) => i,
        None => {
            let large = thumbs::dir_for(&lib.cfg, &item.id).join("l.jpg");
            thumbs::load_oriented(&large)
                .map_err(|e| anyhow::anyhow!("the picture cannot be decoded: {e}"))?
        }
    };
    img = match item.rotation {
        90 => img.rotate90(),
        180 => img.rotate180(),
        270 => img.rotate270(),
        _ => img,
    };
    if img.width().max(img.height()) > max_side {
        img = img.resize(max_side, max_side, image::imageops::FilterType::Lanczos3);
    }
    Ok(image::DynamicImage::ImageRgb8(img.to_rgb8()))
}

/// Write the model's inputs into the job directory: `input.png` and, with a reference, `reference.png`.
/// Blocking.
pub fn prepare_inputs(
    lib: &Library,
    job: &Job,
    max_side: u32,
) -> anyhow::Result<(PathBuf, Option<PathBuf>)> {
    let dir = job_dir(&lib.cfg, &job.id);
    std::fs::create_dir_all(&dir)?;
    let load = |id: &str| -> anyhow::Result<Item> {
        let item = lib
            .db
            .with(|c| db::get_item(c, id))?
            .ok_or_else(|| anyhow::anyhow!("the picture is no longer in the library"))?;
        if item.kind != "photo" {
            anyhow::bail!("only pictures can be edited");
        }
        Ok(item)
    };
    let item = load(&job.item_id)?;
    let input = dir.join("input.png");
    display_image(lib, &item, max_side)?.save(&input)?;
    let reference = match &job.reference_item {
        Some(id) => {
            let r = load(id)?;
            let p = dir.join("reference.png");
            display_image(lib, &r, max_side)?.save(&p)?;
            Some(p)
        }
        None => None,
    };
    Ok((input, reference))
}

fn stem(title: &str) -> String {
    let base = title.rsplit(['/', '\\']).next().unwrap_or(title);
    let stem = base.rsplit_once('.').map_or(base, |(s, _)| s);
    let clean: String = stem.chars().filter(|c| !c.is_control()).take(80).collect();
    if clean.trim().is_empty() {
        "Picture".into()
    } else {
        clean.trim().to_string()
    }
}

#[derive(Debug)]
pub enum AcceptError {
    NotFound,
    /// The job is not `done` (status given).
    Conflict(String),
    Failed(String),
}

#[derive(Debug)]
pub struct Accepted {
    pub job: Job,
    pub item_id: String,
    /// The edit's bytes already were an item: nothing new was added.
    pub duplicate: bool,
}

/// Put a finished result into the library as a new item linked to the original. Blocking.
///
/// The original's file, row (except its `links`) and version are not touched. The result file is copied
/// first, so a failure leaves the job `done` and the result in place.
pub fn accept(lib: &Library, job_id: &str) -> Result<Accepted, AcceptError> {
    static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
    let fail = |e: &dyn std::fmt::Display| AcceptError::Failed(e.to_string());
    let job = lib
        .db
        .with(|c| get_job(c, job_id))
        .map_err(|e| fail(&e))?
        .ok_or(AcceptError::NotFound)?;
    if job.status != "done" {
        return Err(AcceptError::Conflict(job.status));
    }
    let src = result_path(&lib.cfg, &job.id);
    if !src.is_file() {
        return Err(AcceptError::Failed("the result file is gone".into()));
    }
    let original = lib
        .db
        .with(|c| db::get_item(c, &job.item_id))
        .map_err(|e| fail(&e))?;
    let copy = job_dir(&lib.cfg, &job.id).join("accept.png");
    std::fs::copy(&src, &copy).map_err(|e| fail(&e))?;
    let title = format!(
        "{} (Lucina edit)",
        original
            .as_ref()
            .map_or_else(|| "Picture".into(), |o| stem(&o.title))
    );
    let client = ClientMeta {
        filename: Some(format!("{title}.png")),
        mime: Some("image/png".into()),
        // The edit sits next to the picture it came from.
        taken_at: original.as_ref().and_then(|o| o.taken_at.clone()),
        lat: original
            .as_ref()
            .and_then(|o| o.place.as_ref().map(|p| p.lat)),
        lon: original
            .as_ref()
            .and_then(|o| o.place.as_ref().map(|p| p.lon)),
        source: Some("lucina-lab".into()),
        title: Some(title),
        caption: Some(job.prompt.clone()),
        ..ClientMeta::default()
    };
    let r = match ingest::ingest_file(lib, &copy, None, &client, None) {
        Ok(r) => r,
        Err(e) => {
            let _ = std::fs::remove_file(&copy);
            return Err(fail(&e));
        }
    };
    let jid = job.id.clone();
    let orig_id = job.item_id.clone();
    let prompt = job.prompt.clone();
    let new_id = r.item_id.clone();
    let duplicate = r.duplicate;
    lib.db
        .with(move |c| {
            let tx = c.transaction()?;
            if !duplicate {
                set_links(
                    &tx,
                    &new_id,
                    |_| json!({ "derived_from": orig_id, "lab_job": jid, "prompt": prompt }),
                )?;
                if tx
                    .query_row("SELECT 1 FROM items WHERE id = ?1", [&orig_id], |_| Ok(()))
                    .optional()?
                    .is_some()
                {
                    set_links(&tx, &orig_id, |old| {
                        let mut v = if old.is_object() { old.clone() } else { json!({}) };
                        let mut edits = v["lab_edits"].as_array().cloned().unwrap_or_default();
                        edits.push(json!(new_id));
                        v["lab_edits"] = Value::Array(edits);
                        v
                    })?;
                }
            }
            tx.execute(
                "UPDATE lab_jobs SET status = 'accepted', accepted_item_id = ?2, finished_at = COALESCE(finished_at, ?3)
                 WHERE id = ?1 AND status = 'done'",
                params![jid, new_id, now_str()],
            )?;
            tx.commit()
        })
        .map_err(|e| fail(&e))?;
    let _ = std::fs::remove_dir_all(job_dir(&lib.cfg, &job.id));
    lib.emit("lab.accepted", &r.item_id);
    let job = lib
        .db
        .with(|c| get_job(c, job_id))
        .map_err(|e| fail(&e))?
        .ok_or(AcceptError::NotFound)?;
    Ok(Accepted {
        job,
        item_id: r.item_id,
        duplicate: r.duplicate,
    })
}

/// Rewrite an item's `links` JSON in place (only that column).
fn set_links(
    c: &Connection,
    item_id: &str,
    f: impl FnOnce(&Value) -> Value,
) -> rusqlite::Result<()> {
    let old: Option<String> =
        c.query_row("SELECT links FROM items WHERE id = ?1", [item_id], |r| {
            r.get(0)
        })?;
    let old = old
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null);
    c.execute(
        "UPDATE items SET links = ?2 WHERE id = ?1",
        params![item_id, f(&old).to_string()],
    )?;
    Ok(())
}

/// Throw the result away: the file goes, the row stays as `discarded` (hidden from the Lab).
/// Allowed for `done` and `failed` jobs. Blocking.
pub fn discard(lib: &Library, job_id: &str) -> Result<Job, AcceptError> {
    let fail = |e: &dyn std::fmt::Display| AcceptError::Failed(e.to_string());
    let job = lib
        .db
        .with(|c| get_job(c, job_id))
        .map_err(|e| fail(&e))?
        .ok_or(AcceptError::NotFound)?;
    if !matches!(job.status.as_str(), "done" | "failed") {
        return Err(AcceptError::Conflict(job.status));
    }
    let _ = std::fs::remove_dir_all(job_dir(&lib.cfg, &job.id));
    lib.db
        .with(|c| {
            c.execute(
                "UPDATE lab_jobs SET status = 'discarded', phase = NULL, result_hash = NULL, result_bytes = NULL,
                    finished_at = COALESCE(finished_at, ?2) WHERE id = ?1 AND status IN ('done', 'failed')",
                params![job_id, now_str()],
            )
        })
        .map_err(|e| fail(&e))?;
    lib.db
        .with(|c| get_job(c, job_id))
        .map_err(|e| fail(&e))?
        .ok_or(AcceptError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_ids_are_checked_before_they_reach_a_path() {
        assert!(id_ok(&db::new_id("lab")));
        for bad in [
            "",
            "lab_",
            "lab_../x",
            "itm_0123456789abcdef0123456789abcdef",
            "lab_xyz",
        ] {
            assert!(!id_ok(bad), "{bad}");
        }
    }

    #[test]
    fn titles_lose_folders_and_extensions() {
        assert_eq!(stem("a/b/IMG_1.jpg"), "IMG_1");
        assert_eq!(stem("  "), "Picture");
        assert_eq!(stem("noext"), "noext");
    }

    #[test]
    fn queue_order_and_claim() {
        let mut c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        let mk = |p: &str| NewJob {
            item_id: "itm_1".into(),
            prompt: p.into(),
            ..NewJob::default()
        };
        let a = insert_job(&c, &mk("a")).unwrap();
        let b = insert_job(&c, &mk("b")).unwrap();
        assert_eq!(queue_position(&c, &a).unwrap(), Some(1));
        assert_eq!(queue_position(&c, &b).unwrap(), Some(2));
        let first = claim_next(&mut c).unwrap().unwrap();
        assert_eq!(first.id, a.id);
        assert_eq!(first.status, "running");
        let b = get_job(&c, &b.id).unwrap().unwrap();
        assert_eq!(
            queue_position(&c, &b).unwrap(),
            Some(2),
            "the running job counts"
        );
        assert_eq!(requeue_interrupted(&c).unwrap(), 1);
        assert_eq!(claim_next(&mut c).unwrap().unwrap().id, a.id);
    }

    #[test]
    fn guards_keep_status_changes_one_way() {
        let mut c = Connection::open_in_memory().unwrap();
        migrate(&c).unwrap();
        let j = insert_job(
            &c,
            &NewJob {
                item_id: "i".into(),
                prompt: "p".into(),
                ..NewJob::default()
            },
        )
        .unwrap();
        assert!(
            !finish_failed(&c, &j.id, "x").unwrap(),
            "only a running job can fail"
        );
        claim_next(&mut c).unwrap();
        assert!(finish_failed(&c, &j.id, "x").unwrap());
        assert!(!delete_idle(&c, "nope").unwrap());
        assert!(retry(&c, &j.id).unwrap());
        assert!(!retry(&c, &j.id).unwrap(), "already queued");
        assert!(delete_idle(&c, &j.id).unwrap());
        assert!(mark_notified(&c, &j.id).is_ok());
    }
}
