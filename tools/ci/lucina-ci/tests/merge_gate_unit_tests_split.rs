//! `check-fast --merge-gate` builds the tests and runs them as two steps (finding GATE-08: the one `unit-tests` step spent its
//! time building and linking the test binaries before it ran any of them, and crossed its temporary 480 s at load 28 — merge job
//! job_24cf4973dd74. JD-C018's measurement of `cargo test` for `lucina-server` (~50 test binaries),
//! `/home/nez/scratch/platform-tools/jd-c018-tests.log`, reads Finished test profile [...] in 8m 45s under a 1-minute load of
//! 24.59 while the tests themselves ran 40 s.) The real binary runs over a throwaway repository with a stub `cargo` first on PATH
//! that sleeps and prints what cargo prints, so no crate is really built and no step waits a real 1200 s.
//!
//! Three things the split is for, and each is what a test here holds: the build has its own limit and its own words, because
//! running out of it is a loaded machine and not a failed test; the run is timed alone and names the tests it was in the middle
//! of when it was cut; and a compile error is still the build step's own verdict, not a slow-build message.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The memory cap the spawned `check-fast` asks for: these tests judge the time limit, not the machine's RAM, so they ask for
/// a cap every runner holds (`runner::budget`).
const MEM_CAP_GIB: &str = "1";

/// The names a lane's git config comes in with, of which `bq` takes `GIT_CONFIG_KEY_0` out of a job's environment because
/// its name holds KEY and leaves `GIT_CONFIG_COUNT` behind — which no git can read. Everything here runs with the trio gone.
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

/// An executable script, written by a child that is reaped before anything execs it (ETXTBSY; the same reason
/// `merge_gate_step_limits.rs` gives for it).
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

/// A repository whose `work` branch adds one workspace member (`crates/demo`), which is what makes the plain
/// `unit-tests-build` and `unit-tests` steps run. The stub `cargo` runs `build` for `cargo test --no-run` and `run` for
/// `cargo test` without it, and does nothing at all for `fmt` and `clippy` — those steps are what the other file judges.
fn setup(tag: &str, build: &str, run: &str) -> (PathBuf, PathBuf) {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("mguts-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    let repo = d.join("repo");
    let member = repo.join("crates/demo");
    fs::create_dir_all(member.join("src")).unwrap();
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
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &["commit", "-q", "--allow-empty", "-m", "empty base"],
    );
    git(&repo, &["checkout", "-q", "-b", "work"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "one crate"]);
    let bin = d.join("bin");
    fs::create_dir_all(&bin).unwrap();
    write_exec(
        &bin.join("cargo"),
        &format!(
            r#"sub=""
no_run=0
for a in "$@"; do
  case "$a" in
    fmt|clippy|test) sub="$a";;
    --no-run) no_run=1;;
  esac
done
[ "$sub" = test ] || exit 0
if [ "$no_run" = 1 ]; then
{build}
else
{run}
fi
"#
        ),
    );
    write_exec(&bin.join("diffgate"), "exit 0\n");
    write_exec(&bin.join("gitleaks"), "exit 0\n");
    (repo, bin)
}

/// `cap` is `LUCINA_CHECK_FAST_MERGE_CAP_S`, so a test cuts one step at its own small number instead of waiting on the
/// table's 1200 s and 240 s.
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

/// The line of one step.
fn step_line(out: &str, name: &str) -> String {
    out.lines()
        .find(|l| l.split_whitespace().nth(1) == Some(name))
        .unwrap_or_else(|| panic!("no line for step {name}:\n{out}"))
        .to_string()
}

/// Where a step's line sits in the output.
fn step_pos(out: &str, name: &str) -> usize {
    out.lines()
        .position(|l| l.split_whitespace().nth(1) == Some(name))
        .unwrap_or_else(|| panic!("no line for step {name}:\n{out}"))
}

