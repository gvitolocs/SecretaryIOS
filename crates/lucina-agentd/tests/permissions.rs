//! Permissions end to end: `lagent permit`, who may answer (never MCP, never a process
//! an agent started), one answer per request, expiry, tool-policy preapproval, and
//! the external requests the Infisical broker makes (`/permissions`).

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{Daemon, LAGENT};
use serde_json::{json, Value};

fn id_of(v: &Value) -> String {
    v["agentId"].as_str().expect("agentId").to_string()
}

/// One HTTP request on the daemon's socket, with extra headers.
fn http(socket: &Path, method: &str, path: &str, headers: &[(&str, &str)], body: Option<&Value>) -> (u16, Value) {
    let mut s = UnixStream::connect(socket).expect("connect");
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if method == "POST" {
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
        // One chunk is enough for these small bodies.
        rest.split_once("\r\n").map(|(_, r)| r.rsplit_once("\r\n0\r\n").map(|(b, _)| b).unwrap_or(r)).unwrap_or("").to_string()
    } else {
        rest.to_string()
    };
    (status, serde_json::from_str(text.trim()).unwrap_or(Value::Null))
}

fn pending(d: &Daemon, id: &str) -> Vec<Value> {
    d.json(&["inspect", id])["PendingPermissions"].as_array().cloned().unwrap_or_default()
}

#[test]
fn permit_allow_and_deny() {
    let d = Daemon::start();
    let run = d.json(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "emit synthetic plan approval"]);
    assert_eq!(run["status"], "permission");
    let id = id_of(&run);
    let ls = d.json(&["permit", "ls"]);
    assert_eq!(ls.as_array().unwrap().len(), 1);
    assert_eq!(ls[0]["name"], "MockPlanApproval");
    assert_eq!(ls[0]["agentShortId"], &id[..7]);
    assert_eq!(ls[0]["description"], "Review the proposed plan before implementation starts.");

    // deny needs a request id or --all
    let r = d.lagent(&["permit", "deny", &id, "--json"]);
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("MISSING_ARGUMENT"), "{}", r.stderr);

    let allowed = d.json(&["permit", "allow", &id[..7]]);
    assert_eq!(allowed[0]["result"], "allowed");
    assert!(allowed[0]["requestId"].as_str().unwrap().starts_with("mock-plan-"));
    assert_eq!(d.json(&["wait", &id])["status"], "idle");
    assert!(pending(&d, &id).is_empty());
    assert_eq!(d.json(&["permit", "ls"]), json!([]));
    let r = d.lagent(&["permit", "allow", &id, "--json"]);
    assert_eq!(r.code, 1);
    assert!(r.stderr.contains("NO_PENDING_PERMISSIONS"), "{}", r.stderr);

    let q = d.json(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "emit synthetic questions"]);
    assert_eq!(q["status"], "permission");
    let qid = id_of(&q);
    let req = pending(&d, &qid)[0]["id"].as_str().unwrap().to_string();
    let r = d.lagent(&["permit", "deny", &qid, "nope", "--json"]);
    assert!(r.stderr.contains("PERMISSION_NOT_FOUND"), "{}", r.stderr);
    let denied = d.json(&["permit", "deny", &qid, &req[..12], "--message", "not now"]);
    assert_eq!(denied[0]["result"], "denied");
    assert!(pending(&d, &qid).is_empty());

    let all = http(&d.socket(), "GET", "/permissions?all=true", &[], None).1;
    let list = all["permissions"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|p| p["resolution"]["by"] == "local"));
    assert!(list.iter().any(|p| p["status"] == "denied" && p["resolution"]["message"] == "not now"));
}

#[test]
fn only_the_owner_answers() {
    let d = Daemon::start();
    let run = d.json(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "emit synthetic plan approval"]);
    let id = id_of(&run);
    let req = pending(&d, &id)[0]["id"].as_str().unwrap().to_string();
    let path = format!("/agents/{id}/permissions/{req}");
    let allow = json!({"behavior": "allow"});

    for origin in ["mcp:hermes", "mcp", "something-else"] {
        let (status, body) = http(&d.socket(), "POST", &path, &[("X-Lucina-Origin", origin)], Some(&allow));
        assert_eq!(status, 403, "{origin}: {body}");
        assert_eq!(body["error"]["code"], "PERMISSION_FORBIDDEN");
    }
    assert_eq!(pending(&d, &id).len(), 1, "still pending");

    // A process an agent started (here: a fake provider group) can't answer either,
    // whatever header it sends.
    let child = Command::new("sh")
        .args(["-c", &format!("sleep 1; exec {LAGENT} permit allow {id} --json")])
        .env("LUCINA_AGENTD_SOCKET", d.socket())
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
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("PERMISSION_FORBIDDEN"), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(pending(&d, &id).len(), 1, "still pending");

    // An owner device (through secretary-server) can.
    let (status, body) = http(&d.socket(), "POST", &path, &[("X-Lucina-Origin", "device:dev_1")], Some(&allow));
    assert_eq!(status, 200, "{body}");
    assert!(pending(&d, &id).is_empty());
    let all = http(&d.socket(), "GET", "/permissions?all=true", &[], None).1;
    assert_eq!(all["permissions"][0]["resolution"]["by"], "owner_device");
}

