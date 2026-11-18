//! `check-fast --merge-gate` with a limit PER step (finding GATE-02: the one flat 180 s limit blocked SecretaryIOS merges all
//! week on `clippy` and `unit-tests`, whose measured p95 was 151.6 s and 154.5 s). The real binary runs over a throwaway
//! repository with a stub `cargo`, `diffgate` and `gitleaks` first on PATH that sleep a stated amount, so no step is really
//! built and no test sleeps a real 180 s.
//!
//! The repository holds two kinds of crate, because that is what names the steps: a root workspace member, whose fast steps are
//! the plain `fmt`/`clippy`/`unit-tests` the table in `main.rs` raises to 240 s, and a standalone crate under `tools/`, whose
//! steps are named after it (`clippy-tools-ci-fake`) and are in no table row, so they sit at the 180 default.
//!
//! Two ways a step gets its limit, and both are exercised here: the table with the override absent, and
//! `LUCINA_CHECK_FAST_MERGE_CAP_S` with a `step=SECONDS` entry — the only way to see one step pass at a limit the default would
//! have cut, since the table's numbers are minutes apart.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The memory cap the spawned `check-fast` asks for: these tests judge the time limit, not the machine's RAM, so they ask for
/// a cap every runner holds (`runner::budget`).
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

/// An executable script, written by a child that is reaped before anything execs it. Writing the file from the test thread
/// itself keeps its fd open while another thread forks, and that fork keeps a writable copy of the fd until its own exec, so
/// exec answers "Text file busy" (ETXTBSY). There the helper is `testutil::write_exec`; the launcher is another crate, so
/// these tests carry their own copy.
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

/// A repository whose `main` is empty and whose `work` branch adds a workspace member (`crates/demo`) and a standalone crate
/// (`tools/ci/fake`), which is what makes the plain `clippy` and `unit-tests` steps and the `*-tools-ci-fake` steps run.
/// The stub `cargo` sleeps `test_s` for `cargo test` and `clippy_s` for `cargo clippy` and nothing for `cargo fmt`; the stub
/// `diffgate` sleeps `diffgate_s`.
fn setup(tag: &str, test_s: &str, clippy_s: &str, diffgate_s: &str) -> (PathBuf, PathBuf) {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("mgsl-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    let repo = d.join("repo");
    let member = repo.join("crates/demo");
    let standalone = repo.join("tools/ci/fake");
    fs::create_dir_all(member.join("src")).unwrap();
    fs::create_dir_all(standalone.join("src")).unwrap();
    fs::write(
        repo.join("Cargo.toml"),
        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/demo\"]\n",
    )
    .unwrap();
    fs::write(
        member.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(member.join("src/lib.rs"), "pub fn one() -> u32 { 1 }\n").unwrap();
    fs::write(
        standalone.join("Cargo.toml"),
        "[package]\nname = \"fake\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\n",
    )
    .unwrap();
    fs::write(standalone.join("src/lib.rs"), "pub fn two() -> u32 { 2 }\n").unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "empty base"],
    );
    git(&repo, &["checkout", "-q", "-b", "work"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "two crates"]);
    let bin = d.join("bin");
    fs::create_dir_all(&bin).unwrap();
    write_exec(
        &bin.join("cargo"),
        &format!(
            "for a in \"$@\"; do case \"$a\" in test) sleep {test_s};; clippy) sleep {clippy_s};; \
             fmt) ;; esac; done; exit 0\n"
        ),
    );
    write_exec(
        &bin.join("diffgate"),
        &format!("sleep {diffgate_s}\nexit 0\n"),
    );
    write_exec(&bin.join("gitleaks"), "exit 0\n");
    (repo, bin)
}

/// `cap` is `LUCINA_CHECK_FAST_MERGE_CAP_S`; `None` leaves the table and the 180 default in charge, and takes the variable
/// off the child's environment so whatever the caller's shell holds cannot decide the test.
fn run(repo: &Path, bin: &Path, cap: Option<&str>) -> (i32, String) {
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
        "--mem-cap-gib",
        MEM_CAP_GIB,
    ])
    .current_dir(repo)
    .env("PATH", path)
    .env_remove("LAGENT_ALLOW")
    .env_remove("LAGENT_WAIVERS_FILE");
    drop_lane_git_config(&mut c);
    match cap {
        Some(v) => c.env("LUCINA_CHECK_FAST_MERGE_CAP_S", v),
        None => c.env_remove("LUCINA_CHECK_FAST_MERGE_CAP_S"),
    };
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