/// The 1-minute load average, read the same way the binary reads it.
fn loadavg_1min() -> f64 {
    fs::read_to_string("/proc/loadavg")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

/// The build of the tests and the run of them are two steps, each printing the limit of its own row of `MERGE_STEP_LIMITS`:
/// 1200 s for the build, and the 240 s the measured week asked for for the run — which is no longer the temporary 480 s,
/// since the run no longer carries the build with it; the 480 s of `clippy` (GATE-13) is a different row.
#[test]
fn merge_gate_builds_the_tests_and_runs_them_as_two_steps_with_two_limits() {
    let (repo, bin) = setup("split", "exit 0", "exit 0");
    let (_code, out) = run(&repo, &bin, None);
    let built = step_line(&out, "unit-tests-build");
    let ran = step_line(&out, "unit-tests");
    assert!(
        built.starts_with("ok  ") && built.contains("/ 1200s"),
        "the build step is at its own 20 min: {built}"
    );
    assert!(
        ran.starts_with("ok  ") && ran.contains("/ 240s"),
        "the run step is at the 240 s it was measured to need: {ran}"
    );
    assert!(
        !built.contains("/ 480s") && !ran.contains("/ 480s"),
        "the temporary 480 s is gone from both unit-tests lines: {built} / {ran}"
    );
    assert!(
        out.contains(", 0 failed, "),
        "neither step failed and nothing else came near a limit: {out}"
    );
    // Built before run: the run is the step that finds the binaries already there.
    assert!(
        step_pos(&out, "unit-tests-build") < step_pos(&out, "unit-tests"),
        "the build line comes first:\n{out}"
    );
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}

/// A build that runs out of its cap fails the gate — nothing merges on tests nobody built — but it is said as a slow build at
/// the machine's load and not as a test verdict, because no test of the change ran at all.
#[test]
fn merge_gate_a_build_past_its_cap_says_build_too_slow_with_the_load_and_still_fails() {
    let (repo, bin) = setup("slowbuild", "sleep 6", "exit 0");
    let before = loadavg_1min();
    let (code, out) = run(&repo, &bin, Some("unit-tests-build=2"));
    assert_ne!(
        code, 0,
        "a build that never finished fails the merge: {out}"
    );
    let line = step_line(&out, "unit-tests-build");
    assert!(
        line.starts_with("FAIL") && line.contains("/ 2s"),
        "the build step is cut at the limit given it: {line}"
    );
    let said = out
        .lines()
        .find(|l| l.contains("unit-tests-build: build too slow at load "))
        .unwrap_or_else(|| panic!("the slow build says so: {out}"))
        .to_string();
    assert!(
        said.contains(": not a test verdict"),
        "it is not a test that failed: {said}"
    );
    let figure: f64 = said
        .split("at load ")
        .nth(1)
        .unwrap()
        .split(':')
        .next()
        .unwrap()
        .parse()
        .unwrap_or_else(|_| panic!("{said} carries the load average it read"));
    assert!(
        (figure - before).abs() < 25.0,
        "{figure} is this machine's 1-minute load ({before} when the run started)"
    );
    assert!(
        !out.contains("unit-tests-build: past its"),
        "a build is never reported as a late test: {out}"
    );
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}

/// The run is timed alone now, so being cut at its limit means a test was still going: the step prints the harness's own
/// `has been running for over 60 seconds` line, which sits far above the tail of the log the failure already shows, and the
/// last lines that name a test.
#[test]
fn merge_gate_a_run_past_its_limit_prints_the_tests_it_was_cut_in() {
    let build_tail = (1..=40)
        .map(|i| format!("echo 'note: unrelated output line {i}'"))
        .collect::<Vec<_>>()
        .join("\n");
    let (repo, bin) = setup(
        "hang",
        "exit 0",
        &format!(
            r#"{{
  echo 'running 12 tests'
  echo 'test net::upload has been running for over 60 seconds'
  echo 'Running tests/net.rs (target/debug/deps/net-1abcd234)'
{build_tail}
  echo 'test net::slow_a ... ok'
  echo 'test net::slow_b ... ok'
}} 2>&1
sleep 9"#
        ),
    );
    let (code, out) = run(&repo, &bin, Some("unit-tests=3"));
    assert_ne!(code, 0, "a run cut at its limit fails the merge: {out}");
    assert!(
        step_line(&out, "unit-tests").starts_with("FAIL"),
        "the run step failed: {out}"
    );
    assert!(
        out.contains(
            "unit-tests: past its 3 s limit at merge: the machine is loaded; retry when it is quieter, or run scripts/check-fast in the worktree"
        ),
        "the run keeps the words of a step cut at its limit: {out}"
    );
    assert!(
        out.contains("at the cut: test net::upload has been running for over 60 seconds"),
        "the hung test names itself, though 40 lines went under it: {out}"
    );
    assert!(
        out.contains("at the cut: test net::slow_b ... ok"),
        "so do the last tests the log named: {out}"
    );
    assert!(
        !out.contains("at the cut: note: unrelated"),
        "it is the tests that are picked out, not any line: {out}"
    );
    assert!(
        !out.lines()
            .any(|l| l.starts_with("skipped") && l.contains("past its")),
        "a timeout is never moved to skipped: {out}"
    );
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}

/// A compile error is the build step's own verdict: it fails as `unit-tests-build`, with the compiler's words under it, and
/// not as a build that was merely slow.
#[test]
fn merge_gate_a_compile_error_fails_as_the_build_step_and_not_as_a_slow_build() {
    let (repo, bin) = setup(
        "broken",
        r#"echo 'error[E0308]: mismatched types'
echo '  --> crates/demo/src/lib.rs:1:26'
echo 'error: could not compile `demo` (lib test) due to 1 previous error'
exit 1"#,
        "exit 0",
    );
    let (code, out) = run(&repo, &bin, None);
    assert_ne!(code, 0, "a build error fails the merge: {out}");
    assert!(
        step_line(&out, "unit-tests-build").starts_with("FAIL"),
        "the failure is the build step's: {}",
        step_line(&out, "unit-tests-build")
    );
    assert!(
        out.contains("error[E0308]: mismatched types"),
        "the compiler's own words are printed under the step: {out}"
    );
    assert!(
        !out.contains("build too slow"),
        "a step that finished with an error is not a slow build: {out}"
    );
    assert!(
        !out.contains("past its"),
        "and not a step that ran out of time either: {out}"
    );
    assert!(
        out.contains(", 1 failed, "),
        "exactly the one step failed: {out}"
    );
    let _ = fs::remove_dir_all(repo.parent().unwrap());
}
