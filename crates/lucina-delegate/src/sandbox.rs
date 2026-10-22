//! Where delegated agents run, and how they are fenced in.
//!
//! - Every job gets a fresh directory `<sandbox_root>/<job>/`.
//! - Code on a real repo: the repo (under `allowed_repo_roots`) is cloned
//!   into `<job>/repo` on a new branch `lucina/<job>`. When the job ends the
//!   branch is fetched back into the repo as `lucina/<job>`: never `main`,
//!   never pushed, never deployed. Undo: delete that branch and the sandbox.
//! - Processes run under bubblewrap: the system read-only; writable only the
//!   job directory and the agent's own state (`~/.claude`, `~/.codex`, with
//!   their instruction and settings files read-only); hidden: `~/Projects`
//!   (other repos), `~/scratch` (other jobs), `~/.ssh`, `~/.config`,
//!   `~/secrets`, keyrings, the user's systemd and D-Bus, the docker socket.
//!   Network stays (the agents need their APIs).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use crate::config::{DelegateConfig, Isolation};

/// A prepared workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// `<root>/<job>`: what the sandbox may write.
    pub dir: PathBuf,
    /// Where the agent starts (`dir`, or `dir/repo`).
    pub workdir: PathBuf,
    /// Code on a repo: the real repo and the branch.
    pub repo: Option<PathBuf>,
    pub branch: Option<String>,
}

pub fn branch_name(job_id: &str) -> String {
    format!("lucina/{}", &job_id[..job_id.len().min(8)])
}

/// A repo path the owner named: must be a git repo under an allowed root.
pub fn check_repo(cfg: &DelegateConfig, path: &str) -> Result<PathBuf, String> {
    let p = Path::new(path.trim());
    if !p.is_absolute() {
        return Err("the repo path must be absolute".into());
    }
    let canon = std::fs::canonicalize(p).map_err(|_| "no such folder".to_owned())?;
    if !cfg
        .allowed_repo_roots
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .any(|root| canon.starts_with(&root) && canon != root)
    {
        return Err("it is outside the allowed project folders".into());
    }
    if canon.starts_with(
        std::fs::canonicalize(&cfg.sandbox_root).unwrap_or_else(|_| cfg.sandbox_root.clone()),
    ) {
        return Err("it is a sandbox".into());
    }
    if !canon.join(".git").exists() {
        return Err("it is not a git repository".into());
    }
    Ok(canon)
}

async fn git(args: &[&str], cwd: &Path) -> Result<String, String> {
    let out = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(format!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(300)
                .collect::<String>()
        ))
    }
}

/// Make the job's directory (and the repo clone for code on a repo).
pub async fn prepare(
    cfg: &DelegateConfig,
    job_id: &str,
    repo: Option<&Path>,
) -> Result<Workspace, String> {
    let dir = cfg.sandbox_root.join(job_id);
    if dir.exists() {
        return Err("sandbox already exists".into());
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("sandbox: {e}"))?;
    let Some(repo) = repo else {
        return Ok(Workspace {
            workdir: dir.clone(),
            dir,
            repo: None,
            branch: None,
        });
    };
    let clone = dir.join("repo");
    let (src, dst) = (repo.to_string_lossy(), clone.to_string_lossy());
    git(&["clone", "--quiet", "--no-hardlinks", &src, &dst], &dir).await?;
    let branch = branch_name(job_id);
    git(&["checkout", "--quiet", "-b", &branch], &clone).await?;
    // No way back to the real repo from inside the sandbox.
    git(&["remote", "remove", "origin"], &clone).await?;
    Ok(Workspace {
        dir,
        workdir: clone,
        repo: Some(repo.to_path_buf()),
        branch: Some(branch),
    })
}

/// After a code job on a repo: the branch goes back into the real repo as
/// `lucina/<job>` (only when it has commits). Returns the undo command.
pub async fn publish_branch(ws: &Workspace) -> Result<Option<String>, String> {
    let (Some(repo), Some(branch)) = (&ws.repo, &ws.branch) else {
        return Ok(None);
    };
    if branch == "main" || branch == "master" || !branch.starts_with("lucina/") {
        return Err("refusing to publish a non-lucina branch".into());
    }
    let base = git(&["rev-parse", "HEAD"], repo).await?;
    let head = git(&["rev-parse", "HEAD"], &ws.workdir).await?;
    if base == head {
        return Ok(None);
    }
    let refspec = format!("{branch}:{branch}");
    let src = ws.workdir.to_string_lossy().into_owned();
    git(&["fetch", "--quiet", &src, &refspec], repo).await?;
    Ok(Some(format!(
        "git -C {} branch -D {branch} && rm -rf {}",
        repo.display(),
        ws.dir.display()
    )))
}

