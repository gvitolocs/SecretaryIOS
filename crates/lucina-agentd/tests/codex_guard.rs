//! The owner rule: Codex is never used on Lucina projects. `lagent run --provider codex`
//! must be refused for every Lucina project (main checkout, worktree, the extra
//! `LUCINA_AGENTD_CODEX_DENY_REPOS` list), whatever the caller — on a temporary
//! projects root and worktrees root.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::Daemon;
use serde_json::{json, Value};

struct Env {
    root: tempfile::TempDir,
    d: Daemon,
}

impl Env {
    fn start(extra: &[(&str, String)]) -> Env {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("proj")).unwrap();
        let mut env = vec![
            ("WT_ROOT", root.path().join("wt").display().to_string()),
            (
                "WT_PROJECTS",
                root.path().join("proj").display().to_string(),
            ),
        ];
        env.extend(extra.iter().cloned());
        let d = Daemon::start_with_env(&env);
        Env { root, d }
    }

    /// A repo at `root/<rel>` with one commit.
    fn repo(&self, rel: &str) -> PathBuf {
        let r = self.root.path().join(rel);
        std::fs::create_dir_all(&r).unwrap();
        git(&r, &["init", "-q", "-b", "main"]);
        std::fs::write(r.join("README"), "hi\n").unwrap();
        git(&r, &["add", "."]);
        git(
            &r,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );
        r
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Assert that a codex run in `cwd` is refused with the exact owner-rule message for `repo`.
/// `lagent` wraps the daemon error: `error.details` carries the code, `error.message` the
/// prefixed message; two human lines come before the JSON on stderr.
fn expect_refused(d: &Daemon, cwd: &Path, repo: &str) {
    let r = d.lagent(&[
        "run",
        "--provider",
        "codex",
        "--cwd",
        &cwd.display().to_string(),
        "prompt",
        "--json",
    ]);
    let start = r.stderr.find('{').expect("no json on stderr");
    let err: Value = serde_json::from_str(&r.stderr[start..])
        .unwrap_or_else(|e| panic!("lagent stderr not json: {e}: {}", r.stderr));
    assert_eq!(
        (err["error"]["details"].clone(), err["error"]["message"].clone()),
        (
            json!("CODEX_NOT_ALLOWED"),
            json!(format!(
                "Failed to create agent: Codex is not used on Lucina projects (owner rule). Pick Claude, Mistral or Qwen for {repo}."
            )),
        ),
        "{}",
        r.stderr
    );
}

#[test]
fn codex_is_refused_on_lucina_projects() {
    let e = Env::start(&[]);
    // Rule 1: the main checkout under <projects_root>/Lucina/.
    let foo = e.repo("proj/Lucina/Foo");
    expect_refused(&e.d, &foo, "Foo");
    // A nested path inside it counts too.
    let deep = foo.join("some/nested/dir");
    std::fs::create_dir_all(&deep).unwrap();
    expect_refused(&e.d, &deep, "Foo");
    // Rule 2: a worktree under <wt_root>/Foo/ while <projects_root>/Lucina/Foo exists.
    let wt = e.root.path().join("wt/Foo/x");
    git(
        &foo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "x",
            &wt.display().to_string(),
        ],
    );
    expect_refused(&e.d, &wt, "Foo");
    // A non-Lucina project under the projects root is not refused by this rule
    // (codex is not registered on the dev daemon, so the run still fails - just not
    // with the owner rule).
    let bar = e.repo("proj/Other/Bar");
    let r = e.d.lagent(&[
        "run",
        "--provider",
        "codex",
        "--cwd",
        &bar.display().to_string(),
        "prompt",
        "--json",
    ]);
    assert!(!r.stderr.contains("CODEX_NOT_ALLOWED"), "{}", r.stderr);
    // And so is a worktree of it: <wt_root>/Foo exists, but <projects_root>/Lucina/Other does not.
    let bar = e.repo("proj/Other/Other");
    let wt = e.root.path().join("wt/Other/x");
    git(
        &bar,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "x",
            &wt.display().to_string(),
        ],
    );
    let r = e.d.lagent(&[
        "run",
        "--provider",
        "codex",
        "--cwd",
        &wt.display().to_string(),
        "prompt",
        "--json",
    ]);
    assert!(!r.stderr.contains("CODEX_NOT_ALLOWED"), "{}", r.stderr);
}

#[test]
fn codex_deny_repos_extra_list() {
    let e = Env::start(&[("LUCINA_AGENTD_CODEX_DENY_REPOS", "ExtraRepo".into())]);
    let r = e.repo("proj/Other/ExtraRepo");
    expect_refused(&e.d, &r, "ExtraRepo");
}
