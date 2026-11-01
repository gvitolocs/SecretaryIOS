//! `lagent task …`, `merge`, `deploy`, `job …`, `device …`, `board` (Lucina only): the
//! pipeline every coding session goes through ([`crate::pipeline`]).

use std::io::Write;
use std::time::Duration;

use clap::{Args, Subcommand};
use serde_json::{json, Value};

use super::{table, CmdError, Output};
use crate::client::{Client, ClientError};

#[derive(Subcommand, Debug)]
pub enum TaskCommand {
    /// Start a task: a fresh worktree from main, registered with its owner and session
    Start(StartArgs),
    /// Take over a worktree made by hand, so it merges through the queue
    Adopt(AdoptArgs),
    /// List tasks
    Ls,
}

#[derive(Args, Debug)]
pub struct StartArgs {
    /// What the task is (also names the branch unless --branch)
    pub title: String,
    /// Any path in the repo (default: ~/Projects/Lucina/SecretaryIOS)
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long)]
    pub branch: Option<String>,
    /// Who asked for it (default: the session)
    #[arg(long)]
    pub owner: Option<String>,
    /// The Claude session or agent working in it (default: $LAGENT_SESSION, $CLAUDE_CODE_HOST_SESSION_ID, $CLAUDE_CODE_SESSION_ID)
    #[arg(long)]
    pub session: Option<String>,
}

#[derive(Args, Debug)]
pub struct AdoptArgs {
    pub path: String,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub owner: Option<String>,
    #[arg(long)]
    pub session: Option<String>,
}

#[derive(Args, Debug)]
pub struct MergeArgs {
    /// The workspace: its id, its path, or its branch (default: the current directory)
    pub workspace: Option<String>,
    /// Keep the workspace after the merge (it continues from the merged commit)
    #[arg(long)]
    pub keep: bool,
    /// A diffgate waiver, rule=reason (repeatable; said in the report)
    #[arg(long)]
    pub allow: Vec<String>,
    /// Queue it and return the job id
    #[arg(long)]
    pub no_wait: bool,
}

#[derive(Args, Debug)]
pub struct DeployArgs {
    /// The runtime (deploy/targets.tsv), e.g. lucina-agentd
    pub runtime: String,
    /// What this deploy brings (goes to the ledger and the ops record)
    #[arg(long)]
    pub note: Option<String>,
    /// Deploy even when deploy-guard plan sees nothing new
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub repo: Option<String>,
    #[arg(long)]
    pub no_wait: bool,
}

