//! Identifiers end to end: a real daemon, `lagent id` and `lagent trace`, and for the work jobs a
//! stand-in `lane` (a small `sh` script that keeps one file per run id in its directory). The
//! stand-in's `run` also copies the id store as it is at that moment, and writes the values of
//! exactly the three variables `LUCINA_RUN_ID`, `LUCINA_LEAD` and `LUCINA_CHANGE` it was given.

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
  if [ -f "$D/ids-path" ] && [ -f "$(cat "$D/ids-path")" ]; then
    cp "$(cat "$D/ids-path")" "$D/$id.ids-at-run"
  fi
  if [ -f "$D/run-fail" ]; then
    echo "lane: out of cheese" >&2
    exit 1
  fi
  printf '%s\n' "$@" > "$D/$id.args"
  printf 'RUN_ID=%s\nLEAD=%s\nCHANGE=%s\n' "$LUCINA_RUN_ID" "$LUCINA_LEAD" "$LUCINA_CHANGE" > "$D/$id.run"
  cat > "$D/$id.stdin"
  echo "$id"
  ;;
status)
  if [ -f "$D/$1.status" ]; then cat "$D/$1.status"; else exit 2; fi
  ;;
ls)
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
model = "auto-free"
[categories]
deep = ["qoder"]
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
        std::fs::write(root.path().join("spec.md"), "Write the ids.\n").unwrap();
        Fixture { root }
    }

    fn data(&self) -> PathBuf {
        self.root.path().join("lane/data")
    }

    fn spec(&self) -> String {
        self.root.path().join("spec.md").display().to_string()
    }

    /// A git repository with one commit.
    fn repo(&self, name: &str) -> String {
        let dir = self.root.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        std::fs::write(dir.join("README"), "repo\n").unwrap();
        git(&dir, &["add", "."]);
        commit(&dir, "init", &[]);
        dir.display().to_string()
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            (
                "LUCINA_AGENTD_LANE",
                self.root.path().join("lane/lane").display().to_string(),
            ),
            ("LUCINA_AGENTD_WORK_POLL_SECS", "1".to_string()),
            (
                "LANE_CONFIG",
                self.root.path().join("chains.toml").display().to_string(),
            ),
        ]
    }

    fn daemon(&self) -> Daemon {
        let d = Daemon::start_with_env(&self.env());
        // The stand-in copies the store when `run` is called.
        std::fs::write(
            self.data().join("ids-path"),
            ids_file(&d).display().to_string(),
        )
        .unwrap();
        d
    }

    /// Moves a run on: what `lane status <id>` prints from now on.
    fn status(&self, id: &str, json: &str) {
        let tmp = self.data().join(format!("{id}.status.new"));
        std::fs::write(&tmp, json).unwrap();
        std::fs::rename(tmp, self.data().join(format!("{id}.status"))).unwrap();
    }
}

fn ids_file(d: &Daemon) -> PathBuf {
    d.state().join("ids/ids.jsonl")
}

/// A git command in `dir`; its trimmed output.
fn git(dir: impl AsRef<Path>, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir.as_ref())
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

/// A commit with the message `subject` and the trailer lines (a last paragraph).
fn commit(dir: impl AsRef<Path>, subject: &str, trailers: &[&str]) -> String {
    commit_at(dir, subject, trailers, None)
}

