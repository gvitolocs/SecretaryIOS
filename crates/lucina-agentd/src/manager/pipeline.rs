//! The pipeline from the manager's side ([`crate::pipeline`]): tasks start workspaces,
//! merges and deploys wait in one FIFO queue, one job runs at a time, and after a merge
//! the workspace is archived (or kept, starting from the merged commit). Device leases
//! and the board are here too.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lucina_agents::workspace::{WorkspaceKind, WorkspaceRecord};
use lucina_agents::worktree_guard::Trigger;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{Manager, ManagerError};
use crate::now_iso;
use crate::pipeline::{self, Job, JobKind, JobState, Lease, Outcome, Pipeline, Spec, Task};
use crate::trunks::{self, Trunk};
use crate::workspaces::{git, procs, CreateWorkspace, WsError};

/// How long a lease lasts unless asked otherwise, and at most.
const LEASE_DEFAULT_MIN: i64 = 30;
const LEASE_MAX_MIN: i64 = 240;
/// A workspace with no commit or index change for this long, and not merged, is stale.
pub const STALE_DAYS: i64 = 3;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartTask {
    pub title: String,
    /// Any path in the repo (default: SecretaryIOS).
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
    /// The flaw this task fixes, and on which platform.
    #[serde(default)]
    pub flaw: Option<String>,
    #[serde(default)]
    pub flaw_target: Option<String>,
    /// A commit or ref to base the worktree on instead of the trunk tip (`--base`), accepted only
    /// when it is on the trunk or on one of the repo's allowed bases.
    #[serde(default)]
    pub base: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptTask {
    pub path: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeRequest {
    /// A workspace id, its path, or its branch.
    pub workspace: String,
    #[serde(default)]
    pub keep: bool,
    #[serde(default)]
    pub allow: Vec<String>,
    /// Run the tests too, under `cargo = "check"`.
    #[serde(default)]
    pub full: bool,
    #[serde(default)]
    pub requested_by: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployRequest {
    pub runtime: String,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub requested_by: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseRequest {
    pub holder: String,
    #[serde(default)]
    pub purpose: Option<String>,
    #[serde(default)]
    pub minutes: Option<i64>,
    #[serde(default)]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseRequest {
    pub holder: String,
    /// Release someone else's lease (the reason is required).
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub reason: Option<String>,
}

fn default_repo() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/Projects/Lucina/SecretaryIOS")
}

fn invalid(m: impl Into<String>) -> ManagerError {
    ManagerError::Invalid(m.into())
}

/// What the pipeline knows about the repo around `path`: where it is, and its trunk with the
/// remote its merges land through ([`Trunk`], decided by [`crate::trunks`]).
fn repo_info(path: &Path) -> Result<Trunk, ManagerError> {
    trunks::resolve(path)
        .ok_or_else(|| invalid(format!("Not a git repository: {}", path.display())))
}

/// The full commit id `rev` names in `dir` (`^{commit}`, so a tag or a tree goes to its commit),
/// `None` when it names nothing there.
async fn commit_at(dir: &Path, rev: &str) -> Option<String> {
    git::read(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .await
    .filter(|s| !s.is_empty())
}

/// The pure part of the `--base` check, from what git answered about the base: `commit` is the
/// full id `<rev>` resolved to (`None` when it resolves to nothing here), `on_trunk` and
/// `on_allowed` whether that commit is an ancestor of, or equal to, the trunk tip and one of
/// `allowed`.
///
/// `Ok` is the full id to create the worktree from; `Err` why the base is refused.
fn base_decision(
    rev: &str,
    repo: &str,
    trunk_ref: &str,
    allowed: &[&str],
    commit: Option<&str>,
    on_trunk: bool,
    on_allowed: bool,
) -> Result<String, String> {
    let Some(commit) = commit else {
        return Err(format!("base {rev} is not a commit in {repo}"));
    };
    if on_trunk || on_allowed {
        return Ok(commit.to_string());
    }
    Err(format!(
        "base {} is not on {trunk_ref} nor on an allowed base of {repo}; allowed: {}",
        &commit[..commit.len().min(10)],
        if allowed.is_empty() {
            "none".to_string()
        } else {
            allowed.join(", ")
        }
    ))
}

/// `lagent task start --base <rev>`: the commit the worktree is to be created from, resolved in
/// the repository and checked against the trunk tip and [`Trunk::allowed_bases`] before anything
/// is created. An allowed base the checkout does not have is skipped and logged: the row is the
/// owner's, not a promise about what every machine has fetched.
async fn requested_base(repo: &Trunk, rev: &str) -> Result<String, ManagerError> {
    let trunk_ref = repo.base_ref();
    let allowed = repo.allowed_bases();
    let commit = commit_at(&repo.root, rev).await;
    let mut on_trunk = false;
    let mut on_allowed = false;
    if let Some(commit) = &commit {
        on_trunk = git::merged_into(&repo.root, commit, &trunk_ref).await;
        for &name in allowed {
            let Some(tip) = commit_at(&repo.root, name).await else {
                tracing::warn!(repo = %repo.name, base = %name, "allowed base does not resolve");
                continue;
            };
            if git::merged_into(&repo.root, commit, &tip).await {
                on_allowed = true;
                break;
            }
        }
    }
    base_decision(
        rev,
        &repo.name,
        &trunk_ref,
        allowed,
        commit.as_deref(),
        on_trunk,
        on_allowed,
    )
    .map_err(invalid)
}

fn slug(title: &str) -> String {
    let s = lucina_agents::branch_slug::slugify(title);
    let s: String = s.chars().take(48).collect();
    s.trim_end_matches('-').to_string()
}

fn minutes_from_now(m: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::minutes(m))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn expired(iso: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|t| t < chrono::Utc::now())
        .unwrap_or(true)
}

/// Seconds since a commit or index change in the worktree.
async fn idle_secs(dir: &Path) -> Option<i64> {
    let ct: i64 = git::read(dir, &["log", "-1", "--format=%ct", "HEAD"])
        .await?
        .parse()
        .ok()?;
    let index = git::read(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )
    .await;
    let mt = index
        .and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some(chrono::Utc::now().timestamp() - ct.max(mt))
}

impl Manager {
    pub fn pipeline(&self) -> &Pipeline {
        &self.inner.pipeline
    }

    /// The commit ledger in the state directory (`pipeline.config.dir` is `<state>/pipeline`).
    fn ledger_path(&self) -> String {
        let dir = &self.pipeline().config.dir;
        dir.parent()
            .unwrap_or(dir)
            .join(crate::ledger::FILE)
            .display()
            .to_string()
    }

    /// `lagent task start`: fetch, make a worktree from the trunk, register the task.
    pub async fn start_task(
        &self,
        req: StartTask,
    ) -> Result<(Task, WorkspaceRecord), ManagerError> {
        if req.title.trim().is_empty() {
            return Err(invalid("A task needs a title"));
        }
        let repo = repo_info(Path::new(&req.repo.clone().unwrap_or_else(default_repo)))?;
        if let Some(r) = &repo.remote {
            git::must(&repo.root, &["fetch", "-q", r])
                .await
                .map_err(|e| ManagerError::from(WsError::Git(format!("fetch: {e}"))))?;
        }
        let branch = req
            .branch
            .clone()
            .filter(|b| !b.trim().is_empty())
            .unwrap_or_else(|| slug(&req.title));
        if branch.is_empty() {
            return Err(invalid("Give the task a --branch"));
        }
        // `--base` names a commit to start from instead of the trunk tip; nothing is created
        // before it has been resolved and checked, and without it the base is the trunk tip.
        let base = match &req.base {
            Some(rev) => Some(requested_base(&repo, rev).await?),
            None => None,
        };
        let start = base.clone().unwrap_or_else(|| repo.base_ref());
        let (w, _) = self
            .create_workspace(CreateWorkspace {
                isolation: "worktree".into(),
                path: Some(repo.root.display().to_string()),
                title: Some(req.title.trim().to_string()),
                mode: Some("branch-off".into()),
                new_branch: Some(branch),
                base: Some(start),
                ..CreateWorkspace::default()
            })
            .await?;
        let task = Task {
            workspace_id: w.workspace_id.clone(),
            title: req.title.trim().to_string(),
            repo: repo.root.display().to_string(),
            branch: w.branch.clone().unwrap_or_default(),
            trunk: repo.branch,
            base,
            base_ref: req.base,
            owner: req.owner,
            session: req.session,
            flaw: req.flaw,
            flaw_target: req.flaw_target,
            via: "start".into(),
            created_at: now_iso(),
            last_job: None,
            merged_sha: None,
            merged_at: None,
            stale_decision: None,
        };
        self.pipeline()
            .save_task(&task)
            .map_err(ManagerError::Store)?;
        Ok((task, w))
    }

    /// `lagent task adopt`: take over a worktree someone made by hand, so it merges
    /// through the queue and is archived after.
    pub async fn adopt_task(
        &self,
        req: AdoptTask,
    ) -> Result<(Task, WorkspaceRecord), ManagerError> {
        let dir = git::canon(Path::new(&req.path));
        let repo = repo_info(&dir)?;
        let w = match self.workspaces().find_by_cwd(&dir.display().to_string()) {
            Some(w) => w,
            None => {
                self.workspaces()
                    .adopt(&dir, req.title.clone(), &repo.base_ref())
                    .await?
            }
        };
        if let Some(t) = self.pipeline().task(&w.workspace_id) {
            // Adopting again is how a session takes a task back (it lost its link, or changed).
            let t = self
                .pipeline()
                .update_task(&w.workspace_id, |t| {
                    if req.session.is_some() {
                        t.session = req.session.clone();
                    }
                    if req.owner.is_some() {
                        t.owner = req.owner.clone();
                    }
                })
                .unwrap_or(t);
            return Ok((t, w));
        }
        let task = Task {
            workspace_id: w.workspace_id.clone(),
            title: req
                .title
                .clone()
                .or(w.title.clone())
                .unwrap_or_else(|| w.display_name.clone()),
            repo: repo.root.display().to_string(),
            branch: w.branch.clone().unwrap_or_default(),
            trunk: repo.branch,
            base: None,
            base_ref: None,
            owner: req.owner,
            session: req.session,
            flaw: None,
            flaw_target: None,
            via: "adopt".into(),
            created_at: now_iso(),
            last_job: None,
            merged_sha: None,
            merged_at: None,
            stale_decision: None,
        };
        self.pipeline()
            .save_task(&task)
            .map_err(ManagerError::Store)?;
        Ok((task, w))
    }

    /// A project's checkout moved on disk: the project, its workspaces and its tasks follow.
    pub fn move_project(
        &self,
        id: &str,
        path: &str,
        name: Option<String>,
    ) -> Result<Value, ManagerError> {
        let old = self.workspaces().project(id).map(|p| p.root_path);
        let (project, moved, skipped) =
            self.workspaces().move_project(id, Path::new(path), name)?;
        for t in self.pipeline().tasks() {
            if Some(&t.repo) == old.as_ref() {
                self.pipeline()
                    .update_task(&t.workspace_id, |t| t.repo = project.root_path.clone());
            }
        }
        let list = |v: &[(String, String)]| {
            v.iter()
                .map(|(w, c)| json!({"workspaceId": w, "cwd": c}))
                .collect::<Vec<_>>()
        };
        Ok(json!({"project": project, "moved": list(&moved), "skipped": list(&skipped)}))
    }

    /// A workspace by id, path or branch.
    fn find_workspace(&self, key: &str) -> Option<WorkspaceRecord> {
        let key = key.trim().trim_end_matches('/');
        if let Some(w) = self
            .workspaces()
            .get(key)
            .filter(|w| w.archived_at.is_none())
        {
            return Some(w);
        }
        let list = self.workspaces().list(true);
        list.iter()
            .find(|w| w.cwd == key)
            .or_else(|| list.iter().find(|w| w.branch.as_deref() == Some(key)))
            .cloned()
    }

    /// `lagent merge`: queue the workspace's committed head (idempotent while one is queued).
    pub async fn queue_merge(&self, req: MergeRequest) -> Result<Job, ManagerError> {
        let w = self.find_workspace(&req.workspace).ok_or_else(|| {
            ManagerError::NotFound(format!("Workspace not found: {}", req.workspace))
        })?;
        if w.kind != WorkspaceKind::Worktree {
            return Err(invalid(format!(
                "{} is not a worktree: only task worktrees merge through the queue",
                w.cwd
            )));
        }
        let dir = PathBuf::from(&w.cwd);
        let branch = git::info(&dir)
            .await
            .branch
            .or(w.branch.clone())
            .ok_or_else(|| invalid("The workspace is on a detached HEAD; check out its branch"))?;
        let repo = repo_info(&dir)?;
        if branch == repo.branch {
            return Err(invalid(format!(
                "The workspace is on {} itself",
                repo.branch
            )));
        }
        if git::is_dirty(&dir).await {
            return Err(invalid(format!("{} has uncommitted changes: commit them (or stash what isn't part of the task), then merge", w.cwd)));
        }
        let head = git::read(&dir, &["rev-parse", "HEAD"])
            .await
            .ok_or_else(|| invalid("Cannot read the workspace's HEAD"))?;
        if let Some(j) = self.pipeline().active_jobs().into_iter().find(|j| {
            j.kind == JobKind::Merge && j.workspace_id.as_deref() == Some(w.workspace_id.as_str())
        }) {
            if j.spec.head.as_deref() == Some(head.as_str()) {
                return Ok(j);
            }
            return Err(ManagerError::Busy(format!(
                "{} is already queued ({}) at another commit; wait for it or cancel it",
                w.name(),
                j.job_id
            )));
        }
        if self.pipeline().task(&w.workspace_id).is_none() {
            // A workspace made without `task start` becomes a task on its first merge.
            let _ = self
                .adopt_task(AdoptTask {
                    path: w.cwd.clone(),
                    title: w.title.clone(),
                    owner: req.requested_by.clone(),
                    session: None,
                })
                .await;
        }
        let id = pipeline::new_id("job");
        let spec = Spec {
            job_id: id.clone(),
            kind: JobKind::Merge,
            repo: repo.root.display().to_string(),
            trunk: repo.branch.clone(),
            remote: repo.remote.clone(),
            work_root: self
                .pipeline()
                .config
                .work_root
                .join(&repo.name)
                .display()
                .to_string(),
            target_root: Some(self.pipeline().config.target_root.clone()),
            target_dir: self
                .pipeline()
                .config
                .target_root
                .join(format!("{}-landing", repo.name))
                .display()
                .to_string(),
            branch: Some(branch.clone()),
            head: Some(head),
            title: Some(w.name().to_string()),
            runtime: None,
            note: None,
            allow: req.allow.clone(),
            force: false,
            full: req.full,
            ledger: Some(self.ledger_path()),
            requested_by: req.requested_by.clone(),
        };
        let job = Job {
            job_id: id.clone(),
            seq: self.pipeline().next_seq(),
            kind: JobKind::Merge,
            state: JobState::Queued,
            workspace_id: Some(w.workspace_id.clone()),
            runtime: None,
            repo: spec.repo.clone(),
            branch: Some(branch),
            requested_by: req.requested_by,
            keep: req.keep,
            priority: false,
            spec,
            created_at: now_iso(),
            started_at: None,
            finished_at: None,
            runner: None,
            outcome: None,
            after: None,
        };
        self.pipeline()
            .save_job(&job)
            .map_err(ManagerError::Store)?;
        self.pipeline()
            .update_task(&w.workspace_id, |t| t.last_job = Some(id));
        self.pipeline().wake.notify_one();
        Ok(job)
    }

    /// `lagent deploy`: queue a deploy of the trunk's tip (resolved when it runs).
    pub async fn queue_deploy(&self, req: DeployRequest) -> Result<Job, ManagerError> {
        let runtime = req.runtime.trim().to_string();
        if runtime.is_empty()
            || !runtime
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(invalid(format!("Invalid runtime: {runtime}")));
        }
        let repo = repo_info(Path::new(&req.repo.clone().unwrap_or_else(default_repo)))?;
        if let Some(j) = self.pipeline().active_jobs().into_iter().find(|j| {
            j.kind == JobKind::Deploy
                && j.runtime.as_deref() == Some(runtime.as_str())
                && j.state == JobState::Queued
        }) {
            return Ok(j);
        }
        let id = pipeline::new_id("job");
        let spec = Spec {
            job_id: id.clone(),
            kind: JobKind::Deploy,
            repo: repo.root.display().to_string(),
            trunk: repo.branch.clone(),
            remote: repo.remote.clone(),
            work_root: self
                .pipeline()
                .config
                .work_root
                .join(&repo.name)
                .display()
                .to_string(),
            target_root: Some(self.pipeline().config.target_root.clone()),
            target_dir: self
                .pipeline()
                .config
                .target_root
                .join(format!("{}-deploy", repo.name))
                .display()
                .to_string(),
            branch: None,
            head: None,
            title: None,
            runtime: Some(runtime.clone()),
            note: req.note,
            allow: Vec::new(),
            force: req.force,
            full: false,
            ledger: Some(self.ledger_path()),
            requested_by: None,
        };
        let job = Job {
            job_id: id,
            seq: self.pipeline().next_seq(),
            kind: JobKind::Deploy,
            state: JobState::Queued,
            workspace_id: None,
            runtime: Some(runtime),
            repo: spec.repo.clone(),
            branch: None,
            requested_by: req.requested_by,
            keep: false,
            priority: false,
            spec,
            created_at: now_iso(),
            started_at: None,
            finished_at: None,
            runner: None,
            outcome: None,
            after: None,
        };
        self.pipeline()
            .save_job(&job)
            .map_err(ManagerError::Store)?;
        self.pipeline().wake.notify_one();
        Ok(job)
    }

    pub fn job(&self, id: &str) -> Result<Job, ManagerError> {
        self.pipeline()
            .job(id)
            .ok_or_else(|| ManagerError::NotFound(format!("Job not found: {id}")))
    }

    /// A job's log from byte `offset`.
    pub fn job_log(&self, id: &str, offset: u64) -> Result<(u64, String), ManagerError> {
        self.job(id)?;
        let bytes =
            std::fs::read(self.pipeline().config.job_dir(id).join("log")).unwrap_or_default();
        let start = (offset as usize).min(bytes.len());
        Ok((
            bytes.len() as u64,
            String::from_utf8_lossy(&bytes[start..]).to_string(),
        ))
    }

    pub async fn cancel_job(&self, id: &str) -> Result<Job, ManagerError> {
        let job = self.job(id)?;
        if job.state.done() {
            return Ok(job);
        }
        if job.state == JobState::Running {
            if let Some(r) = &job.runner {
                stop_runner(r).await;
            }
        }
        let job = self
            .pipeline()
            .update_job(id, |j| {
                j.state = JobState::Cancelled;
                j.finished_at = Some(now_iso());
                j.outcome = Some(Outcome {
                    ok: false,
                    code: "CANCELLED".into(),
                    message: "cancelled".into(),
                    ..Outcome::default()
                });
            })
            .ok_or_else(|| ManagerError::NotFound(format!("Job not found: {id}")))?;
        self.pipeline().wake.notify_one();
        Ok(job)
    }

    /// `lagent job next`: this queued job runs before the others of its repo.
    pub fn prioritize_job(&self, id: &str) -> Result<Job, ManagerError> {
        if self.job(id)?.state != JobState::Queued {
            return Err(invalid(format!("{id} is not queued")));
        }
        let job = self
            .pipeline()
            .update_job(id, |j| j.priority = true)
            .ok_or_else(|| ManagerError::NotFound(format!("Job not found: {id}")))?;
        self.pipeline().wake.notify_one();
        Ok(job)
    }

    /// Wait until the job is done or `timeout` passes.
    pub async fn wait_job(&self, id: &str, timeout: Duration) -> Result<Job, ManagerError> {
        let until = tokio::time::Instant::now() + timeout;
        loop {
            let j = self.job(id)?;
            if j.state.done() || tokio::time::Instant::now() >= until {
                return Ok(j);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// The queue worker (the daemon starts it).
    pub fn spawn_pipeline(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                manager.pipeline_tick().await;
                let wake = manager.pipeline().wake.clone();
                tokio::select! {
                    _ = wake.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                }
            }
        });
    }

    /// Finish the running jobs whose executor is done, then start the next queued job of
    /// each repo with nothing running: per repo, one job at a time, `lagent job next` first.
    pub async fn pipeline_tick(&self) {
        let active = self.pipeline().active_jobs();
        let mut busy = HashSet::new();
        for job in active.iter().filter(|j| j.state == JobState::Running) {
            if !self.finish_if_done(job).await {
                busy.insert(job.repo.clone());
            }
        }
        let mut queued: Vec<&Job> = active
            .iter()
            .filter(|j| j.state == JobState::Queued)
            .collect();
        queued.sort_by_key(|j| (!j.priority, j.seq));
        for job in queued {
            if busy.insert(job.repo.clone()) {
                self.launch_job(job).await;
            }
        }
    }

    /// Whether the job's executor is done (then the job is finished).
    async fn finish_if_done(&self, job: &Job) -> bool {
        let dir = self.pipeline().config.job_dir(&job.job_id);
        let result = std::fs::read_to_string(dir.join("result.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Outcome>(&s).ok());
        match result {
            Some(outcome) => self.finish_job(job, outcome).await,
            None if !runner_alive(job.runner.as_deref()).await => {
                // It may have written the result just now.
                tokio::time::sleep(Duration::from_millis(300)).await;
                let outcome = std::fs::read_to_string(dir.join("result.json"))
                    .ok()
                    .and_then(|s| serde_json::from_str::<Outcome>(&s).ok())
                    .unwrap_or_else(|| Outcome {
                        ok: false,
                        code: "EXECUTOR_DIED".into(),
                        message: format!(
                            "the job's process ended without a result (lagent job logs {})",
                            job.job_id
                        ),
                        ..Outcome::default()
                    });
                self.finish_job(job, outcome).await;
            }
            None => return false,
        }
        true
    }

    async fn launch_job(&self, job: &Job) {
        let cfg = &self.pipeline().config;
        let dir = cfg.job_dir(&job.job_id);
        let lock = cfg.lock_for(&job.repo, job.kind);
        let written = std::fs::create_dir_all(&dir).and_then(|_| {
            std::fs::write(
                dir.join("job.json"),
                serde_json::to_vec_pretty(&job.spec).unwrap_or_default(),
            )
        });
        let _ = std::fs::write(
            dir.join("log"),
            format!(
                "[{}] {} job {} started; waiting for {}\n",
                chrono::Utc::now().format("%H:%M:%S"),
                if job.kind == JobKind::Merge {
                    "merge"
                } else {
                    "deploy"
                },
                job.job_id,
                lock.display()
            ),
        );
        let runner = match written {
            Ok(()) => start_runner(cfg, &job.job_id, &dir, &lock).await,
            Err(e) => Err(format!("cannot write the job: {e}")),
        };
        match runner {
            Ok(r) => {
                tracing::info!(job = %job.job_id, runner = %r, "pipeline job started");
                self.pipeline().update_job(&job.job_id, |j| {
                    j.state = JobState::Running;
                    j.started_at = Some(now_iso());
                    j.runner = Some(r);
                });
            }
            Err(e) => {
                tracing::warn!(job = %job.job_id, error = %e, "pipeline job did not start");
                let outcome = Outcome {
                    ok: false,
                    code: "START_FAILED".into(),
                    message: e,
                    ..Outcome::default()
                };
                self.finish_job(job, outcome).await;
            }
        }
    }

    async fn finish_job(&self, job: &Job, outcome: Outcome) {
        let mut after = None;
        if job.kind == JobKind::Merge && outcome.ok {
            if let (Some(ws), Some(landed)) = (&job.workspace_id, &outcome.landed) {
                after = Some(self.after_merge(job, ws, landed).await);
            }
        }
        if job.kind == JobKind::Merge && outcome.ok && outcome.code != "NOTHING_TO_MERGE" {
            if let (Some(ws), Some(landed)) = (&job.workspace_id, &outcome.landed) {
                self.note_flaw_merge(ws, landed).await;
            }
        }
        if job.kind == JobKind::Deploy && outcome.code == "DEPLOYED" {
            self.note_flaw_deploy(job, &outcome).await;
        }
        if job.kind == JobKind::Deploy
            && matches!(
                outcome.code.as_str(),
                "INSTALL_FAILED" | "UNHEALTHY" | "RECORD_FAILED"
            )
        {
            if let Some(n) = &self.inner.options.notifier {
                n.send(crate::notify::Push {
                    category: "agent",
                    title: Some(format!(
                        "Deploy of {} failed",
                        job.runtime.clone().unwrap_or_default()
                    )),
                    body: outcome.message.clone(),
                    route: "/agents".into(),
                    dedup_key: format!("pipeline-{}", job.job_id),
                    data: json!({"jobId": job.job_id}),
                });
            }
        }
        let state = if outcome.ok {
            JobState::Succeeded
        } else {
            JobState::Failed
        };
        tracing::info!(job = %job.job_id, code = %outcome.code, "pipeline job finished");
        self.pipeline().update_job(&job.job_id, |j| {
            j.state = state;
            j.finished_at = Some(now_iso());
            j.outcome = Some(outcome);
            j.after = after;
        });
        self.bump();
        self.pipeline().wake.notify_one();
    }

    /// A flaw task landed: its commit goes on the flaw. A failure here never fails the merge.
    async fn note_flaw_merge(&self, ws: &str, landed: &str) {
        let Some(t) = self.pipeline().task(ws) else {
            return;
        };
        let (Some(flaw), Some(target)) = (&t.flaw, &t.flaw_target) else {
            return;
        };
        lucina_flaws_cli(&["flaws", "merged", flaw, target, landed]).await;
    }

    /// A deploy: every flaw fix on this repo that the deployed commit holds gets the new tag
    /// (`lucina-rs` keeps only runtimes that carry that target's fix).
    async fn note_flaw_deploy(&self, job: &Job, outcome: &Outcome) {
        let (Some(runtime), Some(landed)) = (&job.runtime, &outcome.landed) else {
            return;
        };
        let repo = Path::new(&job.repo);
        let pattern = format!("deploy/{runtime}/*");
        let Some(tag) = git::read(
            repo,
            &[
                "tag",
                "--list",
                &pattern,
                "--points-at",
                landed,
                "--sort=-creatordate",
            ],
        )
        .await
        .and_then(|o| o.lines().next().map(str::to_owned)) else {
            tracing::warn!(runtime = %runtime, "no deploy tag at the deployed commit");
            return;
        };
        for t in self
            .pipeline()
            .tasks()
            .into_iter()
            .filter(|t| t.repo == job.repo)
        {
            let (Some(flaw), Some(target), Some(sha)) = (&t.flaw, &t.flaw_target, &t.merged_sha)
            else {
                continue;
            };
            if git::must(repo, &["merge-base", "--is-ancestor", sha, landed])
                .await
                .is_ok()
            {
                lucina_flaws_cli(&["flaws", "deployed", flaw, target, runtime, &tag]).await;
            }
        }
    }

    /// After a merge: move the workspace's branch to the merged commit, then archive the
    /// workspace, or keep it starting from there. Returns what was done.
    async fn after_merge(&self, job: &Job, ws: &str, landed: &str) -> String {
        let now = now_iso();
        self.pipeline().update_task(ws, |t| {
            t.merged_sha = Some(landed.to_string());
            t.merged_at = Some(now.clone());
        });
        let Some(w) = self
            .workspaces()
            .get(ws)
            .filter(|w| w.archived_at.is_none())
        else {
            return "the workspace was already archived".into();
        };
        let dir = PathBuf::from(&w.cwd);
        let head = git::read(&dir, &["rev-parse", "HEAD"]).await;
        if head.as_deref() != job.spec.head.as_deref() || git::is_dirty(&dir).await {
            return "kept: the workspace changed after the merged commit; merge again for the rest"
                .into();
        }
        if let Err(e) = git::must(&dir, &["reset", "-q", "--keep", landed]).await {
            return format!("kept: could not move the branch to the merged commit: {e}");
        }
        if job.keep {
            self.workspaces().restart_at(ws, landed);
            return format!(
                "kept: the workspace now starts at {}",
                &landed[..landed.len().min(10)]
            );
        }
        match self
            .archive_workspace(
                ws,
                Trigger::Merged,
                Some(format!("merged into {} by {}", job.spec.trunk, job.job_id)),
            )
            .await
        {
            Ok(w) => match w.removed_reason {
                Some(r) if r.starts_with("worktree kept") => format!("archived; {r}"),
                _ => "archived: worktree and branch removed".into(),
            },
            Err(e) => format!("kept: archive failed: {e}"),
        }
    }

    pub fn leases(&self) -> Vec<Value> {
        let leases = self.pipeline().leases();
        pipeline::DEVICES
            .iter()
            .map(|(id, about)| {
                let l = leases.iter().find(|l| l.device == *id && !expired(&l.expires_at));
                json!({"device": id, "about": about, "lease": l, "installed": self.devices().installed(id)})
            })
            .collect()
    }

    pub fn lease_device(&self, device: &str, req: LeaseRequest) -> Result<Lease, ManagerError> {
        if !pipeline::DEVICES.iter().any(|(d, _)| *d == device) {
            return Err(ManagerError::NotFound(format!(
                "Unknown device: {device} (devices: {})",
                pipeline::DEVICES
                    .iter()
                    .map(|d| d.0)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if req.holder.trim().is_empty() {
            return Err(invalid("A lease needs a holder (the session id)"));
        }
        if let Some(l) = self
            .pipeline()
            .lease(device)
            .filter(|l| !expired(&l.expires_at))
        {
            if l.holder != req.holder {
                return Err(ManagerError::Busy(format!(
                    "{device} is leased to {} until {} ({})",
                    l.holder, l.expires_at, l.purpose
                )));
            }
        }
        let minutes = req
            .minutes
            .unwrap_or(LEASE_DEFAULT_MIN)
            .clamp(1, LEASE_MAX_MIN);
        let prior = self
            .pipeline()
            .lease(device)
            .filter(|l| l.holder == req.holder && !expired(&l.expires_at));
        let lease = Lease {
            device: device.to_string(),
            holder: req.holder.clone(),
            purpose: req
                .purpose
                .clone()
                .or(prior.as_ref().map(|p| p.purpose.clone()))
                .unwrap_or_default(),
            acquired_at: prior
                .as_ref()
                .map(|p| p.acquired_at.clone())
                .unwrap_or_else(now_iso),
            expires_at: minutes_from_now(minutes),
            workspace_id: req
                .workspace_id
                .clone()
                .or(prior.and_then(|p| p.workspace_id)),
        };
        self.pipeline()
            .save_lease(&lease)
            .map_err(ManagerError::Store)?;
        Ok(lease)
    }

    pub fn release_device(&self, device: &str, req: ReleaseRequest) -> Result<Value, ManagerError> {
        let Some(l) = self.pipeline().lease(device) else {
            return Ok(json!({"device": device, "released": false}));
        };
        if l.holder != req.holder && !expired(&l.expires_at) {
            if !req.force || req.reason.as_deref().unwrap_or("").trim().is_empty() {
                return Err(ManagerError::Forbidden(format!(
                    "{device} is leased to {}; --force --reason releases it anyway",
                    l.holder
                )));
            }
            tracing::warn!(device, holder = %l.holder, by = %req.holder, reason = ?req.reason, "lease released by force");
        }
        self.pipeline()
            .drop_lease(device)
            .map_err(ManagerError::Store)?;
        Ok(json!({"device": device, "released": true, "holder": l.holder}))
    }

    /// One view of everything: workspaces with their pipeline state, the queue, recent
    /// jobs, leases and recent deploys.
    pub async fn board(&self) -> Value {
        let jobs = self.pipeline().jobs(40);
        let tasks = self.pipeline().tasks();
        let live_sessions = self.live_sessions_by_workspace();
        let mut rows = Vec::new();
        let mut repos: Vec<(PathBuf, String)> = Vec::new();
        let worktrees: Vec<WorkspaceRecord> = self
            .workspaces()
            .list(true)
            .into_iter()
            .filter(|w| w.kind == WorkspaceKind::Worktree)
            .collect();
        let usage = procs::usage(
            &worktrees
                .iter()
                .map(|w| PathBuf::from(&w.cwd))
                .collect::<Vec<_>>(),
        );
        for (w, used) in worktrees.iter().zip(&usage) {
            let dir = PathBuf::from(&w.cwd);
            let task = tasks.iter().find(|t| t.workspace_id == w.workspace_id);
            let active = jobs.iter().find(|j| {
                j.workspace_id.as_deref() == Some(w.workspace_id.as_str()) && !j.state.done()
            });
            let last = jobs
                .iter()
                .find(|j| j.workspace_id.as_deref() == Some(w.workspace_id.as_str()));
            let dirty = git::is_dirty(&dir).await;
            // With no stored base (a worktree made by hand), the repo's own trunk; a directory
            // that is not a repo has none, and `ahead` stays unknown as before.
            let base = w.base_ref.clone().unwrap_or_else(|| {
                repo_info(&dir)
                    .map(|r| r.base_ref())
                    .unwrap_or_else(|_| "origin/main".into())
            });
            let ahead = git::ahead_behind(&dir, &base).await;
            let idle = idle_secs(&dir).await;
            let underway = git::in_progress(&dir).await;
            let detached = git::info(&dir).await.branch.is_none();
            let merged = ahead.is_some_and(|(a, _)| a == 0)
                && w.base_sha.as_deref()
                    != git::read(&dir, &["rev-parse", "HEAD"]).await.as_deref();
            let state = match active.map(|j| j.state) {
                Some(JobState::Queued) => "merge_queued",
                Some(JobState::Running) => "merging",
                // HEAD says nothing about the branch while these last.
                _ if underway.is_some() => underway.unwrap_or_default(),
                _ if detached => "detached",
                _ if merged => "merged",
                _ if idle.is_some_and(|s| s > STALE_DAYS * 86400) => "stale",
                _ => "open",
            };
            if let Some(root) = &w.main_repo_root {
                if !repos.iter().any(|(r, _)| r == Path::new(root)) {
                    repos.push((PathBuf::from(root), w.project_id.clone()));
                }
            }
            rows.push(json!({
                "workspaceId": w.workspace_id,
                "name": w.name(),
                "project": self.workspaces().project_name(w),
                "branch": w.branch,
                "cwd": w.cwd,
                "state": state,
                "dirty": dirty,
                "ahead": ahead.map(|a| a.0),
                "behind": ahead.map(|a| a.1),
                "idleDays": idle.map(|s| s / 86400),
                "procs": used.procs,
                "rssMb": used.rss_bytes / (1024 * 1024),
                "commands": used.commands,
                "task": task,
                // Whoever is working there now, else who took the task.
                "session": live_sessions.get(&w.workspace_id).cloned().or_else(|| task.and_then(|t| t.session.clone())),
                "lastJob": last.map(|j| json!({"jobId": j.job_id, "state": j.state, "code": j.outcome.as_ref().map(|o| o.code.clone())})),
            }));
        }
        // Worktrees nobody registered (made by hand): adopt or archive them.
        let mut unmanaged = Vec::new();
        let known: Vec<String> = self
            .workspaces()
            .list(true)
            .iter()
            .map(|w| w.cwd.clone())
            .collect();
        for (root, _) in &repos {
            for (path, branch) in git::worktrees(root).await {
                let p = path.display().to_string();
                // Dot directories are the pipeline's and deploys' own detached trees.
                let hidden = path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'));
                if path == *root || known.contains(&p) || hidden {
                    continue;
                }
                unmanaged.push(json!({"path": p, "branch": branch, "dirty": git::is_dirty(&path).await, "idleDays": idle_secs(&path).await.map(|s| s / 86400)}));
            }
        }
        // Main checkouts are not for working in: uncommitted files there block merges into a
        // local trunk, and two sessions there overwrite each other.
        let roots: Vec<PathBuf> = repos.iter().map(|(r, _)| r.clone()).collect();
        let root_usage = procs::usage(&roots);
        let mut main_checkouts = Vec::new();
        for (root, used) in roots.iter().zip(&root_usage) {
            // Not trimmed: each line starts with two status columns, the first often a space.
            let status = git::must(root, &["status", "--porcelain", "--untracked-files=normal"])
                .await
                .unwrap_or_default();
            let files: Vec<&str> = status.lines().map(|l| l.get(3..).unwrap_or(l)).collect();
            if files.is_empty() && used.procs == 0 {
                continue;
            }
            main_checkouts.push(json!({
                "path": root.display().to_string(),
                "dirtyFiles": files.len(),
                "sample": files.iter().take(8).collect::<Vec<_>>(),
                "procs": used.procs,
                "rssMb": used.rss_bytes / (1024 * 1024),
                "commands": used.commands,
            }));
        }
        let deploys = recent_deploys(12);
        json!({
            "workspaces": rows,
            "mainCheckouts": main_checkouts,
            "unmanaged": unmanaged,
            "queue": jobs.iter().filter(|j| !j.state.done()).collect::<Vec<_>>(),
            "recentJobs": jobs.iter().filter(|j| j.state.done()).take(15).collect::<Vec<_>>(),
            "leases": self.leases(),
            "deploys": deploys,
        })
    }
}

/// `lucina-rs flaws …` (`LUCINA_AGENTD_LUCINA_RS`, default `~/.local/bin/lucina-rs`): the tracker's
/// database belongs to Lucina, and agentd doesn't link it.
async fn lucina_flaws_cli(args: &[&str]) {
    let bin = std::env::var_os("LUCINA_AGENTD_LUCINA_RS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/bin/lucina-rs")
        });
    let run = tokio::process::Command::new(&bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(Duration::from_secs(20), run).await {
        Ok(Ok(o)) if o.status.success() => {}
        Ok(Ok(o)) => {
            tracing::warn!(?args, stderr = %String::from_utf8_lossy(&o.stderr), "flaw not updated")
        }
        Ok(Err(e)) => tracing::warn!(?args, error = %e, "flaw not updated"),
        Err(_) => tracing::warn!(?args, "flaw not updated: timed out"),
    }
}

/// The last `n` entries of deploy-guard's history.
fn recent_deploys(n: usize) -> Vec<Value> {
    let home = std::env::var("HOME").unwrap_or_default();
    let dir = std::env::var("DEPLOY_GUARD_LEDGER")
        .unwrap_or_else(|_| format!("{home}/.local/state/deploy-guard"));
    let text = std::fs::read_to_string(Path::new(&dir).join("history.jsonl")).unwrap_or_default();
    let mut v: Vec<Value> = text
        .lines()
        .rev()
        .take(n)
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    for d in &mut v {
        if let Some(note) = d["note"].as_str() {
            if note.len() > 200 {
                d["note"] = json!(format!("{}…", note.chars().take(200).collect::<String>()));
            }
        }
    }
    v
}

/// Start the executor under the merge lock: a transient systemd unit (it outlives agentd
/// restarts and agentd's own deploys), else a child process. Returns the runner.
async fn start_runner(
    cfg: &pipeline::Config,
    id: &str,
    dir: &Path,
    lock: &Path,
) -> Result<String, String> {
    let lagent = cfg.lagent.display().to_string();
    let path = std::env::var("PATH").unwrap_or_default();
    if cfg.systemd {
        let unit = format!("lagent-job-{}", id.trim_start_matches("job_"));
        let out = tokio::process::Command::new("systemd-run")
            .args(["--user", "--collect", "--quiet", "--unit", &unit])
            .arg(format!("--working-directory={}", dir.display()))
            .arg(format!("--setenv=PATH={path}"))
            .arg("--property=TimeoutStopSec=30")
            .arg("--")
            .arg("flock")
            .arg(lock)
            .arg(&lagent)
            .arg("pipeline-exec")
            .arg(dir)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("systemd-run: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "systemd-run: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        return Ok(unit);
    }
    let mut child = tokio::process::Command::new("flock")
        .arg(lock)
        .arg(&lagent)
        .arg("pipeline-exec")
        .arg(dir)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start {lagent}: {e}"))?;
    let pid = child.id().unwrap_or_default();
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(format!("pid:{pid}"))
}

async fn runner_alive(runner: Option<&str>) -> bool {
    match runner {
        Some(r) if r.starts_with("pid:") => {
            Path::new(&format!("/proc/{}", r.trim_start_matches("pid:"))).exists()
        }
        Some(unit) => tokio::process::Command::new("systemctl")
            .args(["--user", "is-active", "--quiet", unit])
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false),
        None => false,
    }
}

async fn stop_runner(runner: &str) {
    if let Some(pid) = runner.strip_prefix("pid:") {
        let _ = tokio::process::Command::new("kill").arg(pid).status().await;
    } else {
        let _ = tokio::process::Command::new("systemctl")
            .args(["--user", "stop", runner])
            .status()
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full commit id, as `rev-parse --verify --quiet <rev>^{commit}` answers it.
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    /// The check asks nothing of git: it is the two ancestor questions, answered.
    fn verdict(
        rev: &str,
        allowed: &[&str],
        commit: Option<&str>,
        on_trunk: bool,
        on_allowed: bool,
    ) -> Result<String, String> {
        base_decision(
            rev,
            "LucinaNet-servo",
            "lucinanet",
            allowed,
            commit,
            on_trunk,
            on_allowed,
        )
    }

    #[test]
    fn a_base_on_the_trunk_is_accepted() {
        assert_eq!(
            verdict("v1.2.3", &[], Some(COMMIT), true, false),
            Ok(COMMIT.into())
        );
    }

    #[test]
    fn a_base_on_an_allowed_base_is_accepted() {
        assert_eq!(
            verdict(
                COMMIT,
                &["upstream-56", "servo-53.1a"],
                Some(COMMIT),
                false,
                true
            ),
            Ok(COMMIT.into())
        );
    }

    /// The refusal says which commit the base resolved to, what it was measured against, and
    /// what else this repo's row would have allowed.
    #[test]
    fn a_base_on_neither_the_trunk_nor_an_allowed_base_is_refused() {
        assert_eq!(
            verdict("side-work", &[], Some(COMMIT), false, false).unwrap_err(),
            "base 0123456789 is not on lucinanet nor on an allowed base of LucinaNet-servo; allowed: none"
        );
        assert_eq!(
            verdict("side-work", &["upstream-56", "servo-53.1a"], Some(COMMIT), false, false)
                .unwrap_err(),
            "base 0123456789 is not on lucinanet nor on an allowed base of LucinaNet-servo; allowed: upstream-56, servo-53.1a"
        );
    }

    /// A base that names no commit here is refused before anything is measured, in the caller's
    /// own words.
    #[test]
    fn a_base_that_is_not_a_commit_is_refused() {
        assert_eq!(
            verdict("not-a-rev", &["upstream-56"], None, false, false).unwrap_err(),
            "base not-a-rev is not a commit in LucinaNet-servo"
        );
    }
}
