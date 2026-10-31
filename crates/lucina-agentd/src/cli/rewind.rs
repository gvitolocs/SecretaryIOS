//! `lagent rewind` and `lagent fork` (parity row A16). Paseo's CLI has neither: they are
//! app operations there (`agent.rewind.request`, `agent.fork_context.request`), so the
//! flags are Lucina's. Both are thin over the daemon's routes (`api/rewind.rs`).

use clap::Args;
use lucina_agents::rewind::RewindMode;
use serde_json::{json, Value};

use super::{kv, single, CmdError, Output};
use crate::client::Client;

#[derive(Args, Debug)]
pub struct RewindArgs {
    /// Agent ID (or prefix)
    pub id: String,
    /// The user message to go back to: its id (or a unique prefix), its number in `lagent
    /// logs` order (1 is the first prompt), or `last`. The message and everything after it go.
    #[arg(long)]
    pub to: String,
    /// What goes back: conversation, files or both
    #[arg(long, default_value = "conversation")]
    pub mode: String,
    /// Allow rewinding files in a main checkout or stray worktree (the owner only)
    #[arg(long)]
    pub allow_main_checkout: bool,
}

#[derive(Args, Debug)]
pub struct ForkArgs {
    /// Agent ID (or prefix) to take the chat history from
    pub id: String,
    /// The new agent's first prompt; without one it waits for its first message
    pub prompt: Option<String>,
    /// Fork from here: an assistant message id, a timeline sequence number, or `<epoch>:<seq>`
    /// (default: the whole history)
    #[arg(long)]
    pub at: Option<String>,
    #[arg(long)]
    pub title: Option<String>,
    /// Agent provider, or provider/model (default: the source agent's)
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub thinking: Option<String>,
    #[arg(long)]
    pub mode: Option<String>,
    /// Directory for the new agent (default: the source agent's)
    #[arg(long)]
    pub cwd: Option<String>,
    /// Run in an existing workspace
    #[arg(long)]
    pub workspace: Option<String>,
    #[arg(long = "label", value_name = "KEY=VALUE")]
    pub label: Vec<String>,
    #[arg(long)]
    pub idempotency_key: Option<String>,
    /// Allow the new agent to edit a main checkout (the owner only)
    #[arg(long)]
    pub allow_main_checkout: bool,
}

fn mode_error(mode: &str) -> CmdError {
    CmdError::new("INVALID_MODE", format!("Invalid rewind mode: {mode}")).details("Use conversation, files or both")
}

/// The user message `--to` names, from the agent's prompt index; anything that matches no
/// prompt goes to the daemon as is (a `clientMessageId`), which says if it knows it.
async fn resolve_target(client: &Client, id: &str, to: &str) -> Result<String, CmdError> {
    let v = client.get(&format!("/agents/{id}/prompts")).await?;
    let ids: Vec<String> = v["prompts"].as_array().cloned().unwrap_or_default().iter().filter_map(|p| p["messageId"].as_str().map(str::to_string)).collect();
    if ids.iter().any(|m| m == to) {
        return Ok(to.to_string());
    }
    if to == "last" {
        return ids.last().cloned().ok_or_else(|| CmdError::new("REWIND_TARGET_NOT_FOUND", "The agent has no user message to go back to"));
    }
    if let Ok(n) = to.parse::<usize>() {
        return match n.checked_sub(1).and_then(|i| ids.get(i)) {
            Some(m) => Ok(m.clone()),
            None => Err(CmdError::new("REWIND_TARGET_NOT_FOUND", format!("The agent has {} user messages; there is no number {n}", ids.len()))),
        };
    }
    let by_prefix: Vec<&String> = ids.iter().filter(|m| m.starts_with(to)).collect();
    match by_prefix.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Ok(to.to_string()),
        _ => Err(CmdError::new("AMBIGUOUS_MESSAGE", format!("Message id prefix {to} matches more than one message"))),
    }
}

pub async fn rewind(client: &Client, a: &RewindArgs) -> Result<Output, CmdError> {
    let mode = RewindMode::parse(&a.mode).ok_or_else(|| mode_error(&a.mode))?;
    let id = super::resolve(client, &a.id).await?["id"].as_str().unwrap_or_default().to_string();
    let message_id = resolve_target(client, &id, &a.to).await?;
    let mut body = json!({"messageId": message_id, "mode": mode.as_str()});
    if a.allow_main_checkout {
        body["allowMainCheckout"] = json!(true);
    }
    let v = client.post(&format!("/agents/{id}/rewind"), &body).await?;
    Ok(single(v, &["agentId", "messageId", "mode", "removedRows"]))
}

/// `--at`: a cursor (`N`, `<epoch>:<seq>`) or an assistant message id.
async fn boundary(client: &Client, id: &str, at: &str) -> Result<Value, CmdError> {
    let cursor = |epoch: String, seq: u64| json!({"boundaryCursor": {"epoch": epoch, "seq": seq}});
    if let Ok(seq) = at.parse::<u64>() {
        // An empty window still says which epoch the timeline is in.
        let t = client.get(&format!("/agents/{id}/timeline?afterSeq={}", u64::MAX / 2)).await?;
        return Ok(cursor(t["epoch"].as_str().unwrap_or_default().to_string(), seq));
    }
    if let Some((epoch, seq)) = at.rsplit_once(':').and_then(|(e, s)| s.parse::<u64>().ok().map(|s| (e, s))) {
        return Ok(cursor(epoch.to_string(), seq));
    }
    Ok(json!({"boundaryMessageId": at}))
}

pub async fn fork(client: &Client, a: &ForkArgs) -> Result<Output, CmdError> {
    let id = super::resolve(client, &a.id).await?["id"].as_str().unwrap_or_default().to_string();
    let mut body = match &a.at {
        Some(at) => boundary(client, &id, at).await?,
        None => json!({}),
    };
    let mut set = |k: &str, v: &Option<String>| {
        if let Some(v) = v {
            body[k] = json!(v);
        }
    };
    set("prompt", &a.prompt);
    set("title", &a.title);
    set("provider", &a.provider);
    set("model", &a.model);
    set("thinkingOptionId", &a.thinking);
    set("modeId", &a.mode);
    set("cwd", &a.cwd);
    set("workspaceId", &a.workspace);
    set("idempotencyKey", &a.idempotency_key);
    if !a.label.is_empty() {
        body["labels"] = Value::Object(kv(&a.label, "INVALID_LABEL")?);
    }
    if a.allow_main_checkout {
        body["allowMainCheckout"] = json!(true);
    }
    let mut v = client.post(&format!("/agents/{id}/fork"), &body).await?;
    v["agentId"] = v["agent"]["id"].clone();
    let shown = json!({
        "agentId": v["agent"]["id"], "forkedFrom": v["forkedFrom"], "provider": v["agent"]["provider"],
        "status": v["agent"]["status"], "itemCount": v["itemCount"],
    });
    Ok(Output { text: single(shown, &["agentId", "forkedFrom", "provider", "status", "itemCount"]).text, json: Some(v) })
}
