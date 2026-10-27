//! `lagent`: the CLI. Commands, flags and `--json` output follow Paseo 0.13.2's
//! (`cli/dist/commands/agent/*.js`), so scripts and the differential tests can use
//! either. Errors print `{"error": {code, message}}` (JSON) or `Error: ...` and exit 1.

use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Parser, Subcommand};
use lucina_agents::curator::{curate_activity, CurateOptions};
use lucina_agents::projection::project_rows;
use lucina_agents::{TimelineItem, TimelineRow};
use serde_json::{json, Value};

use crate::client::{Client, ClientError};
use crate::config::{default_socket, shorten_path};

#[derive(Parser, Debug)]
#[command(name = "lagent", about = "Lucina Agents: control coding agents from the command line", version)]
pub struct Cli {
    /// Output format: table, json
    #[arg(short = 'o', long = "format", global = true, default_value = "table")]
    pub format: String,
    /// JSON output (alias for --format json)
    #[arg(long, global = true)]
    pub json: bool,
    /// Daemon socket (default: $LUCINA_AGENTD_SOCKET or $XDG_RUNTIME_DIR/lucina-agentd/agentd.sock)
    #[arg(long, global = true)]
    pub socket: Option<std::path::PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create and start an agent with a task
    Run(RunArgs),
    /// List agents. By default excludes archived agents.
    Ls(LsArgs),
    /// View agent activity/timeline
    Logs(LogsArgs),
    /// Attach to a running agent's output stream
    Attach { id: String },
    /// Send a message/task to an existing agent
    Send(SendArgs),
    /// Interrupt an agent if it is running (no-op for idle agents)
    Stop(TargetArgs),
    /// Wait for an agent to become idle
    Wait(WaitArgs),
    /// Archive an agent (soft-delete)
    Archive(ArchiveArgs),
    /// Restore an archived agent
    Unarchive { id: String },
    /// Delete an agent (interrupt if running, then hard-delete)
    Delete(TargetArgs),
    /// Show detailed information about an agent
    Inspect { id: String },
    /// List providers
    Providers,
}

#[derive(Args, Debug)]
pub struct RunArgs {
    pub prompt: String,
    #[arg(long)]
    pub title: Option<String>,
    /// Agent provider, or provider/model
    #[arg(long, default_value = "claude")]
    pub provider: String,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub thinking: Option<String>,
    #[arg(long)]
    pub mode: Option<String>,
    #[arg(long)]
    pub cwd: Option<String>,
    #[arg(long = "env", value_name = "KEY=VALUE")]
    pub env: Vec<String>,
    #[arg(long = "label", value_name = "KEY=VALUE")]
    pub label: Vec<String>,
    /// Return right after creating the agent
    #[arg(long)]
    pub no_wait: bool,
    /// Deprecated alias of --no-wait (Paseo 0.12)
    #[arg(short = 'd', long)]
    pub background: bool,
    #[arg(long)]
    pub wait_timeout: Option<String>,
    #[arg(long)]
    pub idempotency_key: Option<String>,
    /// Run through a lane category (deep, routine, bulk) with quota fall-through;
    /// overrides --provider/--model
    #[arg(long)]
    pub chain: Option<String>,
}

#[derive(Args, Debug)]
pub struct LsArgs {
    /// Include archived agents
    #[arg(short = 'a', long)]
    pub all: bool,
    #[arg(long)]
    pub status: Option<String>,
    #[arg(long)]
    pub cwd: Option<String>,
    #[arg(long = "label", value_name = "KEY=VALUE")]
    pub label: Vec<String>,
}

#[derive(Args, Debug)]
pub struct LogsArgs {
    pub id: String,
    #[arg(short = 'f', long)]
    pub follow: bool,
    #[arg(long)]
    pub tail: Option<String>,
    #[arg(long)]
    pub filter: Option<String>,
}

#[derive(Args, Debug)]
pub struct SendArgs {
    pub id: String,
    pub prompt: Option<String>,
    #[arg(long = "prompt", id = "prompt_flag")]
    pub prompt_flag: Option<String>,
    #[arg(long)]
    pub no_wait: bool,
    /// interrupt (default) or steer
    #[arg(long)]
    pub behavior: Option<String>,
}

#[derive(Args, Debug)]
pub struct TargetArgs {
    pub id: Option<String>,
    #[arg(long)]
    pub all: bool,
    #[arg(long)]
    pub cwd: Option<String>,
}

