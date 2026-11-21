//! `check-fast` says, after its steps, that the integration tests of a touched workspace crate were not run: the unit-test steps
//! run `cargo test --lib` only. The real binary runs over a throwaway repository with a stub `cargo`; no crate is built.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

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
    for n in ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"] {
        c.env_remove(n);
    }
    let o = c.output().unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn krate(repo: &Path, dir: &str, name: &str) {
    let d = repo.join(dir);
    fs::create_dir_all(d.join("src")).unwrap();
    fs::write(
        d.join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"),
    )
    .unwrap();
    fs::write(d.join("src/lib.rs"), "pub fn one() -> u32 { 1 }\n").unwrap();
}

/// A repository whose `work` branch adds the given crates; `with_tests` gives `crates/tested` its `tests/`.
fn run_over(tag: &str, crates: &[(&str, &str)], with_tests: bool) -> String {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("itn-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    let repo = d.join("repo");
    fs::create_dir_all(&repo).unwrap();
    let members: Vec<String> = crates.iter().map(|(p, _)| format!("\"{p}\"")).collect();
    fs::write(
        repo.join("Cargo.toml"),
        format!(
            "[workspace]\nresolver = \"3\"\nmembers = [{}]\n",
            members.join(", ")
        ),
    )
    .unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
    git(&repo, &["checkout", "-q", "-b", "work"]);
    for (p, n) in crates {
        krate(&repo, p, n);
    }
    if with_tests {
        let t = repo.join("crates/tested/tests");
        fs::create_dir_all(t.join("b")).unwrap();
        fs::write(t.join("a.rs"), "#[test]\nfn a() {}\n").unwrap();
        fs::write(t.join("b/main.rs"), "#[test]\nfn b() {}\n").unwrap();
        fs::write(t.join("b/helper.rs"), "// not a test target\n").unwrap();
        fs::write(t.join("notes.txt"), "not rust\n").unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "crates"]);
    let bin = d.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for n in ["cargo", "diffgate", "gitleaks"] {
        let p = bin.join(n);
        fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
        Command::new("chmod").arg("0755").arg(&p).status().unwrap();
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut out = String::new();
    for flags in [&["--merge-gate"][..], &[][..]] {
        let mut c = Command::new(env!("CARGO_BIN_EXE_lucina-ci"));
        c.args(["check-fast", "--base", "main", "--mem-cap-gib", "1"])
            .args(flags)
            .current_dir(&repo)
            .env("PATH", &path)
            .env_remove("LAGENT_ALLOW")
            .env_remove("LAGENT_WAIVERS_FILE");
        for n in ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"] {
            c.env_remove(n);
        }
        let o = c.output().unwrap();
        out.push_str(&String::from_utf8_lossy(&o.stdout));
        out.push_str(&String::from_utf8_lossy(&o.stderr));
        out.push_str("=====\n");
    }
    let _ = fs::remove_dir_all(&d);
    out
}

#[test]
fn a_touched_crate_with_integration_tests_is_named_after_the_steps() {
    if common::skip_without_scope("a_touched_crate_with_integration_tests_is_named_after_the_steps")
    {
        return;
    }
    let out = run_over(
        "with",
        &[("crates/tested", "tested"), ("crates/plain", "plain")],
        true,
    );
    let want = "check-fast: integration tests of touched crates were NOT run at this gate: 2 file(s): crates/tested/tests/a.rs crates/tested/tests/b/main.rs (run them with: cargo test -p tested --tests)";
    assert_eq!(
        out.matches(want).count(),
        2,
        "merge gate and plain run each say it once:\n{out}"
    );
    assert!(
        !out.contains("tests/b/helper.rs") && !out.contains("notes.txt"),
        "{out}"
    );
    // After the steps, before the closing count; the other lines keep their order.
    for run in out.split("=====\n").filter(|s| !s.is_empty()) {
        let lines: Vec<&str> = run.lines().collect();
        let note = lines.iter().position(|l| *l == want).unwrap();
        let last = lines
            .iter()
            .position(|l| l.starts_with("check-fast: ") && l.contains(" failed, "))
            .unwrap();
        let unit = lines
            .iter()
            .position(|l| l.split_whitespace().nth(1) == Some("unit-tests"))
            .unwrap();
        assert!(unit < note && note < last, "{run}");
    }
}

#[test]
fn a_touched_crate_without_tests_dir_gets_no_line() {
    if common::skip_without_scope("a_touched_crate_without_tests_dir_gets_no_line") {
        return;
    }
    let out = run_over("without", &[("crates/plain", "plain")], false);
    assert!(
        !out.contains("integration tests of touched crates"),
        "{out}"
    );
    assert!(out.contains(", 0 failed, "), "{out}");
}
