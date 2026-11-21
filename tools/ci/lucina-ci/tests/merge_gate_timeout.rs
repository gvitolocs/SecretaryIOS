//! `check-fast --merge-gate`: a step that runs past its limit FAILS the merge, it is never moved to "skipped"
//! (2026-11-12: unit-tests skipped at the merge gate under load, job_b6094541ca4c). The real binary runs over a throwaway
//! repository with a stub `diffgate` first on PATH and a limit injected with LUCINA_CHECK_FAST_MERGE_CAP_S (never a real 180 s sleep).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

/// The memory cap the spawned `check-fast` asks for. The binary refuses to start on a runner that cannot hold its cap plus a reserve
/// (the default 8 GiB cap needs 10 GiB; a private repository's 8 GB runner has 7.7), and these tests judge the time limit, not the
/// machine's RAM, so they ask for a cap every runner holds (`runner::budget`).
const MEM_CAP_GIB: &str = "1";

/// The names a lane's git config comes in with, of which `bq` takes `GIT_CONFIG_KEY_0` out of a job's environment because
/// its name holds KEY and leaves `GIT_CONFIG_COUNT` behind — which no git can read, not the one a test runs to build its
/// repository and not the one the spawned `check-fast` calls. Everything here runs with the trio gone.
const LANE_GIT_CONFIG: [&str; 3] = ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"];

fn drop_lane_git_config(c: &mut Command) {
    for n in LANE_GIT_CONFIG {
        c.env_remove(n);
    }
}

/// git, with no inherited config.
fn git(d: &Path, args: &[&str]) {
    let mut c = Command::new("git");
    c.args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "-c",
        "commit.gpgsign=false",
    ])
    .args(args)
    .current_dir(d);
    drop_lane_git_config(&mut c);
    let o = c.output().unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// A repository on `work`, one commit ahead of `main`, and a stub `diffgate` that sleeps `sleep_s`.
fn setup(tag: &str, sleep_s: u32) -> (PathBuf, PathBuf) {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("mgt-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    let repo = d.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("README"), "x\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "work"]);
    fs::write(repo.join("NOTES.txt"), "a change\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "change"]);
    let bin = d.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("diffgate");
    fs::write(&stub, format!("#!/bin/sh\nsleep {sleep_s}\nexit 0\n")).unwrap();
    Command::new("chmod").arg("+x").arg(&stub).status().unwrap();
    (repo, bin)
}

fn run(repo: &Path, bin: &Path, cap: &str) -> (i32, String) {
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut c = Command::new(env!("CARGO_BIN_EXE_lucina-ci"));
    c.args([
        "check-fast",
        "--base",
        "main",
        "--merge-gate",
        "--no-build",
        "--mem-cap-gib",
        MEM_CAP_GIB,
    ])
    .current_dir(repo)
    .env("PATH", path)
    .env("LUCINA_CHECK_FAST_MERGE_CAP_S", cap)
    // A step timed out is what this test measures: nothing inherited may keep diffgate from running at all.
    .env_remove("LAGENT_ALLOW")
    .env_remove("LAGENT_WAIVERS_FILE");
    drop_lane_git_config(&mut c);
    let o = c.output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        ),
    )
}

#[test]
fn merge_gate_step_past_its_limit_fails_the_merge_and_is_never_a_skip() {
    if common::skip_without_scope(
        "merge_gate_step_past_its_limit_fails_the_merge_and_is_never_a_skip",
    ) {
        return;
    }
    let (repo, bin) = setup("late", 30);
    let (code, out) = run(&repo, &bin, "1");
    assert_ne!(code, 0, "a timed-out step fails the merge: {out}");
    assert!(
        out.contains("diffgate: past its 1 s limit at merge: the machine is loaded; retry when it is quieter, or run scripts/check-fast in the worktree"),
        "{out}"
    );
    assert!(
        !out.lines()
            .any(|l| l.starts_with("skipped") && l.contains("past its")),
        "a timeout is never moved to skipped: {out}"
    );
    assert!(out.contains(" failed, "), "{out}");
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn merge_gate_step_within_its_limit_passes_without_the_late_message() {
    if common::skip_without_scope(
        "merge_gate_step_within_its_limit_passes_without_the_late_message",
    ) {
        return;
    }
    let (repo, bin) = setup("ontime", 2);
    let (_code, out) = run(&repo, &bin, "20");
    assert!(out.lines().any(|l| l.starts_with("ok   diffgate")), "{out}");
    assert!(!out.contains("past its"), "{out}");
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}

#[test]
fn a_step_past_its_limit_outside_the_merge_gate_is_the_plain_failure_it_was() {
    if common::skip_without_scope(
        "a_step_past_its_limit_outside_the_merge_gate_is_the_plain_failure_it_was",
    ) {
        return;
    }
    let (repo, bin) = setup("dev", 5);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut c = Command::new(env!("CARGO_BIN_EXE_lucina-ci"));
    c.args([
        "check-fast",
        "--base",
        "main",
        "--no-build",
        "--mem-cap-gib",
        MEM_CAP_GIB,
    ])
    .current_dir(&repo)
    .env("PATH", path)
    .env("LUCINA_CHECK_FAST_MERGE_CAP_S", "1")
    .env_remove("LAGENT_ALLOW")
    .env_remove("LAGENT_WAIVERS_FILE");
    drop_lane_git_config(&mut c);
    let o = c.output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    // the cap is a merge-gate rule: a developer's own run is not cut at 1 s, the 5 s stub finishes
    assert!(out.lines().any(|l| l.starts_with("ok   diffgate")), "{out}");
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}
