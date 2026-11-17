//! Work jobs from the manager's side: one delegated worker run that belongs to a lead.
//! agentd starts it with the installed `lane` binary (`lane run`, the spec on stdin) and
//! follows it with `lane status`; it never reimplements `lane` and never changes it.
//!
//! Jobs live in `<state dir>/work/jobs.json` (written through a temp file and a rename) and
//! the specs in `<state dir>/work/<id>/spec.md`. A poller reads `lane status` for every
//! unfinished job, so a daemon restart loses nothing. The pure rules are in
//! [`lucina_agents::work`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;

use lucina_agents::work::{
    apply_status, from_direct, is_empty, new_id, summarize, validate_target, CostClass,
    LeadSummary, Priority, WorkJob, WorkState,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;

use super::{Manager, ManagerError};
use crate::lane::{is_forbidden, LaneConfig};
use crate::now_iso;

/// How long `lane run` may take to detach.
const RUN_LIMIT: Duration = Duration::from_secs(60);
/// How long `lane status`, `lane ls` and `git rev-parse` may take.
const QUERY_LIMIT: Duration = Duration::from_secs(20);
/// Polls in a row that `lane status` must fail before a job can be lost.
const LOST_MISSES: u32 = 3;
const DAY: i64 = 24 * 3600;
/// The variables `lane` gets from the daemon's own environment, besides `LC_*`.
const LANE_ENV: &[&str] = &[
    "HOME",
    "PATH",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "TERM",
    "TMPDIR",
    "XDG_RUNTIME_DIR",
    "LANE_CONFIG",
];

/// Where work jobs are kept and how they are followed.
#[derive(Debug, Clone)]
pub struct WorkConfig {
    /// `<state dir>/work`.
    pub dir: PathBuf,
    /// `LUCINA_AGENTD_LANE` (default `~/.local/bin/lane`; `off` turns work jobs off).
    pub lane: Option<PathBuf>,
    /// `LUCINA_AGENTD_WORK_POLL_SECS` (default 20).
    pub poll: Duration,
    /// `LUCINA_AGENTD_WORK_LOST_AFTER_SECS` (default 120): a job older than this that `lane` does not know is lost.
    pub lost_after: Duration,
}

impl WorkConfig {
    pub fn from_env(state_dir: &Path) -> Self {
        let lane = match std::env::var("LUCINA_AGENTD_LANE") {
            Ok(v) if v == "off" => None,
            Ok(v) if !v.is_empty() => crate::process::resolve_program(&v),
            _ => crate::process::resolve_program("~/.local/bin/lane"),
        };
        let secs = |name: &str, default: u64| {
            std::env::var(name)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(default)
        };
        WorkConfig {
            dir: state_dir.join("work"),
            lane,
            poll: Duration::from_secs(secs("LUCINA_AGENTD_WORK_POLL_SECS", 20).max(1)),
            lost_after: Duration::from_secs(secs("LUCINA_AGENTD_WORK_LOST_AFTER_SECS", 120)),
        }
    }
}

impl Default for WorkConfig {
    fn default() -> Self {
        Self::from_env(&std::env::temp_dir().join("lucina-agentd"))
    }
}

/// The stored work jobs.
pub(super) struct Work {
    cfg: WorkConfig,
    jobs: Mutex<Vec<WorkJob>>,
    /// Polls in a row where `lane status` exited non-zero, per job (not kept over a restart).
    misses: Mutex<HashMap<String, u32>>,
    /// Jobs whose `lane run` has not returned yet: the poller leaves them alone.
    launching: Mutex<HashSet<String>>,
}

impl Work {
    pub(super) fn open(cfg: WorkConfig) -> Self {
        let jobs = load(&cfg.dir);
        Work {
            cfg,
            jobs: Mutex::new(jobs),
            misses: Mutex::new(HashMap::new()),
            launching: Mutex::new(HashSet::new()),
        }
    }

    fn jobs(&self) -> std::sync::MutexGuard<'_, Vec<WorkJob>> {
        self.jobs.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Writes `jobs.json` through a temp file in the same directory and a rename.
    fn persist(&self, jobs: &[WorkJob]) {
        if let Err(e) = write_jobs(&self.cfg.dir, jobs) {
            tracing::error!(error = %e, dir = %self.cfg.dir.display(), "cannot write work jobs");
        }
    }
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The stored jobs: none when the file is missing; an unreadable file is moved aside, never overwritten.
fn load(dir: &Path) -> Vec<WorkJob> {
    let file = dir.join("jobs.json");
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::error!(error = %e, file = %file.display(), "cannot read work jobs");
            move_aside(&file);
            return Vec::new();
        }
    };
    match serde_json::from_str::<Vec<WorkJob>>(&text) {
        Ok(jobs) => jobs,
        Err(e) => {
            tracing::error!(error = %e, file = %file.display(), "work jobs file is unreadable; moved aside");
            move_aside(&file);
            Vec::new()
        }
    }
}

