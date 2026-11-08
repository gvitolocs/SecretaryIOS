//! `lagent mcp`: the agents' own MCP server (Paseo's agent-scoped `paseo` MCP tools),
//! over stdio. agentd attaches it to every agent it starts whose provider takes MCP
//! servers ([`attach`]), so an agent can start subagents, message them, give itself
//! heartbeats and search past runs.
//!
//! It is a thin client of the daemon socket: the daemon knows which agent is calling
//! from the process tree ([`crate::peer::calling_agent`] through the provider's
//! launch environment), so nothing here is trusted. The depth limit, supervision,
//! "only your own subagents" and schedule ownership are all enforced by the daemon,
//! which is why agentd preapproves these tools ([`is_own_tool`]).

use std::path::{Path, PathBuf};

use lucina_agents::curator::{curate_activity, CurateOptions};
use lucina_agents::projection::project_rows;
use lucina_agents::{TimelineItem, TimelineRow};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::client::{Client, ClientError};

/// The server name agents see (`mcp__lucina-agents__<tool>`).
pub const SERVER: &str = "lucina-agents";
/// `LUCINA_AGENTD_MCP=0` stops attaching it.
pub const ENV: &str = "LUCINA_AGENTD_MCP";
const PROTOCOL: &str = "2025-06-18";
const MAX_WAIT_SECS: u64 = 50;
const MAX_TEXT: usize = 12_000;

/// Whether a permission request is for one of these tools (preapproved).
pub fn is_own_tool(name: &str) -> bool {
    name.strip_prefix("mcp__").and_then(|r| r.strip_prefix(SERVER)).is_some_and(|t| t.starts_with("__") && t.len() > 2)
}

