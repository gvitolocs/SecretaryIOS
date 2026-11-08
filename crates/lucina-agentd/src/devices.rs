//! Installs on leased devices: the owner's iPhone (rules: [`lucina_agents::device`];
//! leases: [`crate::pipeline`]).
//!
//! The phone hangs off the Mac, so the build and `devicectl` run there: agentd checks
//! the request against the lease, origin/main and the owner's approval, issues a one-use
//! grant and runs the Mac executor (`ios-install executor`) over SSH. The executor shows
//! the grant back (`lagent device grant-check`) before it builds and again before it
//! installs. agentd then records a main install with `deploy-guard record --runtime ios`
//! (the ledger and the `deploy/ios/*` tag stay deploy-guard's); test installs and test
//! runs go to this module's history only.
//!
//! State: `<state>/devices/state.json` (what is installed, open grants) and
//! `<state>/devices/history.jsonl`; executor logs in `<state>/devices/logs`; the clean
//! detached checkout deploy-guard reads in `<state>/devices/<device>-ledger`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lucina_agents::device::{self, Approval, Grant, GrantState, Holder, InstallDecision, InstallFacts, InstallKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::Command;

use crate::manager::{LeaseRequest, Manager, ReleaseRequest};
use crate::pipeline::Lease;

/// A device agentd can install on.
#[derive(Debug, Clone)]
pub struct DeviceSpec {
    pub name: String,
    pub label: String,
    /// SSH hosts (this host's `~/.ssh/config`) of the machine the device is attached to, tried in order.
    pub hosts: Vec<String>,
    /// The executor on that machine.
    pub executor: String,
    /// The repository the builds come from (a checkout on this host).
    pub repo: PathBuf,
    /// deploy-guard's runtime for main installs.
    pub runtime: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub dir: PathBuf,
    pub devices: Vec<DeviceSpec>,
    pub deploy_guard: String,
    pub decide: String,
    /// How long one install (build included) may take.
    pub install_timeout: Duration,
}

fn env_or(key: &str, default: String) -> String {
    std::env::var(key).ok().filter(|v| !v.is_empty()).unwrap_or(default)
}

