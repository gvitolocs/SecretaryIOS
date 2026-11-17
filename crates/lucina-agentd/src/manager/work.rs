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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use lucina_agents::work::{
    apply_status, emptiness, from_direct, missing_ls_fields, missing_status_fields, new_id,
    parse_ls, status_unreadable, summarize, validate_target, CostClass, LeadSummary, LsRows,
    Priority, WorkJob, WorkState,
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
    /// An unreadable `jobs.json` could not be moved aside: nothing is written over it.
    blocked: AtomicBool,
}

impl Work {
    pub(super) fn open(cfg: WorkConfig) -> Self {
        let (jobs, blocked) = load_checked(&cfg.dir);
        Work {
            cfg,
            jobs: Mutex::new(jobs),
            misses: Mutex::new(HashMap::new()),
            launching: Mutex::new(HashSet::new()),
            blocked: AtomicBool::new(blocked),
        }
    }

    fn unlaunch(&self, id: &str) {
        self.launching
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(id);
    }

    fn jobs(&self) -> std::sync::MutexGuard<'_, Vec<WorkJob>> {
        self.jobs.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Writes `jobs.json` through a temp file in the same directory and a rename.
    fn persist(&self, jobs: &[WorkJob]) {
        if self.blocked.load(Ordering::Relaxed) {
            tracing::error!(dir = %self.cfg.dir.display(), "work jobs are not saved: the unreadable jobs.json could not be moved aside");
            return;
        }
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
#[cfg(test)]
fn load(dir: &Path) -> Vec<WorkJob> {
    load_checked(dir).0
}

/// The jobs, and whether an unreadable file is still in the way (it could not be moved aside).
fn load_checked(dir: &Path) -> (Vec<WorkJob>, bool) {
    let file = dir.join("jobs.json");
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), false),
        Err(e) => {
            tracing::error!(error = %e, file = %file.display(), "cannot read work jobs");
            return (Vec::new(), !move_aside(&file));
        }
    };
    match serde_json::from_str::<Vec<WorkJob>>(&text) {
        Ok(jobs) => (jobs, false),
        Err(e) => {
            tracing::error!(error = %e, file = %file.display(), "work jobs file is unreadable; moving it aside");
            (Vec::new(), !move_aside(&file))
        }
    }
}

/// `jobs.json.corrupt-<unix secs>-<pid>-<n>`.
fn aside_name(secs: u64, pid: u32, n: u64) -> String {
    format!("jobs.json.corrupt-{secs}-{pid}-{n}")
}

