//! Parity of the two sandboxes on the real `/usr/bin/bwrap` (Linux only): what
//! `lucina_delegate::sandbox::bwrap_args` hides, `lucina_agents::sandbox::to_bwrap_args` must hide
//! too, so moving the delegate onto the shared module later cannot lose a refusal.
//!
//! One throwaway home under `$TMPDIR` with the same marker files for both views, and a second
//! process holding a marker in its environment. The same probe script runs in the old view (a job
//! directory inside that home) and in the new one (a worktree inside it). A marker the old view
//! hides and the new one shows fails the test, naming the marker; a marker only the new view
//! hides is printed as a finding about the old sandbox. Never skipped: a missing `bwrap` or user
//! namespaces that are off FAIL the test.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lucina_agents::sandbox::{to_bwrap_args, CostClass, Policy, RunInputs};
use lucina_delegate::sandbox::{bwrap_args, Mounts};

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
fn git(home: &Path, cwd: &Path, args: &[&str]) {
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
}

fn run_limited(mut cmd: Command, limit: Duration) -> (Option<i32>, String, String) {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
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
        "bubblewrap is missing: {BWRAP} does not exist, so the sandboxes cannot be compared on this host"
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

/// The probe script: for each canary, `<name>=shown` or `<name>=hidden`; plus the process canary.
fn script(canaries: &[(&str, PathBuf)]) -> String {
    let mut s = String::from(
        "r() { if cat \"$2\" >/dev/null 2>&1; then echo \"$1=shown\"; else echo \"$1=hidden\"; fi; }\n",
    );
    for (name, path) in canaries {
        s.push_str(&format!("r {name} '{}'\n", path.display()));
    }
    s.push_str(
        "found=hidden\n\
         for f in /proc/[0-9]*/environ; do\n\
           if tr '\\0' '\\n' < \"$f\" 2>/dev/null | grep -qF \"$P1$P2\"; then found=shown; fi\n\
         done\n\
         echo \"process-environment=$found\"\n\
         r own-work-file ./work.txt\n",
    );
    s
}

fn parse(out: &str) -> BTreeMap<String, String> {
    out.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

#[test]
fn the_new_sandbox_hides_everything_the_delegate_sandbox_hides() {
    preflight();
    let tmp_root = std::fs::canonicalize(std::env::temp_dir()).expect("TMPDIR exists");
    assert!(
        !tmp_root.starts_with("/tmp"),
        "set TMPDIR outside /tmp ({}): the new fence refuses host /tmp paths",
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
        root: tmp_root.join(format!("lucina-parity-{nonce}")),
        child: None,
    };
    let h = scratch.root.join("home");
    std::fs::create_dir_all(&h).unwrap();
    let h = std::fs::canonicalize(&h).unwrap();
    let marker = |kind: &str| format!("LUCINA-PARITY-MARKER-{kind}-{nonce}");

    // The canaries, the same for both views.
    let canaries: Vec<(&str, PathBuf)> = vec![
        ("ssh-key", h.join(".ssh/id")),
        ("pokoin-env", h.join(".config/pokoin/x.env")),
        ("gh-hosts", h.join(".config/gh/hosts.yml")),
        ("git-credentials", h.join(".git-credentials")),
        ("cargo-credentials", h.join(".cargo/credentials.toml")),
        (
            "agentd-ids",
            h.join(".local/share/lucina-agentd/ids/ids.jsonl"),
        ),
        (
            "other-lane-home",
            h.join(".local/share/lane-homes/zen/token"),
        ),
        (
            "sibling-project",
            h.join("Projects/Work/sibling/secret.txt"),
        ),
        ("main-checkout", h.join("Projects/Work/demo/README")),
        ("claude-state", h.join(".claude/.credentials.json")),
        ("netrc", h.join(".netrc")),
        ("gnupg", h.join(".gnupg/private-keys-v1.d/k.key")),
        ("docker-config", h.join(".docker/config.json")),
        ("other-scratch", h.join("scratch/other-run/out.txt")),
    ];
    for (name, path) in &canaries {
        if *name != "main-checkout" {
            write(path, &marker(name));
        }
    }

    // The repository (its README is the main-checkout canary) and a worktree for the new view.
    let repo = h.join("Projects/Work/demo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&h, &repo, &["init", "-q", "-b", "main"]);
    write(&repo.join("README"), &marker("main-checkout"));
    git(&h, &repo, &["add", "README"]);
    git(&h, &repo, &["commit", "-q", "-m", "init"]);
    let ws = h.join("Projects/.worktrees/demo/run1");
    std::fs::create_dir_all(ws.parent().unwrap()).unwrap();
    git(
        &h,
        &repo,
        &["worktree", "add", "-q", "-b", "run1", ws.to_str().unwrap()],
    );
    std::fs::create_dir_all(repo.join(".git/info")).unwrap();
    write(&ws.join("work.txt"), "work");
    std::fs::create_dir_all(h.join(".local/lib/lucina-shims")).unwrap();
    let run_tmp = h.join("scratch/tmp/run-1");
    std::fs::create_dir_all(&run_tmp).unwrap();
    // The old view's job directory.
    let job = h.join("jobs/job-1");
    write(&job.join("work.txt"), "work");

    // A second process with a marker in its environment.
    let proc_marker = marker("process");
    let child = Command::new("/usr/bin/sleep")
        .arg("300")
        .env_clear()
        .env("LUCINA_PARITY_PROBE", &proc_marker)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    scratch.child = Some(child);
    let (p1, p2) = proc_marker.split_at(proc_marker.len() / 2);
    let probe = script(&canaries);

    // Old view. `bwrap_args` reads HOME from this process: point it at the throwaway home
    // (this test is the only one in its binary, so nothing else reads HOME concurrently).
    std::env::set_var("HOME", &h);
    let old_args = bwrap_args(&job, &job, &Mounts::default());
    let mut cmd = Command::new(BWRAP);
    cmd.args(&old_args)
        .arg("--")
        .args(["/bin/sh", "-c", &probe])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &h)
        .env("P1", p1)
        .env("P2", p2);
    let (status, out, err) = run_limited(cmd, Duration::from_secs(60));
    assert_eq!(status, Some(0), "old view: bwrap failed: {err}");
    let old = parse(&out);
    assert_eq!(
        old.get("own-work-file").map(String::as_str),
        Some("shown"),
        "old view did not run in its job directory: {out} {err}"
    );

    // New view.
    let policy = Policy::from_toml(POLICY).unwrap();
    let run = RunInputs {
        home: h.clone(),
        workspace: ws.clone(),
        common_git_dir: repo.join(".git"),
        worktree_git_dir: Some(repo.join(".git/worktrees/run1")),
        lane: "qoder".into(),
        cost: CostClass::Light,
        tmpdir: run_tmp,
        extra_ro: Vec::new(),
        env: vec![("P1".into(), p1.into()), ("P2".into(), p2.into())],
    };
    let new_args = to_bwrap_args(&policy, &run).unwrap();
    let mut cmd = Command::new(BWRAP);
    cmd.args(&new_args)
        .args(["/bin/sh", "-c", &probe])
        .env_clear();
    let (status, out, err) = run_limited(cmd, Duration::from_secs(60));
    assert_eq!(status, Some(0), "new view: bwrap failed: {err}");
    let new = parse(&out);
    assert_eq!(
        new.get("own-work-file").map(String::as_str),
        Some("shown"),
        "new view did not run in its workspace: {out} {err}"
    );

    let mut names: Vec<&str> = canaries.iter().map(|(n, _)| *n).collect();
    names.push("process-environment");
    let mut lost = Vec::new();
    let mut findings = Vec::new();
    for name in names {
        let o = old.get(name).map(String::as_str);
        let n = new.get(name).map(String::as_str);
        assert!(
            o.is_some() && n.is_some(),
            "canary {name} missing from a view's output"
        );
        if o == Some("hidden") && n != Some("hidden") {
            lost.push(name);
        }
        if n == Some("hidden") && o != Some("hidden") {
            findings.push(name);
        }
    }
    eprintln!("hidden by the new sandbox but shown by the delegate's: {findings:?}");
    assert!(
        lost.is_empty(),
        "the new sandbox shows what the delegate's hides: {lost:?}"
    );
}
