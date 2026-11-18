//! Work jobs from the manager's side: one delegated worker run that belongs to a lead.
//! agentd starts it with the installed `lane` binary (`lane run`, the spec on stdin) and
//! follows it with `lane status`; it never reimplements `lane` and never changes it.
//!
//! Jobs live in `<state dir>/work/jobs.json` (written through a temp file and a rename) and
//! the specs in `<state dir>/work/<id>/spec.md`. A poller reads `lane status` for every
//! unfinished job, so a daemon restart loses nothing. The pure rules are in
//! [`lucina_agents::work`].

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use lucina_agents::ids::{lane_run_id, run_state_for, IdRow, Kind};
use lucina_agents::work::{
    apply_status, emptiness, from_direct, missing_ls_fields, missing_status_fields, parse_ls,
    status_unreadable, summarize, validate_target, CostClass, LeadSummary, LsRows, Priority,
    WorkJob, WorkState,
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
    /// `LUCINA_AGENTD_WORK_SCOPE` (`auto`, `on`, `off`; default `auto`).
    pub scope: ScopeMode,
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
            scope: parse_scope_mode(std::env::var("LUCINA_AGENTD_WORK_SCOPE").ok().as_deref()),
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
    /// How long one `lane status` call may take. `LUCINA_AGENTD_WORK_STATUS_SECS` (default 20).
    /// A call that does not answer in time says nothing of the run: it is neither an answer nor
    /// a miss.
    status_limit: Duration,
}

impl Work {
    pub(super) fn open(cfg: WorkConfig) -> Self {
        let (jobs, blocked) = load_checked(&cfg.dir);
        let status_secs = std::env::var("LUCINA_AGENTD_WORK_STATUS_SECS")
            .ok()
            .and_then(|s| s.parse().ok());
        Work {
            cfg,
            jobs: Mutex::new(jobs),
            misses: Mutex::new(HashMap::new()),
            launching: Mutex::new(HashSet::new()),
            blocked: AtomicBool::new(blocked),
            status_limit: Duration::from_secs(status_secs.unwrap_or(QUERY_LIMIT.as_secs()).max(1)),
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
pub(super) fn clean_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.env_clear()
        .envs(lane_env(utf8_vars(std::env::vars_os())))
        .kill_on_drop(true);
    c
}

fn lane_command(bin: &Path) -> Command {
    clean_command(bin)
}

/// `LUCINA_AGENTD_WORK_SCOPE`: whether `lane run` starts in its own transient systemd scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScopeMode {
    /// Use the scope when `systemd-run` is found and `XDG_RUNTIME_DIR` is set (the default).
    #[default]
    Auto,
    /// Require the scope: refuse the start when it cannot be had.
    On,
    /// Never: start `lane run` directly.
    Off,
}

/// The mode a `LUCINA_AGENTD_WORK_SCOPE` value names; an unknown word is `auto`, with a warning.
fn parse_scope_mode(value: Option<&str>) -> ScopeMode {
    match value.map(str::trim) {
        None | Some("") | Some("auto") => ScopeMode::Auto,
        Some("on") => ScopeMode::On,
        Some("off") => ScopeMode::Off,
        Some(other) => {
            tracing::warn!(
                value = other,
                "LUCINA_AGENTD_WORK_SCOPE is not auto, on or off: using auto"
            );
            ScopeMode::Auto
        }
    }
}

/// Whether a start uses the scope, from the mode and what the daemon's environment has: `Ok`
/// carries the answer and a warning to log when the scope was skipped on a systemd host
/// because `XDG_RUNTIME_DIR` is missing; `Err` is the reason a start is refused (mode `on`).
fn scope_decision(
    mode: ScopeMode,
    systemd_run_found: bool,
    xdg_runtime_dir: bool,
) -> Result<(bool, Option<&'static str>), String> {
    match mode {
        ScopeMode::Off => Ok((false, None)),
        ScopeMode::Auto if !systemd_run_found => Ok((false, None)),
        ScopeMode::Auto if !xdg_runtime_dir => Ok((
            false,
            Some("XDG_RUNTIME_DIR is not set: work jobs start without their own systemd scope"),
        )),
        ScopeMode::Auto => Ok((true, None)),
        ScopeMode::On if !systemd_run_found => Err(
            "LUCINA_AGENTD_WORK_SCOPE=on needs systemd-run, which was not found on PATH".into(),
        ),
        ScopeMode::On if !xdg_runtime_dir => Err(
            "LUCINA_AGENTD_WORK_SCOPE=on needs XDG_RUNTIME_DIR (the systemd user manager), which is not set"
                .into(),
        ),
        ScopeMode::On => Ok((true, None)),
    }
}

const SCOPE_PREFIX: &str = "lucina-work-";

/// The transient unit name of a job: `lucina-work-<id>`. The id may hold lower-case letters,
/// digits and `-` only; anything else is refused.
fn scope_unit(id: &str) -> Result<String, String> {
    let ok = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(format!("{SCOPE_PREFIX}{id}"))
    } else {
        Err(format!(
            "job id \"{id}\" cannot name a systemd unit (lower-case letters, digits and - only)"
        ))
    }
}

