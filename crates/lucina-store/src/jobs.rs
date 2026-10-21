//! Secretary job store, compatible with Node's `data/secretary-jobs.json`
//! (`{"jobs": [...], "updatedAt": "..."}`, last 200 jobs).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::atomic::write_atomic;
use crate::timeutil::now_iso;
use crate::Result;

pub const JOBS_KEEP: usize = 200;

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
    #[serde(flatten)]
    pub extra: Map<String, Value>,
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
    lock: Mutex<()>,
    /// Jobs this process runs; queued/running jobs not in here were
    /// interrupted by a restart.
    active: Mutex<HashSet<String>>,
}

impl JobStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            lock: Mutex::new(()),
            active: Mutex::new(HashSet::new()),
        }
    }

    fn read(&self) -> Result<Vec<Job>> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice::<JobsFile>(&bytes)
                .map(|f| f.jobs)
                .unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
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

    pub fn insert(&self, job: Job) -> Result<Job> {
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
        job.updated_at = now_iso();
        let out = job.clone();
        self.write(jobs)?;
        Ok(Some(out))
    }

    pub fn finish(&self, id: &str) {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }

    /// The job; a queued/running job this process does not own is marked
    /// `error: "interrupted"` (Lucina restarted mid-job), like Node.
    pub fn find(&self, id: &str) -> Result<Option<Job>> {
        let job = {
            let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            self.read()?.into_iter().find(|j| j.id == id)
        };
        let Some(job) = job else { return Ok(None) };
        let owned = self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&job.id);
        if (job.status == "queued" || job.status == "running") && !owned {
            return self.update(id, |j| {
                j.status = "error".into();
                j.error = Some("interrupted".into());
                j.finished_at = Some(now_iso());
            });
        }
        Ok(Some(job))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secretary-jobs.json");
        std::fs::write(
            &path,
            r#"{"jobs":[{"id":"j1","action":"graph_task","status":"running","result":null,"error":null,"imageUrl":null,"sessionId":"secretary_app","clientMessageId":null,"createdAt":"2026-10-21T15:10:00.000Z","startedAt":null,"finishedAt":null,"updatedAt":"2026-10-21T15:10:00.000Z"}],"updatedAt":"x"}"#,
        )
        .unwrap();
        let s = JobStore::new(&path);
        let j = s.find("j1").unwrap().unwrap();
        assert_eq!(j.status, "error");
        assert_eq!(j.error.as_deref(), Some("interrupted"));
        assert!(s.find("nope").unwrap().is_none());
        assert_eq!(j.public()["timelineItemId"], Value::Null);
    }
}
