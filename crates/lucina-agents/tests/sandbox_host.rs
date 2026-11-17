//! The worker sandbox on the real `/usr/bin/bwrap` (Linux only).
//!
//! Builds a throwaway home under `$TMPDIR` with marker files where secrets live, a real git
//! repository with a worktree as the workspace, and a second process holding a marker in its
//! environment; runs small shell commands inside the fence the generator describes and checks
//! each register row. Only files and processes this test creates are looked at. It never skips:
//! a missing `bwrap` or user namespaces that are off FAIL the test with a message saying so.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lucina_agents::sandbox::{bound_paths, to_bwrap_args, CostClass, Hidden, Policy, RunInputs};

const POLICY: &str = include_str!("../../../deploy/lucina-agentd/sandbox-policy.toml");
const BWRAP: &str = "/usr/bin/bwrap";

/// Removes the throwaway tree and stops the marker process, on success and on panic.
struct Scratch {
    root: PathBuf,
    child: Option<Child>,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// git on the host for the setup, with no global or system config (never the owner's).
fn git(home: &Path, cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Setup")
        .env("GIT_AUTHOR_EMAIL", "setup@example.invalid")
        .env("GIT_COMMITTER_NAME", "Setup")
        .env("GIT_COMMITTER_EMAIL", "setup@example.invalid")
        .stdin(Stdio::null())
        .output()
        .expect("run /usr/bin/git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// Runs `cmd` with a time limit; returns (status, stdout, stderr).
fn run_limited(mut cmd: Command, limit: Duration) -> (Option<i32>, String, String) {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn preflight() {
    assert!(
        Path::new(BWRAP).exists(),
        "bubblewrap is missing: {BWRAP} does not exist, so the sandbox cannot be checked on this host"
    );
    let mut probe = Command::new(BWRAP);
    probe
        .args([
            "--unshare-user",
            "--unshare-pid",
            "--ro-bind",
            "/usr",
            "/usr",
            "--symlink",
            "usr/lib",
            "/lib",
            "--symlink",
            "usr/lib64",
            "/lib64",
            "--symlink",
            "usr/bin",
            "/bin",
            "--proc",
            "/proc",
            "--",
            "/usr/bin/true",
        ])
        .env_clear();
    let (status, _, err) = run_limited(probe, Duration::from_secs(30));
    assert_eq!(
        status,
        Some(0),
        "bubblewrap cannot create a user namespace here (unprivileged user namespaces off?): {err}"
    );
}

const SCRIPT: &str = r##"
r() { if eval "$2" >/dev/null 2>&1; then echo "$1=yes"; else echo "$1=no"; fi; }
r commit 'echo fenced > fenced.txt && git add fenced.txt && git commit -q -m fenced'
r ssh_read 'cat "$HOME/.ssh/id"'
r ssh_list 'ls "$HOME/.ssh"'
r pokoin_read 'cat "$HOME/.config/pokoin/x.env"'
r config_list 'ls "$HOME/.config"'
r gh_read 'cat "$HOME/.config/gh/hosts.yml"'
r gitcred_read 'cat "$HOME/.git-credentials"'
r cargocred_read 'cat "$HOME/.cargo/credentials.toml"'
r agentd_read 'cat "$HOME/.local/share/lucina-agentd/ids/ids.jsonl"'
r agentd_list 'ls "$HOME/.local/share/lucina-agentd"'
r otherlane_read 'cat "$HOME/.local/share/lane-homes/zen/token"'
r ownlane_read 'cat "$LUCINA_LANE_HOME/token-own"'
r sibling_read 'cat "$HOME/Projects/Work/sibling/secret.txt"'
r mainfile_read 'cat "$HOME/Projects/Work/demo/README"'
echo "home_ls=$(ls -A "$HOME" | tr '\n' ' ')"
r probe_pid 'test -e "/proc/$PROBE_PID"'
found=no
for f in /proc/[0-9]*/environ; do
  if tr '\0' '\n' < "$f" 2>/dev/null | grep -qF "$P1$P2"; then found=yes; fi
done
echo "probe_env=$found"
echo "procs=$(ls -d /proc/[0-9]* | wc -l)"
r outer_env 'test -n "$LUCINA_FENCE_OUTER"'
r tmp_write 'echo x > "/tmp/$TMP_PROBE"'
r runtmp_write 'echo x > "$TMPDIR/$TMP_PROBE"'
r home_write 'echo x > "$HOME/$TMP_PROBE"'
r usr_write 'touch /usr/lucina-fence-probe'
r etc_write 'touch /etc/lucina-fence-probe'
r common_write 'touch "$COMMON/lucina-fence-probe"'
r sibling_write 'mkdir -p "$HOME/Projects/Work/sibling" && touch "$HOME/Projects/Work/sibling/written"'
r hook_write 'printf "#!/bin/sh\n" > "$COMMON/hooks/post-commit"'
r config_append 'echo "[core]" >> "$COMMON/config"'
r git_remote 'git config remote.origin.url https://example.invalid/x.git'
r commondir_write 'echo /elsewhere > "$OWN/commondir"'
r gitfile_write 'echo "gitdir: /elsewhere" > .git'
r symlink_read 'cat peek'
"##;

#[test]
fn the_fence_holds_on_the_real_bubblewrap() {
    preflight();
    let tmp_root = std::fs::canonicalize(std::env::temp_dir()).expect("TMPDIR exists");
    assert!(
        !tmp_root.starts_with("/tmp"),
        "set TMPDIR outside /tmp ({}): the fence refuses host /tmp paths, and nothing of this test goes there",
        tmp_root.display()
    );
    let nonce = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mut scratch = Scratch {
        root: tmp_root.join(format!("lucina-fence-{nonce}")),
        child: None,
    };
    let h = scratch.root.join("home");
    std::fs::create_dir_all(&h).unwrap();
    let h = std::fs::canonicalize(&h).unwrap();
    let marker = |kind: &str| format!("LUCINA-FENCE-MARKER-{kind}-{nonce}");

    // Marker files where secrets live.
    write(&h.join(".ssh/id"), &marker("ssh"));
    write(&h.join(".config/pokoin/x.env"), &marker("pokoin"));
    write(&h.join(".config/gh/hosts.yml"), &marker("gh"));
    write(&h.join(".git-credentials"), &marker("gitcred"));
    write(&h.join(".cargo/credentials.toml"), &marker("cargocred"));
    write(
        &h.join(".local/share/lucina-agentd/ids/ids.jsonl"),
        &marker("agentd"),
    );
    write(
        &h.join(".local/share/lane-homes/zen/token"),
        &marker("otherlane"),
    );
    write(
        &h.join("Projects/Work/sibling/secret.txt"),
        &marker("sibling"),
    );
    // What the run is given.
    write(&h.join(".local/share/lane-homes/qoder/token-own"), "own");
    std::fs::create_dir_all(h.join(".local/lib/lucina-shims")).unwrap();
    std::fs::create_dir_all(h.join(".cargo/bin")).unwrap();
    let run_tmp = h.join("scratch/tmp/run-1");
    std::fs::create_dir_all(&run_tmp).unwrap();

    // A repository and a worktree of it.
    let repo = h.join("Projects/Work/demo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&h, &repo, &["init", "-q", "-b", "main"]);
    write(&repo.join("README"), &marker("mainfile"));
    git(&h, &repo, &["add", "README"]);
    git(&h, &repo, &["commit", "-q", "-m", "init"]);
    let ws = h.join("Projects/.worktrees/demo/run1");
    std::fs::create_dir_all(ws.parent().unwrap()).unwrap();
    git(
        &h,
        &repo,
        &["worktree", "add", "-q", "-b", "run1", ws.to_str().unwrap()],
    );
    let common = repo.join(".git");
    let own = common.join("worktrees/run1");
    assert!(own.join("commondir").exists() && common.join("hooks").is_dir());
    std::fs::create_dir_all(common.join("info")).unwrap();
    std::os::unix::fs::symlink(h.join(".ssh/id"), ws.join("peek")).unwrap();
    let config_before = std::fs::read(common.join("config")).unwrap();

    // A second process with a marker in its environment.
    let proc_marker = marker("proc");
    let child = Command::new("/usr/bin/sleep")
        .arg("300")
        .env_clear()
        .env("LUCINA_FENCE_PROBE", &proc_marker)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let probe_pid = child.id();
    scratch.child = Some(child);
    let (p1, p2) = proc_marker.split_at(proc_marker.len() / 2);
    let tmp_probe = format!("lucina-fence-probe-{nonce}");

    let policy = Policy::from_toml(POLICY).unwrap();
    let run = RunInputs {
        home: h.clone(),
        workspace: ws.clone(),
        common_git_dir: common.clone(),
        worktree_git_dir: Some(own.clone()),
        lane: "qoder".into(),
        cost: CostClass::Light,
        tmpdir: run_tmp.clone(),
        extra_ro: Vec::new(),
        env: vec![
            ("GIT_AUTHOR_NAME".into(), "Fenced".into()),
            ("GIT_AUTHOR_EMAIL".into(), "fenced@example.invalid".into()),
            ("GIT_COMMITTER_NAME".into(), "Fenced".into()),
            (
                "GIT_COMMITTER_EMAIL".into(),
                "fenced@example.invalid".into(),
            ),
            ("COMMON".into(), common.to_str().unwrap().into()),
            ("OWN".into(), own.to_str().unwrap().into()),
            ("PROBE_PID".into(), probe_pid.to_string()),
            ("P1".into(), p1.into()),
            ("P2".into(), p2.into()),
            ("TMP_PROBE".into(), tmp_probe.clone()),
        ],
    };
    let args = to_bwrap_args(&policy, &run).unwrap();

    // The caller's duty: resolve each bound source on the host and check the real path again.
    let hidden = Hidden::for_run(&policy, &h, "qoder").unwrap();
    for src in bound_paths(&args) {
        if let Ok(real) = std::fs::canonicalize(src) {
            hidden
                .check("the resolved bind source", &real)
                .unwrap_or_else(|e| panic!("{e}"));
        }
    }

    let mut cmd = Command::new(BWRAP);
    cmd.args(&args)
        .args(["/bin/sh", "-c", SCRIPT])
        .env_clear()
        .env("LUCINA_FENCE_OUTER", "outer");
    let (status, out, err) = run_limited(cmd, Duration::from_secs(120));
    assert_eq!(status, Some(0), "bwrap failed: {err}\n{out}");
    assert!(
        !out.contains("LUCINA-FENCE-MARKER") && !err.contains("LUCINA-FENCE-MARKER"),
        "register: \"no read access to secrets\": a marker reached the output"
    );
    let got: BTreeMap<&str, &str> = out.lines().filter_map(|l| l.split_once('=')).collect();
    let is = |k: &str, want: &str, row: &str| {
        assert_eq!(
            got.get(k).copied(),
            Some(want),
            "register: \"{row}\" ({k}); stderr: {err}"
        );
    };

    is("ssh_read", "no", "no read access to SSH keys");
    is("ssh_list", "no", "no read access to SSH keys");
    is(
        "pokoin_read",
        "no",
        "no read access to the owner's environment files",
    );
    is(
        "config_list",
        "no",
        "no read access to tool tokens and sign-in files",
    );
    is(
        "gh_read",
        "no",
        "no read access to tool tokens and sign-in files",
    );
    is(
        "gitcred_read",
        "no",
        "no read access to tool tokens and sign-in files",
    );
    is(
        "cargocred_read",
        "no",
        "no read access to tool tokens and sign-in files",
    );
    is("agentd_read", "no", "no read or write of agentd's state");
    is("agentd_list", "no", "no read or write of agentd's state");
    is(
        "otherlane_read",
        "no",
        "no read access to other tools' sign-in files",
    );
    is("ownlane_read", "yes", "a lane reaches its own sign-in home");
    is(
        "sibling_read",
        "no",
        "no read of another run's workspace or other repositories",
    );
    is(
        "mainfile_read",
        "no",
        "no read of another run's workspace or other repositories",
    );
    let listed: Vec<&str> = got
        .get("home_ls")
        .copied()
        .unwrap_or("")
        .split_whitespace()
        .collect();
    for name in &listed {
        assert!(
            [".cargo", ".local", "Projects", "scratch"].contains(name),
            "register: \"no read access to the home\": the home lists {name} ({listed:?})"
        );
    }
    is(
        "probe_pid",
        "no",
        "no view of other processes or their environments",
    );
    is(
        "probe_env",
        "no",
        "no view of other processes or their environments",
    );
    let procs: usize = got
        .get("procs")
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(999);
    assert!(
        procs < 10,
        "register: \"no view of other processes or their environments\": {procs} processes visible"
    );
    is("outer_env", "no", "an environment cleared to the allowlist");
    is("tmp_write", "yes", "a private /tmp");
    assert!(
        !Path::new("/tmp").join(&tmp_probe).exists() && !tmp_root.join(&tmp_probe).exists(),
        "register: \"nothing in the host's /tmp\": the write to /tmp inside reached the host"
    );
    is(
        "runtmp_write",
        "yes",
        "the run's own temp directory is writable",
    );
    assert!(run_tmp.join(&tmp_probe).exists());
    assert!(
        !h.join(&tmp_probe).exists(),
        "register: \"no writes outside the workspace\": a write to the home reached the host"
    );
    is("usr_write", "no", "no writes outside the workspace");
    is("etc_write", "no", "no writes outside the workspace");
    is("common_write", "no", "no writes outside the workspace");
    assert!(
        !h.join("Projects/Work/sibling/written").exists(),
        "register: \"no writes outside the workspace\": a write to a sibling project reached the host"
    );
    is(
        "commit",
        "yes",
        "workspace writes and commits work inside the fence",
    );
    assert_eq!(git(&h, &ws, &["log", "-1", "--format=%s"]), "fenced");
    assert!(ws.join("fenced.txt").exists());
    is("hook_write", "no", "no hook planted in the repository");
    assert!(!common.join("hooks/post-commit").exists());
    is(
        "config_append",
        "no",
        "no change to the repository's config",
    );
    is("git_remote", "no", "no change to the repository's config");
    assert_eq!(
        std::fs::read(common.join("config")).unwrap(),
        config_before,
        "register: \"no change to the repository's config\""
    );
    is(
        "commondir_write",
        "no",
        "no redirect of the worktree's git directory",
    );
    is(
        "gitfile_write",
        "no",
        "no redirect of the worktree's git directory",
    );
    is(
        "symlink_read",
        "no",
        "no read access to SSH keys (through a symlink in the workspace)",
    );
}
