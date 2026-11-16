//! The finished-run gate end to end, in a temporary git repository, with stand-ins for
//! diffgate (refuses files containing `STUB`) and the judge (flags diffs containing
//! `SUSPICIOUS`). The mock writes files with "lucina mock write <path>=<text>".

mod common;

use std::path::Path;
use std::process::Command;

use common::Daemon;
use serde_json::Value;

struct Repo {
    dir: tempfile::TempDir,
}

impl Repo {
    fn new(files: &[(&str, &str)]) -> Repo {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(dir.path().join("README"), "repo\n").unwrap();
        for (p, t) in files {
            let full = dir.path().join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, t).unwrap();
        }
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
        Repo { dir }
    }

    fn path(&self) -> String {
        self.dir.path().display().to_string()
    }
}

fn tools(dir: &Path) -> Vec<(&'static str, String)> {
    let diffgate = dir.join("fake-diffgate");
    common::write_exec(
        &diffgate,
        "#!/bin/sh\n# --repo DIR --base SHA --json\nif grep -rq STUB \"$2\" --exclude-dir=.git; then\n  echo '{\"ok\":false,\"refusals\":[{\"file\":\"a.rs\",\"line\":1,\"rule\":\"code-stub\",\"snippet\":\"STUB\"}]}'; exit 1\nfi\necho '{\"ok\":true,\"refusals\":[]}'\n",
    );
    vec![
        ("LUCINA_AGENTD_DIFFGATE", diffgate.display().to_string()),
        ("LUCINA_AGENTD_JUDGE", r#"if grep -q SUSPICIOUS; then echo '{"suspicious": true, "reasons": ["looks hard-coded"]}'; else echo 'ok {"suspicious": false}'; fi"#.to_string()),
        ("LUCINA_AGENTD_VERIFY_GATE", "off".to_string()),
    ]
}

fn daemon() -> (Daemon, tempfile::TempDir) {
    let t = tempfile::tempdir().unwrap();
    let d = Daemon::start_with_env(&tools(t.path()));
    (d, t)
}

fn run(d: &Daemon, repo: &Repo, extra: &[&str], prompt: &str) -> (Value, Value) {
    let cwd = repo.path();
    let mut args = vec![
        "run",
        "--provider",
        "mock/e2e-fast-stream",
        "--cwd",
        &cwd,
        "--gate",
    ];
    args.extend_from_slice(extra);
    args.push(prompt);
    let r = d.json(&args);
    let gate = d.json(&["inspect", r["agentId"].as_str().unwrap()])["Gate"].clone();
    (r, gate)
}

#[test]
fn verified_when_every_check_passes() {
    let (d, _t) = daemon();
    let repo = Repo::new(&[]);
    let (r, gate) = run(
        &d,
        &repo,
        &["--verify", "grep -q hi hello.txt"],
        "lucina mock write hello.txt=hi",
    );
    assert_eq!(r["status"], "completed");
    assert_eq!(r["verdict"], "verified", "{gate}");
    assert_eq!(gate["diffgate"], "pass");
    assert_eq!(gate["proof"], "pass");
    assert_eq!(gate["judge"], "clean");
    assert_eq!(gate["proofCommand"], "grep -q hi hello.txt");
    assert_eq!(gate["round"], 0);
    assert!(gate["base"].as_str().is_some_and(|b| b.len() == 40));
}

#[test]
fn no_change_no_gate_and_no_proof_is_unverified() {
    let (d, _t) = daemon();
    let repo = Repo::new(&[]);
    let (_, gate) = run(&d, &repo, &[], "just answer");
    assert!(gate.is_null(), "nothing changed: {gate}");
    let (r, gate) = run(&d, &repo, &[], "lucina mock write notes.txt=x");
    assert_eq!(r["verdict"], "unverified", "{gate}");
    assert_eq!(gate["proof"], "skipped");
    // Outside git there is no gate at all.
    let (_, gate) = run(
        &d,
        &Repo {
            dir: tempfile::tempdir().unwrap(),
        },
        &[],
        "lucina mock write x.txt=x",
    );
    assert!(gate.is_null(), "{gate}");
}

#[test]
fn refused_by_diffgate_or_the_proof_then_auto_fix() {
    let (d, _t) = daemon();
    let repo = Repo::new(&[]);
    let (r, gate) = run(
        &d,
        &repo,
        &["--verify", "true"],
        "lucina mock write a.rs=STUB",
    );
    assert_eq!(r["verdict"], "refused", "{gate}");
    assert_eq!(gate["diffgate"], "fail");
    assert_eq!(gate["judge"], "skipped", "no judge once a check failed");
    assert!(
        gate["details"][0].as_str().unwrap().contains("code-stub"),
        "{gate}"
    );

    let repo = Repo::new(&[]);
    let (r, gate) = run(
        &d,
        &repo,
        &["--verify", "grep -q fixed hello.txt", "--auto-fix", "1"],
        "lucina mock write hello.txt=broken",
    );
    assert_eq!(r["verdict"], "refused", "{gate}");
    assert_eq!(gate["proof"], "fail");
    assert_eq!(gate["round"], 1, "one auto-fix round, then it stops");
    let id = r["agentId"].as_str().unwrap();
    let logs = d.lagent(&["logs", id]).stdout;
    assert!(
        logs.contains("[User] The finished-run gate refused your changes (round 1 of 1)"),
        "{logs}"
    );
    assert!(logs.contains("grep -q fixed hello.txt"), "{logs}");
}

#[test]
fn the_proof_comes_from_the_base_and_tampering_is_suspicious() {
    let (d, _t) = daemon();
    let repo = Repo::new(&[(".lucina/agents.toml", "verify = \"grep -q ok out.txt\"\n")]);
    let (r, gate) = run(&d, &repo, &[], "lucina mock write out.txt=ok");
    assert_eq!(r["verdict"], "verified", "{gate}");
    assert_eq!(gate["proofCommand"], "grep -q ok out.txt");

    // The run rewrites the gate config to a proof that always passes: the gate still
    // runs the old one and flags the run.
    let (r, gate) = run(
        &d,
        &repo,
        &[],
        "lucina mock write .lucina/agents.toml=verify = \"true\"",
    );
    assert_eq!(r["verdict"], "suspicious", "{gate}");
    assert_eq!(gate["tampered"], true);
    assert_eq!(gate["proofCommand"], "grep -q ok out.txt");
}

#[test]
fn the_judge_flags_but_never_verifies() {
    let (d, _t) = daemon();
    let repo = Repo::new(&[]);
    let (r, gate) = run(
        &d,
        &repo,
        &["--verify", "true"],
        "lucina mock write answer.txt=SUSPICIOUS 42",
    );
    assert_eq!(r["verdict"], "suspicious", "{gate}");
    assert_eq!(gate["judge"], "flagged");
    assert!(gate["details"].to_string().contains("looks hard-coded"));
}
