//! Phase 7 on the daemon: external agents (Secretary Code sessions) owned by the device
//! that registers them, commands queued for that device until it acknowledges them,
//! the resumable stream, and `service:` callers that may only ask the owner.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use common::Daemon;
use serde_json::{json, Value};

const MAC: &str = "device:dev_mac";
const PHONE: &str = "device:dev_phone";
const MCP: &str = "mcp:hermes";

fn http(socket: &Path, method: &str, path: &str, origin: Option<&str>, body: Option<&Value>) -> (u16, Value) {
    let mut s = UnixStream::connect(socket).expect("connect");
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

/// One SSE frame: its `id:`, `event:` and parsed `data:`.
#[derive(Debug, Clone)]
struct Frame {
    id: Option<String>,
    event: Option<String>,
    data: Value,
}

/// Open `/stream` and read frames until `stop` says so or `limit` passes.
fn read_stream(socket: &Path, origin: Option<&str>, last_event_id: Option<&str>, limit: Duration, stop: impl Fn(&[Frame]) -> bool) -> Vec<Frame> {
    let mut s = UnixStream::connect(socket).expect("connect");
    let mut req = "GET /stream HTTP/1.1\r\nHost: x\r\nAccept: text/event-stream\r\n".to_string();
    if let Some(o) = origin {
        req.push_str(&format!("X-Lucina-Origin: {o}\r\n"));
    }
    if let Some(l) = last_event_id {
        req.push_str(&format!("Last-Event-ID: {l}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).unwrap();
    s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let mut reader = BufReader::new(s);
    let deadline = Instant::now() + limit;
    let mut frames = Vec::new();
    let (mut id, mut event, mut data) = (None, None, String::new());
    let mut line = String::new();
    while Instant::now() < deadline && !stop(&frames) {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => continue,
        }
        let l = line.trim_end_matches(['\r', '\n']);
        if let Some(v) = l.strip_prefix("id: ").or_else(|| l.strip_prefix("id:")) {
            id = Some(v.to_string());
        } else if let Some(v) = l.strip_prefix("event: ").or_else(|| l.strip_prefix("event:")) {
            event = Some(v.to_string());
        } else if let Some(v) = l.strip_prefix("data: ").or_else(|| l.strip_prefix("data:")) {
            data.push_str(v);
        } else if l.is_empty() && !data.is_empty() {
            frames.push(Frame { id: id.take(), event: event.take(), data: serde_json::from_str(&data).unwrap_or(Value::Null) });
            data.clear();
        }
    }
    frames
}

fn register(d: &Daemon, origin: Option<&str>, key: &str) -> (u16, Value) {
    http(
        &d.socket(),
        "POST",
        "/agents",
        origin,
        Some(&json!({"provider": "secretary-code", "external": true, "host": "mac", "cwd": "/Users/giuseppe/Projects/x", "title": "Mac session", "idempotencyKey": key})),
    )
}

fn post_events(d: &Daemon, id: &str, origin: Option<&str>, events: Value) -> (u16, Value) {
    http(&d.socket(), "POST", &format!("/agents/{id}/events"), origin, Some(&json!({"events": events})))
}

fn snapshot(d: &Daemon, id: &str) -> Value {
    http(&d.socket(), "GET", &format!("/agents/{id}"), None, None).1
}

fn commands(d: &Daemon, origin: &str) -> Vec<Value> {
    let (status, v) = http(&d.socket(), "GET", "/commands", Some(origin), None);
    assert_eq!(status, 200, "{v}");
    v["commands"].as_array().cloned().unwrap_or_default()
}

fn wait_until(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn only_a_device_registers_and_owns_an_external_agent() {
    let d = Daemon::start();
    // lagent (local), MCP, and an agent-less unknown origin cannot register one.
    for origin in [None, Some(MCP), Some("device:"), Some("service:infisical-broker")] {
        let (status, v) = register(&d, origin, "k0");
        assert_eq!(status, 403, "{origin:?}: {v}");
        assert_eq!(v["error"]["code"], "NOT_OWNER");
    }
    let (status, a) = register(&d, Some(MAC), "k1");
    assert_eq!(status, 200, "{a}");
    assert_eq!(a["provider"], "secretary-code");
    assert_eq!(a["external"], true);
    assert_eq!(a["ownerDeviceId"], "dev_mac");
    assert_eq!(a["host"], "mac");
    assert_eq!(a["status"], "idle");
    assert!(a.get("workspaceId").is_none(), "a Mac path is not a nezopt workspace");
    // The same key answers with the same agent.
    assert_eq!(register(&d, Some(MAC), "k1").1["id"], a["id"]);
    // The owner can't be chosen in the body.
    let (_, b) = http(&d.socket(), "POST", "/agents", Some(PHONE),
        Some(&json!({"provider": "secretary-code", "external": true, "host": "mac", "cwd": "/x", "ownerDeviceId": "dev_mac"})));
    assert_eq!(b["ownerDeviceId"], "dev_phone");

    for (body, why) in [
        (json!({"provider": "secretary-code", "cwd": "/x"}), "not external"),
        (json!({"provider": "mock", "external": true, "host": "mac", "cwd": "/x"}), "wrong provider"),
        (json!({"provider": "secretary-code", "external": true, "host": "pc", "cwd": "/x"}), "unknown host"),
        (json!({"provider": "secretary-code", "external": true, "host": "mac", "cwd": "/x", "prompt": "go"}), "prompt"),
        (json!({"provider": "secretary-code", "external": true, "host": "mac", "cwd": ""}), "no cwd"),
    ] {
        let (status, v) = http(&d.socket(), "POST", "/agents", Some(MAC), Some(&body));
        assert_eq!(status, 400, "{why}: {v}");
    }
    let items = http(&d.socket(), "GET", "/agents/items", None, None).1;
    let item = items["agents"].as_array().unwrap().iter().find(|i| i["id"] == a["id"]).unwrap().clone();
    assert_eq!((item["external"].clone(), item["ownerDeviceId"].clone()), (json!(true), json!("dev_mac")));
}

#[test]
fn only_the_owner_posts_events_and_they_drive_the_agent() {
    let d = Daemon::start();
    let id = register(&d, Some(MAC), "k").1["id"].as_str().unwrap().to_string();
    let turn = json!([
        {"type": "turn_started", "provider": "secretary-code", "turnId": "t1"},
        {"type": "timeline", "provider": "secretary-code", "turnId": "t1", "item": {"type": "user_message", "text": "fix the bug"}},
    ]);
    for origin in [None, Some(PHONE), Some(MCP), Some("service:infisical-broker")] {
        let (status, v) = post_events(&d, &id, origin, turn.clone());
        assert_eq!(status, 403, "{origin:?}: {v}");
    }
    let (status, v) = post_events(&d, &id, Some(MAC), turn);
    assert_eq!((status, v["applied"].clone()), (200, json!(2)), "{v}");
    assert_eq!(snapshot(&d, &id)["status"], "running");
    let (status, v) = post_events(&d, &id, Some(MAC), json!([{"type": "attention_required", "provider": "secretary-code",
        "reason": "finished", "timestamp": "2026-10-28T00:00:00.000Z", "shouldNotify": true}]));
    assert_eq!(status, 400, "the daemon derives attention itself: {v}");
    post_events(&d, &id, Some(MAC), json!([
        {"type": "timeline", "provider": "secretary-code", "turnId": "t1", "item": {"type": "assistant_message", "text": "Fixed."}},
        {"type": "turn_completed", "provider": "secretary-code", "turnId": "t1"},
    ]));
    let snap = snapshot(&d, &id);
    assert_eq!(snap["status"], "idle");
    assert_eq!(snap["attentionReason"], "finished");
    let tl = http(&d.socket(), "GET", &format!("/agents/{id}/timeline"), None, None).1;
    let kinds: Vec<&str> = tl["rows"].as_array().unwrap().iter().map(|r| r["item"]["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["user_message", "assistant_message"], "the user message is the device's, not duplicated");
    // A non-external agent takes no posted events.
    let run = d.json(&["run", "--provider", "mock/e2e-fast-stream", "--cwd", "/tmp", "hello"]);
    let (status, _) = post_events(&d, run["agentId"].as_str().unwrap(), Some(MAC), json!([]));
    assert_eq!(status, 400);
    // Rename.
    let (status, v) = http(&d.socket(), "PATCH", &format!("/agents/{id}"), Some(MAC), Some(&json!({"title": "Bug hunt"})));
    assert_eq!((status, v["title"].clone()), (200, json!("Bug hunt")));
    let (status, _) = http(&d.socket(), "PATCH", &format!("/agents/{id}"), Some(MAC), Some(&json!({"title": "a\nb"})));
    assert_eq!(status, 400);
}

#[test]
fn sends_steers_and_stops_become_commands_for_the_owner_only() {
    let d = Daemon::start();
    let id = register(&d, Some(MAC), "k").1["id"].as_str().unwrap().to_string();
    let (status, v) = http(&d.socket(), "POST", &format!("/agents/{id}/send"), Some(PHONE), Some(&json!({"prompt": "add tests"})));
    assert_eq!((status, v["disposition"].clone()), (200, json!("queued")), "{v}");
    assert_eq!(snapshot(&d, &id)["status"], "idle", "nothing runs until the Mac says so");
    let mac = commands(&d, MAC);
    assert_eq!(mac.len(), 1);
    assert_eq!(mac[0]["command"], json!({"kind": "send", "prompt": "add tests"}));
    assert_eq!(mac[0]["agentId"], id.as_str());
    assert!(mac[0]["createdAt"].is_string() && mac[0]["expiresAt"].is_string());
    assert!(commands(&d, PHONE).is_empty(), "the phone has no commands");
    let (status, _) = http(&d.socket(), "GET", "/commands", Some(MCP), None);
    assert_eq!(status, 403);

    // While it runs: steer is a steer; the default (interrupt) is stop + send; stop is stop.
    post_events(&d, &id, Some(MAC), json!([{"type": "turn_started", "provider": "secretary-code", "turnId": "t"}]));
    http(&d.socket(), "POST", &format!("/agents/{id}/send"), Some(PHONE), Some(&json!({"prompt": "also docs", "activeTurnBehavior": "steer"})));
    http(&d.socket(), "POST", &format!("/agents/{id}/send"), Some(PHONE), Some(&json!({"prompt": "start over"})));
    let (_, stopped) = http(&d.socket(), "POST", &format!("/agents/{id}/stop"), Some(PHONE), None);
    assert_eq!(stopped["stopped"], true);
    let kinds: Vec<Value> = commands(&d, MAC).iter().map(|c| c["command"]["kind"].clone()).collect();
    assert_eq!(kinds, [json!("send"), json!("steer"), json!("stop"), json!("send"), json!("stop")]);

    // Acknowledgements: only the owner, once is enough, unknown ids are 404.
    let first = commands(&d, MAC)[0]["commandId"].as_str().unwrap().to_string();
    let ack = |origin: &str, cid: &str| http(&d.socket(), "POST", &format!("/agents/{id}/commands/{cid}/ack"), Some(origin), None);
    assert_eq!(ack(PHONE, &first).0, 403);
    assert_eq!(ack(MCP, &first).0, 403);
    assert_eq!(ack(MAC, "cmd_unknown").0, 404);
    let (status, v) = ack(MAC, &first);
    assert_eq!((status, v["acked"].clone()), (200, json!(true)), "{v}");
    assert_eq!(ack(MAC, &first).0, 200, "acknowledging twice is fine");
    assert_eq!(commands(&d, MAC).len(), 4);
    let (status, _) = http(&d.socket(), "POST", &format!("/agents/{id}/commands/{first}/ack"), None, None);
    assert_eq!(status, 403, "lagent is not the owner device either");
}

#[test]
fn the_stream_sends_commands_only_to_the_owner_device() {
    let d = Daemon::start();
    let id = register(&d, Some(MAC), "k").1["id"].as_str().unwrap().to_string();
    http(&d.socket(), "POST", &format!("/agents/{id}/send"), Some(PHONE), Some(&json!({"prompt": "queued while asleep"})));
    // A device connecting without Last-Event-ID gets its waiting commands first.
    let has_command = |f: &[Frame]| f.iter().any(|f| f.data["type"] == "agent_command");
    let mac = read_stream(&d.socket(), Some(MAC), None, Duration::from_secs(3), has_command);
    let c = mac.iter().find(|f| f.data["type"] == "agent_command").expect("waiting command");
    assert_eq!(c.data["command"]["prompt"], "queued while asleep");
    assert!(c.id.is_none(), "a re-sent waiting command carries no id");
    assert!(c.data.get("deviceId").is_none());
    // Live: the phone never sees the Mac's commands, the Mac does, with an id.
    let socket = d.socket();
    let watcher = std::thread::spawn({
        let socket = socket.clone();
        move || read_stream(&socket, Some(PHONE), None, Duration::from_millis(1500), |_| false)
    });
    let mac_live = std::thread::spawn({
        let socket = socket.clone();
        move || read_stream(&socket, Some(MAC), None, Duration::from_secs(3), |f| f.iter().filter(|f| f.data["type"] == "agent_command").count() >= 2)
    });
    std::thread::sleep(Duration::from_millis(300));
    http(&socket, "POST", &format!("/agents/{id}/send"), Some(PHONE), Some(&json!({"prompt": "live one"})));
    http(&socket, "PATCH", &format!("/agents/{id}"), Some(MAC), Some(&json!({"title": "renamed"})));
    let phone = watcher.join().unwrap();
    assert!(!phone.iter().any(|f| f.data["type"] == "agent_command"), "{phone:?}");
    assert!(phone.iter().any(|f| f.data["type"] == "agent_state"), "the phone sees everything else");
    let mac = mac_live.join().unwrap();
    let live = mac.iter().find(|f| f.data["type"] == "agent_command" && f.data["command"]["prompt"] == "live one").expect("live command");
    assert!(live.id.is_some());
}

#[test]
fn reconnecting_with_last_event_id_replays_what_was_missed() {
    let d = Daemon::start();
    let first = read_stream(&d.socket(), None, None, Duration::from_millis(500), |f| f.len() >= 2);
    // Nothing happened yet: produce events, then read them.
    assert!(first.is_empty());
    let socket = d.socket();
    let reader = std::thread::spawn({
        let socket = socket.clone();
        move || read_stream(&socket, None, None, Duration::from_secs(5), |f| f.len() >= 3)
    });
    std::thread::sleep(Duration::from_millis(300));
    let id = register(&d, Some(MAC), "k").1["id"].as_str().unwrap().to_string();
    post_events(&d, &id, Some(MAC), json!([{"type": "turn_started", "provider": "secretary-code", "turnId": "t"}]));
    let seen = reader.join().unwrap();
    assert!(seen.len() >= 3, "{seen:?}");
    let ids: Vec<u64> = seen.iter().map(|f| f.id.as_deref().unwrap().parse().unwrap()).collect();
    assert!(ids.windows(2).all(|w| w[1] == w[0] + 1), "consecutive ids: {ids:?}");
    // More events while "disconnected".
    post_events(&d, &id, Some(MAC), json!([{"type": "turn_completed", "provider": "secretary-code", "turnId": "t"}]));
    let replay = read_stream(&d.socket(), None, Some(&ids[0].to_string()), Duration::from_secs(3), |f| {
        f.iter().any(|f| f.data["type"] == "agent_state" && f.data["agent"]["status"] == "idle")
    });
    let replay_ids: Vec<u64> = replay.iter().map(|f| f.id.as_deref().unwrap().parse().unwrap()).collect();
    assert_eq!(replay_ids[0], ids[0] + 1, "starts right after the last seen id");
    assert!(replay_ids.windows(2).all(|w| w[1] == w[0] + 1), "{replay_ids:?}");
    assert!(replay.iter().any(|f| f.data["type"] == "agent_state" && f.data["agent"]["status"] == "idle"));
    // An id this daemon never issued, or garbage: a reset, with an id to resume from.
    for bad in ["1", "garbage", &(ids.last().unwrap() + 1000).to_string()] {
        let r = read_stream(&d.socket(), None, Some(bad), Duration::from_secs(2), |f| !f.is_empty());
        assert_eq!(r[0].event.as_deref(), Some("stream.reset"), "{bad}: {r:?}");
        assert_eq!(r[0].data["type"], "stream.reset");
        assert!(r[0].id.is_some());
    }
}

#[test]
fn a_client_too_far_behind_gets_a_reset() {
    let d = Daemon::start_with_env(&[("LUCINA_AGENTD_STREAM_KEEP", "3".into())]);
    let reader = std::thread::spawn({
        let socket = d.socket();
        move || read_stream(&socket, None, None, Duration::from_secs(4), |f| !f.is_empty())
    });
    std::thread::sleep(Duration::from_millis(300));
    let id = register(&d, Some(MAC), "k").1["id"].as_str().unwrap().to_string();
    let seen = reader.join().unwrap();
    let old = seen[0].id.clone().unwrap();
    for n in 0..4 {
        http(&d.socket(), "PATCH", &format!("/agents/{id}"), Some(MAC), Some(&json!({"title": format!("t{n}")})));
    }
    let r = read_stream(&d.socket(), None, Some(&old), Duration::from_secs(2), |f| !f.is_empty());
    assert_eq!(r[0].event.as_deref(), Some("stream.reset"), "{r:?}");
}

#[test]
fn permission_answers_for_external_agents_are_queued_and_expire_as_denied() {
    let d = Daemon::start_with_env(&[("LUCINA_AGENTD_COMMAND_TTL_SECS", "2".into())]);
    let id = register(&d, Some(MAC), "k").1["id"].as_str().unwrap().to_string();
    let ask = |rid: &str| {
        post_events(&d, &id, Some(MAC), json!([
            {"type": "turn_started", "provider": "secretary-code", "turnId": "t"},
            {"type": "permission_requested", "provider": "secretary-code",
             "request": {"id": rid, "provider": "secretary-code", "name": "Bash", "kind": "tool", "input": {"command": "rm -rf build"}}},
        ]))
    };
    assert_eq!(ask("r1").0, 200);
    let snap = snapshot(&d, &id);
    assert_eq!(snap["pendingPermissions"][0]["id"], "r1");
    // MCP never answers; the phone does, and the answer goes to the Mac.
    let answer = |origin: &str, rid: &str| {
        http(&d.socket(), "POST", &format!("/agents/{id}/permissions/{rid}"), Some(origin), Some(&json!({"behavior": "allow"})))
    };
    assert_eq!(answer(MCP, "r1").0, 403);
    let (status, v) = answer(PHONE, "r1");
    assert_eq!((status, v["behavior"].clone()), (200, json!("allow")), "{v}");
    let cmd = commands(&d, MAC).into_iter().find(|c| c["command"]["kind"] == "permission_response").expect("queued answer");
    assert_eq!(cmd["command"]["requestId"], "r1");
    assert_eq!(cmd["command"]["response"]["behavior"], "allow");
    assert_eq!(answer(PHONE, "r1").0, 409, "one answer per request");
    // The Mac applies it and says so.
    post_events(&d, &id, Some(MAC), json!([{"type": "permission_resolved", "provider": "secretary-code", "requestId": "r1",
        "resolution": {"behavior": "allow"}}]));
    assert_eq!(snapshot(&d, &id)["pendingPermissions"], json!([]));

    // A second answer the Mac never picks up: after the TTL it counts as denied.
    assert_eq!(ask("r2").0, 200);
    assert_eq!(answer(PHONE, "r2").0, 200);
    let cid = commands(&d, MAC).into_iter().find(|c| c["command"]["requestId"] == "r2").unwrap()["commandId"].as_str().unwrap().to_string();
    assert!(wait_until(Duration::from_secs(5), || snapshot(&d, &id)["pendingPermissions"] == json!([])), "expired answer withdrawn");
    assert!(commands(&d, MAC).is_empty(), "expired commands are no longer offered");
    let (status, v) = http(&d.socket(), "POST", &format!("/agents/{id}/commands/{cid}/ack"), Some(MAC), None);
    assert_eq!((status, v["error"]["code"].clone()), (410, json!("COMMAND_EXPIRED")));
}

#[test]
fn commands_and_external_agents_survive_a_restart() {
    let mut d = Daemon::start();
    let id = register(&d, Some(MAC), "k").1["id"].as_str().unwrap().to_string();
    http(&d.socket(), "POST", &format!("/agents/{id}/send"), Some(PHONE), Some(&json!({"prompt": "while the daemon restarts"})));
    d.restart();
    let snap = snapshot(&d, &id);
    assert_eq!((snap["status"].clone(), snap["ownerDeviceId"].clone()), (json!("closed"), json!("dev_mac")));
    assert_eq!(commands(&d, MAC).len(), 1, "the Mac still gets it");
    // The Mac reports again: the agent comes back without a process.
    let (status, v) = post_events(&d, &id, Some(MAC), json!([{"type": "turn_started", "provider": "secretary-code", "turnId": "t2"}]));
    assert_eq!(status, 200, "{v}");
    assert_eq!(snapshot(&d, &id)["status"], "running");
    // Only the owner, still.
    assert_eq!(post_events(&d, &id, Some(PHONE), json!([])).0, 403);
}

#[test]
fn a_service_may_ask_the_owner_but_never_answer() {
    let d = Daemon::start();
    let req = json!({"kind": "external", "source": "infisical-brokerd", "title": "Read OPENAI_API_KEY", "expiresInSec": 60});
    let (status, v) = http(&d.socket(), "POST", "/permissions", Some("service:infisical-broker"), Some(&req));
    assert_eq!(status, 201, "{v}");
    let pid = v["id"].as_str().unwrap().to_string();
    for origin in ["service:infisical-broker", MCP] {
        let (status, _) = http(&d.socket(), "POST", &format!("/permissions/{pid}"), Some(origin), Some(&json!({"behavior": "allow"})));
        assert_eq!(status, 403, "{origin}");
    }
    let (status, _) = http(&d.socket(), "POST", "/permissions", Some(MCP), Some(&req));
    assert_eq!(status, 403);
    let (status, v) = http(&d.socket(), "POST", "/permissions", Some("service:Not Valid"), Some(&req));
    assert_eq!(status, 403, "{v}");
    let (status, v) = http(&d.socket(), "POST", &format!("/permissions/{pid}"), Some(PHONE), Some(&json!({"behavior": "deny"})));
    assert_eq!((status, v["status"].clone()), (200, json!("denied")));
}
