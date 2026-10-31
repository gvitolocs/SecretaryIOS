//! Today's incident replayed on real git repos: a live build that is not an
//! ancestor of the deploy is refused; the fixed procedure passes.

use std::path::{Path, PathBuf};
use std::process::Command;

struct Repo {
    dir: PathBuf,
    ledger: PathBuf,
    lock: PathBuf,
}

fn sh(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl Repo {
    fn new(name: &str) -> Repo {
        let root =
            std::env::temp_dir().join(format!("deploy-guard-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("repo");
        std::fs::create_dir_all(&dir).unwrap();
        sh(&dir, &["init", "-q", "-b", "main"]);
        sh(&dir, &["config", "user.email", "t@example.test"]);
        sh(&dir, &["config", "user.name", "t"]);
        let r = Repo {
            dir,
            ledger: root.join("ledger"),
            lock: root.join("lock"),
        };
        r.commit("base");
        r
    }

    fn commit(&self, file: &str) -> String {
        std::fs::write(self.dir.join(file), file).unwrap();
        sh(&self.dir, &["add", file]);
        sh(&self.dir, &["commit", "-q", "-m", file]);
        sh(&self.dir, &["rev-parse", "HEAD"])
    }

    fn guard(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_deploy-guard"))
            .args(args)
            .args([
                "--repo",
                self.dir.to_str().unwrap(),
                "--lock",
                self.lock.to_str().unwrap(),
            ])
            .env("DEPLOY_GUARD_LEDGER", &self.ledger)
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    }

    fn take_lock(&self) {
        std::fs::create_dir_all(&self.lock).unwrap();
    }
}

#[test]
fn todays_calls_regression_is_refused_and_the_fix_passes() {
    let r = Repo::new("calls");
    r.take_lock();
    // 24 Oct: production = a commit with calls (here a side branch, as if built from the dirty checkout).
    sh(&r.dir, &["checkout", "-q", "-b", "wip/calls"]);
    let calls = r.commit("calls.rs");
    sh(&r.dir, &["checkout", "-q", "main"]);
    let (rc, out) = r.guard(&[
        "record",
        "--runtime",
        "secretary-server",
        "--commit",
        &calls,
        "--bootstrap",
        "--target",
        "wip/calls",
    ]);
    assert_eq!(rc, 0, "{out}");
    // 25 Oct: Lucina Ink merges into a main without calls and deploys it.
    let ink = r.commit("ink.rs");
    let (rc, out) = r.guard(&["check", "--runtime", "secretary-server", "--target", "main"]);
    assert_eq!(rc, 1, "must refuse: {out}");
    assert!(out.contains("not an ancestor"), "{out}");
    // The fix: calls onto main first, then the build from main's tip passes.
    sh(&r.dir, &["merge", "-q", "--no-edit", "wip/calls"]);
    let (rc, out) = r.guard(&["check", "--runtime", "secretary-server", "--target", "main"]);
    assert_eq!(rc, 0, "{out}");
    let (rc, out) = r.guard(&[
        "record",
        "--runtime",
        "secretary-server",
        "--target",
        "main",
    ]);
    assert_eq!(rc, 0, "{out}");
    let _ = ink;
}

#[test]
fn dirty_tree_branch_build_and_missing_lock_are_refused() {
    let r = Repo::new("rules");
    let base = sh(&r.dir, &["rev-parse", "HEAD"]);
    let (rc, out) = r.guard(&[
        "record",
        "--runtime",
        "web",
        "--commit",
        &base,
        "--bootstrap",
    ]);
    assert_eq!(rc, 0, "{out}");
    // No lock.
    let (rc, out) = r.guard(&["check", "--runtime", "web"]);
    assert_eq!(rc, 1, "{out}");
    assert!(
        out.contains("lucina-merge.lock") || out.contains("not held"),
        "{out}"
    );
    r.take_lock();
    // Uncommitted live work in the checkout.
    std::fs::write(r.dir.join("Desk.tsx"), "x").unwrap();
    let (rc, out) = r.guard(&["check", "--runtime", "web"]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("uncommitted or untracked"), "{out}");
    std::fs::remove_file(r.dir.join("Desk.tsx")).unwrap();
    // A feature branch not merged into main.
    sh(&r.dir, &["checkout", "-q", "-b", "feature"]);
    r.commit("feature.rs");
    let (rc, out) = r.guard(&["check", "--runtime", "web"]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("is not on main"), "{out}");
    // Merged: allowed.
    sh(&r.dir, &["checkout", "-q", "main"]);
    sh(&r.dir, &["merge", "-q", "--no-edit", "feature"]);
    let (rc, out) = r.guard(&["check", "--runtime", "web"]);
    assert_eq!(rc, 0, "{out}");
}

#[test]
fn no_ledger_entry_asks_for_a_bootstrap_record() {
    let r = Repo::new("ledger");
    r.take_lock();
    let (rc, out) = r.guard(&["check", "--runtime", "lucina-rs", "--target", "main"]);
    assert_eq!(rc, 3, "{out}");
    assert!(out.contains("--bootstrap"), "{out}");
}

#[test]
fn a_retired_line_target_is_refused() {
    let r = Repo::new("lines");
    r.take_lock();
    for line in ["lucina-rs", "server-live", "web-live"] {
        let (rc, out) = r.guard(&["check", "--runtime", "web", "--target", line]);
        assert_eq!(rc, 1, "{out}");
        assert!(out.contains("deploys from main"), "{out}");
    }
}

#[test]
fn record_tags_the_deploy_and_plan_reports_changes() {
    let r = Repo::new("tags");
    r.take_lock();
    std::fs::create_dir_all(r.dir.join("deploy")).unwrap();
    std::fs::create_dir_all(r.dir.join("web")).unwrap();
    std::fs::write(r.dir.join("deploy/targets.tsv"), "web\tp1\tweb\nios\tiphone\tios\n").unwrap();
    std::fs::write(r.dir.join("web/a.ts"), "1").unwrap();
    sh(&r.dir, &["add", "."]);
    sh(&r.dir, &["commit", "-q", "-m", "targets"]);
    let base = sh(&r.dir, &["rev-parse", "HEAD"]);
    let (rc, out) = r.guard(&["record", "--runtime", "web", "--commit", &base, "--bootstrap"]);
    assert_eq!(rc, 0, "{out}");
    let tags = sh(&r.dir, &["tag", "--list", "deploy/web/*"]);
    assert!(tags.ends_with(&format!("-{}", &base[..8])), "{tags}");
    let (_, out) = r.guard(&["plan"]);
    assert!(out.contains("up to date"), "{out}");
    assert!(out.contains("no ledger entry"), "{out}");
    std::fs::write(r.dir.join("web/a.ts"), "2").unwrap();
    sh(&r.dir, &["commit", "-q", "-am", "web change"]);
    r.commit("other.rs");
    let (_, out) = r.guard(&["plan", "--runtime", "web"]);
    assert!(out.contains("1 commit(s) to deploy"), "{out}");
}
