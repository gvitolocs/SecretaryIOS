//! "Post it in Lucina agents" from the owner's own ChatGPT: a trusted Lucina MCP relay runs his
//! Agents, on the JSON planner (the reach loop) and in tools mode (the offered registry). An agent's
//! caller (claude-code, an unknown one) does not even when `LUCINA_MCP_TRUSTED=true`, and neither
//! does a trusted app when the relay is not trusted.
//!
//! The model is the fake DeepSeek of `tests/common`; lucina-agentd is a stand-in on a private unix
//! socket in a temp dir, which also lets the case without an agent id run. The live daemon, model
//! and services are never touched.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex, OnceLock};

use common::{harness, script, H};
use lucina_server::fake::Script;
use lucina_server::PlannerMode;
use serde_json::{json, Value};

/// What the stand-in daemon answers. A run names the agent that took it, except a task whose text
/// says `no-id`: that one answers 2xx with nothing, the way a stuck agentd does.
fn answer(method: &str, path: &str, body: &str) -> Value {
    let seg: Vec<&str> = path
        .split('?')
        .next()
        .unwrap_or("")
        .trim_matches('/')
        .split('/')
        .collect();
    match (method, seg.as_slice()) {
        // A run names the agent that took it, except a task whose text says `no-id`: that one is
        // answered 2xx with nothing, the way a stuck agentd does.
        ("POST", ["agents"]) => {
            if body.contains("no-id") {
                json!({})
            } else {
                json!({"id": "ag-run-1", "status": "running"})
            }
        }
        ("GET", ["agents"]) => json!({"agents": [
            {"id": "ag-listed-1", "title": "login fix", "provider": "claude-session",
             "status": "idle", "labels": {"branch": "login-fix"}}
        ]}),
        (_, ["agents", .., "timeline"]) => json!({"rows": []}),
        ("POST", ["agents", _id, "send"]) => json!({"ok": true}),
        ("GET", ["agents", id]) => json!({"id": id, "provider": "opencode", "status": "idle"}),
        _ => json!({"ok": true}),
    }
}

/// The stand-in daemon of the whole binary: one socket, one thread per connection, every request
/// recorded. The cases tell their calls apart by the marker in their own text.
struct Agentd {
    seen: Arc<Mutex<Vec<String>>>,
}

impl Agentd {
    fn calls(&self, marker: &str) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains(marker))
            .cloned()
            .collect()
    }
}

fn agentd() -> &'static Agentd {
    static ONCE: OnceLock<Agentd> = OnceLock::new();
    ONCE.get_or_init(|| {
        // Leaked: the socket lives as long as the process, shared by every harness of this binary.
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let socket = dir.path().join("agentd.sock");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let listener = UnixListener::bind(&socket).unwrap();
        std::thread::spawn({
            let seen = seen.clone();
            move || {
                for conn in listener.incoming() {
                    let Ok(mut conn) = conn else { continue };
                    let seen = seen.clone();
                    std::thread::spawn(move || {
                        let mut rd = BufReader::new(conn.try_clone().unwrap());
                        let mut line = String::new();
                        if rd.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        let mut len = 0usize;
                        loop {
                            let mut h = String::new();
                            if rd.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                                break;
                            }
                            if h.to_ascii_lowercase().starts_with("content-length:") {
                                len = h.split_once(':').map(|(_, v)| v.trim()).unwrap_or("").parse().unwrap_or(0);
                            }
                        }
                        let mut raw = vec![0u8; len];
                        if len > 0 {
                            rd.read_exact(&mut raw).ok();
                        }
                        let body = String::from_utf8_lossy(&raw).to_string();
                        let mut it = line.split_whitespace();
                        let method = it.next().unwrap_or("").to_owned();
                        let path = it.next().unwrap_or("").to_owned();
                        let out = answer(&method, &path, if method == "POST" { &body } else { "" });
                        seen.lock().unwrap().push(format!("{method} {path} {body}"));
                        let text = out.to_string();
                        let _ = conn.write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                                text.len()
                            )
                            .as_bytes(),
                        );
                    });
                }
            }
        });
        std::env::set_var("LUCINA_AGENTD_SOCKET", &socket);
        Agentd { seen }
    })
}

