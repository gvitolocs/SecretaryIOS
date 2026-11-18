//! `check-fast --merge-gate`: the diffgate step takes the lead's waivers from `LAGENT_ALLOW`, the
//! environment `lagent merge` gives every gate command but its own diffgate, whose `--allow` it has
//! already honoured (crates/lucina-agentd/src/pipeline/repo.rs `allow_env`). This is diffgate's second
//! run of the same diff, so a waiver has to reach it here too. Outside `--merge-gate` the variable
//! waives nothing: a worker or a lane verify step must not be able to waive diffgate for itself.
//!
//! The real binary runs over a throwaway repository with a stand-in `diffgate` first on PATH.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The memory cap the spawned `check-fast` asks for: the tests judge what diffgate is handed, not the
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

/// An executable script, written by a child that is reaped before anything execs it. Writing the file
/// from the test thread itself keeps its fd open while another thread forks (git, chmod), and that
/// fork keeps a writable copy of the fd until its own exec, so exec answers "Text file busy"
/// (ETXTBSY): measured today in lucina-agentd, 110 failures in 1500 writes for `fs::write` and chmod,
/// 65 for a temp file renamed into place, 0 when a child writes it. There the helper is
/// `testutil::write_exec`; the launcher is another crate, so these tests carry their own copy.
fn write_exec(path: &Path, script: &str) {
    let mut child = Command::new("sh")
        .args(["-c", "cat > \"$1\" && chmod 0755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut w = child.stdin.take().unwrap();
    w.write_all(script.as_bytes()).unwrap();
    drop(w);
    assert!(
        child.wait().unwrap().success(),
        "sh cannot write {}: the stand-in is never exec'd",
        path.display()
    );
}

/// A repository one commit ahead of `main` and a stand-in `diffgate` on PATH that writes its argv to
/// `argv` and refuses every rule listed in `refuse` unless it is given `--allow <rule>=<reason>`. The
/// directory goes with the fixture, so a panicking test does not leave it behind.
struct Fixture {
    d: PathBuf,
    repo: PathBuf,
    bin: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.d);
    }
}