fn move_aside(file: &Path) {
    let aside = file.with_file_name(format!("jobs.json.corrupt-{}", unix_secs()));
    if let Err(e) = std::fs::rename(file, &aside) {
        tracing::error!(error = %e, "cannot move the work jobs file aside");
    }
}

fn write_jobs(dir: &Path, jobs: &[WorkJob]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("jobs.json.tmp-{}", std::process::id()));
    let body = serde_json::to_vec_pretty(jobs).map_err(std::io::Error::other)?;
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, dir.join("jobs.json")).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// The variables a `lane` child gets: a fixed list and every `LC_*`.
fn lane_env(vars: impl Iterator<Item = (String, String)>) -> Vec<(String, String)> {
    vars.filter(|(k, _)| LANE_ENV.contains(&k.as_str()) || k.starts_with("LC_"))
        .collect()
}

fn lane_command(bin: &Path) -> Command {
    let mut c = Command::new(bin);
    c.env_clear()
        .envs(lane_env(std::env::vars()))
        .kill_on_drop(true);
    c
}

/// The last `n` characters.
fn tail_chars(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}

fn age_secs(rfc3339: &str) -> Option<i64> {
    let then = chrono::DateTime::parse_from_rfc3339(rfc3339).ok()?;
    Some((chrono::Utc::now() - then.with_timezone(&chrono::Utc)).num_seconds())
}

/// Whether a stored or direct job still belongs in the default list.
fn recent(state: WorkState, stamp: Option<&str>) -> bool {
    !state.is_terminal() || stamp.and_then(age_secs).is_none_or(|a| a <= DAY)
}

fn lane_not_found(kind: &str, name: &str) -> ManagerError {
    ManagerError::Invalid(format!("no {kind} \"{name}\" in the lane config"))
}

/// The lane and chain checks of a start, on a loaded config. `lucina`: the workspace is a Lucina project.
fn check_target(
    lanes: &LaneConfig,
    lane: Option<&str>,
    chain: Option<&str>,
    lucina: bool,
) -> Result<(), ManagerError> {
    const NO_CODEX: &str = "no Codex on Lucina work: use the -lucina chain";
    let is_codex = |name: &str| lanes.lanes.get(name).is_some_and(|l| l.kind == "codex");
    if let Some(name) = lane {
        let l = lanes
            .lanes
            .get(name)
            .ok_or_else(|| lane_not_found("lane", name))?;
        if is_forbidden(name, l) {
            return Err(ManagerError::Invalid(format!(
                "lane \"{name}\" is not allowed (the paid DeepSeek key is never used)"
            )));
        }
        if lucina && is_codex(name) {
            return Err(ManagerError::CodexNotAllowed(NO_CODEX.into()));
        }
    }
    if let Some(name) = chain {
        let members = lanes
            .categories
            .get(name)
            .ok_or_else(|| lane_not_found("chain", name))?;
        if lucina && members.iter().any(|m| is_codex(m)) {
            return Err(ManagerError::CodexNotAllowed(NO_CODEX.into()));
        }
    }
    Ok(())
}

/// `expect` must stay inside the workspace.
fn check_expect(expect: &str) -> Result<(), ManagerError> {
    use std::path::Component;
    let p = Path::new(expect);
    if expect.trim().is_empty()
        || p.is_absolute()
        || p.components().any(|c| matches!(c, Component::ParentDir))
    {
        return Err(ManagerError::Invalid(
            "expect must be a path inside the workspace (relative, no \"..\")".into(),
        ));
    }
    Ok(())
}

