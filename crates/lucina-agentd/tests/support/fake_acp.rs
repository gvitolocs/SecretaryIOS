//! A fake ACP agent for lucina-agentd turn-watch tests (`tests/acp_watch.rs`, bin `lucina-fake-acp`); not
//! installed anywhere. `FAKE_ACP_MODE` picks how it answers `session/prompt`:
//! - `limit`: what `opencode acp` did on Zen's free tier (agent cc504145, 2026-11-08):
//!   writes the real "Free usage limit reached" line to `FAKE_ACP_LOG` (for session
//!   `FAKE_ACP_LOG_SESSION`, default its own) and never answers;
//! - `silent`: never answers, logs nothing;
//! - `tool`: a tool call runs for `FAKE_ACP_TOOL_MS` with no other output, then the turn ends;
//! - `permission`: asks for a permission and ends the turn once it is answered;
//! - anything else: replies "done" and ends the turn.
//!
//! A `session/cancel` answers a pending prompt with `stopReason: cancelled`.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

const SESSION: &str = "ses_fake0123456789abcdefABCD";
const PERMISSION_ID: u64 = 9001;

type Out = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Out, v: Value) {
    let mut o = out.lock().unwrap_or_else(|p| p.into_inner());
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

fn reply(out: &Out, id: &Value, result: Value) {
    send(out, json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

fn update(out: &Out, update: Value) {
    send(out, json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": SESSION, "update": update}}));
}

fn say(out: &Out, text: &str) {
    update(out, json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}));
}

fn main() {
    let mode = std::env::var("FAKE_ACP_MODE").unwrap_or_else(|_| "ok".into());
    let tool_ms: u64 = std::env::var("FAKE_ACP_TOOL_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1500);
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let mut pending: Option<Value> = None;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let id = msg["id"].clone();
        match msg["method"].as_str() {
            Some("initialize") => reply(&out, &id, json!({"protocolVersion": 1, "agentCapabilities": {"loadSession": false}})),
            Some("session/new") => reply(&out, &id, json!({"sessionId": SESSION})),
            Some("session/cancel") => {
                if let Some(p) = pending.take() {
                    reply(&out, &p, json!({"stopReason": "cancelled"}));
                }
            }
            Some("session/prompt") => match mode.as_str() {
                "limit" => {
                    if let Ok(path) = std::env::var("FAKE_ACP_LOG") {
                        let sid = std::env::var("FAKE_ACP_LOG_SESSION").unwrap_or_else(|_| SESSION.into());
                        // Word for word what opencode wrote for cc504145, with this session's id.
                        let line = format!(
                            "timestamp=2026-11-08T10:23:36.086Z level=ERROR run=0d6eb771 message=\"stream error\" providerID=opencode modelID=deepseek-v4.1-flash-free session.id={sid} small=false agent=build mode=primary error.error=\"AI_APICallError: Free usage limit reached. Free models work in OpenCode (https://opencode.ai); subscribe to OpenCode Go to keep going: https://opencode.ai/go\"\n"
                        );
                        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).create(true).open(path) {
                            let _ = f.write_all(line.as_bytes());
                        }
                    }
                    pending = Some(id);
                }
                "silent" => pending = Some(id),
                "tool" => {
                    let out = out.clone();
                    std::thread::spawn(move || {
                        update(&out, json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "bash", "kind": "execute", "status": "in_progress", "rawInput": {"command": "cargo build"}}));
                        std::thread::sleep(Duration::from_millis(tool_ms));
                        update(&out, json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed"}));
                        say(&out, "done");
                        reply(&out, &id, json!({"stopReason": "end_turn"}));
                    });
                }
                "permission" => {
                    send(
                        &out,
                        json!({"jsonrpc": "2.0", "id": PERMISSION_ID, "method": "session/request_permission", "params": {
                            "sessionId": SESSION,
                            "toolCall": {"toolCallId": "t2", "title": "Run cargo build", "kind": "execute"},
                            "options": [{"optionId": "allow", "name": "Allow", "kind": "allow_once"}, {"optionId": "deny", "name": "Deny", "kind": "reject_once"}],
                        }}),
                    );
                    pending = Some(id);
                }
                _ => {
                    say(&out, "done");
                    reply(&out, &id, json!({"stopReason": "end_turn"}));
                }
            },
            // The answer to our permission request.
            None if id == json!(PERMISSION_ID) => {
                if let Some(p) = pending.take() {
                    say(&out, "done after the permission");
                    reply(&out, &p, json!({"stopReason": "end_turn"}));
                }
            }
            Some(_) if !id.is_null() => reply(&out, &id, json!({})),
            _ => {}
        }
    }
}
