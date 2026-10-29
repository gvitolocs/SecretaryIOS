//! Secretary job store, in the format of Node's `data/secretary-jobs.json`
//! (`{"jobs": [...], "updatedAt": "..."}`, last 200 jobs).
//!
//! One writer per file (docs/verification.md, P1): this store writes only
//! its own file (`data/lucina-rs/secretary-jobs.json`), never Node's. Every
//! record it writes carries `owner: "lucina-rs"` and the process `bootId`.
//! Node's file is read **read-only** for ids this store does not know (a
//! job Node started before the cutover), and those records are returned as
//! they are. Only this store's own records that no task of this process is
//! running are marked `interrupted` (a restart killed them); a job of
//! another runtime is never marked, because that runtime may still run it.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::atomic::write_atomic;
use crate::timeutil::now_iso;
use crate::Result;

pub const JOBS_KEEP: usize = 200;

/// Job results and errors are a sink (file, API, timeline): no secrets.
fn scrub(job: &mut Job) {
    let r = lucina_secrets::redactor();
    for t in [job.result.as_mut(), job.error.as_mut()]
        .into_iter()
        .flatten()
    {
        r.redact_string(t);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub action: String,
    pub status: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub image_url: Option<String>,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub client_message_id: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_item_id: Option<String>,
    /// The runtime that wrote the record ([`OWNER`] for this store; Node's
    /// records have none). Set by [`JobStore::insert`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// The writing process's boot (a random id per [`JobStore`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `owner` of the records this store writes.
pub const OWNER: &str = "lucina-rs";

/// Whether a poll may mark a job `interrupted` (pure; the P1 rule).
///
/// Only a queued/running record this runtime owns, that no task of this
/// process runs: one from an older boot, or one whose task ended without a
/// final status. Another runtime's job (Node's) is never marked: Node may
/// still be running it. Same-boot records in `active` are being worked on.
pub fn may_mark_interrupted(status: &str, owner: Option<&str>, active_here: bool) -> bool {
    (status == "queued" || status == "running") && owner == Some(OWNER) && !active_here
}

impl Job {
    /// The public job object of `docs/secretary-api.md`.
    pub fn public(&self) -> Value {
        serde_json::json!({
            "id": self.id,
            "action": self.action,
            "status": self.status,
            "result": self.result,
            "error": self.error,
            "imageUrl": self.image_url,
            "sessionId": self.session_id,
            "clientMessageId": self.client_message_id,
            "timelineItemId": self.timeline_item_id,
            "createdAt": self.created_at,
            "startedAt": self.started_at,
            "finishedAt": self.finished_at,
            "updatedAt": self.updated_at,
        })
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobsFile {
    #[serde(default)]
    jobs: Vec<Job>,
    #[serde(default)]
    updated_at: Option<String>,
}

#[derive(Debug)]
pub struct JobStore {
    path: PathBuf,
    /// Node's jobs file, read-only (ids this store does not know).
    node_path: Option<PathBuf>,
    boot_id: String,
    lock: Mutex<()>,
    /// Jobs this process runs; queued/running jobs not in here were
    /// interrupted by a restart.
    active: Mutex<HashSet<String>>,
}

fn read_jobs(path: &std::path::Path) -> Result<Vec<Job>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice::<JobsFile>(&bytes)
            .map(|f| f.jobs)
            .unwrap_or_default()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e.into()),
    }
}

impl JobStore {
    /// The store's own file (it is the only writer of it).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            node_path: None,
            boot_id: uuid::Uuid::new_v4().to_string(),
            lock: Mutex::new(()),
            active: Mutex::new(HashSet::new()),
        }
    }

    /// Also look up unknown ids in Node's jobs file (read-only). Ignored
    /// when it is this store's own file.
    pub fn with_node_file(mut self, path: Option<PathBuf>) -> Self {
        self.node_path = path.filter(|p| p != &self.path);
        self
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn boot_id(&self) -> &str {
        &self.boot_id
    }

    fn read(&self) -> Result<Vec<Job>> {
        read_jobs(&self.path)
    }

    fn write(&self, jobs: Vec<Job>) -> Result<()> {
        let skip = jobs.len().saturating_sub(JOBS_KEEP);
        let file = JobsFile {
            jobs: jobs.into_iter().skip(skip).collect(),
            updated_at: Some(now_iso()),
        };
        let mut text = serde_json::to_string_pretty(&file)?;
        text.push('\n');
        write_atomic(&self.path, text.as_bytes())?;
        Ok(())
    }

    pub fn insert(&self, mut job: Job) -> Result<Job> {
        scrub(&mut job);
        job.owner = Some(OWNER.into());
        job.boot_id = Some(self.boot_id.clone());
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job.id.clone());
        let mut jobs = self.read()?;
        jobs.push(job.clone());
        self.write(jobs)?;
        Ok(job)
    }

    pub fn update(&self, id: &str, patch: impl FnOnce(&mut Job)) -> Result<Option<Job>> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut jobs = self.read()?;
        let Some(job) = jobs.iter_mut().find(|j| j.id == id) else {
            return Ok(None);
        };
        patch(job);
        scrub(job);
        job.updated_at = now_iso();
        let out = job.clone();
        self.write(jobs)?;
        Ok(Some(out))
    }

    /// Takes over a queued/running job a restart interrupted: this process runs it now, so
    /// [`find`](Self::find) does not mark it `interrupted`. None if it is not unfinished.
    pub fn adopt(&self, id: &str) -> Result<Option<Job>> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut jobs = self.read()?;
        let Some(job) = jobs
            .iter_mut()
            .find(|j| j.id == id && matches!(j.status.as_str(), "queued" | "running"))
        else {
            return Ok(None);
        };
        job.boot_id = Some(self.boot_id.clone());
        job.updated_at = now_iso();
        let out = job.clone();
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_owned());
        self.write(jobs)?;
        Ok(Some(out))
    }

    pub fn finish(&self, id: &str) {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    /// The job. A queued/running job of this store that no task of this
    /// process runs is marked `error: "interrupted"` (Lucina restarted
    /// mid-job), like Node; see [`may_mark_interrupted`]. An id only Node's
    /// file has is returned as Node wrote it.
    pub fn find(&self, id: &str) -> Result<Option<Job>> {
        // Read, decide and write under one lock: a job that has just finished
        // (final write, then out of `active`) is never marked interrupted
        // (Node fixed the same race in fee2ccf).
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut jobs = self.read()?;
        let Some(job) = jobs.iter_mut().find(|j| j.id == id) else {
            return match &self.node_path {
                Some(p) => Ok(read_jobs(p)?.into_iter().find(|j| j.id == id)),
                None => Ok(None),
            };
        };
        let active_here = self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&job.id);
        if may_mark_interrupted(&job.status, job.owner.as_deref(), active_here) {
            job.status = "error".into();
            job.error = Some("interrupted".into());
            job.finished_at = Some(now_iso());
            job.updated_at = now_iso();
            let out = job.clone();
            self.write(jobs)?;
            return Ok(Some(out));
        }
        Ok(Some(job.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE_RUNNING: &str = r#"{"jobs":[{"id":"j1","action":"graph_task","status":"running","result":null,"error":null,"imageUrl":null,"sessionId":"secretary_app","clientMessageId":null,"createdAt":"2026-10-21T15:10:00.000Z","startedAt":null,"finishedAt":null,"updatedAt":"2026-10-21T15:10:00.000Z"}],"updatedAt":"x"}"#;

    fn job(id: &str) -> Job {
        serde_json::from_value(serde_json::json!({
            "id": id, "action": "graph_task", "status": "running", "sessionId": "s",
            "createdAt": "2026-10-22T10:00:00.000Z", "updatedAt": "2026-10-22T10:00:00.000Z"
        }))
        .unwrap()
    }

    #[test]
    fn interrupted_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secretary-jobs.json");
        let a = JobStore::new(&path);
        let mut j = job("j1");
        j.owner = Some("node".into()); // ignored: insert stamps its own owner
        a.insert(j).unwrap();
        // Running in this process: left alone.
        assert_eq!(a.find("j1").unwrap().unwrap().status, "running");
        // A restart (new boot): the task is gone, so the poll marks it.
        drop(a);
        let b = JobStore::new(&path);
        let j = b.find("j1").unwrap().unwrap();
        assert_eq!(j.status, "error");
        assert_eq!(j.error.as_deref(), Some("interrupted"));
        assert_eq!(j.owner.as_deref(), Some(OWNER));
        assert!(b.find("nope").unwrap().is_none());
        assert_eq!(j.public()["timelineItemId"], Value::Null);
        assert!(j.public().get("owner").is_none(), "not in the public job");
    }

    /// P1a (docs/verification.md): a job Node started just before the
    /// cutover, polled through Rust, was marked `interrupted` while Node
    /// still ran it. Now Node's file is read-only and its records are
    /// returned as they are.
    #[test]
    fn a_node_job_is_never_marked_interrupted() {
        let dir = tempfile::tempdir().unwrap();
        let node = dir.path().join("secretary-jobs.json");
        let own = dir.path().join("lucina-rs").join("secretary-jobs.json");
        std::fs::write(&node, NODE_RUNNING).unwrap();
        let s = JobStore::new(&own).with_node_file(Some(node.clone()));
        let j = s.find("j1").unwrap().unwrap();
        assert_eq!(j.status, "running");
        assert_eq!(j.error, None);
        assert_eq!(std::fs::read_to_string(&node).unwrap(), NODE_RUNNING);
        assert!(!own.exists(), "a poll writes nothing");
        // An ownerless record in the own file (written before owners) is
        // not this boot's to judge either.
        std::fs::create_dir_all(own.parent().unwrap()).unwrap();
        std::fs::write(&own, NODE_RUNNING).unwrap();
        let s = JobStore::new(&own);
        assert_eq!(s.find("j1").unwrap().unwrap().status, "running");
    }

    /// P1b: Node and Rust read-modify-wrote the same file under
    /// process-local locks, so one could drop the other's update. Rust now
    /// never writes Node's file.
    #[test]
    fn rust_never_writes_the_node_file() {
        let dir = tempfile::tempdir().unwrap();
        let node = dir.path().join("secretary-jobs.json");
        std::fs::write(&node, NODE_RUNNING).unwrap();
        let own = dir.path().join("own.json");
        let s = JobStore::new(&own).with_node_file(Some(node.clone()));
        s.insert(job("r1")).unwrap();
        s.update("r1", |j| j.status = "done".into()).unwrap();
        assert!(s
            .update("j1", |j| j.status = "done".into())
            .unwrap()
            .is_none());
        s.finish("r1");
        assert_eq!(std::fs::read_to_string(&node).unwrap(), NODE_RUNNING);
        assert_eq!(s.find("r1").unwrap().unwrap().status, "done");
        // The own file as Node's file is refused (no second writer).
        let same = JobStore::new(&node).with_node_file(Some(node.clone()));
        assert!(same.node_path.is_none());
    }

    #[test]
    fn mark_rule() {
        for (status, owner, active, want) in [
            ("running", Some(OWNER), false, true),
            ("queued", Some(OWNER), false, true),
            ("running", Some(OWNER), true, false),
            ("done", Some(OWNER), false, false),
            ("running", None, false, false),
            ("running", Some("node"), false, false),
        ] {
            assert_eq!(may_mark_interrupted(status, owner, active), want);
        }
    }

    #[test]
    fn adopt_takes_over_an_interrupted_job_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.json");
        let before = JobStore::new(&path);
        let mut job = sample_for_adopt("j1");
        job.status = "running".into();
        before.insert(job).unwrap();
        let mut done = sample_for_adopt("j2");
        done.status = "done".into();
        before.insert(done).unwrap();
        // A restart: a new store, the job is no longer active here.
        let after = JobStore::new(&path);
        let adopted = after.adopt("j1").unwrap().unwrap();
        assert_eq!(adopted.boot_id.as_deref(), Some(after.boot_id()));
        assert_eq!(after.find("j1").unwrap().unwrap().status, "running");
        assert!(after.adopt("j2").unwrap().is_none());
        assert!(after.adopt("nope").unwrap().is_none());
    }

    fn sample_for_adopt(id: &str) -> Job {
        Job {
            id: id.into(),
            action: "youtube_report".into(),
            status: "queued".into(),
            result: None,
            error: None,
            image_url: None,
            session_id: "s".into(),
            client_message_id: None,
            created_at: now_iso(),
            started_at: None,
            finished_at: None,
            updated_at: now_iso(),
            timeline_item_id: None,
            owner: None,
            boot_id: None,
            extra: Default::default(),
        }
    }
}