/// An MCP relay turn: secretary-mcp's caller marker, the owner's own words after it.
async fn relay(h: &H, caller: &str, text: &str) -> Value {
    let (s, b) = h
        .post(
            "/chat",
            json!({"message": format!("[via Lucina MCP, from {caller}] {text}"),
                   "sessionId": "test", "channel": "web"}),
        )
        .await;
    assert_eq!(s, 200, "{b}");
    b["data"].clone()
}

/// The loop's model calls: the tool ask, then what she answers after its result. The other kinds
/// answer that nothing was run, so a turn that does not reach the loop says so plainly.
fn rules(tool: &str, args: Value) -> Script {
    script(json!({"rules": [
        {"kind": "agent", "afterTool": false,
         "toolCalls": [{"name": tool, "arguments": args}]},
        {"kind": "agent", "afterTool": true, "content": "Done: the tool answered."},
        {"kind": "intent", "content": "{\"intent\":\"agent_message\",\"confidence\":0.9,\"reply_mode\":\"chat\"}"},
        {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"Nothing was run here.\"}"},
        {"kind": "reply", "content": "Nothing was run here."}
    ]}))
}

/// What the tools answered the model, as this turn's harness saw them.
fn tool_results(h: &H) -> Vec<Value> {
    h.fake
        .requests()
        .iter()
        .flat_map(|q| q["messages"].as_array().into_iter().flatten())
        .filter(|m| m["role"] == "tool")
        .filter_map(|m| {
            m["content"]
                .as_str()
                .and_then(|c| serde_json::from_str(c).ok())
        })
        .collect()
}

/// The answer the model got for its `lucina_agents` call.
fn agents_answer(h: &H) -> Value {
    tool_results(h)
        .into_iter()
        .find(|v| {
            v.get("status").is_some() || v.get("agents").is_some() || v.get("recent").is_some()
        })
        .expect("the tool answered")
}

fn harness_trusted(args: Value, planner: PlannerMode) -> impl std::future::Future<Output = H> {
    harness(rules("lucina_agents", args), move |c| {
        c.planner = planner;
        c.mcp_trusted = true;
    })
}

#[tokio::test]
async fn a_trusted_relay_posts_a_task_to_his_agents_on_the_json_planner() {
    let d = agentd();
    let h = harness(
        rules("lucina_agents", json!({"action": "run", "message": "check the login build"})),
        |c| {
            c.planner = PlannerMode::Json;
            c.mcp_trusted = true;
        },
    )
    .await;
    let out = relay(&h, "chatgpt", "post it in Lucina agents: check the login build").await;
    assert_eq!(out["reply"], "Done: the tool answered.", "{out}");
    let calls = d.calls("check the login build");
    assert!(
        calls.iter().any(|l| l.starts_with("POST /agents ")),
        "agentd never took the run: {calls:?}"
    );
    // The answer she got names the agent agentd returned, and it is marked sent.
    let got = agents_answer(&h);
    assert_eq!((got["status"].as_str(), got["agent"].as_str()), (Some("sent"), Some("ag-run-1")), "{got}");
    // Attribution: the relay's words, not his own.
    let via = h.timeline_lines();
    assert!(
        via.iter()
            .any(|i| i["meta"]["via"] == "mcp" && i["meta"]["caller"] == "chatgpt"),
        "{via:?}"
    );
    assert!(
        via.iter()
            .any(|i| i["meta"]["tool"] == "lucina_agents" && i["meta"]["via"] == "mcp"),
        "the tool item is not marked as the relay's: {via:?}"
    );
}

#[tokio::test]
async fn b_trusted_relay_lists_his_agents_on_the_json_planner() {
    let h = harness_trusted(json!({"action": "list"}), PlannerMode::Json).await;
    let out = relay(&h, "chatgpt", "ask Lucina agents what is running right now").await;
    assert_eq!(out["reply"], "Done: the tool answered.", "{out}");
    // The roster is agentd's, not made up: what the tool answered her is what she was given.
    let got = agents_answer(&h);
    assert_eq!(got["agents"][0]["id"], "ag-listed-1", "{got}");
    assert_eq!(got["agents"][0]["kind"], "Claude session", "{got}");
}

