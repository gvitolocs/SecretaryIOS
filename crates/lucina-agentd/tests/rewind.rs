//! Rewind and fork end to end against a real daemon with the mock provider (parity row
//! A16): the three rewind modes on files and conversation, a running turn cancelled
//! first, the stored timeline and the stream event, the worktree guard, who may rewind
//! and fork what (the owner, an MCP token, an agent's own process), and the fork's chat
//! history ahead of its first prompt. The capability matrix per provider flag is in
//! `rewind_manager.rs`; the fork attachment against Paseo's own code is in lucina-agents'
//! `paseo_fork_context.rs`.

mod common;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{Daemon, Run, LAGENT};
use serde_json::{json, Value};

const FAST: &str = "mock/e2e-fast-stream";

fn call(d: &Daemon, method: &str, path: &str, origin: Option<&str>, body: Option<&Value>) -> (u16, Value) {
    let mut s = UnixStream::connect(d.socket()).expect("connect");
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n");
    if let Some(o) = origin {
        req.push_str(&format!("X-Lucina-Origin: {o}\r\n"));
    }
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

fn ok(d: &Daemon, method: &str, path: &str, body: Option<&Value>) -> Value {
    let (status, v) = call(d, method, path, None, body);
    assert!(status < 300, "{method} {path}: {status} {v}");
    v
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn status(d: &Daemon, id: &str) -> String {
    ok(d, "GET", &format!("/agents/{id}"), None)["status"].as_str().unwrap().to_string()
}

fn settle(d: &Daemon, id: &str) {
    wait_until("the agent to be idle", 20, || status(d, id) == "idle");
}

fn rows(d: &Daemon, id: &str) -> Vec<Value> {
    ok(d, "GET", &format!("/agents/{id}/timeline"), None)["rows"].as_array().cloned().unwrap_or_default()
}

fn epoch(d: &Daemon, id: &str) -> String {
    ok(d, "GET", &format!("/agents/{id}/timeline"), None)["epoch"].as_str().unwrap().to_string()
}

/// The ids of the agent's user messages, oldest first (the prompt index).
fn users(d: &Daemon, id: &str) -> Vec<String> {
    ok(d, "GET", &format!("/agents/{id}/prompts"), None)["prompts"].as_array().unwrap().iter().map(|p| p["messageId"].as_str().unwrap().to_string()).collect()
}

fn say(d: &Daemon, id: &str, prompt: &str) {
    ok(d, "POST", &format!("/agents/{id}/send"), Some(&json!({"prompt": prompt})));
    // The answer is in once the agent is idle with the prompt on its timeline.
    wait_until("the answer", 20, || status(d, id) == "idle" && rows(d, id).iter().any(|r| r["item"]["type"] == "user_message" && r["item"]["text"].as_str().is_some_and(|t| t.ends_with(prompt))));
}

fn start(d: &Daemon, cwd: &Path, prompt: &str, extra: Value) -> String {
    let mut body = json!({"provider": FAST, "cwd": cwd, "prompt": prompt});
    for (k, v) in extra.as_object().unwrap() {
        body[k] = v.clone();
    }
    let snap = ok(d, "POST", "/agents", Some(&body));
    let id = snap["id"].as_str().unwrap().to_string();
    settle(d, &id);
    id
}

/// An agent that wrote `a.txt` (new), rewrote it, then wrote `b.txt` (new), one prompt each.
fn writer(d: &Daemon, cwd: &Path, extra: Value) -> (String, Vec<String>) {
    let id = start(d, cwd, "lucina mock write a.txt=one", extra);
    say(d, &id, "lucina mock write a.txt=two");
    say(d, &id, "lucina mock write b.txt=three");
    let ids = users(d, &id);
    assert_eq!(ids.len(), 3, "{ids:?}");
    (id, ids)
}

fn rewind(d: &Daemon, id: &str, message: &str, mode: &str) -> (u16, Value) {
    call(d, "POST", &format!("/agents/{id}/rewind"), None, Some(&json!({"messageId": message, "mode": mode})))
}

fn read(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name)).ok()
}

