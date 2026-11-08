//! lucina-agentd end to end through `lagent`, with the mock provider: the phase 1
//! scenarios, restart, idempotency and shutdown. (The same scenarios against Paseo
//! are in `paseo_diff.rs`.)

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use common::Daemon;
use serde_json::{json, Value};

fn id_of(v: &Value) -> String {
    v["agentId"].as_str().expect("agentId").to_string()
}

#[test]
fn run_logs_inspect() {
    let d = Daemon::start();
    let run = d.json(&["run", "--provider", "mock/e2e-fast-stream", "--title", "t1", "--cwd", "/tmp", "hello"]);
    assert_eq!(run["status"], "completed");
    assert_eq!(run["provider"], "mock");
    assert_eq!(run["title"], "t1");
    let id = id_of(&run);

    let logs = d.lagent(&["logs", &id]);
    assert_eq!(logs.code, 0);
    let lines: Vec<&str> = logs.stdout.trim_end().lines().collect();
    assert_eq!(lines.first(), Some(&"[User] hello"));
    assert_eq!(lines.last(), Some(&"_(end of synthetic stream)_"));
    assert_eq!(lines.get(1), Some(&"## Cycle 1"));

    let inspect = d.json(&["inspect", &id[..7]]);
    assert_eq!(inspect["Status"], "idle");
    assert_eq!(inspect["Model"], "e2e-fast-stream");
    assert_eq!(inspect["Mode"], "load-test");
    assert_eq!(inspect["Capabilities"], json!({"Streaming": true, "Persistence": true, "DynamicModes": false, "McpServers": false}));
    assert!(inspect["LastUsage"]["OutputTokens"].as_u64().unwrap() > 0);
    assert_eq!(inspect["PendingPermissions"], json!([]));

    let ls = d.json(&["ls"]);
    assert_eq!(ls[0]["provider"], "mock/e2e-fast-stream");
    assert_eq!(ls[0]["name"], "t1");
    assert_eq!(ls[0]["shortId"], &id[..7]);
}

#[test]
fn turn_failure() {
    let d = Daemon::start();
    let run = d.json(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "emit synthetic turn failure"]);
    assert_eq!(run["status"], "error");
    assert_eq!(run["title"], "emit synthetic turn failure");
    let logs = d.lagent(&["logs", &id_of(&run)]);
    assert_eq!(logs.stdout.trim_end(), "[User] emit synthetic turn failure\n[System Error] Requested mock provider failure");
    assert_eq!(d.json(&["inspect", &id_of(&run)])["Status"], "error");
}

#[test]
fn plan_approval_then_stop() {
    let d = Daemon::start();
    let run = d.json(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "emit synthetic plan approval"]);
    assert_eq!(run["status"], "permission");
    let id = id_of(&run);
    let inspect = d.json(&["inspect", &id]);
    let pending = &inspect["PendingPermissions"][0];
    assert!(pending["id"].as_str().unwrap().starts_with("mock-plan-"));
    assert_eq!(pending["tool"], "MockPlanApproval");
    let stop = d.json(&["stop", &id]);
    assert_eq!(stop, json!({"stoppedCount": 1, "agentIds": [id]}));
    let after = d.json(&["inspect", &id]);
    assert_eq!(after["Status"], "idle");
    assert_eq!(after["PendingPermissions"], json!([]));
    assert_eq!(d.lagent(&["logs", &id]).stdout.trim_end(), "[User] emit synthetic plan approval");
}

#[test]
fn send_interrupts_and_wait() {
    let d = Daemon::start();
    let run = d.json(&["run", "--provider", "mock/ten-second-stream", "--cwd", "/tmp", "--no-wait", "first task"]);
    assert!(run["status"] == "running" || run["status"] == "created");
    let id = id_of(&run);
    std::thread::sleep(Duration::from_millis(500));
    let send = d.json(&["send", &id, "second task"]);
    assert_eq!(send["status"], "completed");
    assert_eq!(send["message"], "Agent completed processing the message");
    let wait = d.json(&["wait", &id]);
    assert_eq!(wait["status"], "idle");
    assert!(wait["message"].as_str().unwrap().starts_with("Agent is idle.\nLast 5 activity items:\n"));
    let logs = d.lagent(&["logs", &id]).stdout;
    let users: Vec<&str> = logs.lines().filter(|l| l.starts_with("[User]")).collect();
    assert_eq!(users, ["[User] first task", "[User] second task"]);
}

