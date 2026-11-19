//! `lagent work` end to end: a real daemon, a stand-in `lane` (a small `sh` script that keeps
//! one file per run id in its directory) and a test lane config. `run` stores the arguments
//! and stdin; `status <id>` prints `<id>.status` (the test rewrites it to move the run on) and
//! exits 2 without one; `ls` prints `ls.jsonl` (JSON Lines, like the real `lane ls --json`)
//! and exits 1 when `ls-fail` exists. The status files have the real shape, built from a
//! sample captured from the installed `lane`.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{Daemon, Run};
use serde_json::{json, Value};

/// `lane status` of a run that is running, as the real `lane` prints it.
const STATUS_SAMPLE: &str =
    include_str!("../../lucina-agents/tests/fixtures/lane/status-running.json");

/// What `lane status <id>` prints: the sample with `id` and `state` set and the fields of
/// `patch` (an object) laid over it.
fn lane_status(id: &str, state: &str, patch: Value) -> String {
    let mut v: Value = serde_json::from_str(STATUS_SAMPLE).unwrap();
    v["id"] = json!(id);
    v["state"] = json!(state);
    v["workspace"] = json!("/w");
    for (k, val) in patch.as_object().unwrap() {
        v[k.as_str()] = val.clone();
    }
    serde_json::to_string_pretty(&v).unwrap()
}

/// One attempt, with the heads `lane` records per attempt.
fn attempt(before: Option<&str>, after: Option<&str>) -> Value {
    json!({
        "lane": "qoder", "outcome": "ok", "reason": null, "model_used": null, "duration_s": 1.0,
        "before_head": before, "after_head": after, "changed_files": [], "looks": []
    })
}

const FAKE_LANE: &str = r#"#!/bin/sh
D="$(dirname "$0")/data"
cmd="$1"
shift
# A `lane status` that never answers, for the poller's own timeout. exec leaves no child behind.
if [ "$cmd" = status ] && [ -f "$D/$1.hang" ]; then exec sleep 30; fi
# A `lane status` that counts its own calls: exit 2, but the third call prints garbage and
# exits 0. The number in the file is how many calls the run has had.
if [ "$cmd" = status ] && [ -f "$D/$1.seq" ]; then
  n=$(cat "$D/$1.seq")
  n=$((n + 1))
  echo "$n" > "$D/$1.seq"
  if [ "$n" -eq 3 ]; then echo "this is not json"; exit 0; fi
  exit 2
fi
case "$cmd" in
run)
  # A `lane run` that takes its time to detach, so a test can stop the daemon while it is up.
  if [ -f "$D/run-slow" ]; then sleep 5; fi
  id=""
  prev=""
  for a in "$@"; do
    if [ "$prev" = "--id" ]; then id="$a"; fi
    prev="$a"
  done
  if [ -f "$D/run-fail" ]; then
    echo "lane: out of cheese" >&2
    exit 1
  fi
  if [ -f "$D/run-fail-registered" ]; then
    cp "$D/registered.status" "$D/$id.status"
    echo "lane: late failure" >&2
    exit 1
  fi
  printf '%s\n' "$@" > "$D/$id.args"
  cat > "$D/$id.stdin"
  # Like the real tool, leave a detached runner behind: it records its PID and sleeps until
  # the stop file of the run appears. It gets its own session, so a signal to the daemon's
  # process group does not reach it.
  setsid sh -c 'echo $$ > "$1"; while [ ! -f "$2" ]; do sleep 0.2; done' sh "$D/$id.runner" "$D/$id.stop" </dev/null >/dev/null 2>&1 &
  n=0
  while [ ! -s "$D/$id.runner" ] && [ "$n" -lt 100 ]; do
    sleep 0.1
    n=$((n + 1))
  done
  echo "$id"
  ;;
status)
  if [ -f "$D/$1.status" ]; then
    lost=""
    if [ -f "$D/$1.runner" ]; then
      read -r pid < "$D/$1.runner"
      lost=false
      if [ -n "$pid" ]; then
        if ! kill -0 "$pid" 2>/dev/null; then
          lost=true
        else
          read -r _ _ st _ < "/proc/$pid/stat" 2>/dev/null
          if [ "$st" = Z ]; then lost=true; fi
        fi
      fi
    fi
    if [ -z "$lost" ]; then
      cat "$D/$1.status"
    else
      first=1
      while IFS= read -r line || [ -n "$line" ]; do
        # The real status carries the field already: ours replaces it.
        case "$line" in *'"runner_lost"'*) continue ;; esac
        printf '%s\n' "$line"
        if [ "$first" = 1 ]; then
          printf '  "runner_lost": %s,\n' "$lost"
          first=0
        fi
      done < "$D/$1.status"
    fi
  else
    exit 2
  fi
  ;;
ls)
  if [ -f "$D/ls-fail" ]; then exit 1; fi
  if [ -f "$D/ls.jsonl" ]; then cat "$D/ls.jsonl"; fi
  ;;
*)
  exit 64
  ;;
esac
"#;

const CONFIG: &str = r#"
[lanes.qoder]
kind = "command"
[lanes.codex-a]
kind = "codex"
[categories]
deep = ["qoder"]
deep-codex = ["qoder", "codex-a"]
"#;

/// Everything one test needs, under one temporary directory that goes away on drop.
struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("lane/data")).unwrap();
        common::write_exec(&root.path().join("lane/lane"), FAKE_LANE);
        std::fs::write(root.path().join("chains.toml"), CONFIG).unwrap();
        std::fs::write(root.path().join("spec.md"), "Write the report.\n").unwrap();
        Fixture { root }
    }

    fn data(&self) -> PathBuf {
        self.root.path().join("lane/data")
    }

    fn spec(&self) -> String {
        self.root.path().join("spec.md").display().to_string()
    }

    /// A git repository to run in.
    fn repo(&self, name: &str) -> String {
        let dir = self.root.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join("README"), "repo\n").unwrap();
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
        dir.display().to_string()
    }

    fn env(&self, extra: &[(&'static str, String)]) -> Vec<(&'static str, String)> {
        let mut env = vec![
            (
                "LUCINA_AGENTD_LANE",
                self.root.path().join("lane/lane").display().to_string(),
            ),
            ("LUCINA_AGENTD_WORK_POLL_SECS", "1".to_string()),
            // The stand-in `lane` is started directly; the scope tests turn it on.
            ("LUCINA_AGENTD_WORK_SCOPE", "off".to_string()),
            (
                "LANE_CONFIG",
                self.root.path().join("chains.toml").display().to_string(),
            ),
        ];
        env.extend(extra.iter().cloned());
        env
    }

    fn daemon(&self, extra: &[(&'static str, String)]) -> Daemon {
        Daemon::start_with_env(&self.env(extra))
    }

    /// Moves a run on: what `lane status <id>` prints from now on.
    fn status(&self, id: &str, json: &str) {
        let tmp = self.data().join(format!("{id}.status.new"));
        std::fs::write(&tmp, json).unwrap();
        std::fs::rename(tmp, self.data().join(format!("{id}.status"))).unwrap();
    }
}

/// Stops the runners the stand-in `lane` started: writes the stop file of every recorded
/// runner, then signals exactly the PIDs in this fixture's own `*.runner` files (only while
/// that process still has its stop file on its command line). Runs on panic too.
impl Drop for Fixture {
    fn drop(&mut self) {
        let Ok(entries) = std::fs::read_dir(self.data()) else {
            return;
        };
        let mut pids = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "runner") {
                continue;
            }
            let stop = path.with_extension("stop");
            let _ = std::fs::write(&stop, "");
            let Some(pid) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| t.trim().parse::<u32>().ok())
            else {
                continue;
            };
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            if String::from_utf8_lossy(&cmdline).contains(&stop.display().to_string()) {
                pids.push(pid);
            }
        }
        for pid in pids {
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
    }
}

/// A directory with only what the stand-ins need, as the daemon's `PATH`; with a stand-in
/// `systemd-run` when asked. The stand-in records its arguments in `systemd-run.args`, then
/// runs the command after `--`.
fn tool_dir(f: &Fixture, with_systemd_run: bool) -> String {
    let bin = f.root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in ["dirname", "cat", "setsid", "sleep"] {
        let real = ["/usr/bin", "/bin"]
            .iter()
            .map(|d| Path::new(d).join(tool))
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("no {tool} on this host"));
        std::os::unix::fs::symlink(real, bin.join(tool)).unwrap();
    }
    if with_systemd_run {
        common::write_exec(
            &bin.join("systemd-run"),
            r#"#!/bin/sh
D="${0%/*}"
printf '%s\n' "$@" > "$D/systemd-run.args"
while [ "$1" != "--" ]; do shift; done
shift
exec "$@"
"#,
        );
    }
    bin.display().to_string()
}