/// What `POST /work` takes.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartWork {
    #[serde(default)]
    pub lead: String,
    #[serde(default)]
    pub priority: Option<Priority>,
    #[serde(default)]
    pub cost: Option<CostClass>,
    #[serde(default)]
    pub lane: Option<String>,
    #[serde(default)]
    pub chain: Option<String>,
    #[serde(default)]
    pub workspace: String,
    #[serde(default)]
    pub verify: Option<String>,
    #[serde(default)]
    pub expect: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub spec: String,
}

/// What `GET /work` answers.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkList {
    pub jobs: Vec<WorkJob>,
    pub leads: Vec<LeadSummary>,
    pub warning: Option<String>,
}

fn nonblank(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Sets the end of a job: when it ended and whether it produced nothing.
fn finish(job: &mut WorkJob) {
    job.ended_at = Some(now_iso());
    let expect_present = job.expect.as_ref().map(|e| {
        std::fs::metadata(Path::new(&job.workspace).join(e))
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false)
    });
    job.empty = Some(is_empty(
        expect_present,
        job.base_head.as_deref(),
        job.after_head.as_deref(),
        job.verify_head.as_deref(),
        job.dirty_files,
    ));
}

impl Manager {
    fn work(&self) -> &Work {
        &self.inner.work
    }

    fn work_lane(&self) -> Result<&Path, ManagerError> {
        self.work().cfg.lane.as_deref().ok_or_else(|| {
            ManagerError::Invalid(
                "work jobs are off: LUCINA_AGENTD_LANE is \"off\" or the lane binary was not found"
                    .into(),
            )
        })
    }

    /// Starts a work job: stores the spec, then `lane run` with it on stdin.
    pub async fn work_start(&self, req: StartWork) -> Result<WorkJob, ManagerError> {
        let lane_bin = self.work_lane()?.to_path_buf();
        let lead = req.lead.trim().to_string();
        if lead.is_empty() {
            return Err(ManagerError::Invalid("lead is required".into()));
        }
        if req.spec.trim().is_empty() {
            return Err(ManagerError::Invalid("spec is required".into()));
        }
        let lane = nonblank(req.lane);
        let chain = nonblank(req.chain);
        validate_target(lane.as_deref(), chain.as_deref()).map_err(ManagerError::Invalid)?;
        let workspace = req.workspace.trim().to_string();
        if workspace.is_empty() || !Path::new(&workspace).is_dir() {
            return Err(ManagerError::Invalid(format!(
                "workspace \"{workspace}\" is not a directory"
            )));
        }
        let expect = nonblank(req.expect);
        if let Some(e) = &expect {
            check_expect(e)?;
        }
        let lanes = LaneConfig::load();
        let lucina = crate::workspaces::is_lucina_project(
            &self.inner.options.workspaces,
            Path::new(&workspace),
        );
        check_target(&lanes, lane.as_deref(), chain.as_deref(), lucina)?;

        let dir = self.work().cfg.dir.clone();
        let id = loop {
            let id = new_id(uuid::Uuid::new_v4().as_u128());
            if !self.work().jobs().iter().any(|j| j.id == id) && !dir.join(&id).exists() {
                break id;
            }
        };
        let run_dir = dir.join(&id);
        let spec_path = run_dir.join("spec.md");
        std::fs::create_dir_all(&run_dir)
            .map_err(|e| ManagerError::Invalid(format!("cannot store the spec: {e}")))?;
        std::fs::write(&spec_path, &req.spec)
            .map_err(|e| ManagerError::Invalid(format!("cannot store the spec: {e}")))?;

        let base_head = git_head(&workspace).await;
        let job = WorkJob {
            id: id.clone(),
            lead,
            priority: req.priority.unwrap_or_default(),
            cost: req.cost.unwrap_or_default(),
            lane: lane.clone(),
            chain: chain.clone(),
            workspace: workspace.clone(),
            verify: nonblank(req.verify),
            expect,
            title: nonblank(req.title),
            spec_path: spec_path.display().to_string(),
            created_at: now_iso(),
            base_head,
            state: WorkState::Starting,
            ..WorkJob::default()
        };
        self.work()
            .launching
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.clone());
        {
            let mut jobs = self.work().jobs();
            jobs.push(job.clone());
            self.work().persist(&jobs);
        }

