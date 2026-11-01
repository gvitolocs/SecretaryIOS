//! `lagent session …` (Lucina only): Claude sessions as agents. The hook
//! (`pipeline-guard session-hook`) calls `beat` and `ack`; `register` and `ls` are for people.

use std::io::Read;

use clap::Subcommand;
use serde_json::{json, Value};

use super::pipe::session;
use super::{s, table, CmdError, Output};
use crate::client::{Client, ClientError};

#[derive(Subcommand, Debug)]
pub enum SessionCommand {
    /// The Claude sessions agentd knows: their status, branches and when they were last heard from
    Ls,
    /// Show this session as an agent now (the hook does this by itself)
    Register {
        /// The session (default: $LAGENT_SESSION, $CLAUDE_CODE_HOST_SESSION_ID, $CLAUDE_CODE_SESSION_ID)
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// Where it runs: mac or nezopt
        #[arg(long)]
        host: Option<String>,
        /// A workspace id it works in (repeatable)
        #[arg(long)]
        workspace: Vec<String>,
    },
    /// One hook report: a SessionBeat as JSON on stdin; prints what waits for the session
    Beat,
    /// The hook delivered these commands
    Ack {
        session: String,
        command_ids: Vec<String>,
    },
}

pub async fn session_cmd(client: &Client, cmd: &SessionCommand) -> Result<Output, CmdError> {
    let failed = |e: ClientError| CmdError::new("SESSION_FAILED", e.to_string());
    match cmd {
        SessionCommand::Ls => {
            let v = client.get("/sessions").await.map_err(failed)?;
            let sessions = v["sessions"].as_array().cloned().unwrap_or_default();
            let now = chrono::Utc::now().timestamp_millis();
            let rows: Vec<Vec<String>> = sessions
                .iter()
                .map(|x| {
                    let branches: Vec<String> = x["branches"].as_array().map(|b| b.iter().map(s).collect()).unwrap_or_default();
                    let seen = (now - x["lastSeenMs"].as_i64().unwrap_or(now)).max(0) / 1000;
                    vec![s(&x["sessionId"]), s(&x["title"]), s(&x["host"]), s(&x["status"]), branches.join(", "), format!("{seen}s ago")]
                })
                .collect();
            Ok(Output { text: table(&["SESSION", "TITLE", "HOST", "STATUS", "BRANCH", "HEARD"], &rows), json: Some(Value::Array(sessions)) })
        }
        SessionCommand::Register { session: id, title, host, workspace } => {
            let id = id.clone().unwrap_or_else(session);
            let body = json!({"sessionId": id, "event": "link", "title": title, "host": host, "workspaces": workspace, "register": true, "noCommands": true});
            let v = client.post("/sessions/beat", &body).await.map_err(failed)?;
            Ok(Output { text: format!("Session {id} shows as agent {}.", s(&v["agentId"])), json: Some(v) })
        }
        SessionCommand::Beat => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text).map_err(|e| CmdError::new("SESSION_FAILED", e.to_string()))?;
            let body: Value = serde_json::from_str(&text).map_err(|e| CmdError::new("INVALID_BEAT", e.to_string()))?;
            let v = client.post("/sessions/beat", &body).await.map_err(failed)?;
            Ok(Output { text: v.to_string(), json: Some(v) })
        }
        SessionCommand::Ack { session: id, command_ids } => {
            let v = client.post(&format!("/sessions/{id}/ack"), &json!({"commandIds": command_ids})).await.map_err(failed)?;
            Ok(Output { text: v.to_string(), json: Some(v) })
        }
    }
}
