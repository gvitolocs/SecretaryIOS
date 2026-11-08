//! The Juno shell tool's opt-in, end to end: an agent labelled `codecor.juno=bash`
//! is only created as a Claude agent in a mode that asks before each call — anything
//! else is refused with 403 `JUNO_TOOL_REFUSED`. (The daemon in tests runs with
//! `--no-real-providers`, so the mock provider stands in for "not claude".)

mod common;

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use common::Daemon;
use serde_json::{json, Value};

/// One HTTP request on the daemon's socket.
fn http(socket: &Path, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
    let mut s = UnixStream::connect(socket).expect("connect");
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n");
    if method == "POST" {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    req.push_str("\r\n");
    req.push_str(&body);
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let (head, rest) = raw.split_once("\r\n\r\n").expect("response");
    let status: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
    let text = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        rest.split_once("\r\n")
            .map(|(_, r)| r.rsplit_once("\r\n0\r\n").map(|(b, _)| b).unwrap_or(r))
            .unwrap_or("")
            .to_string()
    } else {
        rest.to_string()
    };
    (
        status,
        serde_json::from_str(text.trim()).unwrap_or(Value::Null),
    )
}

fn post_agent(d: &Daemon, body: &Value) -> (u16, Value) {
    http(&d.socket(), "POST", "/agents", Some(body))
}

#[test]
fn juno_label_refused_for_non_claude_agents() {
    let d = Daemon::start();
    // The label opts in, but the mock provider is not claude: 403 JUNO_TOOL_REFUSED.
    let (status, v) = post_agent(
        &d,
        &json!({"provider": "mock", "cwd": "/tmp", "labels": {"codecor.juno": "bash"}}),
    );
    assert_eq!(status, 403, "{v}");
    assert_eq!(v["error"]["code"], "JUNO_TOOL_REFUSED");
    assert!(
        v["error"]["message"].as_str().unwrap().contains(
            "The Juno shell tool needs a Claude agent in a mode that asks before each call"
        ),
        "{v}"
    );
    // A forbidden mode is refused the same way (still not claude here, but the check runs).
    let (status, v) = post_agent(
        &d,
        &json!({"provider": "mock", "cwd": "/tmp", "modeId": "bypassPermissions", "labels": {"codecor.juno": "bash"}}),
    );
    assert_eq!(status, 403);
    assert_eq!(v["error"]["code"], "JUNO_TOOL_REFUSED");
    // A tool policy that preapproves a juno tool is refused too.
    let (status, v) = post_agent(
        &d,
        &json!({"provider": "mock", "cwd": "/tmp", "labels": {"codecor.juno": "bash"}, "toolPolicy": {"preapproved": [{"kind": "mcp", "server": "juno", "tool": "juno_bash"}]}}),
    );
    assert_eq!(status, 403);
    assert_eq!(v["error"]["code"], "JUNO_TOOL_REFUSED");
    // Without the label (or with another value) the same request is fine.
    let (status, v) = post_agent(&d, &json!({"provider": "mock", "cwd": "/tmp"}));
    assert_eq!(status, 200, "{v}");
    let (status, v) = post_agent(
        &d,
        &json!({"provider": "mock", "cwd": "/tmp", "labels": {"codecor.juno": "no"}}),
    );
    assert_eq!(status, 200, "{v}");
}