/// A run agentd takes but does not name: nothing is running, and she must not say otherwise.
#[tokio::test]
async fn c_a_run_agentd_does_not_name_is_not_claimed_sent() {
    let d = agentd();
    let h = harness(
        rules(
            "lucina_agents",
            json!({"action": "run", "message": "no-id work item for the relay"}),
        ),
        |c| {
            c.planner = PlannerMode::Json;
            c.mcp_trusted = true;
        },
    )
    .await;
    relay(
        &h,
        "chatgpt",
        "post it in Lucina agents: no-id work item for the relay",
    )
    .await;
    assert!(d.calls("no-id").iter().any(|l| l.starts_with("POST /agents ")));
    let got = agents_answer(&h);
    assert_eq!(got["status"], "failed", "{got}");
    assert!(got.get("agent").is_none(), "an id she should not have: {got}");
}

#[tokio::test]
async fn d_an_agent_caller_never_reaches_the_agents_even_when_trusted() {
    let d = agentd();
    for caller in ["claude-code", "whoever"] {
        let h = harness(
            rules("lucina_agents", json!({"action": "run", "message": "dispatch probe"})),
            |c| {
                c.planner = PlannerMode::Json;
                c.mcp_trusted = true;
            },
        )
        .await;
        let out = relay(&h, caller, "post it in Lucina agents: dispatch probe").await;
        assert_eq!(out["reply"], "Nothing was run here.", "{caller}: {out}");
        assert!(
            d.calls("dispatch probe").is_empty(),
            "{caller}: agentd was called"
        );
        assert!(h.state.trusted_mcp_registry.get("lucina_agents").is_none());
        assert!(h.state.mcp_registry.get("lucina_agents").is_none());
    }
}

#[tokio::test]
async fn e_the_owner_app_without_trust_never_reaches_the_agents() {
    let d = agentd();
    let h = harness(
        rules("lucina_agents", json!({"action": "run", "message": "untrusted probe"})),
        |c| {
            c.planner = PlannerMode::Json;
            c.mcp_trusted = false;
        },
    )
    .await;
    let out = relay(&h, "chatgpt", "post it in Lucina agents: untrusted probe").await;
    assert_eq!(out["reply"], "Nothing was run here.", "{out}");
    assert!(
        d.calls("untrusted probe").is_empty(),
        "agentd was called without trust"
    );
}

/// Tools mode (`LUCINA_PLANNER=tools`): the relay is offered his Agents tools and they run; the same
/// turn from an agent is not offered them, and keeps the trusted registry without them.
#[tokio::test]
async fn f_tools_mode_offers_the_agents_only_to_the_owner_relay() {
    let d = agentd();
    let h = harness_trusted(
        json!({"action": "read", "agent": "ag-probe-f"}),
        PlannerMode::Tools,
    )
    .await;
    let r = &h.state.owner_relay_mcp_registry;
    for n in ["lucina_agents", "reach_threads"] {
        assert!(r.get(n).is_some(), "{n} missing for the relay");
    }
    for n in [
        "message_claude",
        "message_grok",
        "secrets_status",
        "flaw",
        "parcel",
        "instagram_profile",
    ] {
        assert!(r.get(n).is_none(), "{n} offered on an MCP turn");
    }
    relay(&h, "chatgpt", "read what the agent ag-probe-f did").await;
    let calls = d.calls("ag-probe-f");
    assert!(
        calls.iter().any(|l| l.starts_with("GET /agents/ag-probe-f")),
        "the tools loop never reached agentd: {calls:?}"
    );
    // The read is agentd's answer, keyed by the agent he named.
    let got = agents_answer(&h);
    assert_eq!(got["agent"], "ag-probe-f", "{got}");
    let h2 = harness_trusted(
        json!({"action": "read", "agent": "ag-probe-f2"}),
        PlannerMode::Tools,
    )
    .await;
    relay(&h2, "claude-code", "read what the agent ag-probe-f2 did").await;
    let offered = h2
        .fake
        .requests()
        .iter()
        .filter(|q| q["tools"].as_array().is_some_and(|t| !t.is_empty()))
        .map(|q| q["tools"].to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!offered.is_empty(), "the agent turn got no tool loop at all");
    assert!(
        !offered.contains("lucina_agents") && !offered.contains("reach_threads"),
        "an agent was offered his Agents tools"
    );
    assert!(
        d.calls("ag-probe-f2").is_empty(),
        "agentd was reached by an agent: {:?}",
        d.calls("ag-probe-f2")
    );
    assert!(
        !h2.state.trusted_mcp_registry.get("lucina_agents").is_some(),
        "trusted_mcp_registry grew the Agents pair"
    );
}