fn step_line(out: &str, name: &str) -> String {
    out.lines()
        .find(|l| l.split_whitespace().nth(1) == Some(name))
        .unwrap_or_else(|| panic!("no line for step {name}:\n{out}"))
        .to_string()
}

/// The steps this fixture runs: three for the workspace member, three for the standalone crate, then `diffgate` and `secrets`.
const STEPS: [&str; 8] = [
    "fmt",
    "clippy",
    "unit-tests",
    "fmt-tools-ci-fake",
    "clippy-tools-ci-fake",
    "unit-tests-tools-ci-fake",
    "diffgate",
    "secrets",
];

/// With no override the limits are the table's and the default's, and every step line prints the one actually enforced —
/// never above the step's own timeout.
#[test]
fn merge_gate_prints_each_step_s_own_limit_and_keeps_180_for_the_rest() {
    let (repo, bin) = setup("limits", "0.2", "0.2", "0.2");
    let (_code, out) = run(&repo, &bin, None);
    for name in ["clippy", "unit-tests"] {
        let line = step_line(&out, name);
        assert!(
            line.starts_with("ok  "),
            "{name} passed under the table's 240: {line}"
        );
        assert!(
            line.contains("/ 240s"),
            "{name} is at the table's 240, cut from its own 300: {line}"
        );
    }
    for name in ["clippy-tools-ci-fake", "unit-tests-tools-ci-fake"] {
        let line = step_line(&out, name);
        assert!(
            line.contains("/ 180s"),
            "a step no table names is at the default 180: {line}"
        );
    }
    for name in ["diffgate", "secrets", "fmt"] {
        let line = step_line(&out, name);
        assert!(
            line.contains("/ 60s"),
            "a limit never lifts a step above its own 60 s timeout: {line}"
        );
    }
    for name in STEPS {
        let line = step_line(&out, name);
        assert!(
            line.contains(" / "),
            "every step line prints its limit: {line}"
        );
    }
    assert!(
        !out.contains("near-limit"),
        "nothing came near a limit: {out}"
    );
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}

/// The step with the larger limit passes at a time the limit of every step cuts, and a step left at the smaller one still
/// fails at its own limit. The table's own 240-against-180 pair is the same rule minutes apart: it is checked without sleeping
/// four minutes (`merge_limits_are_per_step_with_an_180_default` and
/// `a_merge_limit_cuts_a_step_s_own_timeout_and_never_lifts_it` in `src/main.rs`).
#[test]
fn merge_gate_a_step_with_a_larger_limit_passes_where_the_default_cuts_it() {
    let (repo, bin) = setup("bigger", "1.5", "1.5", "1.5");
    let (code, out) = run(&repo, &bin, Some("1,clippy=3,unit-tests=3"));
    assert_ne!(code, 0, "two steps timed out: {out}");
    for name in ["clippy", "unit-tests"] {
        let passed = step_line(&out, name);
        assert!(
            passed.starts_with("ok  ") && passed.contains("/ 3s"),
            "{name} ran 1.5 s of its override's 3 s: {passed}"
        );
    }
    for name in ["unit-tests-tools-ci-fake", "diffgate"] {
        let late = step_line(&out, name);
        assert!(
            late.starts_with("FAIL") && late.contains("/ 1s"),
            "{name} is at the override's 1 s, same 1.5 s of work: {late}"
        );
        assert!(
            out.contains(&format!(
                "{name}: past its 1 s limit at merge: the machine is loaded; retry when it is quieter, or run scripts/check-fast in the worktree"
            )),
            "the failure names the step's own limit: {out}"
        );
    }
    assert!(
        !out.lines()
            .any(|l| l.starts_with("skipped") && l.contains("past its")),
        "a timeout is never moved to skipped: {out}"
    );
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}

/// A step that used more than 80% of its limit says so on its own line, so the next raise of a number comes from a log line
/// rather than from a merge that failed.
#[test]
fn merge_gate_a_step_past_eighty_percent_of_its_limit_prints_near_limit() {
    let (repo, bin) = setup("near", "0.2", "4.2", "0.2");
    let (_code, out) = run(&repo, &bin, Some("8,clippy=5"));
    let line = step_line(&out, "clippy");
    assert!(
        line.starts_with("ok  ") && line.contains("/ 5s") && line.contains("near-limit"),
        "4.2 s of 5 s is past 80%: {line}\n{out}"
    );
    for name in ["unit-tests", "diffgate"] {
        assert!(
            !step_line(&out, name).contains("near-limit"),
            "0.2 s of an 8 s limit is not near it: {}",
            step_line(&out, name)
        );
    }
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}