/// Remove what a job left: its sandbox (only under the root) and its branch.
pub async fn undo(
    cfg: &DelegateConfig,
    sandbox: Option<&str>,
    repo: Option<&str>,
    branch: Option<&str>,
) -> Result<Vec<String>, String> {
    let mut done = Vec::new();
    if let (Some(repo), Some(branch)) = (repo, branch) {
        if branch.starts_with("lucina/") {
            match git(&["branch", "-D", branch], Path::new(repo)).await {
                Ok(_) => done.push(format!("branch {branch} deleted")),
                Err(e) => done.push(format!("branch {branch}: {e}")),
            }
        }
    }
    if let Some(dir) = sandbox {
        let p = PathBuf::from(dir);
        let root = std::fs::canonicalize(&cfg.sandbox_root).unwrap_or(cfg.sandbox_root.clone());
        let canon = std::fs::canonicalize(&p).ok();
        match canon {
            Some(c) if c.starts_with(&root) && c != root => {
                std::fs::remove_dir_all(&c).map_err(|e| format!("sandbox: {e}"))?;
                done.push("sandbox removed".into());
            }
            Some(_) => return Err("not a sandbox".into()),
            None => done.push("sandbox already gone".into()),
        }
    }
    Ok(done)
}

/// What the sandboxed process may write besides its job directory.
#[derive(Debug, Clone, Default)]
pub struct Mounts {
    /// Writable (the agent's own state).
    pub rw: Vec<PathBuf>,
    /// Read-only on top of `rw` (instructions and settings the agent must not change).
    pub ro: Vec<PathBuf>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/home/nez"))
}

