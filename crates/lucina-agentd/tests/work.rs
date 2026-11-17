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
case "$cmd" in
run)
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
  echo "$id"
  ;;
status)
  if [ -f "$D/$1.status" ]; then cat "$D/$1.status"; else exit 2; fi
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

fn start(d: &Daemon, f: &Fixture, lead: &str, ws: &str, extra: &[&str]) -> Value {
    let spec = f.spec();
    let mut args = vec![
        "work",
        "start",
        "--lead",
        lead,
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
        "chat-a",
        &ws,
        &["--verify", "true", "--title", "report"],
    );
    let id = job["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("wk-"), "{job}");
    assert_eq!(job["state"], "starting");
    assert_eq!(job["lead"], "chat-a");
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
    let lead = lead_of(&list, "chat-a").unwrap_or_else(|| panic!("{list}"));
    assert_eq!(lead["running"], 1);
    assert_eq!(lead["jobs"][0]["id"], id.as_str());
    assert_eq!(lead["jobs"][0]["finalLane"], "qoder");
    let table = lagent(&d, &["work", "ls"]);
    assert_eq!(table.code, 0, "{}", table.stderr);
    assert!(
        table
            .stdout
            .contains("chat-a: 1 running, 0 queued, 0 ended, 0 empty"),
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

    let a = start(&d, &f, "chat-a", &ws, &["--verify", "true"]);
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

    let b = start(&d, &f, "chat-a", &ws, &["--expect", "out/report.md"]);
    let b = b["id"].as_str().unwrap().to_string();
    f.status(&b, &lane_status(&b, "reported_done", json!({})));
    let done = wait_state(&d, &b, "done");
    assert_eq!(done["empty"], true, "{done}");
    assert!(done["endedAt"].is_string());

    let c = start(&d, &f, "chat-a", &ws, &["--expect", "out/report.md"]);
    let c = c["id"].as_str().unwrap().to_string();
    std::fs::create_dir_all(Path::new(&ws).join("out")).unwrap();
    std::fs::write(Path::new(&ws).join("out/report.md"), "# report\n").unwrap();
    f.status(&c, &lane_status(&c, "reported_done", json!({})));
    let done = wait_state(&d, &c, "done");
    assert_eq!(done["empty"], false, "{done}");

    let list = lagent_json(&d, &["work", "ls"]);
    let lead = lead_of(&list, "chat-a").unwrap();
    assert_eq!(
        (lead["ended"].as_u64(), lead["empty"].as_u64()),
        (Some(3), Some(1)),
        "{lead}"
    );
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
            "c",
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
            "c",
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
            "c",
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
            "c",
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
            "c",
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
            "c",
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
                "--lead",
                "c",
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
            "c",
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
            "c",
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
    let f = Fixture::new();
    let mut d = f.daemon(&[]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "chat-a", &ws, &[])["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(&id, &lane_status(&id, "running", json!({})));
    wait_state(&d, &id, "running");

    assert_eq!(
        d.guard.terminate(Duration::from_secs(5)),
        Some(0),
        "clean exit on SIGTERM"
    );
    d.guard = common::spawn_daemon_env(d.dir.path(), &f.env(&[]));
    let job = show(&d, &id);
    assert_eq!(job["lead"], "chat-a", "{job}");

    f.status(
        &id,
        &lane_status(&id, "failed", json!({"note": "nothing left"})),
    );
    let job = wait_state(&d, &id, "failed");
    assert!(job["endedAt"].is_string());
    assert_eq!(lagent(&d, &["work", "wait", &id, "--timeout", "5"]).code, 1);
}

#[test]
fn runs_started_with_lane_directly_are_listed_as_direct() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let mine = start(&d, &f, "chat-a", &ws, &[])["id"]
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
    assert!(lead_of(&list, "chat-a").is_some());
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
    assert!(lead_of(&none, "chat-a").is_some());
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
    let id = start(&d, &f, "chat-a", &ws, &[])["id"]
        .as_str()
        .unwrap()
        .to_string();
    // No status file: `lane status` exits 2.
    let job = wait_state(&d, &id, "lost");
    assert_eq!(job["note"], "lane has no record of this run");
    assert!(job["endedAt"].is_string());
    assert_eq!(lagent(&d, &["work", "wait", &id, "--timeout", "5"]).code, 1);
}

#[test]
fn a_lane_run_that_fails_marks_the_job_failed_with_its_stderr() {
    let f = Fixture::new();
    std::fs::write(f.data().join("run-fail"), "").unwrap();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    // A failed start still prints the job, and the exit code says it failed.
    let spec = f.spec();
    let r = lagent(
        &d,
        &[
            "work",
            "start",
            "--lead",
            "chat-a",
            "--lane",
            "qoder",
            "--workspace",
            &ws,
            "--spec",
            &spec,
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
    // It stays failed: no later poll turns it into anything else.
    std::thread::sleep(Duration::from_millis(2500));
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
    let r = common::run(
        Command::new(common::LAGENT)
            .args([
                "work",
                "start",
                "--lead",
                "c",
                "--lane",
                "qoder",
                "--workspace",
                ".",
                "--spec",
                &spec,
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
    let job = start(&d, &f, "chat-a", &ws, &[]);
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
    let id = start(&d, &f, "chat-a", &ws, &[])["id"]
        .as_str()
        .unwrap()
        .to_string();
    wait_state(&d, &id, "lost");
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
fn a_lane_status_that_cannot_start_counts_as_a_miss() {
    let f = Fixture::new();
    let d = f.daemon(&[("LUCINA_AGENTD_WORK_LOST_AFTER_SECS", "1".to_string())]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "chat-a", &ws, &[])["id"]
        .as_str()
        .unwrap()
        .to_string();
    f.status(&id, &lane_status(&id, "running", json!({})));
    wait_state(&d, &id, "running");
    std::fs::remove_file(f.root.path().join("lane/lane")).unwrap();
    let job = wait_state(&d, &id, "lost");
    assert_eq!(job["note"], "lane has no record of this run");
}

#[test]
fn a_status_that_is_not_json_changes_nothing_until_it_is_valid() {
    let f = Fixture::new();
    let d = f.daemon(&[]);
    let ws = f.repo("ws");
    let id = start(&d, &f, "chat-a", &ws, &[])["id"]
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

/// The base head of a run and a status that ends on `after` in its last attempt.
fn done_with_after_head(
    d: &Daemon,
    f: &Fixture,
    ws: &str,
    after: impl Fn(&str) -> String,
) -> Value {
    let job = start(d, f, "chat-a", ws, &[]);
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
    let id = start(&d, &f, "chat-a", &ws, &[])["id"]
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
    start(&d, &f, "chat-a", &ws, &[]);
    let r = lagent(&d, &["board"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("\nWORK\n"), "{}", r.stdout);
    assert!(
        r.stdout
            .contains("chat-a: 0 running, 1 queued, 0 ended, 0 empty"),
        "{}",
        r.stdout
    );
}