impl Config {
    /// The iPhone, with `LUCINA_DEVICE_IPHONE_HOSTS` (comma-separated), `…_EXECUTOR` and
    /// `…_REPO` overrides; `LUCINA_DEPLOY_GUARD` and `LUCINA_DECIDE_BIN` for the tools.
    pub fn from_env(state_dir: &Path) -> Self {
        let home = std::env::var("HOME").unwrap_or_default();
        let iphone = DeviceSpec {
            name: "iphone".into(),
            label: "iPhone 16e".into(),
            hosts: env_or("LUCINA_DEVICE_IPHONE_HOSTS", "mac-home,mac-via-cursor".into())
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            executor: env_or("LUCINA_DEVICE_IPHONE_EXECUTOR", "~/.local/bin/ios-install".into()),
            repo: PathBuf::from(env_or("LUCINA_DEVICE_IPHONE_REPO", format!("{home}/Projects/Lucina/SecretaryIOS"))),
            runtime: "ios".into(),
        };
        Config {
            dir: state_dir.join("devices"),
            devices: vec![iphone],
            deploy_guard: env_or("LUCINA_DEPLOY_GUARD", format!("{home}/.local/bin/deploy-guard")),
            decide: env_or("LUCINA_DECIDE_BIN", format!("{home}/.local/bin/lucina-rs")),
            install_timeout: Duration::from_secs(50 * 60),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config::from_env(&std::env::temp_dir().join("lucina-agentd"))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DevError {
    #[error("{0}")]
    NotFound(String),
    #[error("{}", .0.join("; "))]
    Refused(Vec<String>),
    #[error("{0}")]
    Busy(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Failed(String),
}

impl DevError {
    pub fn code(&self) -> &'static str {
        match self {
            DevError::NotFound(_) => "DEVICE_NOT_FOUND",
            DevError::Refused(_) => "DEVICE_REFUSED",
            DevError::Busy(_) => "DEVICE_BUSY",
            DevError::Invalid(_) => "DEVICE_INVALID",
            DevError::Failed(_) => "DEVICE_FAILED",
        }
    }
}

type DevResult<T> = Result<T, DevError>;

/// What is on the device, as far as agentd installed it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    pub kind: InstallKind,
    pub commit: String,
    pub holder: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub at: String,
    #[serde(default)]
    pub approval: Option<String>,
    #[serde(default)]
    pub appex: Vec<String>,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceState {
    #[serde(default)]
    installed: Option<Installed>,
    #[serde(default)]
    grants: Vec<Grant>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct InstallRequest {
    pub holder: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    /// A branch build (needs `approval` and the caller's lease).
    #[serde(default)]
    pub test: bool,
    #[serde(default)]
    pub approval: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
    /// In a dry run, build too (still nothing on the device).
    #[serde(default)]
    pub build: bool,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GrantRequest {
    pub holder: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    pub commit: String,
    #[serde(default)]
    pub approval: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GrantDone {
    pub exit: i32,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub dirty: bool,
    #[serde(default)]
    pub appex: Vec<String>,
}

/// Who asks: the session that holds (or wants) the lease, and its workspace if any.
#[derive(Debug, Clone, Copy)]
struct Who<'a> {
    holder: &'a str,
    workspace: Option<&'a str>,
}

#[derive(Clone)]
pub struct Devices {
    cfg: Arc<Config>,
    state: Arc<Mutex<BTreeMap<String, DeviceState>>>,
    /// One install at a time per agentd.
    installing: Arc<tokio::sync::Mutex<()>>,
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn iso(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

fn live(l: &Lease) -> bool {
    chrono::DateTime::parse_from_rfc3339(&l.expires_at).map(|t| t > chrono::Utc::now()).unwrap_or(false)
}

fn holder_of(l: &Lease) -> Holder<'_> {
    Holder { holder: &l.holder, purpose: &l.purpose, expires_at: &l.expires_at }
}

/// Output of a finished command: (success, stdout, stderr).
async fn run(cmd: &mut Command, timeout: Duration) -> DevResult<(bool, String, String)> {
    cmd.kill_on_drop(true).stdin(std::process::Stdio::null());
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| DevError::Failed(format!("timed out after {} s", timeout.as_secs())))?
        .map_err(|e| DevError::Failed(e.to_string()))?;
    Ok((out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned()))
}

async fn git(repo: &Path, args: &[&str]) -> DevResult<String> {
    let (ok, out, err) = run(Command::new("git").arg("-C").arg(repo).args(args), Duration::from_secs(120)).await?;
    if ok {
        Ok(out.trim().to_string())
    } else {
        Err(DevError::Failed(format!("git {}: {}", args.join(" "), err.trim())))
    }
}

fn last_line(s: &str) -> String {
    s.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

impl Devices {
    pub fn open(cfg: Config) -> Self {
        let state = std::fs::read(cfg.dir.join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Devices { cfg: Arc::new(cfg), state: Arc::new(Mutex::new(state)), installing: Arc::new(tokio::sync::Mutex::new(())) }
    }

    fn spec(&self, device: &str) -> DevResult<&DeviceSpec> {
        self.cfg.devices.iter().find(|d| d.name == device).ok_or_else(|| {
            let known: Vec<&str> = self.cfg.devices.iter().map(|d| d.name.as_str()).collect();
            DevError::NotFound(format!("no installer for {device} yet (installers: {})", known.join(", ")))
        })
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut BTreeMap<String, DeviceState>) -> T) -> DevResult<T> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let out = f(&mut state);
        std::fs::create_dir_all(&self.cfg.dir).map_err(|e| DevError::Failed(e.to_string()))?;
        let tmp = self.cfg.dir.join(".state.json.tmp");
        let body = serde_json::to_vec_pretty(&*state).map_err(|e| DevError::Failed(e.to_string()))?;
        std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, self.cfg.dir.join("state.json"))).map_err(|e| DevError::Failed(e.to_string()))?;
        Ok(out)
    }

    fn history(&self, entry: Value) {
        use std::io::Write;
        let _ = std::fs::create_dir_all(&self.cfg.dir);
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(self.cfg.dir.join("history.jsonl")) {
            let _ = writeln!(f, "{entry}");
        }
    }

    /// What agentd last installed on the device (for `lagent device ls` and the board).
    pub fn installed(&self, device: &str) -> Option<Installed> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).get(device).and_then(|s| s.installed.clone())
    }

    pub fn history_tail(&self, device: &str, limit: usize) -> DevResult<Value> {
        self.spec(device)?;
        let text = std::fs::read_to_string(self.cfg.dir.join("history.jsonl")).unwrap_or_default();
        let rows: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v["device"] == device).collect();
        let start = rows.len().saturating_sub(limit);
        Ok(json!({"device": device, "installed": self.installed(device), "history": rows[start..]}))
    }