#[derive(Args, Debug)]
pub struct WaitArgs {
    pub id: String,
    /// Maximum wait time (e.g. 30, 30s, 5m)
    #[arg(long)]
    pub timeout: Option<String>,
}

#[derive(Args, Debug)]
pub struct ArchiveArgs {
    pub id: String,
    /// Archive a running agent (interrupts the active run first)
    #[arg(long)]
    pub force: bool,
}

/// A command failure, rendered like Paseo's `CommandError`.
#[derive(Debug)]
pub struct CmdError {
    pub code: String,
    pub message: String,
    pub details: Option<String>,
}

impl CmdError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        CmdError { code: code.into(), message: message.into(), details: None }
    }

    fn details(mut self, d: &str) -> Self {
        self.details = Some(d.into());
        self
    }
}

impl From<ClientError> for CmdError {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::Api { code, message, .. } => CmdError { code, message, details: None },
            other => CmdError::new("DAEMON_UNREACHABLE", other.to_string()),
        }
    }
}

/// What a command prints: a value for `--json`, and the table/plain text.
pub struct Output {
    pub json: Option<Value>,
    pub text: String,
}

fn parse_duration_ms(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1000)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60_000)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3_600_000)
    } else {
        (s, 1000)
    };
    num.trim().parse::<f64>().ok().filter(|n| *n > 0.0).map(|n| (n * mult as f64) as u64)
}

fn kv(list: &[String], code: &str) -> Result<serde_json::Map<String, Value>, CmdError> {
    let mut map = serde_json::Map::new();
    for item in list {
        let (k, v) = item
            .split_once('=')
            .ok_or_else(|| CmdError::new(code, format!("Expected KEY=VALUE, got: {item}")))?;
        map.insert(k.to_string(), Value::String(v.to_string()));
    }
    Ok(map)
}

fn relative_time(iso: &str) -> String {
    let then = chrono::DateTime::parse_from_rfc3339(iso).map(|d| d.timestamp()).unwrap_or(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let s = (now - then).max(0);
    if s < 60 {
        "just now".into()
    } else if s < 3600 {
        format!("{} minutes ago", s / 60)
    } else if s < 86400 {
        format!("{} hours ago", s / 3600)
    } else {
        format!("{} days ago", s / 86400)
    }
}

fn normalize_model(v: &Value) -> Option<String> {
    let s = v.as_str()?.trim();
    (!s.is_empty() && !s.eq_ignore_ascii_case("default")).then(|| s.to_string())
}

fn resolved_model(snap: &Value) -> Option<String> {
    normalize_model(&snap["runtimeInfo"]["model"]).or_else(|| normalize_model(&snap["model"]))
}

/// Paseo `toListItem`.
fn ls_item(snap: &Value) -> Value {
    let provider = snap["provider"].as_str().unwrap_or_default();
    json!({
        "id": snap["id"],
        "shortId": snap["id"].as_str().unwrap_or_default().chars().take(7).collect::<String>(),
        "name": snap["title"].as_str().unwrap_or("-"),
        "provider": match resolved_model(snap) { Some(m) => format!("{provider}/{m}"), None => provider.to_string() },
        "thinking": snap["effectiveThinkingOptionId"].as_str().unwrap_or("auto"),
        "status": snap["status"],
        "cwd": shorten_path(snap["cwd"].as_str().unwrap_or_default()),
        "created": relative_time(snap["createdAt"].as_str().unwrap_or_default()),
    })
}

/// Paseo `toInspectData`.
fn inspect_data(snap: &Value) -> Value {
    let usage = &snap["lastUsage"];
    let caps = &snap["capabilities"];
    let num = |v: &Value| if v.is_number() { v.clone() } else { json!(0) };
    let flag = |v: &Value| Value::Bool(v.as_bool().unwrap_or(false));
    json!({
        "Id": snap["id"],
        "Name": snap["title"].as_str().unwrap_or("-"),
        "Provider": snap["provider"],
        "Model": resolved_model(snap).unwrap_or_else(|| "-".into()),
        "Thinking": snap["effectiveThinkingOptionId"].as_str().unwrap_or("auto"),
        "Status": snap["status"],
        "Archived": !snap["archivedAt"].is_null(),
        "ArchivedAt": snap["archivedAt"],
        "Mode": snap["currentModeId"].as_str().unwrap_or("default"),
        "Cwd": snap["cwd"],
        "CreatedAt": snap["createdAt"],
        "UpdatedAt": snap["updatedAt"],
        "LastUsage": if usage.is_object() { json!({
            "InputTokens": num(&usage["inputTokens"]),
            "OutputTokens": num(&usage["outputTokens"]),
            "CachedTokens": num(&usage["cachedInputTokens"]),
            "CostUsd": num(&usage["totalCostUsd"]),
        }) } else { Value::Null },
        "Capabilities": if caps.is_object() { json!({
            "Streaming": flag(&caps["supportsStreaming"]),
            "Persistence": flag(&caps["supportsSessionPersistence"]),
            "DynamicModes": flag(&caps["supportsDynamicModes"]),
            "McpServers": flag(&caps["supportsMcpServers"]),
        }) } else { Value::Null },
        "AvailableModes": snap["availableModes"].as_array().map(|m| m.iter().map(|x| json!({"id": x["id"], "label": x["label"]})).collect::<Vec<_>>()),
        "PendingPermissions": snap["pendingPermissions"].as_array().map(|p| p.iter().map(|x| json!({"id": x["id"], "tool": x["name"].as_str().unwrap_or("unknown")})).collect::<Vec<_>>()).unwrap_or_default(),
        "Worktree": snap["labels"]["lucina.worktree"],
        "ParentAgentId": snap["labels"][crate::manager::PARENT_LABEL],
    })
}

fn table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    let line = |cells: Vec<String>| {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let mut out = vec![line(headers.iter().map(|h| h.to_string()).collect())];
    out.extend(rows.iter().map(|r| line(r.clone())));
    out.join("\n")
}

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        other => other.to_string(),
    }
}

