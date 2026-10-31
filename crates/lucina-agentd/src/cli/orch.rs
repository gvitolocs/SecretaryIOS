//! `lagent schedule …`, `lagent heartbeat …`, `lagent search`, `lagent detach` (Paseo
//! `commands/schedule/*`, `commands/heartbeat/*`, `agent detach`). Inside an agent
//! (`$LUCINA_AGENT_ID`), the daemon treats these as that agent's: a heartbeat targets
//! it, and it changes only what it created.

use clap::{Args, Subcommand};
use serde_json::{json, Value};

use super::{parse_duration_ms, s, table, CmdError, Output};
use crate::client::{Client, ClientError};
use crate::mcp::urlencode;

#[derive(Args, Debug, Clone, Default)]
pub struct CadenceArgs {
    /// Fixed interval, e.g. 5m or 1h
    #[arg(long)]
    pub every: Option<String>,
    /// Cron expression (5 fields)
    #[arg(long)]
    pub cron: Option<String>,
    /// IANA time zone for --cron (default: the owner's)
    #[arg(long)]
    pub timezone: Option<String>,
}

#[derive(Args, Debug)]
pub struct ScheduleCreateArgs {
    pub prompt: String,
    #[command(flatten)]
    pub cadence: CadenceArgs,
    #[arg(long)]
    pub name: Option<String>,
    /// provider or provider/model
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub mode: Option<String>,
    #[arg(long)]
    pub thinking: Option<String>,
    #[arg(long)]
    pub cwd: Option<String>,
    /// local (default) or worktree
    #[arg(long)]
    pub isolation: Option<String>,
    /// Keep the run's workspace (and agent) after it ends
    #[arg(long)]
    pub keep: bool,
    /// Also run right away
    #[arg(long)]
    pub run_now: bool,
    #[arg(long)]
    pub max_runs: Option<u32>,
    /// e.g. 2h, 3d
    #[arg(long)]
    pub expires_in: Option<String>,
}

#[derive(Args, Debug)]
pub struct ScheduleUpdateArgs {
    pub id: String,
    #[command(flatten)]
    pub cadence: CadenceArgs,
    /// New name ("" clears)
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub prompt: Option<String>,
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub mode: Option<String>,
    #[arg(long)]
    pub cwd: Option<String>,
    #[arg(long)]
    pub max_runs: Option<u32>,
    #[arg(long)]
    pub no_max_runs: bool,
    #[arg(long)]
    pub expires_in: Option<String>,
    #[arg(long)]
    pub no_expires_in: bool,
}

#[derive(Subcommand, Debug)]
pub enum ScheduleCommand {
    /// Create a recurring schedule that starts a new agent each time
    Create(ScheduleCreateArgs),
    /// List schedules and heartbeats
    Ls,
    /// Show a schedule with its run history
    Inspect { id: String },
    /// Recent runs of a schedule
    Logs { id: String },
    Pause { id: String },
    Resume { id: String },
    Delete { id: String },
    /// Trigger one run without touching the cadence
    RunOnce { id: String },
    /// Update a schedule in place
    Update(ScheduleUpdateArgs),
}

