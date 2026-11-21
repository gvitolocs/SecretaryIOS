//! Projects and workspaces (Paseo `workspace-registry*.js`, `worktree-*.js`; design:
//! "Workspaces and worktrees").
//!
//! - A project is a directory the owner works in; a workspace is one place an agent
//!   runs: a plain directory, a main checkout, or a worktree. One directory can back
//!   several workspaces (Paseo's "Model B"), so every bare `lagent run` gets its own.
//! - Worktrees are made with the owner's `wt add <branch> [base]`, so they live in
//!   `~/Projects/.worktrees/<Repo>/<branch with / as ->`, never next to the repo and
//!   never in `~/.paseo`. Modes: branch-off (a new branch from a base), checkout-branch
//!   (an existing branch, or a copy of it when it is checked out elsewhere), checkout-pr
//!   (a GitHub pull request; a fork's is untrusted and its scripts wait for the owner).
//! - `paseo.json` `worktree.setup` runs in the background after a worktree is made;
//!   `worktree.teardown` runs before it is archived.
//! - Archive and the merge sweep remove a worktree only as `worktree_guard::removal`
//!   allows: agentd made it, it is clean, nothing runs in it; the branch goes only when
//!   it is merged (`git branch -d`). The sweep also requires that the branch moved past
//!   the commit it started from, so a fresh worktree is never taken for a merged one.

pub mod checkout;
pub mod files;
pub mod git;
mod names;
pub mod procs;
pub mod setup;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use lucina_agents::branch_slug::{slugify, valid_git_branch};
use lucina_agents::workspace::{
    AutoName, ProjectRecord, SetupSnapshot, UntrustedSource, WorkspaceKind, WorkspaceRecord,
};
use lucina_agents::worktree_guard::{self as guard, RemoveFacts, StartFacts, Trigger};
use serde::Deserialize;

use crate::now_iso;
use crate::store::{Store, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum WsError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Git(String),
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

/// Where worktrees go, and the owner's repo rules.
#[derive(Debug, Clone)]
pub struct Config {
    /// The `wt` script (`LUCINA_AGENTD_WT`, default `~/.local/bin/wt`).
    pub wt: Option<PathBuf>,
    /// `$WT_ROOT`, default `~/Projects/.worktrees`.
    pub wt_root: PathBuf,
    /// `$WT_PROJECTS`, default `~/Projects`: main checkouts here are protected.
    pub projects_root: PathBuf,
    /// Repos where Codex works only on `codex/` branches from `codex_base`
    /// (`LUCINA_AGENTD_CODEX_REPOS`, comma-separated basenames; default `SecretaryIOS`).
    pub codex_repos: Vec<String>,
    pub codex_base: String,
    /// More repo basenames where Codex is refused outright, beside every Lucina project (the owner's rule;
    /// `LUCINA_AGENTD_CODEX_DENY_REPOS`, comma-separated). See [`lucina_repo_name`].
    pub codex_deny_repos: Vec<String>,
    /// The merge sweep leaves a worktree git touched more recently than this
    /// (`LUCINA_AGENTD_SWEEP_IDLE_SECS`, default 30 minutes).
    pub sweep_idle_secs: u64,
}

impl Config {
    pub fn from_env() -> Self {
        let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
        let projects_root = std::env::var_os("WT_PROJECTS")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("Projects"));
        let wt_root = std::env::var_os("WT_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| projects_root.join(".worktrees"));
        let wt = match std::env::var("LUCINA_AGENTD_WT") {
            Ok(p) if !p.is_empty() => crate::process::resolve_program(&p),
            _ => crate::process::resolve_program("~/.local/bin/wt")
                .or_else(|| crate::process::resolve_program("wt")),
        };
        let codex_repos = std::env::var("LUCINA_AGENTD_CODEX_REPOS")
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_else(|_| vec!["SecretaryIOS".to_string()]);
        let codex_deny_repos = std::env::var("LUCINA_AGENTD_CODEX_DENY_REPOS")
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let sweep_idle_secs = std::env::var("LUCINA_AGENTD_SWEEP_IDLE_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1800);
        Config {
            wt,
            wt_root,
            projects_root,
            codex_repos,
            codex_base: "lucina-rs".into(),
            codex_deny_repos,
            sweep_idle_secs,
        }
    }
}

/// The immediate child of `base` that `path` sits in, when `path` is under it. A path equal to `base` is not under it.
fn child_of(path: &Path, base: &Path) -> Option<PathBuf> {
    let rel = path.strip_prefix(base).ok()?;
    let first = rel.components().next()?;
    Some(base.join(first))
}

/// The repository a git common dir names: the parent of a `.git` directory, or the dir itself (a bare repo).
fn repo_of_common_dir(common: &Path) -> PathBuf {
    if common.file_name().is_some_and(|n| n == ".git") {
        common
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| common.to_path_buf())
    } else {
        common.to_path_buf()
    }
}

/// `git -C dir rev-parse --path-format=absolute --git-common-dir`, trimmed; `None` when it fails (not a repo, no git).
/// Synchronous, like this module's `canon`.
fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "core.quotePath=false",
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        ])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// The Lucina project's directory name when `cwd` is one, else `None`. The owner's rule: Codex is not used on Lucina
/// projects. A Lucina project is a checkout under `<projects_root>/Lucina/`, a worktree under `<wt_root>/<Repo>/` where
/// `<projects_root>/Lucina/<Repo>` exists, or any checkout whose repo is under `<projects_root>/Lucina/`; plus the
/// basenames in `codex_deny_repos` (a worktree under `<wt_root>/<name>/`, or a repo named `<name>`).
pub fn lucina_repo_name(cfg: &Config, cwd: &Path) -> Option<String> {
    let name_of = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_string());
    let cwd = git::canon(cwd);
    let lucina = git::canon(&git::canon(&cfg.projects_root).join("Lucina"));
    let wt_child = child_of(&cwd, &git::canon(&cfg.wt_root)).and_then(|r| name_of(&r));
    if let Some(repo) = child_of(&cwd, &lucina) {
        return name_of(&repo);
    }
    if let Some(name) = &wt_child {
        if lucina.join(name).is_dir() {
            return Some(name.clone());
        }
    }
    if let Some(common) = git_common_dir(&cwd) {
        let repo = repo_of_common_dir(&common);
        if let Some(under) = child_of(&repo, &lucina) {
            return name_of(&under);
        }
        if let Some(name) = name_of(&repo).filter(|n| cfg.codex_deny_repos.contains(n)) {
            return Some(name);
        }
    }
    wt_child.filter(|n| cfg.codex_deny_repos.contains(n))
}

/// Whether `cwd` is a Lucina project (see [`lucina_repo_name`]).
pub fn is_lucina_project(cfg: &Config, cwd: &Path) -> bool {
    lucina_repo_name(cfg, cwd).is_some()
}