fn lagent(d: &Daemon, args: &[&str]) -> Run {
    common::run(
        Command::new(common::LAGENT)
            .args(args)
            .env("LUCINA_AGENTD_SOCKET", d.socket())
            .env_remove("LAGENT_SESSION")
            .env_remove("CLAUDE_CODE_HOST_SESSION_ID")
            .env_remove("CLAUDE_CODE_SESSION_ID"),
    )
}

fn lagent_json(d: &Daemon, args: &[&str]) -> Value {
    let mut a = args.to_vec();
    a.push("--json");
    let r = lagent(d, &a);
    assert_eq!(r.code, 0, "lagent {args:?} failed: {}", r.stderr);
    serde_json::from_str(&r.stdout).unwrap_or_else(|e| panic!("lagent {args:?}: {e}: {}", r.stdout))
}

/// Like [`lagent_json`] for a command that must exit with `code`.
fn lagent_json_code(d: &Daemon, args: &[&str], code: i32) -> Value {
    let mut a = args.to_vec();
    a.push("--json");
    let r = lagent(d, &a);
    assert_eq!(r.code, code, "lagent {args:?}: {}", r.stderr);
    serde_json::from_str(&r.stdout).unwrap_or_else(|e| panic!("lagent {args:?}: {e}: {}", r.stdout))
}

/// Registers the lead (an already registered one is fine) and opens a change of it; the
/// change's id. A work start needs both.
fn change_for(d: &Daemon, lead: &str) -> String {
    let _ = lagent(d, &["id", "lead", "add", lead, "a test lead"]);
    let row = lagent_json(
        d,
        &["id", "new", "change", "--lead", lead, "--title", "a change"],
    );
    row["id"].as_str().unwrap().to_string()
}

fn start(d: &Daemon, f: &Fixture, lead: &str, ws: &str, extra: &[&str]) -> Value {
    let spec = f.spec();
    let change = change_for(d, lead);
    let mut args = vec![
        "work",
        "start",
        "--lead",
        lead,
        "--change",
        &change,
        "--lane",
        "qoder",
        "--workspace",
        ws,
        "--spec",
        &spec,
    ];
    args.extend_from_slice(extra);
    lagent_json(d, &args)
}

fn show(d: &Daemon, id: &str) -> Value {
    lagent_json(d, &["work", "show", id])
}

/// Waits for `ready(job)`; panics with the last job after 20 seconds.
fn wait_for(d: &Daemon, id: &str, what: &str, ready: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let job = show(d, id);
        if ready(&job) {
            return job;
        }
        assert!(Instant::now() < deadline, "{what}: still {job}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn wait_state(d: &Daemon, id: &str, state: &str) -> Value {
    wait_for(d, id, &format!("waiting for {state}"), |j| {
        j["state"] == state
    })
}

/// [`wait_state`] with a budget of its own. A run becomes `lost` only after three misses, so
/// three poll intervals are the least it takes, and a loaded host spends those slowly: the 20
/// seconds of `wait_for` are not enough for it.
fn wait_state_within(d: &Daemon, id: &str, state: &str, limit: Duration) -> Value {
    let deadline = Instant::now() + limit;
    loop {
        let job = show(d, id);
        if job["state"] == state {
            return job;
        }
        assert!(
            Instant::now() < deadline,
            "waiting for {state}: still {job}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// How long a run is given to be found `lost`.
fn lost_budget() -> Duration {
    Duration::from_secs(90)
}

fn lead_of<'a>(list: &'a Value, lead: &str) -> Option<&'a Value> {
    list["leads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["lead"] == lead)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn start_passes_the_spec_and_lists_the_run_under_its_lead() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let job = start(
        &d,
        &f,
        "LA",
        &ws,
        &["--cost", "build", "--verify", "true", "--title", "report"],
    );
    let id = job["id"].as_str().unwrap().to_string();
    // The job id is the lane run id: the run id and a slug of the title.
    assert_eq!(id, "la-r001-report", "{job}");
    assert_eq!(job["state"], "starting");
    assert_eq!(job["lead"], "LA");
    assert_eq!(job["priority"], "normal");
    assert_eq!(job["cost"], "build");
    assert!(
        job["baseHead"].as_str().is_some_and(|h| h.len() == 40),
        "{job}"
    );

    let args: Vec<String> = read(&f.data().join(format!("{id}.args")))
        .lines()
        .map(str::to_string)
        .collect();
    let has = |flag: &str, value: &str| args.windows(2).any(|w| w[0] == flag && w[1] == value);
    assert!(has("--id", &id), "{args:?}");
    assert!(has("--lane", "qoder"), "{args:?}");
    assert!(has("--workspace", &ws), "{args:?}");
    assert!(has("--verify", "true"), "{args:?}");
    assert!(has("--owner", "LA"), "{args:?}");
    assert!(has("--cost", "build"), "{args:?}");
    assert!(!args.iter().any(|a| a == "--expect-file"), "{args:?}");
    assert_eq!(
        read(&f.data().join(format!("{id}.stdin"))),
        "Write the report.\n"
    );
    assert_eq!(
        read(Path::new(job["specPath"].as_str().unwrap())),
        "Write the report.\n"
    );

    f.status(
        &id,
        &lane_status(&id, "running", json!({"final_lane": "qoder", "model": "m"})),
    );
    wait_state(&d, &id, "running");
    let list = lagent_json(&d, &["work", "ls"]);
    let lead = lead_of(&list, "LA").unwrap_or_else(|| panic!("{list}"));
    assert_eq!(lead["running"], 1);
    assert_eq!(lead["jobs"][0]["id"], id.as_str());
    assert_eq!(lead["jobs"][0]["finalLane"], "qoder");
    let table = lagent(&d, &["work", "ls"]);
    assert_eq!(table.code, 0, "{}", table.stderr);
    assert!(
        table
            .stdout
            .contains("LA: 1 running, 0 queued, 0 ended, 0 empty"),
        "{}",
        table.stdout
    );
    assert!(table.stdout.contains(&id), "{}", table.stdout);
}

#[test]
fn finished_runs_get_an_end_time_and_an_empty_flag() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");

    let a = start(&d, &f, "LA", &ws, &["--cost", "build", "--verify", "true"]);
    let a = a["id"].as_str().unwrap().to_string();
    f.status(
        &a,
        &lane_status(
            &a,
            "verified",
            json!({
                "verify": {"cmd": "true", "ran_cmd": null, "exit_code": 0, "head": "deadbeef",
                           "ran": true, "timed_out": false, "note": null},
                "attempts": [attempt(None, None)],
                "dirty_files": []
            }),
        ),
    );
    let done = wait_state(&d, &a, "verified");
    assert_eq!(done["empty"], false, "{done}");
    assert_eq!(done["verifyExit"], 0);
    assert!(done["endedAt"].is_string(), "{done}");
    // Waiting on a finished run returns at once; 0 is a good end.
    let w = lagent(&d, &["work", "wait", &a, "--timeout", "5"]);
    assert_eq!(w.code, 0, "{}", w.stderr);

    let b = start(&d, &f, "LA", &ws, &["--expect", "out/report.md"]);
    let b = b["id"].as_str().unwrap().to_string();
    f.status(&b, &lane_status(&b, "reported_done", json!({})));
    let done = wait_state(&d, &b, "done");
    assert_eq!(done["empty"], true, "{done}");
    assert!(done["endedAt"].is_string());

    let c = start(&d, &f, "LA", &ws, &["--expect", "out/report.md"]);
    let c = c["id"].as_str().unwrap().to_string();
    std::fs::create_dir_all(Path::new(&ws).join("out")).unwrap();
    std::fs::write(Path::new(&ws).join("out/report.md"), "# report\n").unwrap();
    f.status(&c, &lane_status(&c, "reported_done", json!({})));
    let done = wait_state(&d, &c, "done");
    assert_eq!(done["empty"], false, "{done}");

    let list = lagent_json(&d, &["work", "ls"]);
    let lead = lead_of(&list, "LA").unwrap();
    assert_eq!(
        (lead["ended"].as_u64(), lead["empty"].as_u64()),
        (Some(3), Some(1)),
        "{lead}"
    );
}

#[test]
fn an_expected_file_is_handed_to_lane_as_expect_file() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let job = start(
        &d,
        &f,
        "LA",
        &ws,
        &["--expect", "out/report.md", "--verify", "true"],
    );
    let id = job["id"].as_str().unwrap();
    let args: Vec<String> = read(&f.data().join(format!("{id}.args")))
        .lines()
        .map(str::to_string)
        .collect();
    let at = |flag: &str| args.iter().position(|a| a == flag).unwrap();
    assert_eq!(args[at("--expect-file") + 1], "out/report.md", "{args:?}");
    // After --workspace <ws>, before --verify.
    assert!(at("--workspace") < at("--expect-file"), "{args:?}");
    assert!(at("--expect-file") < at("--verify"), "{args:?}");
}

