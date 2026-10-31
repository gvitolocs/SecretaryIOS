//! Terminals, scripts and services end to end against a real daemon (phase 5,
//! parity rows T1, T2, T3, S1, S2, W3): keys and capture, the resumable output stream,
//! scripts with Paseo's env and ports from the range, health, the service proxy, port
//! detection, cleanup, and who may touch what (agents, MCP tokens, terminal processes).

mod common;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::Engine;
use common::{Daemon, Guard, LAGENT};
use serde_json::{json, Value};

struct Resp {
    status: u16,
    head: String,
    body: String,
}

impl Resp {
    fn json(&self) -> Value {
        serde_json::from_str(self.body.trim()).unwrap_or(Value::String(self.body.clone()))
    }

    fn header(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    }
}

fn dechunk(mut rest: &str) -> String {
    let mut out = String::new();
    while let Some((size, after)) = rest.split_once("\r\n") {
        let Ok(n) = usize::from_str_radix(size.trim(), 16) else { break };
        if n == 0 || after.len() < n {
            break;
        }
        out.push_str(&after[..n]);
        rest = after[n..].strip_prefix("\r\n").unwrap_or(&after[n..]);
    }
    out
}

/// One request on the daemon socket, with extra headers (`X-Lucina-Origin`, `Host`).
fn http(d: &Daemon, method: &str, path: &str, body: Option<&Value>, headers: &[(&str, &str)]) -> Resp {
    let mut s = UnixStream::connect(d.socket()).unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut req = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n", body.len());
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host")) {
        req.push_str("Host: lucina-agentd\r\n");
    }
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(&body);
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let raw = String::from_utf8_lossy(&raw).to_string();
    let (head, rest) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") { dechunk(rest) } else { rest.to_string() };
    Resp { status, head: head.to_string(), body }
}

fn get(d: &Daemon, path: &str) -> Value {
    let r = http(d, "GET", path, None, &[]);
    assert!(r.status < 300, "GET {path}: {} {}", r.status, r.body);
    r.json()
}

fn post(d: &Daemon, path: &str, body: Value) -> Value {
    let r = http(d, "POST", path, Some(&body), &[]);
    assert!(r.status < 300, "POST {path}: {} {}", r.status, r.body);
    r.json()
}

fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn capture(d: &Daemon, id: &str) -> Vec<String> {
    get(d, &format!("/terminals/{id}/capture"))["lines"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_string()).collect()
}

fn local_workspace(d: &Daemon, dir: &Path) -> String {
    let w = d.json(&["workspace", "create", "--isolation", "local", "--path", &dir.display().to_string()]);
    w["workspace"]["workspaceId"].as_str().or(w["workspaceId"].as_str()).expect("workspace id").to_string()
}

fn alive(pid: u64) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z "))
}

