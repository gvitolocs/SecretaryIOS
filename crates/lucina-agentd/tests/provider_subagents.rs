//! Parity row P7: a provider's subagents have timelines of their own. The daemon runs the real
//! Claude provider against a fake `claude` (a shell script on `PATH` that answers the first
//! user message with a stream-json run, `fixtures/claude-task-subagent.jsonl`: Claude Code
//! 2.1.310's `init` frame, then the task-protocol and sidechain frames of a `Task` subagent that runs
//! `ls`, as Paseo reads them (assembled by hand; a live capture was not possible, the quota was out). The checks go through the
//! unix socket and `lagent`: the routes, `logs`, `inspect`, the event stream, restart,
//! archive and delete.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{Daemon, Guard, AGENTD};
use serde_json::Value;

const FIXTURE: &str = include_str!("fixtures/claude-task-subagent.jsonl");
const TASK: &str = "toolu_01TaskListFiles";

/// Answers the first user message with the first `lines` of the fixture, then waits for stdin to
/// close (the daemon going away), like a Claude that is still alive.
fn fake_claude(dir: &Path, lines: usize) {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("stream.jsonl"), FIXTURE).unwrap();
    let script = format!("#!/bin/sh\nread -r first\nsleep 0.3\nhead -n {lines} \"$(dirname \"$0\")/stream.jsonl\"\nwhile read -r line; do :; done\n");
    std::fs::write(bin.join("claude"), script).unwrap();
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A daemon with the real providers it finds: only the fake `claude` (`HOME` is empty and
/// `PATH` has nothing else).
fn spawn(dir: &Path) -> Guard {
    let socket = dir.join("agentd.sock");
    let home = dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let child = Command::new(AGENTD)
        .args(["--state-dir", &dir.join("state").display().to_string(), "--socket", &socket.display().to_string()])
        .env("HOME", &home)
        .env("PATH", format!("{}:/usr/bin:/bin", dir.join("bin").display()))
        .env("LUCINA_AGENTD_DEV", "1")
        .env("RUST_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(dir.join("agentd.log")).map(Stdio::from).unwrap())
        .process_group(0)
        .spawn()
        .expect("spawn lucina-agentd");
    let guard = Guard { child: Some(child) };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "lucina-agentd did not create its socket");
        std::thread::sleep(Duration::from_millis(20));
    }
    guard
}

fn start(lines: usize) -> Daemon {
    let dir = tempfile::tempdir().unwrap();
    fake_claude(dir.path(), lines);
    std::fs::create_dir_all(dir.path().join("work")).unwrap();
    let guard = spawn(dir.path());
    Daemon { dir, guard }
}

fn restart(d: &mut Daemon) {
    assert_eq!(d.guard.terminate(Duration::from_secs(10)), Some(0), "clean exit on SIGTERM");
    d.guard = spawn(d.dir.path());
}

fn http(d: &Daemon, method: &str, path: &str) -> (u16, Value) {
    let mut s = UnixStream::connect(d.socket()).expect("connect");
    let body = if method == "GET" { String::new() } else { "{}".to_string() };
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n");
    if method != "GET" {
        req.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    req.push_str(&body);
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let (head, rest) = raw.split_once("\r\n\r\n").expect("response");
    let status: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
    let text = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        rest.split_once("\r\n").map(|(_, r)| r.rsplit_once("\r\n0\r\n").map(|(b, _)| b).unwrap_or(r)).unwrap_or("").to_string()
    } else {
        rest.to_string()
    };
    (status, serde_json::from_str(text.trim()).unwrap_or(Value::Null))
}

fn get(d: &Daemon, path: &str) -> Value {
    let (status, v) = http(d, "GET", path);
    assert_eq!(status, 200, "GET {path}: {v}");
    v
}

/// The JSON at `path`, or `Null` while it is a 404.
fn try_get(d: &Daemon, path: &str) -> Value {
    let (status, v) = http(d, "GET", path);
    if status == 200 { v } else { Value::Null }
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn run(d: &Daemon, no_wait: bool) -> String {
    let cwd = d.dir.path().join("work").display().to_string();
    let mut args = vec!["run", "--provider", "claude", "--mode", "bypassPermissions", "--cwd", &cwd];
    if no_wait {
        args.push("--no-wait");
    }
    args.push("Have a subagent list the files.");
    let v = d.json(&args);
    v["agentId"].as_str().expect("agentId").to_string()
}

/// `kind: text` for each row of a child's timeline.
fn describe(rows: &Value) -> Vec<String> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let i = &r["item"];
            match i["type"].as_str().unwrap() {
                "user_message" | "assistant_message" | "reasoning" => format!("{}: {}", i["type"].as_str().unwrap(), i["text"].as_str().unwrap()),
                "tool_call" => format!("tool_call {} {} {}", i["callId"].as_str().unwrap(), i["name"].as_str().unwrap(), i["status"].as_str().unwrap()),
                other => other.to_string(),
            }
        })
        .collect()
}

