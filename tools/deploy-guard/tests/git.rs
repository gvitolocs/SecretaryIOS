//! Today's incident replayed on real git repos: a live build that is not an
//! ancestor of the deploy is refused; the fixed procedure passes. The origin
//! tests use a bare origin and a second clone (another chat) that pushes to it.

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
            .env("DEPLOY_GUARD_OPS", "off")
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

    /// A bare `origin` next to the repo with main pushed, and a second clone of it ("other chat").
    fn with_origin(name: &str) -> (Repo, PathBuf) {
        let r = Repo::new(name);
        let root = r.dir.parent().unwrap().to_path_buf();
        let origin = root.join("origin.git");
        let other = root.join("other");
        sh(&root, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
        sh(&r.dir, &["remote", "add", "origin", origin.to_str().unwrap()]);
        sh(&r.dir, &["push", "-q", "-u", "origin", "main"]);
        sh(&root, &["clone", "-q", origin.to_str().unwrap(), other.to_str().unwrap()]);
        sh(&other, &["config", "user.email", "o@example.test"]);
        sh(&other, &["config", "user.name", "o"]);
        (r, other)
    }
}

fn commit_in(dir: &Path, file: &str) -> String {
    std::fs::write(dir.join(file), file).unwrap();
    sh(dir, &["add", file]);
    sh(dir, &["commit", "-q", "-m", file]);
    sh(dir, &["rev-parse", "HEAD"])
}