/// `lagent` next to the running daemon binary, when attaching is on.
pub fn lagent_path() -> Option<PathBuf> {
    if std::env::var(ENV).is_ok_and(|v| v == "0" || v == "false") {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let p = exe.parent()?.join("lagent");
    p.is_file().then_some(p)
}

/// The daemon socket the server talks to.
fn socket() -> String {
    crate::config::default_socket().display().to_string()
}

/// The server as a Claude `mcpServers` entry.
pub fn attach() -> Option<Value> {
    let p = lagent_path()?;
    Some(attach_with(&p))
}

/// The server entry for a given `lagent` path.
pub fn attach_with(lagent: &Path) -> Value {
    json!({"type": "stdio", "command": lagent.display().to_string(), "args": ["mcp"], "env": {crate::config::SOCKET_ENV: socket()}})
}

/// ACP `session/new` `mcpServers` (stdio servers, which every ACP agent supports).
pub fn acp_servers(for_agent: bool) -> Value {
    match lagent_path().filter(|_| for_agent) {
        Some(p) => json!([{"name": SERVER, "command": p.display().to_string(), "args": ["mcp"], "env": [{"name": crate::config::SOCKET_ENV, "value": socket()}]}]),
        None => json!([]),
    }
}

/// The `config` override for Codex's `thread/start` and `thread/resume` (Paseo's
/// `buildCodexInnerConfig` + `applyCodexToolPolicy`): the server under
/// `mcp_servers.lucina-agents`, its tools listed and preapproved one by one
/// (`default_tools_approval_mode: "prompt"` for anything else). Codex hands an MCP
/// server only a small environment, so the agent id goes in explicitly.
pub fn codex_config(agent_id: Option<&str>) -> Option<Value> {
    let id = agent_id?;
    Some(codex_config_for(&lagent_path()?, &socket(), id))
}

#[cfg(test)]
pub fn codex_config_for_tests(lagent: &str, socket: &str, agent_id: &str) -> Value {
    codex_config_for(Path::new(lagent), socket, agent_id)
}

fn codex_config_for(lagent: &Path, socket: &str, agent_id: &str) -> Value {
    let names: Vec<String> = tools().iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect();
    let approvals: serde_json::Map<String, Value> = names.iter().map(|n| (n.clone(), json!({"approval_mode": "approve"}))).collect();
    json!({"mcp_servers": {SERVER: {
        "command": lagent.display().to_string(),
        "args": ["mcp"],
        "env": {crate::config::SOCKET_ENV: socket, "LUCINA_AGENT_ID": agent_id},
        "enabled_tools": names,
        "default_tools_approval_mode": "prompt",
        "tools": approvals,
    }}})
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({"name": name, "description": description, "inputSchema": {"type": "object", "properties": properties, "required": required, "additionalProperties": false}})
}

fn s(desc: &str) -> Value {
    json!({"type": "string", "description": desc})
}

fn tools() -> Vec<Value> {
    let id = s("Agent id (or a unique prefix)");
    vec![
        tool(
            "create_agent",
            "Start a subagent in your workspace with a task. You get one message with its answer when it finishes (notifyOnFinish, default true). Subagents can't start further agents.",
            json!({"provider": s("provider or provider/model, e.g. claude or claude/claude-sonnet-5-5"), "initialPrompt": s("The task"),
                   "title": s("Short title (60 chars)"), "mode": s("Permission mode id (one that keeps permission prompts)"),
                   "cwd": s("Directory (default: your workspace)"), "notifyOnFinish": {"type": "boolean"}}),
            &["provider", "initialPrompt"],
        ),
        tool(
            "send_agent_prompt",
            "Send a message to your subagent (or your parent). It is delivered after 2 quiet seconds; with notifyOnFinish you hear when the subagent's run ends.",
            json!({"agentId": id, "prompt": s("The message"), "notifyOnFinish": {"type": "boolean"}}),
            &["agentId", "prompt"],
        ),
        tool("list_agents", "List agents (yours are marked as subagents).", json!({}), &[]),
        tool("get_agent_status", "An agent's status, title and pending permission requests.", json!({"agentId": id}), &["agentId"]),
        tool(
            "get_agent_activity",
            "An agent's recent activity as text. Its provider subagents (a Claude Task) show as one entry with what they did; pass subagentId for one's own activity.",
            json!({"agentId": id, "limit": {"type": "integer", "description": "Last N entries (default 30)"}, "subagentId": {"type": "string", "description": "A provider subagent of this agent (its Task call id)"}}),
            &["agentId"],
        ),
        tool(
            "wait_for_agent",
            "Wait until an agent is idle, errors or needs permission (at most 50 s per call).",
            json!({"agentId": id, "timeoutSeconds": {"type": "integer"}}),
            &["agentId"],
        ),
        tool("cancel_agent", "Interrupt your subagent's run.", json!({"agentId": id}), &["agentId"]),
        tool("archive_agent", "Archive your subagent.", json!({"agentId": id}), &["agentId"]),
        tool("detach_agent", "Make your subagent independent (it keeps running; you stop hearing about it).", json!({"agentId": id}), &["agentId"]),
        tool(
            "create_heartbeat",
            "A recurring prompt to yourself (cron, 5 fields). Re-creating one with the same name replaces it.",
            json!({"prompt": s("What to check each time"), "cron": s("e.g. */30 * * * *"), "timezone": s("IANA zone (default: the owner's)"),
                   "name": s("Name"), "maxRuns": {"type": "integer"}, "expiresIn": s("e.g. 2h, 3d")}),
            &["prompt", "cron"],
        ),
        tool(
            "create_schedule",
            "A recurring schedule that starts a new agent each time (your provider unless given).",
            json!({"prompt": s("The task"), "cron": s("e.g. 0 7 * * 1-5"), "timezone": s("IANA zone (default: the owner's)"), "name": s("Name"),
                   "provider": s("provider or provider/model"), "cwd": s("Directory (default: yours)"), "isolation": {"type": "string", "enum": ["local", "worktree"]},
                   "maxRuns": {"type": "integer"}, "expiresIn": s("e.g. 2h, 3d")}),
            &["prompt", "cron"],
        ),
        tool("list_schedules", "All schedules and heartbeats (yours are marked).", json!({}), &[]),
        tool("delete_schedule", "Delete a schedule or heartbeat you created.", json!({"id": s("Schedule id")}), &["id"]),
        tool("delete_heartbeat", "Delete a heartbeat you created.", json!({"id": s("Schedule id")}), &["id"]),
        tool(
            "search_agents",
            "Search agents by title, workspace, branch or what was said.",
            json!({"query": s("Words to find"), "limit": {"type": "integer"}}),
            &["query"],
        ),
    ]
}

fn duration_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = n.parse().ok()?;
    let mult = match unit.trim() {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return None,
    };
    Some(n * mult)
}