/// Moves the file to a name nothing has; false when that did not work.
fn move_aside(file: &Path) -> bool {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let aside = loop {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = file.with_file_name(aside_name(unix_secs(), std::process::id(), n));
        if std::fs::symlink_metadata(&p).is_err() {
            break p;
        }
    };
    match std::fs::rename(file, &aside) {
        Ok(()) => true,
        Err(e) => {
            tracing::error!(error = %e, "cannot move the work jobs file aside");
            false
        }
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

/// The variables whose name and value are both valid UTF-8; others are skipped silently.
fn utf8_vars(
    vars: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> impl Iterator<Item = (String, String)> {
    vars.filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
}

/// A child with the cleared environment of [`lane_env`].
fn clean_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.env_clear()
        .envs(lane_env(utf8_vars(std::env::vars_os())))
        .kill_on_drop(true);
    c
}

fn lane_command(bin: &Path) -> Command {
    clean_command(bin)
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
        for member in members {
            if let Some(l) = lanes.lanes.get(member) {
                if is_forbidden(member, l) {
                    return Err(ManagerError::Invalid(format!(
                        "lane \"{member}\" (in chain \"{name}\") is not allowed (the paid DeepSeek key is never used)"
                    )));
                }
            }
        }
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
    /// `lane ls` rows that could not be read.
    pub unparsed: usize,
}

/// The note of a status that lacks fields the reader relies on.
const LACKS_NOTE: &str = "lane status lacks the field(s):";

/// Field sets of `lane` output already logged in this daemon run.
fn warn_missing_once(names: &[&'static str]) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let key = names.join(", ");
    let fresh = SEEN
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key.clone());
    if fresh {
        tracing::warn!("lane output lacks the field(s) {key}");
    }
}

/// [`apply_status`] for a status read from `lane`: a status that lacks fields the reader
/// relies on is still applied, logged once per field set, and noted on the job unless the
/// job already has another note. A stale lacks-note goes first.
fn apply_read(job: &mut WorkJob, status: &Value) {
    if job
        .note
        .as_deref()
        .is_some_and(|n| n.starts_with(LACKS_NOTE))
    {
        job.note = None;
    }
    apply_status(job, status);
    let missing = missing_status_fields(status);
    if !missing.is_empty() {
        warn_missing_once(&missing);
        if job.note.is_none() {
            job.note = Some(format!("{LACKS_NOTE} {}", missing.join(", ")));
        }
    }
}

fn nonblank(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Sets the end of a job: when it ended and whether it produced nothing.
fn finish(job: &mut WorkJob) {
    job.ended_at = Some(now_iso());
    // `lane` itself found nothing: no need to ask the heads.
    if job.state == WorkState::EndedEmpty {
        job.empty = Some(true);
        return;
    }
    let expect_present = job.expect.as_ref().map(|e| {
        std::fs::metadata(Path::new(&job.workspace).join(e))
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false)
    });
    // Without a head from the start (not a git directory) lane's own start head is the base.
    let base = job.base_head.as_deref().or(job.lane_base_head.as_deref());
    job.empty = emptiness(
        expect_present,
        base,
        job.after_head.as_deref(),
        job.verify_head.as_deref(),
        job.dirty_files,
    );
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
        if !Path::new(&workspace).is_absolute() {
            return Err(ManagerError::Invalid(
                "workspace must be an absolute path".into(),
            ));
        }
        if !Path::new(&workspace).is_dir() {
            return Err(ManagerError::Invalid(format!(
                "workspace \"{workspace}\" is not a directory"
            )));
        }
        let expect = nonblank(req.expect);
        if let Some(e) = &expect {
            check_expect(e)?;
        }
        if req.cost == Some(CostClass::Light) && expect.is_none() {
            return Err(ManagerError::Invalid(
                "a light job needs --expect <file>: a job that reads or reviews must name the file it leaves behind"
                    .into(),
            ));
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

        // The launch runs to its end even when the caller goes away.
        let manager = self.clone();
        let launched = job.clone();
        let handle = tokio::spawn(async move { manager.launch(lane_bin, launched, run_dir).await });
        match handle.await {
            Ok(out) => Ok(out),
            Err(e) => {
                let mut jobs = self.work().jobs();
                if let Some(j) = jobs.iter_mut().find(|j| j.id == id) {
                    if !j.state.is_terminal() {
                        j.state = WorkState::Failed;
                        j.note = Some(format!("the start did not finish: {e}"));
                        j.ended_at = Some(now_iso());
                    }
                }
                self.work().persist(&jobs);
                let out = jobs.iter().find(|j| j.id == id).cloned().unwrap_or(job);
                drop(jobs);
                self.work().unlaunch(&id);
                Ok(out)
            }
        }
    }

    /// `lane run`, then the job's state from it. When `lane run` failed but `lane` has a
    /// record of the run, that record counts; otherwise the job is `failed`. The id leaves
    /// `launching` on every path.
    async fn launch(&self, lane_bin: PathBuf, job: WorkJob, run_dir: PathBuf) -> WorkJob {
        let id = job.id.clone();
        let failure = run_lane(&lane_bin, &job, &run_dir).await.err();
        let status = match &failure {
            Some(_) => match query_status(&lane_bin, &id).await {
                StatusReply::Status(v) => Some(v),
                _ => None,
            },
            None => None,
        };
        let out = {
            let mut jobs = self.work().jobs();
            if let Some(j) = jobs.iter_mut().find(|j| j.id == id) {
                if let Some(note) = failure {
                    match status {
                        Some(st) => {
                            apply_read(j, &st);
                            if j.note.is_none() {
                                j.note = Some(note);
                            }
                            if j.state.is_terminal() && j.ended_at.is_none() {
                                finish(j);
                            }
                        }
                        None => {
                            j.state = WorkState::Failed;
                            j.note = Some(note);
                            j.ended_at = Some(now_iso());
                        }
                    }
                }
            }
            self.work().persist(&jobs);
            jobs.iter().find(|j| j.id == id).cloned().unwrap_or(job)
        };
        self.work().unlaunch(&id);
        out
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
        let mut unparsed = 0;
        if include_direct {
            match self.lane_ls().await {
                Ok(ls) => {
                    if ls.unparsed > 0 {
                        let total = ls.rows.len() + ls.unparsed;
                        let first = ls.first_error.clone().unwrap_or_default();
                        tracing::warn!(
                            unparsed = ls.unparsed,
                            total,
                            error = %first,
                            "some lane ls rows could not be read"
                        );
                        unparsed = ls.unparsed;
                        warning = Some(format!(
                            "{} of {total} lane rows could not be read: {first}",
                            ls.unparsed
                        ));
                    }
                    for row in &ls.rows {
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
                Err(w) => {
                    tracing::warn!(error = %w, "lane ls failed");
                    warning = Some(w);
                }
            }
        }
        let leads = summarize(jobs);
        let jobs = leads.iter().flat_map(|l| l.jobs.iter().cloned()).collect();
        WorkList {
            jobs,
            leads,
            warning,
            unparsed,
        }
    }

    /// `lane ls --json --limit 200` (JSON Lines), as rows; the text of the failure when it fails.
    async fn lane_ls(&self) -> Result<LsRows, String> {
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
        let ls = parse_ls(&String::from_utf8_lossy(&out.stdout));
        for row in &ls.rows {
            let missing = missing_ls_fields(row);
            if !missing.is_empty() {
                warn_missing_once(&missing);
            }
        }
        Ok(ls)
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
                .filter(|j| pollable(j) && !launching.contains(&j.id))
                .map(|j| j.id.clone())
                .collect()
        };
        for id in open {
            self.poll_one(&bin, &id).await;
        }
    }

    /// One `lane status` read for one job. An answer that exits 0 but is not readable JSON is
    /// not a miss: the job never becomes `lost` from it and its state does not change. It
    /// goes through [`status_unreadable`] (a note and a count) and the job is saved when
    /// that changed it.
    async fn poll_one(&self, bin: &Path, id: &str) {
        let reply = query_status(bin, id).await;
        let Some(mut job) = self.work().jobs().iter().find(|j| j.id == id).cloned() else {
            return;
        };
        if !pollable(&job) {
            return;
        }
        let before = job.clone();
        match reply {
            StatusReply::Status(status) => {
                self.work()
                    .misses
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(id);
                if job.state == WorkState::Lost {
                    // `lane` knows the run after all: it is not lost.
                    job.ended_at = None;
                    job.note = None;
                    job.empty = None;
                }
                apply_read(&mut job, &status);
            }
            StatusReply::BadJson(e) => {
                tracing::warn!(job = id, error = %e, "lane status printed bad JSON");
                if !status_unreadable(&mut job, &e) {
                    return;
                }
            }
            StatusReply::Miss if job.state == WorkState::Lost => return,
            StatusReply::Miss => {
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
        }
        if job.state.is_terminal() && job.ended_at.is_none() {
            finish(&mut job);
        }
        if job == before {
            return;
        }
        let mut jobs = self.work().jobs();
        // A start that failed in the meantime wins: only an unfinished (or lost) job takes the update.
        if let Some(stored) = jobs.iter_mut().find(|j| j.id == id && pollable(j)) {
            *stored = job;
            self.work().persist(&jobs);
        }
    }
}

/// What `lane status <id>` gave. A start failure, a timeout and a non-zero exit all count as a miss.
enum StatusReply {
    Status(Value),
    BadJson(String),
    Miss,
}

async fn query_status(bin: &Path, id: &str) -> StatusReply {
    let mut cmd = lane_command(bin);
    cmd.args(["status", id])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    match tokio::time::timeout(QUERY_LIMIT, cmd.output()).await {
        Ok(Ok(o)) if o.status.success() => match serde_json::from_slice::<Value>(&o.stdout) {
            Ok(v) => StatusReply::Status(v),
            Err(e) => StatusReply::BadJson(e.to_string()),
        },
        Ok(Ok(_)) => StatusReply::Miss,
        Ok(Err(e)) => {
            tracing::warn!(job = id, error = %e, "lane status did not start");
            StatusReply::Miss
        }
        Err(_) => {
            tracing::warn!(job = id, "lane status timed out");
            StatusReply::Miss
        }
    }
}

/// Whether the poller follows a job: it is unfinished, or it was lost less than a day ago.
fn pollable(job: &WorkJob) -> bool {
    match job.state {
        WorkState::Lost => job
            .ended_at
            .as_deref()
            .and_then(age_secs)
            .is_some_and(|a| a < DAY),
        s => !s.is_terminal(),
    }
}

/// `git rev-parse HEAD` in the workspace, `None` when it fails.
async fn git_head(workspace: &str) -> Option<String> {
    let mut cmd = clean_command("git");
    cmd.arg("-C")
        .arg(workspace)
        .args(["rev-parse", "HEAD"])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
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
    cmd.arg("--owner").arg(&job.lead);
    cmd.arg("--cost").arg(job.cost.as_str());
    if let Some(e) = &job.expect {
        cmd.arg("--expect-file").arg(e);
    }
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
    fn a_chain_with_a_forbidden_member_is_refused() {
        let mut c = cfg();
        c.categories
            .insert("with-paid".into(), vec!["qoder".into(), "paid".into()]);
        c.categories
            .insert("with-unknown".into(), vec!["qoder".into(), "ghost".into()]);
        match check_target(&c, None, Some("with-paid"), false) {
            Err(ManagerError::Invalid(m)) => assert!(m.contains("\"paid\""), "{m}"),
            other => panic!("{other:?}"),
        }
        // A member the config does not have is not a forbidden lane.
        assert!(check_target(&c, None, Some("with-unknown"), false).is_ok());
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
    fn aside_names_are_unique_and_never_replace_a_file() {
        assert_eq!(aside_name(10, 7, 3), "jobs.json.corrupt-10-7-3");
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("jobs.json");
        let mut seen = Vec::new();
        for i in 0..3 {
            std::fs::write(&file, format!("bad {i}")).unwrap();
            assert!(move_aside(&file));
            assert!(!file.exists());
        }
        for e in std::fs::read_dir(dir.path()).unwrap() {
            let e = e.unwrap();
            seen.push(std::fs::read_to_string(e.path()).unwrap());
        }
        seen.sort();
        assert_eq!(seen, ["bad 0", "bad 1", "bad 2"]);
    }

    #[test]
    fn nothing_is_written_while_an_unreadable_file_is_in_the_way() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = WorkConfig {
            dir: dir.path().join("work"),
            lane: None,
            poll: Duration::from_secs(1),
            lost_after: Duration::from_secs(1),
        };
        let work = Work::open(cfg);
        let job = WorkJob {
            id: "wk-1".into(),
            ..WorkJob::default()
        };
        work.persist(std::slice::from_ref(&job));
        assert!(work.cfg.dir.join("jobs.json").exists());
        std::fs::remove_file(work.cfg.dir.join("jobs.json")).unwrap();
        work.blocked.store(true, Ordering::Relaxed);
        work.persist(std::slice::from_ref(&job));
        work.persist(std::slice::from_ref(&job));
        assert!(!work.cfg.dir.join("jobs.json").exists());
    }

    #[test]
    fn non_utf8_variables_are_skipped() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let vars = vec![
            (OsString::from("HOME"), OsString::from("/h")),
            (OsString::from("LC_X"), OsString::from_vec(vec![0xff, 0xfe])),
            (OsString::from_vec(vec![0xff]), OsString::from("v")),
            (OsString::from("PATH"), OsString::from("/p")),
        ];
        let got: Vec<String> = lane_env(utf8_vars(vars.into_iter()))
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(got, ["HOME", "PATH"]);
    }

    #[test]
    fn a_status_that_lacks_fields_is_applied_and_noted() {
        let mut full: Value = serde_json::from_str(include_str!(
            "../../../lucina-agents/tests/fixtures/lane/status-running.json"
        ))
        .unwrap();
        let mut job = WorkJob {
            id: "wk-1".into(),
            ..WorkJob::default()
        };
        apply_read(&mut job, &full);
        assert_eq!(job.state, WorkState::Running);
        assert_eq!(job.note, None);

        full.as_object_mut().unwrap().remove("dirty_files");
        apply_read(&mut job, &full);
        assert_eq!(job.state, WorkState::Running);
        assert_eq!(
            job.note.as_deref(),
            Some("lane status lacks the field(s): dirty_files")
        );
        // A complete status takes the note away again.
        full["dirty_files"] = serde_json::json!([]);
        apply_read(&mut job, &full);
        assert_eq!(job.note, None);
        // Another note stays.
        job.note = Some("mine".into());
        full.as_object_mut().unwrap().remove("dirty_files");
        apply_read(&mut job, &full);
        assert_eq!(job.note.as_deref(), Some("mine"));
    }

    #[test]
    fn an_ended_empty_run_is_empty() {
        let mut status: Value = serde_json::from_str(include_str!(
            "../../../lucina-agents/tests/fixtures/lane/status-reported_done.json"
        ))
        .unwrap();
        status["state"] = serde_json::json!("ended_empty");
        let mut job = WorkJob {
            id: "wk-1".into(),
            // A head that differs from the fixture's would say "not empty" if it were asked.
            base_head: Some("0".repeat(40)),
            ..WorkJob::default()
        };
        apply_read(&mut job, &status);
        assert_eq!(job.state, WorkState::EndedEmpty);
        finish(&mut job);
        assert_eq!(job.empty, Some(true));
        assert!(job.ended_at.is_some());
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
