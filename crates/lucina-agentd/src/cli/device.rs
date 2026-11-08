//! `lagent device install|grant|grant-check|grant-done|history` (Lucina only): installs
//! on a leased device (the owner's iPhone). `ls`, `lease` and `release` are the
//! pipeline's ([`super::pipe`]). The holder is `--holder`, else the calling session.

use clap::Args;
use serde_json::{json, Value};

use super::pipe::session;
use super::{s, table, CmdError, Output};
use crate::client::{Client, ClientError};

#[derive(Args, Debug)]
pub struct InstallArgs {
    pub device: String,
    /// The commit (default, and the only choice for a main install: origin/main's tip)
    #[arg(long)]
    pub commit: Option<String>,
    /// A test build of a branch: needs your lease and --approval
    #[arg(long)]
    pub test: bool,
    /// The owner's go from Lucina (`lucina-rs decide --impact high`), naming the sha8, your session or workspace
    #[arg(long)]
    pub approval: Option<String>,
    /// Check everything (the executor too), install nothing
    #[arg(long)]
    pub dry_run: bool,
    /// With --dry-run: build too
    #[arg(long)]
    pub build: bool,
    /// For the deploy ledger
    #[arg(long)]
    pub note: Option<String>,
    #[arg(long)]
    pub holder: Option<String>,
    #[arg(long)]
    pub workspace: Option<String>,
}

#[derive(Args, Debug)]
pub struct GrantArgs {
    pub device: String,
    /// The commit the test run builds
    #[arg(long)]
    pub commit: String,
    /// Needed unless the commit is origin/main's tip
    #[arg(long)]
    pub approval: Option<String>,
    #[arg(long)]
    pub holder: Option<String>,
    #[arg(long)]
    pub workspace: Option<String>,
}

#[derive(Args, Debug)]
pub struct GrantDoneArgs {
    pub device: String,
    pub grant: String,
    #[arg(long)]
    pub exit: i32,
    #[arg(long, default_value = "")]
    pub command: String,
    #[arg(long)]
    pub dirty: bool,
}

fn api(e: ClientError) -> CmdError {
    match e {
        ClientError::Api { code, message, .. } => CmdError::new(&code, message),
        other => other.into(),
    }
}

fn sha8(v: &Value) -> String {
    s(v).chars().take(8).collect()
}

pub async fn install(client: &Client, a: &InstallArgs) -> Result<Output, CmdError> {
    let body = json!({
        "holder": a.holder.clone().unwrap_or_else(session), "workspaceId": a.workspace,
        "commit": a.commit, "test": a.test, "approval": a.approval, "dryRun": a.dry_run, "build": a.build, "note": a.note,
    });
    let v = client.post(&format!("/devices/{}/install", a.device), &body).await.map_err(api)?;
    let text = if v["dryRun"] == true {
        let built = if v["executor"]["built"] == false { " (not built: add --build)" } else { "" };
        format!("dry run OK: {} {} would go on {}{built}\nlog {}", s(&v["kind"]), sha8(&v["commit"]), a.device, s(&v["log"]))
    } else {
        let appex: Vec<String> = v["appex"].as_array().into_iter().flatten().map(s).collect();
        let rec = v["deployGuard"].as_str().map(|r| format!("\n{r}")).unwrap_or_default();
        format!("installed {} {} on {}; extensions: {}{rec}\nlog {}", s(&v["kind"]), sha8(&v["commit"]), a.device, appex.join(", "), s(&v["log"]))
    };
    Ok(Output { text, json: Some(v) })
}

pub async fn grant(client: &Client, a: &GrantArgs) -> Result<Output, CmdError> {
    let body = json!({"holder": a.holder.clone().unwrap_or_else(session), "workspaceId": a.workspace, "commit": a.commit, "approval": a.approval});
    let v = client.post(&format!("/devices/{}/grants", a.device), &body).await.map_err(api)?;
    let text = format!(
        "grant {} for a test run of {} (one use, 1 h): on the Mac, from that checkout: ios-install exec --grant {} -- xcodebuild test …",
        s(&v["grant"]["id"]),
        sha8(&v["grant"]["commit"]),
        s(&v["grant"]["id"])
    );
    Ok(Output { text, json: Some(v) })
}

pub async fn grant_check(client: &Client, device: &str, grant: &str) -> Result<Output, CmdError> {
    let v = client.get(&format!("/devices/{device}/grants/{grant}")).await.map_err(api)?;
    let text = format!("grant {grant}: ok ({} {})", s(&v["grant"]["kind"]), s(&v["grant"]["commit"]));
    Ok(Output { text, json: Some(v) })
}

pub async fn grant_done(client: &Client, a: &GrantDoneArgs) -> Result<Output, CmdError> {
    let body = json!({"exit": a.exit, "command": a.command, "dirty": a.dirty});
    let v = client.post(&format!("/devices/{}/grants/{}/done", a.device, a.grant), &body).await.map_err(api)?;
    Ok(Output { text: format!("recorded the test run of grant {}", a.grant), json: Some(v) })
}

pub async fn history(client: &Client, device: &str, limit: usize) -> Result<Output, CmdError> {
    let v = client.get(&format!("/devices/{device}/history?limit={limit}")).await.map_err(api)?;
    let rows: Vec<Vec<String>> = v["history"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|h| vec![s(&h["at"]), s(&h["event"]), h["kind"].as_str().unwrap_or("").into(), sha8(&h["commit"]), s(&h["holder"])])
        .collect();
    Ok(Output { text: table(&["AT", "EVENT", "KIND", "COMMIT", "HOLDER"], &rows), json: Some(v) })
}

/// For `lagent device ls`: what agentd last installed.
pub fn installed_text(i: &Value) -> String {
    if i.is_null() {
        return "-".into();
    }
    format!("{} {} ({})", s(&i["kind"]), sha8(&i["commit"]), s(&i["at"]))
}