#[derive(Args, Debug)]
pub struct HeartbeatCreateArgs {
    pub prompt: String,
    /// Cron expression (5 fields)
    #[arg(long)]
    pub cron: String,
    #[arg(long)]
    pub timezone: Option<String>,
    #[arg(long)]
    pub name: Option<String>,
    #[arg(long)]
    pub max_runs: Option<u32>,
    #[arg(long)]
    pub expires_in: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum HeartbeatCommand {
    /// A recurring prompt to the calling agent (only inside an agent)
    Create(HeartbeatCreateArgs),
    /// Change a heartbeat's cron
    Update {
        id: String,
        #[arg(long)]
        cron: Option<String>,
        #[arg(long)]
        timezone: Option<String>,
    },
    Delete { id: String },
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    pub query: String,
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Leave archived agents out
    #[arg(long)]
    pub no_archived: bool,
}

fn failed(code: &'static str) -> impl Fn(ClientError) -> CmdError {
    move |e| match e {
        ClientError::Api { code: c, message, .. } if c == "SCHEDULE_NOT_FOUND" || c == "SCHEDULE_FORBIDDEN" => CmdError::new(&c, message),
        ClientError::Api { message, .. } => CmdError::new(code, message),
        other => other.into(),
    }
}

fn expires_ms(e: &Option<String>) -> Result<Option<u64>, CmdError> {
    e.as_deref()
        .map(|x| {
            let days = x.strip_suffix('d').and_then(|n| n.trim().parse::<u64>().ok()).map(|n| n * 86_400_000);
            days.or_else(|| parse_duration_ms(x)).ok_or_else(|| CmdError::new("INVALID_DURATION", format!("Invalid duration: {x}")))
        })
        .transpose()
}

fn cadence(c: &CadenceArgs, body: &mut Value, required: bool) -> Result<(), CmdError> {
    match (&c.every, &c.cron) {
        (Some(_), Some(_)) => return Err(CmdError::new("INVALID_CADENCE", "Use either --every or --cron, not both")),
        (Some(e), None) => {
            body["everyMs"] = json!(parse_duration_ms(e).ok_or_else(|| CmdError::new("INVALID_CADENCE", format!("Invalid --every value: {e}")))?);
        }
        (None, Some(cron)) => {
            body["cron"] = json!(cron);
            if let Some(tz) = &c.timezone {
                body["timezone"] = json!(tz);
            }
        }
        (None, None) if required => return Err(CmdError::new("INVALID_CADENCE", "Provide --every or --cron")),
        (None, None) => {
            if let Some(tz) = &c.timezone {
                body["timezone"] = json!(tz);
            }
        }
    }
    Ok(())
}

fn cadence_text(s: &Value) -> String {
    match s["cadence"]["type"].as_str() {
        Some("cron") => {
            let tz = s["cadence"]["timezone"].as_str().map(|t| format!(" ({t})")).unwrap_or_default();
            format!("{}{tz}", s["cadence"]["expression"].as_str().unwrap_or(""))
        }
        _ => format!("every {}", every_text(s["cadence"]["everyMs"].as_u64().unwrap_or(0))),
    }
}

fn every_text(ms: u64) -> String {
    match ms {
        m if m % 3_600_000 == 0 => format!("{}h", m / 3_600_000),
        m if m % 60_000 == 0 => format!("{}m", m / 60_000),
        m if m % 1000 == 0 => format!("{}s", m / 1000),
        m => format!("{m}ms"),
    }
}

fn target_text(s: &Value) -> String {
    match s["target"]["type"].as_str() {
        Some("agent") => format!("heartbeat → {}", s["target"]["agentId"].as_str().unwrap_or("").chars().take(8).collect::<String>()),
        _ => format!("new {}", s["target"]["config"]["provider"].as_str().unwrap_or("agent")),
    }
}

fn schedule_rows(list: &[Value]) -> String {
    let rows: Vec<Vec<String>> = list
        .iter()
        .map(|x| {
            vec![
                s(&x["id"]),
                x["name"].as_str().unwrap_or("").to_string(),
                cadence_text(x),
                target_text(x),
                s(&x["status"]),
                x["nextRunAt"].as_str().unwrap_or("-").to_string(),
                x["createdBy"].as_str().unwrap_or("").to_string(),
            ]
        })
        .collect();
    table(&["SCHEDULE ID", "NAME", "CADENCE", "TARGET", "STATUS", "NEXT RUN", "CREATED BY"], &rows)
}

fn one(v: Value) -> Output {
    let text = schedule_rows(std::slice::from_ref(&v));
    Output { text, json: Some(v) }
}

pub async fn schedule(client: &Client, cmd: &ScheduleCommand) -> Result<Output, CmdError> {
    match cmd {
        ScheduleCommand::Create(a) => {
            let mut body = json!({"prompt": a.prompt});
            cadence(&a.cadence, &mut body, true)?;
            let me = std::env::var("LUCINA_AGENT_ID").ok().filter(|s| !s.is_empty());
            let provider = match (&a.provider, &me) {
                (Some(p), _) => p.clone(),
                (None, Some(m)) => {
                    let mine = client.get(&format!("/agents/{m}")).await?;
                    match mine["model"].as_str() {
                        Some(model) => format!("{}/{model}", s(&mine["provider"])),
                        None => s(&mine["provider"]),
                    }
                }
                (None, None) => return Err(CmdError::new("MISSING_PROVIDER", "--provider is required")),
            };
            body["provider"] = json!(provider);
            let cwd = a.cwd.clone().unwrap_or_else(|| std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default());
            body["cwd"] = json!(cwd);
            for (k, v) in [("name", &a.name), ("modeId", &a.mode), ("thinkingOptionId", &a.thinking), ("isolation", &a.isolation)] {
                if let Some(v) = v {
                    body[k] = json!(v);
                }
            }
            if a.keep {
                body["archiveOnFinish"] = json!(false);
            }
            if a.run_now {
                body["runOnCreate"] = json!(true);
            }
            if let Some(m) = a.max_runs {
                body["maxRuns"] = json!(m);
            }
            if let Some(ms) = expires_ms(&a.expires_in)? {
                body["expiresInMs"] = json!(ms);
            }
            Ok(one(client.post("/schedules", &body).await.map_err(failed("SCHEDULE_CREATE_FAILED"))?))
        }
        ScheduleCommand::Ls => {
            let v = client.get("/schedules").await?;
            let list = v["schedules"].as_array().cloned().unwrap_or_default();
            Ok(Output { text: schedule_rows(&list), json: Some(Value::Array(list)) })
        }
        ScheduleCommand::Inspect { id } => {
            let v = client.get(&format!("/schedules/{id}")).await.map_err(failed("SCHEDULE_NOT_FOUND"))?;
            let mut text = schedule_rows(std::slice::from_ref(&v));
            text.push_str(&format!("\n\nPrompt:\n{}\n\nRuns: {}", s(&v["prompt"]), v["runs"].as_array().map(|r| r.len()).unwrap_or(0)));
            Ok(Output { text, json: Some(v) })
        }
        ScheduleCommand::Logs { id } => {
            let v = client.get(&format!("/schedules/{id}/logs")).await.map_err(failed("SCHEDULE_NOT_FOUND"))?;
            let runs = v["runs"].as_array().cloned().unwrap_or_default();
            let rows: Vec<Vec<String>> = runs
                .iter()
                .map(|r| {
                    vec![
                        s(&r["id"]).chars().take(8).collect(),
                        s(&r["scheduledFor"]),
                        s(&r["status"]),
                        r["agentId"].as_str().map(|a| a.chars().take(8).collect()).unwrap_or_default(),
                        r["error"].as_str().or_else(|| r["output"].as_str()).unwrap_or("").lines().next().unwrap_or("").chars().take(80).collect(),
                    ]
                })
                .collect();
            Ok(Output { text: table(&["RUN", "SCHEDULED FOR", "STATUS", "AGENT", "OUTPUT"], &rows), json: Some(Value::Array(runs)) })
        }
        ScheduleCommand::Pause { id } | ScheduleCommand::Resume { id } | ScheduleCommand::RunOnce { id } => {
            let action = match cmd {
                ScheduleCommand::Pause { .. } => "pause",
                ScheduleCommand::Resume { .. } => "resume",
                _ => "run-once",
            };
            Ok(one(client.post(&format!("/schedules/{id}/{action}"), &json!({})).await.map_err(failed("SCHEDULE_UPDATE_FAILED"))?))
        }
        ScheduleCommand::Delete { id } => {
            client.request("DELETE", &format!("/schedules/{id}"), None).await.map_err(failed("SCHEDULE_DELETE_FAILED"))?;
            Ok(Output { text: format!("Deleted schedule {id}"), json: Some(json!({"scheduleId": id, "deleted": true})) })
        }
        ScheduleCommand::Update(a) => {
            let mut body = json!({});
            cadence(&a.cadence, &mut body, false)?;
            if let Some(n) = &a.name {
                body["name"] = if n.is_empty() { Value::Null } else { json!(n) };
            }
            if let Some(p) = &a.prompt {
                body["prompt"] = json!(p);
            }
            match (a.max_runs, a.no_max_runs) {
                (Some(m), false) => body["maxRuns"] = json!(m),
                (None, true) => body["maxRuns"] = Value::Null,
                (Some(_), true) => return Err(CmdError::new("INVALID_ARGUMENTS", "Use either --max-runs or --no-max-runs")),
                _ => {}
            }
            match (expires_ms(&a.expires_in)?, a.no_expires_in) {
                (Some(ms), false) => body["expiresAt"] = json!(crate::schedules::iso(chrono::Utc::now() + chrono::Duration::milliseconds(ms as i64))),
                (None, true) => body["expiresAt"] = Value::Null,
                (Some(_), true) => return Err(CmdError::new("INVALID_ARGUMENTS", "Use either --expires-in or --no-expires-in")),
                _ => {}
            }
            let mut cfg = json!({});
            for (k, v) in [("provider", &a.provider), ("model", &a.model), ("modeId", &a.mode), ("cwd", &a.cwd)] {
                if let Some(v) = v {
                    cfg[k] = json!(v);
                }
            }
            if cfg.as_object().is_some_and(|o| !o.is_empty()) {
                body["newAgentConfig"] = cfg;
            }
            Ok(one(client.request("PATCH", &format!("/schedules/{}", a.id), Some(&body)).await.map_err(failed("SCHEDULE_UPDATE_FAILED"))?))
        }
    }
}

pub async fn heartbeat(client: &Client, cmd: &HeartbeatCommand) -> Result<Output, CmdError> {
    match cmd {
        HeartbeatCommand::Create(a) => {
            if std::env::var("LUCINA_AGENT_ID").map(|s| s.is_empty()).unwrap_or(true) {
                return Err(CmdError::new("NOT_IN_AGENT", "lagent heartbeat works only inside an agent ($LUCINA_AGENT_ID)"));
            }
            let mut body = json!({"prompt": a.prompt, "cron": a.cron, "agentId": "self", "runOnCreate": false});
            for (k, v) in [("timezone", &a.timezone), ("name", &a.name)] {
                if let Some(v) = v {
                    body[k] = json!(v);
                }
            }
            if let Some(m) = a.max_runs {
                body["maxRuns"] = json!(m);
            }
            if let Some(ms) = expires_ms(&a.expires_in)? {
                body["expiresInMs"] = json!(ms);
            }
            Ok(one(client.post("/schedules", &body).await.map_err(failed("HEARTBEAT_CREATE_FAILED"))?))
        }
        HeartbeatCommand::Update { id, cron, timezone } => {
            let mut body = json!({});
            if let Some(c) = cron {
                body["cron"] = json!(c);
            }
            if let Some(t) = timezone {
                body["timezone"] = json!(t);
            }
            Ok(one(client.request("PATCH", &format!("/schedules/{id}"), Some(&body)).await.map_err(failed("HEARTBEAT_UPDATE_FAILED"))?))
        }
        HeartbeatCommand::Delete { id } => {
            client.request("DELETE", &format!("/schedules/{id}"), None).await.map_err(failed("HEARTBEAT_DELETE_FAILED"))?;
            Ok(Output { text: format!("Deleted heartbeat {id}"), json: Some(json!({"scheduleId": id, "deleted": true})) })
        }
    }
}

pub async fn search(client: &Client, a: &SearchArgs) -> Result<Output, CmdError> {
    let v = client.get(&format!("/agents/search?q={}&limit={}&archived={}", urlencode(&a.query), a.limit, !a.no_archived)).await?;
    let results = v["results"].as_array().cloned().unwrap_or_default();
    let rows: Vec<Vec<String>> = results
        .iter()
        .map(|r| {
            let m = r["matches"].as_array().and_then(|m| m.first()).cloned().unwrap_or(Value::Null);
            vec![
                s(&r["agentId"]).chars().take(8).collect(),
                r["title"].as_str().unwrap_or("").to_string(),
                s(&r["status"]),
                m["field"].as_str().unwrap_or("").to_string(),
                m["snippet"].as_str().unwrap_or("").replace('\n', " ").chars().take(80).collect(),
            ]
        })
        .collect();
    Ok(Output { text: table(&["AGENT ID", "TITLE", "STATUS", "MATCH", "SNIPPET"], &rows), json: Some(v) })
}

pub async fn detach(client: &Client, id: &str) -> Result<Output, CmdError> {
    let v = client.post(&format!("/agents/{id}/detach"), &json!({})).await?;
    Ok(Output { text: format!("Detached {}", s(&v["id"])), json: Some(v) })
}