        let failure = run_lane(&lane_bin, &job, &run_dir).await.err();
        let out = {
            let mut jobs = self.work().jobs();
            if let Some(j) = jobs.iter_mut().find(|j| j.id == id) {
                if let Some(note) = failure {
                    j.state = WorkState::Failed;
                    j.note = Some(note);
                    j.ended_at = Some(now_iso());
                }
            }
            self.work().persist(&jobs);
            jobs.iter().find(|j| j.id == id).cloned().unwrap_or(job)
        };
        self.work()
            .launching
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
        Ok(out)
    }

    /// The stored jobs, and with `include_direct` the runs started with `lane` directly.
    pub async fn work_list(
        &self,
        lead: Option<String>,
        include_direct: bool,
        include_old: bool,
    ) -> WorkList {
        let stored: Vec<WorkJob> = self.work().jobs().clone();
        let known: HashSet<String> = stored.iter().map(|j| j.id.clone()).collect();
        let mut jobs: Vec<WorkJob> = stored
            .into_iter()
            .filter(|j| lead.as_deref().is_none_or(|l| j.lead == l))
            .filter(|j| include_old || recent(j.state, j.ended_at.as_deref()))
            .collect();
        let mut warning = None;
        if include_direct {
            match self.lane_ls().await {
                Ok(rows) => {
                    for row in &rows {
                        let Some(job) = from_direct(row) else {
                            continue;
                        };
                        if known.contains(&job.id) || lead.as_deref().is_some_and(|l| job.lead != l)
                        {
                            continue;
                        }
                        if include_old
                            || recent(job.state, row.get("updated").and_then(Value::as_str))
                        {
                            jobs.push(job);
                        }
                    }
                }
                Err(w) => warning = Some(w),
            }
        }
        let leads = summarize(jobs);
        let jobs = leads.iter().flat_map(|l| l.jobs.iter().cloned()).collect();
        WorkList {
            jobs,
            leads,
            warning,
        }
    }

    /// `lane ls --json --limit 200`, as rows; the text of the failure when it fails.
    async fn lane_ls(&self) -> Result<Vec<Value>, String> {
        let bin = self.work_lane().map_err(|e| e.to_string())?;
        let mut cmd = lane_command(bin);
        cmd.args(["ls", "--json", "--limit", "200"])
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        let out = tokio::time::timeout(QUERY_LIMIT, cmd.output())
            .await
            .map_err(|_| "lane ls timed out".to_string())?
            .map_err(|e| format!("lane ls: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "lane ls exited {}",
                out.status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "on a signal".into())
            ));
        }
        match serde_json::from_slice::<Value>(&out.stdout) {
            Ok(Value::Array(rows)) => Ok(rows),
            Ok(_) => Err("lane ls did not print a list".into()),
            Err(e) => Err(format!("lane ls printed bad JSON: {e}")),
        }
    }

    pub fn work_get(&self, id: &str) -> Result<WorkJob, ManagerError> {
        self.work()
            .jobs()
            .iter()
            .find(|j| j.id == id)
            .cloned()
            .ok_or_else(|| ManagerError::WorkNotFound(id.to_string()))
    }

    /// Returns when the job is final or the time is up; checks once a second.
    pub async fn work_wait(&self, id: &str, timeout: Duration) -> Result<WorkJob, ManagerError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let job = self.work_get(id)?;
            if job.state.is_terminal() || tokio::time::Instant::now() >= deadline {
                return Ok(job);
            }
            let left = deadline - tokio::time::Instant::now();
            tokio::time::sleep(left.min(Duration::from_secs(1))).await;
        }
    }

    /// The poller: reads `lane status` for every unfinished job, one after another.
    pub fn spawn_work(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(manager.work().cfg.poll).await;
                manager.work_poll().await;
            }
        });
    }

    /// One pass over the unfinished jobs.
    pub async fn work_poll(&self) {
        let Some(bin) = self.work().cfg.lane.clone() else {
            return;
        };
        let open: Vec<String> = {
            let launching = self
                .work()
                .launching
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            self.work()
                .jobs()
                .iter()
                .filter(|j| !j.state.is_terminal() && !launching.contains(&j.id))
                .map(|j| j.id.clone())
                .collect()
        };
        for id in open {
            self.poll_one(&bin, &id).await;
        }
    }

    async fn poll_one(&self, bin: &Path, id: &str) {
        let mut cmd = lane_command(bin);
        cmd.args(["status", id])
            .stdin(Stdio::null())
            .stderr(Stdio::null());
        let out = match tokio::time::timeout(QUERY_LIMIT, cmd.output()).await {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => return tracing::warn!(job = id, error = %e, "lane status did not start"),
            Err(_) => return tracing::warn!(job = id, "lane status timed out"),
        };
        let Some(mut job) = self.work().jobs().iter().find(|j| j.id == id).cloned() else {
            return;
        };
        if job.state.is_terminal() {
            return;
        }
        let before = job.clone();
        if out.status.success() {
            self.work()
                .misses
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(id);
            match serde_json::from_slice::<Value>(&out.stdout) {
                Ok(status) => {
                    apply_status(&mut job, &status);
                }
                Err(e) => {
                    return tracing::warn!(job = id, error = %e, "lane status printed bad JSON")
                }
            }
        } else {
            let misses = {
                let mut m = self.work().misses.lock().unwrap_or_else(|p| p.into_inner());
                let n = m.entry(id.to_string()).or_insert(0);
                *n += 1;
                *n
            };
            let old_enough = age_secs(&job.created_at)
                .is_some_and(|a| a >= self.work().cfg.lost_after.as_secs() as i64);
            if misses >= LOST_MISSES && old_enough {
                job.state = WorkState::Lost;
                job.note = Some("lane has no record of this run".into());
                job.ended_at = Some(now_iso());
            }
        }
        if job.state.is_terminal() && job.ended_at.is_none() {
            finish(&mut job);
        }
        if job == before {
            return;
        }
        let mut jobs = self.work().jobs();
        // A start that failed in the meantime wins: only an unfinished job takes the update.
        if let Some(stored) = jobs
            .iter_mut()
            .find(|j| j.id == id && !j.state.is_terminal())
        {
            *stored = job;
            self.work().persist(&jobs);
        }
    }
}