impl Fixture {
    fn new(tag: &str, refuse: &[&str]) -> Fixture {
        let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("dag-{tag}-{}", std::process::id()));
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
        fs::write(d.join("refuse"), format!("{}\n", refuse.join("\n"))).unwrap();
        // The stand-in goes in through `write_exec`, never from this thread: it is exec'd by the step.
        let stub = bin.join("diffgate");
        write_exec(
            &stub,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{d}/argv'\nfor r in $(cat '{d}/refuse'); do\n  ok=0\n  prev=\n  for a in \"$@\"; do\n    if [ \"$prev\" = '--allow' ]; then\n      case $a in \"$r\"=*) ok=1 ;; esac\n    fi\n    prev=$a\n  done\n  if [ \"$ok\" != 1 ]; then\n    echo \"stub-diffgate: $r is refused\" >&2\n    exit 1\n  fi\ndone\nexit 0\n",
                d = d.display()
            ),
        );
        Fixture { d, repo, bin }
    }

    /// What the stand-in recorded, one argument per line; nothing when it never ran.
    fn argv(&self) -> Vec<String> {
        fs::read_to_string(self.d.join("argv"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// `check-fast` over the fixture, with or without `--merge-gate` and with this `LAGENT_ALLOW`.
    fn check_fast(&self, merge_gate: bool, allow: &str) -> (i32, String) {
        let mut c = Command::new(env!("CARGO_BIN_EXE_lucina-ci"));
        c.args([
            "check-fast",
            "--base",
            "main",
            "--no-build",
            "--mem-cap-gib",
            MEM_CAP_GIB,
        ]);
        if merge_gate {
            c.arg("--merge-gate");
        }
        c.current_dir(&self.repo)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("LAGENT_ALLOW", allow)
            // This file judges `LAGENT_ALLOW` alone: a job's waivers file inherited from the merge gate that runs the
            // suite would forward from a second source and answer for a step this test did not give it.
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
}

/// The step line of the diffgate step, whatever else passed.
fn diffgate_line(out: &str) -> String {
    out.lines()
        .find(|l| l.starts_with("ok   diffgate") || l.starts_with("FAIL diffgate"))
        .unwrap_or_else(|| panic!("no diffgate step line in:\n{out}"))
        .to_string()
}

#[test]
fn the_merge_gate_hands_the_lead_waiver_to_diffgate_and_counts_it() {
    let f = Fixture::new("waived", &["guard-deleted"]);
    let (_code, out) = f.check_fast(true, "guard-deleted=a reason; with spaces");
    let argv = f.argv();
    let i = argv
        .iter()
        .position(|a| a == "--allow")
        .unwrap_or_else(|| panic!("diffgate got no --allow: {argv:?}\n{out}"));
    assert_eq!(
        argv[i + 1],
        "guard-deleted=a reason; with spaces",
        "the entry as given, as one argument: {argv:?}"
    );
    assert!(
        out.contains("waived: guard-deleted = a reason; with spaces"),
        "{out}"
    );
    assert!(diffgate_line(&out).starts_with("ok   diffgate"), "{out}");
    assert!(diffgate_line(&out).contains("(1 waived)"), "{out}");
}

#[test]
fn outside_the_merge_gate_the_variable_waives_nothing_and_says_so() {
    let f = Fixture::new("ignored", &["guard-deleted"]);
    let (code, out) = f.check_fast(false, "guard-deleted=a reason; with spaces");
    assert_ne!(code, 0, "the stand-in refuses an unwaived rule: {out}");
    assert!(
        !f.argv().iter().any(|a| a == "--allow"),
        "no waiver outside the merge gate: {:?}",
        f.argv()
    );
    assert!(diffgate_line(&out).starts_with("FAIL diffgate"), "{out}");
    assert!(!out.contains("waived:"), "{out}");
    assert!(
        out.contains(
            "diffgate: LAGENT_ALLOW is set but ignored: only the merge gate takes a lead's allows"
        ),
        "{out}"
    );
}

#[test]
fn a_rule_the_variable_does_not_name_still_refuses_at_the_merge_gate() {
    let f = Fixture::new("one-of-two", &["guard-deleted", "never-waived"]);
    let (code, out) = f.check_fast(true, "guard-deleted=the lead checked it");
    assert_ne!(code, 0, "the other rule is not waived: {out}");
    assert!(
        out.contains("stub-diffgate: never-waived is refused"),
        "{out}"
    );
    assert!(
        !f.argv().iter().any(|a| a.starts_with("never-waived")),
        "only the named rule got an allow: {:?}",
        f.argv()
    );
    assert!(
        out.contains("waived: guard-deleted = the lead checked it"),
        "the waiver it did take is shown: {out}"
    );
    assert!(diffgate_line(&out).contains("(1 waived)"), "{out}");
}

#[test]
fn a_malformed_entry_fails_the_step_and_names_the_entry() {
    let f = Fixture::new("malformed", &[]);
    let (code, out) = f.check_fast(true, "noequals\n=reason\nBad Rule=x\nrule=");
    assert_ne!(code, 0, "a waiver that is not RULE=reason fails: {out}");
    for entry in ["noequals", "=reason", "Bad Rule=x", "rule="] {
        assert!(out.contains(entry), "{entry} is named: {out}");
    }
    assert!(!out.contains("waived:"), "nothing is waived: {out}");
    assert!(f.argv().is_empty(), "diffgate does not run: {out}");
}

#[test]
fn lagent_own_rules_in_the_variable_are_not_diffgate_waivers() {
    let f = Fixture::new("not-diffgate", &[]);
    let (_code, out) = f.check_fast(
        true,
        "gate-change=bootstrap the pipeline file\nspec-change=proof reworded",
    );
    assert!(
        !f.argv().iter().any(|a| a == "--allow"),
        "diffgate never gets lagent's own rules: {:?}",
        f.argv()
    );
    assert!(!out.contains("waived:"), "{out}");
    assert!(!out.contains("is not RULE=reason"), "{out}");
    assert!(
        !diffgate_line(&out).contains("waived"),
        "the step line counts nothing: {out}"
    );
}