/// The bubblewrap arguments (before the program).
pub fn bwrap_args(workdir: &Path, writable: &Path, mounts: &Mounts) -> Vec<String> {
    let h = home();
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let mut a: Vec<String> = [
        "--die-with-parent",
        "--new-session",
        "--unshare-pid",
        "--unshare-ipc",
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
        "--tmpfs",
        "/tmp",
        "--tmpfs",
        "/run/user",
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    for sock in ["/run/docker.sock", "/var/run/docker.sock"] {
        if Path::new(sock).exists() {
            a.extend(["--ro-bind".into(), "/dev/null".into(), sock.into()]);
        }
    }
    for hidden in [
        "Projects",
        "scratch",
        ".ssh",
        ".config",
        "secrets",
        ".local/share/keyrings",
        ".infisical",
        ".docker",
        ".kube",
        ".gnupg",
        ".password-store",
        ".aws",
        "mnt",
    ] {
        let p = h.join(hidden);
        if p.is_dir() {
            a.extend(["--tmpfs".into(), s(&p)]);
        }
    }
    for f in [".git-credentials", ".netrc", ".npmrc", ".pypirc"] {
        let p = h.join(f);
        if p.is_file() {
            a.extend(["--ro-bind".into(), "/dev/null".into(), s(&p)]);
        }
    }
    a.extend(["--bind".into(), s(writable), s(writable)]);
    for p in &mounts.rw {
        if p.exists() {
            a.extend(["--bind".into(), s(p), s(p)]);
        }
    }
    for p in &mounts.ro {
        if p.exists() {
            a.extend(["--ro-bind".into(), s(p), s(p)]);
        }
    }
    a.extend([
        "--setenv".into(),
        "LUCINA_DELEGATION_DEPTH".into(),
        "1".into(),
        "--chdir".into(),
        s(workdir),
    ]);
    a
}

/// A finished process.
#[derive(Debug, Clone)]
pub struct Output {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

const MAX_OUTPUT: usize = 4 * 1024 * 1024;

async fn read_all<R: tokio::io::AsyncRead + Unpin>(pipe: Option<R>) -> String {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    if let Some(mut p) = pipe {
        let mut chunk = [0u8; 8192];
        loop {
            match p.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if buf.len() < MAX_OUTPUT {
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Run `program args` (under bubblewrap unless isolation is off), feeding
/// `stdin`, with a hard timeout that kills the whole process group.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    isolation: Isolation,
    program: &Path,
    args: &[String],
    workdir: &Path,
    writable: &Path,
    mounts: &Mounts,
    env: &[(String, String)],
    stdin: &str,
    timeout: Duration,
) -> Result<Output, String> {
    let mut cmd = match isolation {
        Isolation::Bwrap => {
            let mut c = tokio::process::Command::new("bwrap");
            c.args(bwrap_args(workdir, writable, mounts));
            c.arg("--").arg(program).args(args);
            c
        }
        Isolation::None => {
            let mut c = tokio::process::Command::new(program);
            c.args(args).current_dir(workdir);
            c.env("LUCINA_DELEGATION_DEPTH", "1");
            c
        }
    };
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    let pid = child.id();
    if let Some(mut si) = child.stdin.take() {
        let input = stdin.to_owned();
        tokio::spawn(async move {
            let _ = si.write_all(input.as_bytes()).await;
            let _ = si.shutdown().await;
        });
    }
    let so = child.stdout.take();
    let se = child.stderr.take();
    let out_task = tokio::spawn(read_all(so));
    let err_task = tokio::spawn(read_all(se));
    let (status, timed_out) = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(s)) => (s.code(), false),
        Ok(Err(e)) => return Err(format!("wait: {e}")),
        Err(_) => {
            if let Some(pid) = pid {
                // The whole group (bwrap or the agent and its children).
                let _ = tokio::process::Command::new("kill")
                    .args(["-KILL", "--", &format!("-{pid}")])
                    .status()
                    .await;
            }
            let _ = child.kill().await;
            (None, true)
        }
    };
    let stdout = out_task.await.unwrap_or_default();
    let stderr = err_task.await.unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
        timed_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(root: &Path, projects: &Path) -> DelegateConfig {
        DelegateConfig {
            sandbox_root: root.to_path_buf(),
            allowed_repo_roots: vec![projects.to_path_buf()],
            ..DelegateConfig::default()
        }
    }

    async fn sh(cmd: &str, cwd: &Path) {
        let ok = tokio::process::Command::new("sh")
            .args(["-c", cmd])
            .current_dir(cwd)
            .status()
            .await
            .unwrap()
            .success();
        assert!(ok, "{cmd}");
    }

    #[tokio::test]
    async fn repo_jobs_work_on_a_clone_and_leave_a_lucina_branch() {
        let t = tempfile::tempdir().unwrap();
        let projects = t.path().join("Projects");
        let repo = projects.join("demo");
        std::fs::create_dir_all(&repo).unwrap();
        sh("git init -q -b main && git -c user.name=t -c user.email=t@t commit -q --allow-empty -m init", &repo).await;
        let root = t.path().join("sandboxes");
        let c = cfg(&root, &projects);
        assert!(check_repo(&c, "relative/path").is_err());
        assert!(
            check_repo(&c, t.path().to_str().unwrap()).is_err(),
            "outside the roots"
        );
        assert!(
            check_repo(&c, projects.to_str().unwrap()).is_err(),
            "the root itself"
        );
        let canon = check_repo(&c, repo.to_str().unwrap()).unwrap();
        let ws = prepare(&c, "abcdef0123456789", Some(&canon)).await.unwrap();
        assert_eq!(ws.branch.as_deref(), Some("lucina/abcdef01"));
        assert!(ws.workdir.join(".git").exists());
        assert!(
            git(&["remote"], &ws.workdir).await.unwrap().is_empty(),
            "no origin"
        );
        // nothing committed: nothing published
        assert_eq!(publish_branch(&ws).await.unwrap(), None);
        sh("echo hi > a.txt && git add a.txt && git -c user.name=t -c user.email=t@t commit -q -m work", &ws.workdir).await;
        let undo_cmd = publish_branch(&ws).await.unwrap().unwrap();
        assert!(undo_cmd.contains("branch -D lucina/abcdef01"));
        let branches = git(&["branch", "--list"], &canon).await.unwrap();
        assert!(
            branches.contains("lucina/abcdef01") && branches.contains("* main"),
            "{branches}"
        );
        // main untouched
        assert_eq!(
            git(&["log", "--oneline", "main"], &canon)
                .await
                .unwrap()
                .lines()
                .count(),
            1
        );
        let done = undo(
            &c,
            Some(ws.dir.to_str().unwrap()),
            Some(canon.to_str().unwrap()),
            ws.branch.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(done.len(), 2, "{done:?}");
        assert!(!ws.dir.exists());
        assert!(!git(&["branch", "--list"], &canon)
            .await
            .unwrap()
            .contains("lucina/"));
        // undo refuses anything outside the sandbox root
        assert!(undo(&c, Some(repo.to_str().unwrap()), None, None)
            .await
            .is_err());
        assert!(repo.exists());
    }

    #[tokio::test]
    async fn runs_with_a_timeout() {
        let t = tempfile::tempdir().unwrap();
        let out = run(
            Isolation::None,
            Path::new("sh"),
            &[
                "-c".into(),
                "cat; echo depth=$LUCINA_DELEGATION_DEPTH".into(),
            ],
            t.path(),
            t.path(),
            &Mounts::default(),
            &[],
            "hello\n",
            Duration::from_secs(10),
        )
        .await
        .unwrap();
        assert_eq!(out.stdout, "hello\ndepth=1\n");
        let out = run(
            Isolation::None,
            Path::new("sh"),
            &["-c".into(), "sleep 30".into()],
            t.path(),
            t.path(),
            &Mounts::default(),
            &[],
            "",
            Duration::from_millis(300),
        )
        .await
        .unwrap();
        assert!(out.timed_out);
    }

    #[test]
    fn bwrap_hides_the_rest_of_home() {
        let a = bwrap_args(Path::new("/w/x"), Path::new("/w"), &Mounts::default()).join(" ");
        assert!(a.contains("--ro-bind / /"));
        assert!(a.contains("--bind /w /w"));
        assert!(a.contains("--tmpfs /run/user"));
        assert!(a.contains("--chdir /w/x"));
        assert!(a.contains("LUCINA_DELEGATION_DEPTH 1"));
    }
}