/// `git rev-parse HEAD` in the workspace, `None` when it fails.
async fn git_head(workspace: &str) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(workspace)
        .args(["rev-parse", "HEAD"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(QUERY_LIMIT, cmd.output())
        .await
        .ok()?
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let head = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!head.is_empty()).then_some(head)
}

/// `lane run` with the spec on stdin. Its output goes to files in the run's directory so
/// that a detached run which keeps a pipe open cannot hold this call up. Err: the note.
async fn run_lane(bin: &Path, job: &WorkJob, run_dir: &Path) -> Result<(), String> {
    let open = |p: PathBuf| {
        std::fs::File::create(&p).map_err(|e| format!("cannot open {}: {e}", p.display()))
    };
    let stdin =
        std::fs::File::open(&job.spec_path).map_err(|e| format!("cannot read the spec: {e}"))?;
    let err_path = run_dir.join("run.err");
    let mut cmd = lane_command(bin);
    cmd.arg("run").arg("--id").arg(&job.id);
    match (&job.lane, &job.chain) {
        (Some(l), _) => cmd.arg("--lane").arg(l),
        (None, Some(c)) => cmd.arg("--category").arg(c),
        (None, None) => return Err("no lane or chain".into()),
    };
    cmd.arg("--workspace").arg(&job.workspace);
    if let Some(v) = &job.verify {
        cmd.arg("--verify").arg(v);
    }
    cmd.stdin(Stdio::from(stdin))
        .stdout(Stdio::from(open(run_dir.join("run.out"))?))
        .stderr(Stdio::from(open(err_path.clone())?));
    let tail = |p: &Path| {
        tail_chars(
            String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).trim(),
            400,
        )
    };
    let mut child = cmd.spawn().map_err(|e| format!("cannot start lane: {e}"))?;
    match tokio::time::timeout(RUN_LIMIT, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => {
            let text = tail(&err_path);
            Err(if text.is_empty() {
                format!(
                    "lane run exited {}",
                    status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "on a signal".into())
                )
            } else {
                text
            })
        }
        Ok(Err(e)) => Err(format!("lane run: {e}")),
        Err(_) => {
            let _ = child.start_kill();
            let text = tail(&err_path);
            Err(if text.is_empty() {
                "lane run did not return within 60 seconds".into()
            } else {
                text
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
[lanes.qoder]
kind = "command"
[lanes.codex-a]
kind = "codex"
[lanes.paid]
kind = "command"
model = "deepseek-paid-x"
[categories]
deep = ["qoder"]
deep-codex = ["qoder", "codex-a"]
"#;

    fn cfg() -> LaneConfig {
        LaneConfig::parse(CONFIG).unwrap()
    }

    #[test]
    fn target_checks() {
        let c = cfg();
        assert!(check_target(&c, Some("qoder"), None, true).is_ok());
        assert!(check_target(&c, None, Some("deep"), true).is_ok());
        assert!(matches!(
            check_target(&c, Some("nope"), None, false),
            Err(ManagerError::Invalid(_))
        ));
        assert!(matches!(
            check_target(&c, None, Some("nope"), false),
            Err(ManagerError::Invalid(_))
        ));
        assert!(matches!(
            check_target(&c, Some("paid"), None, false),
            Err(ManagerError::Invalid(_))
        ));
    }

    #[test]
    fn no_codex_on_lucina() {
        let c = cfg();
        let msg = "no Codex on Lucina work: use the -lucina chain";
        for (lane, chain) in [(Some("codex-a"), None), (None, Some("deep-codex"))] {
            match check_target(&c, lane, chain, true) {
                Err(ManagerError::CodexNotAllowed(m)) => assert_eq!(m, msg),
                other => panic!("{other:?}"),
            }
            assert!(check_target(&c, lane, chain, false).is_ok());
        }
    }

    #[test]
    fn expect_stays_inside() {
        assert!(check_expect("out/report.md").is_ok());
        assert!(check_expect("/etc/passwd").is_err());
        assert!(check_expect("../x").is_err());
        assert!(check_expect("a/../../x").is_err());
        assert!(check_expect(" ").is_err());
    }

    #[test]
    fn child_environment() {
        let vars = [
            ("HOME", "/h"),
            ("PATH", "/p"),
            ("LC_ALL", "C"),
            ("SECRET_TOKEN", "x"),
            ("LANE_CONFIG", "/c"),
            ("HOMEX", "y"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        let got: Vec<String> = lane_env(vars.into_iter())
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(got, ["HOME", "PATH", "LC_ALL", "LANE_CONFIG"]);
    }

    #[test]
    fn tail_is_by_characters() {
        assert_eq!(tail_chars("abcdef", 3), "def");
        assert_eq!(tail_chars("ab", 5), "ab");
        assert_eq!(tail_chars("aéb€", 2), "b€");
    }

    #[test]
    fn jobs_file_round_trip_and_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).is_empty());
        let job = WorkJob {
            id: "wk-1".into(),
            lead: "l".into(),
            ..WorkJob::default()
        };
        write_jobs(dir.path(), std::slice::from_ref(&job)).unwrap();
        assert_eq!(load(dir.path()), vec![job]);
        std::fs::write(dir.path().join("jobs.json"), "{not json").unwrap();
        assert!(load(dir.path()).is_empty());
        assert!(!dir.path().join("jobs.json").exists());
        let aside: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("jobs.json.corrupt-")
            })
            .collect();
        assert_eq!(aside.len(), 1);
    }

    #[test]
    fn old_finished_jobs_are_hidden() {
        let old = (chrono::Utc::now() - chrono::Duration::hours(30)).to_rfc3339();
        let fresh = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        assert!(recent(WorkState::Running, Some(&old)));
        assert!(!recent(WorkState::Verified, Some(&old)));
        assert!(recent(WorkState::Verified, Some(&fresh)));
        assert!(recent(WorkState::Verified, None));
    }
}