#[test]
fn a_task_subagent_has_its_own_timeline() {
    let d = start(18);
    // Watch the event stream from before the run.
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    {
        let mut s = UnixStream::connect(d.socket()).unwrap();
        write!(s, "GET /stream HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let seen = seen.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(s).lines().map_while(Result::ok) {
                if let Some(data) = line.strip_prefix("data:") {
                    if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                        seen.lock().unwrap().push(v);
                    }
                }
            }
        });
    }
    let id = run(&d, false);

    // The descriptor.
    let list = get(&d, &format!("/agents/{id}/provider-subagents"));
    assert_eq!(list["agentId"], id.as_str());
    let subs = list["subagents"].as_array().unwrap();
    assert_eq!(subs.len(), 1, "{list}");
    let sub = &subs[0];
    assert_eq!(sub["id"], TASK);
    assert_eq!(sub["parentAgentId"], id.as_str());
    assert_eq!(sub["parentSubagentId"], Value::Null);
    assert_eq!(sub["provider"], "claude");
    assert_eq!(sub["title"], "general-purpose");
    assert_eq!(sub["description"], "List files");
    assert_eq!(sub["status"], "completed");
    assert_eq!(sub["toolCallId"], TASK);
    assert_eq!(sub["subtitle"], "general-purpose · claude-opus-5-5 · 5.1k tokens");

    // Its timeline, by id, by prefix, and as a filter on the agent's timeline.
    let tl = get(&d, &format!("/agents/{id}/provider-subagents/{TASK}/timeline"));
    assert_eq!(
        describe(&tl["rows"]),
        [
            "user_message: Run `ls` in the current directory with Bash and report the file names in one short sentence.",
            "assistant_message: I'll list the directory.",
            "tool_call toolu_01SubBash Bash running",
            "tool_call toolu_01SubBash Bash completed",
            "assistant_message: The directory has a.txt and b.txt.",
        ]
    );
    let seqs: Vec<u64> = tl["rows"].as_array().unwrap().iter().map(|r| r["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, [1, 2, 3, 4, 5]);
    assert_eq!((tl["nextSeq"].as_u64(), tl["window"]["minSeq"].as_u64(), tl["window"]["maxSeq"].as_u64()), (Some(5), Some(1), Some(5)));
    assert_eq!(tl["epoch"], get(&d, &format!("/agents/{id}/timeline"))["epoch"]);
    assert_eq!(tl["subagent"]["status"], "completed");
    let tail = get(&d, &format!("/agents/{id}/provider-subagents/toolu_01Task/timeline?direction=tail&limit=2"));
    assert_eq!(describe(&tail["rows"]).len(), 2);
    assert_eq!((tail["hasOlder"].as_bool(), tail["hasNewer"].as_bool()), (Some(true), Some(false)));
    let page = get(&d, &format!("/agents/{id}/timeline?subagent={TASK}&afterSeq=3"));
    assert_eq!(describe(&page["rows"]).len(), 2);
    assert_eq!(get(&d, &format!("/agents/{id}/provider-subagents/{TASK}"))["id"], TASK);
    let (status, err) = http(&d, "GET", &format!("/agents/{id}/provider-subagents/nope"));
    assert_eq!((status, err["error"]["code"].as_str()), (404, Some("PROVIDER_SUBAGENT_NOT_FOUND")));
    let (status, _) = http(&d, "GET", &format!("/agents/{id}/provider-subagents/{TASK}/timeline?direction=sideways"));
    assert_eq!(status, 400);

    // The agent's own timeline holds none of the child's items, only the Task card.
    let main = get(&d, &format!("/agents/{id}/timeline"));
    let main_rows = describe(&main["rows"]);
    assert!(main_rows.iter().all(|r| !r.contains("I'll list the directory") && !r.contains("toolu_01SubBash")), "{main_rows:?}");
    assert!(main_rows.iter().any(|r| r == &format!("tool_call {TASK} Task completed")), "{main_rows:?}");

    // `lagent logs` shows the card with what the child did; `--subagent` shows the child itself.
    let logs = d.lagent(&["logs", &id]);
    assert_eq!(logs.code, 0, "{}", logs.stderr);
    assert!(logs.stdout.contains("[Bash] ls\nThe directory has a.txt and b.txt."), "{}", logs.stdout);
    assert!(!logs.stdout.contains("I'll list the directory"), "{}", logs.stdout);
    let sub_logs = d.lagent(&["logs", &id, "--subagent", "toolu_01Task"]);
    assert_eq!(sub_logs.code, 0, "{}", sub_logs.stderr);
    let lines: Vec<&str> = sub_logs.stdout.trim_end().lines().collect();
    assert_eq!(
        lines,
        [
            "[User] Run `ls` in the current directory with Bash and report the file names in one short sentence.",
            "I'll list the directory.",
            "[Shell] ls",
            "The directory has a.txt and b.txt.",
        ]
    );
    assert_eq!(d.lagent(&["logs", &id, "--subagent", "zzz"]).code, 1);

    // `lagent inspect` lists the children (Lucina only: the key is absent without any).
    let inspect = d.json(&["inspect", &id]);
    assert_eq!(inspect["Subagents"][0]["Id"], TASK);
    assert_eq!(inspect["Subagents"][0]["Status"], "completed");
    let text = d.lagent(&["inspect", &id]);
    assert!(text.stdout.contains(&format!("{TASK} (general-purpose, completed): List files")), "{}", text.stdout);

    // The event stream carried the descriptor, each timeline item once, and the status changes.
    wait_for("the stream to deliver the child's last status", || {
        seen.lock().unwrap().iter().any(|e| e["type"] == "provider_subagent" && e["event"]["kind"] == "upsert" && e["event"]["subagent"]["status"] == "completed")
    });
    let events: Vec<Value> = seen.lock().unwrap().iter().filter(|e| e["type"] == "provider_subagent").map(|e| e["event"].clone()).collect();
    let timeline: Vec<&Value> = events.iter().filter(|e| e["kind"] == "timeline").collect();
    assert_eq!(timeline.len(), 5);
    assert_eq!(timeline[0]["subagentId"], TASK);
    assert_eq!(timeline[0]["parentAgentId"], id.as_str());
    assert_eq!(timeline.iter().map(|e| e["seq"].as_u64().unwrap()).collect::<Vec<_>>(), [1, 2, 3, 4, 5]);
    assert_eq!(timeline[0]["epoch"], tl["epoch"]);
    // None of them came through as an `agent_stream` timeline event of the parent.
    let leaked = seen.lock().unwrap().iter().filter(|e| e["type"] == "agent_stream" && e["event"]["type"] == "timeline").filter(|e| e["event"]["item"].to_string().contains("I'll list the directory")).count();
    assert_eq!(leaked, 0);
}

#[test]
fn children_survive_a_restart_and_a_running_one_is_canceled() {
    // The fake Claude stops after the child's first tool call and waits.
    let mut d = start(11);
    let id = run(&d, true);
    wait_for("the child's tool call", || {
        try_get(&d, &format!("/agents/{id}/provider-subagents/{TASK}/timeline"))["rows"].as_array().is_some_and(|r| r.len() == 3)
    });
    let running = get(&d, &format!("/agents/{id}/provider-subagents/{TASK}"));
    assert_eq!(running["status"], "running");
    let before = get(&d, &format!("/agents/{id}/provider-subagents/{TASK}/timeline"));
    // The card in the parent shows what the child has done so far.
    let logs = d.lagent(&["logs", &id]);
    assert!(logs.stdout.contains("[Bash] ls"), "{}", logs.stdout);

    restart(&mut d);
    let after = get(&d, &format!("/agents/{id}/provider-subagents/{TASK}/timeline"));
    assert_eq!(after["rows"], before["rows"], "the rows are stored");
    assert_eq!(after["epoch"], before["epoch"]);
    assert_eq!(after["subagent"]["status"], "canceled", "its Claude process died with the daemon");
    assert_eq!(after["subagent"]["title"], "general-purpose");
    assert_eq!(after["subagent"]["createdAt"], before["subagent"]["createdAt"]);
}

#[test]
fn archiving_cancels_a_running_child_and_deleting_removes_it() {
    let d = start(11);
    let id = run(&d, true);
    wait_for("the child to run", || try_get(&d, &format!("/agents/{id}/provider-subagents"))["subagents"].as_array().is_some_and(|s| s.len() == 1));
    assert_eq!(get(&d, &format!("/agents/{id}/provider-subagents/{TASK}"))["status"], "running");
    let archived = d.lagent(&["archive", "--force", &id]);
    assert_eq!(archived.code, 0, "{}{}", archived.stdout, archived.stderr);
    assert_eq!(get(&d, &format!("/agents/{id}/provider-subagents/{TASK}"))["status"], "canceled");
    assert_eq!(d.lagent(&["delete", &id]).code, 0);
    let (status, _) = http(&d, "GET", &format!("/agents/{id}/provider-subagents"));
    assert_eq!(status, 404);
    // Its rows are gone from the database too: a new agent starts with none.
    let id2 = run(&d, true);
    wait_for("its three rows", || try_get(&d, &format!("/agents/{id2}/provider-subagents/{TASK}/timeline"))["rows"].as_array().is_some_and(|r| r.len() == 3));
    assert_eq!(get(&d, &format!("/agents/{id2}/provider-subagents/{TASK}/timeline"))["rows"].as_array().unwrap().len(), 3);
}