/// The `lane` arguments of a start, `run` first.
fn lane_run_args(job: &WorkJob) -> Result<Vec<OsString>, String> {
    let mut a: Vec<OsString> = vec!["run".into(), "--id".into(), (&job.id).into()];
    match (&job.lane, &job.chain) {
        (Some(l), _) => a.extend(["--lane".into(), l.into()]),
        (None, Some(c)) => a.extend(["--category".into(), c.into()]),
        (None, None) => return Err("no lane or chain".into()),
    }
    a.extend(["--workspace".into(), (&job.workspace).into()]);
    a.extend(["--owner".into(), (&job.lead).into()]);
    a.extend(["--cost".into(), job.cost.as_str().into()]);
    // The ids as flags: they win over the environment `lane` may carry.
    if let (Some(run), Some(change)) = (&job.run_id, &job.change) {
        a.extend([
            "--lead".into(),
            (&job.lead).into(),
            "--run-id".into(),
            run.into(),
            "--change".into(),
            change.into(),
        ]);
    }
    if let Some(e) = &job.expect {
        a.extend(["--expect-file".into(), e.into()]);
    }
    if let Some(v) = &job.verify {
        a.extend(["--verify".into(), v.into()]);
    }
    Ok(a)
}

/// The program and arguments of the start. With a scope (`Some(unit)`) the list is
/// `systemd-run --user --scope --quiet --collect --unit <unit> -- <lane> <run arguments>`: a
/// scope, not a service, because `lane run` exits at once and the runner it leaves must live
/// on in a control group outside the daemon's unit. Without one it is `<lane> <run arguments>`.
/// No test here can show the control-group behaviour itself; it is proved live after deploy.
fn run_argv(scope: Option<&str>, lane: &Path, run_args: &[OsString]) -> Vec<OsString> {
    let mut v: Vec<OsString> = Vec::new();
    if let Some(unit) = scope {
        v.extend(
            [
                "systemd-run",
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "--unit",
            ]
            .map(OsString::from),
        );
        v.push(unit.into());
        v.push("--".into());
    }
    v.push(lane.into());
    v.extend(run_args.iter().cloned());
    v
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
    /// The change (`LA-C004`) the run works for; it must exist and belong to the lead.
    #[serde(default)]
    pub change: String,
    /// A short name for the run; it ends up in the lane run id (`la-r023-ids-writer`).
    #[serde(default)]
    pub name: Option<String>,
    /// The chat that starts the run.
    #[serde(default)]
    pub session: Option<String>,
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

/// Whether `workspace/expect` is a regular file with something in it, seen without following a
/// symlink anywhere on the way: the file the run had to leave behind is inside the workspace,
/// and a link that steps out of it is not that file. The length is the no-follow one too, so a
/// link is neither a file nor an empty one.
fn expect_written(workspace: &str, expect: &str) -> bool {
    let mut path = PathBuf::from(workspace);
    let mut last = None;
    for part in Path::new(expect).components() {
        // `check_expect` refused an absolute path and a `..`; anything else that is not a plain
        // name is a refusal too.
        let Component::Normal(name) = part else {
            return false;
        };
        path.push(name);
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            return false;
        };
        if meta.file_type().is_symlink() {
            return false;
        }
        last = Some(meta);
    }
    last.is_some_and(|m| m.is_file() && m.len() > 0)
}

