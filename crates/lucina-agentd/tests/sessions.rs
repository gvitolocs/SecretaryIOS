//! Claude sessions as agents, end to end on the daemon: a session is tracked once it has a
//! workspace, its beats drive its status, messages wait for its hook, agentd refuses what it
//! can't do to a session, and the merge sweep leaves a live session's workspace alone.

mod common;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::Daemon;
use serde_json::{json, Value};

const SESSION: &str = "local_11111111-2222-3333-4444-555555555555";

fn http(socket: &Path, method: &str, path: &str, origin: Option<&str>, body: Option<&Value>) -> (u16, Value) {
    let mut s = UnixStream::connect(socket).unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n", body.len());
    if let Some(o) = origin {
        req.push_str(&format!("X-Lucina-Origin: {o}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(&body);
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let (head, rest) = raw.split_once("\r\n\r\n").unwrap();
    let status: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
    let text = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        rest.split_once("\r\n").map(|(_, r)| r.rsplit_once("\r\n0\r\n").map(|(b, _)| b).unwrap_or(r)).unwrap_or("").to_string()
    } else {
        rest.to_string()
    };
    (status, serde_json::from_str(text.trim()).unwrap_or(Value::Null))
}

fn beat(d: &Daemon, body: Value) -> Value {
    let (status, v) = http(&d.socket(), "POST", "/sessions/beat", None, Some(&body));
    assert_eq!(status, 200, "{v}");
    v
}

fn agent(d: &Daemon, id: &str) -> Value {
    http(&d.socket(), "GET", &format!("/agents/{id}"), None, None).1
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit(dir: &Path, message: &str) {
    git(dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", message]);
}

struct Env {
    root: tempfile::TempDir,
    d: Daemon,
    repo: PathBuf,
}

impl Env {
    /// A daemon with the owner's real `wt`, and one repo (skipped where `wt` isn't installed).
    fn start(extra: &[(&str, String)]) -> Option<Env> {
        let wt = PathBuf::from(std::env::var("HOME").ok()?).join(".local/bin/wt");
        if !wt.exists() {
            eprintln!("sessions: skipped (no wt at {})", wt.display());
            return None;
        }
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("proj/Repo")).unwrap();
        let mut env = vec![
            ("WT_ROOT", root.path().join("wt").display().to_string()),
            ("WT_PROJECTS", root.path().join("proj").display().to_string()),
            ("LUCINA_AGENTD_WT", wt.display().to_string()),
            ("LUCINA_AGENTD_METADATA_PROVIDER", "mock/e2e-fast-stream".to_string()),
        ];
        env.extend(extra.iter().map(|(k, v)| (*k, v.clone())));
        let d = Daemon::start_with_env(&env);
        let repo = root.path().join("proj/Repo");
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README"), "hi\n").unwrap();
        git(&repo, &["add", "."]);
        commit(&repo, "init");
        Some(Env { root, d, repo })
    }

    fn worktree(&self, branch: &str) -> (String, PathBuf) {
        let p = self.repo.display().to_string();
        let w = self.d.json(&["workspace", "create", "--isolation", "worktree", "--path", &p, "--new-branch", branch]);
        (w["workspaceId"].as_str().unwrap().to_string(), PathBuf::from(w["cwd"].as_str().unwrap()))
    }

    /// A worktree whose branch is on main, as the sweep looks for.
    fn merged_worktree(&self, branch: &str) -> (String, PathBuf) {
        let (id, dir) = self.worktree(branch);
        std::fs::write(dir.join(format!("{branch}.txt")), "x").unwrap();
        git(&dir, &["add", "."]);
        commit(&dir, branch);
        git(&self.repo, &["merge", "-q", "--ff-only", branch]);
        (id, dir)
    }
}

#[test]
fn a_session_without_a_workspace_leaves_no_trace() {
    let d = Daemon::start();
    let v = beat(&d, json!({"sessionId": SESSION, "event": "tool", "paths": ["/nowhere"]}));
    assert_eq!(v, json!({"tracked": false}));
    assert!(d.json(&["ls"]).as_array().is_none_or(|a| a.is_empty()));
    assert!(http(&d.socket(), "GET", "/sessions", None, None).1["sessions"].as_array().unwrap().is_empty());
    // `lagent`'s fallback for a session it can't name is no session.
    let (status, _) = http(&d.socket(), "POST", "/sessions/beat", None, Some(&json!({"sessionId": "nez@nez_opt", "event": "tool"})));
    assert_eq!(status, 400);
}

#[test]
fn only_lagent_on_the_host_reports_for_a_session() {
    let d = Daemon::start();
    for origin in ["device:dev_mac", "mcp:hermes", "service:lucina"] {
        let (status, v) = http(&d.socket(), "POST", "/sessions/beat", Some(origin), Some(&json!({"sessionId": SESSION, "event": "tool", "register": true})));
        assert_eq!(status, 403, "{origin}: {v}");
    }
    // Nor can anyone make one through the agents API, or see it among the providers to start.
    let (status, v) = http(&d.socket(), "POST", "/agents", Some("device:dev_mac"), Some(&json!({"provider": "claude-session", "cwd": "/tmp"})));
    assert_eq!(status, 400, "{v}");
    let (_, providers) = http(&d.socket(), "GET", "/providers", None, None);
    assert!(!providers.to_string().contains("claude-session"), "{providers}");
}

#[test]
fn a_registered_session_is_an_agent_that_can_only_be_told_things() {
    let mut d = Daemon::start();
    let r = d.lagent(&["session", "register", "--session", SESSION, "--title", "Fix the login", "--host", "mac", "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let reply: Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(reply["tracked"], true);
    let id = reply["agentId"].as_str().unwrap().to_string();

    let a = agent(&d, &id);
    assert_eq!(a["provider"], "claude-session");
    assert_eq!(a["external"], true);
    assert_eq!(a["host"], "mac");
    assert_eq!(a["title"], "Fix the login");
    assert_eq!(a["status"], "idle");
    assert_eq!(a["labels"]["session"], SESSION);

    // Listed, for the app too; `--supervised` is what agentd itself runs.
    let (_, items) = http(&d.socket(), "GET", "/agents/items", None, None);
    assert_eq!(items["agents"][0]["id"], id.as_str());
    assert_eq!(items["agents"][0]["provider"], "claude-session");
    assert_eq!(d.json(&["ls"]).as_array().unwrap().len(), 1);
    assert!(d.json(&["ls", "--supervised"]).as_array().unwrap().is_empty());

    // The hook drives its status.
    beat(&d, json!({"sessionId": SESSION, "event": "prompt", "prompt": "fix it"}));
    assert_eq!(agent(&d, &id)["status"], "running");
    beat(&d, json!({"sessionId": SESSION, "event": "tool"}));
    assert_eq!(agent(&d, &id)["status"], "running");

    // agentd supervises nothing here: it says so, and leaves the session alone.
    for args in [vec!["stop", &id[..]], vec!["archive", &id[..]], vec!["delete", &id[..]]] {
        let r = d.lagent(&args);
        assert_ne!(r.code, 0, "{args:?}: {}", r.stdout);
        assert!(r.stderr.contains("Claude session") && !r.stderr.contains("null"), "{args:?}: {}", r.stderr);
    }
    let (status, v) = http(&d.socket(), "POST", &format!("/agents/{id}/stop"), None, Some(&json!({})));
    assert_eq!(status, 403, "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("Claude session on mac") || v["error"]["message"].as_str().unwrap().contains("running on mac"), "{v}");
    let (status, v) = http(&d.socket(), "POST", &format!("/agents/{id}/rewind"), None, Some(&json!({"messageId": "m", "mode": "conversation"})));
    assert_ne!(status, 200, "{v}");

    // A message waits for the hook, however the sender asked for it to arrive.
    let r = d.lagent(&["send", &id, "--behavior", "interrupt", "what's the status?", "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let sent: Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(sent["status"], "queued", "{sent}");
    assert_eq!(agent(&d, &id)["status"], "running", "the message stops nothing");

    // A subagent's hook gets none; the session's own does, until it is acknowledged.
    let v = beat(&d, json!({"sessionId": SESSION, "event": "tool", "noCommands": true}));
    assert!(v.get("commands").is_none(), "{v}");
    let v = beat(&d, json!({"sessionId": SESSION, "event": "tool"}));
    let commands = v["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 1, "{v}");
    assert_eq!(commands[0]["command"]["kind"], "send");
    assert_eq!(commands[0]["command"]["prompt"], "what's the status?");
    let command = commands[0]["commandId"].as_str().unwrap().to_string();
    assert!(beat(&d, json!({"sessionId": SESSION, "event": "tool"}))["commands"].as_array().is_some(), "not acknowledged yet");
    let r = d.lagent(&["session", "ack", SESSION, &command, "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(serde_json::from_str::<Value>(&r.stdout).unwrap()["acked"], 1);
    assert!(beat(&d, json!({"sessionId": SESSION, "event": "tool"})).get("commands").is_none());

    // The turn's end: idle, nothing to push, and the conversation is on the timeline.
    beat(&d, json!({"sessionId": SESSION, "event": "stop", "message": "All green."}));
    let a = agent(&d, &id);
    assert_eq!(a["status"], "idle");
    assert_eq!(a["requiresAttention"], false, "{a}");
    let (_, timeline) = http(&d.socket(), "GET", &format!("/agents/{id}/timeline?limit=50"), None, None);
    let text = timeline.to_string();
    for wanted in ["fix it", "what's the status?", "All green."] {
        assert!(text.contains(wanted), "{wanted}: {text}");
    }

    // A message to an idle session wakes it: the ack shows it working.
    d.lagent(&["send", &id, "and now?"]);
    let v = beat(&d, json!({"sessionId": SESSION, "event": "poll"}));
    assert_eq!(agent(&d, &id)["status"], "idle", "a poll alone changes nothing");
    let command = v["commands"][0]["commandId"].as_str().unwrap().to_string();
    assert_eq!(http(&d.socket(), "POST", &format!("/sessions/{SESSION}/ack"), None, Some(&json!({"commandIds": [command.clone()]}))).1["acked"], 1);
    assert_eq!(agent(&d, &id)["status"], "running");
    assert_eq!(http(&d.socket(), "POST", &format!("/sessions/{SESSION}/ack"), None, Some(&json!({"commandIds": [command]}))).1["acked"], 0, "again is fine");
    beat(&d, json!({"sessionId": SESSION, "event": "stop"}));

    // A restart of agentd interrupts nothing: like every agent it shows closed until it is heard
    // from again (at most two minutes, the idle hook's poll), and its row still says it is live.
    d.restart();
    assert_eq!(agent(&d, &id)["status"], "closed");
    assert!(http(&d.socket(), "GET", "/sessions", None, None).1["sessions"][0]["endedAtMs"].is_null());
    beat(&d, json!({"sessionId": SESSION, "event": "poll"}));
    assert_eq!(agent(&d, &id)["status"], "idle");
    let (_, timeline) = http(&d.socket(), "GET", &format!("/agents/{id}/timeline?limit=50"), None, None);
    assert!(!timeline.to_string().contains("interrupted"), "{timeline}");
    assert_eq!(beat(&d, json!({"sessionId": SESSION, "event": "tool"}))["agentId"], id.as_str());
    assert_eq!(agent(&d, &id)["status"], "running");

    // The end: closed, and now it can be archived. A later beat brings it back.
    let v = beat(&d, json!({"sessionId": SESSION, "event": "end"}));
    assert_eq!(v["tracked"], true);
    assert_eq!(agent(&d, &id)["status"], "closed");
    assert_eq!(d.lagent(&["archive", &id]).code, 0);
    assert_eq!(beat(&d, json!({"sessionId": SESSION, "event": "prompt", "prompt": "back"}))["agentId"], id.as_str());
    let a = agent(&d, &id);
    assert_eq!(a["status"], "running");
    assert!(a["archivedAt"].is_null(), "{a}");
    beat(&d, json!({"sessionId": SESSION, "event": "end"}));
    assert_eq!(d.lagent(&["delete", &id]).code, 0);
    assert!(http(&d.socket(), "GET", "/sessions", None, None).1["sessions"].as_array().unwrap().is_empty());
}

#[test]
fn a_session_shows_its_workspace_on_the_list_and_the_board() {
    let Some(e) = Env::start(&[]) else { return };
    let d = &e.d;
    let (first, first_dir) = e.worktree("first");
    let (second, second_dir) = e.worktree("second");
    assert!(first_dir.starts_with(e.root.path()));

    // Its cwd, or a file it edits, is in a worktree: that is enough.
    let v = beat(d, json!({"sessionId": SESSION, "event": "tool", "paths": [first_dir]}));
    assert_eq!(v["workspaces"], json!([first]));
    let id = v["agentId"].as_str().unwrap().to_string();
    let a = agent(d, &id);
    assert_eq!(a["workspaceId"], first.as_str());
    assert_eq!(a["labels"]["branch"], "first");
    assert_eq!(a["cwd"], first_dir.display().to_string());
    assert_eq!(a["host"], "mac", "desktop sessions run on the Mac");

    let board = d.json(&["board"]);
    let row = |b: &Value, w: &str| b["workspaces"].as_array().unwrap().iter().find(|x| x["workspaceId"] == w).cloned().unwrap();
    assert_eq!(row(&board, &first)["session"], SESSION);
    assert!(row(&board, &second)["session"].is_null());

    // Moving on to another worktree: both stay its, the latest shows.
    let v = beat(d, json!({"sessionId": SESSION, "event": "tool", "workspaces": [second]}));
    assert_eq!(v["workspaces"], json!([first, second]));
    assert_eq!(agent(d, &id)["labels"]["branch"], "second");
    assert_eq!(agent(d, &id)["cwd"], second_dir.display().to_string());
    let board = d.json(&["board"]);
    assert_eq!(row(&board, &first)["session"], SESSION);
    assert_eq!(row(&board, &second)["session"], SESSION);
    let listed = d.json(&["session", "ls"]);
    assert_eq!(listed[0]["branches"], json!(["first", "second"]), "{listed}");

    // A worktree going away (a merge archives it) unlinks it; the session carries on, and the
    // merge isn't stopped by the session working there.
    assert_eq!(agent(d, &id)["status"], "running");
    assert_eq!(d.lagent(&["workspace", "archive", &second]).code, 0);
    assert!(!second_dir.exists());
    let a = agent(d, &id);
    assert_eq!(a["status"], "running", "{a}");
    assert_eq!(a["workspaceId"], first.as_str());
    assert!(beat(d, json!({"sessionId": SESSION, "event": "tool"}))["workspaces"] == json!([first]));
    assert_eq!(agent(d, &id)["labels"]["branch"], "first");
}

#[test]
fn a_task_names_its_session() {
    let Some(e) = Env::start(&[]) else { return };
    let d = &e.d;
    let repo = e.repo.display().to_string();
    // A real task needs a trunk and no remote is fine (a local trunk).
    let r = d.lagent(&["task", "start", "Fix the login", "--repo", &repo, "--branch", "login", "--session", SESSION, "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let v: Value = serde_json::from_str(&r.stdout).unwrap();
    let workspace = v["workspace"]["workspaceId"].as_str().unwrap().to_string();
    // Its agent exists from that moment, titled after the task.
    let (_, sessions) = http(&d.socket(), "GET", "/sessions", None, None);
    let s = &sessions["sessions"][0];
    assert_eq!(s["sessionId"], SESSION);
    assert_eq!(s["title"], "Fix the login");
    assert_eq!(s["workspaces"], json!([workspace]));
    assert_eq!(s["branches"], json!(["login"]));
    // Later beats find the task too, whatever the session's cwd is (the Mac's own, here).
    let v = beat(d, json!({"sessionId": SESSION, "event": "tool", "paths": ["/Users/giuseppe/mnt/nezopt/Projects/Lucina"], "title": "Login work"}));
    assert_eq!(v["workspaces"], json!([workspace]));
    assert_eq!(agent(d, v["agentId"].as_str().unwrap())["title"], "Login work");

    // A bare `ssh nezopt lagent task start` names user@host, and no agent appears; once the
    // session's hook points at the workspace, the task is the session's.
    let r = d.lagent(&["task", "start", "Other", "--repo", &repo, "--branch", "other", "--session", "nez@nez_opt", "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let other = serde_json::from_str::<Value>(&r.stdout).unwrap()["workspace"]["workspaceId"].as_str().unwrap().to_string();
    assert_eq!(http(&d.socket(), "GET", "/sessions", None, None).1["sessions"].as_array().unwrap().len(), 1);
    beat(d, json!({"sessionId": SESSION, "event": "tool", "workspaces": [other]}));
    let tasks = d.json(&["task", "ls"]);
    let task = tasks.as_array().unwrap().iter().find(|t| t["workspaceId"] == other.as_str()).unwrap();
    assert_eq!(task["session"], SESSION);
}

#[test]
fn the_sweep_leaves_a_live_sessions_workspace_alone() {
    let Some(e) = Env::start(&[]) else { return };
    let d = &e.d;
    let (workspace, dir) = e.merged_worktree("done");
    let (unclaimed, unclaimed_dir) = e.merged_worktree("unclaimed");
    beat(d, json!({"sessionId": SESSION, "event": "tool", "workspaces": [workspace]}));

    // Merged, clean and idle: only the session keeps it. The other one goes.
    let swept = d.json(&["workspace", "sweep"]);
    assert_eq!(swept.as_array().unwrap().len(), 1, "{swept}");
    assert_eq!(swept[0]["workspaceId"], unclaimed.as_str(), "{swept}");
    assert!(!unclaimed_dir.exists());
    assert!(dir.exists(), "a live session's worktree stays");

    // Idle is still live: between turns the session may need its worktree again.
    beat(d, json!({"sessionId": SESSION, "event": "stop"}));
    assert_eq!(d.json(&["workspace", "sweep"]), json!([]));
    assert!(dir.exists());

    // When it ends, the worktree is ordinary merged work again.
    beat(d, json!({"sessionId": SESSION, "event": "end"}));
    let swept = d.json(&["workspace", "sweep"]);
    assert_eq!(swept[0]["workspaceId"], workspace.as_str(), "{swept}");
    assert!(!dir.exists());
}

#[test]
fn a_silent_session_ends_and_comes_back() {
    let Some(e) = Env::start(&[("LUCINA_AGENTD_SESSION_TTL_SECS", "1".to_string())]) else { return };
    let d = &e.d;
    let (workspace, dir) = e.merged_worktree("quiet");
    let v = beat(d, json!({"sessionId": SESSION, "event": "tool", "workspaces": [workspace]}));
    let id = v["agentId"].as_str().unwrap().to_string();
    assert_eq!(agent(d, &id)["status"], "running");

    // No beats for longer than the limit: ended, and no longer in the sweep's way.
    let deadline = Instant::now() + Duration::from_secs(10);
    while agent(d, &id)["status"] != "closed" {
        assert!(Instant::now() < deadline, "{}", agent(d, &id));
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(http(&d.socket(), "GET", "/sessions", None, None).1["sessions"][0]["endedAtMs"].is_u64(), true);
    let swept = d.json(&["workspace", "sweep"]);
    assert_eq!(swept[0]["workspaceId"], workspace.as_str(), "{swept}");
    assert!(!dir.exists());

    // The Mac woke up: same agent, working again, without the removed worktree.
    let v = beat(d, json!({"sessionId": SESSION, "event": "tool"}));
    assert_eq!(v["agentId"], id.as_str());
    assert_eq!(agent(d, &id)["status"], "running");
    assert!(v.get("workspaces").is_none(), "{v}");
}

/// Only a run he started on his own device alerts him when it finishes, and nobody can claim that.
#[test]
fn only_a_run_from_his_device_is_marked_for_a_finish_alert() {
    const LABEL: &str = "lucina.started-on-device";
    let d = Daemon::start();
    let cwd = tempfile::tempdir().unwrap();
    let body = json!({"provider": "mock", "cwd": cwd.path(), "labels": {LABEL: "true", "from": "some-provider-test"}});
    let (status, v) = http(&d.socket(), "POST", "/agents", Some("device:dev_phone"), Some(&body));
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["labels"][LABEL], "true", "{v}");
    for origin in [None, Some("mcp:hermes"), Some("service:lucina")] {
        let (status, v) = http(&d.socket(), "POST", "/agents", origin, Some(&body));
        assert_eq!(status, 200, "{origin:?}: {v}");
        assert!(v["labels"].get(LABEL).is_none(), "{origin:?}: {v}");
        assert_eq!(v["labels"]["from"], "some-provider-test", "{v}");
    }
}