fn seg(v: &Value, k: &str) -> Result<String, String> {
    let s = v[k].as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or(format!("`{k}` is required"))?;
    if s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        Ok(s.to_string())
    } else {
        Err(format!("`{k}` is not an id"))
    }
}

fn clip(s: String) -> String {
    if s.chars().count() <= MAX_TEXT {
        return s;
    }
    s.chars().take(MAX_TEXT).collect::<String>() + "\n[truncated]"
}

fn err_text(e: ClientError) -> String {
    match e {
        ClientError::Api { code, message, .. } => format!("{code}: {message}"),
        other => other.to_string(),
    }
}

fn agent_line(a: &Value, me: Option<&str>) -> String {
    let child = me.is_some_and(|m| a["labels"]["lucina.parent-agent-id"].as_str() == Some(m));
    format!(
        "- {} · {} · {} · {}{}",
        a["id"].as_str().unwrap_or(""),
        a["status"].as_str().unwrap_or(""),
        a["provider"].as_str().unwrap_or(""),
        a["title"].as_str().unwrap_or(""),
        if child { " (your subagent)" } else { "" }
    )
}

async fn call(client: &Client, me: Option<&str>, name: &str, a: &Value) -> Result<String, String> {
    let e = err_text;
    match name {
        "create_agent" => {
            let mut body = json!({"provider": a["provider"], "prompt": a["initialPrompt"], "cwd": a["cwd"].as_str().unwrap_or("")});
            for (from, to) in [("title", "title"), ("mode", "modeId"), ("notifyOnFinish", "notifyOnFinish")] {
                if !a[from].is_null() {
                    body[to] = a[from].clone();
                }
            }
            let v = client.post("/agents", &body).await.map_err(e)?;
            Ok(format!(
                "Started subagent {} ({}) on {}, status {}. You'll get a message when it finishes.",
                v["id"].as_str().unwrap_or(""),
                v["title"].as_str().unwrap_or(""),
                v["provider"].as_str().unwrap_or(""),
                v["status"].as_str().unwrap_or("")
            ))
        }
        "send_agent_prompt" => {
            let id = seg(a, "agentId")?;
            let mut body = json!({"prompt": a["prompt"]});
            if !a["notifyOnFinish"].is_null() {
                body["notifyOnFinish"] = a["notifyOnFinish"].clone();
            }
            let v = client.post(&format!("/agents/{id}/send"), &body).await.map_err(e)?;
            Ok(format!("Queued message {} for {id}; it is delivered after 2 quiet seconds.", v["messageId"].as_str().unwrap_or("")))
        }
        "list_agents" => {
            let v = client.get("/agents/items").await.map_err(e)?;
            let items = v["agents"].as_array().cloned().unwrap_or_default();
            Ok(clip(format!("{} agent(s):\n{}", items.len(), items.iter().map(|i| agent_line(i, me)).collect::<Vec<_>>().join("\n"))))
        }
        "get_agent_status" => {
            let id = seg(a, "agentId")?;
            let v = client.get(&format!("/agents/{id}")).await.map_err(e)?;
            let pending = v["pendingPermissions"].as_array().map(|p| p.len()).unwrap_or(0);
            Ok(format!("{}\npending permission requests: {pending} (the owner answers them)", agent_line(&v, me)))
        }
        "get_agent_activity" => {
            let id = seg(a, "agentId")?;
            let path = match a["subagentId"].as_str().filter(|s| !s.is_empty()) {
                Some(_) => format!("/agents/{id}/provider-subagents/{}/timeline", seg(a, "subagentId")?),
                None => format!("/agents/{id}/timeline"),
            };
            let v = client.get(&path).await.map_err(e)?;
            let rows: Vec<TimelineRow> = serde_json::from_value(v["rows"].clone()).unwrap_or_default();
            let items: Vec<TimelineItem> = project_rows(&rows).into_iter().map(|e| e.item).collect();
            let limit = a["limit"].as_u64().unwrap_or(30) as usize;
            Ok(clip(curate_activity(&items, &CurateOptions { max_items: limit, ..CurateOptions::default() })))
        }
        "wait_for_agent" => {
            let id = seg(a, "agentId")?;
            let secs = a["timeoutSeconds"].as_u64().unwrap_or(MAX_WAIT_SECS).clamp(1, MAX_WAIT_SECS);
            let v = client.post(&format!("/agents/{id}/wait?timeoutMs={}", secs * 1000), &json!({})).await.map_err(e)?;
            let mut out = format!("status: {}", v["status"].as_str().unwrap_or(""));
            if let Some(m) = v["lastMessage"].as_str() {
                out.push_str(&format!("\nlast message:\n{m}"));
            }
            Ok(clip(out))
        }
        "cancel_agent" | "archive_agent" | "detach_agent" => {
            let id = seg(a, "agentId")?;
            let action = match name {
                "cancel_agent" => "stop",
                "archive_agent" => "archive",
                _ => "detach",
            };
            client.post(&format!("/agents/{id}/{action}"), &json!({})).await.map_err(e)?;
            Ok(format!("{action}: done for {id}."))
        }
        "create_heartbeat" | "create_schedule" => {
            let mut body = json!({"prompt": a["prompt"], "cron": a["cron"]});
            for k in ["timezone", "name", "maxRuns", "cwd", "isolation"] {
                if !a[k].is_null() {
                    body[k] = a[k].clone();
                }
            }
            if let Some(x) = a["expiresIn"].as_str() {
                body["expiresInMs"] = json!(duration_ms(x).ok_or("`expiresIn` is like 2h or 3d")?);
            }
            if name == "create_heartbeat" {
                body["agentId"] = json!("self");
                body["runOnCreate"] = json!(false);
            } else {
                let mine = match me {
                    Some(m) => client.get(&format!("/agents/{m}")).await.ok(),
                    None => None,
                };
                let provider = a["provider"].as_str().map(str::to_string).or_else(|| {
                    mine.as_ref().map(|m| match m["model"].as_str() {
                        Some(model) => format!("{}/{model}", m["provider"].as_str().unwrap_or("")),
                        None => m["provider"].as_str().unwrap_or("").to_string(),
                    })
                });
                body["provider"] = json!(provider.ok_or("`provider` is required")?);
                if body["cwd"].is_null() {
                    body["cwd"] = mine.as_ref().map(|m| m["cwd"].clone()).unwrap_or(Value::Null);
                }
            }
            let v = client.post("/schedules", &body).await.map_err(e)?;
            Ok(format!("Schedule {} is set; next run {}.", v["id"].as_str().unwrap_or(""), v["nextRunAt"].as_str().unwrap_or("-")))
        }
        "list_schedules" => {
            let v = client.get("/schedules").await.map_err(e)?;
            let list = v["schedules"].as_array().cloned().unwrap_or_default();
            let mine = me.map(|m| format!("agent:{m}"));
            let lines: Vec<String> = list
                .iter()
                .map(|s| {
                    let cadence = s["cadence"]["expression"].as_str().map(str::to_string).unwrap_or_else(|| format!("every {} ms", s["cadence"]["everyMs"]));
                    format!(
                        "- {} · {} · {} · next {} · {}{}",
                        s["id"].as_str().unwrap_or(""),
                        if s["target"]["type"] == "agent" { "heartbeat" } else { "new agent" },
                        cadence,
                        s["nextRunAt"].as_str().unwrap_or("-"),
                        s["name"].as_str().or_else(|| s["prompt"].as_str()).unwrap_or("").chars().take(100).collect::<String>(),
                        if mine.as_deref() == s["createdBy"].as_str() { " (yours)" } else { "" }
                    )
                })
                .collect();
            Ok(clip(format!("{} schedule(s):\n{}", list.len(), lines.join("\n"))))
        }
        "delete_schedule" | "delete_heartbeat" => {
            let id = seg(a, "id")?;
            client.request("DELETE", &format!("/schedules/{id}"), None).await.map_err(e)?;
            Ok(format!("Deleted schedule {id}."))
        }
        "search_agents" => {
            let q = a["query"].as_str().unwrap_or("");
            let limit = a["limit"].as_u64().unwrap_or(10).clamp(1, 50);
            let v = client.get(&format!("/agents/search?q={}&limit={limit}", urlencode(q))).await.map_err(e)?;
            let results = v["results"].as_array().cloned().unwrap_or_default();
            let mut out = format!("{} match(es):\n", results.len());
            for r in &results {
                out.push_str(&format!("- {} · {} · {}\n", r["agentId"].as_str().unwrap_or(""), r["status"].as_str().unwrap_or(""), r["title"].as_str().unwrap_or("")));
                for m in r["matches"].as_array().into_iter().flatten().take(3) {
                    out.push_str(&format!("    {}: {}\n", m["field"].as_str().unwrap_or(""), m["snippet"].as_str().unwrap_or("")));
                }
            }
            Ok(clip(out))
        }
        other => Err(format!("unknown tool {other}")),
    }
}

