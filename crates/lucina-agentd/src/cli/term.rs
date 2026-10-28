//! `lagent terminal …`, `lagent script …` and `lagent hooks` (Paseo
//! `commands/terminal/*`, `commands/script/*`, `commands/hooks.js`). Outputs, flags and
//! error codes are Paseo's.

use std::io::Read;

use clap::Subcommand;
use serde_json::{json, Value};

use super::{s, table, CmdError, Output};
use crate::client::{Client, ClientError};

#[derive(Subcommand, Debug)]
pub enum TerminalCommand {
    /// List terminals
    Ls {
        /// List terminals across all workspaces
        #[arg(long, conflicts_with_all = ["cwd", "workspace"])]
        all: bool,
        /// Workspace ID
        #[arg(long)]
        workspace: Option<String>,
        /// Working directory
        #[arg(long)]
        cwd: Option<String>,
    },
    /// Create a terminal
    Create {
        /// Workspace ID
        #[arg(long)]
        workspace: Option<String>,
        /// Working directory
        #[arg(long)]
        cwd: Option<String>,
        /// Terminal name
        #[arg(long)]
        name: Option<String>,
    },
    /// Kill a terminal
    Kill {
        /// Terminal ID, ID prefix, or name
        id: String,
    },
    /// Capture terminal output
    Capture {
        /// Terminal ID, ID prefix, or name
        id: String,
        /// Capture start line
        #[arg(long, allow_hyphen_values = true)]
        start: Option<String>,
        /// Capture end line
        #[arg(long, allow_hyphen_values = true)]
        end: Option<String>,
        /// Capture from the beginning of scrollback
        #[arg(short = 'S', long)]
        scrollback: bool,
        /// Preserve ANSI escape codes
        #[arg(long)]
        ansi: bool,
    },
    /// Send keys to a terminal
    SendKeys {
        /// Terminal ID, ID prefix, or name
        id: String,
        /// Keys to send
        #[arg(required = true)]
        keys: Vec<String>,
        /// Send raw keys without interpreting special tokens
        #[arg(short = 'l', long)]
        literal: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ScriptCommand {
    /// List configured workspace scripts
    Ls(ScriptTarget),
    /// Start a configured workspace script
    Start {
        name: String,
        #[command(flatten)]
        target: ScriptTarget,
    },
    /// Stop a running workspace script
    Stop {
        name: String,
        #[command(flatten)]
        target: ScriptTarget,
    },
}

#[derive(clap::Args, Debug)]
pub struct ScriptTarget {
    /// Workspace directory (default: current directory)
    #[arg(long)]
    cwd: Option<String>,
    /// Workspace ID (required when a directory has multiple workspaces)
    #[arg(long)]
    workspace: Option<String>,
}

fn failed(code: &'static str, action: &'static str) -> impl Fn(ClientError) -> CmdError {
    move |e| match e {
        ClientError::Api { code: c, message, .. } if c.starts_with("TERMINAL_") || c.starts_with("WORKSPACE") => CmdError::new(&c, message),
        ClientError::Api { message, .. } => CmdError::new(code, format!("Failed to {action}: {message}")),
        other => other.into(),
    }
}

fn cwd_or_current(cwd: &Option<String>) -> String {
    let c = cwd.clone().unwrap_or_else(|| std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_else(|_| ".".into()));
    std::fs::canonicalize(&c).map(|p| p.display().to_string()).unwrap_or(c)
}

/// Paseo `terminalSchema` rows.
fn rows(terms: &[Value]) -> Output {
    let rows: Vec<Vec<String>> = terms
        .iter()
        .map(|t| vec![t["id"].as_str().unwrap_or_default().chars().take(8).collect(), s(&t["name"]), s(&t["cwd"]), s(&t["workspaceId"])])
        .collect();
    Output { text: table(&["ID", "NAME", "CWD", "WORKSPACE"], &rows), json: Some(Value::Array(terms.to_vec())) }
}

/// The JSON Paseo's terminal commands print (`{id, workspaceId, cwd, name}`, plus ours).
fn not_found(id: &str) -> CmdError {
    CmdError::new("TERMINAL_NOT_FOUND", format!("No terminal found matching: {id}")).details("Use `lagent terminal ls --all` to list available terminals.")
}

async fn resolve(client: &Client, id: &str) -> Result<String, CmdError> {
    match client.get(&format!("/terminals/{}", enc(id))).await {
        Ok(t) => Ok(t["id"].as_str().unwrap_or(id).to_string()),
        Err(ClientError::Api { status: 404, .. }) => Err(not_found(id)),
        Err(e) => Err(e.into()),
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

/// The workspace for `--workspace`, or the one whose directory is `cwd` (Paseo `resolveWorkspaceScriptWorkspaceId`).
async fn workspace_for(client: &Client, workspace: &Option<String>, cwd: &Option<String>) -> Result<String, CmdError> {
    if let Some(w) = workspace {
        return Ok(w.clone());
    }
    let cwd = cwd_or_current(cwd);
    let all = client.get("/workspaces?background=true").await?;
    let matches: Vec<String> = all["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["cwd"].as_str().map(|c| std::fs::canonicalize(c).map(|p| p.display().to_string()).unwrap_or(c.to_string())) == Some(cwd.clone()))
        .filter_map(|w| w["workspaceId"].as_str().map(str::to_string))
        .collect();
    match matches.len() {
        1 => Ok(matches[0].clone()),
        0 => Err(CmdError::new("WORKSPACE_NOT_FOUND", format!("No Lucina workspace found for {cwd}")).details("Open the directory as a workspace first, or pass --workspace <workspace-id>.")),
        _ => Err(CmdError::new("WORKSPACE_AMBIGUOUS", format!("Multiple workspaces use {cwd}")).details("Pass --workspace <workspace-id> to select one.")),
    }
}

pub async fn terminal(client: &Client, cmd: &TerminalCommand) -> Result<Output, CmdError> {
    match cmd {
        TerminalCommand::Ls { all, workspace, cwd } => {
            let q = match (all, workspace) {
                (true, _) => String::new(),
                (_, Some(w)) => format!("?workspaceId={}", enc(w)),
                _ => format!("?cwd={}", enc(&cwd_or_current(cwd))),
            };
            let v = client.get(&format!("/terminals{q}")).await.map_err(failed("TERMINAL_LIST_FAILED", "list terminals"))?;
            Ok(rows(v["terminals"].as_array().map(Vec::as_slice).unwrap_or_default()))
        }
        TerminalCommand::Create { workspace, cwd, name } => {
            let dir = cwd_or_current(cwd);
            let workspace_id = match workspace {
                Some(w) => w.clone(),
                None => {
                    // Paseo `workspaces.open(cwd)`: the directory's workspace, made when missing.
                    match workspace_for(client, &None, &Some(dir.clone())).await {
                        Ok(w) => w,
                        Err(e) if e.code == "WORKSPACE_NOT_FOUND" => {
                            let v = client.post("/workspaces", &json!({"isolation": "local", "path": dir})).await.map_err(failed("TERMINAL_CREATE_FAILED", "create terminal"))?;
                            v["workspace"]["workspaceId"].as_str().unwrap_or_default().to_string()
                        }
                        Err(e) => return Err(e),
                    }
                }
            };
            let body = json!({"workspaceId": workspace_id, "cwd": if cwd.is_some() || workspace.is_none() { Some(dir) } else { None }, "name": name});
            let t = client.post("/terminals", &body).await.map_err(failed("TERMINAL_CREATE_FAILED", "create terminal"))?;
            let mut out = rows(std::slice::from_ref(&t));
            out.json = Some(t);
            Ok(out)
        }
        TerminalCommand::Kill { id } => {
            let rid = resolve(client, id).await?;
            let v = client.request("DELETE", &format!("/terminals/{rid}"), None).await.map_err(failed("TERMINAL_KILL_FAILED", "kill terminal"))?;
            let short: String = rid.chars().take(8).collect();
            Ok(Output { text: table(&["ID", "SUCCESS"], &[vec![short, s(&v["success"])]]), json: Some(json!({"terminalId": rid, "success": v["success"]})) })
        }
        TerminalCommand::Capture { id, start, end, scrollback, ansi } => {
            let line = |flag: &str, v: &Option<String>| -> Result<Option<i64>, CmdError> {
                match v {
                    None => Ok(None),
                    Some(x) => x.trim().parse::<i64>().map(Some).map_err(|_| CmdError::new("INVALID_LINE_NUMBER", format!("Invalid {flag} value: {x}")).details("Use an integer line number.")),
                }
            };
            let (start, end) = (if *scrollback { Some(0) } else { line("--start", start)? }, line("--end", end)?);
            let rid = resolve(client, id).await?;
            let mut q = vec![format!("stripAnsi={}", !ansi)];
            if let Some(s) = start {
                q.push(format!("start={s}"));
            }
            if let Some(e) = end {
                q.push(format!("end={e}"));
            }
            let v = client.get(&format!("/terminals/{rid}/capture?{}", q.join("&"))).await.map_err(failed("TERMINAL_CAPTURE_FAILED", "capture terminal output"))?;
            let lines: Vec<String> = v["lines"].as_array().into_iter().flatten().map(|l| l.as_str().unwrap_or_default().to_string()).collect();
            let text = if lines.is_empty() { String::new() } else { lines.join("\n") };
            Ok(Output { text, json: Some(json!({"terminalId": rid, "lines": lines, "totalLines": v["totalLines"]})) })
        }
        TerminalCommand::SendKeys { id, keys, literal } => {
            let rid = resolve(client, id).await?;
            let v = client.post(&format!("/terminals/{rid}/input"), &json!({"keys": keys, "literal": literal})).await.map_err(failed("TERMINAL_SEND_KEYS_FAILED", "send terminal keys"))?;
            Ok(Output { text: String::new(), json: Some(json!({"terminalId": rid, "keysSent": v["keysSent"]})) })
        }
    }
}

/// Paseo `workspaceScriptSchema` rows.
fn script_rows(scripts: &[Value]) -> Output {
    let dash = |v: &Value| if v.is_null() { "-".to_string() } else { s(v) };
    let rows: Vec<Vec<String>> = scripts
        .iter()
        .map(|x| vec![s(&x["scriptName"]), s(&x["type"]), s(&x["lifecycle"]), dash(&x["health"]), dash(&x["port"]), dash(&x["proxyUrl"]), dash(&x["terminalId"])])
        .collect();
    Output { text: table(&["NAME", "TYPE", "LIFECYCLE", "HEALTH", "PORT", "PROXY URL", "TERMINAL"], &rows), json: Some(Value::Array(scripts.to_vec())) }
}

pub async fn script(client: &Client, cmd: &ScriptCommand) -> Result<Output, CmdError> {
    match cmd {
        ScriptCommand::Ls(t) => {
            let ws = workspace_for(client, &t.workspace, &t.cwd).await?;
            let v = client.get(&format!("/workspaces/{ws}/scripts")).await.map_err(failed("WORKSPACE_SCRIPT_LIST_FAILED", "list workspace scripts"))?;
            Ok(script_rows(v["scripts"].as_array().map(Vec::as_slice).unwrap_or_default()))
        }
        ScriptCommand::Start { name, target } | ScriptCommand::Stop { name, target } => {
            let (verb, code, action) = match cmd {
                ScriptCommand::Start { .. } => ("start", "WORKSPACE_SCRIPT_START_FAILED", "start workspace script"),
                _ => ("stop", "WORKSPACE_SCRIPT_STOP_FAILED", "stop workspace script"),
            };
            let ws = workspace_for(client, &target.workspace, &target.cwd).await?;
            let v = client.post(&format!("/workspaces/{ws}/scripts/{}/{verb}", enc(name)), &json!({})).await.map_err(|e| match e {
                ClientError::Api { message, .. } => CmdError::new(code, message),
                other => failed(code, action)(other),
            })?;
            let mut out = script_rows(std::slice::from_ref(&v));
            out.json = Some(v);
            Ok(out)
        }
    }
}

/// `lagent hooks <cli> <event>`: an agent CLI in a terminal reports what it is doing.
/// A no-op outside agentd's terminals; never fails the CLI that calls it.
pub async fn hooks(client: &Client, cli: &str, event: &str) -> Output {
    let quiet = Output { text: String::new(), json: None };
    let (Ok(id), Ok(token)) = (std::env::var("PASEO_TERMINAL_ID"), std::env::var("PASEO_ACTIVITY_TOKEN")) else { return quiet };
    let stdin = read_stdin();
    let Some(state) = hook_state(cli, event, &stdin) else { return quiet };
    let _ = client.post("/terminal-activity", &json!({"terminalId": id, "token": token, "state": state})).await;
    quiet
}

/// Paseo's hook table (`terminal/agent-hooks`).
pub fn hook_state(cli: &str, event: &str, stdin: &str) -> Option<&'static str> {
    match (cli, event) {
        ("claude", "UserPromptSubmit") => Some("running"),
        ("claude", "Stop" | "StopFailure" | "SessionEnd") => Some("idle"),
        ("claude", "Notification") => {
            let v: Value = serde_json::from_str(stdin).unwrap_or(Value::Null);
            (v["notification_type"] == "idle_prompt").then_some("needs-input")
        }
        ("codex", "UserPromptSubmit" | "PreToolUse" | "PostToolUse") => Some("running"),
        ("codex", "PermissionRequest") => Some("needs-input"),
        ("codex", "Stop") => Some("idle"),
        ("opencode", "session.status.busy" | "session.status.retry" | "permission.replied") => Some("running"),
        ("opencode", "session.status.idle") => Some("idle"),
        ("opencode", "permission.asked") => Some("needs-input"),
        _ => None,
    }
}

/// stdin, with a 100 ms limit when it isn't a terminal (Paseo's hooks do the same).
fn read_stdin() -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::stdin().take(1 << 20).read_to_string(&mut s);
        let _ = tx.send(s);
    });
    rx.recv_timeout(std::time::Duration::from_millis(100)).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_table() {
        assert_eq!(hook_state("claude", "UserPromptSubmit", ""), Some("running"));
        assert_eq!(hook_state("claude", "Notification", r#"{"notification_type":"idle_prompt"}"#), Some("needs-input"));
        assert_eq!(hook_state("claude", "Notification", "{}"), None);
        assert_eq!(hook_state("codex", "PermissionRequest", ""), Some("needs-input"));
        assert_eq!(hook_state("opencode", "session.status.idle", ""), Some("idle"));
        assert_eq!(hook_state("x", "Stop", ""), None);
        assert_eq!(enc("a b/c"), "a%20b%2Fc");
    }
}