fn commit_at(
    dir: impl AsRef<Path>,
    subject: &str,
    trailers: &[&str],
    date: Option<&str>,
) -> String {
    let message = if trailers.is_empty() {
        subject.to_string()
    } else {
        format!("{subject}\n\n{}", trailers.join("\n"))
    };
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir.as_ref()).args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        &message,
    ]);
    if let Some(d) = date {
        cmd.env("GIT_COMMITTER_DATE", d);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "commit: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    git(dir, &["rev-parse", "HEAD"])
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

/// The stdout of a command that must succeed, trimmed.
fn ok(d: &Daemon, args: &[&str]) -> String {
    let r = lagent(d, args);
    assert_eq!(r.code, 0, "lagent {args:?} failed: {}", r.stderr);
    r.stdout.trim().to_string()
}

fn lagent_json(d: &Daemon, args: &[&str]) -> Value {
    let mut a = args.to_vec();
    a.push("--json");
    let r = lagent(d, &a);
    assert_eq!(r.code, 0, "lagent {args:?} failed: {}", r.stderr);
    serde_json::from_str(&r.stdout).unwrap_or_else(|e| panic!("lagent {args:?}: {e}: {}", r.stdout))
}

/// A command that must fail; its stderr.
fn refused(d: &Daemon, args: &[&str]) -> String {
    let r = lagent(d, args);
    assert_ne!(
        r.code, 0,
        "lagent {args:?} should have failed: {}",
        r.stdout
    );
    r.stderr
}

fn lead_add(d: &Daemon, code: &str) {
    ok(d, &["id", "lead", "add", code, &format!("{code} chat")]);
}

/// A run declared by hand.
fn new_run(d: &Daemon, lead: &str, title: &str) -> String {
    ok(
        d,
        &[
            "id", "new", "run", "--lead", lead, "--title", title, "--by", "qoder", "--agent",
            "agent-1",
        ],
    )
}

fn new_change(d: &Daemon, lead: &str) -> String {
    ok(
        d,
        &["id", "new", "change", "--lead", lead, "--title", "a change"],
    )
}

fn view(d: &Daemon, id: &str) -> Value {
    lagent_json(d, &["id", "show", id])["view"].clone()
}

fn rows(d: &Daemon, id: &str) -> Vec<Value> {
    lagent_json(d, &["id", "show", id])["rows"]
        .as_array()
        .unwrap()
        .clone()
}

/// Waits for `ready()`; panics with `what` after 20 seconds.
fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn wait_rows(d: &Daemon, id: &str, n: usize) -> Vec<Value> {
    wait_until(&format!("{n} rows of {id}"), || rows(d, id).len() >= n);
    rows(d, id)
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn stop(d: &mut Daemon) {
    assert_eq!(
        d.guard.terminate(Duration::from_secs(5)),
        Some(0),
        "clean exit on SIGTERM"
    );
}

fn start_again(d: &mut Daemon, env: &[(&'static str, String)]) {
    d.guard = common::spawn_daemon_env(d.dir.path(), env);
}

/// `lagent work start` for lead LA and change LA-C001.
fn work_start(d: &Daemon, f: &Fixture, ws: &str, extra: &[&str]) -> Run {
    let spec = f.spec();
    let mut args = vec![
        "work",
        "start",
        "--lead",
        "LA",
        "--change",
        "LA-C001",
        "--workspace",
        ws,
        "--spec",
        &spec,
        "--json",
    ];
    args.extend_from_slice(extra);
    lagent(d, &args)
}

fn work_job(r: &Run) -> Value {
    serde_json::from_str(&r.stdout).unwrap_or_else(|e| panic!("{e}: {} {}", r.stdout, r.stderr))
}

/// Lead LA with change LA-C001.
fn setup_la(d: &Daemon) {
    lead_add(d, "LA");
    assert_eq!(new_change(d, "LA"), "LA-C001");
}

#[test]
fn ids_come_out_in_order_and_each_lead_counts_alone() {
    let d = Daemon::start();
    lead_add(&d, "LA");
    lead_add(&d, "JS");
    ok(
        &d,
        &[
            "id",
            "area",
            "add",
            "GC",
            "--lead",
            "JS",
            "garbage collector",
        ],
    );
    assert_eq!(new_run(&d, "LA", "first"), "LA-R001");
    assert_eq!(new_run(&d, "LA", "second"), "LA-R002");
    assert_eq!(new_change(&d, "LA"), "LA-C001");
    assert_eq!(
        ok(
            &d,
            &["id", "new", "version", "--lead", "LA", "--title", "v"]
        ),
        "LA-V001"
    );
    assert_eq!(
        ok(
            &d,
            &["id", "new", "finding", "--area", "GC", "--title", "uaf"]
        ),
        "GC-01"
    );
    assert_eq!(
        ok(
            &d,
            &["id", "new", "finding", "--area", "GC", "--title", "leak"]
        ),
        "GC-02"
    );
    // JS numbers on its own.
    assert_eq!(new_run(&d, "JS", "js run"), "JS-R001");
    assert_eq!(new_change(&d, "JS"), "JS-C001");
    // The registry lists.
    let leads = ok(&d, &["id", "lead", "ls"]);
    assert!(leads.contains("LA") && leads.contains("JS"), "{leads}");
    let areas = ok(&d, &["id", "area", "ls"]);
    assert!(
        areas.contains("GC") && areas.contains("garbage collector"),
        "{areas}"
    );
    // Defaults per kind, and the people behind the rows.
    assert_eq!(view(&d, "LA-R001")["state"], "started");
    assert_eq!(view(&d, "LA-C001")["state"], "open");
    assert_eq!(view(&d, "LA-V001")["state"], "built");
    assert_eq!(view(&d, "GC-01")["state"], "reported");
    // A row issued through the API says the lead declared it.
    assert_eq!(view(&d, "LA-R001")["declaredBy"], "lead");
    assert_eq!(view(&d, "LA-R001")["agent"], "agent-1");
}

#[test]
fn twenty_simultaneous_requests_give_twenty_different_ids_without_a_gap() {
    let d = Daemon::start();
    lead_add(&d, "LA");
    let socket = d.socket();
    let ids: Vec<String> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..20)
            .map(|i| {
                let socket = socket.clone();
                s.spawn(move || {
                    let r = common::run(
                        Command::new(common::LAGENT)
                            .args([
                                "id",
                                "new",
                                "run",
                                "--lead",
                                "LA",
                                "--title",
                                &format!("run {i}"),
                                "--by",
                                "qoder",
                                "--agent",
                                "agent-1",
                            ])
                            .env("LUCINA_AGENTD_SOCKET", socket),
                    );
                    assert_eq!(r.code, 0, "{}", r.stderr);
                    r.stdout.trim().to_string()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut sorted = ids.clone();
    sorted.sort();
    let want: Vec<String> = (1..=20).map(|n| format!("LA-R{n:03}")).collect();
    assert_eq!(sorted, want, "{ids:?}");
}

#[test]
fn refusals() {
    let d = Daemon::start();
    lead_add(&d, "LA");
    lead_add(&d, "JS");
    ok(&d, &["id", "area", "add", "GC", "--lead", "JS", "gc"]);
    new_run(&d, "LA", "a run");
    new_change(&d, "LA");

    let e = refused(&d, &["id", "new", "change", "--lead", "ZZ", "--title", "t"]);
    assert!(e.contains("lead ZZ is not registered"), "{e}");
    let e = refused(
        &d,
        &["id", "new", "finding", "--area", "NOPE", "--title", "t"],
    );
    assert!(e.contains("area NOPE is not registered"), "{e}");
    // A state of another kind.
    let e = refused(&d, &["id", "state", "LA-C001", "accepted"]);
    assert!(e.contains("does not belong to a change"), "{e}");
    let e = refused(&d, &["id", "state", "LA-R001", "merged"]);
    assert!(e.contains("does not belong to a run"), "{e}");
    // A state row for an id that does not exist.
    let e = refused(&d, &["id", "state", "LA-R999", "accepted"]);
    assert!(e.contains("no id LA-R999"), "{e}");
    // A refs id that does not exist.
    let e = refused(
        &d,
        &[
            "id", "new", "change", "--lead", "LA", "--title", "t", "--refs", "LA-C999",
        ],
    );
    assert!(e.contains("no id LA-C999"), "{e}");
    // An area code equal to a lead code.
    let e = refused(&d, &["id", "area", "add", "LA", "--lead", "JS", "x"]);
    assert!(e.contains("lead code"), "{e}");
    // A lead twice, and an area twice.
    let e = refused(&d, &["id", "lead", "add", "LA", "again"]);
    assert!(e.contains("already registered"), "{e}");
    let e = refused(&d, &["id", "area", "add", "GC", "--lead", "LA", "again"]);
    assert!(e.contains("already registered"), "{e}");
    // A blank title and a bad lead code.
    let e = refused(&d, &["id", "new", "change", "--lead", "LA", "--title", " "]);
    assert!(e.contains("title is required"), "{e}");
    let e = refused(&d, &["id", "lead", "add", "lab", "x"]);
    assert!(e.contains("two capital letters"), "{e}");
    // A run declared by hand names who stands behind it: the CLI and the API both insist.
    let e = refused(
        &d,
        &[
            "id", "new", "run", "--lead", "LA", "--title", "t", "--by", "qoder",
        ],
    );
    assert!(e.contains("--agent"), "{e}");
    let e = refused(
        &d,
        &[
            "id", "new", "run", "--lead", "LA", "--title", "t", "--agent", "a",
        ],
    );
    assert!(e.contains("--by"), "{e}");
    let sock = d.socket().display().to_string();
    let r = common::run(Command::new("curl").args([
        "-s",
        "-X",
        "POST",
        "-H",
        "content-type: application/json",
        "-d",
        r#"{"kind":"run","lead":"LA","title":"t"}"#,
        "--unix-socket",
        &sock,
        "http://x/ids",
    ]));
    let v: Value = serde_json::from_str(&r.stdout).unwrap_or_else(|e| panic!("{e}: {}", r.stdout));
    assert!(
        v["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("must name who stands behind it")),
        "{v}"
    );
    // None of the refusals wrote anything: one run, one change and the registry.
    let all = lagent_json(&d, &["id", "ls"]);
    assert_eq!(all["ids"].as_array().unwrap().len(), 2 + 2 + 1, "{all}");
}

#[test]
fn a_state_change_is_a_new_row() {
    let d = Daemon::start();
    lead_add(&d, "LA");
    new_run(&d, "LA", "a run");
    ok(
        &d,
        &[
            "id",
            "state",
            "LA-R001",
            "accepted",
            "--result",
            "commit abc",
        ],
    );
    let shown = lagent_json(&d, &["id", "show", "LA-R001"]);
    let rows = shown["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{shown}");
    assert_eq!(rows[0]["state"], "started");
    assert_eq!(rows[1]["state"], "accepted");
    assert_eq!(rows[1]["result"], "commit abc");
    assert_eq!(shown["view"]["state"], "accepted");
    assert_eq!(shown["view"]["result"], "commit abc");
    // The title is kept from the first row.
    assert_eq!(shown["view"]["title"], "a run");
    let found = lagent_json(&d, &["id", "ls", "--state", "accepted"]);
    let ids = found["ids"].as_array().unwrap();
    assert_eq!(ids.len(), 1, "{found}");
    assert_eq!(ids[0]["id"], "LA-R001");
    assert!(lagent_json(&d, &["id", "ls", "--state", "started"])["ids"]
        .as_array()
        .unwrap()
        .is_empty());
    // The table and the filters.
    let table = ok(
        &d,
        &["id", "ls", "--lead", "LA", "--kind", "run", "-q", "A RUN"],
    );
    assert!(table.starts_with("ID"), "{table}");
    assert!(
        table.contains("LA-R001") && table.contains("accepted"),
        "{table}"
    );
    let text = ok(&d, &["id", "show", "LA-R001"]);
    assert!(text.contains("ROWS"), "{text}");
    assert!(text.contains("started"), "{text}");
}

#[test]
fn the_numbering_continues_after_a_restart() {
    let mut d = Daemon::start();
    lead_add(&d, "LA");
    assert_eq!(new_run(&d, "LA", "one"), "LA-R001");
    assert_eq!(new_run(&d, "LA", "two"), "LA-R002");
    stop(&mut d);
    start_again(&mut d, &[]);
    assert_eq!(new_run(&d, "LA", "three"), "LA-R003");
    assert_eq!(view(&d, "LA-R001")["title"], "one");
    assert_eq!(lagent_json(&d, &["id", "ls"])["chainBrokenAt"], Value::Null);
}

#[test]
fn an_unreadable_line_makes_the_store_read_only_and_the_file_stays_as_it_was() {
    let mut d = Daemon::start();
    lead_add(&d, "LA");
    new_run(&d, "LA", "one");
    stop(&mut d);
    let file = ids_file(&d);
    let mut text = read(&file);
    text.push_str("this is not json\n");
    std::fs::write(&file, &text).unwrap();
    start_again(&mut d, &[]);

    // Reads work with the lines that parsed.
    assert_eq!(view(&d, "LA-R001")["title"], "one");
    let list = lagent_json(&d, &["id", "ls"]);
    assert_eq!(list["ids"].as_array().unwrap().len(), 2, "{list}");
    // Writes answer the error.
    let e = refused(
        &d,
        &[
            "id", "new", "run", "--lead", "LA", "--title", "two", "--by", "qoder", "--agent", "a",
        ],
    );
    assert!(
        e.contains("the id store has an unreadable line (3); fix it by hand"),
        "{e}"
    );
    let e = refused(&d, &["id", "state", "LA-R001", "accepted"]);
    assert!(e.contains("unreadable line (3)"), "{e}");
    assert_eq!(read(&file), text, "the file is byte-identical");
}

#[test]
fn a_changed_line_breaks_the_chain() {
    let mut d = Daemon::start();
    lead_add(&d, "LA");
    new_run(&d, "LA", "first");
    new_run(&d, "LA", "second");
    stop(&mut d);
    // Three rows verify.
    start_again(&mut d, &[]);
    let list = lagent_json(&d, &["id", "ls"]);
    assert_eq!(list["chainBrokenAt"], Value::Null, "{list}");
    assert_eq!(list["readOnly"], Value::Null, "{list}");
    assert_eq!(new_run(&d, "LA", "third"), "LA-R003");
    stop(&mut d);

    // The test changes line 2 (the first run); line 3's prev no longer matches.
    let file = ids_file(&d);
    let text = read(&file);
    let edited = text.replacen("\"title\":\"first\"", "\"title\":\"FIRST\"", 1);
    assert_ne!(text, edited);
    std::fs::write(&file, &edited).unwrap();
    start_again(&mut d, &[]);

    let list = lagent_json(&d, &["id", "ls"]);
    assert_eq!(list["chainBrokenAt"], 3, "{list}");
    let after = |id: &str| {
        list["ids"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing: {list}"))["afterBreak"]
            .clone()
    };
    assert_eq!(after("LA"), false);
    assert_eq!(after("LA-R001"), false);
    assert_eq!(after("LA-R002"), true);
    assert_eq!(after("LA-R003"), true);
    let e = refused(
        &d,
        &[
            "id", "new", "run", "--lead", "LA", "--title", "x", "--by", "qoder", "--agent", "a",
        ],
    );
    assert!(
        e.contains("the id store's hash chain is broken at line 3; fix it by hand"),
        "{e}"
    );
    assert_eq!(read(&file), edited, "the file is byte-identical");
    // The table says so too.
    assert!(ok(&d, &["id", "ls"]).contains("hash chain is broken at line 3"));
}

/// A hand-kept ledger: eight ids and a state change.
const LEDGER: &str = "id\tutc\tkind\tstate\ttitle\tby\tspec\twhere\tbase\tresult\trefs
LA-R001\t2026-11-10T09:00:00Z\trun\tstarted\tIds pure rules\tqoder auto-free\t/home/nez/specs/a.md\tlagent-ids\tabc123\t\tLA-C001
LA-R001\t2026-11-10T10:00:00Z\trun\tverified\tIds pure rules\tqoder auto-free\t\t\t\tcommit def456\tLA-C001
LA-R002\t2026-11-10T10:30:00Z\trun\tstarted\tReview\topus\t\t\t\t\tLA-C001 GC-01
LA-C001\t2026-11-10T08:00:00Z\tchange\topen\tIdentifiers\tLA
LA-C001\t2026-11-11T08:00:00Z\tchange\tmerged\t\t\t\t\t\tmerge 9f9f\tLA-V001
LA-V001\t2026-11-12T08:00:00Z\tversion\tbuilt\tbuild\tLA\t\t\tmain 9f9f
GC-01\t2026-11-09T08:00:00Z\tfinding\treported\tUse after free\tJS-R004
GC-01\t2026-11-12T09:00:00Z\tfinding\tfixed\t\t\t\t\t\t\tLA-C001
";

#[test]
fn a_hand_kept_ledger_imports_once() {
    let d = Daemon::start();
    lead_add(&d, "LA");
    lead_add(&d, "JS");
    ok(&d, &["id", "area", "add", "GC", "--lead", "JS", "gc"]);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("ledger.tsv");
    std::fs::write(&file, LEDGER).unwrap();
    let path = file.display().to_string();

    assert_eq!(ok(&d, &["id", "import", &path]), "imported 8, skipped 0");
    assert_eq!(ok(&d, &["id", "import", &path]), "imported 0, skipped 8");
    // The rows are there, merged: the latest state, the first row's fields.
    let r = view(&d, "LA-R001");
    assert_eq!(r["state"], "verified");
    assert_eq!(r["rows"], 2);
    assert_eq!(r["declaredBy"], "import");
    assert_eq!(r["base"], "abc123");
    assert_eq!(r["result"], "commit def456");
    assert_eq!(view(&d, "GC-01")["state"], "fixed");
    assert_eq!(view(&d, "LA-C001")["state"], "merged");
    // The numbering goes on after the imported ids.
    assert_eq!(new_run(&d, "LA", "next"), "LA-R003");
    assert_eq!(new_change(&d, "LA"), "LA-C002");
    assert_eq!(
        ok(
            &d,
            &["id", "new", "finding", "--area", "GC", "--title", "next"]
        ),
        "GC-02"
    );

    // Rows of a lead that is not registered are refused, and nothing is written.
    let before = read(&ids_file(&d));
    let bad = dir.path().join("bad.tsv");
    std::fs::write(
        &bad,
        "id\tutc\tkind\tstate\ttitle\nLA-R010\t2026-11-13T00:00:00Z\trun\tstarted\tok row\nPK-R001\t2026-11-13T00:00:00Z\trun\tstarted\tpokoin\n",
    )
    .unwrap();
    let e = refused(&d, &["id", "import", &bad.display().to_string()]);
    assert!(e.contains("lead PK is not registered"), "{e}");
    assert_eq!(read(&ids_file(&d)), before);
    // An area that is not registered is refused for a finding.
    std::fs::write(
        &bad,
        "id\tutc\tkind\tstate\ttitle\nZZZ-01\tt\tfinding\treported\tx\n",
    )
    .unwrap();
    let e = refused(&d, &["id", "import", &bad.display().to_string()]);
    assert!(e.contains("area ZZZ is not registered"), "{e}");
    // A line that is not a row names its line.
    std::fs::write(&bad, "id\tutc\tkind\tstate\ntrash\n").unwrap();
    let e = refused(&d, &["id", "import", &bad.display().to_string()]);
    assert!(e.contains("line 2"), "{e}");
    assert_eq!(read(&ids_file(&d)), before);

    // An area file.
    let areas = dir.path().join("areas.tsv");
    std::fs::write(&areas, "code\tlead\tcovers\nREENT\tLA\tre-entrancy\n").unwrap();
    let a = areas.display().to_string();
    assert_eq!(
        ok(&d, &["id", "import", &a, "--areas"]),
        "imported 1, skipped 0"
    );
    assert_eq!(
        ok(&d, &["id", "import", &a, "--areas"]),
        "imported 0, skipped 1"
    );
    assert_eq!(view(&d, "REENT")["by"], "LA");
}

/// The first line of the fake lane's stored arguments after `flag`.
fn arg_after(args: &[String], flag: &str) -> String {
    let at = args
        .iter()
        .position(|a| a == flag)
        .unwrap_or_else(|| panic!("{flag} missing in {args:?}"));
    args[at + 1].clone()
}

fn stored_args(f: &Fixture, id: &str) -> Vec<String> {
    read(&f.data().join(format!("{id}.args")))
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn a_work_start_issues_the_run_and_follows_it_to_its_end() {
    let f = Fixture::new();
    let d = f.daemon();
    setup_la(&d);
    let ws = f.repo("ws");
    let head = git(&ws, &["rev-parse", "HEAD"]);
    let branch = git(&ws, &["rev-parse", "--abbrev-ref", "HEAD"]);

    let r = work_start(
        &d,
        &f,
        &ws,
        &["--cost", "build", "--lane", "qoder", "--name", "Ids writer"],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let job = work_job(&r);
    let id = job["id"].as_str().unwrap().to_string();
    assert_eq!(id, "la-r001-ids-writer", "{job}");
    assert_eq!(job["runId"], "LA-R001");
    assert_eq!(job["change"], "LA-C001");
    assert_eq!(job["lead"], "LA");

    // The stand-in got the lane id, and the ids as flags and as exactly three variables.
    let args = stored_args(&f, &id);
    assert_eq!(arg_after(&args, "--id"), id);
    assert_eq!(arg_after(&args, "--lead"), "LA");
    assert_eq!(arg_after(&args, "--run-id"), "LA-R001");
    assert_eq!(arg_after(&args, "--change"), "LA-C001");
    assert_eq!(arg_after(&args, "--owner"), "LA");
    assert_eq!(
        read(&f.data().join(format!("{id}.run"))),
        "RUN_ID=LA-R001\nLEAD=LA\nCHANGE=LA-C001\n"
    );

    // The row existed when `lane run` was called: the stand-in copied the store at that moment.
    let at_run = read(&f.data().join(format!("{id}.ids-at-run")));
    let started = at_run
        .lines()
        .find(|l| l.contains("\"id\":\"LA-R001\""))
        .unwrap_or_else(|| panic!("no row of LA-R001 when lane ran: {at_run}"));
    let started: Value = serde_json::from_str(started).unwrap();
    assert_eq!(started["state"], "started");
    assert_eq!(started["lane"], "qoder");
    assert_eq!(started["declaredBy"], "daemon");
    assert_eq!(started["base"], head.as_str());
    assert_eq!(started["workspace"], ws.as_str());

    // The row's facts.
    let v = view(&d, "LA-R001");
    assert_eq!(v["state"], "started");
    assert_eq!(v["refs"], json!(["LA-C001"]));
    assert_eq!(v["by"], "qoder auto-free");
    assert_eq!(v["lane"], "qoder");
    assert_eq!(v["model"], "auto-free");
    assert_eq!(v["modelRaw"], "auto-free");
    assert_eq!(v["base"], head.as_str());
    assert_eq!(v["declaredBy"], "daemon");
    assert_eq!(v["workspace"], ws.as_str());
    assert_eq!(v["branch"], branch.as_str());
    assert_eq!(v["spec"], job["specPath"]);
    assert_eq!(v["where"], ws.as_str());
    assert_eq!(v["title"], "Write the ids.");
    assert_eq!(
        std::fs::canonicalize(v["repo"].as_str().unwrap()).unwrap(),
        std::fs::canonicalize(&ws).unwrap()
    );
    assert_eq!(v["rows"], 1);

    // verified: a `verified` row is added.
    f.status(
        &id,
        &lane_status(
            &id,
            "verified",
            json!({"final_lane": "qoder", "model": "auto-free",
                   "verify": {"cmd": "true", "ran_cmd": null, "exit_code": 0, "head": "deadbeef",
                              "ran": true, "timed_out": false, "note": null},
                   "attempts": [{"lane": "qoder", "outcome": "ok", "reason": null,
                                 "before_head": head, "after_head": null}]}),
        ),
    );
    let rs = wait_rows(&d, "LA-R001", 2);
    assert_eq!(rs[1]["state"], "verified");
    assert_eq!(rs[1]["declaredBy"], "daemon");
    assert_eq!(rs[1]["lane"], "qoder");
    assert_eq!(rs[1]["model"], "auto-free");
    assert_eq!(rs[1]["by"], "qoder auto-free");
    assert!(
        rs[1]["result"].as_str().unwrap().starts_with("verified"),
        "{}",
        rs[1]
    );
    assert_eq!(view(&d, "LA-R001")["state"], "verified");

    // A reported_done job whose expected file is missing: an `empty` row.
    let job = work_job(&work_start(
        &d,
        &f,
        &ws,
        &[
            "--lane", "qoder", "--name", "Second", "--expect", "out/x.md",
        ],
    ));
    assert_eq!(job["id"], "la-r002-second", "{job}");
    f.status(
        "la-r002-second",
        &lane_status("la-r002-second", "reported_done", json!({})),
    );
    let rs = wait_rows(&d, "LA-R002", 2);
    assert_eq!(rs[1]["state"], "empty", "{rs:?}");
    assert!(rs[1]["result"].as_str().unwrap().contains("empty"));

    // failed: a `failed` row.
    work_start(
        &d,
        &f,
        &ws,
        &["--cost", "build", "--lane", "qoder", "--name", "Third"],
    );
    f.status(
        "la-r003-third",
        &lane_status("la-r003-third", "failed", json!({"note": "nothing left"})),
    );
    let rs = wait_rows(&d, "LA-R003", 2);
    assert_eq!(rs[1]["state"], "failed", "{rs:?}");

    // A plain reported_done that made a commit is for the lead to accept: no second row.
    work_start(
        &d,
        &f,
        &ws,
        &["--cost", "build", "--lane", "qoder", "--name", "Fourth"],
    );
    f.status(
        "la-r004-fourth",
        &lane_status(
            "la-r004-fourth",
            "reported_done",
            json!({"attempts": [{"lane": "qoder", "outcome": "ok", "reason": null,
                                 "before_head": head, "after_head": "1".repeat(40)}]}),
        ),
    );
    wait_until("the job is done", || {
        lagent_json(&d, &["work", "show", "la-r004-fourth"])["state"] == "done"
    });
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(rows(&d, "LA-R004").len(), 1);
    assert_eq!(view(&d, "LA-R004")["state"], "started");

    // The list shows the run and the change first.
    let list = ok(&d, &["work", "ls"]);
    let head_line = list.lines().nth(1).unwrap();
    assert!(head_line.starts_with("RUN"), "{list}");
    assert!(
        list.contains("LA-R001") && list.contains("LA-C001"),
        "{list}"
    );
}

#[test]
fn a_chain_run_shows_no_lane_until_the_final_lane_is_known() {
    let f = Fixture::new();
    let d = f.daemon();
    setup_la(&d);
    let ws = f.repo("ws");
    let r = work_start(
        &d,
        &f,
        &ws,
        &["--cost", "build", "--chain", "deep", "--name", "chained"],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let id = work_job(&r)["id"].as_str().unwrap().to_string();
    assert_eq!(id, "la-r001-chained");
    let args = stored_args(&f, &id);
    assert_eq!(arg_after(&args, "--category"), "deep");

    let v = view(&d, "LA-R001");
    assert_eq!(v["by"], "chain deep");
    assert_eq!(v["chain"], "deep");
    assert_eq!(v["lane"], Value::Null, "{v}");
    assert_eq!(v["model"], Value::Null, "{v}");
    assert_eq!(v["declaredBy"], "daemon");

    // The poller sees the lane that is writing: one more `started` row, once.
    f.status(
        &id,
        &lane_status(
            &id,
            "running",
            json!({"final_lane": "qoder", "model": "Auto.Free"}),
        ),
    );
    let rs = wait_rows(&d, "LA-R001", 2);
    assert_eq!(rs[1]["state"], "started");
    assert_eq!(rs[1]["by"], "qoder Auto.Free");
    assert_eq!(rs[1]["declaredBy"], "daemon");
    let v = view(&d, "LA-R001");
    assert_eq!(v["lane"], "qoder");
    assert_eq!(v["model"], "auto-free");
    assert_eq!(v["modelRaw"], "Auto.Free");
    assert_eq!(v["chain"], "deep", "the chain is kept");
    assert_eq!(v["by"], "qoder Auto.Free");
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(
        rows(&d, "LA-R001").len(),
        2,
        "one row per distinct final lane"
    );
}

#[test]
fn a_failing_lane_leaves_the_started_row_and_a_failed_row() {
    let f = Fixture::new();
    std::fs::write(f.data().join("run-fail"), "").unwrap();
    let d = f.daemon();
    setup_la(&d);
    let ws = f.repo("ws");
    let r = work_start(
        &d,
        &f,
        &ws,
        &["--cost", "build", "--lane", "qoder", "--name", "doomed"],
    );
    assert_eq!(r.code, 1, "{}", r.stderr);
    // `lane run` was called (it copied the store) and the row was already there.
    let at_run = read(&f.data().join("la-r001-doomed.ids-at-run"));
    assert!(at_run.contains("\"id\":\"LA-R001\""), "{at_run}");
    assert!(at_run.contains("\"state\":\"started\""), "{at_run}");
    let rs = rows(&d, "LA-R001");
    let states: Vec<&str> = rs.iter().map(|r| r["state"].as_str().unwrap()).collect();
    assert_eq!(states, ["started", "failed"], "{rs:?}");
    assert!(rs[1]["result"].as_str().is_some(), "{}", rs[1]);
}

#[test]
fn a_read_only_store_refuses_the_start_and_lane_is_not_called() {
    let f = Fixture::new();
    let mut d = f.daemon();
    setup_la(&d);
    stop(&mut d);
    let file = ids_file(&d);
    let mut text = read(&file);
    text.push_str("not a row\n");
    std::fs::write(&file, &text).unwrap();
    start_again(&mut d, &f.env());
    let ws = f.repo("ws");
    let r = work_start(
        &d,
        &f,
        &ws,
        &["--cost", "build", "--lane", "qoder", "--name", "blocked"],
    );
    assert_ne!(r.code, 0);
    assert!(
        r.stderr
            .contains("the id store has an unreadable line (3); fix it by hand"),
        "{}",
        r.stderr
    );
    // The stand-in was never called.
    let called: Vec<_> = std::fs::read_dir(f.data())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".args") || n.ends_with(".ids-at-run"))
        .collect();
    assert!(called.is_empty(), "{called:?}");
    assert_eq!(read(&file), text);
    assert!(lagent_json(&d, &["work", "ls"])["jobs"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn work_refusals() {
    let f = Fixture::new();
    let d = f.daemon();
    setup_la(&d);
    lead_add(&d, "JS");
    let ws = f.repo("ws");
    let spec = f.spec();

    // No --change.
    let e = refused(
        &d,
        &[
            "work",
            "start",
            "--cost",
            "build",
            "--lead",
            "LA",
            "--lane",
            "qoder",
            "--workspace",
            &ws,
            "--spec",
            &spec,
        ],
    );
    assert!(e.contains("--change"), "{e}");
    // A change of another lead.
    let r = work_start(&d, &f, &ws, &["--cost", "build", "--lane", "qoder"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let r = lagent(
        &d,
        &[
            "work",
            "start",
            "--cost",
            "build",
            "--lead",
            "JS",
            "--change",
            "LA-C001",
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
        r.stderr
            .contains("change LA-C001 belongs to lead LA, not JS"),
        "{}",
        r.stderr
    );
    // An unregistered lead.
    let r = lagent(
        &d,
        &[
            "work",
            "start",
            "--cost",
            "build",
            "--lead",
            "ZZ",
            "--change",
            "ZZ-C001",
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
        r.stderr.contains("lead \"ZZ\" is not registered"),
        "{}",
        r.stderr
    );
    // A change that does not exist, and an id that is not a change.
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
            "LA-C099",
            "--lane",
            "qoder",
            "--workspace",
            &ws,
            "--spec",
            &spec,
        ],
    );
    assert!(r.stderr.contains("does not exist"), "{}", r.stderr);
    let run = new_run(&d, "LA", "hand run");
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
            &run,
            "--lane",
            "qoder",
            "--workspace",
            &ws,
            "--spec",
            &spec,
        ],
    );
    assert!(r.stderr.contains("is not a change"), "{}", r.stderr);
    // Only the one good start reached the stand-in.
    let runs = std::fs::read_dir(f.data())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".args"))
        .count();
    assert_eq!(runs, 1);
}

#[test]
fn a_trace_follows_a_finding_to_the_runs_and_a_commit_to_its_run() {
    let f = Fixture::new();
    let d = f.daemon();
    lead_add(&d, "LA");
    lead_add(&d, "JS");
    ok(&d, &["id", "area", "add", "GC", "--lead", "JS", "gc"]);
    assert_eq!(new_change(&d, "LA"), "LA-C001");
    assert_eq!(
        ok(
            &d,
            &["id", "new", "finding", "--area", "GC", "--title", "uaf"]
        ),
        "GC-01"
    );
    let repo = f.repo("repo");
    let base = git(&repo, &["rev-parse", "HEAD"]);
    let r = work_start(
        &d,
        &f,
        &repo,
        &["--cost", "build", "--lane", "qoder", "--name", "tracer"],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let run = work_job(&r)["runId"].as_str().unwrap().to_string();
    assert_eq!(run, "LA-R001");

    // The worker's commit, with the trailers; after the run started and on top of its base.
    let sha = commit(
        &repo,
        "ids: the writer",
        &[
            "Lead: LA",
            &format!("Run: {run}"),
            "Change: LA-C001",
            "Fixes: GC-01",
        ],
    );
    // The finding names the run that found it; the change names the finding it fixes.
    ok(&d, &["id", "state", "GC-01", "confirmed", "--refs", &run]);
    ok(
        &d,
        &["id", "state", "LA-C001", "reviewed", "--refs", "GC-01"],
    );

    // From the finding: the run under `found by`, the change under `fixed by`.
    let t = lagent_json(&d, &["trace", "GC-01"]);
    let links = t["links"].as_array().unwrap();
    let under = |relation: &str| -> Vec<String> {
        links
            .iter()
            .filter(|l| l["relation"] == relation)
            .map(|l| l["view"]["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(under("found by"), [run.as_str()], "{t}");
    assert_eq!(under("fixed by"), ["LA-C001"], "{t}");
    assert_eq!(under("written or reviewed by"), [run.as_str()], "{t}");
    let text = ok(&d, &["trace", "GC-01"]);
    assert!(text.starts_with("GC-01  confirmed"), "{text}");
    assert!(text.contains("found by\n  LA-R001"), "{text}");
    assert!(text.contains("fixed by\n  LA-C001"), "{text}");

    // From the run: its lane, model, brief and the commit.
    let t = lagent_json(&d, &["trace", &run]);
    let commits = t["subject"]["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 1, "{t}");
    assert_eq!(commits[0]["sha"], sha.as_str());
    assert_eq!(commits[0]["subject"], "ids: the writer");
    assert_eq!(t["subject"]["job"]["lane"], "qoder");
    let text = ok(&d, &["trace", &run]);
    assert!(text.contains(&format!("commit {}", &sha[..10])), "{text}");
    assert!(text.contains("lane qoder  model auto-free"), "{text}");
    assert!(text.contains("works for\n  LA-C001"), "{text}");

    // From the commit: the run, the lead, the change and the lane.
    let c = lagent_json(&d, &["trace", "--commit", &sha, "--repo", &repo]);
    assert_eq!(c["sha"], sha.as_str());
    assert_eq!(c["trailers"]["lead"], "LA");
    assert_eq!(c["trailers"]["run"], run.as_str());
    assert_eq!(c["trailers"]["change"], "LA-C001");
    assert_eq!(c["trailers"]["fixes"], json!(["GC-01"]));
    assert_eq!(c["missing"], json!([]));
    assert_eq!(c["runView"]["id"], run.as_str());
    assert_eq!(c["runView"]["lane"], "qoder");
    assert_eq!(c["runView"]["model"], "auto-free");
    assert_eq!(c["runView"]["base"], base.as_str());
    assert_eq!(c["runView"]["declaredBy"], "daemon");
    assert_eq!(c["descendsFromBase"], true, "{c}");
    assert_eq!(c["afterFirstRow"], true, "{c}");
    assert!(c["committerTime"].is_string());
    let text = ok(&d, &["trace", "--commit", &sha, "--repo", &repo]);
    assert!(text.contains("run        LA-R001"), "{text}");
    assert!(text.contains("lead       LA"), "{text}");
    assert!(text.contains("change     LA-C001"), "{text}");
    assert!(text.contains("lane       qoder"), "{text}");

    // A commit dated in the year 2000 is before the run's first row.
    let old = commit_at(
        &repo,
        "ids: dated",
        &["Lead: LA", &format!("Run: {run}"), "Change: LA-C001"],
        Some("2000-01-01T00:00:00Z"),
    );
    let c = lagent_json(&d, &["trace", "--commit", &old, "--repo", &repo]);
    assert_eq!(c["afterFirstRow"], false, "{c}");
    assert_eq!(c["descendsFromBase"], true, "{c}");

    // A commit on an unrelated root does not descend from the run's base.
    git(&repo, &["checkout", "-q", "--orphan", "unrelated"]);
    git(&repo, &["rm", "-rfq", "."]);
    let other = commit(
        &repo,
        "unrelated root",
        &["Lead: LA", &format!("Run: {run}"), "Change: LA-C001"],
    );
    let c = lagent_json(&d, &["trace", "--commit", &other, "--repo", &repo]);
    assert_eq!(c["descendsFromBase"], false, "{c}");
    assert_eq!(c["runView"]["id"], run.as_str());

    // A commit without trailers names what is missing.
    let plain = commit(&repo, "no trailers here", &[]);
    let c = lagent_json(&d, &["trace", "--commit", &plain, "--repo", &repo]);
    assert_eq!(c["missing"], json!(["Lead", "Run"]), "{c}");
    assert_eq!(c["run"], Value::Null);
    assert_eq!(c["runView"], Value::Null);
    let text = ok(&d, &["trace", "--commit", &plain, "--repo", &repo]);
    assert!(text.contains("missing    Lead, Run"), "{text}");
    // Only the Lead missing.
    let half = commit(&repo, "half", &[&format!("Run: {run}")]);
    let c = lagent_json(&d, &["trace", "--commit", &half, "--repo", &repo]);
    assert_eq!(c["missing"], json!(["Lead"]), "{c}");

    // Unknown ids and commits are errors.
    assert!(refused(&d, &["trace", "LA-R999"]).contains("no id LA-R999"));
    assert!(
        refused(&d, &["trace", "--commit", "deadbeef", "--repo", &repo]).contains("not a commit")
    );
    assert!(refused(&d, &["trace"]).contains("--commit"));
}

/// A state row through the API, with `base` as the column of the row: what a lead state row or
/// an imported ledger can hold.
fn state_with_base(d: &Daemon, id: &str, state: &str, base: &str) {
    let sock = d.socket().display().to_string();
    let body = json!({ "state": state, "base": base }).to_string();
    let r = common::run(Command::new("curl").args([
        "-s",
        "-X",
        "POST",
        "-H",
        "content-type: application/json",
        "-d",
        &body,
        "--unix-socket",
        &sock,
        &format!("http://x/ids/{id}/state"),
    ]));
    assert_eq!(r.code, 0, "{}", r.stderr);
    let v: Value = serde_json::from_str(&r.stdout).unwrap_or_else(|e| panic!("{e}: {}", r.stdout));
    assert_eq!(v["id"], id, "{v}");
    assert_eq!(v["base"], base, "{v}");
}

#[test]
fn only_a_full_commit_id_is_asked_of_git_as_the_base() {
    let f = Fixture::new();
    let d = f.daemon();
    setup_la(&d);
    let repo = f.repo("repo");
    let base = git(&repo, &["rev-parse", "HEAD"]);
    let r = work_start(
        &d,
        &f,
        &repo,
        &["--cost", "build", "--lane", "qoder", "--name", "Gitty"],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let run = work_job(&r)["runId"].as_str().unwrap().to_string();
    let sha = commit(
        &repo,
        "the worker's commit",
        &["Lead: LA", &format!("Run: {run}"), "Change: LA-C001"],
    );

    // The base the daemon wrote is a full commit id, so git really answers.
    let c = lagent_json(&d, &["trace", "--commit", &sha, "--repo", &repo]);
    assert_eq!(c["runView"]["base"], base.as_str(), "{c}");
    assert_eq!(c["descendsFromBase"], true, "{c}");

    // Anything else never reaches git: `--help` would make `git merge-base` print its help,
    // and a ref would be resolved to a commit that has nothing to do with the run's base.
    let upper = base.to_uppercase();
    let short = "0".repeat(39);
    for bad in ["--help", "HEAD", upper.as_str(), short.as_str()] {
        state_with_base(&d, &run, "verified", bad);
        let c = lagent_json(&d, &["trace", "--commit", &sha, "--repo", &repo]);
        assert_eq!(c["runView"]["id"], run.as_str(), "{c}");
        assert_eq!(c["runView"]["base"], bad, "{c}");
        assert_eq!(c["descendsFromBase"], Value::Null, "base {bad}: {c}");
    }

    // The full id again gets its answer: the guard is about the shape, not about the run.
    state_with_base(&d, &run, "accepted", &base);
    let c = lagent_json(&d, &["trace", "--commit", &sha, "--repo", &repo]);
    assert_eq!(c["descendsFromBase"], true, "{c}");
}

#[test]
fn a_lead_code_must_differ_from_every_area_code() {
    let d = Daemon::start();
    lead_add(&d, "LA");
    ok(&d, &["id", "area", "add", "GC", "--lead", "LA", "gc"]);

    // The area side has always refused a code that is a lead; the lead side now refuses a
    // code that is an area, or one id would be two kinds at once.
    let e = refused(&d, &["id", "lead", "add", "GC", "the gc chat"]);
    assert!(
        e.contains("\"GC\" is an area code: a lead code must differ from every area code"),
        "{e}"
    );
    assert_eq!(view(&d, "GC")["kind"], "area", "GC stayed an area");
    assert_eq!(
        view(&d, "GC")["by"],
        "LA",
        "the area kept its owning lead, not a lead's title"
    );

    // The import route refuses it too and writes nothing.
    let before = read(&ids_file(&d));
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lead.tsv");
    std::fs::write(
        &file,
        "id\tutc\tkind\tstate\ttitle\nGC\t2026-11-13T00:00:00Z\tlead\tregistered\tthe gc chat\n",
    )
    .unwrap();
    let e = refused(&d, &["id", "import", &file.display().to_string()]);
    assert!(e.contains("\"GC\" is an area code"), "{e}");
    assert_eq!(read(&ids_file(&d)), before, "the import wrote nothing");
}

#[test]
fn a_lost_run_that_ends_failed_again_gets_a_second_failed_row() {
    let f = Fixture::new();
    let mut env = f.env();
    env.push(("LUCINA_AGENTD_WORK_LOST_AFTER_SECS", "1".to_string()));
    let d = Daemon::start_with_env(&env);
    std::fs::write(
        f.data().join("ids-path"),
        ids_file(&d).display().to_string(),
    )
    .unwrap();
    setup_la(&d);
    let ws = f.repo("ws");
    let job = work_job(&work_start(
        &d,
        &f,
        &ws,
        &["--cost", "build", "--lane", "qoder", "--name", "Again"],
    ));
    let id = job["id"].as_str().unwrap().to_string();
    assert_eq!(id, "la-r001-again", "{job}");
    assert_eq!(job["runId"], "LA-R001");

    // `lane` never answers for the run: the job is lost and its `failed` row is written.
    let rs = wait_rows(&d, "LA-R001", 2);
    assert_eq!(rs[1]["state"], "failed", "{rs:?}");
    assert_eq!(lagent_json(&d, &["work", "show", &id])["state"], "lost");

    // `lane` knows the run again: a running job has no run state, so no row is written.
    f.status(&id, &lane_status(&id, "running", json!({})));
    wait_until("the job runs again", || {
        lagent_json(&d, &["work", "show", &id])["state"] == "running"
    });
    assert_eq!(rows(&d, "LA-R001").len(), 2, "the recovery wrote a row");

    // It ends `failed` again: the new ending has its own row, with its own result.
    f.status(
        &id,
        &lane_status(&id, "failed", json!({"note": "the runner died"})),
    );
    let rs = wait_rows(&d, "LA-R001", 3);
    let states: Vec<&str> = rs.iter().map(|r| r["state"].as_str().unwrap()).collect();
    assert_eq!(states, ["started", "failed", "failed"], "{rs:?}");
    assert_eq!(rs[2]["declaredBy"], "daemon", "{rs[2]}");
    assert!(
        rs[2]["result"]
            .as_str()
            .is_some_and(|r| r.contains("failed")),
        "{}",
        rs[2]
    );
}

#[test]
fn a_final_job_is_followed_until_the_store_has_its_end_row() {
    let f = Fixture::new();
    let mut d = f.daemon();
    setup_la(&d);
    let ws = f.repo("ws");
    let head = git(&ws, &["rev-parse", "HEAD"]);
    let job = work_job(&work_start(
        &d,
        &f,
        &ws,
        &["--cost", "build", "--lane", "qoder", "--name", "Owed"],
    ));
    let id = job["id"].as_str().unwrap().to_string();
    assert_eq!(job["runId"], "LA-R001");
    assert_eq!(rows(&d, "LA-R001").len(), 1);

    // The run ends while the store cannot be written: the row is refused, and the job says so.
    stop(&mut d);
    let file = ids_file(&d);
    let good = read(&file);
    let broken = format!("{good}not a row\n");
    std::fs::write(&file, &broken).unwrap();
    f.status(
        &id,
        &lane_status(
            &id,
            "verified",
            json!({"final_lane": "qoder", "model": "auto-free",
                   "verify": {"cmd": "true", "ran_cmd": null, "exit_code": 0, "head": "deadbeef",
                              "ran": true, "timed_out": false, "note": null},
                   "attempts": [{"lane": "qoder", "outcome": "ok", "reason": null,
                                 "before_head": head, "after_head": null}]}),
        ),
    );
    start_again(&mut d, &f.env());

    wait_until("the note names the refused end row", || {
        lagent_json(&d, &["work", "show", &id])["note"]
            .as_str()
            .is_some_and(|n| n.starts_with("the id store refused the end row: "))
    });
    let shown = lagent_json(&d, &["work", "show", &id]);
    let note = shown["note"].as_str().unwrap_or_default().to_string();
    assert!(
        note.contains("the id store has an unreadable line (4); fix it by hand"),
        "{note}"
    );
    assert_eq!(shown["state"], "verified", "{shown}");
    assert_eq!(rows(&d, "LA-R001").len(), 1, "the store took nothing");
    assert_eq!(read(&file), broken, "the file is byte-identical");

    // The bad line is gone and the daemon starts again: the final job is still followed, so
    // the row it owed is written although `lane` has nothing new to say.
    stop(&mut d);
    std::fs::write(&file, &good).unwrap();
    start_again(&mut d, &f.env());
    let rs = wait_rows(&d, "LA-R001", 2);
    assert_eq!(rs[1]["state"], "verified", "{rs:?}");
    assert_eq!(rs[1]["declaredBy"], "daemon", "{}", rs[1]);
    assert!(
        lagent_json(&d, &["work", "show", &id])["state"] == "verified",
        "the job moved on for the row alone"
    );
}