/// An annotated deploy tag with an explicit (old) stamp, so a real `record` tag sorts after it.
fn deploy_tag(dir: &Path, stamp: &str, commit: &str) -> String {
    let tag = format!("deploy/web/{stamp}-{}", &commit[..8]);
    sh(dir, &["tag", "-a", &tag, commit, "-m", "web deployed"]);
    tag
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

#[test]
fn main_tip_is_allowed() {
    let (r, _other) = Repo::with_origin("tip");
    r.take_lock();
    let base = sh(&r.dir, &["rev-parse", "HEAD"]);
    deploy_tag(&r.dir, "20200101T0000Z", &base);
    sh(&r.dir, &["push", "-q", "origin", "--tags"]);
    r.commit("new.rs");
    sh(&r.dir, &["push", "-q", "origin", "main"]);
    let (rc, out) = r.guard(&["check", "--runtime", "web"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("deploy-guard: OK web"), "{out}");
    assert!(!out.contains("no origin remote"), "{out}");
}

#[test]
fn a_branch_build_is_refused() {
    let (r, _other) = Repo::with_origin("branch");
    r.take_lock();
    let base = sh(&r.dir, &["rev-parse", "HEAD"]);
    deploy_tag(&r.dir, "20200101T0000Z", &base);
    sh(&r.dir, &["push", "-q", "origin", "--tags"]);
    sh(&r.dir, &["checkout", "-q", "-b", "feature"]);
    r.commit("feature.rs");
    let (rc, out) = r.guard(&["check", "--runtime", "web"]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("is not on main"), "{out}");
    assert!(out.contains("--allow-not-main"), "{out}");
    let (rc, out) = r.guard(&["check", "--runtime", "web", "--allow-not-main=hotfix test"]);
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains("allow-not-main (reason: hotfix test"), "{out}");
    let (rc, out) = r.guard(&["check", "--runtime", "web", "--allow-not-main="]);
    assert_eq!(rc, 2, "{out}");
}

#[test]
fn a_stale_main_is_refused() {
    let (r, other) = Repo::with_origin("stale");
    r.take_lock();
    let base = sh(&r.dir, &["rev-parse", "HEAD"]);
    deploy_tag(&r.dir, "20200101T0000Z", &base);
    sh(&r.dir, &["push", "-q", "origin", "--tags"]);
    // The other chat lands a commit on origin/main; this checkout's main is now stale.
    commit_in(&other, "other-chat-change.rs");
    sh(&other, &["push", "-q", "origin", "main"]);
    let (rc, out) = r.guard(&["check", "--runtime", "web"]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("is not main's tip"), "{out}");
    assert!(out.contains("main has 1 newer commit(s)"), "{out}");
    assert!(out.contains("other-chat-change.rs"), "{out}");
    // Merged onto the fresh origin/main: allowed.
    sh(&r.dir, &["pull", "-q", "--ff-only", "origin", "main"]);
    let (rc, out) = r.guard(&["check", "--runtime", "web"]);
    assert_eq!(rc, 0, "{out}");
}

#[test]
fn a_missing_live_commit_is_refused() {
    let (r, other) = Repo::with_origin("missing");
    r.take_lock();
    // The other chat deploys a branch build and pushes only its tag; nothing is in this ledger.
    sh(&other, &["checkout", "-q", "-b", "hotfix"]);
    let hotfix = commit_in(&other, "live-hotfix.rs");
    let tag = deploy_tag(&other, "20200101T0000Z", &hotfix);
    sh(&other, &["push", "-q", "origin", &format!("refs/tags/{tag}")]);
    assert!(sh(&r.dir, &["tag", "--list"]).is_empty());
    // This deploy is origin/main's tip but lacks the live hotfix.
    r.commit("main-change.rs");
    sh(&r.dir, &["push", "-q", "origin", "main"]);
    let (rc, out) = r.guard(&["check", "--runtime", "web"]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains(&tag), "{out}");
    assert!(out.contains("would drop 1 live commit(s)"), "{out}");
    assert!(out.contains("live-hotfix.rs"), "{out}");
    assert!(!out.contains("is not main's tip"), "{out}");
}

#[test]
fn an_explicit_rollback_is_allowed() {
    let (r, _other) = Repo::with_origin("rollback");
    r.take_lock();
    let a = r.commit("a.rs");
    let tag_a = deploy_tag(&r.dir, "20200101T0000Z", &a);
    let b = r.commit("b.rs");
    deploy_tag(&r.dir, "20200102T0000Z", &b);
    sh(&r.dir, &["tag", "v1", &a]);
    sh(&r.dir, &["push", "-q", "origin", "main", "--tags"]);
    sh(&r.dir, &["checkout", "-q", "--detach", &tag_a]);
    let rollback = ["--rollback-to", tag_a.as_str(), "--reason", "B broke login"];
    let (rc, out) = r.guard(&[&["check", "--runtime", "web"][..], &rollback].concat());
    assert_eq!(rc, 0, "{out}");
    assert!(out.contains(&format!("ROLLBACK to {tag_a} (reason: B broke login)")), "{out}");
    let (rc, out) = r.guard(&["check", "--runtime", "web"]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("would drop 1 live commit(s)"), "{out}");
    let (rc, out) = r.guard(&["check", "--runtime", "web", "--rollback-to", "v1", "--reason", "x"]);
    assert_eq!(rc, 1, "{out}");
    assert!(out.contains("not a deploy/web/ tag"), "{out}");
    let (rc, out) = r.guard(&["check", "--runtime", "web", "--rollback-to", &tag_a]);
    assert_eq!(rc, 2, "{out}");
    let (rc, out) = r.guard(&["check", "--runtime", "web", "--reason", "x"]);
    assert_eq!(rc, 2, "{out}");
    let (rc, out) = r.guard(&[&["check", "--runtime", "web", "--commit", &b][..], &rollback].concat());
    assert_eq!(rc, 2, "{out}");
    // The record tags the rollback, so the newest deploy tag is what is live again.
    let (rc, out) = r.guard(&[&["record", "--runtime", "web"][..], &rollback].concat());
    assert_eq!(rc, 0, "{out}");
    let newest = sh(
        &r.dir,
        &["for-each-ref", "--sort=-refname", "--count=1", "--format=%(refname:short)", "refs/tags/deploy/web/"],
    );
    assert_ne!(newest, tag_a);
    assert_eq!(sh(&r.dir, &["rev-parse", &format!("{newest}^{{commit}}")]), a);
    let history = std::fs::read_to_string(r.ledger.join("history.jsonl")).unwrap();
    assert!(history.contains(&format!("\"override\":\"rollback-to {tag_a}: B broke login\"")), "{history}");
}
