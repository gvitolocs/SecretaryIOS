//! The repo pipeline every coding session goes through (design: `docs/design/agents-pipeline.md`):
//! task → workspace → merge → deploy → device.
//!
//! - A **task** is a workspace someone registered with `lagent task start` (or adopted from a
//!   hand-made worktree): who owns it, which session works in it, its branch.
//! - A **job** is one merge or deploy. Each repo has its own FIFO queue and lock: one job at a
//!   time per repo, so one repo's long gate never holds up another's.
//!   Each runs as its own process (`lagent pipeline-exec <dir>`, under the repo's merge lock,
//!   as a transient systemd unit when there is one), so restarting agentd, or deploying it,
//!   doesn't stop a job. agentd writes `job.json`; the executor ([`exec`]) appends to `log`
//!   and writes `result.json`.
//! - A **lease** says who may use a device (iPhone, Kindle, ...) until when.
//!
//! The queue worker and what happens after a job are in `manager/pipeline.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::store::{Store, StoreError};

pub mod exec;
pub mod repo;
mod sweep;

/// A registered piece of work: one workspace, one branch, one session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub workspace_id: String,
    pub title: String,
    /// The main checkout.
    pub repo: String,
    pub branch: String,
    /// The branch it merges into (`main`).
    pub trunk: String,
    /// Who asked for it (`owner`, a session's name, an agent id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// The Claude session (`local_…`) or Lucina agent working in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// `start` (lagent task start) or `adopt` (a hand-made worktree taken over).
    pub via: String,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_job: Option<String>,
    /// The commit main got from this task's last merge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<String>,
    /// The flaw this task fixes (`lagent task start --flaw`): a merge records its commit there,
    /// a deploy its tag (`lucina-rs flaws`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flaw: Option<String>,
    /// The platform of that flaw (`--target`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flaw_target: Option<String>,
    /// The stale question sent to Lucina's decide API (`dc-…`), so it is asked once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_decision: Option<String>,
}

/// A Claude session working in Lucina workspaces, and the agent that shows it
/// (`manager/sessions.rs`). A live session's workspaces are never swept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeSession {
    pub session_id: String,
    pub agent_id: String,
    pub host: String,
    /// The workspaces it works in, the latest last.
    #[serde(default)]
    pub workspaces: Vec<String>,
    pub last_seen_ms: u64,
    /// It ended, or went silent. The next beat clears this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Merge,
    Deploy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn done(self) -> bool {
        matches!(
            self,
            JobState::Succeeded | JobState::Failed | JobState::Cancelled
        )
    }
}

/// What the executor reads (`job.json`). Paths are absolute.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Spec {
    pub job_id: String,
    pub kind: JobKind,
    /// The main checkout.
    pub repo: String,
    pub trunk: String,
    /// `origin` when the repo has one; else the trunk is a local branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// Where the landing and deploy worktrees go (`~/Projects/.worktrees/<Repo>`).
    pub work_root: String,
    /// `CARGO_TARGET_DIR` for the gates and builds (shared between jobs of a repo).
    pub target_dir: String,
    /// The configured boundary for cache cleanup; absent in jobs queued by older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_root: Option<PathBuf>,
    /// Merge: the branch and the commit queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Deploy: the runtime (a `deploy/targets.tsv` row with a recipe in `.lucina/pipeline.toml`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// diffgate waivers, `rule=reason` (said in the report).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// Deploy even when `deploy-guard plan` sees nothing new.
    #[serde(default)]
    pub force: bool,
    /// Merge: run the tests of touched crates even under `cargo = "check"`.
    #[serde(default)]
    pub full: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateResult {
    pub name: String,
    pub command: String,
    pub ok: bool,
    pub seconds: u64,
    /// It failed, then passed when run again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flaky: bool,
}