#[test]
fn an_expect_reached_through_a_symlink_is_not_the_file_the_run_left_behind() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let out = Path::new(&ws).join("out");
    // The control: the expected file is where the job said it is.
    std::fs::create_dir(&out).unwrap();
    std::fs::write(out.join("report.md"), "here\n").unwrap();
    let good = start(
        &d,
        &f,
        "LA",
        &ws,
        &["--cost", "build", "--expect", "out/report.md"],
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(&good, &lane_status(&good, "verified", json!({})));
    let job = wait_state(&d, &good, "verified");
    assert_eq!(job["empty"], false, "the file is there: {job}");

    // The same job with `out` a link to a directory outside the workspace, where a file of
    // that name is not empty either: what the worker left is not in the workspace.
    std::fs::remove_dir_all(&out).unwrap();
    let outside = f.root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("report.md"), "elsewhere\n").unwrap();
    std::os::unix::fs::symlink(&outside, &out).unwrap();
    let bad = start(
        &d,
        &f,
        "LA",
        &ws,
        &["--cost", "build", "--expect", "out/report.md"],
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(&bad, &lane_status(&bad, "verified", json!({})));
    let job = wait_state(&d, &bad, "verified");
    assert_eq!(job["empty"], true, "a link is not the file: {job}");
}

/// `./report.md` names the file the run left in the workspace: a leading `.` is the directory
/// the walk starts in, not a step out of it.
#[test]
fn an_expect_named_with_a_leading_dot_is_the_file_in_the_workspace() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    std::fs::write(Path::new(&ws).join("report.md"), "# report\n").unwrap();
    let id = start(
        &d,
        &f,
        "LA",
        &ws,
        &["--cost", "build", "--expect", "./report.md"],
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(&id, &lane_status(&id, "verified", json!({})));
    let job = wait_state(&d, &id, "verified");
    assert_eq!(job["empty"], false, "the file is there, dot or not: {job}");

    // The workspace itself, on the other hand, is not a file a run left behind.
    let id = start(&d, &f, "LA", &ws, &["--cost", "build", "--expect", "."])["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(&id, &lane_status(&id, "verified", json!({})));
    let job = wait_state(&d, &id, "verified");
    assert_eq!(job["empty"], true, "a directory is not the file: {job}");
}

/// The job must be on disk before `lane run` begins.
#[test]
fn a_job_that_cannot_be_saved_does_not_start_the_run() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    // What the job file's own temp name should be is a directory: the write cannot land.
    let work = d.state().join("work");
    std::fs::create_dir_all(&work).unwrap();
    let tmp = format!("jobs.json.tmp-{}", d.guard.pid());
    std::fs::create_dir(work.join(&tmp)).unwrap();
    let change = change_for(&d, "LA");
    let spec = f.spec();
    let r = lagent(
        &d,
        &[
            "work",
            "start",
            "--lead",
            "LA",
            "--change",
            &change,
            "--lane",
            "qoder",
            "--workspace",
            &ws,
            "--spec",
            &spec,
            "--cost",
            "build",
        ],
    );
    assert_eq!(r.code, 1, "stdout {} | stderr {}", r.stdout, r.stderr);
    assert!(
        r.stderr.contains("cannot write the job file"),
        "{}",
        r.stderr
    );
    // `lane run` was not called: no run left its arguments behind.
    assert!(
        !std::fs::read_dir(f.data())
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".args")),
        "the run started although its job is not saved"
    );
    // The spec directory the start made is gone with it.
    let left: Vec<String> = std::fs::read_dir(&work)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n != &tmp && n != "jobs.json")
        .collect();
    assert!(left.is_empty(), "the failed start left {left:?}");
}

#[test]
fn a_light_job_runs_with_cost_light_and_needs_an_expected_file() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let job = start(
        &d,
        &f,
        "LA",
        &ws,
        &["--cost", "light", "--expect", "out/r.md"],
    );
    let id = job["id"].as_str().unwrap();
    let args: Vec<String> = read(&f.data().join(format!("{id}.args")))
        .lines()
        .map(str::to_string)
        .collect();
    let pos = |flag: &str| args.iter().position(|a| a == flag).unwrap();
    assert_eq!(args[pos("--cost") + 1], "light", "{args:?}");
    assert_eq!(args[pos("--owner") + 1], "LA", "{args:?}");
    // run --id <id> --lane L --workspace W --owner O --cost C --expect-file E
    assert!(pos("--workspace") < pos("--owner"), "{args:?}");
    assert!(pos("--owner") < pos("--cost"), "{args:?}");
    assert!(pos("--cost") < pos("--expect-file"), "{args:?}");

    let spec = f.spec();
    let change = change_for(&d, "LA");
    let r = lagent(
        &d,
        &[
            "work",
            "start",
            "--lead",
            "LA",
            "--change",
            &change,
            "--lane",
            "qoder",
            "--workspace",
            &ws,
            "--spec",
            &spec,
            "--cost",
            "light",
        ],
    );
    assert_ne!(r.code, 0);
    assert!(
        r.stderr.contains(
            "a light job needs --expect <file>: a job that reads or reviews must name the file it leaves behind"
        ),
        "{}",
        r.stderr
    );
}

/// No `--cost` is a light run, the way a plain `lane run` has been since lane LB-V010.
#[test]
fn a_job_started_without_a_cost_is_light() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let job = start(&d, &f, "LA", &ws, &["--expect", "out/n.md"]);
    let id = job["id"].as_str().unwrap();
    assert_eq!(job["cost"], "light", "{job}");
    let args: Vec<String> = read(&f.data().join(format!("{id}.args")))
        .lines()
        .map(str::to_string)
        .collect();
    let at = |flag: &str| args.iter().position(|a| a == flag).unwrap();
    assert_eq!(args[at("--cost") + 1], "light", "{args:?}");

    // Light is the default, and a light job must name the file it leaves behind.
    let spec = f.spec();
    let change = change_for(&d, "LA");
    let r = lagent(
        &d,
        &[
            "work",
            "start",
            "--lead",
            "LA",
            "--change",
            &change,
            "--lane",
            "qoder",
            "--workspace",
            &ws,
            "--spec",
            &spec,
        ],
    );
    assert_ne!(r.code, 0);
    assert!(
        r.stderr.contains(
            "a light job needs --expect <file>: a job that reads or reviews must name the file it leaves behind"
        ),
        "{}",
        r.stderr
    );
}

#[test]
fn an_ended_empty_run_ends_in_that_state_and_waits_with_exit_8() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut v: Value = serde_json::from_str(include_str!(
        "../../lucina-agents/tests/fixtures/lane/status-reported_done.json"
    ))
    .unwrap();
    v["id"] = json!(id);
    v["state"] = json!("ended_empty");
    f.status(&id, &serde_json::to_string_pretty(&v).unwrap());
    let job = wait_state(&d, &id, "ended_empty");
    assert_eq!(job["empty"], true, "{job}");
    assert!(job["endedAt"].is_string(), "{job}");
    assert_eq!(lagent(&d, &["work", "wait", &id, "--timeout", "5"]).code, 8);
}

