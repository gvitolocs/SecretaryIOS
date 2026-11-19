//! GATE-06: a `check-fast` that is already running must not lose a step because the shared tool was rebuilt under it.
//!
//! The launcher `scripts/check-fast` builds `lucina-ci` and installs it at one path every worktree shares
//! (`~/.cache/secretary-ci-tool/release/lucina-ci`), then execs it — and every step of the check is that same binary started
//! again as `__inner` (`runner::run_unit`), as is the `inventory` step, which runs the tool by path. A second check-fast, or a
//! merge landing, builds again, and that build takes the name away from the run in flight: in merge job job_fc4fabf8e6fe four
//! steps — `unit-tests-build`, `unit-tests`, `secrets`, `inventory` — died 0.1 s after `clippy` with
//! `Failed to find executable /home/nez/.cache/secretary-ci-tool/release/lucina-ci (deleted)`.
//!
//! Two things close it, and each is what a test here holds:
//!
//!  * a run keeps the binary it started with: at its start `check-fast` copies itself into its own per-run directory and every
//!    step is started from that copy, so no rebuild of a path it no longer reads can touch it (`self_copy` and
//!    `runner::install_step_exe`). Chosen over keying the installed binary by its source id, because the id only separates
//!    *different* sources — two runs of one source, which is what a merge and its own lane both build, still share a name and
//!    still replace each other, and the deleted-path case is a rename of the very file a run was started from;
//!  * the install is a `rename(2)` onto the final name and never a write through it, so the file a run was started from keeps
//!    its bytes, and a build that died leaves the previous install complete (the build output is copied aside rather than
//!    hard-linked to the shared name, because a link there would let the next build write through it).
//!
//! GATE-07 is the same directory: it was left behind by every run that did not reach the last line of `cmd_check_fast`. The
//! tests look after a clean run, after a step that fails and after a check that aborts on an error half way — and the trees
//! these tests make go with them whether they pass or fail.
//!
//! The swap is never a matter of luck: the first step of the run parks itself until the test says the tool has been replaced
//! (`SWAP_HOLD` and `hold` in the stub), and every step after it records whether the replacement was already in place when it
//! started. So the log proves the swap fell between two steps of one run, and a run that finished first cannot pass.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// The memory cap a spawned `check-fast` asks for, as in the other merge-gate tests: these tests judge which binary a step is
/// started from and not the machine's RAM, so they ask for a cap every runner holds (`runner::budget`).
const MEM_CAP_GIB: &str = "1";

/// The names a lane's git config comes in with, of which `bq` keeps `GIT_CONFIG_COUNT` and drops `GIT_CONFIG_KEY_0`, which
/// leaves a git that reads nothing. Everything here runs with the trio gone.
const LANE_GIT_CONFIG: [&str; 3] = ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"];

/// The stub steps of one run, in the order they run and as each writes itself to the log: the four build steps of the one
/// touched workspace member, the diffgate of the base, and the secret check. `inventory` is not here because it starts the
/// tool itself and not a stub on PATH — and it is one of the four steps the merge job of GATE-06 lost.
const STUB_STEPS: &str =
    "diffgate\ncargo-fmt\ncargo-clippy\ncargo-test-no-run\ncargo-test\ngitleaks";

/// What a `--proofs` run says when the toolchain does not match the lock: the words of the check that aborts it.
const LOCK_PINS: &str = "verus.lock pins";

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
/// `merge_gate_unit_tests_split.rs` gives for it).
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

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_default()
}

fn append(p: &Path) -> fs::File {
    fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(p)
        .unwrap()
}

