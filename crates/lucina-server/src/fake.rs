//! A fake DeepSeek (OpenAI-compatible `/v1/chat/completions`) for parity
//! runs and tests (port plan §4 "Fake DeepSeek"): scripted completions,
//! every request recorded. Both runtimes can talk to it: point the Node
//! Lucina's and this server's `DEEPSEEK_BASE_URL` at `http://<bind>/v1`.
//!
//! Script (JSON):
//! ```json
//! {"rules": [
//!   {"kind": "intent", "contains": "tempo", "content": "{\"intent\":\"chat\",\"confidence\":0.9,\"reply_mode\":\"chat\"}"},
//!   {"kind": "agent", "contains": "tempo", "toolCalls": [{"name": "weather", "arguments": {"location": "Copenhagen"}}]},
//!   {"kind": "agent", "afterTool": true, "content": "Domani a Copenaghen 10 gradi e pioggia."}
//! ], "default": {"content": "{\"action\":\"reply\",\"text\":\"ok\"}"}}
//! ```
//! `kind`: `intent` (the tier-1 classifier), `plan` (the JSON planner),
//! `agent` (requests that carry tools), `reply` (everything else) or `any`.
//! `contains` is matched (case-insensitively) against the last user message;
//! `afterTool` matches only when the last message is a tool result. Rules are
//! tried in order; `once: true` rules are used at most once.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default)]
    pub after_tool: Option<bool>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<FakeCall>>,
    #[serde(default)]
    pub once: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FakeCall {
    pub name: String,
    #[serde(default)]
    pub arguments: Value,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Script {
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub default: Option<Rule>,
}

#[derive(Default)]
pub struct FakeState {
    rules: Mutex<Vec<(Rule, bool)>>,
    default: Option<Rule>,
    pub requests: Mutex<Vec<Value>>,
    record: Option<PathBuf>,
}

/// Which prompt a request is (Lucina's prompt families).
pub fn kind_of(req: &Value) -> &'static str {
    if req["tools"].as_array().is_some_and(|t| !t.is_empty()) {
        return "agent";
    }
    let system = req["messages"]
        .as_array()
        .and_then(|m| m.iter().find(|x| x["role"] == "system"))
        .and_then(|m| m["content"].as_str())
        .unwrap_or("");
    if system.starts_with("You classify ") {
        "intent"
    } else if system.starts_with("You turn a draft reply by Lucina") {
        "rich"
    } else if system
        .contains("a concise helpful assistant connected to chat platforms through Hermes")
        && system.contains("Return only compact JSON with this schema:")
    {
        "plan"
    } else {
        "reply"
    }
}

fn last_user(req: &Value) -> String {
    req["messages"]
        .as_array()
        .and_then(|m| m.iter().rev().find(|x| x["role"] == "user"))
        .and_then(|m| m["content"].as_str())
        .map(|s| s.rsplit("Message:\n").next().unwrap_or(s).to_owned())
        .unwrap_or_default()
}

fn after_tool(req: &Value) -> bool {
    req["messages"]
        .as_array()
        .and_then(|m| m.last())
        .is_some_and(|m| m["role"] == "tool")
}

fn matches(rule: &Rule, req: &Value) -> bool {
    let kind = kind_of(req);
    rule.kind.as_deref().is_none_or(|k| k == "any" || k == kind)
        && rule
            .contains
            .as_deref()
            .is_none_or(|c| last_user(req).to_lowercase().contains(&c.to_lowercase()))
        && rule.after_tool.is_none_or(|a| a == after_tool(req))
}

fn completion(rule: Option<&Rule>, n: usize) -> Value {
    let content = rule.and_then(|r| r.content.clone());
    let calls: Option<Vec<Value>> = rule.and_then(|r| r.tool_calls.as_ref()).map(|calls| {
        calls
            .iter()
            .enumerate()
            .map(|(i, c)| json!({"id": format!("call_{n}_{i}"), "type": "function", "function": {"name": c.name, "arguments": c.arguments.to_string()}}))
            .collect()
    });
    let finish = if calls.is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    let mut message = json!({"role": "assistant", "content": content.unwrap_or_else(|| if calls.is_some() { String::new() } else { "{\"action\":\"reply\",\"text\":\"ok\"}".into() })});
    if let Some(c) = calls {
        message["tool_calls"] = Value::Array(c);
        message["content"] = Value::Null;
    }
    json!({
        "id": format!("fake-{n}"), "object": "chat.completion", "model": "fake-deepseek",
        "choices": [{"index": 0, "finish_reason": finish, "message": message}],
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
    })
}

async fn chat(State(st): State<Arc<FakeState>>, Json(req): Json<Value>) -> Json<Value> {
    let n = {
        let mut r = st.requests.lock().unwrap();
        r.push(req.clone());
        r.len()
    };
    if let Some(path) = &st.record {
        use std::io::Write;
        let line = json!({"kind": kind_of(&req), "lastUser": last_user(&req), "afterTool": after_tool(&req), "request": req});
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
    let rule = {
        let mut rules = st.rules.lock().unwrap();
        let hit = rules
            .iter_mut()
            .find(|(r, used)| !(*used && r.once) && matches(r, &req));
        hit.map(|(r, used)| {
            *used = true;
            r.clone()
        })
    };
    Json(completion(rule.as_ref().or(st.default.as_ref()), n))
}

/// A running fake: its base URL (`.../v1`) and recorded requests.
pub struct Fake {
    pub base_url: String,
    pub state: Arc<FakeState>,
}

impl Fake {
    pub fn requests(&self) -> Vec<Value> {
        self.state.requests.lock().unwrap().clone()
    }
}

pub fn router(script: Script, record: Option<PathBuf>) -> (Router, Arc<FakeState>) {
    let state = Arc::new(FakeState {
        rules: Mutex::new(script.rules.into_iter().map(|r| (r, false)).collect()),
        default: script.default,
        requests: Mutex::new(Vec::new()),
        record,
    });
    (
        Router::new()
            .route("/v1/chat/completions", post(chat))
            .route("/chat/completions", post(chat))
            .with_state(state.clone()),
        state,
    )
}

/// Serve on `addr` (port 0 for an ephemeral port).
pub async fn spawn(
    addr: SocketAddr,
    script: Script,
    record: Option<PathBuf>,
) -> std::io::Result<Fake> {
    let (app, state) = router(script, record);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(Fake {
        base_url: format!("http://{local}/v1"),
        state,
    })
}