#[test]
fn refusals() {
    let f = Fixture::new();
    // A Lucina project: a directory under <WT_PROJECTS>/Lucina/.
    let projects = f.root.path().join("projects");
    std::fs::create_dir_all(projects.join("Lucina/Repo")).unwrap();
    let lucina_ws = projects.join("Lucina/Repo").display().to_string();
    let d = f.daemon(&[("WT_PROJECTS", projects.display().to_string())]);
    let ws = f.repo("ws");
    let spec = f.spec();
    let change = change_for(&d, "LA");

    let no_lead = lagent(
        &d,
        &[
            "work",
            "start",
            "--lane",
            "qoder",
            "--workspace",
            &ws,
            "--spec",
            &spec,
        ],
    );
    assert_ne!(no_lead.code, 0);
    assert!(no_lead.stderr.contains("--lead"), "{}", no_lead.stderr);

    let both = lagent(
        &d,
        &[
            "work",
            "start",
            "--lead",
            "LA",
            "--change",
            &change,
            "--lane",
            "qoder",
            "--chain",
            "deep",
            "--workspace",
            &ws,
            "--spec",
            &spec,
        ],
    );
    assert_ne!(both.code, 0);
    let neither = lagent(
        &d,
        &[
            "work",
            "start",
            "--lead",
            "LA",
            "--change",
            &change,
            "--workspace",
            &ws,
            "--spec",
            &spec,
        ],
    );
    assert_ne!(neither.code, 0);

    let unknown = lagent(
        &d,
        &[
            "work",
            "start",
            "--lead",
            "LA",
            "--change",
            &change,
            "--cost",
            "build",
            "--lane",
            "nope",
            "--workspace",
            &ws,
            "--spec",
            &spec,
        ],
    );
    assert_ne!(unknown.code, 0);
    assert!(unknown.stderr.contains("nope"), "{}", unknown.stderr);
    let unknown = lagent(
        &d,
        &[
            "work",
            "start",
            "--lead",
            "LA",
            "--change",
            &change,
            "--chain",
            "nope",
            "--workspace",
            &ws,
            "--spec",
            &spec,
        ],
    );
    assert_ne!(unknown.code, 0);

    let missing = lagent(
        &d,
        &[
            "work",
            "start",
            "--lead",
            "LA",
            "--change",
            &change,
            "--lane",
            "qoder",
            "--workspace",
            "/no/such/dir",
            "--spec",
            &spec,
        ],
    );
    assert_ne!(missing.code, 0);
    let escape = lagent(
        &d,
        &[
            "work",
            "start",
            "--lead",
            "LA",
            "--change",
            &change,
            "--lane",
            "qoder",
            "--workspace",
            &ws,
            "--expect",
            "../x",
            "--spec",
            &spec,
        ],
    );
    assert_ne!(escape.code, 0);

    for target in [["--lane", "codex-a"], ["--chain", "deep-codex"]] {
        let r = lagent(
            &d,
            &[
                "work",
                "start",
                "--cost",
                "build",
                "--lead",
                "LA",
                "--change",
                &change,
                target[0],
                target[1],
                "--workspace",
                &lucina_ws,
                "--spec",
                &spec,
            ],
        );
        assert_ne!(r.code, 0, "{target:?}");
        assert!(
            r.stderr
                .contains("no Codex on Lucina work: use the -lucina chain"),
            "{target:?}: {}",
            r.stderr
        );
    }
    // The same lane is fine outside a Lucina project.
    let ok = lagent_json(
        &d,
        &[
            "work",
            "start",
            "--lead",
            "LA",
            "--change",
            &change,
            "--cost",
            "build",
            "--lane",
            "codex-a",
            "--workspace",
            &ws,
            "--spec",
            &spec,
        ],
    );
    assert!(ok["id"].is_string(), "{ok}");

    // Nothing the refusals asked for reached the stand-in lane: only the one run above is there.
    let runs = std::fs::read_dir(f.data())
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".args")
        })
        .count();
    assert_eq!(runs, 1);

    let off = f.daemon(&[("LUCINA_AGENTD_LANE", "off".to_string())]);
    let r = lagent(
        &off,
        &[
            "work",
            "start",
            "--lead",
            "LA",
            "--change",
            &change,
            "--lane",
            "qoder",
            "--workspace",
            &ws,
            "--spec",
            &spec,
        ],
    );
    assert_ne!(r.code, 0);
    assert!(r.stderr.contains("work jobs are off"), "{}", r.stderr);
}

#[test]
fn a_restarted_daemon_keeps_following_its_runs() {
    // This cannot show the control-group kill that the scope prevents: that is shown live.
    let f = Fixture::new();
    let mut d = f.daemon(&[]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(&id, &lane_status(&id, "running", json!({})));
    wait_state(&d, &id, "running");
    let runner_file = f.data().join(format!("{id}.runner"));
    let runner_before = read(&runner_file).trim().to_string();
    assert!(
        Path::new(&format!("/proc/{runner_before}")).exists(),
        "the runner is alive before the restart"
    );

    assert_eq!(
        d.guard.terminate(Duration::from_secs(5)),
        Some(0),
        "clean exit on SIGTERM"
    );
    d.guard = common::spawn_daemon_env(d.dir.path(), &f.env(&[]));
    let job = show(&d, &id);
    assert_eq!(job["lead"], "LA", "{job}");
    assert_ne!(job["runnerLost"], true, "{job}");
    assert_eq!(read(&runner_file).trim(), runner_before);
    assert!(
        Path::new(&format!("/proc/{runner_before}")).exists(),
        "the same runner is still alive after the restart"
    );

    f.status(
        &id,
        &lane_status(&id, "failed", json!({"note": "nothing left"})),
    );
    let job = wait_state(&d, &id, "failed");
    assert!(job["endedAt"].is_string());
    assert_eq!(lagent(&d, &["work", "wait", &id, "--timeout", "5"]).code, 1);
}

/// `lane run` is detached from the daemon's process group and its handle does not kill it: a
/// stop between saving the job and `lane` registering the run leaves the run to itself.
#[test]
fn a_run_that_is_still_starting_outlives_the_daemon_that_started_it() {
    let f = Fixture::new();
    let mut d = f.daemon(&[]);
    // The stand-in `lane` sleeps five seconds before it registers the run: the stop below
    // lands inside that window.
    std::fs::write(f.data().join("run-slow"), "").unwrap();
    let ws = f.repo("ws");
    let spec = f.spec();
    let change = change_for(&d, "LA");
    // The start waits for `lane run` to finish, so it goes on in a thread of its own while the
    // daemon is stopped.
    let socket = d.socket();
    let started = std::thread::spawn(move || {
        common::run(
            Command::new(common::LAGENT)
                .args([
                    "work",
                    "start",
                    "--lead",
                    "LA",
                    "--change",
                    &change,
                    "--lane",
                    "qoder",
                    "--workspace",
                    &ws,
                    "--spec",
                    &spec,
                    "--cost",
                    "build",
                ])
                .env("LUCINA_AGENTD_SOCKET", socket),
        )
    });
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(
        d.guard.terminate(Duration::from_secs(5)),
        Some(0),
        "clean exit on SIGTERM"
    );
    let jobs: Value =
        serde_json::from_str(&std::fs::read_to_string(d.state().join("work/jobs.json")).unwrap())
            .unwrap();
    let id = jobs[0]["id"].as_str().unwrap().to_string();
    assert!(
        !f.data().join(format!("{id}.args")).exists(),
        "the stop came after lane had registered the run"
    );
    // The job is on disk, and the run it started carries on by itself: `lane` registers it and
    // leaves its runner behind a few seconds later.
    let runner = f.data().join(format!("{id}.runner"));
    let deadline = Instant::now() + Duration::from_secs(12);
    while !runner.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(runner.exists(), "lane run did not outlive the daemon");
    assert!(
        f.data().join(format!("{id}.args")).exists(),
        "the run was registered without its arguments"
    );
    let _ = started.join();
    // The daemon that starts again follows the run like any other.
    f.status(&id, &lane_status(&id, "running", json!({})));
    d.guard = common::spawn_daemon_env(d.dir.path(), &f.env(&[]));
    let job = wait_state(&d, &id, "running");
    assert_eq!(job["id"], id, "{job}");
}

#[test]
fn a_runner_that_is_gone_shows_as_lost_and_the_state_stays() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(&id, &lane_status(&id, "running", json!({})));
    let job = wait_state(&d, &id, "running");
    assert_ne!(job["runnerLost"], true, "{job}");

    // Stop the runner by the PID it recorded.
    let pid = read(&f.data().join(format!("{id}.runner")))
        .trim()
        .to_string();
    assert!(Command::new("kill").arg(&pid).status().unwrap().success());
    let job = wait_for(&d, &id, "waiting for the lost runner", |j| {
        j["runnerLost"] == true
    });
    assert_eq!(job["state"], "running", "{job}");

    let list = lagent_json(&d, &["work", "ls"]);
    assert_eq!(lead_of(&list, "LA").unwrap()["lost"], 1, "{list}");
    let table = lagent(&d, &["work", "ls"]);
    assert_eq!(table.code, 0, "{}", table.stderr);
    assert!(
        table
            .stdout
            .contains("LA: 1 running, 0 queued, 0 ended, 0 empty, 1 lost"),
        "{}",
        table.stdout
    );
    assert!(table.stdout.contains("running(lost)"), "{}", table.stdout);
    let shown = lagent(&d, &["work", "show", &id]);
    assert_eq!(shown.code, 0, "{}", shown.stderr);
    assert!(
        shown
            .stdout
            .lines()
            .any(|l| l == "runner lost: yes (resume or dismiss it with lane)"),
        "{}",
        shown.stdout
    );
}