/// What is in a directory, by name, sorted: for a message that says what a run left behind.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{} cannot be read: {e}", dir.display()))
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// One test's own tree, and the `check-fast` it is running. `Drop` takes the tree away whether the test passed, failed an
/// assertion half way or panicked, and reaps the run it left behind first, because a live check would else go on writing into
/// the directory it is deleted out of.
struct Scratch {
    dir: PathBuf,
    run: Option<Child>,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("cfswap-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for d in ["markers", "tmp", "bin"] {
            fs::create_dir_all(dir.join(d)).unwrap();
        }
        Scratch { dir, run: None }
    }

    fn at(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn markers(&self) -> PathBuf {
        self.dir.join("markers")
    }

    /// The file whose existence says the shared tool has been replaced: the stubs test for it, and the test writes it.
    fn swapped(&self) -> PathBuf {
        self.dir.join("swapped")
    }

    fn bin(&self) -> PathBuf {
        self.dir.join("bin")
    }

    fn tmp(&self) -> PathBuf {
        self.dir.join("tmp")
    }

    /// The directory a `check-fast` of process `pid` makes for itself under the `TMPDIR` it was given.
    fn run_dir(&self, pid: u32) -> PathBuf {
        self.tmp().join(format!("check-fast-{pid}"))
    }

    /// The installed tool: the binary under test, at the path the launcher shares between worktrees. It is the path the run is
    /// started from, and the one another build replaces.
    fn install_tool(&self) -> PathBuf {
        let to = self.at("tool/release/lucina-ci");
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(env!("CARGO_BIN_EXE_lucina-ci"), &to).unwrap();
        fs::set_permissions(&to, fs::Permissions::from_mode(0o755)).unwrap();
        to
    }

    /// Starts a run with its stdout and stderr in one log file a test can read at any time — including while it runs, which a
    /// pipe would not allow once its buffer is full. Returns the pid, which names the run's own directory.
    fn spawn(&mut self, c: &mut Command, log: &Path) -> u32 {
        // Both ends on the same log file, at its end: a file, not a pipe, so a test can read what a run printed while the
        // run is still alive.
        let f = append(log);
        c.stdout(Stdio::from(f.try_clone().unwrap()));
        c.stderr(Stdio::from(f));
        let child = c.spawn().unwrap();
        let pid = child.id();
        self.run = Some(child);
        pid
    }

    fn try_wait(&mut self) -> Option<ExitStatus> {
        self.run.as_mut().unwrap().try_wait().unwrap()
    }

    fn wait(&mut self) -> ExitStatus {
        self.run.as_mut().unwrap().wait().unwrap()
    }

    /// Waits for the first step to say it started, which is the moment the test may take the tool away from under the run. A
    /// run that is over by then never got that far, and says so in its own log.
    fn wait_for_first_step(&mut self, log: &Path) {
        let started = self.markers().join("diffgate-started");
        let t = Instant::now();
        loop {
            if started.exists() {
                return;
            }
            if self.try_wait().is_some() {
                panic!(
                    "the run was over before its first step started: {}",
                    read(log).trim()
                );
            }
            assert!(
                t.elapsed() < Duration::from_secs(90),
                "the first step never started: {}",
                read(log).trim()
            );
            sleep(Duration::from_millis(20));
        }
    }

    /// What each stub step recorded: `<step> swapped=<yes|no>`, in the order they ran.
    fn steps(&self) -> String {
        read(&self.markers().join("steps"))
    }

    /// Every `check-fast-<pid>` directory left anywhere in the test's `TMPDIR`, of this run or of one before it.
    fn leftover_run_dirs(&self) -> Vec<String> {
        names(&self.tmp())
            .into_iter()
            .filter(|n| n.starts_with("check-fast-"))
            .collect()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(c) = &mut self.run {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Every stub tool one `check-fast` step may want. Each records that it ran and whether the shared tool had already been
/// replaced by then; `diffgate` parks the run while the test takes it away, when `SWAP_HOLD` names it.
fn stubs(s: &Scratch, cargo_body: &str) {
    let prelude = r#"#!/usr/bin/env bash
mark() {
  local swapped=no
  [ -n "${SWAPPED:-}" ] && [ -e "$SWAPPED" ] && swapped=yes
  printf '%s swapped=%s\n' "$1" "$swapped" >> "$MARKERS/steps"
}
hold() {
  [ "${SWAP_HOLD:-}" = "$1" ] || return 0
  touch "$MARKERS/$1-started"
  # Parked on purpose, so the swap certainly falls between two steps of one run and never after the last one of them. The
  # bound is the step's own clock: a loaded machine kills the step at its 60 s rather than stretch this wait past it.
  while [ "$SECONDS" -lt 20 ]; do
    [ -e "${SWAPPED:-/none}" ] && return 0
    sleep 0.05
  done
}
"#;
    write_exec(
        &s.bin().join("diffgate"),
        &format!("{prelude}hold diffgate\nmark diffgate\n"),
    );
    write_exec(
        &s.bin().join("gitleaks"),
        &format!("{prelude}mark gitleaks\n"),
    );
    write_exec(
        &s.bin().join("cargo"),
        &format!(
            r#"{prelude}sub=""
no_run=0
for a in "$@"; do
  case "$a" in
    fmt|clippy|test) sub="$a";;
    --no-run) no_run=1;;
  esac
done
label="cargo-$sub"
[ "$sub" = test ] && [ "$no_run" = 1 ] && label=cargo-test-no-run
mark "$label"
{cargo_body}
"#
        ),
    );
}

/// A throwaway repository whose one change is `crates/demo/src/lib.rs`, with the inventory this very binary wrote — so
/// `inventory --check` is current, the run reaches every step `check-fast` has and passes them all, and a `--proofs` run has
/// a unit to ask for. The crate is a workspace member (which is what makes the four cargo steps run) and one of its files is
/// listed as shipped proof (which is what makes `hits` non-empty).
fn repo(s: &Scratch, tool: &Path) -> PathBuf {
    let r = s.at("repo");
    let src = r.join("crates/demo/src");
    for d in [
        "crates/demo/src",
        "tools/ci",
        "scripts",
        "docs/ci",
        "verification/verus",
    ] {
        fs::create_dir_all(r.join(d)).unwrap();
    }
    fs::write(
        r.join("Cargo.toml"),
        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/demo\"]\n",
    )
    .unwrap();
    fs::write(
        r.join("crates/demo/Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(src.join("lib.rs"), "pub fn one() -> u32 { 1 }\n").unwrap();
    fs::write(src.join("rule.rs"), "pub fn two() -> u32 { 2 }\n").unwrap();
    fs::write(
        r.join("scripts/verify-shipped.sh"),
        "files=(crates/demo/src/rule.rs)\n",
    )
    .unwrap();
    fs::write(
        r.join("tools/ci/proof-groups.tsv"),
        "# group\tkeys\nmodels\tmodels\ndemo\tdemo\n",
    )
    .unwrap();
    // A lock of another release than `/bin/true --version` answers with: enough for `--proofs` to get as far as the
    // toolchain check, which is where such a run then aborts.
    fs::write(
        r.join("tools/ci/verus.lock"),
        "[verus]\nrelease = \"11.0\"\nbinary_sha256 = \"be\"\nverifier_sha256 = \"fe\"\n",
    )
    .unwrap();
    git(&r, &["init", "-q", "-b", "main"]);
    git(&r, &["add", "-A"]);
    git(
        &r,
        &["commit", "-q", "-m", "everything but the proof inventory"],
    );
    let o = Command::new(tool)
        .args(["inventory", "--write"])
        .current_dir(&r)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "inventory --write: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    for f in ["tools/ci/proof-inventory.tsv", "docs/ci/PROOF-MATRIX.md"] {
        assert!(
            r.join(f).is_file(),
            "{f}: written by inventory --write before the run that checks it"
        );
    }
    git(&r, &["add", "-A"]);
    git(
        &r,
        &["commit", "-q", "-m", "the inventory the run checks against"],
    );
    git(&r, &["checkout", "-q", "-b", "work"]);
    fs::write(src.join("lib.rs"), "pub fn one() -> u32 { 1 }\n\n").unwrap();
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-q", "-m", "one crate"]);
    r
}

/// A `check-fast` run over `r`: the stub tools first on PATH, the run's per-run directory inside this test's own tree, and
/// none of the merge-gate or waiver environment. With `hold`, its first step waits for the test.
///
/// Every run asks for the scope-less fallback (`LUCINA_CI_NO_SCOPE`, the switch `runner_kill_scope.rs` drives the same
/// branch with): on a lane worker `systemd-run` is a shim that refuses the step with 79 and HOST-08, so a test that went
/// through systemd would judge the machine it happens to run on rather than the code. Which branch a step is started from
/// does not matter here — both start it from the path `step_exe()` answers with, and that is the thing under test; where
/// there is no copy, the scope says `Failed to find executable ... (deleted)` and the fallback says `cannot start ...:
/// No such file or directory`, and both are refused below.
fn check_fast(s: &Scratch, r: &Path, proofs: bool, hold: bool) -> Command {
    let mut c = Command::new(s.at("tool/release/lucina-ci"));
    c.arg("check-fast")
        .args(["--base", "main", "--mem-cap-gib", MEM_CAP_GIB])
        .current_dir(r)
        .env("LUCINA_CI_NO_SCOPE", "1")
        .env(
            "PATH",
            format!(
                "{}:{}",
                s.bin().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("MARKERS", s.markers())
        .env("SWAPPED", s.swapped())
        .env("TMPDIR", s.tmp())
        .env_remove("SWAP_HOLD")
        .env_remove("LAGENT_ALLOW")
        .env_remove("LAGENT_WAIVERS_FILE")
        .env_remove("LUCINA_CHECK_FAST_MERGE_CAP_S")
        .env_remove("VERUS");
    if hold {
        c.env("SWAP_HOLD", "diffgate");
    }
    if proofs {
        // A Verus that exists, so the run is on its way to the proofs when the toolchain check stops it.
        c.arg("--proofs").env("VERUS", "/bin/true");
    }
    drop_lane_git_config(&mut c);
    c
}

/// `scripts/check-fast`, the launcher under test: a shell script of the repository, not of the crate.
fn launcher() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../scripts/check-fast");
    assert!(p.is_file(), "no launcher at {}", p.display());
    p
}

/// A run of the launcher: the stubs on PATH, the scratch `tool` directory as the shared install place, and no build
/// directory of its own (`SECRETARY_CI_TOOL_BUILD` then defaults to `<tool>-build` beside it).
fn launcher_run(s: &Scratch) -> Command {
    let mut c = Command::new("bash");
    c.arg(launcher())
        .env(
            "PATH",
            format!(
                "{}:{}",
                s.bin().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("SECRETARY_CI_TOOL_TARGET", s.at("tool"))
        .env("TMPDIR", s.tmp())
        .env_remove("SECRETARY_CI_TOOL_BUILD");
    drop_lane_git_config(&mut c);
    c
}

/// The copy of the tool a run made for itself, wherever in its directory it put it.
fn find_self_copy(dir: &Path) -> Option<PathBuf> {
    for e in fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(q) = find_self_copy(&p) {
                return Some(q);
            }
        } else if p.file_name().is_some_and(|n| n == "lucina-ci")
            && fs::metadata(&p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
        {
            return Some(p);
        }
    }
    None
}

/// (a) The run in flight loses its tool: a rebuild installs another binary over the shared name the way the launcher does —
/// a new file renamed into place — and the name is then removed as well. Every step after that still runs, from the copy the
/// run made of itself at its start, and the check passes.
#[test]
fn a_check_fast_run_keeps_the_tool_it_started_with_when_the_shared_one_is_replaced() {
    let mut s = Scratch::new("swap");
    let tool = s.install_tool();
    stubs(&s, "exit 0");
    let r = repo(&s, &tool);
    let log = s.at("run.log");
    let mut c = check_fast(&s, &r, false, true);
    let pid = s.spawn(&mut c, &log);
    s.wait_for_first_step(&log);
    assert!(
        s.run_dir(pid).is_dir(),
        "the run works in a directory of its own: {}",
        s.run_dir(pid).display()
    );

    let wrong = s.at("tool/release/lucina-ci.tmp.replacement");
    write_exec(
        &wrong,
        "#!/usr/bin/env bash\necho 'THE SHARED TOOL WAS STARTED AGAIN MID-RUN' >&2\nexit 47\n",
    );
    fs::rename(&wrong, &tool).unwrap();
    fs::remove_file(&tool).unwrap();
    assert!(
        !tool.exists(),
        "the shared tool is gone while the run is in flight: {}",
        tool.display()
    );
    fs::write(s.swapped(), b"swapped\n").unwrap();

    let status = s.wait();
    let text = read(&log);
    assert!(status.success(), "the run still passes: {text}");
    assert!(text.contains(", 0 failed, "), "nothing failed: {text}");
    for step in [
        "diffgate",
        "fmt",
        "clippy",
        "unit-tests-build",
        "unit-tests",
        "secrets",
        "inventory",
    ] {
        assert!(
            text.lines()
                .any(|l| l.starts_with("ok  ") && l.split_whitespace().nth(1) == Some(step)),
            "step {step} did not run to the end:\n{text}"
        );
    }
    // And they ran *after* the swap, each one saying so. The run never went back to the name that was replaced and removed,
    // where there was nothing left to read — a step that tried would have failed the two checks above, the `inventory` step
    // of GATE-06 among them, since it starts the tool itself and not a stub on PATH.
    let ran = s.steps();
    let lines: Vec<&str> = ran.lines().collect();
    let got: Vec<&str> = lines
        .iter()
        .map(|l| l.split_whitespace().next().unwrap_or(""))
        .collect();
    assert_eq!(
        got,
        STUB_STEPS.lines().collect::<Vec<_>>(),
        "every stub step ran, and in order:\n{ran}"
    );
    assert!(
        lines.iter().all(|l| l.ends_with(" swapped=yes")),
        "every step started with the shared tool already gone:\n{ran}"
    );
    for bad in [
        "Failed to find executable",
        "cannot start",
        "THE SHARED TOOL WAS STARTED AGAIN",
    ] {
        assert!(!text.contains(bad), "the run says {bad:?}:\n{text}");
    }
}

/// (c) The per-run directory is the run's own and goes with it: it holds the copy the steps are started from while the run
/// lives, and nothing of it is left after a clean exit, after a step that fails, or after a check that aborts on an error
/// half way — the shape of exit GATE-07 left behind.
#[test]
fn the_per_run_directory_holds_the_run_and_leaves_nothing_behind() {
    for (tag, proofs, cargo_body, want) in [
        ("clean", false, "exit 0", 0),
        (
            "failing-step",
            false,
            r#"if [ "$sub" = clippy ]; then exit 1; fi"#,
            1,
        ),
        ("aborted", true, "exit 0", 2),
    ] {
        let mut s = Scratch::new(tag);
        let tool = s.install_tool();
        stubs(&s, cargo_body);
        let r = repo(&s, &tool);
        let hold = !proofs && tag == "clean";
        let log = s.at("run.log");
        let mut c = check_fast(&s, &r, proofs, hold);
        let pid = s.spawn(&mut c, &log);
        if hold {
            s.wait_for_first_step(&log);
            let seen = s.run_dir(pid);
            assert!(seen.is_dir(), "the run's directory is {}", seen.display());
            let copy = find_self_copy(&seen).unwrap_or_else(|| {
                panic!(
                    "the run never copied the tool it started from into its own directory:\n{}",
                    read(&log)
                )
            });
            assert_eq!(
                fs::metadata(&copy).unwrap().len(),
                fs::metadata(&tool).unwrap().len(),
                "the copy is the tool, and it is not the shared name"
            );
            fs::write(s.swapped(), b"release the first step\n").unwrap();
        }
        let status = s.wait();
        let text = read(&log);
        assert_eq!(status.code(), Some(want), "{tag} exited wrongly:\n{text}");
        if proofs {
            assert!(
                text.contains(LOCK_PINS),
                "{tag}: the run got to its toolchain check and says why it stops:\n{text}"
            );
        }
        if want == 1 {
            assert!(
                text.contains("FAIL clippy") && text.contains(", 1 failed, "),
                "{tag}: exactly the one step failed:\n{text}"
            );
        }
        assert!(
            !s.run_dir(pid).exists(),
            "{tag}: its own directory is gone, and everything in it"
        );
        assert!(
            s.leftover_run_dirs().is_empty(),
            "{tag}: nothing left in its TMPDIR: {:?}",
            s.leftover_run_dirs()
        );
    }
}

/// (b), first half: the launcher never writes the shared name it replaces. A hard link to the previous install catches it —
/// a write through the path would go through the link and change the bytes a run in flight is executing; a rename cannot.
#[test]
fn the_launcher_installs_the_tool_by_rename_and_not_by_writing_the_name() {
    let s = Scratch::new("install");
    // The launcher asks for 20 GiB of room on `$HOME` before it builds; the answer here is the test's and not the machine's,
    // so the test does not depend on how full nezopt happens to be.
    write_exec(&s.bin().join("df"), "#!/usr/bin/env bash\necho '  999G'\n");
    let mut first = String::new();
    for version in ["one", "two"] {
        // The stand-in build: it leaves its artifact in the build directory, which is where the install reads from and never
        // writes to.
        write_exec(
            &s.bin().join("cargo"),
            &r#"#!/usr/bin/env bash
mkdir -p "$CARGO_TARGET_DIR/release"
{ printf '#!/usr/bin/env bash\necho TOOL-VERSION-'; echo %VERSION%; } > "$CARGO_TARGET_DIR/release/lucina-ci"
chmod 0755 "$CARGO_TARGET_DIR/release/lucina-ci"
exit 0
"#
            .replace("%VERSION%", version),
        );
        let o = launcher_run(&s).output().unwrap();
        let said = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(o.status.success(), "the launcher and its build: {said}");
        assert!(
            said.contains(&format!("TOOL-VERSION-{version}")),
            "the launcher execs the tool it just installed, and not the one before it: {said}"
        );
        let installed = read(&s.at("tool/release/lucina-ci"));
        assert!(
            installed.contains(&format!("TOOL-VERSION-{version}")),
            "the new build is what sits at the shared name: {installed}"
        );
        assert_eq!(
            names(&s.at("tool/release")),
            vec!["lucina-ci".to_string()],
            "the install leaves nothing but the tool in its directory"
        );
        if version == "one" {
            fs::hard_link(s.at("tool/release/lucina-ci"), s.at("keep")).unwrap();
            first = installed;
        } else {
            assert_eq!(
                read(&s.at("keep")),
                first,
                "the file the first run was started from still holds what it held: nobody wrote through its name"
            );
        }
    }
}

/// (b), second half: a build that dies leaves the installed tool exactly as it was, with nothing partial at the final name
/// and nothing new beside it — the build output is copied aside first, and only a whole file is ever renamed over the shared
/// name.
#[test]
fn a_failed_build_leaves_the_installed_tool_whole_and_no_partial_file_behind() {
    let s = Scratch::new("build-failed");
    fs::create_dir_all(s.at("tool/release")).unwrap();
    let installed = s.at("tool/release/lucina-ci");
    let previous = "#!/usr/bin/env bash\necho PREVIOUS-INSTALL\n";
    write_exec(&installed, previous);
    write_exec(&s.bin().join("df"), "#!/usr/bin/env bash\necho '  999G'\n");
    // The worst a build has: it leaves a torn artifact where the install would read from, and then fails.
    write_exec(
        &s.bin().join("cargo"),
        r#"#!/usr/bin/env bash
mkdir -p "$CARGO_TARGET_DIR/release"
printf '#!/usr/bin/env ba' > "$CARGO_TARGET_DIR/release/lucina-ci"
echo 'stub cargo: the build died' >&2
exit 101
"#,
    );
    let o = launcher_run(&s).output().unwrap();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(
        o.status.code(),
        Some(101),
        "the launcher fails with its build and execs nothing: {said}"
    );
    assert!(
        !said.contains("PREVIOUS-INSTALL"),
        "the failed run is not handed the old tool either: {said}"
    );
    assert_eq!(
        fs::read(&installed).unwrap(),
        previous.as_bytes(),
        "the tool that was there before is the tool that is there now, byte for byte"
    );
    assert_eq!(
        names(&s.at("tool/release")),
        vec!["lucina-ci".to_string()],
        "nothing was left beside it"
    );
    assert_eq!(
        read(&s.at("tool-build/release/lucina-ci")),
        "#!/usr/bin/env ba",
        "the build's own file stayed exactly as the dead build left it: nothing was renamed from it over the shared name"
    );
}

/// GATE-16: the install's temp name is `lucina-ci.tmp.<pid of the launcher's shell>`, in a directory other processes may be
/// able to write, and a symlink planted at it before the copy must not be followed. A wrapper plants the link under its own pid
/// and execs the launcher, which keeps that pid, so the name is the very one the launcher will use. The file the link points
/// at keeps its bytes, and what is installed is the new build, a plain file.
#[test]
fn a_symlink_planted_at_the_install_temp_name_is_not_followed() {
    let s = Scratch::new("tmplink");
    write_exec(&s.bin().join("df"), "#!/usr/bin/env bash\necho '  999G'\n");
    write_exec(
        &s.bin().join("cargo"),
        r#"#!/usr/bin/env bash
mkdir -p "$CARGO_TARGET_DIR/release"
printf '#!/usr/bin/env bash\necho TOOL-VERSION-built\n' > "$CARGO_TARGET_DIR/release/lucina-ci"
chmod 0755 "$CARGO_TARGET_DIR/release/lucina-ci"
exit 0
"#,
    );
    fs::create_dir_all(s.at("tool/release")).unwrap();
    let victim = s.at("victim");
    fs::write(&victim, "VICTIM-BEFORE").unwrap();
    let mut c = Command::new("bash");
    c.args([
        "-c",
        "ln -s \"$VICTIM\" \"$SECRETARY_CI_TOOL_TARGET/release/lucina-ci.tmp.$$\"; exec bash \"$0\"",
    ])
    .arg(launcher())
    .env("VICTIM", &victim);
    launcher_env(&mut c, &s);
    let o = c.output().unwrap();
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(o.status.success(), "the launcher and its build: {said}");
    assert!(said.contains("TOOL-VERSION-built"), "{said}");
    assert_eq!(
        read(&victim),
        "VICTIM-BEFORE",
        "the copy went through the planted link and wrote the file it points at"
    );
    let installed = s.at("tool/release/lucina-ci");
    let m = fs::symlink_metadata(&installed).unwrap();
    assert!(
        m.is_file(),
        "the installed tool is a plain file, not the link"
    );
    assert!(read(&installed).contains("TOOL-VERSION-built"));
    assert_eq!(
        names(&s.at("tool/release")),
        vec!["lucina-ci".to_string()],
        "the planted link did not survive beside the tool"
    );
}