/// What a worktree's checkout and processes show, for the merge sweep.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepFacts {
    pub in_progress: Option<&'static str>,
    pub detached: bool,
    pub dirty: bool,
    pub idle_secs: Option<u64>,
    pub procs: u32,
}

/// Why the merge sweep must leave a worktree alone, if it must. During a rebase HEAD sits on
/// the base's tip, so a merged-looking HEAD proves nothing while something is under way or
/// the checkout was just used.
pub fn sweep_blocker(f: &SweepFacts, min_idle_secs: u64) -> Option<&'static str> {
    if f.in_progress.is_some() {
        Some("an operation is in progress")
    } else if f.detached {
        Some("detached HEAD")
    } else if f.dirty {
        Some("uncommitted changes")
    } else if f.procs > 0 {
        Some("a process runs in it")
    } else if f.idle_secs.is_none_or(|s| s < min_idle_secs) {
        Some("used recently")
    } else {
        None
    }
}

/// `POST /workspaces` (Paseo `workspace.create.request` through `workspace create`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateWorkspace {
    /// `local` or `worktree`.
    pub isolation: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// `branch-off` (default), `checkout-branch`, `checkout-pr`.
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub worktree_slug: Option<String>,
    #[serde(default)]
    pub new_branch: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub pr_number: Option<u64>,
    #[serde(default)]
    pub forge: Option<String>,
    #[serde(default)]
    pub background: bool,
    /// The first agent's prompt (Lucina): the branch is named after it, and the title
    /// and branch are generated from it.
    #[serde(default)]
    pub prompt: Option<String>,
    /// The first agent's provider (Lucina): Codex gets `codex/` from `lucina-rs` in the
    /// repos where the owner's Codex rule applies.
    #[serde(default)]
    pub provider: Option<String>,
}

#[derive(Default)]
struct State {
    projects: BTreeMap<String, ProjectRecord>,
    workspaces: BTreeMap<String, WorkspaceRecord>,
}

pub type Publish = Arc<dyn Fn(&WorkspaceRecord) + Send + Sync>;

#[derive(Clone)]
pub struct Registry {
    inner: Arc<Inner>,
}

struct Inner {
    store: Arc<Store>,
    config: Config,
    state: Mutex<State>,
    publish: Publish,
    pub tokens: files::Tokens,
}

fn new_id(prefix: &str) -> String {
    format!(
        "{prefix}_{}",
        &uuid::Uuid::new_v4().simple().to_string()[..16]
    )
}

fn basename(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| p.display().to_string())
}

fn free_port() -> Option<u16> {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()?
        .local_addr()
        .ok()
        .map(|a| a.port())
}