#[test]
fn scope_on_without_systemd_run_refuses_the_start() {
    let f = Fixture::new();
    let bin = tool_dir(&f, false);
    let d = f.daemon(&[
        ("LUCINA_AGENTD_WORK_SCOPE", "on".to_string()),
        ("XDG_RUNTIME_DIR", f.root.path().display().to_string()),
        ("PATH", bin),
    ]);
    let ws = f.repo("ws");
    let spec = f.spec();
    let change = change_for(&d, "LA");
    let r = lagent(
        &d,
        &[
            "work",
            "start",
            "--lead",
            "LA",
            "--change",
            &change,
            "--lane",
            "qoder",
            "--workspace",
            &ws,
            "--spec",
            &spec,
        ],
    );
    assert_ne!(r.code, 0, "{}", r.stdout);
    assert!(r.stderr.contains("systemd-run"), "{}", r.stderr);
    // The stand-in `lane` was never called, and no job was stored.
    let called: Vec<_> = std::fs::read_dir(f.data())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(called.is_empty(), "{called:?}");
    let list = lagent_json(&d, &["work", "ls"]);
    assert!(list["jobs"].as_array().unwrap().is_empty(), "{list}");
}

#[test]
fn scope_on_starts_lane_through_systemd_run() {
    let f = Fixture::new();
    let bin = tool_dir(&f, true);
    let d = f.daemon(&[
        ("LUCINA_AGENTD_WORK_SCOPE", "on".to_string()),
        ("XDG_RUNTIME_DIR", f.root.path().display().to_string()),
        ("PATH", bin.clone()),
    ]);
    let ws = f.repo("ws");
    let job = start(&d, &f, "LA", &ws, &["--cost", "build"]);
    let id = job["id"].as_str().unwrap().to_string();
    let unit = format!("lucina-work-{id}");
    assert_eq!(job["scope"], unit.as_str(), "{job}");
    assert_ne!(job["state"], "failed", "{job}");
    assert_eq!(show(&d, &id)["scope"], unit.as_str());

    let recorded: Vec<String> = read(&Path::new(&bin).join("systemd-run.args"))
        .lines()
        .map(str::to_string)
        .collect();
    let lane = f.root.path().join("lane/lane").display().to_string();
    let head = [
        "--user",
        "--scope",
        "--quiet",
        "--collect",
        "--unit",
        &unit,
        "--slice=build.slice",
        "-p",
        "CPUQuota=400%",
        "-p",
        "MemoryMax=12G",
        "--",
        &lane,
        "run",
        "--id",
        &id,
    ];
    assert_eq!(recorded[..head.len()], head, "{recorded:?}");
    // The command after `--` ran with the spec on stdin.
    assert_eq!(
        read(&f.data().join(format!("{id}.stdin"))),
        "Write the report.\n"
    );
    let shown = lagent(&d, &["work", "show", &id]);
    assert!(shown.stdout.contains(&unit), "{}", shown.stdout);
}

#[test]
fn runs_started_with_lane_directly_are_listed_as_direct() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let mine = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let old = (chrono::Utc::now() - chrono::Duration::hours(48)).to_rfc3339();
    std::fs::write(
        f.data().join("ls.jsonl"),
        format!(
            r#"{{"id":"zz-direct","state":"running","owner":"chat-x","category":"deep","final_lane":"qoder","queue_position":null,"workspace":"/w/direct","updated":"{now}"}}
{{"id":"{mine}","state":"running","owner":"chat-a","category":"deep","final_lane":"qoder","queue_position":null,"workspace":"{ws}","updated":"{now}"}}
{{"id":"zz-old","state":"verified","owner":"chat-x","category":"deep","final_lane":"qoder","queue_position":null,"workspace":"/w/old","updated":"{old}"}}
"#
        ),
    )
    .unwrap();

    let list = lagent_json(&d, &["work", "ls"]);
    let direct = lead_of(&list, "direct:chat-x").unwrap_or_else(|| panic!("{list}"));
    assert_eq!(
        direct["jobs"].as_array().unwrap().len(),
        1,
        "the old finished run stays hidden: {direct}"
    );
    assert_eq!(direct["jobs"][0]["id"], "zz-direct");
    assert_eq!(direct["jobs"][0]["direct"], true);
    // The run agentd started shows once, under its lead.
    assert_eq!(
        list["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|j| j["id"] == mine.as_str())
            .count(),
        1,
        "{list}"
    );
    assert!(lead_of(&list, "LA").is_some());
    assert_eq!(list["warning"], Value::Null);

    let all = lagent_json(&d, &["work", "ls", "--old"]);
    assert!(
        all["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["id"] == "zz-old"),
        "{all}"
    );

    let none = lagent_json(&d, &["work", "ls", "--no-direct"]);
    assert!(lead_of(&none, "direct:chat-x").is_none(), "{none}");
    assert!(lead_of(&none, "LA").is_some());
}

#[test]
fn a_failing_lane_ls_is_a_warning_not_an_error() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    std::fs::write(f.data().join("ls-fail"), "").unwrap();
    let r = lagent(&d, &["work", "ls", "--json"]);
    // A warning ends the listing with exit code 2 (it used to be 0).
    assert_eq!(r.code, 2, "{}", r.stderr);
    let v: Value = serde_json::from_str(&r.stdout).unwrap();
    assert!(v["warning"].is_string(), "{v}");
    assert!(r.stderr.contains("warning"), "{}", r.stderr);
}

#[test]
fn a_listing_with_one_broken_line_lists_the_good_rows() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let now = chrono::Utc::now().to_rfc3339();
    let row = |id: &str| {
        format!(
            r#"{{"id":"{id}","state":"running","owner":"chat-x","category":"deep","final_lane":"qoder","queue_position":null,"workspace":"/w","updated":"{now}"}}"#
        )
    };
    std::fs::write(
        f.data().join("ls.jsonl"),
        format!("{}\n{{\"id\": broken\n{}\n", row("zz-a"), row("zz-b")),
    )
    .unwrap();
    let list = lagent_json_code(&d, &["work", "ls"], 2);
    assert_eq!(list["jobs"].as_array().unwrap().len(), 2, "{list}");
    assert_eq!(list["unparsed"], 1, "{list}");
    let w = list["warning"].as_str().unwrap();
    assert!(
        w.starts_with("1 of 3 lane rows could not be read: line 2: "),
        "{w}"
    );
    // The good rows alone give no warning and exit 0.
    std::fs::write(f.data().join("ls.jsonl"), format!("{}\n", row("zz-a"))).unwrap();
    let list = lagent_json(&d, &["work", "ls"]);
    assert_eq!(list["unparsed"], 0, "{list}");
    assert_eq!(list["warning"], Value::Null);
}