    /// The clean detached checkout deploy-guard reads, fetched fresh: `(path, tip)`.
    async fn ledger(&self, spec: &DeviceSpec) -> DevResult<(PathBuf, String)> {
        let path = self.cfg.dir.join(format!("{}-ledger", spec.name));
        if git(&path, &["rev-parse", "--git-dir"]).await.is_err() {
            let _ = git(&spec.repo, &["worktree", "prune"]).await;
            std::fs::create_dir_all(&self.cfg.dir).map_err(|e| DevError::Failed(e.to_string()))?;
            let p = path.display().to_string();
            git(&spec.repo, &["worktree", "add", "-q", "--detach", &p, "origin/main"]).await?;
        }
        let tags = format!("+refs/tags/deploy/{0}/*:refs/tags/deploy/{0}/*", spec.runtime);
        git(&path, &["fetch", "-q", "origin", "+refs/heads/main:refs/remotes/origin/main", &tags]).await?;
        let tip = git(&path, &["rev-parse", "origin/main"]).await?;
        Ok((path, tip))
    }

    /// The live build (deploy-guard's ledger entry) is an ancestor of `commit`.
    async fn contains_live(&self, spec: &DeviceSpec, ledger: &Path, commit: &str) -> DevResult<bool> {
        let home = std::env::var("HOME").unwrap_or_default();
        let dir = std::env::var("DEPLOY_GUARD_LEDGER").unwrap_or_else(|_| format!("{home}/.local/state/deploy-guard"));
        let live = std::fs::read_to_string(Path::new(&dir).join(format!("{}.commit", spec.runtime)))
            .ok()
            .and_then(|t| t.lines().find_map(|l| l.strip_prefix("commit ").map(|c| c.trim().to_string())));
        let Some(live) = live else { return Ok(true) };
        let (ok, _, _) = run(Command::new("git").arg("-C").arg(ledger).args(["merge-base", "--is-ancestor", &live, commit]), Duration::from_secs(60)).await?;
        Ok(ok)
    }

