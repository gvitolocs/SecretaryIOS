//! Diffs and checkout operations (Paseo `utils/checkout-git.js`, `checkout_*_request`).
//!
//! Diff modes, as in Paseo 0.13:
//! - `uncommitted`: the working tree against `HEAD`, untracked files included;
//! - `base`: committed work only, the merge base with the base branch against `HEAD`;
//! - `all` (the default, "All changes"): the merge base against the working tree,
//!   untracked files included.
//!
//! Untracked files are added as intent-to-add to a private copy of the index
//! (`GIT_INDEX_FILE`), so the real index is never touched.
//!
//! Pushing and opening pull requests happen only on an explicit request.

use std::path::Path;

use lucina_agents::diffparse;
use lucina_agents::workspace::DiffFile;
use serde_json::{json, Value};

use super::git;

/// The empty tree, for a repository without commits.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
const MAX_FILE_LINES: usize = 2000;
const MAX_DIFF_BYTES: usize = 8 * 1024 * 1024;

pub struct Diff {
    pub files: Vec<DiffFile>,
    pub too_large: bool,
}

/// The base a workspace compares against: the caller's, else the worktree's own base,
/// else the default branch.
pub async fn base_ref(dir: &Path, requested: Option<&str>, stored: Option<&str>) -> Option<String> {
    if let Some(r) = requested.filter(|r| !r.is_empty()) {
        return Some(r.to_string());
    }
    if let Some(s) = stored {
        return Some(s.to_string());
    }
    let repo = git::info(dir).await;
    git::default_branch(repo.repo_root()?).await
}

async fn merge_base(dir: &Path, base: &str) -> Result<String, String> {
    git::read(dir, &["merge-base", base, "HEAD"]).await.ok_or_else(|| format!("No merge base between {base} and HEAD"))
}