#[test]
fn a_run_lane_does_not_know_is_lost() {
    let f = Fixture::new();
    let d = f.daemon(&[("LUCINA_AGENTD_WORK_LOST_AFTER_SECS", "1".to_string())]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    // No status file: `lane status` exits 2.
    let job = wait_state_within(&d, &id, "lost", lost_budget());
    assert_eq!(job["note"], "lane has no record of this run");
    assert!(job["endedAt"].is_string());
    assert_eq!(lagent(&d, &["work", "wait", &id, "--timeout", "5"]).code, 9);
}

#[test]
fn a_lane_run_that_fails_marks_the_job_failed_with_its_stderr() {
    let f = Fixture::new();
    common::write_exec(&f.root.path().join("lane/lane"), &counting_stand_in());
    std::fs::write(f.data().join("run-fail"), "").unwrap();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    // A failed start still prints the job, and the exit code says it failed.
    let spec = f.spec();
    let change = change_for(&d, "LA");
    let r = lagent(
        &d,
        &[
            "work",
            "start",
            "--lead",
            "LA",
            "--change",
            &change,
            "--lane",
            "qoder",
            "--workspace",
            &ws,
            "--spec",
            &spec,
            "--cost",
            "build",
            "--json",
        ],
    );
    assert_eq!(r.code, 1, "{}", r.stderr);
    let job: Value =
        serde_json::from_str(&r.stdout).unwrap_or_else(|e| panic!("{e}: {}", r.stdout));
    assert_eq!(job["state"], "failed", "{job}");
    assert!(
        job["note"]
            .as_str()
            .is_some_and(|n| n.contains("out of cheese")),
        "{job}"
    );
    assert!(job["endedAt"].is_string());
    let id = job["id"].as_str().unwrap();
    // It stays failed: no later poll turns it into anything else. A failed run is never asked
    // about, so the proof that a poll pass has gone by is a second job of the same daemon:
    // `run-fail` goes, the second run starts, and its first `lane status` is counted.
    let _ = std::fs::remove_file(f.data().join("run-fail"));
    let second = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(&second, &lane_status(&second, "running", json!({})));
    let calls = f.data().join(format!("{second}.status-calls"));
    wait_for(&d, id, "waiting for a poll pass", |_| {
        std::fs::metadata(&calls).is_ok_and(|m| m.len() > 0)
    });
    assert_eq!(show(&d, id)["state"], "failed");
}

#[test]
fn the_api_refuses_a_relative_workspace() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let body = r#"{"lead":"c","lane":"qoder","workspace":"relative/dir","spec":"x"}"#;
    let sock = d.socket().display().to_string();
    let r = common::run(Command::new("curl").args([
        "-s",
        "-X",
        "POST",
        "-H",
        "content-type: application/json",
        "-d",
        body,
        "--unix-socket",
        &sock,
        "http://x/work",
    ]));
    let v: Value = serde_json::from_str(&r.stdout).unwrap_or_else(|e| panic!("{e}: {}", r.stdout));
    assert_eq!(
        v["error"]["message"], "workspace must be an absolute path",
        "{v}"
    );
}

#[test]
fn a_dot_workspace_means_the_callers_directory() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("here");
    let spec = f.spec();
    let change = change_for(&d, "LA");
    let r = common::run(
        Command::new(common::LAGENT)
            .args([
                "work",
                "start",
                "--lead",
                "LA",
                "--change",
                &change,
                "--lane",
                "qoder",
                "--workspace",
                ".",
                "--spec",
                &spec,
                "--cost",
                "build",
                "--json",
            ])
            .current_dir(&ws)
            .env("LUCINA_AGENTD_SOCKET", d.socket()),
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let job: Value = serde_json::from_str(&r.stdout).unwrap();
    assert!(
        job["workspace"]
            .as_str()
            .is_some_and(|w| w.ends_with("/here")),
        "{job}"
    );
}

#[test]
fn a_lane_run_that_fails_after_lane_registered_the_run_is_not_failed() {
    let f = Fixture::new();
    std::fs::write(f.data().join("run-fail-registered"), "").unwrap();
    std::fs::write(
        f.data().join("registered.status"),
        lane_status("wk-x", "running", json!({})),
    )
    .unwrap();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let job = start(&d, &f, "LA", &ws, &["--cost", "build"]);
    assert_eq!(job["state"], "running", "{job}");
    assert!(
        job["note"]
            .as_str()
            .is_some_and(|n| n.contains("late failure")),
        "{job}"
    );
}

#[test]
fn a_lost_run_follows_lane_again_when_it_shows_up() {
    let f = Fixture::new();
    let d = f.daemon(&[("LUCINA_AGENTD_WORK_LOST_AFTER_SECS", "1".to_string())]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_state_within(&d, &id, "lost", lost_budget());
    f.status(&id, &lane_status(&id, "running", json!({})));
    let job = wait_state(&d, &id, "running");
    assert!(job["endedAt"].is_null(), "{job}");
    assert!(job["note"].is_null(), "{job}");
    f.status(
        &id,
        &lane_status(
            &id,
            "verified",
            json!({"verify": {"cmd": "true", "ran_cmd": null, "exit_code": 0, "head": "abc",
                              "ran": true, "timed_out": false, "note": null}}),
        ),
    );
    let job = wait_state(&d, &id, "verified");
    assert!(job["endedAt"].is_string(), "{job}");
}

#[test]
fn a_lane_status_that_cannot_start_is_not_a_miss() {
    let f = Fixture::new();
    let d = f.daemon(&[("LUCINA_AGENTD_WORK_LOST_AFTER_SECS", "1".to_string())]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(&id, &lane_status(&id, "running", json!({})));
    wait_state(&d, &id, "running");
    std::fs::remove_file(f.root.path().join("lane/lane")).unwrap();
    // The daemon says so, and the job is none the worse for it.
    let log = d.dir.path().join("agentd.log");
    let failed_spawns = || read(&log).matches("lane status did not start").count() as u64;
    // Not one line, but three of them: a job this old is lost on three misses in a row, and
    // the assert below says nothing until a host that would lose it has had its three passes.
    wait_for(
        &d,
        &id,
        "waiting for three statuses that cannot start",
        |j| failed_spawns() >= 3 && j["state"] == "running",
    );
    let job = show(&d, &id);
    assert_eq!(job["state"], "running", "{job}");
    assert!(job["note"].is_null(), "{job}");
    // `lane` is back: its next answer moves the run on, as nothing had been counted.
    common::write_exec(&f.root.path().join("lane/lane"), FAKE_LANE);
    f.status(&id, &lane_status(&id, "verified", json!({})));
    let job = wait_state(&d, &id, "verified");
    assert!(job["endedAt"].is_string(), "{job}");
}

#[test]
fn a_lane_status_that_never_answers_is_dropped_and_is_not_a_miss() {
    let f = Fixture::new();
    let d = f.daemon(&[
        ("LUCINA_AGENTD_WORK_LOST_AFTER_SECS", "1".to_string()),
        ("LUCINA_AGENTD_WORK_STATUS_SECS", "2".to_string()),
    ]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(&id, &lane_status(&id, "running", json!({})));
    wait_state(&d, &id, "running");
    // From now on `lane status` hangs: the poller drops it after two seconds.
    std::fs::write(f.data().join(format!("{id}.hang")), "").unwrap();
    let log = d.dir.path().join("agentd.log");
    wait_for(&d, &id, "waiting for a status that was dropped", |_| {
        read(&log).contains("lane status timed out")
    });
    // Three of those in a row would have been lost before; a call that never answers says
    // nothing of the run, so the job keeps waiting for one that does.
    std::thread::sleep(Duration::from_secs(6));
    let job = show(&d, &id);
    assert_eq!(job["state"], "running", "{job}");
    assert!(job["note"].is_null(), "{job}");
    // The hang ends: the answer is applied.
    std::fs::remove_file(f.data().join(format!("{id}.hang"))).unwrap();
    f.status(&id, &lane_status(&id, "reported_done", json!({})));
    let job = wait_state(&d, &id, "done");
    assert!(job["endedAt"].is_string(), "{job}");
}

#[test]
fn a_status_that_is_not_json_changes_nothing_until_it_is_valid() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(&id, "this is not json");
    let job = wait_for(&d, &id, "waiting for the unreadable note", |j| {
        j["note"]
            .as_str()
            .is_some_and(|n| n.starts_with("lane status could not be read:"))
    });
    assert!(job["statusErrors"].as_u64().unwrap() > 0, "{job}");
    assert_eq!(job["state"], "starting", "{job}");
    f.status(&id, &lane_status(&id, "running", json!({})));
    let job = wait_state(&d, &id, "running");
    assert!(job["note"].is_null(), "{job}");
    assert_eq!(job["statusErrors"], 0, "{job}");
}

#[test]
fn a_status_that_is_json_but_not_an_object_is_unreadable() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    // The stand-in writes its `runner_lost` line into the status of a run it has a runner
    // record for, the way `lane` carries the field inside the object it prints: with the record
    // gone the answer reaches the daemon as it is. The runner is stopped before its record goes,
    // so nothing of this fixture is left behind.
    std::fs::write(f.data().join(format!("{id}.stop")), "").unwrap();
    std::thread::sleep(Duration::from_millis(400));
    std::fs::remove_file(f.data().join(format!("{id}.runner"))).unwrap();
    // An array is valid JSON and no status: it is unreadable like a line of text.
    f.status(&id, "[1,2,3]");
    let job = wait_for(&d, &id, "waiting for the unreadable note", |j| {
        j["note"]
            .as_str()
            .is_some_and(|n| n.starts_with("lane status could not be read:"))
    });
    assert!(job["note"].as_str().unwrap().contains("an array"), "{job}");
    assert_eq!(job["state"], "starting", "{job}");
    assert!(job["statusErrors"].as_u64().unwrap() > 0, "{job}");
    f.status(&id, &lane_status(&id, "running", json!({})));
    let job = wait_state(&d, &id, "running");
    assert!(job["note"].is_null(), "{job}");
}

#[test]
fn an_unreadable_status_breaks_the_streak_of_misses() {
    let f = Fixture::new();
    let d = f.daemon(&[("LUCINA_AGENTD_WORK_LOST_AFTER_SECS", "1".to_string())]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    // The stand-in exits 2 on every call but its third, which prints garbage and exits 0.
    let seq = f.data().join(format!("{id}.seq"));
    std::fs::write(&seq, "0\n").unwrap();
    let asked = || read(&seq).trim().parse::<u64>().unwrap_or(0);
    // The call counter is what the asserts are read against, and read before the run is lost:
    // a lost job is asked about too, and by the time `lost` is on the job the calls that broke
    // the streak are long gone among those that counted towards it.
    let deadline = Instant::now() + Duration::from_secs(30);
    while asked() < 4 {
        assert!(Instant::now() < deadline, "the fourth status never came");
        std::thread::sleep(Duration::from_millis(100));
    }
    // The pass that read the garbage is over by now, and two misses do not make three.
    std::thread::sleep(Duration::from_millis(300));
    let job = show(&d, &id);
    assert_ne!(job["state"], "lost", "lost on the fourth call: {job}");
    // Three misses in a row are needed; the readable exit 0 in the middle broke the streak, so
    // the lost came no earlier than the sixth call.
    let job = wait_state_within(&d, &id, "lost", lost_budget());
    let called = asked();
    assert!(
        called >= 6,
        "lost after {called} polls, of which three were misses in a row"
    );
    assert_eq!(job["note"], "lane has no record of this run", "{job}");
}

#[test]
fn a_status_with_no_note_of_its_own_clears_the_note_lane_gave() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(
        &id,
        &lane_status(&id, "running", json!({"note": "thinking hard"})),
    );
    let job = wait_for(&d, &id, "waiting for lane's note", |j| {
        j["note"] == "thinking hard"
    });
    assert_eq!(job["state"], "running", "{job}");
    // The next status says it has no note: the note was lane's own, and lane withdrew it.
    f.status(&id, &lane_status(&id, "verified", json!({"note": null})));
    let job = wait_state(&d, &id, "verified");
    assert!(job["note"].is_null(), "{job}");
}

/// The note came from `lane` before the daemon went down; the job that comes back from
/// `jobs.json` still knows that, and a status that says it has no note takes the note away.
#[test]
fn a_note_lane_gave_is_cleared_after_a_restart_too() {
    let f = Fixture::new();
    let mut d = f.daemon(&[]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(
        &id,
        &lane_status(&id, "running", json!({"note": "thinking hard"})),
    );
    let job = wait_for(&d, &id, "waiting for lane's note", |j| {
        j["note"] == "thinking hard"
    });
    assert_eq!(job["state"], "running", "{job}");

    assert_eq!(
        d.guard.terminate(Duration::from_secs(5)),
        Some(0),
        "clean exit on SIGTERM"
    );
    d.guard = common::spawn_daemon_env(d.dir.path(), &f.env(&[]));
    // The note came over the restart, and with it the word that it is lane's own.
    let job = show(&d, &id);
    assert_eq!(job["note"], "thinking hard", "{job}");

    // A status that says it has no note: what lane gave before the restart it takes back after.
    f.status(&id, &lane_status(&id, "verified", json!({"note": null})));
    let job = wait_state(&d, &id, "verified");
    assert!(job["note"].is_null(), "{job}");
}

/// The base head of a run and a status that ends on `after` in its last attempt.
fn done_with_after_head(
    d: &Daemon,
    f: &Fixture,
    ws: &str,
    after: impl Fn(&str) -> String,
) -> Value {
    let job = start(d, f, "LA", ws, &["--cost", "build"]);
    let id = job["id"].as_str().unwrap().to_string();
    let base = job["baseHead"].as_str().unwrap().to_string();
    let after = after(&base);
    f.status(
        &id,
        &lane_status(
            &id,
            "reported_done",
            json!({"attempts": [attempt(Some(&base), Some(&after))]}),
        ),
    );
    wait_state(d, &id, "done")
}

#[test]
fn a_run_that_ended_on_a_new_head_is_not_empty_and_one_on_the_base_is() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let moved = done_with_after_head(&d, &f, &ws, |_| "1".repeat(40));
    assert_eq!(moved["empty"], false, "{moved}");
    assert_eq!(moved["afterHead"], "1".repeat(40), "{moved}");
    let same = done_with_after_head(&d, &f, &ws, |base| base.to_string());
    assert_eq!(same["empty"], true, "{same}");
}

#[test]
fn waiting_on_a_running_job_times_out_with_exit_5() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(&id, &lane_status(&id, "running", json!({})));
    wait_state(&d, &id, "running");
    let r = lagent(&d, &["work", "wait", &id, "--timeout", "1"]);
    assert_eq!(r.code, 5, "{}", r.stderr);
}