#[test]
fn archive_ls_delete() {
    let d = Daemon::start();
    let run = d.json(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "hello"]);
    let id = id_of(&run);
    let archived = d.json(&["archive", &id]);
    assert_eq!(archived["status"], "archived");
    assert!(archived["archivedAt"].is_string());
    assert_eq!(d.json(&["ls"]), json!([]));
    assert_eq!(d.json(&["ls", "-a"]).as_array().unwrap().len(), 1);
    let again = d.lagent(&["archive", &id, "--json"]);
    assert_eq!(again.code, 1);
    assert!(again.stderr.contains("AGENT_ALREADY_ARCHIVED"));
    let deleted = d.json(&["delete", &id]);
    assert_eq!(deleted, json!({"deletedCount": 1, "agentIds": [id]}));
    let gone = d.lagent(&["inspect", &id, "--json"]);
    assert_eq!(gone.code, 1);
    let err: Value = serde_json::from_str(&gone.stderr).unwrap();
    assert_eq!(err["error"]["code"], "AGENT_NOT_FOUND");
    assert_eq!(err["error"]["message"], format!("Agent not found: {id}"));
}

#[test]
fn restart_keeps_timeline_and_closes_agents() {
    let mut d = Daemon::start();
    let run = d.json(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "hello"]);
    let id = id_of(&run);
    let before = d.lagent(&["logs", &id]).stdout;
    let running = d.json(&["run", "--provider", "mock/ten-second-stream", "--cwd", "/tmp", "--no-wait", "long"]);
    d.restart();
    let ls = d.json(&["ls"]);
    assert!(ls.as_array().unwrap().iter().all(|a| a["status"] == "closed"), "{ls}");
    assert_eq!(d.lagent(&["logs", &id]).stdout, before, "the timeline survives a restart");
    let interrupted = d.lagent(&["logs", &id_of(&running)]).stdout;
    assert!(interrupted.starts_with("[User] long"));
    // A closed agent resumes on the next message.
    let send = d.json(&["send", &id, "/mock handled-command"]);
    assert_eq!(send["status"], "completed");
    assert!(d.lagent(&["logs", &id]).stdout.trim_end().ends_with("Mock command handled"));
}

fn http(d: &Daemon, method: &str, path: &str, body: &Value) -> Value {
    let mut s = UnixStream::connect(d.socket()).unwrap();
    let b = body.to_string();
    write!(s, "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}", b.len()).unwrap();
    let mut out = String::new();
    std::io::Read::read_to_string(&mut s, &mut out).unwrap();
    let body = out.split("\r\n\r\n").nth(1).unwrap_or_default();
    serde_json::from_str(body).unwrap_or(Value::Null)
}

#[test]
fn idempotent_create() {
    let d = Daemon::start();
    let req = json!({"provider": "mock/e2e-fast-stream", "cwd": "/tmp", "idempotencyKey": "k-1"});
    let a = http(&d, "POST", "/agents", &req);
    let b = http(&d, "POST", "/agents", &req);
    assert!(a["id"].is_string());
    assert_eq!(a["id"], b["id"]);
    assert_eq!(d.json(&["ls"]).as_array().unwrap().len(), 1);
}

#[test]
fn sigterm_exits_cleanly_with_open_stream_and_running_agent() {
    let mut d = Daemon::start();
    d.json(&["run", "--provider", "mock/ten-second-stream", "--cwd", "/tmp", "--no-wait", "busy"]);
    let mut s = UnixStream::connect(d.socket()).unwrap();
    write!(s, "GET /stream HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let mut first = String::new();
    BufReader::new(s.try_clone().unwrap()).read_line(&mut first).unwrap();
    assert!(first.starts_with("HTTP/1.1 200"));
    let start = Instant::now();
    assert_eq!(d.guard.terminate(Duration::from_secs(5)), Some(0));
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(!d.socket().exists(), "socket removed on exit");
}