/// Sets the end of a job: when it ended and whether it produced nothing.
fn finish(job: &mut WorkJob) {
    job.ended_at = Some(now_iso());
    // `lane` itself found nothing: no need to ask the heads.
    if job.state == WorkState::EndedEmpty {
        job.empty = Some(true);
        return;
    }
    // A lost run is not asked whether it produced something: a run whose runner vanished
    // leaves no heads to compare, which is how agentd has always treated its own lost jobs.
    if job.state == WorkState::Lost {
        return;
    }
    let expect_present = job
        .expect
        .as_ref()
        .map(|e| expect_written(&job.workspace, e));
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
        let (scoped, skipped) = scope_decision(
            self.work().cfg.scope,
            crate::process::resolve_program("systemd-run").is_some(),
            std::env::var_os("XDG_RUNTIME_DIR").is_some(),
        )
        .map_err(ManagerError::Invalid)?;
        if let Some(w) = skipped {
            static WARNED: AtomicBool = AtomicBool::new(false);
            if !WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!("{w}");
            }
        }
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
        // No cost at all is a light run, the same as a plain `lane run` since lane LB-V010.
        let cost = req.cost.unwrap_or(CostClass::Light);
        if cost == CostClass::Light && expect.is_none() {
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
        let change = nonblank(Some(req.change)).ok_or_else(|| {
            ManagerError::Invalid(
                "change is required: the run works for a change (`lagent id new change --lead <code> --title <text>`)"
                    .into(),
            )
        })?;
        self.check_work_ids(&lead, &change)?;
        let name = nonblank(req.name);
        let session = nonblank(req.session);
        let title = nonblank(req.title);

        // The run's row is written BEFORE `lane run` starts, so the lead, the lane, the base
        // commit and the workspace are on record before the worker can make a commit. A trailer
        // in a commit is text the worker types; this row, written by the daemon, is what later
        // checks trust. When the store cannot be written the start is refused.
        let facts = git_facts(&workspace).await;
        let run_title = title.clone().unwrap_or_else(|| first_line(&req.spec, 80));
        let lane_model = lane
            .as_deref()
            .and_then(|l| lanes.lanes.get(l))
            .and_then(|l| l.model.clone())
            .filter(|m| !m.is_empty());
        let by = match (&lane, &chain) {
            (Some(l), _) => match &lane_model {
                Some(m) => format!("{l} {m}"),
                None => l.clone(),
            },
            (None, Some(c)) => format!("chain {c}"),
            (None, None) => String::new(),
        };
        let dir = self.work().cfg.dir.clone();
        let name_for_id = name.clone().unwrap_or_else(|| run_title.clone());
        let mut chosen = String::new();
        let run = self
            .inner
            .ids
            .issue(Kind::Run, &lead, None, "daemon", |run_id| {
                let base = lane_run_id(run_id, &name_for_id);
                let mut id = base.clone();
                let mut n = 2;
                while self.work().jobs().iter().any(|j| j.id == id) || dir.join(&id).exists() {
                    id = format!("{base}-{n}");
                    n += 1;
                }
                let spec_path = dir.join(&id).join("spec.md").display().to_string();
                chosen = id;
                IdRow {
                    state: "started".into(),
                    title: run_title.clone(),
                    by,
                    spec: spec_path,
                    where_: workspace.clone(),
                    base: facts.head.clone().unwrap_or_default(),
                    refs: vec![change.clone()],
                    lane: lane.clone(),
                    model: lane_model.clone(),
                    chain: chain.clone(),
                    repo: facts.repo.clone(),
                    workspace: Some(workspace.clone()),
                    branch: facts.branch.clone(),
                    ..IdRow::default()
                }
            })?;
        let id = chosen;
        let run_id = run.id.clone();
        let scope = if scoped {
            Some(scope_unit(&id).map_err(ManagerError::Invalid)?)
        } else {
            None
        };
        let run_dir = dir.join(&id);
        let spec_path = run_dir.join("spec.md");
        let stored =
            std::fs::create_dir_all(&run_dir).and_then(|_| std::fs::write(&spec_path, &req.spec));
        if let Err(e) = stored {
            let note = format!("cannot store the spec: {e}");
            if let Err(id_err) = self.append_run_row(
                &run_id,
                IdRow {
                    state: "failed".into(),
                    result: note.clone(),
                    ..IdRow::default()
                },
            ) {
                tracing::error!(run = run_id, error = %id_err, "cannot write the run's id row");
            }
            return Err(ManagerError::Invalid(note));
        }

        let base_head = facts.head.clone();
        let job = WorkJob {
            id: id.clone(),
            lead,
            priority: req.priority.unwrap_or_default(),
            cost,
            lane: lane.clone(),
            chain: chain.clone(),
            workspace: workspace.clone(),
            verify: nonblank(req.verify),
            expect,
            title,
            spec_path: spec_path.display().to_string(),
            created_at: now_iso(),
            base_head,
            state: WorkState::Starting,
            run_id: Some(run_id),
            change: Some(change),
            session,
            scope,
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
                self.write_end_row(&out);
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
            Some(_) => match query_status(&lane_bin, &id, self.work().status_limit).await {
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
                                // What `lane run` printed is lane's own word.
                                j.note = Some(note);
                                j.note_from_lane = true;
                            }
                            if j.state.is_terminal() && j.ended_at.is_none() {
                                finish(j);
                            }
                        }
                        None => {
                            j.state = WorkState::Failed;
                            j.note = Some(note);
                            j.note_from_lane = true;
                            j.ended_at = Some(now_iso());
                        }
                    }
                }
            }
            self.work().persist(&jobs);
            jobs.iter().find(|j| j.id == id).cloned().unwrap_or(job)
        };
        // While the id is in `launching` the poller leaves the job alone: one end row.
        self.write_end_row(&out);
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
        // A final job the store has no end row for is followed too: `lane` has nothing more to
        // say about it, the id store has the whole row owed.
        let owed = self.end_rows_owed();
        for id in open.into_iter().chain(owed) {
            self.poll_one(&bin, &id).await;
        }
    }

    /// The ids of the final jobs whose end row is not in the store. The id store is only asked
    /// once the jobs lock is gone: `issue` takes the store's lock and then the jobs one.
    fn end_rows_owed(&self) -> Vec<String> {
        let launching: HashSet<String> = self
            .work()
            .launching
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let want: Vec<(String, String, &'static str)> = {
            let jobs = self.work().jobs();
            jobs.iter()
                .filter(|j| !pollable(j) && !launching.contains(&j.id))
                .filter_map(|j| {
                    let state = run_state_for(j.state, j.empty)?;
                    Some((j.id.clone(), j.run_id.clone()?, state))
                })
                .collect()
        };
        want.into_iter()
            .filter(|(_, run_id, state)| {
                self.inner.ids.latest_state(run_id).as_deref() != Some(*state)
            })
            .map(|(id, _, _)| id)
            .collect()
    }

    /// The run state the store still owes this final job: what [`run_state_for`] gives when the
    /// run's latest row is not it. `None` for an unfinished job, for a state that writes no
    /// row, and for a row that is already there.
    fn owed_end_row_state(&self, job: &WorkJob) -> Option<&'static str> {
        if !job.state.is_terminal() {
            return None;
        }
        let state = run_state_for(job.state, job.empty)?;
        let run_id = job.run_id.as_deref()?;
        (self.inner.ids.latest_state(run_id).as_deref() != Some(state)).then_some(state)
    }

    /// One `lane status` read for one job. An answer that exits 0 but is not readable JSON is
    /// not a miss: the job never becomes `lost` from it and its state does not change. It
    /// goes through [`status_unreadable`] (a note and a count) and the job is saved when
    /// that changed it. A call that never answers (it did not start, or it timed out) is not a
    /// miss either: state and miss count stay as they are. A final job that is followed only
    /// owes its end row: the store is asked again and `lane` is left alone.
    async fn poll_one(&self, bin: &Path, id: &str) {
        let Some(job) = self.stored_job(id) else {
            return;
        };
        if !pollable(&job) {
            // A final job reaches here only when the store still owes it its end row: `lane`
            // is not asked again, only the store.
            if self.owed_end_row_state(&job).is_some() {
                self.write_end_row(&job);
            }
            return;
        }
        let reply = query_status(bin, id, self.work().status_limit).await;
        let Some(mut job) = self.stored_job(id) else {
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
                    job.note_from_lane = false;
                    job.empty = None;
                }
                apply_read(&mut job, &status);
            }
            StatusReply::BadJson(e) => {
                tracing::warn!(job = id, error = %e, "lane status printed bad JSON");
                // The streak of non-zero exits is broken: an exit 0 of any kind says something.
                self.work()
                    .misses
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(id);
                if !status_unreadable(&mut job, &e) {
                    return;
                }
            }
            StatusReply::Stalled => {
                // The call said nothing of the run; only the wall clock is at fault. State and
                // miss count stay as they are and the same status is asked again next tick.
                return;
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
        self.record_final_lane(&job);
        if job == before {
            return;
        }
        let ended_now = job.state.is_terminal() && job.state != before.state;
        let stored = {
            let mut jobs = self.work().jobs();
            // A start that failed in the meantime wins: only an unfinished (or lost) job takes the update.
            match jobs.iter_mut().find(|j| j.id == id && pollable(j)) {
                Some(stored) => {
                    *stored = job.clone();
                    self.work().persist(&jobs);
                    true
                }
                None => false,
            }
        };
        if stored && ended_now {
            self.write_end_row(&job);
        }
    }

    /// The stored job whose run id is `run_id`.
    pub(super) fn work_job_of_run(&self, run_id: &str) -> Option<WorkJob> {
        self.work()
            .jobs()
            .iter()
            .find(|j| j.run_id.as_deref() == Some(run_id))
            .cloned()
    }

    /// The stored job with this id.
    fn stored_job(&self, id: &str) -> Option<WorkJob> {
        self.work().jobs().iter().find(|j| j.id == id).cloned()
    }

    /// Appends a row of a run the daemon declares; the store's refusal is the caller's to keep.
    fn append_run_row(&self, run_id: &str, row: IdRow) -> Result<(), ManagerError> {
        self.inner.ids.append(run_id, row, "daemon").map(|_| ())
    }

    /// The job's end row. A store that refuses it is written into the job's note, and the
    /// poller keeps the job until the row is in the store.
    fn write_end_row(&self, job: &WorkJob) {
        if let Err(e) = self.record_end(job) {
            self.note_refused_end_row(&job.id, &e.to_string());
        }
    }

    /// The note of an end row the store would not take, saved with the job.
    fn note_refused_end_row(&self, id: &str, error: &str) {
        let note = format!("the id store refused the end row: {error}");
        let mut jobs = self.work().jobs();
        if let Some(j) = jobs.iter_mut().find(|j| j.id == id) {
            if j.note.as_deref() != Some(note.as_str()) {
                j.note = Some(note);
                self.work().persist(&jobs);
            }
        }
    }

    /// For a chain: the first time `lane status` names the lane that is writing, one `started`
    /// row says which lane and model it is, so they are on record while the work is going on.
    /// The next poll asks again while the row is missing, so a refusal needs no note.
    fn record_final_lane(&self, job: &WorkJob) {
        let (Some(run_id), Some(lane)) = (&job.run_id, &job.final_lane) else {
            return;
        };
        if job.chain.is_none()
            || job.state.is_terminal()
            || self.inner.ids.has_lane_row(run_id, lane)
        {
            return;
        }
        if let Err(e) = self.append_run_row(
            run_id,
            IdRow {
                state: "started".into(),
                by: lane_by(lane, job.model.as_deref()),
                lane: Some(lane.clone()),
                model: job.model.clone().filter(|m| !m.is_empty()),
                ..IdRow::default()
            },
        ) {
            tracing::error!(run = run_id, error = %e, "cannot write the run's id row");
        }
    }

    /// When a job reaches a final state: one run row from [`run_state_for`] (nothing when it
    /// gives none, and nothing when the run's latest row already has that state for this very
    /// ending). The store's refusal is the caller's.
    fn record_end(&self, job: &WorkJob) -> Result<(), ManagerError> {
        let Some(run_id) = &job.run_id else {
            return Ok(());
        };
        let Some(state) = run_state_for(job.state, job.empty) else {
            return Ok(());
        };
        if let Some((row_state, row_utc)) = self.inner.ids.latest_row(run_id) {
            if row_state == state && ending_is_recorded(&row_utc, job.ended_at.as_deref()) {
                return Ok(());
            }
        }
        let mut result = job
            .lane_state
            .clone()
            .unwrap_or_else(|| job.state.as_str().to_string());
        if let Some(head) = job.after_head.as_ref().or(job.verify_head.as_ref()) {
            result.push_str(&format!(" head {head}"));
        }
        if job.empty == Some(true) {
            result.push_str(" empty");
        }
        let mut row = IdRow {
            state: state.into(),
            result,
            ..IdRow::default()
        };
        if let Some(lane) = &job.final_lane {
            row.by = lane_by(lane, job.model.as_deref());
            row.lane = Some(lane.clone());
            row.model = job.model.clone().filter(|m| !m.is_empty());
        }
        self.append_run_row(run_id, row)
    }
}