fn single(v: Value, keys: &[&str]) -> Output {
    let rows: Vec<Vec<String>> = vec![keys.iter().map(|k| s(&v[*k])).collect()];
    let headers: Vec<String> = keys.iter().map(|k| upper_snake(k)).collect();
    let headers: Vec<&str> = headers.iter().map(String::as_str).collect();
    Output { text: table(&headers, &rows), json: Some(v) }
}

fn upper_snake(k: &str) -> String {
    let mut out = String::new();
    for c in k.chars() {
        if c.is_uppercase() {
            out.push(' ');
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

/// Projected timeline items (Paseo `fetchProjectedTimelineItems`).
async fn projected_items(client: &Client, id: &str) -> Result<Vec<TimelineItem>, CmdError> {
    let v = client.get(&format!("/agents/{id}/timeline")).await?;
    let rows: Vec<TimelineRow> = serde_json::from_value(v["rows"].clone())
        .map_err(|e| CmdError::new("BAD_RESPONSE", e.to_string()))?;
    Ok(project_rows(&rows).into_iter().map(|e| e.item).collect())
}

/// Paseo `matchesFilter`.
fn matches_filter(item: &TimelineItem, filter: Option<&str>) -> bool {
    let Some(f) = filter else { return true };
    let f = f.to_lowercase();
    let t = item.kind();
    match f.as_str() {
        "tools" => t == "tool_call",
        "text" => matches!(t, "user_message" | "assistant_message" | "reasoning"),
        "errors" => t == "error",
        "permissions" => t.contains("permission"),
        _ => t.contains(&f),
    }
}

/// Paseo `formatAgentActivityTranscript`.
fn transcript(items: &[TimelineItem], tail: Option<usize>) -> String {
    if tail == Some(0) {
        return String::new();
    }
    curate_activity(items, &CurateOptions { max_items: tail.unwrap_or(0), ..CurateOptions::default() })
}

/// How a command words "no such agent" (Paseo's differ per command).
#[derive(Clone, Copy)]
enum NotFound {
    /// logs, stop, delete, send, wait: "No agent found matching: <id>".
    Matching,
    /// inspect, archive: "Agent not found: <id>".
    Plain,
}

async fn resolve_as(client: &Client, id: &str, style: NotFound) -> Result<Value, CmdError> {
    client.get(&format!("/agents/{id}")).await.map_err(|e| match e {
        ClientError::Api { status: 404, .. } => match style {
            NotFound::Matching => CmdError::new("AGENT_NOT_FOUND", format!("No agent found matching: {id}"))
                .details("Use `lagent ls` to list available agents"),
            NotFound::Plain => CmdError::new("AGENT_NOT_FOUND", format!("Agent not found: {id}"))
                .details("Use \"lagent ls\" to list available agents"),
        },
        other => other.into(),
    })
}

async fn resolve(client: &Client, id: &str) -> Result<Value, CmdError> {
    resolve_as(client, id, NotFound::Matching).await
}

async fn wait_for(client: &Client, id: &str, timeout_ms: u64) -> Result<Value, CmdError> {
    Ok(client.post(&format!("/agents/{id}/wait?timeoutMs={timeout_ms}"), &json!({})).await?)
}

/// Agents selected by an id, `--all` or `--cwd` (Paseo stop/delete).
async fn targets(client: &Client, args: &TargetArgs) -> Result<Vec<Value>, CmdError> {
    if args.id.is_none() && !args.all && args.cwd.is_none() {
        return Err(CmdError::new("MISSING_ARGUMENT", "Agent ID required unless --all or --cwd is specified"));
    }
    if let Some(id) = &args.id {
        return Ok(vec![resolve(client, id).await?]);
    }
    let all = client.get("/agents?archived=true").await?;
    let agents = all["agents"].as_array().cloned().unwrap_or_default();
    Ok(agents
        .into_iter()
        .filter(|a| a["archivedAt"].is_null())
        .filter(|a| match &args.cwd {
            Some(cwd) => a["cwd"].as_str().is_some_and(|c| c == cwd || c.starts_with(&format!("{}/", cwd.trim_end_matches('/')))),
            None => true,
        })
        .collect())
}

pub async fn run(cli: &Cli) -> Result<Output, CmdError> {
    let client = Client::new(cli.socket.clone().unwrap_or_else(default_socket));
    match &cli.command {
        Command::Run(a) => {
            let wait = !a.no_wait && !a.background;
            let timeout_ms = match &a.wait_timeout {
                Some(t) => parse_duration_ms(t).ok_or_else(|| CmdError::new("INVALID_TIMEOUT", "Invalid --wait-timeout value"))?,
                None => 0,
            };
            let cwd = match &a.cwd {
                Some(c) => c.clone(),
                None => std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_else(|_| ".".into()),
            };
            let body = json!({
                "provider": a.provider,
                "model": a.model,
                "cwd": cwd,
                "prompt": a.prompt,
                "title": a.title,
                "labels": kv(&a.label, "INVALID_LABEL")?,
                "env": kv(&a.env, "INVALID_ENV")?,
                "modeId": a.mode,
                "thinkingOptionId": a.thinking,
                "idempotencyKey": a.idempotency_key,
                "chain": a.chain,
            });
            let created = client.post("/agents", &body).await.map_err(|e| match e {
                ClientError::Api { code, message, .. } if code != "AGENT_NOT_FOUND" => CmdError {
                    code: "AGENT_CREATE_FAILED".into(),
                    message: format!("Failed to create agent: {message}"),
                    details: Some(code),
                },
                other => other.into(),
            })?;
            let id = created["id"].as_str().unwrap_or_default().to_string();
            let (agent, status) = if wait {
                let state = wait_for(&client, &id, timeout_ms).await?;
                let status = match state["status"].as_str().unwrap_or("idle") {
                    "idle" => "completed".to_string(),
                    other => other.to_string(),
                };
                (state["final"].clone(), status)
            } else {
                let st = if created["status"] == "running" { "running" } else { "created" };
                (created, st.to_string())
            };
            let result = json!({
                "agentId": agent["id"],
                "status": status,
                "provider": agent["provider"],
                "cwd": agent["cwd"],
                "title": agent["title"],
            });
            Ok(single(result, &["agentId", "status", "provider", "cwd", "title"]))
        }
        Command::Ls(a) => {
            let v = client.get(&format!("/agents?archived={}", a.all)).await?;
            let mut agents: Vec<Value> = v["agents"].as_array().cloned().unwrap_or_default();
            if !a.all {
                agents.retain(|x| x["archivedAt"].is_null());
            }
            if let Some(st) = &a.status {
                agents.retain(|x| x["status"] == st.as_str());
            }
            if let Some(cwd) = &a.cwd {
                agents.retain(|x| x["cwd"].as_str().is_some_and(|c| c == cwd || c.starts_with(&format!("{}/", cwd.trim_end_matches('/')))));
            }
            let labels = kv(&a.label, "INVALID_LABEL")?;
            agents.retain(|x| labels.iter().all(|(k, v)| x["labels"][k] == *v));
            let order = |st: &Value| match st.as_str() {
                Some("running") => 0,
                Some("idle") => 1,
                _ => 999,
            };
            agents.sort_by(|x, y| {
                order(&x["status"])
                    .cmp(&order(&y["status"]))
                    .then_with(|| y["createdAt"].as_str().cmp(&x["createdAt"].as_str()))
            });
            let items: Vec<Value> = agents.iter().map(ls_item).collect();
            let rows: Vec<Vec<String>> = items
                .iter()
                .map(|i| ["shortId", "name", "provider", "thinking", "status", "cwd", "created"].iter().map(|k| s(&i[*k])).collect())
                .collect();
            Ok(Output {
                text: table(&["AGENT ID", "NAME", "PROVIDER", "THINKING", "STATUS", "CWD", "CREATED"], &rows),
                json: Some(Value::Array(items)),
            })
        }
        Command::Logs(a) => {
            let agent = resolve(&client, &a.id).await?;
            let id = agent["id"].as_str().unwrap_or_default().to_string();
            let tail = match &a.tail {
                Some(t) => Some(t.parse::<usize>().map_err(|_| CmdError::new("INVALID_TAIL", format!("Invalid --tail value: {t}")))?),
                None => None,
            };
            let items: Vec<TimelineItem> = projected_items(&client, &id)
                .await?
                .into_iter()
                .filter(|i| matches_filter(i, a.filter.as_deref()))
                .collect();
            if a.follow {
                let existing = transcript(&items, Some(tail.unwrap_or(10)));
                if !existing.is_empty() && existing != "No activity to display." {
                    println!("{existing}");
                }
                println!("\n--- Following logs (Ctrl+C to stop) ---\n");
                follow(&client, &id, a.filter.clone(), false).await?;
                return Ok(Output { json: None, text: String::new() });
            }
            Ok(Output { json: None, text: transcript(&items, tail) })
        }
        Command::Attach { id } => {
            let agent = resolve(&client, id).await?;
            let id = agent["id"].as_str().unwrap_or_default().to_string();
            follow(&client, &id, None, true).await?;
            Ok(Output { json: None, text: String::new() })
        }
        Command::Send(a) => {
            let prompt = match (&a.prompt, &a.prompt_flag) {
                (Some(_), Some(_)) => return Err(CmdError::new("CONFLICTING_PROMPT_INPUT", "Provide exactly one of prompt argument, --prompt, or --prompt-file")),
                (Some(p), None) | (None, Some(p)) => p.clone(),
                (None, None) => return Err(CmdError::new("MISSING_PROMPT", "A prompt is required")),
            };
            let agent = resolve(&client, &a.id).await?;
            let id = agent["id"].as_str().unwrap_or_default().to_string();
            let behavior = a.behavior.clone().unwrap_or_else(|| "interrupt".into());
            client
                .post(&format!("/agents/{id}/send"), &json!({"prompt": prompt, "activeTurnBehavior": behavior}))
                .await
                .map_err(|e| CmdError::new("SEND_FAILED", format!("Failed to send message: {e}")))?;
            let result = if a.no_wait {
                json!({"agentId": a.id, "status": "sent", "message": "Message sent, not waiting for completion"})
            } else {
                let state = wait_for(&client, &id, 600_000).await?;
                match state["status"].as_str() {
                    Some("timeout") => json!({"agentId": id, "status": "timeout", "message": "Timed out waiting for agent to finish"}),
                    Some("permission") => json!({"agentId": id, "status": "permission", "message": "Agent is waiting for permission"}),
                    Some("error") => json!({"agentId": id, "status": "error", "message": state["error"].as_str().unwrap_or("Agent finished with error")}),
                    _ => json!({"agentId": id, "status": "completed", "message": "Agent completed processing the message"}),
                }
            };
            Ok(single(result, &["agentId", "status", "message"]))
        }
        Command::Stop(a) => {
            let agents = targets(&client, a).await?;
            let mut stopped = Vec::new();
            for agent in agents {
                let id = agent["id"].as_str().unwrap_or_default().to_string();
                if agent["status"] != "running" {
                    continue;
                }
                match client.post(&format!("/agents/{id}/stop"), &json!({})).await {
                    Ok(v) if v["stopped"] == true => stopped.push(Value::String(id)),
                    Ok(_) => {}
                    Err(e) => eprintln!("Warning: Failed to stop agent {}: {e}", &id[..id.len().min(7)]),
                }
            }
            let result = json!({"stoppedCount": stopped.len(), "agentIds": stopped});
            Ok(single(result, &["stoppedCount", "agentIds"]))
        }
        Command::Wait(a) => {
            let agent = resolve(&client, &a.id).await?;
            let id = agent["id"].as_str().unwrap_or_default().to_string();
            let (timeout_ms, label) = match &a.timeout {
                Some(t) => {
                    let ms = parse_duration_ms(t).ok_or_else(|| CmdError::new("INVALID_TIMEOUT", "Invalid timeout value"))?;
                    let secs = ms / 1000;
                    (ms, Some(format!("{secs} second{}", if secs == 1 { "" } else { "s" })))
                }
                None => (0, None),
            };
            let state = wait_for(&client, &id, timeout_ms).await?;
            let recent = projected_items(&client, &id).await.ok().map(|items| transcript(&items, Some(5)));
            let with_recent = |msg: String| match recent.as_deref().filter(|t| !t.trim().is_empty()) {
                Some(t) => format!("{msg}\nLast 5 activity items:\n{t}"),
                None => msg,
            };
            let result = match state["status"].as_str() {
                Some("timeout") => {
                    let msg = match &label {
                        Some(l) => format!("Agent did not finish within {l}. Run `lagent wait {id}` again to keep waiting."),
                        None => format!("Agent wait timed out. Run `lagent wait {id}` again to keep waiting."),
                    };
                    json!({"agentId": id, "status": "timeout", "message": with_recent(msg)})
                }
                Some("permission") => {
                    let kind = state["final"]["pendingPermissions"][0]["kind"].as_str().map(str::to_string);
                    let msg = match kind {
                        Some(k) => format!("Agent is waiting for permission: {k}"),
                        None => "Agent is waiting for permission".into(),
                    };
                    json!({"agentId": id, "status": "permission", "message": msg})
                }
                Some("error") => json!({"agentId": id, "status": "error", "message": state["error"].as_str().unwrap_or("Agent finished with error")}),
                _ => json!({"agentId": id, "status": "idle", "message": with_recent("Agent is idle.".into())}),
            };
            Ok(single(result, &["agentId", "status", "message"]))
        }
        Command::Archive(a) => {
            let agent = resolve_as(&client, &a.id, NotFound::Plain).await?;
            let id = agent["id"].as_str().unwrap_or_default().to_string();
            let short = &id[..id.len().min(7)];
            if !agent["archivedAt"].is_null() {
                return Err(CmdError::new("AGENT_ALREADY_ARCHIVED", format!("Agent {short} is already archived")));
            }
            if agent["status"] == "running" && !a.force {
                return Err(CmdError::new("AGENT_RUNNING", format!("Agent {short} is currently running")).details(
                    "Use --force to archive a running agent (it will interrupt the active run), or stop it first with: lagent stop. Use lagent delete to hard-delete it.",
                ));
            }
            let v = client
                .post(&format!("/agents/{id}/archive"), &json!({}))
                .await
                .map_err(|e| CmdError::new("ARCHIVE_FAILED", format!("Failed to archive agent: {e}")))?;
            Ok(single(json!({"agentId": id, "status": "archived", "archivedAt": v["archivedAt"]}), &["agentId", "status", "archivedAt"]))
        }
        Command::Unarchive { id } => {
            let agent = resolve(&client, id).await?;
            let id = agent["id"].as_str().unwrap_or_default().to_string();
            let v = client.post(&format!("/agents/{id}/unarchive"), &json!({})).await?;
            Ok(single(json!({"agentId": id, "status": v["status"]}), &["agentId", "status"]))
        }
        Command::Delete(a) => {
            let agents = targets(&client, a).await?;
            let mut deleted = Vec::new();
            for agent in agents {
                let id = agent["id"].as_str().unwrap_or_default().to_string();
                match client.request("DELETE", &format!("/agents/{id}"), None).await {
                    Ok(_) => deleted.push(Value::String(id)),
                    Err(e) => eprintln!("Warning: Failed to delete agent {}: {e}", &id[..id.len().min(7)]),
                }
            }
            Ok(single(json!({"deletedCount": deleted.len(), "agentIds": deleted}), &["deletedCount", "agentIds"]))
        }
        Command::Inspect { id } => {
            let agent = resolve_as(&client, id, NotFound::Plain).await?;
            let data = inspect_data(&agent);
            let mut rows = Vec::new();
            for key in ["Id", "Name", "Provider", "Model", "Thinking", "Status", "Archived", "ArchivedAt", "Mode", "Cwd", "CreatedAt", "UpdatedAt"] {
                let value = if key == "Cwd" { shorten_path(data[key].as_str().unwrap_or_default()) } else { s(&data[key]) };
                rows.push(vec![key.to_string(), value]);
            }
            rows.push(vec!["PendingPermissions".into(), {
                let p = data["PendingPermissions"].as_array().cloned().unwrap_or_default();
                if p.is_empty() { "[]".into() } else { p.iter().map(|x| format!("{} ({})", s(&x["id"]), s(&x["tool"]))).collect::<Vec<_>>().join(", ") }
            }]);
            Ok(Output { text: table(&["KEY", "VALUE"], &rows), json: Some(data) })
        }
        Command::Providers => {
            let v = client.get("/providers").await?;
            let list = v.as_array().cloned().unwrap_or_default();
            let rows: Vec<Vec<String>> = list
                .iter()
                .map(|p| vec![s(&p["id"]), s(&p["label"]), s(&p["defaultModeId"])])
                .collect();
            Ok(Output { text: table(&["PROVIDER", "LABEL", "DEFAULT MODE"], &rows), json: Some(Value::Array(list)) })
        }
    }
}

/// Stream an agent's timeline (Paseo `createFollowTranscriptWriter`, simplified:
/// one line per projected text chunk run). `until_idle` stops when a turn ends.
async fn follow(client: &Client, id: &str, filter: Option<String>, until_idle: bool) -> Result<(), CmdError> {
    use std::io::Write;
    let id = id.to_string();
    let mut chain: Option<&'static str> = None;
    client
        .stream(|ev| {
            if ev["agentId"] != id.as_str() || ev["type"] != "agent_stream" {
                return true;
            }
            let event = &ev["event"];
            match event["type"].as_str() {
                Some("timeline") => {
                    let Ok(item) = serde_json::from_value::<TimelineItem>(event["item"].clone()) else { return true };
                    if !matches_filter(&item, filter.as_deref()) {
                        return true;
                    }
                    let mut out = std::io::stdout();
                    match &item {
                        TimelineItem::AssistantMessage { text, .. } | TimelineItem::Reasoning { text } => {
                            let kind = item.kind();
                            if chain != Some(kind) {
                                if chain.is_some() {
                                    let _ = writeln!(out);
                                }
                                if kind == "reasoning" {
                                    let _ = write!(out, "[Thought] ");
                                }
                                chain = Some(kind);
                            }
                            let _ = write!(out, "{text}");
                        }
                        _ => {
                            if chain.take().is_some() {
                                let _ = writeln!(out);
                            }
                            let t = transcript(std::slice::from_ref(&item), None);
                            if t != "No activity to display." {
                                let _ = writeln!(out, "{t}");
                            }
                        }
                    }
                    let _ = out.flush();
                    true
                }
                Some("turn_completed") | Some("turn_failed") | Some("turn_canceled") => {
                    if chain.take().is_some() {
                        println!();
                    }
                    !until_idle
                }
                _ => true,
            }
        })
        .await?;
    Ok(())
}

/// Print an [`Output`] or an error the way Paseo's `withOutput` does; returns the
/// exit code.
pub fn render(cli: &Cli, result: Result<Output, CmdError>) -> i32 {
    let json_mode = cli.json || cli.format == "json";
    match result {
        Ok(out) => {
            if json_mode {
                if let Some(v) = out.json {
                    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                    return 0;
                }
            }
            if !out.text.is_empty() {
                println!("{}", out.text);
            }
            0
        }
        Err(e) => {
            if json_mode {
                let mut err = json!({"code": e.code, "message": e.message});
                if let Some(d) = &e.details {
                    err["details"] = Value::String(d.clone());
                }
                eprintln!("{}", serde_json::to_string_pretty(&json!({"error": err})).unwrap_or_default());
            } else {
                match &e.details {
                    Some(d) => eprintln!("Error: {}\n{d}", e.message),
                    None => eprintln!("Error: {}", e.message),
                }
            }
            1
        }
    }
}