#[test]
fn board_shows_the_work_section() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    start(&d, &f, "LA", &ws, &["--cost", "build"]);
    let r = lagent(&d, &["board"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("\nWORK\n"), "{}", r.stdout);
    assert!(
        r.stdout
            .contains("LA: 0 running, 1 queued, 0 ended, 0 empty"),
        "{}",
        r.stdout
    );
}

/// The work section is an addition to the board, and a daemon that will not answer `/work`
/// costs that addition only: the board is printed as it is and keeps its exit code.
#[test]
fn a_board_that_cannot_read_the_work_jobs_is_still_the_board() {
    use std::io::{Read, Write};
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("daemon.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    // A daemon of one answer each: the pipeline board is served, `/work` refuses.
    let answering = std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).unwrap_or(0) == 0 {
                    break;
                }
                head.push(byte[0]);
            }
            let request = String::from_utf8_lossy(&head).to_string();
            let path = request.split(' ').nth(1).unwrap_or("/").to_string();
            let (status, body) = if path.starts_with("/work") {
                (
                    "500 Internal Server Error",
                    r#"{"error":{"code":"ERROR","message":"the work jobs could not be read"}}"#,
                )
            } else {
                (
                    "200 OK",
                    r#"{"workspaces":[{"state":"active","project":"SecretaryIOS","branch":"main","workspaceId":"ws-1"}],"mainCheckouts":[],"unmanaged":[]}"#,
                )
            };
            let text = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(text.as_bytes());
            let _ = stream.flush();
        }
    });
    let r = common::run(
        Command::new(common::LAGENT)
            .args(["board"])
            .env("LUCINA_AGENTD_SOCKET", &socket),
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("SecretaryIOS"), "{}", r.stdout);
    assert!(r.stdout.contains("ws-1"), "{}", r.stdout);
    assert!(!r.stdout.contains("\nWORK\n"), "{}", r.stdout);
    drop(answering);
}

/// `lagent` speaks the paths of the machine the owner is on: a workspace named under
/// `LAGENT_MAC_PREFIX` is the same directory on this host, and that is what the daemon is told
/// and what `lane run` gets.
#[test]
fn a_workspace_under_the_mac_prefix_reaches_the_run_as_the_host_path() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    // The host home as the Mac sees it, one prefix over this fixture's own tree.
    let home = f.root.path().display().to_string();
    let prefix = format!("{home}/mnt");
    let mac_ws = format!("{prefix}/ws");
    let spec = f.spec();
    let change = change_for(&d, "LA");
    let started = common::run(
        Command::new(common::LAGENT)
            .args([
                "work",
                "start",
                "--json",
                "--lead",
                "LA",
                "--change",
                &change,
                "--lane",
                "qoder",
                "--workspace",
                &mac_ws,
                "--cost",
                "build",
                "--spec",
                &spec,
            ])
            .env("LUCINA_AGENTD_SOCKET", d.socket())
            .env("HOME", &home)
            .env("LAGENT_MAC_PREFIX", &prefix)
            .env_remove("LAGENT_SESSION"),
    );
    assert_eq!(started.code, 0, "{}", started.stderr);
    let job: Value = serde_json::from_str(&started.stdout).unwrap();
    let id = job["id"].as_str().unwrap().to_string();
    assert_eq!(job["workspace"], ws, "{job}");
    // The run itself was started in the host directory, not in the Mac's mount of it.
    let args = read(&f.data().join(format!("{id}.args")));
    assert!(args.lines().any(|l| l == ws), "{args}");
    assert!(!args.contains(&mac_ws), "{args}");
}