impl Registry {
    pub fn open(store: Arc<Store>, config: Config, publish: Publish) -> Result<Self, WsError> {
        let mut state = State::default();
        for p in store.load_projects()? {
            state.projects.insert(p.project_id.clone(), p);
        }
        for w in store.load_workspaces()? {
            state.workspaces.insert(w.workspace_id.clone(), w);
        }
        Ok(Registry {
            inner: Arc::new(Inner {
                store,
                config,
                state: Mutex::new(state),
                publish,
                tokens: files::Tokens::default(),
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn config(&self) -> &Config {
        &self.inner.config
    }

    pub fn tokens(&self) -> &files::Tokens {
        &self.inner.tokens
    }

    fn save(&self, w: &WorkspaceRecord) {
        if let Err(e) = self.inner.store.save_workspace(w) {
            tracing::error!(workspace = %w.workspace_id, error = %e, "failed to save workspace");
        }
        (self.inner.publish)(w);
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut WorkspaceRecord)) -> Option<WorkspaceRecord> {
        let w = {
            let mut st = self.lock();
            let w = st.workspaces.get_mut(id)?;
            f(w);
            w.updated_at = now_iso();
            w.clone()
        };
        self.save(&w);
        Some(w)
    }

    // -- projects ------------------------------------------------------------

    /// The project rooted at `root`, made on first use.
    fn project_for(&self, root: &Path, is_git: bool) -> Result<ProjectRecord, WsError> {
        let root_s = root.display().to_string();
        if let Some(p) = self
            .lock()
            .projects
            .values()
            .find(|p| p.root_path == root_s && p.archived_at.is_none())
        {
            return Ok(p.clone());
        }
        let now = now_iso();
        let p = ProjectRecord {
            project_id: new_id("prj"),
            root_path: root_s,
            kind: if is_git { "git" } else { "non_git" }.into(),
            display_name: basename(root),
            custom_name: None,
            created_at: now.clone(),
            updated_at: now,
            archived_at: None,
        };
        self.inner.store.save_project(&p)?;
        self.lock().projects.insert(p.project_id.clone(), p.clone());
        Ok(p)
    }

    pub async fn create_project(&self, path: &str) -> Result<ProjectRecord, WsError> {
        let dir = PathBuf::from(path);
        if !dir.is_dir() {
            return Err(WsError::Invalid(format!("Not a directory: {path}")));
        }
        let info = git::info(&dir).await;
        self.project_for(&git::canon(&dir), info.is_git)
    }

    pub fn projects(&self) -> Vec<ProjectRecord> {
        self.lock()
            .projects
            .values()
            .filter(|p| p.archived_at.is_none())
            .cloned()
            .collect()
    }

    pub fn project(&self, id: &str) -> Option<ProjectRecord> {
        self.lock().projects.get(id).cloned()
    }

    pub fn rename_project(&self, id: &str, name: Option<String>) -> Result<ProjectRecord, WsError> {
        let p = {
            let mut st = self.lock();
            let p = st
                .projects
                .get_mut(id)
                .filter(|p| p.archived_at.is_none())
                .ok_or_else(|| WsError::NotFound("Project not found".into()))?;
            p.custom_name = name;
            p.updated_at = now_iso();
            p.clone()
        };
        self.inner.store.save_project(&p)?;
        Ok(p)
    }

    /// A project's checkout moved on disk (renamed, regrouped): point the project at its new
    /// path, and its workspaces there too: those under the old checkout, and those under
    /// `<worktrees root>/<old name>` when that became `<worktrees root>/<new name>`. Nothing
    /// on disk changes, and a workspace whose new directory doesn't exist keeps its old path
    /// (returned as skipped). Agents already running keep their cwd: leave a symlink at the old
    /// path while any runs. Returns the project, `(workspace, new cwd)` and `(workspace, old cwd)`.
    #[allow(clippy::type_complexity)]
    pub fn move_project(
        &self,
        id: &str,
        path: &Path,
        name: Option<String>,
    ) -> Result<(ProjectRecord, Vec<(String, String)>, Vec<(String, String)>), WsError> {
        if !path.is_dir() {
            return Err(WsError::Invalid(format!(
                "Not a directory: {}",
                path.display()
            )));
        }
        let new_root = git::canon(path);
        let new_root_s = new_root.display().to_string();
        let old = self
            .project(id)
            .filter(|p| p.archived_at.is_none())
            .ok_or_else(|| WsError::NotFound(format!("Project not found: {id}")))?;
        let old_root = PathBuf::from(&old.root_path);
        if self
            .lock()
            .projects
            .values()
            .any(|p| p.project_id != id && p.root_path == new_root_s && p.archived_at.is_none())
        {
            return Err(WsError::Invalid(format!(
                "Another project is at {new_root_s}"
            )));
        }
        let wt_root = &self.inner.config.wt_root;
        let (old_wt, new_wt) = (
            wt_root.join(basename(&old_root)),
            wt_root.join(basename(&new_root)),
        );
        let moved_path = |p: &str| -> Option<String> {
            let p = Path::new(p);
            let (from, to) = if p.starts_with(&old_root) {
                (&old_root, &new_root)
            } else if p.starts_with(&old_wt) {
                (&old_wt, &new_wt)
            } else {
                return None;
            };
            let rest = p.strip_prefix(from).ok()?;
            Some(if rest.as_os_str().is_empty() {
                to.display().to_string()
            } else {
                to.join(rest).display().to_string()
            })
        };
        let list: Vec<WorkspaceRecord> = self
            .lock()
            .workspaces
            .values()
            .filter(|w| w.project_id == id && w.archived_at.is_none())
            .cloned()
            .collect();
        let (mut moved, mut skipped) = (Vec::new(), Vec::new());
        for w in list {
            let Some(cwd) = moved_path(&w.cwd) else {
                continue;
            };
            if cwd == w.cwd {
                continue;
            }
            if !Path::new(&cwd).is_dir() {
                skipped.push((w.workspace_id, w.cwd));
                continue;
            }
            self.update(&w.workspace_id, |r| {
                r.cwd = cwd.clone();
                r.main_repo_root = r
                    .main_repo_root
                    .as_deref()
                    .map(|m| moved_path(m).unwrap_or_else(|| m.to_string()));
            });
            moved.push((w.workspace_id, cwd));
        }
        let p = {
            let mut st = self.lock();
            let p = st
                .projects
                .get_mut(id)
                .ok_or_else(|| WsError::NotFound(format!("Project not found: {id}")))?;
            p.root_path = new_root_s;
            p.display_name = basename(&new_root);
            if name.is_some() {
                p.custom_name = name;
            }
            p.updated_at = now_iso();
            p.clone()
        };
        self.inner.store.save_project(&p)?;
        tracing::info!(project = id, from = %old.root_path, to = %p.root_path, moved = moved.len(), skipped = skipped.len(), "project moved");
        Ok((p, moved, skipped))
    }

    /// Remove a project; returns its active workspaces (the caller archives them).
    pub fn delete_project(&self, id: &str) -> Result<Vec<String>, WsError> {
        let ids: Vec<String> = {
            let mut st = self.lock();
            if !st.projects.contains_key(id) {
                return Err(WsError::NotFound(format!("Project not found: {id}")));
            }
            st.projects.remove(id);
            st.workspaces
                .values()
                .filter(|w| w.project_id == id && w.archived_at.is_none())
                .map(|w| w.workspace_id.clone())
                .collect()
        };
        self.inner.store.delete_project(id)?;
        Ok(ids)
    }

    // -- workspaces ----------------------------------------------------------

    pub fn get(&self, id: &str) -> Option<WorkspaceRecord> {
        self.lock().workspaces.get(id).cloned()
    }

    pub fn project_name(&self, w: &WorkspaceRecord) -> String {
        self.lock()
            .projects
            .get(&w.project_id)
            .map(|p| p.name().to_string())
            .unwrap_or_default()
    }

    /// Active workspaces, by id (Paseo `fetch_workspaces_request` without a sort).
    pub fn list(&self, include_background: bool) -> Vec<WorkspaceRecord> {
        self.lock()
            .workspaces
            .values()
            .filter(|w| w.archived_at.is_none() && (include_background || !w.background))
            .cloned()
            .collect()
    }

    pub fn rename(&self, id: &str, title: Option<String>) -> Result<WorkspaceRecord, WsError> {
        if self.get(id).is_none_or(|w| w.archived_at.is_some()) {
            return Err(WsError::NotFound("Workspace not found".into()));
        }
        if title
            .as_deref()
            .is_some_and(crate::manager::pipeline::poisoned_title)
        {
            return Err(WsError::Invalid("A title that starts with \"Error:\" is refused (typically a failed command substituted into it)".into()));
        }
        self.update(id, |w| w.title = title.filter(|t| !t.trim().is_empty()))
            .ok_or_else(|| WsError::NotFound("Workspace not found".into()))
    }

    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<WorkspaceRecord, WsError> {
        self.update(id, |w| w.pinned_at = pinned.then(now_iso))
            .ok_or_else(|| WsError::NotFound("Workspace not found".into()))
    }

    /// Paseo label keys: whitespace collapsed, lowercased.
    pub fn set_label(
        &self,
        id: &str,
        label: &str,
        assigned: bool,
    ) -> Result<WorkspaceRecord, WsError> {
        let key = label
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if key.is_empty() || key.chars().count() > 40 {
            return Err(WsError::Invalid("label: 1 to 40 characters".into()));
        }
        self.update(id, |w| {
            w.labels.retain(|l| *l != key);
            if assigned {
                w.labels.push(key.clone());
                w.labels.sort();
            }
        })
        .ok_or_else(|| WsError::NotFound("Workspace not found".into()))
    }

    /// The branch name of a new worktree, from what the caller gave.
    async fn branch_off_name(
        &self,
        repo: &Path,
        req: &CreateWorkspace,
        codex: bool,
    ) -> Result<String, WsError> {
        let prefix = if codex { "codex/" } else { "agent/" };
        let name = match (&req.new_branch, &req.worktree_slug, &req.prompt) {
            (Some(b), _, _) => b.trim().to_string(),
            (None, Some(s), _) => s.trim().to_string(),
            (None, None, Some(p)) if !slugify(p).is_empty() => format!("{prefix}{}", slugify(p)),
            _ => names::random(),
        };
        let name = if codex && !name.starts_with("codex/") {
            format!("codex/{}", name.trim_start_matches("agent/"))
        } else {
            name
        };
        if !valid_git_branch(&name) {
            return Err(WsError::Invalid(format!("Invalid branch name: {name}")));
        }
        self.unique_branch(repo, &name).await
    }

    /// `name`, else `name-1`, `name-2`, ...: no such branch and no such worktree path.
    async fn unique_branch(&self, repo: &Path, name: &str) -> Result<String, WsError> {
        let dir = self.inner.config.wt_root.join(basename(repo));
        for n in 0..100 {
            let candidate = if n == 0 {
                name.to_string()
            } else {
                format!("{name}-{n}")
            };
            if !git::branch_exists(repo, &candidate).await
                && !dir.join(candidate.replace('/', "-")).exists()
            {
                return Ok(candidate);
            }
        }
        Err(WsError::Invalid(format!("No free branch name like {name}")))
    }

    /// `wt add <branch> [base]` in the main checkout; returns the new worktree.
    async fn wt_add(
        &self,
        repo: &Path,
        branch: &str,
        base: Option<&str>,
    ) -> Result<PathBuf, WsError> {
        let cfg = &self.inner.config;
        let wt = cfg
            .wt
            .clone()
            .ok_or_else(|| WsError::Git("wt is not installed (~/.local/bin/wt)".into()))?;
        let mut cmd = tokio::process::Command::new(wt);
        cmd.arg("add").arg(branch);
        if let Some(b) = base {
            cmd.arg(b);
        }
        let out = cmd
            .current_dir(repo)
            .env("WT_ROOT", &cfg.wt_root)
            .env("WT_PROJECTS", &cfg.projects_root)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| WsError::Git(format!("cannot run wt: {e}")))?;
        if !out.status.success() {
            return Err(WsError::Git(
                String::from_utf8_lossy(&out.stderr)
                    .trim()
                    .trim_start_matches("wt: ")
                    .to_string(),
            ));
        }
        let path = String::from_utf8_lossy(&out.stdout)
            .lines()
            .last()
            .unwrap_or_default()
            .trim()
            .to_string();
        let path = PathBuf::from(path);
        let expected = git::canon(&cfg.wt_root.join(basename(repo)));
        if !git::canon(&path).starts_with(&expected) {
            return Err(WsError::Git(format!(
                "wt made {} outside {}",
                path.display(),
                expected.display()
            )));
        }
        Ok(path)
    }

    /// Create a workspace; returns it and why setup didn't run, if it didn't.
    pub async fn create(
        &self,
        req: CreateWorkspace,
    ) -> Result<(WorkspaceRecord, Option<String>), WsError> {
        let path = req.path.clone().or_else(|| {
            req.project_id
                .as_ref()
                .and_then(|p| self.project(p))
                .map(|p| p.root_path)
        });
        let path = PathBuf::from(path.ok_or_else(|| WsError::Invalid("path is required".into()))?);
        if !path.is_dir() {
            return Err(WsError::Invalid(format!(
                "Not a directory: {}",
                path.display()
            )));
        }
        let path = git::canon(&path);
        let info = git::info(&path).await;
        let now = now_iso();
        match req.isolation.as_str() {
            "local" => {
                if req.mode.is_some()
                    || req.worktree_slug.is_some()
                    || req.new_branch.is_some()
                    || req.base.is_some()
                    || req.branch.is_some()
                    || req.pr_number.is_some()
                    || req.forge.is_some()
                {
                    return Err(WsError::Invalid(
                        "Worktree options require --isolation worktree".into(),
                    ));
                }
                let project = match &req.project_id {
                    Some(id) => self
                        .project(id)
                        .ok_or_else(|| WsError::NotFound(format!("Project not found: {id}")))?,
                    None => self.project_for(&path, info.is_git)?,
                };
                let w = WorkspaceRecord {
                    workspace_id: new_id("wks"),
                    project_id: project.project_id,
                    cwd: path.display().to_string(),
                    kind: info.kind(),
                    display_name: info.branch.clone().unwrap_or_else(|| basename(&path)),
                    title: req.title.clone().filter(|t| !t.trim().is_empty()),
                    branch: info.branch.clone(),
                    base_ref: None,
                    base_sha: None,
                    main_repo_root: info.main_root.as_ref().map(|p| p.display().to_string()),
                    owned_worktree: false,
                    created_at: now.clone(),
                    updated_at: now,
                    archived_at: None,
                    pinned_at: None,
                    labels: Vec::new(),
                    untrusted_source: None,
                    background: req.background,
                    setup: None,
                    port: None,
                    auto_name: None,
                    removed_reason: None,
                };
                self.lock()
                    .workspaces
                    .insert(w.workspace_id.clone(), w.clone());
                self.save(&w);
                Ok((w, None))
            }
            "worktree" => self.create_worktree(req, &path, info).await,
            other => Err(WsError::Invalid(format!(
                "Unsupported workspace isolation: {other}"
            ))),
        }
    }

    async fn create_worktree(
        &self,
        req: CreateWorkspace,
        path: &Path,
        info: git::Info,
    ) -> Result<(WorkspaceRecord, Option<String>), WsError> {
        if !info.is_git {
            return Err(WsError::Invalid(
                "Create worktree requires a git repository".into(),
            ));
        }
        let repo = info
            .repo_root()
            .map(Path::to_path_buf)
            .ok_or_else(|| WsError::Invalid("Create worktree requires a git repository".into()))?;
        let project = match &req.project_id {
            Some(id) => self
                .project(id)
                .ok_or_else(|| WsError::NotFound(format!("Project not found: {id}")))?,
            None => self.project_for(path, true)?,
        };
        let cfg = &self.inner.config;
        let codex = req
            .provider
            .as_deref()
            .is_some_and(|p| p == "codex" || p.starts_with("codex/"))
            && cfg.codex_repos.contains(&basename(&repo));
        let default_base = git::default_branch(&repo).await;
        let mut untrusted = None;
        let (branch, base_spec): (String, Option<String>) = match req
            .mode
            .as_deref()
            .unwrap_or("branch-off")
        {
            "branch-off" => {
                if req.branch.is_some() || req.pr_number.is_some() || req.forge.is_some() {
                    return Err(WsError::Invalid(
                        "--branch, --pr-number, and --forge require a checkout mode".into(),
                    ));
                }
                let base = match (&req.base, codex) {
                    (Some(b), _) => Some(b.clone()),
                    (None, true) => Some(cfg.codex_base.clone()),
                    (None, false) => default_base.clone(),
                };
                if let Some(b) = &base {
                    if git::resolve_ref(&repo, b).await.is_none() {
                        return Err(WsError::Invalid(format!("Unknown base: {b}")));
                    }
                }
                (self.branch_off_name(&repo, &req, codex).await?, base)
            }
            "checkout-branch" => {
                let b = req.branch.clone().ok_or_else(|| {
                    WsError::Invalid("--branch is required for --mode checkout-branch".into())
                })?;
                if req.new_branch.is_some()
                    || req.base.is_some()
                    || req.pr_number.is_some()
                    || req.forge.is_some()
                {
                    return Err(WsError::Invalid("--new-branch, --base, --pr-number, and --forge are not valid for --mode checkout-branch".into()));
                }
                let local = git::branch_exists(&repo, &b).await;
                let remote = git::read(
                    &repo,
                    &[
                        "show-ref",
                        "--verify",
                        "--quiet",
                        &format!("refs/remotes/origin/{b}"),
                    ],
                )
                .await
                .is_some();
                if !local && !remote {
                    return Err(WsError::Invalid(format!("Unknown branch: {b}")));
                }
                let busy = git::worktrees(&repo)
                    .await
                    .into_iter()
                    .any(|(_, br)| br.as_deref() == Some(b.as_str()));
                if busy {
                    // Checked out elsewhere: a copy of it (Paseo does the same).
                    (self.unique_branch(&repo, &b).await?, Some(b.clone()))
                } else if local {
                    (b, None)
                } else {
                    (b.clone(), Some(format!("origin/{b}")))
                }
            }
            "checkout-pr" => {
                let n = req.pr_number.filter(|n| *n > 0).ok_or_else(|| {
                    WsError::Invalid("--pr-number is required for --mode checkout-pr".into())
                })?;
                if req.new_branch.is_some() || req.base.is_some() || req.branch.is_some() {
                    return Err(WsError::Invalid(
                        "--new-branch, --base, and --branch are not valid for --mode checkout-pr"
                            .into(),
                    ));
                }
                if req.forge.as_deref().is_some_and(|f| f != "github") {
                    return Err(WsError::Invalid(
                        "Only GitHub pull requests are supported (--forge github)".into(),
                    ));
                }
                let pr = checkout::pr_head(&repo, n).await.map_err(WsError::Git)?;
                let head = pr["headRefName"].as_str().unwrap_or("pr").to_string();
                let local = self.unique_branch(&repo, &head).await?;
                git::must(
                    &repo,
                    &["fetch", "origin", &format!("pull/{n}/head:{local}")],
                )
                .await
                .map_err(WsError::Git)?;
                if pr["isCrossRepository"] == true {
                    let owner = pr["headRepositoryOwner"]["login"].as_str().unwrap_or("?");
                    let name = pr["headRepository"]["name"].as_str().unwrap_or("?");
                    untrusted = Some(UntrustedSource {
                        kind: "change_request".into(),
                        forge: "github".into(),
                        number: n,
                        head_repository: format!("{owner}/{name}"),
                    });
                }
                (local, None)
            }
            other => {
                return Err(WsError::Invalid(format!(
                    "Unsupported worktree mode: {other}"
                )))
            }
        };
        let wt_path = self.wt_add(&repo, &branch, base_spec.as_deref()).await?;
        // An untracked paseo.json in the source checkout comes along (Paseo copies it too).
        let src_cfg = repo.join(setup::CONFIG_FILE);
        let dst_cfg = wt_path.join(setup::CONFIG_FILE);
        if src_cfg.is_file() && !dst_cfg.exists() {
            let _ = std::fs::copy(&src_cfg, &dst_cfg);
        }
        let base_ref = match &base_spec {
            Some(b) => git::resolve_ref(&repo, b).await,
            None => match &default_base {
                Some(d) => git::resolve_ref(&repo, d).await,
                None => None,
            },
        };
        let now = now_iso();
        let auto = req.prompt.is_some()
            && req.new_branch.is_none()
            && req.worktree_slug.is_none()
            && req.mode.as_deref().unwrap_or("branch-off") == "branch-off";
        let w = WorkspaceRecord {
            workspace_id: new_id("wks"),
            project_id: project.project_id,
            cwd: wt_path.display().to_string(),
            kind: WorkspaceKind::Worktree,
            display_name: branch.clone(),
            title: req.title.clone().filter(|t| !t.trim().is_empty()),
            branch: Some(branch.clone()),
            base_ref,
            base_sha: git::read(&wt_path, &["rev-parse", "HEAD"]).await,
            main_repo_root: Some(repo.display().to_string()),
            owned_worktree: true,
            created_at: now.clone(),
            updated_at: now,
            archived_at: None,
            pinned_at: None,
            labels: Vec::new(),
            untrusted_source: untrusted.clone(),
            background: req.background,
            setup: None,
            port: free_port(),
            auto_name: auto.then(|| AutoName {
                status: "pending".into(),
                placeholder_branch_name: branch.clone(),
            }),
            removed_reason: None,
        };
        self.lock()
            .workspaces
            .insert(w.workspace_id.clone(), w.clone());
        self.save(&w);
        tracing::info!(workspace = %w.workspace_id, project = %basename(&repo), branch = %branch, cwd = %w.cwd, "worktree workspace created");
        let skipped = match &untrusted {
            Some(u) => {
                let msg = format!(
                    "Scripts are blocked for PR #{} from {}. Run setup to allow them.",
                    u.number, u.head_repository
                );
                self.update(&w.workspace_id, |r| {
                    r.setup = Some(SetupSnapshot {
                        status: "blocked".into(),
                        commands: Vec::new(),
                        error: Some(msg.clone()),
                    })
                });
                Some(msg)
            }
            None => {
                self.spawn_setup(&w.workspace_id);
                None
            }
        };
        Ok((self.get(&w.workspace_id).unwrap_or(w), skipped))
    }

    /// Run `paseo.json` `worktree.setup` in the background.
    fn spawn_setup(&self, id: &str) {
        let Some(w) = self.get(id) else { return };
        if w.kind != WorkspaceKind::Worktree {
            return;
        }
        let dir = PathBuf::from(&w.cwd);
        let cmds = setup::commands(&setup::read_config(&dir)["worktree"]["setup"]);
        if cmds.is_empty() {
            self.update(id, |r| {
                r.setup = Some(SetupSnapshot {
                    status: "none".into(),
                    commands: Vec::new(),
                    error: None,
                })
            });
            return;
        }
        let source = PathBuf::from(w.main_repo_root.clone().unwrap_or_default());
        let env = setup::env(
            &source,
            &dir,
            w.branch.as_deref().unwrap_or_default(),
            w.port,
        );
        let me = self.clone();
        let id = id.to_string();
        tokio::spawn(async move {
            let final_snap = setup::run_setup(&cmds, &dir, &env, |snap| {
                let snap = snap.clone();
                me.update(&id, |r| r.setup = Some(snap));
            })
            .await;
            if final_snap.status == "failed" {
                tracing::warn!(workspace = %id, error = ?final_snap.error, "worktree setup failed");
            }
        });
    }

    /// `workspace setup`: allow a blocked (untrusted) workspace's scripts and run setup.
    pub fn run_setup(&self, id: &str) -> Result<&'static str, WsError> {
        let w = self
            .get(id)
            .filter(|w| w.archived_at.is_none())
            .ok_or_else(|| WsError::NotFound(format!("Workspace not found: {id}")))?;
        let blocked = w.setup.as_ref().is_some_and(|s| s.status == "blocked");
        if !blocked {
            return Ok("already_allowed");
        }
        self.update(id, |r| r.untrusted_source = None);
        self.spawn_setup(id);
        Ok("started")
    }

    /// The facts the guard decides an agent start on.
    pub async fn start_facts(
        &self,
        cwd: &str,
        provider: &str,
        mode: Option<&str>,
        owner_override: bool,
    ) -> StartFacts {
        let cfg = &self.inner.config;
        let dir = PathBuf::from(cwd);
        let info = git::info(&dir).await;
        let kind = info.kind();
        let top = info.toplevel.as_deref().map(git::canon);
        let repo = info.repo_root().map(git::canon);
        let projects_root = git::canon(&cfg.projects_root);
        let wt_root = git::canon(&cfg.wt_root);
        let repo_name = repo.as_deref().map(basename).unwrap_or_default();
        let under_root = top.as_deref().is_some_and(|t| {
            t.starts_with(wt_root.join(&repo_name)) && t != wt_root.join(&repo_name)
        });
        let codex = provider == "codex" || provider.starts_with("codex/");
        let branch = info.branch.clone().unwrap_or_default();
        let ws = self
            .lock()
            .workspaces
            .values()
            .find(|w| w.cwd == cwd && w.kind == WorkspaceKind::Worktree && w.archived_at.is_none())
            .cloned();
        let base = ws
            .as_ref()
            .and_then(|w| w.base_ref.clone())
            .unwrap_or_default();
        let lucina_rs_base = base == format!("refs/heads/{}", cfg.codex_base)
            || base == format!("refs/remotes/origin/{}", cfg.codex_base);
        StartFacts {
            kind: Some(kind),
            protected: kind == WorkspaceKind::LocalCheckout
                && top
                    .as_deref()
                    .is_some_and(|t| t.starts_with(&projects_root) && !t.starts_with(&wt_root)),
            under_root,
            read_only: matches!(mode, Some("plan") | Some("read-only")),
            owner_override,
            codex,
            codex_repo: cfg.codex_repos.contains(&repo_name),
            codex_branch: branch.starts_with("codex/"),
            lucina_rs_base,
        }
    }

    /// Archive a workspace: teardown, then the worktree and its branch go as the guard
    /// allows. The caller has closed its agents (`busy` = one still runs).
    pub async fn archive(
        &self,
        id: &str,
        busy: bool,
        trigger: Trigger,
        reason: Option<String>,
    ) -> Result<WorkspaceRecord, WsError> {
        let w = self
            .get(id)
            .filter(|w| w.archived_at.is_none())
            .ok_or_else(|| WsError::NotFound(format!("Workspace not found: {id}")))?;
        let mut note = reason;
        if w.kind == WorkspaceKind::Worktree && w.owned_worktree {
            let dir = PathBuf::from(&w.cwd);
            let repo = PathBuf::from(w.main_repo_root.clone().unwrap_or_default());
            if dir.is_dir() {
                let blocked = w.untrusted_source.is_some();
                let teardown = if blocked {
                    Vec::new()
                } else {
                    setup::commands(&setup::read_config(&dir)["worktree"]["teardown"])
                };
                let env = setup::env(&repo, &dir, w.branch.as_deref().unwrap_or_default(), w.port);
                for cmd in &teardown {
                    let (code, log) = setup::run_command(cmd, &dir, &env).await;
                    if code != Some(0) {
                        tracing::warn!(workspace = id, command = %cmd, log = %log, "teardown command failed");
                    }
                }
                let branch = git::info(&dir)
                    .await
                    .branch
                    .or(w.branch.clone())
                    .unwrap_or_default();
                let merged = match &w.base_ref {
                    Some(base) if !branch.is_empty() => {
                        git::merged_into(&repo, &branch, base).await
                            || checkout::merged_on_github(&dir).await
                    }
                    _ => false,
                };
                let facts = RemoveFacts {
                    owned: true,
                    clean: !git::is_dirty(&dir).await,
                    busy,
                    merged,
                };
                let plan = guard::removal(&facts, trigger);
                if plan.remove_dir {
                    match git::run(&repo, &["worktree", "remove", &w.cwd]).await {
                        Ok(o) if o.ok => {}
                        Ok(o) => note = Some(format!("worktree kept: {}", o.stderr)),
                        Err(e) => note = Some(format!("worktree kept: {e}")),
                    }
                } else if !facts.clean {
                    note = Some("worktree kept: it has uncommitted changes".into());
                }
                if plan.delete_branch && !branch.is_empty() {
                    let _ = git::run(&repo, &["branch", "-d", &branch]).await;
                }
            }
        }
        tracing::info!(workspace = id, project = %self.project_name(&w), branch = ?w.branch, cwd = %w.cwd, note = ?note, "workspace archived");
        let w = self
            .update(id, |r| {
                r.archived_at = Some(now_iso());
                if note.is_some() {
                    r.removed_reason = note.clone();
                }
            })
            .ok_or_else(|| WsError::NotFound(format!("Workspace not found: {id}")))?;
        Ok(w)
    }

    /// Owned worktrees whose branch moved past its start and is now merged into its base
    /// (or merged on GitHub), and that nobody is using ([`sweep_blocker`]; `busy` = an
    /// agentd agent runs there). Most work happens in sessions agentd doesn't know, so what
    /// the checkout and its processes show is all there is to go on.
    pub async fn merged_candidates(&self, busy: &HashSet<String>) -> Vec<(String, String)> {
        let list: Vec<WorkspaceRecord> = self
            .lock()
            .workspaces
            .values()
            .filter(|w| w.archived_at.is_none() && w.owned_worktree)
            .cloned()
            .collect();
        let dirs: Vec<PathBuf> = list.iter().map(|w| PathBuf::from(&w.cwd)).collect();
        let usage = procs::usage(&dirs);
        let mut out = Vec::new();
        for (w, used) in list.iter().zip(&usage) {
            if busy.contains(&w.workspace_id) {
                continue;
            }
            let dir = PathBuf::from(&w.cwd);
            if !dir.is_dir() {
                continue;
            }
            let (Some(base), Some(start)) = (&w.base_ref, &w.base_sha) else {
                continue;
            };
            let info = git::info(&dir).await;
            let facts = SweepFacts {
                in_progress: git::in_progress(&dir).await,
                detached: info.branch.is_none(),
                dirty: git::is_dirty(&dir).await,
                idle_secs: git::idle_secs(&dir).await,
                procs: used.procs,
            };
            if let Some(why) = sweep_blocker(&facts, self.inner.config.sweep_idle_secs) {
                tracing::debug!(workspace = %w.workspace_id, why, "merge sweep: left alone");
                continue;
            }
            let Some(branch) = info.branch else { continue };
            let head = git::read(&dir, &["rev-parse", "HEAD"]).await;
            if head.as_deref() == Some(start.as_str()) {
                continue;
            }
            let short = base
                .trim_start_matches("refs/heads/")
                .trim_start_matches("refs/remotes/");
            if git::merged_into(&dir, &format!("refs/heads/{branch}"), base).await {
                out.push((w.workspace_id.clone(), format!("merged into {short}")));
            } else if checkout::merged_on_github(&dir).await {
                out.push((w.workspace_id.clone(), "pull request merged".to_string()));
            }
        }
        out
    }

    /// The generated title and branch for a worktree still on its placeholder.
    pub async fn apply_auto_name(&self, id: &str, title: Option<String>, branch: Option<String>) {
        let Some(w) = self.get(id) else { return };
        let Some(auto) = w.auto_name.clone().filter(|a| a.status == "pending") else {
            return;
        };
        let dir = PathBuf::from(&w.cwd);
        let mut new_branch = None;
        if let Some(b) = branch
            .map(|b| b.trim().to_lowercase())
            .filter(|b| lucina_agents::branch_slug::validate(b).is_ok())
        {
            let current = git::info(&dir).await.branch;
            if current.as_deref() == Some(auto.placeholder_branch_name.as_str()) {
                let prefix = auto
                    .placeholder_branch_name
                    .split_once('/')
                    .map(|(p, _)| format!("{p}/"))
                    .unwrap_or_default();
                let wanted = if b.contains('/') || prefix.is_empty() {
                    b
                } else {
                    format!("{prefix}{b}")
                };
                let repo = PathBuf::from(w.main_repo_root.clone().unwrap_or_default());
                if wanted == auto.placeholder_branch_name {
                    // The placeholder is already the name the model would give.
                } else if let Ok(unique) = self.unique_branch(&repo, &wanted).await {
                    if git::run(
                        &dir,
                        &["branch", "-m", &auto.placeholder_branch_name, &unique],
                    )
                    .await
                    .is_ok_and(|o| o.ok)
                    {
                        new_branch = Some(unique);
                    }
                }
            }
        }
        self.update(id, |r| {
            if let Some(b) = &new_branch {
                r.branch = Some(b.clone());
                r.display_name = b.clone();
            }
            if r.title.is_none() {
                r.title = title.filter(|t| !t.trim().is_empty());
            }
            r.auto_name = Some(AutoName {
                status: "attempted".into(),
                ..auto
            });
        });
    }

    /// The workspace an agent's cwd belongs to, for its `workspaceId`.
    pub fn find_by_cwd(&self, cwd: &str) -> Option<WorkspaceRecord> {
        self.lock()
            .workspaces
            .values()
            .filter(|w| w.cwd == cwd && w.archived_at.is_none())
            .max_by(|a, b| a.created_at.cmp(&b.created_at))
            .cloned()
    }

    /// Register a worktree someone made by hand (`git worktree add`, `wt add`) as an owned
    /// worktree workspace, so the pipeline merges it and archive may remove it. Its start
    /// is where it left `base` (the merge base).
    pub async fn adopt(
        &self,
        dir: &Path,
        title: Option<String>,
        base: &str,
    ) -> Result<WorkspaceRecord, WsError> {
        let info = git::info(dir).await;
        if info.kind() != WorkspaceKind::Worktree {
            return Err(WsError::Invalid(format!(
                "Not a linked worktree: {}",
                dir.display()
            )));
        }
        let repo = info
            .main_root
            .clone()
            .ok_or_else(|| WsError::Invalid(format!("No main checkout for {}", dir.display())))?;
        let branch = info
            .branch
            .clone()
            .ok_or_else(|| WsError::Invalid(format!("{} is on a detached HEAD", dir.display())))?;
        let project = self.project_for(&repo, true)?;
        let base_ref = git::resolve_ref(&repo, base).await;
        let base_sha = git::read(dir, &["merge-base", "HEAD", base]).await;
        let now = now_iso();
        let w = WorkspaceRecord {
            workspace_id: new_id("wks"),
            project_id: project.project_id,
            cwd: dir.display().to_string(),
            kind: WorkspaceKind::Worktree,
            display_name: branch.clone(),
            title: title.filter(|t| !t.trim().is_empty()),
            branch: Some(branch),
            base_ref,
            base_sha,
            main_repo_root: Some(repo.display().to_string()),
            owned_worktree: true,
            created_at: now.clone(),
            updated_at: now,
            archived_at: None,
            pinned_at: None,
            labels: vec!["adopted".into()],
            untrusted_source: None,
            background: false,
            setup: None,
            port: None,
            auto_name: None,
            removed_reason: None,
        };
        self.lock()
            .workspaces
            .insert(w.workspace_id.clone(), w.clone());
        self.save(&w);
        tracing::info!(workspace = %w.workspace_id, project = %basename(&repo), branch = ?w.branch, cwd = %w.cwd, "worktree adopted");
        Ok(w)
    }

    /// A kept workspace continues from `sha` (its branch was moved there after a merge),
    /// so the merge sweep doesn't take it for merged work.
    pub fn restart_at(&self, id: &str, sha: &str) -> Option<WorkspaceRecord> {
        self.update(id, |r| r.base_sha = Some(sha.to_string()))
    }

    /// Refresh the branch of local checkouts (the owner may switch it).
    pub async fn refresh(&self, id: &str) -> Option<WorkspaceRecord> {
        let w = self.get(id)?;
        if w.kind == WorkspaceKind::Directory || w.archived_at.is_some() {
            return Some(w);
        }
        let info = git::info(Path::new(&w.cwd)).await;
        if info.branch != w.branch && info.is_git {
            return self.update(id, |r| {
                r.branch = info.branch.clone();
                r.display_name = info
                    .branch
                    .clone()
                    .unwrap_or_else(|| basename(Path::new(&r.cwd)));
            });
        }
        Some(w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_agents::worktree_guard::{check_start, Refusal};

    fn registry(root: &Path) -> Registry {
        let store = Arc::new(Store::open(&root.join("state")).unwrap());
        let config = Config {
            wt: None,
            wt_root: root.join("proj/.worktrees"),
            projects_root: root.join("proj"),
            codex_repos: vec!["SecretaryIOS".into()],
            codex_base: "lucina-rs".into(),
            codex_deny_repos: vec![],
            sweep_idle_secs: 0,
        };
        Registry::open(store, config, Arc::new(|_: &WorkspaceRecord| {})).unwrap()
    }

    fn lucina_config(root: &Path, deny: Vec<String>) -> Config {
        Config {
            wt: None,
            wt_root: root.join("wt"),
            projects_root: root.join("proj"),
            codex_repos: vec![],
            codex_base: "lucina-rs".into(),
            codex_deny_repos: deny,
            sweep_idle_secs: 0,
        }
    }

    #[test]
    fn lucina_projects_are_found_by_path_worktree_and_repo() {
        let t = tempfile::tempdir().unwrap();
        let cfg = lucina_config(t.path(), vec![]);
        let (proj, wt) = (t.path().join("proj"), t.path().join("wt"));
        let lucina = proj.join("Lucina");
        // A checkout under <projects_root>/Lucina/, and a folder inside it.
        let sios = lucina.join("SecretaryIOS");
        repo(&sios);
        assert_eq!(lucina_repo_name(&cfg, &sios), Some("SecretaryIOS".into()));
        assert!(is_lucina_project(&cfg, &sios.join("crates")));
        // A worktree under <wt_root>/<Repo>/ of a Lucina repo; one of another repo is not.
        let wt_sios = wt.join("SecretaryIOS").join("feature-x");
        repo(&wt_sios);
        assert_eq!(
            lucina_repo_name(&cfg, &wt_sios),
            Some("SecretaryIOS".into())
        );
        let wt_other = wt.join("Other").join("feature-x");
        repo(&wt_other);
        assert_eq!(lucina_repo_name(&cfg, &wt_other), None);
        // A nested path of a repo under Lucina.
        let deep = lucina.join("Nested").join("a").join("b");
        repo(&lucina.join("Nested"));
        std::fs::create_dir_all(&deep).unwrap();
        assert_eq!(lucina_repo_name(&cfg, &deep), Some("Nested".into()));
        // Another project under projects_root, and a repo outside it.
        repo(&proj.join("Other"));
        assert_eq!(lucina_repo_name(&cfg, &proj.join("Other")), None);
        repo(&t.path().join("elsewhere"));
        assert_eq!(lucina_repo_name(&cfg, &t.path().join("elsewhere")), None);
    }

    #[test]
    fn a_symlink_into_lucina_is_lucina() {
        let t = tempfile::tempdir().unwrap();
        let cfg = lucina_config(t.path(), vec![]);
        let sios = t.path().join("proj/Lucina/SecretaryIOS");
        repo(&sios);
        let link = t.path().join("link");
        std::os::unix::fs::symlink(&sios, &link).unwrap();
        assert_eq!(lucina_repo_name(&cfg, &link), Some("SecretaryIOS".into()));
    }

    #[test]
    fn the_extra_deny_list_counts_worktrees_and_repos_by_name() {
        let t = tempfile::tempdir().unwrap();
        let cfg = lucina_config(t.path(), vec!["MyRepo".into()]);
        let wt_repo = t.path().join("wt/MyRepo/branch-a");
        repo(&wt_repo);
        assert_eq!(lucina_repo_name(&cfg, &wt_repo), Some("MyRepo".into()));
        let standalone = t.path().join("somewhere/MyRepo");
        repo(&standalone);
        assert_eq!(lucina_repo_name(&cfg, &standalone), Some("MyRepo".into()));
        assert_eq!(
            lucina_repo_name(&lucina_config(t.path(), vec![]), &standalone),
            None
        );
    }

    fn repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        let run = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .unwrap()
                .success())
        };
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("README"), "x").unwrap();
        run(&["add", "."]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
    }

    #[test]
    fn the_sweep_leaves_used_worktrees_alone() {
        let idle = SweepFacts {
            idle_secs: Some(4000),
            ..SweepFacts::default()
        };
        assert_eq!(sweep_blocker(&idle, 1800), None);
        assert!(sweep_blocker(
            &SweepFacts {
                in_progress: Some("rebase"),
                ..idle.clone()
            },
            1800
        )
        .is_some());
        assert!(sweep_blocker(
            &SweepFacts {
                detached: true,
                ..idle.clone()
            },
            1800
        )
        .is_some());
        assert!(sweep_blocker(
            &SweepFacts {
                dirty: true,
                ..idle.clone()
            },
            1800
        )
        .is_some());
        assert!(sweep_blocker(
            &SweepFacts {
                procs: 1,
                ..idle.clone()
            },
            1800
        )
        .is_some());
        assert!(sweep_blocker(
            &SweepFacts {
                idle_secs: Some(60),
                ..idle.clone()
            },
            1800
        )
        .is_some());
        assert!(
            sweep_blocker(
                &SweepFacts {
                    idle_secs: None,
                    ..idle
                },
                1800
            )
            .is_some(),
            "unknown means used"
        );
    }

    #[tokio::test]
    async fn an_operation_in_progress_is_seen() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("r");
        repo(&dir);
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap()
        };
        let edit = |text: &str, msg: &str| {
            std::fs::write(dir.join("f.txt"), text).unwrap();
            assert!(git(&["add", "-A"]).status.success());
            assert!(git(&["commit", "-q", "-m", msg]).status.success());
        };
        edit("base\n", "base");
        assert!(git(&["checkout", "-q", "-b", "topic"]).status.success());
        edit("topic\n", "topic");
        assert!(git(&["checkout", "-q", "main"]).status.success());
        edit("main\n", "main");
        assert_eq!(git::in_progress(&dir).await, None);
        assert!(git(&["checkout", "-q", "topic"]).status.success());
        assert!(
            !git(&["rebase", "-q", "main"]).status.success(),
            "the rebase conflicts"
        );
        assert_eq!(git::in_progress(&dir).await, Some("rebase"));
        assert_eq!(git::info(&dir).await.branch, None, "a rebase detaches HEAD");
        assert!(git::idle_secs(&dir).await.is_some());
    }

    #[tokio::test]
    async fn codex_rule_facts() {
        let t = tempfile::tempdir().unwrap();
        let r = registry(t.path());
        let sios = t.path().join("proj/Lucina/SecretaryIOS");
        repo(&sios);
        let cwd = sios.display().to_string();
        let codex = r.start_facts(&cwd, "codex", None, true).await;
        assert!(codex.codex && codex.codex_repo && codex.protected);
        assert_eq!(
            check_start(&codex),
            Err(Refusal::CodexRule),
            "no override for Codex in the main checkout"
        );
        let claude = r.start_facts(&cwd, "claude", Some("default"), false).await;
        assert_eq!(check_start(&claude), Err(Refusal::MainCheckout));
        assert_eq!(
            check_start(&r.start_facts(&cwd, "claude", Some("plan"), false).await),
            Ok(())
        );
        let other = t.path().join("proj/Other");
        repo(&other);
        let f = r
            .start_facts(&other.display().to_string(), "codex", None, true)
            .await;
        assert!(!f.codex_repo);
        assert_eq!(
            check_start(&f),
            Ok(()),
            "the Codex rule is for SecretaryIOS"
        );
    }

    #[tokio::test]
    async fn untrusted_scripts_wait_for_the_owner() {
        let t = tempfile::tempdir().unwrap();
        let r = registry(t.path());
        let dir = t.path().join("plain");
        std::fs::create_dir_all(&dir).unwrap();
        let (w, _) = r
            .create(CreateWorkspace {
                isolation: "local".into(),
                path: Some(dir.display().to_string()),
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(w.kind, WorkspaceKind::Directory);
        r.update(&w.workspace_id, |x| {
            x.untrusted_source = Some(UntrustedSource {
                kind: "change_request".into(),
                forge: "github".into(),
                number: 7,
                head_repository: "someone/fork".into(),
            });
            x.setup = Some(SetupSnapshot {
                status: "blocked".into(),
                commands: Vec::new(),
                error: None,
            });
        });
        assert_eq!(r.run_setup(&w.workspace_id).unwrap(), "started");
        assert!(r.get(&w.workspace_id).unwrap().untrusted_source.is_none());
        r.update(&w.workspace_id, |x| {
            x.setup = Some(SetupSnapshot {
                status: "completed".into(),
                commands: Vec::new(),
                error: None,
            })
        });
        assert_eq!(r.run_setup(&w.workspace_id).unwrap(), "already_allowed");
    }
}
