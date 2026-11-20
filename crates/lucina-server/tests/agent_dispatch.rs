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
use lucina_server::{Config, PlannerMode};
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
/// The script of a turn whose model calls one tool. Building it installs the stand-in daemon: an
/// unset `LUCINA_AGENTD_SOCKET` falls back to the live daemon's path, and a test of this file must
/// never read from it, not even a `list`.
fn rules(tool: &str, args: Value) -> Script {
    let _d = agentd();
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

/// The tools this turn's model was offered, by name.
fn offered_tools(h: &H) -> Vec<String> {
    h.fake
        .requests()
        .iter()
        .flat_map(|q| q["tools"].as_array().into_iter().flatten())
        .filter_map(|t| {
            t["function"]["name"]
                .as_str()
                .or_else(|| t["name"].as_str())
                .map(str::to_owned)
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

/// Tools mode (`LUCINA_PLANNER=tools`) on a trusted relay, with delegation switched on: the `ask_*`
/// and `delegate` tools exist in the registry, so a turn that is not offered them was held back and
/// not simply given nothing. No delegation is ever run here: the fake model only calls the tool of
/// its own rule.
fn tools_trusted(c: &mut Config) {
    let p = c.data_dir.join("delegate.toml");
    std::fs::create_dir_all(c.data_dir.clone()).ok();
    std::fs::write(&p, "enabled = true\n").unwrap();
    c.delegate_enabled = true;
    c.delegate_config = Some(p);
    c.planner = PlannerMode::Tools;
    c.mcp_trusted = true;
}

#[tokio::test]
async fn a_trusted_relay_posts_a_task_to_his_agents_on_the_json_planner() {
    let d = agentd();
    let h = harness(
        rules(
            "lucina_agents",
            json!({"action": "run", "message": "check the login build"}),
        ),
        |c| {
            c.planner = PlannerMode::Json;
            c.mcp_trusted = true;
        },
    )
    .await;
    let out = relay(
        &h,
        "chatgpt",
        "post it in Lucina agents: check the login build",
    )
    .await;
    assert_eq!(out["reply"], "Done: the tool answered.", "{out}");
    let calls = d.calls("check the login build");
    assert!(
        calls.iter().any(|l| l.starts_with("POST /agents ")),
        "agentd never took the run: {calls:?}"
    );
    // The answer she got names the agent agentd returned, and it is marked sent.
    let got = agents_answer(&h);
    assert_eq!(
        (got["status"].as_str(), got["agent"].as_str()),
        (Some("sent"), Some("ag-run-1")),
        "{got}"
    );
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
    // His own words: a question about the Agents is a read of them, not a task to post.
    let out = relay(&h, "chatgpt", "what is running in Lucina agents right now?").await;
    assert_eq!(out["reply"], "Done: the tool answered.", "{out}");
    // The roster is agentd's, not made up: what the tool answered her is what she was given.
    let got = agents_answer(&h);
    assert_eq!(got["agents"][0]["id"], "ag-listed-1", "{got}");
    assert_eq!(got["agents"][0]["kind"], "Claude session", "{got}");
}

/// The same question when the planner reads the turn as plain chat: the explicit Agents question is
/// the relay route's own signal, and it must not reach the dispatch side of the tool.
#[tokio::test]
async fn g_a_question_about_the_agents_lists_without_the_route() {
    let d = agentd();
    let h = harness(
        script(json!({"rules": [
            {"kind": "agent", "afterTool": false,
             "toolCalls": [{"name": "lucina_agents", "arguments": {"action": "list"}}]},
            {"kind": "agent", "afterTool": true, "content": "Listed from the daemon."},
            {"kind": "intent", "content": "{\"intent\":\"chat\",\"confidence\":0.9,\"reply_mode\":\"chat\"}"},
            {"kind": "plan", "content": "{\"action\":\"reply\",\"text\":\"Nothing was run here.\"}"},
            {"kind": "reply", "content": "Nothing was run here."}
        ]})),
        |c| {
            c.planner = PlannerMode::Json;
            c.mcp_trusted = true;
        },
    )
    .await;
    let out = relay(
        &h,
        "chatgpt",
        "what is still running in Lucina agents right now?",
    )
    .await;
    assert_eq!(out["reply"], "Listed from the daemon.", "{out}");
    let got = agents_answer(&h);
    assert_eq!(got["agents"][0]["id"], "ag-listed-1", "{got}");
    // A question answered with the roster, not with a dispatch outcome.
    assert!(
        got.get("status").is_none(),
        "the list answered as a run: {got}"
    );
    // And agentd never took a task from his words.
    let calls = d.calls("still running in Lucina agents right now");
    assert!(
        !calls.iter().any(|l| l.starts_with("POST ")),
        "the question dispatched work: {calls:?}"
    );
}

/// A run agentd takes but does not name: the launch is unconfirmed, and she is told so in the answer
/// the loop works from.
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
    assert!(d
        .calls("no-id")
        .iter()
        .any(|l| l.starts_with("POST /agents ")));
    let got = agents_answer(&h);
    // Not sent, and no agent id she does not have.
    assert_eq!(got["status"], "unconfirmed", "{got}");
    assert_eq!(got["confirmed"], false, "{got}");
    assert!(
        got.get("agent").is_none(),
        "an id she should not have: {got}"
    );
    // What she is told to say: the launch is unconfirmed, no claim that nothing is running, and no
    // second dispatch on the doubt.
    let said = got["instruction"]
        .as_str()
        .unwrap_or_default()
        .to_lowercase();
    for claim in ["nothing is running", "did not take", "did not start"] {
        assert!(
            !said.contains(claim),
            "the answer claims an absence: {claim} in {said}"
        );
    }
    assert!(said.contains("unconfirmed"), "{said}");
}

#[tokio::test]
async fn d_an_agent_caller_never_reaches_the_agents_even_when_trusted() {
    let d = agentd();
    for caller in ["claude-code", "whoever"] {
        let h = harness(
            rules(
                "lucina_agents",
                json!({"action": "run", "message": "dispatch probe"}),
            ),
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
        rules(
            "lucina_agents",
            json!({"action": "run", "message": "untrusted probe"}),
        ),
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
        calls
            .iter()
            .any(|l| l.starts_with("GET /agents/ag-probe-f")),
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
    let names = offered_tools(&h2);
    assert!(!names.is_empty(), "the agent turn got no tool loop at all");
    for n in [
        "lucina_agents",
        "reach_threads",
        "message_claude",
        "message_grok",
        "secrets_status",
    ] {
        assert!(!names.iter().any(|o| o == n), "an agent was offered {n}");
    }
    assert!(
        d.calls("ag-probe-f2").is_empty(),
        "agentd was reached by an agent: {:?}",
        d.calls("ag-probe-f2")
    );
    assert!(
        h2.state.trusted_mcp_registry.get("lucina_agents").is_none(),
        "trusted_mcp_registry grew the Agents pair"
    );
}

/// Tools mode with the relay trusted (`LUCINA_MCP_TRUSTED=true`): an agent's turn is a consultation,
/// so it is held to the MCP tool set — the tools that drive another agent are not offered to it,
/// however it asks. The owner's own app, on the same token and the same trust, is offered them: that
/// is what makes the exclusion below a choice and not an empty world.
#[tokio::test]
async fn h_a_consultation_is_never_offered_the_delegation_tools() {
    // `delegate` and its `ask_*` siblings are in this harness's registry (delegation is switched on
    // in `tools_trusted`), and `mcp_registry` is the only set that keeps them away.
    const DRIVERS: &[&str] = &[
        "ask_grok",
        "ask_claude",
        "ask_chatgpt",
        "delegate",
        "delegation_jobs",
        "delegation_cancel",
    ];
    for caller in ["claude-code", "whoever"] {
        for text in [
            "ask Grok what is new on X and tell me",
            "delegate this to Claude code: fix the login build",
            "post it in Lucina agents: check the login build",
        ] {
            let h = harness(
                rules("ask_grok", json!({"question": "h probe"})),
                tools_trusted,
            )
            .await;
            relay(&h, caller, text).await;
            let names = offered_tools(&h);
            assert!(
                !names.is_empty(),
                "{caller} on {text:?}: no tool list at all"
            );
            for n in DRIVERS {
                assert!(
                    !names.iter().any(|o| o == n),
                    "{caller} on {text:?} was offered {n}: the consultation ran as the owner's turn"
                );
            }
            for n in [
                "lucina_agents",
                "reach_threads",
                "message_grok",
                "secrets_status",
            ] {
                assert!(
                    !names.iter().any(|o| o == n),
                    "{caller} on {text:?} was offered {n}"
                );
            }
            // Held to the MCP set, not stripped of the turn: it keeps its reading tools.
            assert!(
                names.iter().any(|o| o == "reminders" || o == "notes"),
                "{caller} on {text:?}: a consultation without even its reading tools"
            );
        }
    }
    // The owner's own app: the drivers are there, his Agents are there, and the list reaches the
    // daemon he is given rather than the live one.
    let h = harness(
        rules("lucina_agents", json!({"action": "list"})),
        tools_trusted,
    )
    .await;
    let out = relay(&h, "chatgpt", "what is running in Lucina agents right now?").await;
    assert_eq!(out["reply"], "Done: the tool answered.", "{out}");
    let names = offered_tools(&h);
    for n in ["ask_grok", "delegate", "lucina_agents"] {
        assert!(
            names.iter().any(|o| o == n),
            "the relay was not offered {n}: {names:?}"
        );
    }
    assert_eq!(agents_answer(&h)["agents"][0]["id"], "ag-listed-1");
}