/// What the executor wrote (`result.json`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub ok: bool,
    /// `MERGED`, `NOTHING_TO_MERGE`, `CONFLICT`, `GATE_FAILED`, `PUSH_FAILED`, `DEPLOYED`,
    /// `NOTHING_TO_DEPLOY`, `REFUSED`, `NO_RECIPE`, `INSTALL_FAILED`, `FAILED`, ...
    pub code: String,
    pub message: String,
    /// Merge: the commit the trunk now points at. Deploy: the commit deployed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed: Option<String>,
    /// `merge` or `already` (`rebase` in jobs from before 2026-11-07).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<GateResult>,
    /// Files that conflicted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
    /// Merge: gate files (`.lucina/`, scripts the gates run, `[merge] gate_files`) this merge changes.
    /// Set when the merge was refused for it (`GATE_CHANGE_NEEDS_ALLOW`) or let through by `--allow gate-change`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_changes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub job_id: String,
    pub seq: i64,
    pub kind: JobKind,
    pub state: JobState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    pub repo: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<String>,
    /// Keep the workspace after the merge (it continues from the merged commit).
    #[serde(default)]
    pub keep: bool,
    /// `lagent job next`: ahead of the other queued jobs of its repo.
    #[serde(default)]
    pub priority: bool,
    pub spec: Spec,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// The systemd unit (or `pid:<n>`) running it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// What agentd did after the job (archived the workspace, kept it, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
}

/// Who may use a device, until when.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lease {
    pub device: String,
    pub holder: String,
    pub purpose: String,
    pub acquired_at: String,
    pub expires_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

/// The devices Lucina hands out (`lagent device ls`).
pub const DEVICES: &[(&str, &str)] = &[
    ("iphone", "Giuseppe - iPhone (iPhone 16e): Secretary iOS builds, installed from the Mac with ios-install"),
    ("stick", "Samsung laptop USB stick (gbgo): LucinaOS Juno images"),
    ("kindle", "Kindle Paperwhite 3 (pw3): LucinaOS Minerva reader"),
    ("poco", "POCO X3 (surya): LucinaOS Diana"),
];

/// Where jobs live and how they are started.
#[derive(Debug, Clone)]
pub struct Config {
    /// `<state>/pipeline`: `jobs/<id>/{job.json,log,result.json}`.
    pub dir: PathBuf,
    /// The executor (`LUCINA_AGENTD_LAGENT`, else `lagent` beside this binary, else `~/.local/bin/lagent`).
    pub lagent: PathBuf,
    /// Run jobs as transient systemd user units (`LUCINA_AGENTD_PIPELINE_SPAWN=direct` turns it off).
    pub systemd: bool,
    /// `CARGO_TARGET_DIR`s go under it (`~/.cache/cargo-target`).
    pub target_root: PathBuf,
    /// `$WT_ROOT` (`~/Projects/.worktrees`).
    pub work_root: PathBuf,
    /// SecretaryIOS's merge lock, which deploys also hold ([`MERGE_LOCK`]; `LUCINA_AGENTD_MERGE_LOCK` for tests).
    pub lock: PathBuf,
}

impl Config {
    pub fn from_env(state_dir: &Path) -> Self {
        let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
        let lagent = std::env::var_os("LUCINA_AGENTD_LAGENT")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("lagent")))
                    .filter(|p| p.is_file())
            })
            .unwrap_or_else(|| home.join(".local/bin/lagent"));
        let systemd = std::env::var("LUCINA_AGENTD_PIPELINE_SPAWN")
            .map(|v| v != "direct")
            .unwrap_or(true)
            && std::env::var_os("XDG_RUNTIME_DIR").is_some()
            && crate::process::resolve_program("systemd-run").is_some();
        let projects = std::env::var_os("WT_PROJECTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("Projects"));
        Config {
            dir: state_dir.join("pipeline"),
            lagent,
            systemd,
            target_root: std::env::var_os("LUCINA_AGENTD_TARGET_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".cache/cargo-target")),
            work_root: std::env::var_os("WT_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| projects.join(".worktrees")),
            lock: std::env::var_os("LUCINA_AGENTD_MERGE_LOCK")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(MERGE_LOCK)),
        }
    }

    /// The lock a job holds. Each repo has its own, so a long LucinaOS gate never holds up
    /// SecretaryIOS; deploys and SecretaryIOS keep the one deploy-guard checks.
    pub fn lock_for(&self, repo: &str, kind: JobKind) -> PathBuf {
        let name = Path::new(repo)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if kind == JobKind::Deploy || name == "SecretaryIOS" {
            return self.lock.clone();
        }
        let stem = self
            .lock
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        self.lock.with_file_name(format!("{stem}-{name}.lock"))
    }

    pub fn job_dir(&self, id: &str) -> PathBuf {
        self.dir.join("jobs").join(id)
    }
}

/// The lock deploy-guard checks: SecretaryIOS merges and every deploy hold it.
pub const MERGE_LOCK: &str = "/tmp/lucina-merge.lock";