/// The stream's raw text so far, read until `what` shows or the time is up.
fn read_stream_until(s: &mut UnixStream, what: &str) -> String {
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut seen = String::new();
    let mut buf = [0u8; 8192];
    while !seen.contains(what) {
        let n = s.read(&mut buf).unwrap_or(0);
        assert!(n > 0, "the stream ended before {what}: {seen}");
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    seen
}

#[test]
fn conversation_rewind_takes_the_timeline_back_and_leaves_the_files() {
    let d = Daemon::start();
    let dir = tempfile::tempdir().unwrap();
    let (id, ids) = writer(&d, dir.path(), json!({}));
    let (rows_before, epoch_before) = (rows(&d, &id), epoch(&d, &id));
    let seq_of_second = rows_before.iter().find(|r| r["item"]["messageId"] == ids[1].as_str()).unwrap()["seq"].as_u64().unwrap();

    let mut stream = UnixStream::connect(d.socket()).unwrap();
    stream.write_all(b"GET /stream HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    read_stream_until(&mut stream, "200 OK");

    let (status, out) = rewind(&d, &id, &ids[1], "conversation");
    assert_eq!(status, 200, "{out}");
    assert_eq!((out["ok"].as_bool(), out["mode"].as_str(), out["agentId"].as_str()), (Some(true), Some("conversation"), Some(id.as_str())));
    let after = rows(&d, &id);
    assert_eq!(out["removedRows"], (rows_before.len() - after.len()) as u64);
    assert!(after.iter().all(|r| r["seq"].as_u64().unwrap() < seq_of_second), "the target and what followed are gone");
    assert_eq!(users(&d, &id), vec![ids[0].clone()]);
    assert_ne!(epoch(&d, &id), epoch_before);
    assert_eq!(out["epoch"], epoch(&d, &id).as_str());
    assert_eq!(read(dir.path(), "a.txt").as_deref(), Some("two"), "files stay");
    assert_eq!(read(dir.path(), "b.txt").as_deref(), Some("three"));
    assert_eq!(status_of(&d, &id), "idle");

    let seen = read_stream_until(&mut stream, "timeline_replacement");
    assert!(seen.contains(&format!("\"agentId\":\"{id}\"")) && seen.contains(&epoch(&d, &id)), "{seen}");

    // The rewound agent goes on from there; the removed message is no longer a target.
    say(&d, &id, "lucina mock write c.txt=four");
    let numbered: Vec<u64> = rows(&d, &id).iter().map(|r| r["seq"].as_u64().unwrap()).collect();
    assert_eq!(numbered, (1..=numbered.len() as u64).collect::<Vec<_>>(), "sequence numbers continue without a gap");
    let (status, gone) = rewind(&d, &id, &ids[1], "conversation");
    assert_eq!((status, code(&gone)), (404, "REWIND_TARGET_NOT_FOUND"), "{gone}");

    // It is stored: after a restart the rows and the epoch are the rewound ones.
    let (stored_rows, stored_epoch) = (rows(&d, &id).len(), epoch(&d, &id));
    let mut d = d;
    d.restart();
    assert_eq!((rows(&d, &id).len(), epoch(&d, &id)), (stored_rows, stored_epoch));
}

fn status_of(d: &Daemon, id: &str) -> String {
    status(d, id)
}

#[test]
fn files_rewind_restores_files_and_keeps_the_conversation() {
    let d = Daemon::start();
    let dir = tempfile::tempdir().unwrap();
    let (id, ids) = writer(&d, dir.path(), json!({}));
    let (rows_before, epoch_before) = (rows(&d, &id), epoch(&d, &id));

    // To the second message: what the second and third turns wrote goes back.
    let (status, out) = rewind(&d, &id, &ids[1], "files");
    assert_eq!(status, 200, "{out}");
    assert_eq!(out["removedRows"], 0);
    assert_eq!(read(dir.path(), "a.txt").as_deref(), Some("one"));
    assert_eq!(read(dir.path(), "b.txt"), None, "a file the turns created is removed");
    assert_eq!((rows(&d, &id).len(), epoch(&d, &id)), (rows_before.len(), epoch_before.clone()));
    assert_eq!(users(&d, &id), ids);

    // To the first message: the first turn's file goes too. Doing it twice is harmless.
    for _ in 0..2 {
        assert_eq!(rewind(&d, &id, &ids[0], "files").0, 200);
        assert_eq!(read(dir.path(), "a.txt"), None);
    }
    assert_eq!(epoch(&d, &id), epoch_before);

    // The CLI says the same: a number, `last`, an id prefix.
    std::fs::write(dir.path().join("a.txt"), "edited by hand").unwrap();
    let r = d.json(&["rewind", &id, "--to", "1", "--mode", "files"]);
    assert_eq!((r["mode"].as_str(), r["messageId"].as_str()), (Some("files"), Some(ids[0].as_str())));
    assert_eq!(read(dir.path(), "a.txt"), None, "back to before the first message, hand edits included");
    let r = d.json(&["rewind", &id, "--to", "last", "--mode", "files"]);
    assert_eq!(r["messageId"], ids[2].as_str());
    let r = d.json(&["rewind", &id, "--to", &ids[1][..8], "--mode", "files"]);
    assert_eq!(r["messageId"], ids[1].as_str());
}

#[test]
fn both_rewinds_files_and_conversation_together() {
    let d = Daemon::start();
    let dir = tempfile::tempdir().unwrap();
    let (id, ids) = writer(&d, dir.path(), json!({}));
    let r = d.json(&["rewind", &id[..8], "--to", &ids[1], "--mode", "both"]);
    assert_eq!((r["mode"].as_str(), r["ok"].as_bool()), (Some("both"), Some(true)));
    assert_eq!(read(dir.path(), "a.txt").as_deref(), Some("one"));
    assert_eq!(read(dir.path(), "b.txt"), None);
    assert_eq!(users(&d, &id), vec![ids[0].clone()]);
    // The file checkpoints of the dropped turns went with them: a later files rewind to
    // the first message only knows the first turn.
    assert_eq!(rewind(&d, &id, &ids[0], "files").0, 200);
    assert_eq!(read(dir.path(), "a.txt"), None);
    // The default mode is the conversation.
    let (id2, ids2) = writer(&d, dir.path(), json!({}));
    let r = d.json(&["rewind", &id2, "--to", "2"]);
    assert_eq!(r["mode"], "conversation");
    assert_eq!(users(&d, &id2), vec![ids2[0].clone()]);
}

#[test]
fn checkpoints_survive_a_restart_and_a_closed_agent_resumes_to_rewind() {
    let mut d = Daemon::start();
    let dir = tempfile::tempdir().unwrap();
    let (id, ids) = writer(&d, dir.path(), json!({}));
    d.restart();
    assert_eq!(status(&d, &id), "closed");
    assert_eq!(rewind(&d, &id, &ids[1], "files").0, 200);
    assert_eq!(read(dir.path(), "a.txt").as_deref(), Some("one"));
    assert_eq!(read(dir.path(), "b.txt"), None);
}

#[test]
fn a_running_turn_is_cancelled_first_and_a_refused_rewind_leaves_it_alone() {
    let d = Daemon::start();
    let dir = tempfile::tempdir().unwrap();
    let id = start(&d, dir.path(), "lucina mock write a.txt=one", json!({"provider": "mock/ten-second-stream"}));
    ok(&d, "POST", &format!("/agents/{id}/send"), Some(&json!({"prompt": "now tell me a long story"})));
    // A provider that echoes the prompt puts it on the timeline just after the turn starts.
    wait_until("the story to run", 10, || status(&d, &id) == "running" && users(&d, &id).len() == 2);
    let ids = users(&d, &id);
    assert_eq!(ids.len(), 2);

    // A target that isn't there: refused, still running.
    let (st, v) = rewind(&d, &id, "nope", "conversation");
    assert_eq!((st, code(&v)), (404, "REWIND_TARGET_NOT_FOUND"), "{v}");
    assert_eq!(status(&d, &id), "running");

    let (st, v) = rewind(&d, &id, &ids[1], "conversation");
    assert_eq!(st, 200, "{v}");
    assert_eq!(status(&d, &id), "idle");
    assert_eq!(users(&d, &id), vec![ids[0].clone()]);
}

#[test]
fn a_provider_failure_is_reported_and_changes_nothing() {
    let d = Daemon::start();
    let dir = tempfile::tempdir().unwrap();
    let (id, ids) = writer(&d, dir.path(), json!({"featureValues": {"mockRewindError": "disk on fire"}}));
    let before = (rows(&d, &id).len(), epoch(&d, &id));
    for mode in ["conversation", "files", "both"] {
        let (st, v) = rewind(&d, &id, &ids[1], mode);
        assert_eq!((st, code(&v), v["error"]["message"].as_str()), (502, "PROVIDER_ERROR", Some("disk on fire")), "{mode}: {v}");
    }
    assert_eq!((rows(&d, &id).len(), epoch(&d, &id)), before);
    assert_eq!(read(dir.path(), "a.txt").as_deref(), Some("two"));
    let r = d.lagent(&["rewind", &id, "--to", "2", "--json"]);
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("PROVIDER_ERROR") && r.stderr.contains("disk on fire"), "{}", r.stderr);
    // Not a mode.
    assert_eq!(d.lagent(&["rewind", &id, "--to", "1", "--mode", "everything"]).code, 1);
}

/// The worktree guard: files in a main checkout under the owner's projects root are the
/// owner's call, and only the owner's.
#[test]
fn files_rewind_respects_the_worktree_guard() {
    let proj = tempfile::tempdir().unwrap();
    let projects = std::fs::canonicalize(proj.path()).unwrap();
    let repo = projects.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [vec!["init", "-q", "-b", "main"], vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "init"]] {
        assert!(Command::new("git").arg("-C").arg(&repo).args(&args).status().unwrap().success());
    }
    let d = Daemon::start_with_env(&[("WT_PROJECTS", projects.display().to_string())]);
    let ws = d.json(&["workspace", "create", "--isolation", "local", "--path", &repo.display().to_string()]);
    assert!(ws["workspaceId"].is_string(), "{ws}");
    let (st, v) = call(&d, "POST", "/agents", None, Some(&json!({"provider": FAST, "cwd": repo, "prompt": "lucina mock write a.txt=one"})));
    assert_eq!((st, code(&v)), (403, "WORKSPACE_GUARD"), "a main checkout is refused: {v}");
    let snap = ok(&d, "POST", "/agents", Some(&json!({"provider": FAST, "cwd": repo, "prompt": "lucina mock write a.txt=one", "allowMainCheckout": true, "labels": {"lucina.mcp-token": "tokG"}})));
    let id = snap["id"].as_str().unwrap().to_string();
    settle(&d, &id);
    say(&d, &id, "lucina mock write a.txt=two");
    let ids = users(&d, &id);

    let (st, v) = rewind(&d, &id, &ids[1], "files");
    assert_eq!((st, code(&v)), (403, "WORKSPACE_GUARD"), "{v}");
    assert_eq!(read(&repo, "a.txt").as_deref(), Some("two"));
    // The conversation isn't about files: no guard.
    let (st, v) = rewind(&d, &id, &ids[1], "conversation");
    assert_eq!(st, 200, "{v}");
    let ids = users(&d, &id);
    assert_eq!(ids.len(), 1);
    say(&d, &id, "lucina mock write a.txt=three");
    let ids = users(&d, &id);

    // An MCP token never gets the override, whatever it sends (the agent is in a workspace).
    let (st, v) = call(&d, "POST", &format!("/agents/{id}/rewind"), Some("mcp:tokG"), Some(&json!({"messageId": ids[1], "mode": "files", "allowMainCheckout": true})));
    assert_eq!((st, code(&v)), (403, "WORKSPACE_GUARD"), "{v}");
    assert_eq!(read(&repo, "a.txt").as_deref(), Some("three"));
    // The owner's override works.
    let (st, v) = call(&d, "POST", &format!("/agents/{id}/rewind"), None, Some(&json!({"messageId": ids[1], "mode": "files", "allowMainCheckout": true})));
    assert_eq!(st, 200, "{v}");
    assert_eq!(read(&repo, "a.txt").as_deref(), Some("two"), "back to how it was before the third write");
}