/// `<lane> <model>` (the lane alone when it has no model).
fn lane_by(lane: &str, model: Option<&str>) -> String {
    match model.filter(|m| !m.is_empty()) {
        Some(m) => format!("{lane} {m}"),
        None => lane.to_string(),
    }
}

/// The store's row of this state already tells of the job's current ending: it was written at
/// or after the job ended. A lost run that came back and ended again has a later `ended_at`
/// than the row of its first ending, so its second row is written.
fn ending_is_recorded(row_utc: &str, ended_at: Option<&str>) -> bool {
    match (utc_time(row_utc), ended_at.and_then(utc_time)) {
        (Some(row), Some(ended)) => row >= ended,
        // Nothing to compare the row's time with: the row says this state.
        _ => true,
    }
}

fn utc_time(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}

/// The first non-blank line of `text`, cut to `max` characters.
fn first_line(text: &str, max: usize) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
        .chars()
        .take(max)
        .collect()
}

/// What `git` says about a workspace at the start of a run.
#[derive(Debug, Default)]
struct GitFacts {
    /// `git rev-parse HEAD`, the full 40 characters.
    head: Option<String>,
    /// The directory that holds the repository's common git directory.
    repo: Option<String>,
    /// The branch, absent on a detached head.
    branch: Option<String>,
}