#[test]
fn unanswered_requests_are_denied_when_they_expire() {
    let d = Daemon::start_with_env(&[("LUCINA_AGENTD_PERMISSION_TTL_SECS", "1".into())]);
    let run = d.json(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "emit synthetic plan approval"]);
    let id = id_of(&run);
    assert_eq!(pending(&d, &id).len(), 1);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !pending(&d, &id).is_empty() {
        assert!(Instant::now() < deadline, "the request did not expire");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(d.json(&["wait", &id])["status"], "idle");
    let all = http(&d.socket(), "GET", "/permissions?all=true", &[], None).1;
    let p = &all["permissions"][0];
    assert_eq!(p["status"], "expired");
    assert_eq!(p["resolution"]["behavior"], "deny");
    assert_eq!(p["resolution"]["by"], "expiry");
    // A timeline notification (`lagent logs`, like Paseo's, doesn't print those).
    let timeline = http(&d.socket(), "GET", &format!("/agents/{id}/timeline"), &[], None).1;
    assert!(timeline.to_string().contains("Permission request expired: Plan was denied."), "{timeline}");
}

#[test]
fn tool_policy_preapproves_exact_mcp_tools() {
    let d = Daemon::start();
    let ok = d.json(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "--preapprove", "lucina/notes_search", "lucina mock mcp permission lucina/notes_search"]);
    assert_eq!(ok["status"], "completed", "{ok}");
    let all = http(&d.socket(), "GET", "/permissions?all=true", &[], None).1;
    assert_eq!(all["permissions"][0]["resolution"]["by"], "policy");
    assert_eq!(all["permissions"][0]["name"], "mcp__lucina__notes_search");

    let gated = d.json(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "--preapprove", "lucina/notes_search", "lucina mock mcp permission lucina/note_save"]);
    assert_eq!(gated["status"], "permission", "a different tool still asks");
    let r = d.lagent(&["run", "--provider", "mock", "--cwd", "/tmp", "--preapprove", "lucina", "x", "--json"]);
    assert!(r.stderr.contains("INVALID_PREAPPROVE"), "{}", r.stderr);
}

#[test]
fn external_requests_from_the_broker() {
    let d = Daemon::start();
    let sock = d.socket();
    // Watch the stream for the permission events.
    let mut stream = UnixStream::connect(&sock).unwrap();
    stream.write_all(b"GET /stream HTTP/1.1\r\nHost: x\r\nAccept: text/event-stream\r\n\r\n").unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    std::thread::sleep(Duration::from_millis(200));

    let req = json!({"kind": "external", "source": "infisical-brokerd", "title": "Hand LUCINA_MCP_AGENTS_TOKEN to lucina-agentd", "description": "exec for lucina-agentd.service", "details": {"secret": "agents/prod#LUCINA_MCP_AGENTS_TOKEN"}, "expiresInSec": 60});
    let (status, body) = http(&sock, "POST", "/permissions", &[("X-Lucina-Origin", "mcp:hermes")], Some(&req));
    assert_eq!((status, body["error"]["code"].clone()), (403, json!("PERMISSION_FORBIDDEN")), "MCP can't ask the owner");
    let (status, bad) = http(&sock, "POST", "/permissions", &[], Some(&json!({"kind": "external", "source": "Bad Source", "title": "x"})));
    assert_eq!(status, 400, "{bad}");

    let (status, view) = http(&sock, "POST", "/permissions", &[], Some(&req));
    assert_eq!(status, 201, "{view}");
    let id = view["id"].as_str().unwrap().to_string();
    assert!(id.starts_with("ext_"));
    assert_eq!(view["status"], "pending");
    assert_eq!(view["source"], "infisical-brokerd");

    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut saw = false;
    let mut line = String::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.contains("\"type\":\"permission\"") && line.contains(&id) {
            saw = true;
            break;
        }
    }
    assert!(saw, "the stream carries the external request");

    let ls = d.json(&["permit", "ls", "--external"]);
    assert!(ls.as_array().unwrap().iter().any(|p| p["id"] == id.as_str() && p["agentShortId"] == "infisical-brokerd"));
    assert_eq!(d.json(&["permit", "ls"]), json!([]), "plain `permit ls` stays Paseo's (agents only)");

    let path = format!("/permissions/{id}");
    let (status, _) = http(&sock, "POST", &path, &[("X-Lucina-Origin", "mcp")], Some(&json!({"behavior": "allow"})));
    assert_eq!(status, 403);

    // A waiting caller gets the answer as soon as it lands.
    let sock2 = sock.clone();
    let path2 = path.clone();
    let waiter = std::thread::spawn(move || http(&sock2, "GET", &format!("{path2}?waitMs=5000"), &[], None));
    std::thread::sleep(Duration::from_millis(300));
    let allowed = d.json(&["permit", "allow", &id]);
    assert_eq!(allowed[0]["result"], "allowed");
    let (status, waited) = waiter.join().unwrap();
    assert_eq!(status, 200);
    assert_eq!(waited["status"], "allowed");
    assert_eq!(waited["resolution"]["by"], "local");

    let (status, again) = http(&sock, "POST", &path, &[], Some(&json!({"behavior": "deny"})));
    assert_eq!((status, again["error"]["code"].clone()), (409, json!("PERMISSION_ALREADY_ANSWERED")), "answered once");
    let (status, _) = http(&sock, "GET", "/permissions/ext_nope", &[], None);
    assert_eq!(status, 404);
}
