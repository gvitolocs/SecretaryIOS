//! Git, run so the owner's git settings can't change what agentd reads: no colors, no
//! external diff or textconv, unquoted paths, fixed `a/` `b/` prefixes. Reads don't
//! take optional locks (`GIT_OPTIONAL_LOCKS=0`), so they never fight an agent's git.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

const TIMEOUT: Duration = Duration::from_secs(120);

pub struct Out {
    pub ok: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// `git -C dir <args>` with the fixed settings above.
pub async fn run_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<Out, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["-c", "core.quotePath=false", "-c", "color.ui=false", "-c", "diff.noprefix=false", "-c", "diff.mnemonicPrefix=false"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = tokio::time::timeout(TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("git {} timed out", args.first().unwrap_or(&"")))?
        .map_err(|e| format!("cannot run git: {e}"))?;
    Ok(Out {
        ok: out.status.success(),
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

pub async fn run(dir: &Path, args: &[&str]) -> Result<Out, String> {
    run_env(dir, args, &[]).await
}

/// stdout (trimmed) when git succeeds.
pub async fn read(dir: &Path, args: &[&str]) -> Option<String> {
    match run(dir, args).await {
        Ok(o) if o.ok => Some(o.stdout.trim().to_string()),
        _ => None,
    }
}

/// stdout when git succeeds, else its error text.
pub async fn must(dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = run(dir, args).await?;
    if o.ok {
        Ok(o.stdout)
    } else {
        Err(if o.stderr.is_empty() { format!("git {} failed", args.join(" ")) } else { o.stderr })
    }
}

/// What a directory is, from git's point of view.
#[derive(Debug, Clone, Default)]
pub struct Info {
    pub is_git: bool,
    /// The checkout's top directory.
    pub toplevel: Option<PathBuf>,
    /// The main working tree, when this is a linked worktree.
    pub main_root: Option<PathBuf>,
    pub branch: Option<String>,
    pub head: Option<String>,
}

impl Info {
    pub fn kind(&self) -> lucina_agents::worktree_guard::Kind {
        use lucina_agents::worktree_guard::Kind;
        match (self.is_git, &self.main_root) {
            (false, _) => Kind::Directory,
            (true, Some(_)) => Kind::Worktree,
            (true, None) => Kind::LocalCheckout,
        }
    }

    /// The repository's main working tree (itself for a main checkout).
    pub fn repo_root(&self) -> Option<&Path> {
        self.main_root.as_deref().or(self.toplevel.as_deref())
    }
}

pub async fn info(dir: &Path) -> Info {
    let Some(toplevel) = read(dir, &["rev-parse", "--show-toplevel"]).await else { return Info::default() };
    let toplevel = PathBuf::from(toplevel);
    let common = read(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await.map(PathBuf::from);
    let main = common.map(|c| if c.file_name().is_some_and(|n| n == ".git") { c.parent().map(Path::to_path_buf).unwrap_or(c) } else { c });
    let main_root = main.filter(|m| canon(m) != canon(&toplevel));
    Info {
        is_git: true,
        branch: read(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await,
        head: read(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).await,
        toplevel: Some(toplevel),
        main_root,
    }
}

pub fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// wt's `default_branch`: origin's HEAD, else main or master, else the current branch.
pub async fn default_branch(repo: &Path) -> Option<String> {
    if let Some(b) = read(repo, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"]).await {
        return Some(b.trim_start_matches("origin/").to_string());
    }
    for b in ["main", "master"] {
        if read(repo, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{b}")]).await.is_some() {
            return Some(b.to_string());
        }
    }
    read(repo, &["branch", "--show-current"]).await.filter(|b| !b.is_empty())
}

pub async fn branch_exists(repo: &Path, branch: &str) -> bool {
    read(repo, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")]).await.is_some()
}

/// The full ref a base name resolves to: a local branch, else `origin/<name>`, else
/// any ref git accepts.
pub async fn resolve_ref(repo: &Path, name: &str) -> Option<String> {
    for candidate in [format!("refs/heads/{name}"), format!("refs/remotes/origin/{name}")] {
        if read(repo, &["show-ref", "--verify", "--quiet", &candidate]).await.is_some() {
            return Some(candidate);
        }
    }
    if let Some(full) = read(repo, &["rev-parse", "--verify", "--quiet", "--symbolic-full-name", name]).await.filter(|s| !s.is_empty()) {
        return Some(full);
    }
    read(repo, &["rev-parse", "--verify", "--quiet", &format!("{name}^{{commit}}")]).await
}

pub async fn is_dirty(dir: &Path) -> bool {
    match read(dir, &["status", "--porcelain", "--untracked-files=normal"]).await {
        Some(s) => !s.is_empty(),
        None => true,
    }
}

/// Worktrees of the repository with their checked-out branch.
pub async fn worktrees(repo: &Path) -> Vec<(PathBuf, Option<String>)> {
    let Some(out) = read(repo, &["worktree", "list", "--porcelain"]).await else { return Vec::new() };
    let mut list = Vec::new();
    let mut path: Option<PathBuf> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(prev) = path.take() {
                list.push((prev, None));
            }
            path = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
            if let Some(p) = path.take() {
                list.push((p, Some(b.to_string())));
            }
        }
    }
    if let Some(p) = path {
        list.push((p, None));
    }
    list
}

/// `(ahead, behind)` of HEAD against `base`.
pub async fn ahead_behind(dir: &Path, base: &str) -> Option<(u64, u64)> {
    let s = read(dir, &["rev-list", "--left-right", "--count", &format!("HEAD...{base}")]).await?;
    let mut it = s.split_whitespace();
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// Every commit of `branch` is already on `base`.
pub async fn merged_into(dir: &Path, branch: &str, base: &str) -> bool {
    matches!(run(dir, &["merge-base", "--is-ancestor", branch, base]).await, Ok(o) if o.ok)
}