/// [`FAKE_LANE`] with one addition: every `lane status <id>` call, however it ends, appends a
/// byte to `<id>.status-calls`. Counting those tells a test whether the poller still asks.
fn counting_stand_in() -> String {
    let script = FAKE_LANE.replace(
        "status)\n  if [ -f",
        "status)\n  printf x >> \"$D/$1.status-calls\"\n  if [ -f",
    );
    assert_ne!(
        script, FAKE_LANE,
        "the stand-in has no status case to count"
    );
    script
}

#[test]
fn a_run_lane_calls_lost_is_final_and_is_never_asked_again() {
    let f = Fixture::new();
    common::write_exec(&f.root.path().join("lane/lane"), &counting_stand_in());
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "LA", &ws, &["--cost", "build"])["id"]
        .as_str()
        .unwrap()
        .to_string();
    let asked = || {
        std::fs::metadata(f.data().join(format!("{id}.status-calls")))
            .map(|m| m.len())
            .unwrap_or(0)
    };
    f.status(
        &id,
        &lane_status(
            &id,
            "lost",
            json!({"attempts": [], "dirty_files": [], "note": "the runner is gone"}),
        ),
    );
    let job = wait_state_within(&d, &id, "lost", lost_budget());
    assert_eq!(job["laneState"], "lost", "{job}");
    // Lane's own note, not the `unknown lane state` of a word it did not know.
    assert_eq!(job["note"], "the runner is gone", "{job}");
    assert!(
        !job["note"]
            .as_str()
            .unwrap_or_default()
            .starts_with("unknown lane state:"),
        "{job}"
    );
    assert_eq!(job["laneLost"], true, "{job}");
    assert!(job["endedAt"].is_string(), "{job}");
    assert_eq!(job["empty"], Value::Null, "{job}");
    let asked_before = asked();
    assert!(asked_before > 0, "the status was never read");
    assert_eq!(lagent(&d, &["work", "wait", &id, "--timeout", "5"]).code, 9);
    // Three poll intervals, and the listing that reads `lane ls`: the run is asked no more.
    std::thread::sleep(Duration::from_millis(3_500));
    assert_eq!(asked(), asked_before, "the poller asked again");
    assert_eq!(show(&d, &id)["state"], "lost");
    lagent_json(&d, &["work", "ls"]);
    assert_eq!(asked(), asked_before, "the poller asked a final run");
}

/// AGENT-01: the create caps count agents, never work runs. A caller that holds six live work
/// runs (more than the per-caller cap of 4) can still create an agent through the same socket.
#[test]
fn a_caller_holding_six_live_work_runs_can_still_create_an_agent() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let mut ids = Vec::new();
    for n in 0..6 {
        let title = format!("run{n}");
        let job = start(&d, &f, "LA", &ws, &["--cost", "build", "--title", &title]);
        ids.push(job["id"].as_str().unwrap().to_string());
    }
    for id in &ids {
        f.status(id, &lane_status(id, "running", json!({"final_lane": "qoder"})));
        wait_state(&d, id, "running");
    }
    let list = lagent_json(&d, &["work", "ls"]);
    assert_eq!(lead_of(&list, "LA").unwrap()["running"], 6, "{list}");

    let body = format!(r#"{{"provider":"mock","cwd":"{ws}"}}"#);
    let out = common::run(Command::new("curl").args([
        "-s",
        "-w",
        "\n%{http_code}",
        "--unix-socket",
        &d.socket().display().to_string(),
        "-X",
        "POST",
        "-H",
        "content-type: application/json",
        "-d",
        &body,
        "http://x/agents",
    ]));
    assert!(out.stdout.ends_with("\n200"), "create refused: {}", out.stdout);
}

/// A stand-in `systemctl` in the daemon's tool directory: it appends its arguments to
/// `systemctl.args`, then runs `body`.
fn add_systemctl(bin: &str, body: &str) {
    common::write_exec(
        &Path::new(bin).join("systemctl"),
        &format!(
            "#!/bin/sh\nD=\"${{0%/*}}\"\nprintf '%s\\n' \"$@\" >> \"$D/systemctl.args\"\n{body}\n"
        ),
    );
}

/// A daemon with the scope on and the stand-ins of `tool_dir` on its `PATH`.
fn scoped_daemon(f: &Fixture, bin: &str) -> Daemon {
    f.daemon(&[
        ("LUCINA_AGENTD_WORK_SCOPE", "on".to_string()),
        ("XDG_RUNTIME_DIR", f.root.path().display().to_string()),
        ("PATH", bin.to_string()),
    ])
}

/// `lagent work start` with the defaults of these tests, not checked.
fn start_unchecked(d: &Daemon, f: &Fixture, ws: &str) -> Run {
    let spec = f.spec();
    let change = change_for(d, "LA");
    lagent(
        d,
        &[
            "work",
            "start",
            "--lead",
            "LA",
            "--change",
            &change,
            "--lane",
            "qoder",
            "--workspace",
            ws,
            "--spec",
            &spec,
            "--cost",
            "build",
        ],
    )
}

/// The start was refused with the slice message and nothing was started or stored.
fn assert_refused_for_the_slice(d: &Daemon, f: &Fixture, bin: &str, r: &Run) {
    assert_ne!(r.code, 0, "{}", r.stdout);
    assert!(
        r.stderr
            .contains("build.slice is not loaded in the user manager"),
        "{}",
        r.stderr
    );
    assert!(
        r.stderr.contains("LUCINA_AGENTD_WORK_SCOPE=off"),
        "{}",
        r.stderr
    );
    let called: Vec<_> = std::fs::read_dir(f.data())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(called.is_empty(), "lane ran: {called:?}");
    assert!(
        !Path::new(bin).join("systemd-run.args").exists(),
        "systemd-run ran"
    );
    let list = lagent_json(d, &["work", "ls"]);
    assert!(list["jobs"].as_array().unwrap().is_empty(), "{list}");
}

#[test]
fn a_scoped_start_without_build_slice_is_refused_before_anything_runs() {
    let f = Fixture::new();
    let bin = tool_dir(&f, true);
    add_systemctl(&bin, "echo not-found");
    let d = scoped_daemon(&f, &bin);
    let ws = f.repo("ws");
    let r = start_unchecked(&d, &f, &ws);
    assert_refused_for_the_slice(&d, &f, &bin, &r);
    let asked = read(&Path::new(&bin).join("systemctl.args"));
    assert_eq!(asked, "--user\nshow\nbuild.slice\n-p\nLoadState\n--value\n");
}

#[test]
fn a_scoped_start_with_build_slice_loaded_starts_as_before() {
    let f = Fixture::new();
    let bin = tool_dir(&f, true);
    add_systemctl(&bin, "echo loaded");
    let d = scoped_daemon(&f, &bin);
    let ws = f.repo("ws");
    let job = start(&d, &f, "LA", &ws, &["--cost", "build"]);
    let id = job["id"].as_str().unwrap().to_string();
    assert_ne!(job["state"], "failed", "{job}");
    assert_eq!(job["scope"], format!("lucina-work-{id}").as_str(), "{job}");
    assert!(Path::new(&bin).join("systemd-run.args").exists());
    // The answer is kept: a second start does not ask again.
    let _ = start(&d, &f, "LA", &ws, &["--cost", "build"]);
    let asked = read(&Path::new(&bin).join("systemctl.args"));
    assert_eq!(asked.matches("build.slice").count(), 1, "{asked}");
}

#[test]
fn a_systemctl_that_does_not_answer_in_time_refuses_the_start() {
    let f = Fixture::new();
    let bin = tool_dir(&f, true);
    add_systemctl(&bin, "exec sleep 30");
    let d = scoped_daemon(&f, &bin);
    let ws = f.repo("ws");
    let began = Instant::now();
    let r = start_unchecked(&d, &f, &ws);
    let took = began.elapsed();
    assert_refused_for_the_slice(&d, &f, &bin, &r);
    assert!(took < Duration::from_secs(7), "refused after {took:?}");
}

#[test]
fn scope_off_never_asks_systemctl() {
    let f = Fixture::new();
    let bin = tool_dir(&f, true);
    add_systemctl(&bin, "echo not-found");
    let d = f.daemon(&[
        ("LUCINA_AGENTD_WORK_SCOPE", "off".to_string()),
        ("XDG_RUNTIME_DIR", f.root.path().display().to_string()),
        ("PATH", bin.clone()),
    ]);
    let ws = f.repo("ws");
    let job = start(&d, &f, "LA", &ws, &["--cost", "build"]);
    assert_ne!(job["state"], "failed", "{job}");
    assert!(!Path::new(&bin).join("systemctl.args").exists());
    assert!(!Path::new(&bin).join("systemd-run.args").exists());
}