async fn git_facts(workspace: &str) -> GitFacts {
    let head = git_output(workspace, &["rev-parse", "HEAD"]).await;
    let common = git_output(
        workspace,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await;
    let branch = git_output(workspace, &["rev-parse", "--abbrev-ref", "HEAD"]).await;
    GitFacts {
        head,
        repo: common.map(|c| c.strip_suffix("/.git").unwrap_or(&c).to_string()),
        branch: branch.filter(|b| b != "HEAD"),
    }
}

/// What `lane status <id>` gave. Only a non-zero exit is a miss: a call that did not start or
/// did not answer in time says nothing of the run, and is [`StatusReply::Stalled`].
enum StatusReply {
    Status(Value),
    BadJson(String),
    Stalled,
    Miss,
}

async fn query_status(bin: &Path, id: &str, limit: Duration) -> StatusReply {
    let mut cmd = lane_command(bin);
    cmd.args(["status", id])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    match tokio::time::timeout(limit, cmd.output()).await {
        Ok(Ok(o)) if o.status.success() => match serde_json::from_slice::<Value>(&o.stdout) {
            Ok(v @ Value::Object(_)) => StatusReply::Status(v),
            Ok(v) => StatusReply::BadJson(format!(
                "the answer is {}, not a status object",
                json_kind(&v)
            )),
            Err(e) => StatusReply::BadJson(e.to_string()),
        },
        Ok(Ok(_)) => StatusReply::Miss,
        Ok(Err(e)) => {
            tracing::warn!(job = id, error = %e, "lane status did not start");
            StatusReply::Stalled
        }
        Err(_) => {
            tracing::warn!(job = id, "lane status timed out");
            StatusReply::Stalled
        }
    }
}

/// What a JSON value is, in the note of a status that is not an object.
fn json_kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "a null",
        Value::Bool(_) => "a true or false",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Whether the poller follows a job: it is unfinished, or it was lost less than a day ago.
/// A run `lane` called `lost` itself is not followed at all: that is the tool's own end, and
/// only the runs agentd marked lost on its misses are asked again, in case it shows up.
fn pollable(job: &WorkJob) -> bool {
    match job.state {
        WorkState::Lost => {
            !job.lane_lost
                && job
                    .ended_at
                    .as_deref()
                    .and_then(age_secs)
                    .is_some_and(|a| a < DAY)
        }
        s => !s.is_terminal(),
    }
}

/// A `git` call in the workspace: its trimmed output, `None` when it fails or prints nothing.
async fn git_output(workspace: &str, args: &[&str]) -> Option<String> {
    let mut cmd = clean_command("git");
    cmd.arg("-C")
        .arg(workspace)
        .args(args)
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
    let argv = run_argv(job.scope.as_deref(), bin, &lane_run_args(job)?);
    // The environment is today's allowlist either way: `systemd-run --user` finds the user
    // manager through `XDG_RUNTIME_DIR`, which is on it.
    let mut cmd = clean_command(&argv[0]);
    cmd.args(&argv[1..]);
    // `lane run` detaches the run it starts, so a stop of the daemon may not reach it: the
    // child is not killed when its handle is dropped, and it is started in a process group of
    // its own, out of the way of a signal to the daemon's. The explicit kill on the 60 s
    // window is the only one this child gets.
    cmd.kill_on_drop(false).process_group(0);
    // The ids as variables for the worker; as flags they are in `lane_run_args`.
    if let (Some(run), Some(change)) = (&job.run_id, &job.change) {
        cmd.env("LUCINA_RUN_ID", run)
            .env("LUCINA_LEAD", &job.lead)
            .env("LUCINA_CHANGE", change);
    }
    let scoped = job.scope.is_some();
    cmd.stdin(Stdio::from(stdin))
        .stdout(Stdio::from(open(run_dir.join("run.out"))?))
        .stderr(Stdio::from(open(err_path.clone())?));
    let tail = |p: &Path| {
        tail_chars(
            String::from_utf8_lossy(&std::fs::read(p).unwrap_or_default()).trim(),
            400,
        )
    };
    let mut child = cmd.spawn().map_err(|e| {
        if scoped {
            format!("cannot start systemd-run: {e}")
        } else {
            format!("cannot start lane: {e}")
        }
    })?;
    match tokio::time::timeout(RUN_LIMIT, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => {
            let text = tail(&err_path);
            let text = if text.is_empty() {
                format!(
                    "lane run exited {}",
                    status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "on a signal".into())
                )
            } else {
                text
            };
            // `systemd-run --scope` passes the command's exit code on; a run `lane` knows
            // still counts (see `launch`), so this note is for a start nobody knows.
            Err(if scoped {
                format!("could not start the run in its own systemd scope: {text}")
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
    // Only the scope test below needs it: a job's own id now comes from the ids store.
    use lucina_agents::work::new_id;

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
            scope: ScopeMode::Off,
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

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    fn sample_job() -> WorkJob {
        WorkJob {
            id: "wk-0123456789ab".into(),
            lead: "chat-a".into(),
            lane: Some("qoder".into()),
            workspace: "/w".into(),
            verify: Some("true".into()),
            expect: Some("out.md".into()),
            ..WorkJob::default()
        }
    }

    #[test]
    fn the_argument_list_with_and_without_the_scope() {
        let args = lane_run_args(&sample_job()).unwrap();
        assert_eq!(
            args,
            os(&[
                "run",
                "--id",
                "wk-0123456789ab",
                "--lane",
                "qoder",
                "--workspace",
                "/w",
                "--owner",
                "chat-a",
                "--cost",
                "build",
                "--expect-file",
                "out.md",
                "--verify",
                "true"
            ])
        );
        let lane = Path::new("/bin/lane");
        let plain = run_argv(None, lane, &args);
        assert_eq!(plain[0], OsString::from("/bin/lane"));
        assert_eq!(plain[1..], args[..]);
        let scoped = run_argv(Some("lucina-work-wk-0123456789ab"), lane, &args);
        assert_eq!(
            scoped[..8],
            os(&[
                "systemd-run",
                "--user",
                "--scope",
                "--quiet",
                "--collect",
                "--unit",
                "lucina-work-wk-0123456789ab",
                "--"
            ])[..]
        );
        assert_eq!(scoped[8], OsString::from("/bin/lane"));
        assert_eq!(scoped[9..], args[..]);
        let mut chained = sample_job();
        chained.lane = None;
        assert!(lane_run_args(&chained).is_err());
        chained.chain = Some("deep".into());
        assert!(lane_run_args(&chained)
            .unwrap()
            .ends_with(&os(&["--verify", "true"])));
    }

    #[test]
    fn the_unit_name_takes_only_lower_case_digits_and_dashes() {
        assert_eq!(
            scope_unit("wk-0123456789ab").as_deref(),
            Ok("lucina-work-wk-0123456789ab")
        );
        for bad in ["", "WK-1", "wk_1", "wk 1", "wk-1.service", "wk-é", "wk/1"] {
            assert!(scope_unit(bad).is_err(), "{bad:?}");
        }
        assert!(scope_unit(&new_id(u128::MAX)).is_ok());
    }

    #[test]
    fn the_scope_mode_words() {
        assert_eq!(parse_scope_mode(None), ScopeMode::Auto);
        assert_eq!(parse_scope_mode(Some("auto")), ScopeMode::Auto);
        assert_eq!(parse_scope_mode(Some("on")), ScopeMode::On);
        assert_eq!(parse_scope_mode(Some("off")), ScopeMode::Off);
        assert_eq!(parse_scope_mode(Some("maybe")), ScopeMode::Auto);
    }

    #[test]
    fn the_scope_decision() {
        use ScopeMode::*;
        assert_eq!(scope_decision(Off, true, true), Ok((false, None)));
        assert_eq!(scope_decision(Auto, false, true), Ok((false, None)));
        assert_eq!(scope_decision(Auto, true, true), Ok((true, None)));
        let (used, warning) = scope_decision(Auto, true, false).unwrap();
        assert!(!used && warning.is_some_and(|w| w.contains("XDG_RUNTIME_DIR")));
        assert_eq!(scope_decision(On, true, true), Ok((true, None)));
        assert!(scope_decision(On, false, true)
            .unwrap_err()
            .contains("systemd-run"));
        assert!(scope_decision(On, true, false)
            .unwrap_err()
            .contains("XDG_RUNTIME_DIR"));
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

    #[test]
    fn the_two_kinds_of_lost_are_polled_differently() {
        let fresh = chrono::Utc::now().to_rfc3339();
        let old = (chrono::Utc::now() - chrono::Duration::hours(30)).to_rfc3339();
        let lost = |lane_lost: bool, ended_at: Option<String>| WorkJob {
            id: "wk-1".into(),
            state: WorkState::Lost,
            lane_lost,
            ended_at,
            ..WorkJob::default()
        };
        // What agentd concluded from its misses is asked again for a day, then dropped.
        assert!(pollable(&lost(false, Some(fresh.clone()))));
        assert!(!pollable(&lost(false, Some(old))));
        // Lane's own word is final from the answer that gave it.
        assert!(!pollable(&lost(true, Some(fresh))));
        assert!(!pollable(&lost(true, None)));
    }

    #[test]
    fn a_lost_run_is_not_asked_whether_it_produced_anything() {
        // A head to start from and nothing since would say `empty` if the heads were asked.
        for lane_lost in [true, false] {
            let mut job = WorkJob {
                id: "wk-1".into(),
                state: WorkState::Lost,
                lane_lost,
                base_head: Some("a".repeat(40)),
                ..WorkJob::default()
            };
            finish(&mut job);
            assert!(job.ended_at.is_some(), "{lane_lost}");
            assert_eq!(job.empty, None, "{lane_lost}");
        }
    }
}