/// Percent-encode a query value.
pub fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// One JSON-RPC message in, at most one out.
pub async fn handle(client: &Client, me: Option<&str>, msg: Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or(PROTOCOL),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER, "version": env!("CARGO_PKG_VERSION")},
            "instructions": "Lucina Agents: start and steer subagents, give yourself heartbeats, and search past agent runs. Only the owner answers permission requests.",
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools()})),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let args = msg["params"]["arguments"].clone();
            let (text, is_error) = match call(client, me, name, &args).await {
                Ok(t) => (t, false),
                Err(t) => (t, true),
            };
            Ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
        }
        _ => Err(json!({"code": -32601, "message": format!("Method not found: {method}")})),
    };
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
    })
}

/// Serve stdio until EOF.
pub async fn serve(client: Client) -> std::io::Result<()> {
    let me = std::env::var("LUCINA_AGENT_ID").ok().filter(|s| !s.is_empty());
    let client = std::sync::Arc::new(client);
    serve_stdio(move |msg| {
        let client = client.clone();
        let me = me.clone();
        async move { handle(&client, me.as_deref(), msg).await }
    })
    .await
}

/// Serve stdio until EOF: one JSON-RPC message per line in, at most one reply out.
/// Shared by `lagent mcp` and `lagent juno-mcp` (same protocol version).
pub async fn serve_stdio<F, Fut>(handler: F) -> std::io::Result<()>
where
    F: Fn(Value) -> Fut,
    Fut: std::future::Future<Output = Option<Value>>,
{
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handler(msg).await,
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": e.to_string()}})),
        };
        if let Some(r) = reply {
            out.write_all(format!("{r}\n").as_bytes()).await?;
            out.flush().await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_tools_and_durations() {
        assert!(is_own_tool("mcp__lucina-agents__create_agent"));
        assert!(!is_own_tool("mcp__lucina-agents__"));
        assert!(!is_own_tool("mcp__lucina-agents-evil__create_agent"));
        assert!(!is_own_tool("mcp__lucina__note_save"));
        assert_eq!(duration_ms("2h"), Some(7_200_000));
        assert_eq!(duration_ms("3d"), Some(259_200_000));
        assert_eq!(duration_ms("x"), None);
        assert_eq!(urlencode("a b/ü"), "a%20b%2F%C3%BC");
        assert_eq!(tools().len(), 15);
    }

    #[test]
    fn codex_config_lists_and_preapproves_every_tool() {
        let c = codex_config_for(Path::new("/opt/bin/lagent"), "/run/agentd.sock", "agent-1");
        let s = &c["mcp_servers"][SERVER];
        assert_eq!(s["command"], "/opt/bin/lagent");
        assert_eq!(s["args"], json!(["mcp"]));
        assert_eq!(s["env"], json!({crate::config::SOCKET_ENV: "/run/agentd.sock", "LUCINA_AGENT_ID": "agent-1"}));
        assert_eq!(s["default_tools_approval_mode"], "prompt");
        let names: Vec<&str> = s["enabled_tools"].as_array().unwrap().iter().map(|n| n.as_str().unwrap()).collect();
        assert_eq!(names.len(), 15);
        assert!(names.contains(&"create_agent") && names.contains(&"search_agents"));
        let approvals = s["tools"].as_object().unwrap();
        assert_eq!(approvals.len(), 15);
        assert!(names.iter().all(|n| approvals[*n] == json!({"approval_mode": "approve"})));
        assert!(codex_config(None).is_none());
    }
}