fn agent_with_token(d: &Daemon, cwd: &Path, ws: Option<&str>, token: &str, prompt: &str) -> String {
    let mut body = json!({"provider": FAST, "cwd": cwd, "prompt": prompt, "labels": {"lucina.mcp-token": token}});
    if let Some(w) = ws {
        body["workspaceId"] = json!(w);
    }
    let snap = ok(d, "POST", "/agents", Some(&body));
    let id = snap["id"].as_str().unwrap().to_string();
    settle(d, &id);
    id
}

/// An MCP token rewinds and forks only what it started, and files only inside the
/// agent's workspace.
#[test]
fn an_mcp_token_rewinds_and_forks_only_its_own_agents_inside_their_workspace() {
    let d = Daemon::start();
    let root = std::fs::canonicalize(tempfile::tempdir().unwrap().keep()).unwrap();
    let (inside, other) = (root.join("inside"), root.join("other"));
    for p in [&inside, &other] {
        std::fs::create_dir_all(p).unwrap();
    }
    let ws = d.json(&["workspace", "create", "--isolation", "local", "--path", &inside.display().to_string()])["workspaceId"].as_str().unwrap().to_string();
    let a = agent_with_token(&d, &inside, Some(&ws), "tokA", "lucina mock write a.txt=one");
    say(&d, &a, "lucina mock write a.txt=two");
    let b = agent_with_token(&d, &inside, Some(&ws), "tokB", "hello");
    let mcp_a = Some("mcp:tokA");
    let ids = users(&d, &a);

    // Someone else's agent: neither rewind nor fork, nor its history.
    let (st, v) = call(&d, "POST", &format!("/agents/{b}/rewind"), mcp_a, Some(&json!({"messageId": users(&d, &b)[0], "mode": "conversation"})));
    assert_eq!((st, code(&v)), (403, "NOT_YOUR_AGENT"), "{v}");
    for path in ["fork", "fork-context"] {
        let (st, v) = call(&d, "POST", &format!("/agents/{b}/{path}"), mcp_a, Some(&json!({})));
        assert_eq!((st, code(&v)), (403, "NOT_YOUR_AGENT"), "{path}: {v}");
    }
    // A token that names no one owns nothing.
    for origin in ["mcp", "mcp:unknown", "mcp:"] {
        let (st, _) = call(&d, "POST", &format!("/agents/{a}/fork-context"), Some(origin), Some(&json!({})));
        assert_eq!(st, 403, "{origin}");
    }

    // Its own agent, files in its workspace: fine.
    let (st, v) = call(&d, "POST", &format!("/agents/{a}/rewind"), mcp_a, Some(&json!({"messageId": ids[1], "mode": "files"})));
    assert_eq!(st, 200, "{v}");
    assert_eq!(read(&inside, "a.txt").as_deref(), Some("one"));
    // Forking its own agent: the fork is its too (it can rewind it, no one else can).
    let (st, v) = call(&d, "POST", &format!("/agents/{a}/fork"), mcp_a, Some(&json!({"prompt": "carry on"})));
    assert_eq!(st, 200, "{v}");
    let fork = v["agent"]["id"].as_str().unwrap().to_string();
    assert_eq!(v["agent"]["labels"]["lucina.mcp-token"], "tokA");
    assert_eq!(v["agent"]["labels"]["lucina.forked-from"], a.as_str());
    settle(&d, &fork);
    let (st, _) = call(&d, "POST", &format!("/agents/{fork}/fork-context"), Some("mcp:tokB"), Some(&json!({})));
    assert_eq!(st, 403);
    // The MCP's fork runs under supervision, like anything it starts: the mock's default
    // mode skips permission prompts, so it gets the other one.
    assert_eq!(v["agent"]["currentModeId"], "approval-test", "{v}");

    // An agent in a workspace but running outside its directory: files are the owner's only.
    let stray = agent_with_token(&d, &other, Some(&ws), "tokA", "lucina mock write s.txt=one");
    say(&d, &stray, "lucina mock write s.txt=two");
    let sids = users(&d, &stray);
    let (st, v) = call(&d, "POST", &format!("/agents/{stray}/rewind"), mcp_a, Some(&json!({"messageId": sids[1], "mode": "files"})));
    assert_eq!((st, code(&v)), (403, "REWIND_FORBIDDEN"), "{v}");
    assert_eq!(read(&other, "s.txt").as_deref(), Some("two"));
    // The conversation of that agent isn't about files.
    let (st, v) = call(&d, "POST", &format!("/agents/{stray}/rewind"), mcp_a, Some(&json!({"messageId": sids[1], "mode": "conversation"})));
    assert_eq!(st, 200, "{v}");
    let (st, v) = rewind(&d, &stray, &users(&d, &stray)[0], "files");
    assert_eq!(st, 200, "the owner may: {v}");
    assert_eq!(read(&other, "s.txt"), None);
    // An agent with no workspace at all: files are the owner's too.
    let loose = agent_with_token(&d, &other, None, "tokA", "lucina mock write l.txt=one");
    let (st, v) = call(&d, "POST", &format!("/agents/{loose}/rewind"), mcp_a, Some(&json!({"messageId": users(&d, &loose)[0], "mode": "files"})));
    assert_eq!((st, code(&v)), (403, "REWIND_FORBIDDEN"), "{v}");
}