/// T1, T3: terminals through `lagent` and the API.
#[test]
fn terminal_keys_capture_stream_and_kill() {
    let d = Daemon::start_with_env(&[("SHELL", "/bin/sh".into())]);
    let dir = tempfile::tempdir().unwrap();
    let ws = local_workspace(&d, dir.path());
    let t = d.json(&["terminal", "create", "--workspace", &ws, "--name", "build"]);
    let id = t["id"].as_str().unwrap().to_string();
    assert_eq!((t["name"].as_str(), t["workspaceId"].as_str(), t["owner"].as_str()), (Some("build"), Some(ws.as_str()), Some("owner")));
    wait_for("a prompt", 10, || capture(&d, &id).iter().any(|l| l.starts_with('$')));
    let sent = d.json(&["terminal", "send-keys", &id[..10], "echo lucina-$((6*7))", "Enter"]);
    assert_eq!(sent["keysSent"], 21);
    wait_for("echo output", 10, || capture(&d, &id).iter().any(|l| l.ends_with("lucina-42")));
    let cap = d.json(&["terminal", "capture", "build"]);
    assert_eq!(cap["totalLines"], 24);
    assert!(cap["lines"].as_array().unwrap().iter().any(|l| l.as_str().unwrap().ends_with("echo lucina-$((6*7))")));
    let text = d.lagent(&["terminal", "capture", &id, "--start", "-24", "--end", "1"]);
    assert_eq!(text.code, 0);
    assert_eq!(text.stdout.lines().count(), 2, "{}", text.stdout);
    assert_eq!(d.lagent(&["terminal", "capture", &id, "--start", "x"]).code, 1);

    // The stream: a snapshot first, then numbered output; a resume gets what it missed.
    let mut s = UnixStream::connect(d.socket()).unwrap();
    s.write_all(format!("GET /terminals/{id}/stream HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut seen = String::new();
    let mut buf = [0u8; 8192];
    while !seen.contains("event: snapshot") {
        let n = s.read(&mut buf).unwrap_or(0);
        assert!(n > 0, "{seen}");
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    post(&d, &format!("/terminals/{id}/input"), json!({"data": "echo streamed\r"}));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !(seen.contains("event: snapshot") && seen.contains("event: output")) && Instant::now() < deadline {
        let n = s.read(&mut buf).unwrap_or(0);
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    assert!(seen.contains("event: snapshot") && seen.contains("event: output"), "{seen}");
    drop(s);
    // A viewer that resumes from offset 0 gets the bytes again, not a snapshot.
    let mut r = UnixStream::connect(d.socket()).unwrap();
    r.write_all(format!("GET /terminals/{id}/stream HTTP/1.1\r\nHost: x\r\nLast-Event-ID: 0\r\n\r\n").as_bytes()).unwrap();
    r.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut first = String::new();
    while !first.contains("\n\n") || !first.contains("event:") {
        let n = r.read(&mut buf).unwrap_or(0);
        assert!(n > 0, "{first}");
        first.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
    assert!(first.contains("event: output") && !first.contains("event: snapshot"), "{first}");
    drop(r);

    // Paste: the file is saved and its path typed in.
    let pasted = post(&d, &format!("/terminals/{id}/paste"), json!({"name": "shot one.png", "dataBase64": base64::engine::general_purpose::STANDARD.encode(b"PNG")}));
    let path = pasted["path"].as_str().unwrap().to_string();
    assert_eq!(std::fs::read(&path).unwrap(), b"PNG");
    assert!(path.ends_with("-shotone.png"), "{path}");
    wait_for("the pasted path", 10, || capture(&d, &id).iter().any(|l| l.contains("shotone.png")));

    let resized = post(&d, &format!("/terminals/{id}/resize"), json!({"rows": 40, "cols": 120}));
    assert_eq!((resized["rows"].as_u64(), resized["cols"].as_u64()), (Some(40), Some(120)));
    let pid = resized["pid"].as_u64().unwrap();
    assert_eq!(d.json(&["terminal", "ls", "--all"]).as_array().unwrap().len(), 1);
    assert_eq!(d.json(&["terminal", "ls", "--workspace", "nope"]).as_array().unwrap().len(), 0);
    let killed = d.json(&["terminal", "kill", &id]);
    assert_eq!(killed["success"], true);
    wait_for("the shell to go", 10, || !alive(pid));
    assert!(!Path::new(&path).exists(), "uploads go with the terminal");
    let gone = d.lagent(&["terminal", "kill", &id, "--json"]);
    assert_eq!(gone.code, 1);
    assert!(gone.stdout.contains("TERMINAL_NOT_FOUND") || gone.stderr.contains("TERMINAL_NOT_FOUND"), "{} {}", gone.stdout, gone.stderr);
}

/// T2: an agent CLI in a terminal reports its activity with the terminal's token.
#[test]
fn terminal_hooks_report_activity() {
    let d = Daemon::start_with_env(&[("SHELL", "/bin/sh".into())]);
    let dir = tempfile::tempdir().unwrap();
    let ws = local_workspace(&d, dir.path());
    let t = post(&d, "/terminals", json!({"workspaceId": ws, "command": "/bin/sh", "args": ["-c", format!("echo '{{\"notification_type\":\"idle_prompt\"}}' | {LAGENT} hooks claude Notification; sleep 30")]}));
    let id = t["id"].as_str().unwrap().to_string();
    wait_for("needs-input", 10, || get(&d, &format!("/terminals/{id}"))["activity"] == "needs-input");
    let bad = http(&d, "POST", "/terminal-activity", Some(&json!({"terminalId": id, "token": "nope", "state": "idle"})), &[]);
    assert_eq!(bad.status, 403);
    // Outside a terminal it does nothing and succeeds.
    assert_eq!(d.lagent(&["hooks", "claude", "Stop"]).code, 0);
}

fn systemd_scope_works() -> bool {
    Command::new("systemd-run").args(["--user", "--scope", "--quiet", "--collect", "true"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

/// S1, S2, W3: scripts with Paseo's env, ports in the range, health, the proxy, detected ports.
#[test]
fn scripts_services_and_proxy() {
    let scope = systemd_scope_works();
    let d = Daemon::start_with_env(&[("SHELL", "/bin/sh".into()), ("LUCINA_AGENTD_SCRIPT_SCOPE", if scope { "on" } else { "off" }.into())]);
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(
        root.join("paseo.json"),
        json!({"scripts": {
            "env": {"command": "env > env.txt"},
            "web": {"command": "exec python3 -m http.server \"$PASEO_PORT\" --bind 127.0.0.1", "type": "service"},
        }})
        .to_string(),
    )
    .unwrap();
    std::fs::create_dir_all(root.join(".lucina")).unwrap();
    std::fs::write(root.join(".lucina/agents.toml"), "[scripts.api]\ncmd = \"exec python3 -m http.server $PORT --bind 127.0.0.1\"\nport = \"auto\"\nhealth = \"/missing\"\n").unwrap();
    std::fs::write(root.join("index.html"), "<p>hello from web</p>").unwrap();
    let ws = local_workspace(&d, &root);

    let ls = d.json(&["script", "ls", "--workspace", &ws]);
    let names: Vec<&str> = ls.as_array().unwrap().iter().map(|s| s["scriptName"].as_str().unwrap()).collect();
    assert_eq!(names, ["api", "env", "web"]);
    assert_eq!(ls[1]["type"], "script");
    assert_eq!(ls[2]["type"], "service");
    assert_eq!(ls[2]["lifecycle"], "stopped");

    // A plain script: Paseo's env, then it ends and stays capturable.
    let env = d.json(&["script", "start", "env", "--cwd", &root.display().to_string()]);
    assert_eq!(env["lifecycle"], "running");
    wait_for("the env script to end", 10, || get(&d, &format!("/workspaces/{ws}/scripts"))["scripts"][1]["lifecycle"] == "stopped");
    let text = std::fs::read_to_string(root.join("env.txt")).unwrap();
    let var = |k: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).map(str::to_string);
    assert_eq!(var("PASEO_SCRIPTNAME").as_deref(), Some("env"));
    assert_eq!(var("PASEO_WORKSPACE_ID").as_deref(), Some(ws.as_str()));
    assert_eq!(var("PASEO_WORKTREE_PATH").as_deref(), Some(root.to_str().unwrap()));
    assert!(var("PASEO_TERMINAL_ID").is_some() && var("PASEO_ACTIVITY_TOKEN").is_some());
    assert_eq!(var("PASEO_PORT"), None, "plain scripts get no port");
    assert_eq!(var(lucina_agentd::notify::TOKEN_ENV), None);
    assert_eq!(get(&d, &format!("/workspaces/{ws}/scripts"))["scripts"][1]["exitCode"], 0);

    // A service: a port from 4100-4999, its env, health, the proxy.
    let web = d.json(&["script", "start", "web", "--workspace", &ws]);
    let port = web["port"].as_u64().unwrap();
    assert!((4100..=4999).contains(&port), "{web}");
    assert_eq!(web["servicePath"], format!("/v1/agents/workspaces/{ws}/services/web/"));
    assert!(web["hostname"].as_str().unwrap().starts_with("web--") && web["hostname"].as_str().unwrap().ends_with(".localhost"));
    let again = d.lagent(&["script", "start", "web", "--workspace", &ws, "--json"]);
    assert_eq!(again.code, 1);
    wait_for("web healthy", 20, || get(&d, &format!("/workspaces/{ws}/scripts"))["scripts"][2]["health"] == "healthy");
    let page = http(&d, "GET", &format!("/workspaces/{ws}/services/web/index.html"), None, &[]);
    assert_eq!(page.status, 200, "{}", page.body);
    assert!(page.body.contains("hello from web"));
    assert!(page.header("content-security-policy").is_some_and(|c| c.starts_with("sandbox")));
    let redirect = http(&d, "GET", &format!("/workspaces/{ws}/services/web/.lucina"), None, &[]);
    assert_eq!(redirect.header("location").as_deref(), Some(format!("/v1/agents/workspaces/{ws}/services/web/.lucina/").as_str()));
    let label = web["hostname"].as_str().unwrap().to_string();
    let by_host = http(&d, "GET", "/index.html", None, &[("Host", &format!("{label}:6768"))]);
    assert!(by_host.body.contains("hello from web"));
    for origin in ["mcp:tok", "device:"] {
        assert_eq!(http(&d, "GET", &format!("/workspaces/{ws}/services/web/"), None, &[("X-Lucina-Origin", origin)]).status, 403, "{origin}");
    }
    if scope {
        let term = get(&d, &format!("/terminals/{}", web["terminalId"].as_str().unwrap()));
        let cg = std::fs::read_to_string(format!("/proc/{}/cgroup", term["pid"])).unwrap();
        assert!(cg.contains(".scope"), "the script runs in a scope: {cg}");
    }

    // A service whose health path fails is unhealthy; a second service's env names its peer.
    let api = post(&d, &format!("/workspaces/{ws}/scripts/api/start"), json!({}));
    assert!(api["port"].as_u64().unwrap() != port);
    wait_for("api unhealthy", 20, || get(&d, &format!("/workspaces/{ws}/scripts"))["scripts"][0]["health"] == "unhealthy");

    // A server started by hand in a terminal is detected.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let free = probe.local_addr().unwrap().port();
    drop(probe);
    post(&d, "/terminals", json!({"workspaceId": ws, "command": "python3", "args": ["-m", "http.server", free.to_string(), "--bind", "127.0.0.1"]}));
    wait_for("the detected port", 20, || {
        get(&d, &format!("/workspaces/{ws}/services"))["services"].as_array().unwrap().iter().any(|s| s["kind"] == "detected" && s["port"] == free)
    });
    let detected = http(&d, "GET", &format!("/workspaces/{ws}/services/port-{free}/index.html"), None, &[]);
    assert!(detected.body.contains("hello from web"));
    assert_eq!(http(&d, "GET", &format!("/workspaces/{ws}/services/port-1/"), None, &[]).status, 404);

    let logs = get(&d, &format!("/workspaces/{ws}/scripts/web/logs?lines=50"));
    assert!(logs["lines"].as_array().unwrap().iter().any(|l| l.as_str().unwrap().contains("GET /index.html")), "{logs}");
    let stopped = d.json(&["script", "stop", "web", "--workspace", &ws]);
    assert_eq!(stopped["lifecycle"], "stopped");
    assert_eq!(d.lagent(&["script", "stop", "web", "--workspace", &ws, "--json"]).code, 1);

    // Archive stops the rest.
    let pids: Vec<u64> = get(&d, "/terminals")["terminals"].as_array().unwrap().iter().filter(|t| t["status"] == "running").filter_map(|t| t["pid"].as_u64()).collect();
    assert!(!pids.is_empty());
    d.json(&["workspace", "archive", &ws]);
    for pid in pids {
        wait_for("archive to stop the workspace's processes", 10, || !alive(pid));
    }
    assert_eq!(get(&d, "/terminals")["terminals"].as_array().unwrap().len(), 0);
}

/// Field 22 of `/proc/<pid>/stat`.
fn start_ticks(pid: u32) -> u64 {
    let s = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    s[s.rfind(')').unwrap() + 2..].split(' ').nth(19).unwrap().parse().unwrap()
}

/// Run `lagent <args>` as agent `agent` would: under a "provider" process agentd's
/// registry knows, started with `LUCINA_AGENT_ID`. Returns lagent's stdout.
fn as_agent(d: &Daemon, agent: &str, args: &[&str]) -> String {
    let out = tempfile::NamedTempFile::new().unwrap();
    let quoted: Vec<String> = args.iter().map(|a| format!("'{}'", a.replace('\'', r"'\''"))).collect();
    let script = format!("while [ ! -e '{0}.go' ]; do sleep 0.05; done; '{LAGENT}' {1} > '{0}' 2>&1", out.path().display(), quoted.join(" "));
    let child = Command::new("/bin/sh")
        .args(["-c", &script])
        .env("LUCINA_AGENT_ID", agent)
        .env("LUCINA_AGENTD_SOCKET", d.socket())
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = child.id();
    let mut guard = Guard { child: Some(child) };
    let procs = d.state().join("procs");
    std::fs::create_dir_all(&procs).unwrap();
    std::fs::write(procs.join(format!("{pid}.json")), json!({"pid": pid, "start_ticks": start_ticks(pid), "program": "fake-provider"}).to_string()).unwrap();
    std::fs::write(format!("{}.go", out.path().display()), "").unwrap();
    let mut child = guard.child.take().unwrap();
    let _ = child.wait();
    let _ = std::fs::remove_file(procs.join(format!("{pid}.json")));
    let _ = std::fs::remove_file(format!("{}.go", out.path().display()));
    std::fs::read_to_string(out.path()).unwrap()
}

fn agent(d: &Daemon, cwd: &Path, ws: &str, token: &str) -> String {
    let snap = post(d, "/agents", json!({"provider": "mock/e2e-fast-stream", "cwd": cwd, "workspaceId": ws, "labels": {"lucina.mcp-token": token}, "prompt": "hello"}));
    assert_eq!(snap["workspaceId"], ws, "{snap}");
    snap["id"].as_str().unwrap().to_string()
}

/// Security: agents and MCP tokens touch only their own terminals and workspaces.
#[test]
fn agents_and_mcp_stay_in_their_lane() {
    let d = Daemon::start_with_env(&[("SHELL", "/bin/sh".into()), ("LUCINA_AGENTD_SCRIPT_SCOPE", "off".into())]);
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (one, two) = (root.join("one"), root.join("two"));
    for p in [&one, &two] {
        std::fs::create_dir_all(p.join("sub")).unwrap();
        std::fs::write(p.join("paseo.json"), r#"{"scripts": {"hello": {"command": "echo hi; sleep 30"}}}"#).unwrap();
    }
    let w1 = local_workspace(&d, &one);
    let w2 = local_workspace(&d, &two);
    let a = agent(&d, &one, &w1, "tokA");
    let b = agent(&d, &one, &w1, "tokB");
    let ta = post(&d, "/terminals", json!({"workspaceId": w1, "agentId": a, "name": "for-a"}))["id"].as_str().unwrap().to_string();
    let tb = post(&d, "/terminals", json!({"workspaceId": w1, "agentId": b, "name": "for-b"}))["id"].as_str().unwrap().to_string();
    let mine = post(&d, "/terminals", json!({"workspaceId": w2, "name": "owners"}))["id"].as_str().unwrap().to_string();

    // MCP token A: only agent A's terminal; no raw terminals, no kill, no services.
    let mcp = [("X-Lucina-Origin", "mcp:tokA")];
    let list = http(&d, "GET", "/terminals", None, &mcp).json();
    let ids: Vec<&str> = list["terminals"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [ta.as_str()]);
    assert_eq!(http(&d, "GET", &format!("/terminals/{tb}/capture"), None, &mcp).status, 404);
    assert_eq!(http(&d, "POST", &format!("/terminals/{mine}/input"), Some(&json!({"data": "x"})), &mcp).status, 404);
    // It may read agent A's terminal, but the owner opened it: no typing into it.
    assert_eq!(http(&d, "GET", &format!("/terminals/{ta}/capture"), None, &mcp).status, 200);
    assert_eq!(http(&d, "POST", &format!("/terminals/{ta}/input"), Some(&json!({"keys": "Enter"})), &mcp).status, 403);
    assert_eq!(http(&d, "DELETE", &format!("/terminals/{ta}"), None, &mcp).status, 403);
    assert_eq!(http(&d, "POST", "/terminals", Some(&json!({"workspaceId": w1})), &mcp).status, 403);
    // Its scripts: the workspace where its agent runs, not the owner's other one.
    assert_eq!(http(&d, "GET", &format!("/workspaces/{w1}/scripts"), None, &mcp).status, 200);
    assert_eq!(http(&d, "GET", &format!("/workspaces/{w2}/scripts"), None, &mcp).status, 403);
    assert_eq!(http(&d, "POST", &format!("/workspaces/{w2}/scripts/hello/start"), Some(&json!({})), &mcp).status, 403);
    // A workspace it makes is its own (label mcp:tokA); token B gets nothing there,
    // and nobody but the owner can hand out that label.
    let three = root.join("three");
    std::fs::create_dir_all(&three).unwrap();
    std::fs::write(three.join("paseo.json"), r#"{"scripts": {"hello": {"command": "echo hi; sleep 30"}}}"#).unwrap();
    let made = http(&d, "POST", "/workspaces", Some(&json!({"isolation": "local", "path": three})), &mcp).json();
    let w3 = made["workspace"]["workspaceId"].as_str().unwrap().to_string();
    assert!(made["workspace"]["labels"].as_array().unwrap().iter().any(|l| l.as_str().unwrap().starts_with("mcp:")), "{made}");
    let started = http(&d, "POST", &format!("/workspaces/{w3}/scripts/hello/start"), Some(&json!({})), &mcp);
    assert_eq!(started.status, 200, "{}", started.body);
    let script_term = started.json()["terminalId"].as_str().unwrap().to_string();
    assert_eq!(http(&d, "GET", &format!("/terminals/{script_term}/capture"), None, &mcp).status, 200);
    assert_eq!(http(&d, "POST", &format!("/terminals/{script_term}/input"), Some(&json!({"keys": "Enter"})), &mcp).status, 200, "its own script's terminal");
    assert_eq!(http(&d, "POST", &format!("/terminals/{script_term}/input"), Some(&json!({"keys": "Enter"})), &[("X-Lucina-Origin", "mcp:tokB")]).status, 404);
    assert_eq!(http(&d, "GET", &format!("/workspaces/{w3}/scripts"), None, &[("X-Lucina-Origin", "mcp:tokB")]).status, 403);
    assert_eq!(http(&d, "POST", &format!("/workspaces/{w2}/labels"), Some(&json!({"label": "mcp:tokB", "assigned": true})), &[("X-Lucina-Origin", "mcp:tokB")]).status, 403);
    assert_eq!(http(&d, "GET", &format!("/workspaces/{w1}/services/hello/"), None, &mcp).status, 403);

    // Agent A (a process under its provider): its own terminal only, inside its workspace.
    let seen: Value = serde_json::from_str(&as_agent(&d, &a, &["terminal", "ls", "--all", "--json"])).unwrap();
    let ids: Vec<&str> = seen.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [ta.as_str()]);
    let other = as_agent(&d, &a, &["terminal", "send-keys", &tb, "echo pwned", "Enter", "--json"]);
    assert!(other.contains("TERMINAL_NOT_FOUND"), "{other}");
    let escape = as_agent(&d, &a, &["terminal", "create", "--workspace", &w1, "--cwd", "/", "--json"]);
    assert!(escape.contains("outside the workspace"), "{escape}");
    let wrong = as_agent(&d, &a, &["terminal", "create", "--workspace", &w2, "--json"]);
    assert!(wrong.contains("own workspace"), "{wrong}");
    let made: Value = serde_json::from_str(&as_agent(&d, &a, &["terminal", "create", "--workspace", &w1, "--cwd", &one.join("sub").display().to_string(), "--json"])).unwrap();
    assert_eq!(made["owner"], format!("agent:{a}"));
    assert_eq!(http(&d, "POST", &format!("/terminals/{}/input", made["id"].as_str().unwrap()), Some(&json!({"data": "x"})), &mcp).status, 200, "a terminal its agent opened");
    assert_eq!(made["agentId"], a.as_str());
    let other_ws = as_agent(&d, &a, &["script", "start", "hello", "--workspace", &w2, "--json"]);
    assert!(other_ws.contains("not yours"), "{other_ws}");

    // A process in the owner's terminal sees only that terminal, and can't answer permissions.
    let ext = http(&d, "POST", "/permissions", Some(&json!({"kind": "external", "source": "infisical-brokerd", "title": "t", "expiresInSec": 60})), &[]);
    assert_eq!(ext.status, 201, "{}", ext.body);
    let ext = ext.json()["id"].as_str().unwrap().to_string();
    let out = root.join("inside.json");
    post(&d, &format!("/terminals/{mine}/input"), json!({"data": format!("{LAGENT} terminal ls --all --json > {}; {LAGENT} permit allow {ext} --json > {}.permit 2>&1\r", out.display(), out.display())}));
    wait_for("the terminal's lagent", 10, || std::fs::read_to_string(format!("{}.permit", out.display())).is_ok_and(|s| !s.is_empty()));
    let inside: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let ids: Vec<&str> = inside.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [mine.as_str()]);
    let permit = std::fs::read_to_string(format!("{}.permit", out.display())).unwrap();
    assert!(permit.contains("PERMISSION_FORBIDDEN"), "{permit}");
    assert_eq!(get(&d, &format!("/permissions/{ext}"))["status"], "pending");

    // Cleanup: stopping agent A kills its terminals (also the one it opened), not B's.
    d.json(&["stop", &a]);
    let left: Vec<String> = get(&d, "/terminals")["terminals"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap().to_string()).collect();
    assert!(!left.contains(&ta) && !left.contains(&made["id"].as_str().unwrap().to_string()), "{left:?}");
    assert!(left.contains(&tb) && left.contains(&mine));
    d.json(&["archive", "--force", &b]);
    assert!(!get(&d, "/terminals")["terminals"].as_array().unwrap().iter().any(|t| t["id"] == tb.as_str()));
}