    async fn approval(&self, id: &str) -> DevResult<Approval> {
        if !(id.starts_with("dc-") && id.len() == 15 && id[3..].bytes().all(|b| b.is_ascii_hexdigit())) {
            return Err(DevError::Invalid(format!("{id} is not a decision id (dc- and 12 hex digits)")));
        }
        let (ok, out, err) = run(Command::new(&self.cfg.decide).args(["decide", "status", id]), Duration::from_secs(30)).await?;
        if !ok {
            return Err(DevError::Refused(vec![format!("decision {id}: {}", last_line(&err))]));
        }
        let v: Value = serde_json::from_str(&out).map_err(|e| DevError::Failed(format!("decision {id}: {e}")))?;
        let at = v["decided_at"].as_str().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()).map(|t| t.timestamp_millis() as u64).unwrap_or(0);
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        Ok(Approval { id: id.into(), status: s("status"), decision: s("decision"), decided_by: s("decided_by"), decided_at_ms: at, question: s("question") })
    }

    /// Resolve the commit and check everything: `(ledger, tip, commit, approval, take_lease)`.
    async fn check(&self, m: &Manager, spec: &DeviceSpec, kind: InstallKind, who: Who<'_>, rev: Option<&str>, approval: Option<&str>) -> DevResult<(PathBuf, String, String, Option<Approval>, bool)> {
        let Who { holder, workspace } = who;
        if holder.trim().is_empty() {
            return Err(DevError::Invalid("a holder is required (the session id)".into()));
        }
        let (ledger, tip) = self.ledger(spec).await?;
        let commit = match rev {
            Some(r) => git(&ledger, &["rev-parse", "--verify", &format!("{r}^{{commit}}")])
                .await
                .map_err(|_| DevError::Invalid(format!("unknown revision {r}: push it (or fetch it into {})", spec.repo.display())))?,
            None if kind == InstallKind::Main => tip.clone(),
            None => return Err(DevError::Invalid(format!("--commit is required for a {}", kind.noun()))),
        };
        if kind == InstallKind::Main && approval.is_some() {
            return Err(DevError::Invalid("--approval is for test builds (--test)".into()));
        }
        let approval = match approval {
            Some(id) => Some(self.approval(id).await?),
            None => None,
        };
        let contains_live = self.contains_live(spec, &ledger, &commit).await?;
        let lease = m.pipeline().lease(&spec.name).filter(live);
        let facts = InstallFacts {
            kind,
            holder,
            workspace_id: workspace,
            commit: &commit,
            tip: &tip,
            contains_live,
            lease: lease.as_ref().map(holder_of),
            approval: approval.as_ref(),
            now_ms: now_ms(),
        };
        match device::decide_install(&facts) {
            InstallDecision::Refuse(why) => Err(DevError::Refused(why)),
            InstallDecision::Proceed { take_lease } => Ok((ledger, tip, commit, approval, take_lease)),
        }
    }

    fn issue(&self, device: &str, who: Who<'_>, kind: InstallKind, commit: &str, approval: Option<String>, dry_run: bool) -> DevResult<Grant> {
        let Who { holder, workspace } = who;
        let now = now_ms();
        let g = Grant {
            id: format!("gr_{}", &uuid::Uuid::new_v4().simple().to_string()[..16]),
            device: device.into(),
            holder: holder.into(),
            workspace_id: workspace.map(String::from),
            kind,
            commit: commit.into(),
            created_at_ms: now,
            expires_at_ms: now + device::GRANT_TTL_MS,
            state: GrantState::Issued,
            approval,
            dry_run,
        };
        self.with_state(|st| {
            let s = st.entry(device.to_string()).or_default();
            s.grants.retain(|x| x.state == GrantState::Issued && now < x.expires_at_ms);
            s.grants.push(g.clone());
        })?;
        Ok(g)
    }

    fn finish_grant(&self, device: &str, id: &str, to: GrantState) {
        let _ = self.with_state(|st| {
            if let Some(g) = st.get_mut(device).and_then(|s| s.grants.iter_mut().find(|g| g.id == id)) {
                g.state = to;
            }
        });
    }

    /// The executor's check: the grant is live, issued, and its holder holds the device.
    pub fn grant_check(&self, m: &Manager, device: &str, id: &str) -> DevResult<Value> {
        self.spec(device)?;
        let g = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(device)
            .and_then(|s| s.grants.iter().find(|g| g.id == id).cloned())
            .ok_or_else(|| DevError::Refused(vec![format!("no open grant {id} for {device}")]))?;
        let lease = m.pipeline().lease(device).filter(live);
        if let Some(p) = device::grant_problem(&g, lease.as_ref().map(holder_of).as_ref(), now_ms()) {
            return Err(DevError::Refused(vec![p]));
        }
        Ok(json!({"device": device, "grant": g}))
    }

    /// A grant for a phone test run the Mac starts itself (`ios-install exec`).
    pub async fn grant(&self, m: &Manager, device: &str, req: &GrantRequest) -> DevResult<Value> {
        let spec = self.spec(device)?.clone();
        let ws = req.workspace_id.as_deref();
        let who = Who { holder: &req.holder, workspace: ws };
        let (_, tip, commit, approval, _) = self.check(m, &spec, InstallKind::TestRun, who, Some(&req.commit), req.approval.as_deref()).await?;
        let g = self.issue(device, who, InstallKind::TestRun, &commit, approval.map(|a| a.id), false)?;
        self.history(json!({"device": device, "event": "grant", "kind": "test-run", "grant": g.id, "commit": commit, "holder": req.holder, "workspaceId": ws, "approval": g.approval, "at": iso(now_ms())}));
        // Without an approval the run must be main's clean tip: the executor checks the tree.
        Ok(json!({"device": device, "grant": g, "requireCleanTip": g.approval.is_none(), "tip": tip}))
    }

    /// The Mac reports a test run's end: the device now runs that build's test host.
    pub fn grant_done(&self, device: &str, id: &str, done: &GrantDone) -> DevResult<Value> {
        self.spec(device)?;
        let now = now_ms();
        let g = self.with_state(|st| {
            let s = st.entry(device.to_string()).or_default();
            let g = s.grants.iter_mut().find(|g| g.id == id && g.state == GrantState::Issued)?;
            g.state = if done.exit == 0 { GrantState::Done } else { GrantState::Failed };
            let g = g.clone();
            s.installed = Some(Installed {
                kind: g.kind,
                commit: g.commit.clone(),
                holder: g.holder.clone(),
                workspace_id: g.workspace_id.clone(),
                at: iso(now),
                approval: g.approval.clone(),
                appex: done.appex.clone(),
                note: format!("{} (exit {}{})", done.command, done.exit, if done.dirty { ", uncommitted changes" } else { "" }),
            });
            Some(g)
        })?;
        let g = g.ok_or_else(|| DevError::Refused(vec![format!("no open grant {id}")]))?;
        self.history(json!({"device": device, "event": "test-run", "kind": "test-run", "grant": id, "commit": g.commit, "holder": g.holder, "workspaceId": g.workspace_id, "exit": done.exit, "dirty": done.dirty, "command": done.command, "approval": g.approval, "at": iso(now)}));
        Ok(json!({"device": device, "recorded": true}))
    }

    /// Check, build on the Mac and install; a main install is recorded with deploy-guard.
    pub async fn install(&self, m: &Manager, device: &str, req: &InstallRequest) -> DevResult<Value> {
        let spec = self.spec(device)?.clone();
        let _one = self.installing.try_lock().map_err(|_| DevError::Busy(format!("another install on {device} is running")))?;
        let kind = if req.test { InstallKind::Test } else { InstallKind::Main };
        let ws = req.workspace_id.as_deref();
        let who = Who { holder: &req.holder, workspace: ws };
        let (ledger, tip, commit, approval, take_lease) = self.check(m, &spec, kind, who, req.commit.as_deref(), req.approval.as_deref()).await?;
        if kind == InstallKind::Main {
            git(&ledger, &["checkout", "-q", "--detach", "--force", &commit]).await?;
            // Not inside the merge lock: an install never pushes main, and merges hold that
            // lock through whole builds; `installing` orders this daemon's installs.
            let (ok, out, err) = run(
                Command::new(&self.cfg.deploy_guard).args(["check", "--runtime", &spec.runtime, "--no-lock-check"]).arg("--repo").arg(&ledger),
                Duration::from_secs(120),
            )
            .await?;
            if !ok {
                let mut why = vec![format!("deploy-guard check refused {}", short(&commit))];
                why.extend(format!("{out}\n{err}").lines().filter(|l| l.trim_start().starts_with("- ")).map(|l| l.trim().to_string()));
                return Err(DevError::Refused(why));
            }
        }
        let took = take_lease && !req.dry_run;
        if took {
            let lease = LeaseRequest {
                holder: req.holder.clone(),
                purpose: Some(format!("{} {} (lagent device install)", kind.noun(), short(&commit))),
                minutes: Some(device::AUTO_LEASE_MIN),
                workspace_id: req.workspace_id.clone(),
            };
            m.lease_device(device, lease).map_err(|e| DevError::Refused(vec![e.to_string()]))?;
        }
        let g = self.issue(device, who, kind, &commit, approval.as_ref().map(|a| a.id.clone()), req.dry_run)?;
        let result = self.execute(&spec, &g, req.build).await;
        if took {
            let _ = m.release_device(device, ReleaseRequest { holder: req.holder.clone(), force: false, reason: None });
        }
        let (log, outcome) = match result {
            Ok(r) => r,
            Err(e) => {
                self.finish_grant(device, &g.id, GrantState::Failed);
                return Err(e);
            }
        };
        self.finish_grant(device, &g.id, GrantState::Done);
        let appex: Vec<String> = outcome["appex"].as_str().unwrap_or_default().split(',').filter(|s| !s.is_empty()).map(String::from).collect();
        if req.dry_run {
            return Ok(json!({"device": device, "dryRun": true, "kind": kind.as_str(), "commit": commit, "tip": tip, "log": log, "executor": outcome}));
        }
        let note = req.note.clone().unwrap_or_else(|| match kind {
            InstallKind::Main => format!("Main {} on the {} (Release, devicectl)", short(&commit), spec.label),
            _ => format!("test build {} ({})", short(&commit), approval.as_ref().map(|a| a.id.as_str()).unwrap_or("")),
        });
        let note = format!("{note} [lagent device install by {}]", req.holder);
        let installed = Installed {
            kind,
            commit: commit.clone(),
            holder: req.holder.clone(),
            workspace_id: req.workspace_id.clone(),
            at: iso(now_ms()),
            approval: approval.as_ref().map(|a| a.id.clone()),
            appex: appex.clone(),
            note: note.clone(),
        };
        self.with_state(|st| st.entry(device.to_string()).or_default().installed = Some(installed))?;
        self.history(json!({"device": device, "event": "install", "kind": kind.as_str(), "commit": commit, "holder": req.holder, "workspaceId": ws, "approval": approval.as_ref().map(|a| &a.id), "appex": appex, "log": log, "at": iso(now_ms())}));
        let mut recorded = Value::Null;
        if kind == InstallKind::Main {
            git(&ledger, &["checkout", "-q", "--detach", "--force", &commit]).await?;
            let (ok, out, err) = run(
                Command::new(&self.cfg.deploy_guard)
                    .args(["record", "--runtime", &spec.runtime, "--commit", &commit, "--note", &note])
                    .arg("--repo")
                    .arg(&ledger),
                Duration::from_secs(120),
            )
            .await?;
            if !ok {
                return Err(DevError::Failed(format!(
                    "installed {} but deploy-guard record failed ({}); record it: deploy-guard record --runtime {} --repo {} --commit {commit}",
                    short(&commit),
                    last_line(&err),
                    spec.runtime,
                    ledger.display()
                )));
            }
            recorded = json!(out.trim());
        }
        Ok(json!({"device": device, "dryRun": false, "kind": kind.as_str(), "commit": commit, "tip": tip, "appex": appex, "log": log, "deployGuard": recorded}))
    }

    /// Run the executor on the first reachable host; its last `IOS_INSTALL_RESULT {json}`
    /// line is the outcome. `(log path, outcome)`.
    async fn execute(&self, spec: &DeviceSpec, g: &Grant, build: bool) -> DevResult<(String, Value)> {
        let logs = self.cfg.dir.join("logs");
        std::fs::create_dir_all(&logs).map_err(|e| DevError::Failed(e.to_string()))?;
        let log = logs.join(format!("{}-{}-{}.log", chrono::Utc::now().format("%Y%m%dT%H%M%SZ"), short(&g.commit), g.kind.as_str()));
        let socket = crate::config::default_socket().display().to_string();
        // The executor checks its grant with the `lagent` next to this daemon (a dev build
        // answers for a dev daemon).
        let lagent = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("lagent")))
            .filter(|p| p.exists())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "~/.local/bin/lagent".into());
        let mut remote = format!(
            "{} executor --grant {} --commit {} --kind {} --agentd-socket {} --lagent {}",
            spec.executor,
            g.id,
            g.commit,
            g.kind.as_str(),
            shell_quote(&socket),
            shell_quote(&lagent)
        );
        if g.dry_run {
            remote.push_str(" --dry-run");
            if build {
                remote.push_str(" --build");
            }
        }
        let mut transcript = String::new();
        for host in &spec.hosts {
            // `local`: the device hangs off this host (and the tests use it).
            let mut cmd = if host == "local" {
                let mut c = Command::new("sh");
                c.args(["-c", &remote]);
                c
            } else {
                let mut c = Command::new("ssh");
                c.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=8", "-o", "ServerAliveInterval=30", host, &remote]);
                c
            };
            let (ok, out, err) = run(&mut cmd, self.cfg.install_timeout).await?;
            transcript.push_str(&format!("== {host}: {remote}\n{out}\n{err}\n"));
            let _ = std::fs::write(&log, &transcript);
            let unreachable = !ok && out.is_empty() && ["Could not resolve", "Connection refused", "timed out", "No route"].iter().any(|s| err.contains(s));
            if unreachable {
                continue;
            }
            let outcome = out.lines().rev().find_map(|l| l.strip_prefix("IOS_INSTALL_RESULT ")).and_then(|j| serde_json::from_str::<Value>(j).ok());
            return match (ok, outcome) {
                (true, Some(v)) if v["ok"] == json!(true) => Ok((log.display().to_string(), v)),
                (_, Some(v)) => Err(DevError::Refused(vec![format!("executor on {host}: {}", v["error"].as_str().unwrap_or("failed")), format!("log {}", log.display())])),
                (_, None) => Err(DevError::Failed(format!("executor on {host} failed: {} (log {})", last_line(&format!("{out}\n{err}")), log.display()))),
            };
        }
        Err(DevError::Failed(format!("no host of {} is reachable ({}); log {}", spec.name, spec.hosts.join(", "), log.display())))
    }
}