#[derive(Subcommand, Debug)]
pub enum JobCommand {
    /// Recent jobs, newest first
    Ls {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// One job with its outcome
    Show { id: String },
    /// A job's log (-f follows it until it ends)
    Logs {
        id: String,
        #[arg(short, long)]
        follow: bool,
    },
    /// Wait for a job, showing its log
    Wait { id: String },
    /// Cancel a queued or running job
    Cancel { id: String },
}

#[derive(Subcommand, Debug)]
pub enum DeviceCommand {
    /// Devices and who holds them
    Ls,
    /// Lease a device (again to renew it)
    Lease {
        device: String,
        #[arg(long)]
        purpose: Option<String>,
        #[arg(long, default_value_t = 30)]
        minutes: i64,
        #[arg(long)]
        holder: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Release a device
    Release {
        device: String,
        #[arg(long)]
        holder: Option<String>,
        /// Release someone else's lease (needs --reason)
        #[arg(long)]
        force: bool,
        #[arg(long)]
        reason: Option<String>,
    },
}

/// A cell: the string, number or bool, blank for null (`cli::s` prints `null`).
fn s(v: &Value) -> String {
    if v.is_null() {
        String::new()
    } else {
        super::s(v)
    }
}

/// The calling session: `$LAGENT_SESSION`, then the Claude Code session ids, else user@host.
pub fn session() -> String {
    for k in ["LAGENT_SESSION", "CLAUDE_CODE_HOST_SESSION_ID", "CLAUDE_CODE_SESSION_ID", "LUCINA_AGENT_ID"] {
        if let Ok(v) = std::env::var(k) {
            if !v.trim().is_empty() {
                return v.trim().to_string();
            }
        }
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "?".into());
    let host = std::fs::read_to_string("/etc/hostname").unwrap_or_default();
    format!("{user}@{}", host.trim())
}

/// The workspace path as the owner's Mac sees it over the nezopt mount.
fn mac_path(p: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let prefix = std::env::var("LAGENT_MAC_PREFIX").unwrap_or_else(|_| "/Users/giuseppe/mnt/nezopt".into());
    match p.strip_prefix(&home) {
        Some(rest) if !home.is_empty() => format!("{prefix}{rest}"),
        _ => p.to_string(),
    }
}

fn api(e: ClientError) -> CmdError {
    e.into()
}

fn task_rows(tasks: &[Value]) -> Output {
    let rows: Vec<Vec<String>> = tasks
        .iter()
        .map(|t| {
            vec![s(&t["workspaceId"]), s(&t["title"]), s(&t["branch"]), s(&t["session"]), t["mergedSha"].as_str().map(|x| x[..x.len().min(10)].to_string()).unwrap_or_default()]
        })
        .collect();
    Output { text: table(&["WORKSPACE ID", "TITLE", "BRANCH", "SESSION", "MERGED"], &rows), json: Some(Value::Array(tasks.to_vec())) }
}

fn started(v: Value) -> Output {
    let w = &v["workspace"];
    let cwd = s(&w["cwd"]);
    let text = format!(
        "Task {} on branch {} (from {}).\n  workspace  {}\n  nezopt     {}\n  mac        {}\nWork and commit there; then `lagent merge {}` puts it on {} (rebase, gates, push, archive).",
        s(&v["task"]["title"]),
        s(&v["task"]["branch"]),
        s(&v["task"]["trunk"]),
        s(&w["workspaceId"]),
        cwd,
        mac_path(&cwd),
        s(&w["workspaceId"]),
        s(&v["task"]["trunk"]),
    );
    Output { text, json: Some(v) }
}

pub async fn task(client: &Client, cmd: &TaskCommand) -> Result<Output, CmdError> {
    match cmd {
        TaskCommand::Start(a) => {
            let session = a.session.clone().unwrap_or_else(session);
            let body = json!({"title": a.title, "repo": a.repo, "branch": a.branch, "owner": a.owner.clone().unwrap_or_else(|| session.clone()), "session": session});
            Ok(started(client.post("/pipeline/tasks", &body).await.map_err(api)?))
        }
        TaskCommand::Adopt(a) => {
            let path = std::fs::canonicalize(&a.path).map(|p| p.display().to_string()).unwrap_or_else(|_| a.path.clone());
            let body = json!({"path": path, "title": a.title, "owner": a.owner, "session": a.session});
            Ok(started(client.post("/pipeline/tasks/adopt", &body).await.map_err(api)?))
        }
        TaskCommand::Ls => {
            let v = client.get("/pipeline/tasks").await.map_err(api)?;
            Ok(task_rows(v["tasks"].as_array().map(Vec::as_slice).unwrap_or_default()))
        }
    }
}

fn job_line(j: &Value) -> Vec<String> {
    let what = match j["kind"].as_str() {
        Some("merge") => format!("merge {}", s(&j["branch"])),
        _ => format!("deploy {}", s(&j["runtime"])),
    };
    vec![s(&j["jobId"]), what, s(&j["state"]), s(&j["outcome"]["code"]), s(&j["createdAt"]), s(&j["requestedBy"])]
}

fn job_output(j: Value) -> Result<Output, CmdError> {
    let o = &j["outcome"];
    if j["state"] == "failed" || j["state"] == "cancelled" {
        let mut e = CmdError::new(o["code"].as_str().unwrap_or("JOB_FAILED"), o["message"].as_str().unwrap_or("the job failed"));
        if let Some(c) = o["conflicts"].as_array().filter(|c| !c.is_empty()) {
            e.details = Some(format!("Conflicting files:\n{}", c.iter().map(|x| format!("  {}", s(x))).collect::<Vec<_>>().join("\n")));
        }
        return Err(e);
    }
    let mut text = format!("{} {}: {}", s(&j["jobId"]), s(&j["state"]), o["message"].as_str().unwrap_or("waiting in the queue"));
    if let Some(a) = j["after"].as_str() {
        text.push_str(&format!("\nworkspace: {a}"));
    }
    Ok(Output { text, json: Some(j) })
}

/// How long `follow` waits for agentd to come back (a deploy of agentd restarts it; the job
/// keeps running in its own unit).
const RECONNECT_FOR: Duration = Duration::from_secs(120);

/// Print the job's log as it grows until the job ends; returns the finished job.
async fn follow(client: &Client, id: &str) -> Result<Value, CmdError> {
    let mut offset = 0u64;
    let mut out = std::io::stderr();
    let mut down_since: Option<std::time::Instant> = None;
    loop {
        let v = match client.get(&format!("/pipeline/jobs/{id}/log?offset={offset}")).await {
            Ok(v) => {
                down_since = None;
                v
            }
            Err(ClientError::Api { code, message, .. }) => return Err(CmdError { code, message, details: None }),
            Err(e) => {
                let since = *down_since.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() > RECONNECT_FOR {
                    return Err(api(e));
                }
                if since.elapsed() < Duration::from_secs(2) {
                    let _ = writeln!(out, "(agentd is restarting; still following {id})");
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let text = v["text"].as_str().unwrap_or_default();
        if !text.is_empty() {
            let _ = write!(out, "{text}");
            let _ = out.flush();
        }
        offset = v["offset"].as_u64().unwrap_or(offset);
        if matches!(v["state"].as_str(), Some("succeeded" | "failed" | "cancelled")) {
            return client.get(&format!("/pipeline/jobs/{id}")).await.map_err(api);
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
    }
}

pub async fn merge(client: &Client, a: &MergeArgs) -> Result<Output, CmdError> {
    let ws = match &a.workspace {
        Some(w) => std::fs::canonicalize(w).map(|p| p.display().to_string()).unwrap_or_else(|_| w.clone()),
        None => std::env::current_dir().map(|p| p.display().to_string()).map_err(|e| CmdError::new("INVALID_OPTIONS", e.to_string()))?,
    };
    let body = json!({"workspace": ws, "keep": a.keep, "allow": a.allow, "requestedBy": session()});
    let job = client.post("/pipeline/merge", &body).await.map_err(api)?;
    queued(client, job, a.no_wait).await
}

pub async fn deploy(client: &Client, a: &DeployArgs) -> Result<Output, CmdError> {
    let body = json!({"runtime": a.runtime, "note": a.note, "force": a.force, "repo": a.repo, "requestedBy": session()});
    let job = client.post("/pipeline/deploy", &body).await.map_err(api)?;
    queued(client, job, a.no_wait).await
}

async fn queued(client: &Client, job: Value, no_wait: bool) -> Result<Output, CmdError> {
    let id = s(&job["jobId"]);
    if no_wait {
        return Ok(Output { text: format!("{id} queued (lagent job wait {id})"), json: Some(job) });
    }
    eprintln!("{id} queued; following it (Ctrl-C stops following, not the job: lagent job wait {id})");
    job_output(follow(client, &id).await?)
}

pub async fn job(client: &Client, cmd: &JobCommand) -> Result<Output, CmdError> {
    match cmd {
        JobCommand::Ls { limit } => {
            let v = client.get(&format!("/pipeline/jobs?limit={limit}")).await.map_err(api)?;
            let jobs = v["jobs"].as_array().cloned().unwrap_or_default();
            let rows: Vec<Vec<String>> = jobs.iter().map(job_line).collect();
            Ok(Output { text: table(&["JOB", "WHAT", "STATE", "RESULT", "QUEUED", "BY"], &rows), json: Some(Value::Array(jobs)) })
        }
        JobCommand::Show { id } => {
            let j = client.get(&format!("/pipeline/jobs/{id}")).await.map_err(api)?;
            Ok(Output { text: serde_json::to_string_pretty(&j).unwrap_or_default(), json: Some(j) })
        }
        JobCommand::Logs { id, follow: f } => {
            if *f {
                let j = follow(client, id).await?;
                return job_output(j);
            }
            let v = client.get(&format!("/pipeline/jobs/{id}/log")).await.map_err(api)?;
            Ok(Output { text: v["text"].as_str().unwrap_or_default().trim_end().to_string(), json: Some(v) })
        }
        JobCommand::Wait { id } => job_output(follow(client, id).await?),
        JobCommand::Cancel { id } => job_output(client.post(&format!("/pipeline/jobs/{id}/cancel"), &json!({})).await.map_err(api)?),
    }
}

pub async fn device(client: &Client, cmd: &DeviceCommand) -> Result<Output, CmdError> {
    match cmd {
        DeviceCommand::Ls => {
            let v = client.get("/devices").await.map_err(api)?;
            let list = v["devices"].as_array().cloned().unwrap_or_default();
            let rows: Vec<Vec<String>> = list.iter().map(|d| vec![s(&d["device"]), s(&d["lease"]["holder"]), s(&d["lease"]["expiresAt"]), s(&d["lease"]["purpose"]), s(&d["about"])]).collect();
            Ok(Output { text: table(&["DEVICE", "HOLDER", "UNTIL", "PURPOSE", "ABOUT"], &rows), json: Some(Value::Array(list)) })
        }
        DeviceCommand::Lease { device, purpose, minutes, holder, workspace } => {
            let body = json!({"holder": holder.clone().unwrap_or_else(session), "purpose": purpose, "minutes": minutes, "workspaceId": workspace});
            let l = client.post(&format!("/devices/{device}/lease"), &body).await.map_err(api)?;
            Ok(Output { text: format!("{} leased to {} until {}", s(&l["device"]), s(&l["holder"]), s(&l["expiresAt"])), json: Some(l) })
        }
        DeviceCommand::Release { device, holder, force, reason } => {
            let body = json!({"holder": holder.clone().unwrap_or_else(session), "force": force, "reason": reason});
            let v = client.post(&format!("/devices/{device}/release"), &body).await.map_err(api)?;
            let text = if v["released"] == true { format!("{device} released") } else { format!("{device} had no lease") };
            Ok(Output { text, json: Some(v) })
        }
    }
}

pub async fn board(client: &Client) -> Result<Output, CmdError> {
    let v = client.get("/pipeline/board").await.map_err(api)?;
    let mut text = String::new();
    let ws = v["workspaces"].as_array().cloned().unwrap_or_default();
    let rows: Vec<Vec<String>> = ws
        .iter()
        .map(|w| {
            vec![
                s(&w["state"]),
                s(&w["project"]),
                s(&w["branch"]),
                format!("{}{}", w["ahead"].as_u64().map(|a| format!("+{a}")).unwrap_or_default(), if w["dirty"] == true { " dirty" } else { "" }),
                w["idleDays"].as_i64().map(|d| format!("{d}d")).unwrap_or_default(),
                s(&w["task"]["session"]),
                s(&w["workspaceId"]),
            ]
        })
        .collect();
    text.push_str(&table(&["STATE", "REPO", "BRANCH", "CHANGES", "IDLE", "SESSION", "WORKSPACE"], &rows));
    let un = v["unmanaged"].as_array().cloned().unwrap_or_default();
    if !un.is_empty() {
        text.push_str(&format!("\n\nUnmanaged worktrees ({}; `lagent task adopt <path>` or archive):\n", un.len()));
        let rows: Vec<Vec<String>> = un.iter().map(|u| vec![s(&u["path"]), s(&u["branch"]), if u["dirty"] == true { "dirty".into() } else { String::new() }, u["idleDays"].as_i64().map(|d| format!("{d}d")).unwrap_or_default()]).collect();
        text.push_str(&table(&["PATH", "BRANCH", "", "IDLE"], &rows));
    }
    let q = v["queue"].as_array().cloned().unwrap_or_default();
    text.push_str(&format!("\n\nQueue ({}):", q.len()));
    if !q.is_empty() {
        text.push('\n');
        text.push_str(&table(&["JOB", "WHAT", "STATE", "RESULT", "QUEUED", "BY"], &q.iter().map(job_line).collect::<Vec<_>>()));
    }
    let leases: Vec<String> = v["leases"].as_array().cloned().unwrap_or_default().iter().filter(|d| !d["lease"].is_null()).map(|d| format!("{} → {} until {}", s(&d["device"]), s(&d["lease"]["holder"]), s(&d["lease"]["expiresAt"]))).collect();
    text.push_str(&format!("\n\nLeases: {}", if leases.is_empty() { "none".into() } else { leases.join("; ") }));
    let deploys = v["deploys"].as_array().cloned().unwrap_or_default();
    text.push_str("\n\nRecent deploys:\n");
    let rows: Vec<Vec<String>> = deploys.iter().take(8).map(|d| vec![s(&d["recorded"]), s(&d["runtime"]), d["commit"].as_str().map(|c| c[..c.len().min(10)].to_string()).unwrap_or_default()]).collect();
    text.push_str(&table(&["WHEN", "RUNTIME", "COMMIT"], &rows));
    Ok(Output { text, json: Some(v) })
}