pub async fn diff(dir: &Path, mode: &str, requested_base: Option<&str>, stored_base: Option<&str>, ignore_whitespace: bool) -> Result<Diff, String> {
    let info = git::info(dir).await;
    if !info.is_git {
        return Err("Not a git repository".into());
    }
    let head = info.head.clone();
    let mut args: Vec<String> = ["diff", "--no-color", "--no-ext-diff", "--no-textconv", "--find-renames", "--src-prefix=a/", "--dst-prefix=b/", "-U3"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if ignore_whitespace {
        args.push("--ignore-all-space".into());
    }
    let with_untracked = match mode {
        "uncommitted" => {
            args.push(head.clone().unwrap_or_else(|| EMPTY_TREE.into()));
            true
        }
        "base" | "all" => {
            let base = base_ref(dir, requested_base, stored_base).await.ok_or("No base branch to compare with")?;
            let from = if head.is_some() { merge_base(dir, &base).await? } else { EMPTY_TREE.to_string() };
            args.push(from);
            if mode == "base" {
                args.push(head.clone().unwrap_or_else(|| EMPTY_TREE.into()));
                false
            } else {
                true
            }
        }
        other => return Err(format!("Unknown diff mode: {other} (uncommitted, base or all)")),
    };
    let tmp = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    let index_path = tmp.path().display().to_string();
    let mut env: Vec<(&str, &str)> = Vec::new();
    if with_untracked {
        if let Some(real) = git::read(dir, &["rev-parse", "--path-format=absolute", "--git-path", "index"]).await {
            let _ = std::fs::copy(&real, tmp.path());
        }
        env.push(("GIT_INDEX_FILE", index_path.as_str()));
        let untracked = git::run_env(dir, &["ls-files", "--others", "--exclude-standard", "-z"], &env).await?;
        let files: Vec<&str> = untracked.stdout.split('\0').filter(|s| !s.is_empty()).collect();
        for chunk in files.chunks(200) {
            let mut add = vec!["add", "--intent-to-add", "--"];
            add.extend(chunk.iter().copied());
            git::run_env(dir, &add, &env).await?;
        }
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = git::run_env(dir, &refs, &env).await?;
    if !out.ok {
        return Err(out.stderr);
    }
    if out.stdout.len() > MAX_DIFF_BYTES {
        return Ok(Diff { files: Vec::new(), too_large: true });
    }
    Ok(Diff { files: diffparse::parse(&out.stdout, MAX_FILE_LINES), too_large: false })
}

/// Paseo `checkout_status_response`.
pub async fn status(dir: &Path, stored_base: Option<&str>, owned: bool) -> Value {
    let cwd = dir.display().to_string();
    let info = git::info(dir).await;
    if !info.is_git {
        return json!({"cwd": cwd, "isGit": false, "error": null});
    }
    let base = base_ref(dir, None, stored_base).await;
    let short_base = base.as_deref().map(|b| b.trim_start_matches("refs/heads/").trim_start_matches("refs/remotes/").to_string());
    let ahead_behind = match &base {
        Some(b) => git::ahead_behind(dir, b).await.map(|(a, b)| json!({"ahead": a, "behind": b})),
        None => None,
    };
    let upstream = git::read(dir, &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]).await;
    let (ahead_origin, behind_origin) = match &upstream {
        Some(u) => match git::ahead_behind(dir, u).await {
            Some((a, b)) => (json!(a), json!(b)),
            None => (Value::Null, Value::Null),
        },
        None => (Value::Null, Value::Null),
    };
    let remote_url = git::read(dir, &["remote", "get-url", "origin"]).await;
    json!({
        "cwd": cwd,
        "isGit": true,
        "repoRoot": info.toplevel.as_ref().map(|p| p.display().to_string()),
        "mainRepoRoot": info.main_root.as_ref().map(|p| p.display().to_string()),
        "currentBranch": info.branch,
        "isDirty": git::is_dirty(dir).await,
        "baseRef": short_base,
        "aheadBehind": ahead_behind,
        "upstreamRef": upstream,
        "aheadOfOrigin": ahead_origin,
        "behindOfOrigin": behind_origin,
        "hasRemote": remote_url.is_some(),
        "remoteUrl": remote_url,
        "isPaseoOwnedWorktree": owned,
        "error": null,
    })
}

/// Commit; `author` is `(name, email)`, passed as `-c user.name/-c user.email` (the
/// owner's way: nezopt has no global git identity).
pub async fn commit(dir: &Path, message: &str, add_all: bool, author: Option<(&str, &str)>) -> Result<String, String> {
    if message.trim().is_empty() {
        return Err("A commit message is required".into());
    }
    if add_all {
        git::must(dir, &["add", "-A"]).await?;
    }
    let (name, email) = author.map(|(n, e)| (format!("user.name={n}"), format!("user.email={e}"))).unzip();
    let mut args: Vec<&str> = Vec::new();
    if let (Some(n), Some(e)) = (&name, &email) {
        args.extend(["-c", n.as_str(), "-c", e.as_str()]);
    }
    args.extend(["commit", "-m", message]);
    git::must(dir, &args).await?;
    git::must(dir, &["rev-parse", "HEAD"]).await.map(|s| s.trim().to_string())
}

/// Push the current branch to origin (only ever on an explicit request).
pub async fn push(dir: &Path) -> Result<String, String> {
    let branch = git::read(dir, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await.ok_or("Detached HEAD: nothing to push")?;
    let o = git::run(dir, &["push", "-u", "origin", &branch]).await?;
    if o.ok {
        Ok(branch)
    } else {
        Err(o.stderr)
    }
}

async fn gh(dir: &Path, args: &[&str]) -> Result<String, String> {
    let gh = crate::process::resolve_program("gh").or_else(|| crate::process::resolve_program("~/.local/bin/gh")).ok_or("The GitHub CLI (gh) is not installed")?;
    let out = tokio::process::Command::new(gh)
        .args(args)
        .current_dir(dir)
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| format!("cannot run gh: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

pub async fn pr_status(dir: &Path) -> Result<Value, String> {
    let out = gh(dir, &["pr", "view", "--json", "number,state,url,title,isDraft,headRefName,headRefOid,baseRefName,mergedAt"]).await?;
    serde_json::from_str(&out).map_err(|e| e.to_string())
}

pub async fn pr_create(dir: &Path, title: &str, body: &str, base: Option<&str>) -> Result<Value, String> {
    let mut args = vec!["pr", "create", "--title", title, "--body", body];
    if let Some(b) = base {
        args.extend(["--base", b]);
    }
    let url = gh(dir, &args).await?;
    Ok(json!({"url": url.trim()}))
}

/// The change request a worktree is made from (`gh pr view <n>`).
pub async fn pr_head(dir: &Path, number: u64) -> Result<Value, String> {
    let n = number.to_string();
    let out = gh(dir, &["pr", "view", &n, "--json", "number,headRefName,headRefOid,headRepository,headRepositoryOwner,isCrossRepository,url,state"]).await?;
    serde_json::from_str(&out).map_err(|e| e.to_string())
}

/// Whether GitHub says the branch's pull request is merged, with the local head on it.
pub async fn merged_on_github(dir: &Path) -> bool {
    let Ok(pr) = pr_status(dir).await else { return false };
    let head = git::read(dir, &["rev-parse", "HEAD"]).await;
    pr["state"] == "MERGED" && head.is_some() && pr["headRefOid"].as_str() == head.as_deref()
}