/// `lagent <args>` as agent `agent_id` would run it: a process in its own group, listed in
/// the daemon's `procs/` with `LUCINA_AGENT_ID` in its launch environment.
fn as_agent(d: &Daemon, agent_id: &str, args: &[&str]) -> Run {
    let quoted: Vec<String> = args.iter().map(|a| format!("'{}'", a.replace('\'', "'\\''"))).collect();
    let child = Command::new("sh")
        .args(["-c", &format!("sleep 0.4; exec {LAGENT} {}", quoted.join(" "))])
        .env("LUCINA_AGENTD_SOCKET", d.socket())
        .env("LUCINA_AGENT_ID", agent_id)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = child.id();
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let ticks: u64 = stat[stat.rfind(')').unwrap() + 2..].split(' ').nth(19).unwrap().parse().unwrap();
    let procs = d.state().join("procs");
    std::fs::create_dir_all(&procs).unwrap();
    std::fs::write(procs.join(format!("{pid}.json")), json!({"pid": pid, "start_ticks": ticks, "program": "fake-provider"}).to_string()).unwrap();
    let out = child.wait_with_output().unwrap();
    let _ = std::fs::remove_file(procs.join(format!("{pid}.json")));
    Run { code: out.status.code().unwrap_or(-1), stdout: String::from_utf8_lossy(&out.stdout).into(), stderr: String::from_utf8_lossy(&out.stderr).into() }
}