pub fn new_id(prefix: &str) -> String {
    format!(
        "{prefix}_{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    )
}

/// The pipeline's tables in the agents database.
#[derive(Clone)]
pub struct Pipeline {
    store: Arc<Store>,
    pub config: Config,
    /// Wakes the queue worker.
    pub wake: Arc<tokio::sync::Notify>,
}

impl Pipeline {
    pub fn open(store: Arc<Store>, config: Config) -> Result<Self, StoreError> {
        store.with_db(|db| {
            db.execute_batch(
                "CREATE TABLE IF NOT EXISTS pipeline_tasks (id TEXT PRIMARY KEY, record TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS pipeline_jobs (
                     id TEXT PRIMARY KEY,
                     seq INTEGER NOT NULL,
                     state TEXT NOT NULL,
                     record TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS pipeline_jobs_state ON pipeline_jobs (state, seq);
                 CREATE TABLE IF NOT EXISTS device_leases (device TEXT PRIMARY KEY, record TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS claude_sessions (id TEXT PRIMARY KEY, record TEXT NOT NULL);",
            )
        })?;
        std::fs::create_dir_all(config.dir.join("jobs"))?;
        Ok(Pipeline {
            store,
            config,
            wake: Arc::new(tokio::sync::Notify::new()),
        })
    }

    fn put(&self, table: &str, id: &str, json: String) -> Result<(), StoreError> {
        self.store.with_db(|db| {
            db.execute(&format!("INSERT INTO {table} VALUES (?1, ?2) ON CONFLICT DO UPDATE SET record = excluded.record"), params![id, json])
        })?;
        Ok(())
    }

    fn get<T: serde::de::DeserializeOwned>(
        &self,
        table: &str,
        key: &str,
        id: &str,
    ) -> Result<Option<T>, StoreError> {
        let json: Option<String> = self.store.with_db(|db| {
            db.query_row(
                &format!("SELECT record FROM {table} WHERE {key} = ?1"),
                params![id],
                |r| r.get(0),
            )
            .optional()
        })?;
        Ok(match json {
            Some(j) => Some(serde_json::from_str(&j)?),
            None => None,
        })
    }

    fn all<T: serde::de::DeserializeOwned>(&self, sql: &str) -> Result<Vec<T>, StoreError> {
        let rows: Vec<String> = self.store.with_db(|db| {
            let mut stmt = db.prepare(sql)?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()
        })?;
        rows.iter()
            .map(|j| serde_json::from_str(j).map_err(StoreError::from))
            .collect()
    }

    pub fn save_task(&self, t: &Task) -> Result<(), StoreError> {
        self.put("pipeline_tasks", &t.workspace_id, serde_json::to_string(t)?)
    }

    pub fn task(&self, workspace_id: &str) -> Option<Task> {
        self.get("pipeline_tasks", "id", workspace_id)
            .ok()
            .flatten()
    }

    pub fn tasks(&self) -> Vec<Task> {
        self.all("SELECT record FROM pipeline_tasks ORDER BY id")
            .unwrap_or_default()
    }

    pub fn update_task(&self, workspace_id: &str, f: impl FnOnce(&mut Task)) -> Option<Task> {
        let mut t = self.task(workspace_id)?;
        f(&mut t);
        self.save_task(&t).ok()?;
        Some(t)
    }

    pub fn save_job(&self, j: &Job) -> Result<(), StoreError> {
        let json = serde_json::to_string(j)?;
        let state = serde_json::to_value(j.state)?
            .as_str()
            .unwrap_or_default()
            .to_string();
        self.store.with_db(|db| {
            db.execute(
                "INSERT INTO pipeline_jobs (id, seq, state, record) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET state = excluded.state, record = excluded.record",
                params![j.job_id, j.seq, state, json],
            )
        })?;
        Ok(())
    }

    pub fn next_seq(&self) -> i64 {
        self.store
            .with_db(|db| {
                db.query_row(
                    "SELECT COALESCE(MAX(seq), 0) + 1 FROM pipeline_jobs",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap_or(1)
    }

    pub fn job(&self, id: &str) -> Option<Job> {
        self.get("pipeline_jobs", "id", id).ok().flatten()
    }

    /// Newest first, at most `limit`.
    pub fn jobs(&self, limit: usize) -> Vec<Job> {
        self.all(&format!(
            "SELECT record FROM pipeline_jobs ORDER BY seq DESC LIMIT {limit}"
        ))
        .unwrap_or_default()
    }

    /// Queued and running jobs, oldest first.
    pub fn active_jobs(&self) -> Vec<Job> {
        self.all(
            "SELECT record FROM pipeline_jobs WHERE state IN ('queued', 'running') ORDER BY seq",
        )
        .unwrap_or_default()
    }

    pub fn update_job(&self, id: &str, f: impl FnOnce(&mut Job)) -> Option<Job> {
        let mut j = self.job(id)?;
        f(&mut j);
        self.save_job(&j).ok()?;
        Some(j)
    }

    pub fn lease(&self, device: &str) -> Option<Lease> {
        self.get("device_leases", "device", device).ok().flatten()
    }

    pub fn leases(&self) -> Vec<Lease> {
        self.all("SELECT record FROM device_leases ORDER BY device")
            .unwrap_or_default()
    }

    pub fn save_lease(&self, l: &Lease) -> Result<(), StoreError> {
        self.put("device_leases", &l.device, serde_json::to_string(l)?)
    }

    pub fn drop_lease(&self, device: &str) -> Result<(), StoreError> {
        self.store.with_db(|db| {
            db.execute(
                "DELETE FROM device_leases WHERE device = ?1",
                params![device],
            )
        })?;
        Ok(())
    }

    pub fn claude_session(&self, id: &str) -> Option<ClaudeSession> {
        self.get("claude_sessions", "id", id).ok().flatten()
    }

    pub fn claude_sessions(&self) -> Vec<ClaudeSession> {
        self.all("SELECT record FROM claude_sessions")
            .unwrap_or_default()
    }

    pub fn save_claude_session(&self, s: &ClaudeSession) -> Result<(), StoreError> {
        self.put("claude_sessions", &s.session_id, serde_json::to_string(s)?)
    }

    pub fn drop_claude_session(&self, id: &str) -> Result<(), StoreError> {
        self.store
            .with_db(|db| db.execute("DELETE FROM claude_sessions WHERE id = ?1", params![id]))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_and_tasks_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let cfg = Config::from_env(dir.path());
        let p = Pipeline::open(store, cfg).unwrap();
        let spec = Spec {
            job_id: "job_1".into(),
            kind: JobKind::Merge,
            repo: "/r".into(),
            trunk: "main".into(),
            remote: Some("origin".into()),
            work_root: "/w".into(),
            target_dir: "/t".into(),
            target_root: None,
            branch: Some("b".into()),
            head: Some("abc".into()),
            title: None,
            runtime: None,
            note: None,
            allow: vec![],
            force: false,
            full: false,
        };
        for (i, id) in ["job_1", "job_2"].iter().enumerate() {
            let j = Job {
                job_id: id.to_string(),
                seq: p.next_seq(),
                kind: JobKind::Merge,
                state: JobState::Queued,
                workspace_id: None,
                runtime: None,
                repo: "/r".into(),
                branch: None,
                requested_by: None,
                keep: false,
                priority: false,
                spec: Spec {
                    job_id: id.to_string(),
                    ..spec.clone()
                },
                created_at: format!("t{i}"),
                started_at: None,
                finished_at: None,
                runner: None,
                outcome: None,
                after: None,
            };
            p.save_job(&j).unwrap();
        }
        assert_eq!(
            p.active_jobs().iter().map(|j| j.seq).collect::<Vec<_>>(),
            vec![1, 2]
        );
        p.update_job("job_1", |j| j.state = JobState::Succeeded)
            .unwrap();
        assert_eq!(p.active_jobs().len(), 1);
        assert_eq!(p.jobs(10)[0].job_id, "job_2");
        // One lock for every repo: the one deploy-guard checks and hand merges take.
        assert_eq!(MERGE_LOCK, "/tmp/lucina-merge.lock");
        let c = Config::from_env(dir.path());
        assert_eq!(c.lock, PathBuf::from(MERGE_LOCK));
        assert_eq!(
            c.lock_for("/p/Lucina/SecretaryIOS", JobKind::Merge),
            PathBuf::from(MERGE_LOCK)
        );
        assert_eq!(
            c.lock_for("/p/Lucina/LucinaOS", JobKind::Merge),
            PathBuf::from("/tmp/lucina-merge-LucinaOS.lock")
        );
        assert_eq!(
            c.lock_for("/p/Lucina/LucinaOS", JobKind::Deploy),
            PathBuf::from(MERGE_LOCK)
        );
    }
}