/// An agent forks itself and rewinds its own subagents, never itself or anyone else.
#[test]
fn an_agent_forks_itself_and_rewinds_only_its_subagents() {
    let d = Daemon::start();
    let dir = tempfile::tempdir().unwrap();
    let parent = start(&d, dir.path(), "plan the work", json!({}));
    let stranger = start(&d, dir.path(), "something else", json!({}));
    let child = {
        let r = as_agent(&d, &parent, &["run", "--provider", FAST, "--mode", "approval-test", "--cwd", &dir.path().display().to_string(), "--no-wait", "lucina mock write k.txt=one", "--json"]);
        assert_eq!(r.code, 0, "{}", r.stderr);
        let v: Value = serde_json::from_str(&r.stdout).unwrap();
        v["agentId"].as_str().unwrap().to_string()
    };
    settle(&d, &child);

    // Itself: a fork (a subagent of its own). Not a rewind.
    let r = as_agent(&d, &parent, &["fork", &parent, "carry on", "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let forked: Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(forked["agent"]["labels"]["lucina.parent-agent-id"], parent.as_str());
    let r = as_agent(&d, &parent, &["rewind", &parent, "--to", "last", "--json"]);
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("NOT_YOUR_AGENT"), "{}", r.stderr);
    // Someone else's agent: neither.
    for args in [vec!["fork", &stranger[..]], vec!["rewind", &stranger[..], "--to", "last"]] {
        let mut args = args.clone();
        args.push("--json");
        let r = as_agent(&d, &parent, &args);
        assert_eq!(r.code, 1, "{args:?}");
        assert!(r.stderr.contains("NOT_YOUR_AGENT"), "{args:?}: {}", r.stderr);
    }
    // Its subagent: files and conversation.
    let r = as_agent(&d, &parent, &["rewind", &child, "--to", "1", "--mode", "both", "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(read(dir.path(), "k.txt"), None);
    assert_eq!(users(&d, &child).len(), 0);
    // A subagent starts nothing, forks included.
    let r = as_agent(&d, &child, &["fork", &child, "x", "--json"]);
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("DELEGATION_DEPTH"), "{}", r.stderr);
}

fn answers(d: &Daemon, dir: &Path, title: &str) -> String {
    let id = start(d, dir, "hello there", json!({"title": title, "featureValues": {"mockAssistantResponses": ["First answer.", "Second answer.", "Third answer."]}}));
    say(d, &id, "and more");
    id
}

fn history_of(user: &Value) -> String {
    user["item"]["text"].as_str().unwrap().to_string()
}

#[test]
fn fork_context_is_paseos_attachment() {
    let d = Daemon::start();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().display().to_string();
    let id = answers(&d, dir.path(), "Scroll bug");
    let ctx = ok(&d, "POST", &format!("/agents/{id}/fork-context"), Some(&json!({})));
    assert_eq!(ctx["attachment"]["type"], "text");
    assert_eq!(ctx["attachment"]["contextKind"], "chat_history");
    assert_eq!(ctx["attachment"]["mimeType"], "text/plain");
    assert_eq!(ctx["attachment"]["title"], "Chat history");
    assert_eq!(
        ctx["attachment"]["text"],
        format!("<chat-history-summary>\nChat history from a previous Paseo agent.\nSource agent: Scroll bug\nSource directory: {cwd}\n\n[User] hello there\n[Assistant] First answer.\n[User] and more\n[Assistant] Second answer.\n</chat-history-summary>").as_str()
    );
    assert_eq!((ctx["itemCount"].as_u64(), ctx["error"].is_null()), (Some(4), true));

    // Up to an assistant message, by id or by cursor (the seq it ends at).
    let timeline = ok(&d, "GET", &format!("/agents/{id}/timeline"), None);
    let first_answer = timeline["rows"].as_array().unwrap().iter().find(|r| r["item"]["type"] == "assistant_message").unwrap();
    let message_id = first_answer["item"]["messageId"].as_str().unwrap().to_string();
    let by_id = ok(&d, "POST", &format!("/agents/{id}/fork-context"), Some(&json!({"boundaryMessageId": message_id})));
    assert_eq!((by_id["itemCount"].as_u64(), by_id["boundaryMessageId"].as_str()), (Some(2), Some(message_id.as_str())));
    assert!(by_id["attachment"]["text"].as_str().unwrap().ends_with("[User] hello there\n[Assistant] First answer.\n</chat-history-summary>"));
    let cursor = json!({"epoch": timeline["epoch"], "seq": first_answer["seq"]});
    let by_cursor = ok(&d, "POST", &format!("/agents/{id}/fork-context"), Some(&json!({"boundaryCursor": cursor})));
    assert_eq!(by_cursor["attachment"], by_id["attachment"]);
    assert_eq!(by_cursor["boundaryCursor"], cursor);

    // A boundary that isn't there, and one from another timeline.
    for (body, fragment) in [
        (json!({"boundaryMessageId": "nope"}), "Selected assistant message is no longer available."),
        (json!({"boundaryCursor": {"epoch": timeline["epoch"], "seq": 99}}), "Selected timeline position is no longer available."),
        (json!({"boundaryCursor": {"epoch": "old", "seq": 2}}), "Selected timeline position is no longer available."),
    ] {
        let (st, v) = call(&d, "POST", &format!("/agents/{id}/fork-context"), None, Some(&body));
        assert_eq!((st, code(&v), v["error"]["message"].as_str()), (409, "FORK_BOUNDARY_UNAVAILABLE", Some(fragment)), "{body}");
    }
    // After a rewind the old cursor is stale.
    let ids = users(&d, &id);
    assert_eq!(rewind(&d, &id, &ids[1], "conversation").0, 200);
    let (st, v) = call(&d, "POST", &format!("/agents/{id}/fork-context"), None, Some(&json!({"boundaryCursor": cursor})));
    assert_eq!((st, code(&v)), (409, "FORK_BOUNDARY_UNAVAILABLE"), "{v}");
}

#[test]
fn a_fork_starts_with_the_chat_history_ahead_of_its_prompt() {
    let d = Daemon::start();
    let dir = tempfile::tempdir().unwrap();
    let id = answers(&d, dir.path(), "Scroll bug");

    // Through the CLI, from the first answer on.
    let first_answer = rows(&d, &id).into_iter().find(|r| r["item"]["type"] == "assistant_message").unwrap();
    let at = first_answer["item"]["messageId"].as_str().unwrap().to_string();
    let r = d.json(&["fork", &id, "continue from here", "--at", &at, "--title", "Forked"]);
    let fork = r["agentId"].as_str().unwrap().to_string();
    assert_ne!(fork, id);
    assert_eq!((r["forkedFrom"].as_str(), r["itemCount"].as_u64(), r["agent"]["title"].as_str()), (Some(id.as_str()), Some(2), Some("Forked")));
    assert_eq!(r["agent"]["labels"]["lucina.forked-from"], id.as_str());
    assert_eq!((r["agent"]["provider"].as_str(), r["agent"]["cwd"].as_str()), (Some("mock"), Some(dir.path().to_str().unwrap())));
    settle(&d, &fork);
    let sent = rows(&d, &fork).into_iter().find(|r| r["item"]["type"] == "user_message").map(|r| history_of(&r)).unwrap();
    assert!(sent.starts_with("<chat-history-summary>\nChat history from a previous Paseo agent.\nSource agent: Scroll bug\n"), "{sent}");
    assert!(sent.ends_with("[User] hello there\n[Assistant] First answer.\n</chat-history-summary>\n\ncontinue from here"), "{sent}");
    // The source is untouched.
    assert_eq!(users(&d, &id).len(), 2);

    // By cursor, whole history, no prompt: the new agent waits, and the history goes
    // ahead of the first prompt it gets, only that one, also across a restart.
    let seq = rows(&d, &id)[1]["seq"].as_u64().unwrap();
    let r = d.json(&["fork", &id, "--at", &seq.to_string()]);
    assert_eq!(r["itemCount"], 2);
    let r = d.json(&["fork", &id]);
    let waiting = r["agentId"].as_str().unwrap().to_string();
    assert_eq!(r["itemCount"], 4);
    assert_eq!((status(&d, &waiting), rows(&d, &waiting).len()), ("idle".to_string(), 0), "nothing runs until it is told");
    let mut d = d;
    d.restart();
    say(&d, &waiting, "what next?");
    let first = rows(&d, &waiting).into_iter().find(|r| r["item"]["type"] == "user_message").map(|r| history_of(&r)).unwrap();
    assert!(first.starts_with("<chat-history-summary>") && first.ends_with("[Assistant] Second answer.\n</chat-history-summary>\n\nwhat next?"), "{first}");
    say(&d, &waiting, "and then?");
    let texts: Vec<String> = rows(&d, &waiting).iter().filter(|r| r["item"]["type"] == "user_message").map(history_of).collect();
    assert_eq!(texts.len(), 2);
    assert_eq!(texts[1], "and then?", "the history is sent once");

    // Overrides: another provider, labels; an external agent can't seed its own kind.
    let r = d.json(&["fork", &id, "--provider", "mock/ten-second-stream", "--label", "team=a", "--mode", "approval-test"]);
    assert_eq!((r["agent"]["provider"].as_str(), r["agent"]["model"].as_str(), r["agent"]["labels"]["team"].as_str()), (Some("mock"), Some("ten-second-stream"), Some("a")));
    assert_eq!(r["agent"]["currentModeId"], "approval-test");
    let (st, v) = call(&d, "POST", "/agents/nope/fork", None, Some(&json!({})));
    assert_eq!((st, code(&v)), (404, "AGENT_NOT_FOUND"), "{v}");
}
