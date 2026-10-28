//! `agents_*`, `agent_*`, `workspace*`, `files_list`, `file_read`, `terminal_*`,
//! `script*`, `schedule*`, `permissions_list`: Lucina Agents, the owner's coding
//! agents on nezopt (lucina-agentd), through secretary-server `/v1/agents/*`
//! (docs/design/agents-api.md).
//!
//! Scope `agents`, separate from `write` (docs/design/agents.md, "Security requirements"):
//!
//! - Calls carry the agents service token (`SECRETARY_MCP_AGENTS_TOKEN_FILE`), never
//!   the device token, plus `X-Lucina-Mcp-Caller: <tokenId>:<name>`. The server turns
//!   that into `X-Lucina-Origin: mcp:…`, for which agentd refuses permission answers,
//!   push, PRs, workspace archive and main checkouts. No tool here offers those.
//! - `agent_run` never forwards `verify` (a command agentd runs without asking), `env`,
//!   `toolPolicy` (preapproved tools) or `allowMainCheckout`, never picks a permission
//!   mode the provider marks dangerous (Claude `bypassPermissions`, Codex
//!   `full-access`), and refuses providers that have no permission prompts at all. So
//!   an MCP-started run can't get past the owner's permission answers.
//! - Delegation depth is at most [`MAX_DEPTH`], as in lucina-delegate: a caller at that
//!   depth (a token handed to an agent run) cannot start agents or add schedules. Runs
//!   are labelled with their depth, token and caller. On top of that each token has a
//!   budget: running agents at once and runs per hour (`SECRETARY_MCP_AGENTS_*`).
//! - Schedules: a caller changes only schedules it created (label [`LABEL_TOKEN`]);
//!   anything without that label is refused (fail closed).
//! - Every call, refusals included, is appended to `$SECRETARY_MCP_DATA_DIR/agents-audit.jsonl`
//!   ([`audit`], called from `crate::mcp`), with the prompt's length and hash, not its text.
//!
//! - Terminals and scripts (phase 5): agentd shows a token only the terminals of agents
//!   it started and of the scripts it ran, and the scripts of workspaces it made or runs
//!   agents in. It can read and type into those terminals, never open or kill one, and
//!   never open a service (the proxy is the owner's). Against an agentd without these
//!   routes the tools say "not available yet".
//!
//! Schedules, full-text search and schedule counts in `agents_usage` use agentd's phase-6 routes;
//! against an older daemon schedules say "not available yet" and search falls back to names only.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lucina_agents::curator::{curate_activity, CurateOptions};
use lucina_agents::projection::project_rows;
use lucina_agents::TimelineRow;
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Method;
use rmcp::model::JsonObject;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::backend::{seg, BackendError};
use crate::scope::{Caller, CallerKind};
use crate::store::{rfc3339, sha256_hex, unix_now};
use crate::tools::{ToolError, Tools};

/// Deepest delegation allowed: the owner's clients are 0, a run they start is 1,
/// and a caller at 1 starts nothing (lucina-delegate's `max_depth`).
pub const MAX_DEPTH: u8 = 1;
/// Longest a single tool call waits on an agent.
pub const MAX_WAIT_SECS: u64 = 50;
/// Text returned to the model from one call.
const MAX_TEXT: usize = 12_000;
const MAX_PROMPT_CHARS: usize = 50_000;
const CALL_TIMEOUT: Duration = Duration::from_secs(30);
/// One long-poll of `/wait`; progress is reported between polls.
const WAIT_SLICE_SECS: u64 = 10;
const AUDIT_FILE: &str = "agents-audit.jsonl";

/// Labels `agent_run` and `schedule_add` set (callers can't set `lucina.*` themselves).
pub const LABEL_ORIGIN: &str = "lucina.origin";
pub const LABEL_CALLER: &str = "lucina.mcp-caller";
pub const LABEL_TOKEN: &str = "lucina.mcp-token";
pub const LABEL_DEPTH: &str = "lucina.delegation-depth";

/// Providers that run without any permission prompt (ZCode's headless CLI is
/// always yolo, Pi has no permission system), so an MCP run can't use them.
const UNSUPERVISED: [&str; 2] = ["zcode", "pi"];

const PHASE5: &str = "Terminals and workspace scripts arrive with Lucina Agents phase 5; this tool is not available yet.";
const PHASE6_SCHEDULES: &str = "Agent schedules arrive with Lucina Agents phase 6; this tool is not available yet.";

// -- arguments ------------------------------------------------------------------

/// Arguments of `agents_list`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListArgs {
    /// Only agents in this state: running, idle, error, closed, or a list bucket (attention, active, done).
    #[serde(default)]
    pub status: Option<String>,
    /// List archived agents instead of current ones.
    #[serde(default)]
    pub archived: Option<bool>,
    /// Only the agents this connection started.
    #[serde(default)]
    pub mine: Option<bool>,
    /// At most this many, newest first (default 30, max 200).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Arguments of `agent_run`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunArgs {
    /// The task for the coding agent, in words (up to 50000 characters).
    pub prompt: String,
    /// Provider id: claude, codex, opencode, cursor, copilot (agents_usage lists them). zcode and pi are not available from the MCP.
    pub provider: String,
    /// Model id for that provider, e.g. claude-opus-5-5 or gpt-6.1-sol (default: the provider's).
    #[serde(default)]
    pub model: Option<String>,
    /// Run in this workspace (a worktree from workspace_create or workspaces_list). Preferred.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Or an absolute directory on nezopt. Main checkouts are refused: use a worktree.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Short title shown in the apps (default: generated from the prompt).
    #[serde(default)]
    pub title: Option<String>,
    /// Permission mode, e.g. plan, default, acceptEdits (Claude) or read-only, auto (Codex). Modes that skip permission prompts are refused.
    #[serde(default)]
    pub mode: Option<String>,
    /// Extra labels (up to 8; keys a-z, 0-9, '.', '_', '-', not starting with lucina. or paseo.).
    #[serde(default)]
    pub labels: Option<BTreeMap<String, String>>,
    /// Seconds to wait for the first answer before returning (0-50, default 30).
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

/// Arguments of `agent_send`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendArgs {
    /// The agent's id (or a unique prefix of it, as agents_list shows).
    pub agent_id: String,
    /// The follow-up message.
    pub prompt: String,
    /// true: add it to the running turn (steer). false (default): interrupt the running turn and start a new one.
    #[serde(default)]
    pub steer: Option<bool>,
    /// Seconds to wait for its answer (0-50, default 0).
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

/// Arguments of `agent_inspect`, `agent_stop`, `agent_archive`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentArgs {
    /// The agent's id (or a unique prefix of it).
    pub agent_id: String,
}

/// Arguments of `agent_logs`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogsArgs {
    /// The agent's id (or a unique prefix of it).
    pub agent_id: String,
    /// Only the last N entries (messages, tool calls); default 40, 0 = all (the text is still cut at about 12000 characters, keeping the end).
    #[serde(default)]
    pub tail: Option<u32>,
}

/// Arguments of `agent_wait`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WaitArgs {
    /// The agent's id (or a unique prefix of it).
    pub agent_id: String,
    /// Seconds to wait (1-50, default 45).
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

/// Arguments of `agents_search`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchArgs {
    /// Words to find in titles, ids, directories, providers, models and labels.
    pub q: String,
    /// Also search archived agents (default true).
    #[serde(default)]
    pub archived: Option<bool>,
    /// At most this many (default 20).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Arguments of `workspaces_list`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspacesArgs {
    /// Only workspaces whose project, title, branch or directory contains this.
    #[serde(default)]
    pub q: Option<String>,
}

/// Arguments of `workspace_create`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCreateArgs {
    /// The repository's main checkout on nezopt, absolute, e.g. /home/nez/Projects/Lucina/SecretaryIOS.
    pub repo_path: String,
    /// Branch and worktree name (a-z, 0-9, '-'); default: generated from the title or prompt.
    #[serde(default)]
    pub branch: Option<String>,
    /// Branch or ref to start from (default: the repository's base branch).
    #[serde(default)]
    pub base: Option<String>,
    /// Title shown in the apps.
    #[serde(default)]
    pub title: Option<String>,
    /// The task you will give the first agent: the title and branch are named after it.
    #[serde(default)]
    pub prompt: Option<String>,
}

/// Arguments of `workspace_diff`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiffArgs {
    pub workspace_id: String,
    /// uncommitted (working tree), base (commits since the base branch) or all (default).
    #[serde(default)]
    pub mode: Option<DiffMode>,
    /// Only this file (path relative to the workspace).
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DiffMode {
    Uncommitted,
    Base,
    All,
}

impl DiffMode {
    fn as_str(self) -> &'static str {
        match self {
            DiffMode::Uncommitted => "uncommitted",
            DiffMode::Base => "base",
            DiffMode::All => "all",
        }
    }
}

/// Arguments of `files_list`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FilesArgs {
    pub workspace_id: String,
    /// Directory relative to the workspace (default: its root).
    #[serde(default)]
    pub path: Option<String>,
}

/// Arguments of `file_read`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FileReadArgs {
    pub workspace_id: String,
    /// File path relative to the workspace.
    pub path: String,
    /// Start at this character, for files longer than one page (about 12000 characters).
    #[serde(default)]
    pub offset: Option<usize>,
}

/// Arguments of `permissions_list`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PermissionsArgs {
    /// Also show answered and expired requests.
    #[serde(default)]
    pub all: Option<bool>,
}

/// Arguments of `terminal_list` and `scripts_list`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceArgs {
    #[serde(default)]
    pub workspace_id: Option<String>,
}

/// Arguments of `terminal_read`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalArgs {
    pub terminal_id: String,
    /// How many of the last lines to show (default 200, at most 2000).
    #[serde(default)]
    pub lines: Option<u32>,
}

/// Arguments of `terminal_send`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TerminalSendArgs {
    /// A terminal an agent owns.
    pub terminal_id: String,
    /// Text to type (e.g. "y\n"), or one key token: Enter, Tab, Escape, Space, BSpace,
    /// Up, Down, Left, Right, Home, End, PageUp, PageDown, F1-F12, with C-/M-/S-
    /// prefixes (C-c, S-Tab).
    pub keys: String,
    /// Type `keys` exactly as given, even when it looks like a key token.
    #[serde(default)]
    pub literal: Option<bool>,
}

/// Arguments of `script_start` and `script_stop`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptArgs {
    pub workspace_id: String,
    /// A script name from the workspace's paseo.json (scripts_list).
    pub script: String,
}

/// Arguments of `schedule_add`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleAddArgs {
    /// Cron expression in the owner's time zone, e.g. "0 7 * * 1-5".
    pub cron: String,
    /// Provider id, as for agent_run.
    pub provider: String,
    /// What the agent does each time.
    pub prompt: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

/// Arguments of `schedule_remove`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleArgs {
    pub schedule_id: String,
}

/// No arguments.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

// -- run budget ----------------------------------------------------------------

/// `agent_run` times per token over the last hour.
#[derive(Default)]
pub struct RunLog(Mutex<HashMap<String, VecDeque<Instant>>>);

impl RunLog {
    /// Takes one run from `token`'s hourly budget; false when it is spent.
    pub fn reserve(&self, token: &str, per_hour: usize) -> bool {
        let mut m = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let q = m.entry(token.to_owned()).or_default();
        while q.front().is_some_and(|t| t.elapsed() >= Duration::from_secs(3600)) {
            q.pop_front();
        }
        if q.len() >= per_hour {
            return false;
        }
        q.push_back(Instant::now());
        true
    }

    /// Gives back the run [`Self::reserve`] took when it never started.
    pub fn release(&self, token: &str) {
        let mut m = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(q) = m.get_mut(token) {
            q.pop_back();
        }
    }
}

// -- outcome and audit ------------------------------------------------------------

/// What one tool call answers, and the agent or workspace it acted on (for the audit).
pub struct Outcome {
    pub text: String,
    pub target: Option<String>,
}

impl Outcome {
    fn text(text: String) -> Self {
        Self { text, target: None }
    }
}

/// Appends one line to `agents-audit.jsonl` (0600) and logs it on
/// `secretary_mcp::audit`. Best effort: a failed write never fails the call.
pub fn audit(dir: &Path, tool: &str, c: &Caller, args: &JsonObject, result: Result<Option<&str>, &str>) {
    let prompt = args.get("prompt").and_then(Value::as_str);
    let target = match result {
        Ok(Some(t)) => Some(t.to_owned()),
        _ => ["agent_id", "workspace_id", "schedule_id", "terminal_id"]
            .iter()
            .find_map(|k| args.get(*k).and_then(Value::as_str))
            .map(|s| s.chars().take(80).collect()),
    };
    let line = json!({
        "ts": rfc3339(unix_now()),
        "tool": tool,
        "caller": c.name,
        "auth": if c.kind == CallerKind::OAuth { "oauth" } else { "token" },
        "tokenId": c.id,
        "depth": c.depth,
        "ok": result.is_ok(),
        "target": target,
        "promptChars": prompt.map(|p| p.chars().count()),
        "promptSha256": prompt.map(|p| sha256_hex(p)[..16].to_owned()),
        "error": result.err().map(|e| e.chars().take(300).collect::<String>()),
    });
    tracing::info!(target: "secretary_mcp::audit", event = "agents_call", tool, caller = %c.name, id = %c.id, ok = result.is_ok());
    let write = || -> std::io::Result<()> {
        crate::store::ensure_dir(dir)?;
        let mut f = OpenOptions::new().create(true).append(true).mode(0o600).open(dir.join(AUDIT_FILE))?;
        let mut s = line.to_string();
        s.push('\n');
        f.write_all(s.as_bytes())
    };
    if let Err(e) = write() {
        tracing::warn!(target: "secretary_mcp::audit", error = %e, "agents audit line not written");
    }
}

// -- validation -----------------------------------------------------------------

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError(msg.into())
}

fn parse<T: serde::de::DeserializeOwned>(args: &JsonObject) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(args.clone())).map_err(|e| bad(format!("Invalid arguments: {e}")))
}

fn word(s: &str, max: usize, extra: &[u8]) -> bool {
    (1..=max).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
}

fn agent_ref(s: &str) -> Result<&str, ToolError> {
    let s = s.trim();
    if word(s, 64, b"-") {
        Ok(s)
    } else {
        Err(bad("`agent_id` is an agent id (or a unique prefix) from agents_list"))
    }
}

fn terminal_ref(s: &str) -> Result<&str, ToolError> {
    let s = s.trim();
    if word(s, 64, b"-_") {
        Ok(s)
    } else {
        Err(bad("`terminal_id` is a terminal id (term_…) from terminal_list"))
    }
}

fn script_ref(s: &str) -> Result<&str, ToolError> {
    let s = s.trim();
    if word(s, 64, b"-_.:") {
        Ok(s)
    } else {
        Err(bad("`script` is a script name from scripts_list"))
    }
}

/// An older agentd without the phase 5 routes.
fn no_terminals(e: BackendError, found: &[&str]) -> ToolError {
    if (e.status == 404 || e.status == 405) && !found.contains(&e.code.as_str()) {
        bad(PHASE5)
    } else {
        refusal(e)
    }
}

fn terminal_line(t: &Value) -> String {
    let mut out = format!("- {} · {} · {}", str_of(t, "id"), str_of(t, "name"), str_of(t, "status"));
    if let Some(code) = t["exitCode"].as_i64() {
        out.push_str(&format!(" (exit {code})"));
    }
    out.push_str(&format!(" · {}", str_of(t, "cwd")));
    if let Some(s) = t["scriptName"].as_str() {
        out.push_str(&format!(" · script {s}"));
    } else if let Some(a) = t["agentId"].as_str() {
        out.push_str(&format!(" · agent {}", a.chars().take(8).collect::<String>()));
    }
    if let Some(a) = t["activity"].as_str() {
        out.push_str(&format!(" · {a}"));
    }
    let ports: Vec<String> = t["ports"].as_array().into_iter().flatten().filter_map(|p| p["port"].as_u64()).map(|p| p.to_string()).collect();
    if !ports.is_empty() {
        out.push_str(&format!(" · listening on {}", ports.join(", ")));
    }
    out
}

fn script_line(x: &Value) -> String {
    let mut out = format!("- {} · {} · {}", str_of(x, "scriptName"), str_of(x, "type"), str_of(x, "lifecycle"));
    if let Some(p) = x["port"].as_u64() {
        out.push_str(&format!(" · port {p}"));
    }
    if let Some(h) = x["health"].as_str() {
        out.push_str(&format!(" · {h}"));
    }
    if let Some(c) = x["exitCode"].as_i64() {
        out.push_str(&format!(" · exit {c}"));
    }
    if let Some(t) = x["terminalId"].as_str() {
        out.push_str(&format!(" · terminal {t}"));
    }
    if let Some(c) = x["command"].as_str() {
        out.push_str(&format!(" · `{}`", c.chars().take(120).collect::<String>()));
    }
    out
}

fn workspace_ref(s: &str) -> Result<&str, ToolError> {
    let s = s.trim();
    if word(s, 64, b"-_") {
        Ok(s)
    } else {
        Err(bad("`workspace_id` is a workspace id (wks_…) from workspaces_list or workspace_create"))
    }
}

fn text(field: &str, s: &str, max: usize) -> Result<String, ToolError> {
    let t = s.trim();
    if t.is_empty() || t.chars().count() > max {
        return Err(bad(format!("`{field}` must be 1-{max} characters")));
    }
    if t.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(bad(format!("`{field}` contains control characters")));
    }
    Ok(t.to_owned())
}

fn opt_text(field: &str, s: Option<&str>, max: usize) -> Result<Option<String>, ToolError> {
    s.map(str::trim).filter(|s| !s.is_empty()).map(|s| text(field, s, max)).transpose()
}

/// An absolute path on nezopt without `..`.
fn abs_path(field: &str, s: &str) -> Result<String, ToolError> {
    let s = s.trim();
    if !s.starts_with('/') || s.len() > 1024 || s.contains('\0') || s.split('/').any(|p| p == "..") {
        return Err(bad(format!("`{field}` must be an absolute path on nezopt, without ..")));
    }
    Ok(s.to_owned())
}

/// A path relative to a workspace (agentd also refuses anything outside it).
fn rel_path(s: &str) -> Result<String, ToolError> {
    let s = s.trim();
    if s.starts_with('/') || s.len() > 1024 || s.contains('\0') || s.split('/').any(|p| p == "..") {
        return Err(bad("`path` is relative to the workspace, without .."));
    }
    Ok(s.to_owned())
}

fn provider_id(s: &str) -> Result<&str, ToolError> {
    let s = s.trim();
    if !word(s, 32, b"-") || !s.starts_with(|c: char| c.is_ascii_lowercase()) {
        return Err(bad("`provider` is a provider id such as claude, codex, opencode, cursor or copilot"));
    }
    if UNSUPERVISED.contains(&s) {
        return Err(bad(format!(
            "{s} runs without permission prompts, so it can't be started from the MCP. Use claude, codex, opencode, cursor or copilot."
        )));
    }
    Ok(s)
}

/// The caller's labels, checked, plus the ones only this module sets.
fn run_labels(c: &Caller, extra: Option<BTreeMap<String, String>>) -> Result<BTreeMap<String, String>, ToolError> {
    let extra = extra.unwrap_or_default();
    if extra.len() > 8 {
        return Err(bad("at most 8 labels"));
    }
    let mut out = BTreeMap::new();
    for (k, v) in extra {
        let reserved = k.starts_with("lucina.") || k.starts_with("paseo.");
        if reserved || !word(&k, 64, b"._-") || k.bytes().any(|b| b.is_ascii_uppercase()) {
            return Err(bad(format!("label {k:?}: keys are a-z, 0-9, '.', '_', '-' and can't start with lucina. or paseo.")));
        }
        if v.chars().count() > 200 || v.chars().any(char::is_control) {
            return Err(bad(format!("label {k:?}: values are up to 200 characters")));
        }
        out.insert(k, v);
    }
    out.insert(LABEL_ORIGIN.into(), format!("mcp:{}", c.name));
    out.insert(LABEL_CALLER.into(), c.name.clone());
    out.insert(LABEL_TOKEN.into(), c.id.clone());
    out.insert(LABEL_DEPTH.into(), (c.depth + 1).to_string());
    Ok(out)
}

/// Starting agents (or schedules) from a caller at the depth limit is refused.
fn may_delegate(c: &Caller) -> Result<(), ToolError> {
    if c.depth >= MAX_DEPTH {
        return Err(bad(
            "This connection belongs to an agent run, and agents started from the MCP can't start further agents (delegation depth is limited to 1). Do the work yourself, or report back to whoever started you.",
        ));
    }
    Ok(())
}

/// The permission mode for an MCP run on this provider (from `/providers`):
/// the asked one or the provider's default, never one marked dangerous.
fn pick_mode(provider: &Value, asked: Option<&str>) -> Result<Option<String>, ToolError> {
    let id = provider["id"].as_str().unwrap_or("?");
    let modes = provider["modes"].as_array().cloned().unwrap_or_default();
    let safe = |m: &Value| m["colorTier"].as_str() != Some("dangerous");
    let safe_ids = || {
        modes.iter().filter(|m| safe(m)).filter_map(|m| m["id"].as_str()).collect::<Vec<_>>().join(", ")
    };
    if let Some(a) = asked {
        let Some(m) = modes.iter().find(|m| m["id"].as_str() == Some(a)) else {
            return Err(bad(if modes.is_empty() {
                format!("{id} has no permission modes; leave `mode` out.")
            } else {
                format!("{id} has no mode {a:?}. Its modes: {}.", safe_ids())
            }));
        };
        if !safe(m) {
            return Err(bad(format!(
                "Mode {a:?} skips permission prompts, so runs from the MCP can't use it. Pick one of: {}.",
                safe_ids()
            )));
        }
        return Ok(Some(a.to_owned()));
    }
    if modes.is_empty() {
        return Ok(None);
    }
    let default = provider["defaultModeId"].as_str();
    modes
        .iter()
        .filter(|m| safe(m))
        .find(|m| m["id"].as_str() == default)
        .or_else(|| modes.iter().find(|m| safe(m)))
        .and_then(|m| m["id"].as_str())
        .map(|m| Some(m.to_owned()))
        .ok_or_else(|| bad(format!("{id} has no mode that keeps permission prompts, so it can't run from the MCP.")))
}

/// May this caller change or remove `schedule`? Only its own (fail closed). agentd
/// checks again: it records the creator (`createdBy: mcp:<token id>`).
pub fn may_change_schedule(c: &Caller, schedule: &Value) -> bool {
    schedule["labels"][LABEL_TOKEN].as_str() == Some(c.id.as_str())
}

/// `0 7 * * 1-5 (Europe/Rome)` or `every 5m`, from agentd's schedule `cadence`.
fn cadence_text(s: &Value) -> Option<String> {
    let c = &s["cadence"];
    match c["type"].as_str()? {
        "cron" => Some(match c["timezone"].as_str() {
            Some(tz) => format!("{} ({tz})", str_of(c, "expression")),
            None => str_of(c, "expression").to_owned(),
        }),
        "every" => Some(format!("every {} min", c["everyMs"].as_u64().unwrap_or(0) / 60_000)),
        _ => None,
    }
}

/// One `schedules_list` line.
fn schedule_line(c: &Caller, s: &Value) -> String {
    let mine = if may_change_schedule(c, s) { " (yours)" } else { "" };
    let target = match s["target"]["type"].as_str() {
        Some("agent") => "heartbeat".to_owned(),
        _ => s["target"]["config"]["provider"].as_str().unwrap_or("agent").to_owned(),
    };
    format!(
        "- {} · {} · {} · {} · next {} · {}{mine}",
        str_of(s, "id"),
        cadence_text(s).unwrap_or_default(),
        target,
        str_of(s, "status"),
        s["nextRunAt"].as_str().unwrap_or("-"),
        s["name"].as_str().or_else(|| s["prompt"].as_str()).unwrap_or("").chars().take(120).collect::<String>()
    )
}

fn wait_secs(v: Option<u64>, default: u64) -> u64 {
    v.unwrap_or(default).min(MAX_WAIT_SECS)
}

// -- formatting -----------------------------------------------------------------

fn clip(s: &str) -> String {
    if s.chars().count() <= MAX_TEXT {
        return s.to_owned();
    }
    s.chars().take(MAX_TEXT).collect::<String>() + "\n[truncated]"
}

/// The end of `s`, for logs and last messages.
fn clip_tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_owned();
    }
    format!("[… {} earlier characters cut]\n{}", n - max, s.chars().skip(n - max).collect::<String>())
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn short(v: &Value) -> String {
    v["shortId"].as_str().map(str::to_owned).unwrap_or_else(|| str_of(v, "id").chars().take(8).collect())
}

fn provider_model(v: &Value) -> String {
    match v["model"].as_str() {
        Some(m) if !m.is_empty() => format!("{}/{m}", str_of(v, "provider")),
        _ => str_of(v, "provider").to_owned(),
    }
}

/// One agent line for lists.
fn item_line(v: &Value) -> String {
    let mut out = format!(
        "- {} {} · {} · {}",
        short(v),
        v["title"].as_str().unwrap_or("(untitled)"),
        provider_model(v),
        str_of(v, "status"),
    );
    if let Some(b) = v["bucket"].as_str() {
        out.push_str(&format!(" ({b})"));
    }
    if v["requiresAttention"] == true {
        out.push_str(&format!(", needs attention: {}", v["attentionReason"].as_str().unwrap_or("yes")));
    }
    out.push_str(&format!(" · {} · updated {}", str_of(v, "cwd"), str_of(v, "updatedAt")));
    if let Some(c) = v["labels"][LABEL_CALLER].as_str() {
        out.push_str(&format!(" · started by {c} (MCP)"));
    }
    out.push_str(&format!(" · id {}", str_of(v, "id")));
    out
}

fn permission_line(p: &Value) -> String {
    let what = p["title"].as_str().or_else(|| p["name"].as_str()).unwrap_or("permission");
    let mut out = format!("- {} · {what}", str_of(p, "id"));
    if let Some(d) = p["description"].as_str().filter(|d| !d.is_empty()) {
        out.push_str(&format!(": {}", d.chars().take(300).collect::<String>()));
    }
    if let Some(a) = p["agentId"].as_str() {
        out.push_str(&format!(" · agent {a}"));
    }
    if let Some(s) = p["status"].as_str() {
        out.push_str(&format!(" · {s}"));
    }
    if let Some(e) = p["expiresAt"].as_str() {
        out.push_str(&format!(" · expires {e}"));
    }
    out
}

const OWNER_ANSWERS: &str = "Only the owner can answer permission requests (Secretary app, or `lagent permit` on nezopt); tell them, then agent_wait again.";

fn gate_line(g: &Value) -> Option<String> {
    if !g.is_object() {
        return None;
    }
    let verdict = g["verdict"].as_str().or_else(|| g["status"].as_str()).unwrap_or("done");
    let reason = g["reason"].as_str().or_else(|| g["summary"].as_str()).unwrap_or("");
    Some(format!("Finished-run gate: {verdict}{}", if reason.is_empty() { String::new() } else { format!(" ({reason})") }))
}

/// `/wait`'s result (`{status, error, lastMessage, final}`) in words.
fn wait_text(v: &Value, waited: u64) -> String {
    let snap = &v["final"];
    let id = str_of(snap, "id");
    let last = v["lastMessage"].as_str().map(str::trim).filter(|m| !m.is_empty());
    let mut out = match str_of(v, "status") {
        "idle" => "It finished its turn and is idle.".to_owned(),
        "error" => format!("It stopped with an error: {}", v["error"].as_str().unwrap_or("unknown error")),
        "permission" => {
            let mut s = "It is waiting for a permission answer:".to_owned();
            for p in snap["pendingPermissions"].as_array().into_iter().flatten() {
                s.push('\n');
                s.push_str(&permission_line(p));
            }
            s.push('\n');
            s.push_str(OWNER_ANSWERS);
            s
        }
        "timeout" => format!(
            "Still working after {waited} s. Call agent_wait with agent_id \"{id}\" to wait more, or agent_logs to see what it is doing."
        ),
        other => format!("Status: {other}."),
    };
    if let Some(g) = gate_line(&snap["gate"]) {
        out.push('\n');
        out.push_str(&g);
    }
    if let Some(m) = last {
        out.push_str("\n\nIts latest message:\n");
        out.push_str(&clip_tail(m, 6000));
    }
    out
}

/// `GET /agents/{id}` (a snapshot) in words.
fn snapshot_text(v: &Value) -> String {
    let mut out = format!(
        "Agent {} — {}\nid: {}\nprovider: {}\nstatus: {}\ncwd: {}\n",
        short(v),
        v["title"].as_str().unwrap_or("(untitled)"),
        str_of(v, "id"),
        provider_model(v),
        str_of(v, "status"),
        str_of(v, "cwd"),
    );
    if let Some(w) = v["workspaceId"].as_str() {
        out.push_str(&format!("workspace: {w}\n"));
    }
    if let Some(m) = v["currentModeId"].as_str() {
        out.push_str(&format!("mode: {m}\n"));
    }
    out.push_str(&format!("created: {} · updated: {}\n", str_of(v, "createdAt"), str_of(v, "updatedAt")));
    if let Some(a) = v["archivedAt"].as_str() {
        out.push_str(&format!("archived: {a}\n"));
    }
    if v["requiresAttention"] == true {
        out.push_str(&format!("needs attention: {}\n", v["attentionReason"].as_str().unwrap_or("yes")));
    }
    if let Some(e) = v["lastError"].as_str() {
        out.push_str(&format!("last error: {e}\n"));
    }
    if let Some(u) = v["lastUsage"].as_object() {
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        out.push_str(&format!("last turn: {} input / {} output tokens\n", n("inputTokens"), n("outputTokens")));
    }
    if let Some(g) = gate_line(&v["gate"]) {
        out.push_str(&g);
        out.push('\n');
    }
    if let Some(labels) = v["labels"].as_object().filter(|l| !l.is_empty()) {
        let l: Vec<String> = labels.iter().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or(""))).collect();
        out.push_str(&format!("labels: {}\n", l.join(", ")));
    }
    let pending = v["pendingPermissions"].as_array().cloned().unwrap_or_default();
    if !pending.is_empty() {
        out.push_str("pending permissions:\n");
        for p in &pending {
            out.push_str(&permission_line(p));
            out.push('\n');
        }
        out.push_str(OWNER_ANSWERS);
        out.push('\n');
    }
    out
}

fn workspace_line(w: &Value) -> String {
    let mut out = format!(
        "- {} · {} · project {} · ",
        str_of(w, "workspaceId"),
        w["name"].as_str().or_else(|| w["title"].as_str()).unwrap_or("(untitled)"),
        str_of(w, "projectDisplayName"),
    );
    match w["branch"].as_str() {
        Some(b) => out.push_str(&format!("branch {b}")),
        None => out.push_str(str_of(w, "isolation")),
    }
    if let Some(b) = w["baseRef"].as_str() {
        out.push_str(&format!(" from {}", b.trim_start_matches("refs/heads/")));
    }
    out.push_str(&format!(" · {}", str_of(w, "cwd")));
    if let Some(s) = w["setup"]["status"].as_str().filter(|s| *s != "none") {
        out.push_str(&format!(" · setup {s}"));
    }
    out
}

/// agentd's parsed diff (`{files: [{path, additions, deletions, hunks: [{lines}]}]}`) as text.
fn diff_text(v: &Value, mode: &str, only: Option<&str>) -> String {
    let files: Vec<&Value> = v["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| only.is_none_or(|p| f["path"].as_str() == Some(p)))
        .collect();
    let adds: u64 = files.iter().filter_map(|f| f["additions"].as_u64()).sum();
    let dels: u64 = files.iter().filter_map(|f| f["deletions"].as_u64()).sum();
    let mut out = format!("{} file(s) changed, +{adds} -{dels} ({mode}) in {}\n", files.len(), str_of(v, "cwd"));
    if v["diffTooLarge"] == true {
        out.push_str("The diff is too large to show in full.\n");
    }
    if let Some(e) = v["error"].as_str() {
        out.push_str(&format!("error: {e}\n"));
    }
    for f in files {
        let mut tag = String::new();
        if f["isNew"] == true {
            tag.push_str(" (new)");
        }
        if f["isDeleted"] == true {
            tag.push_str(" (deleted)");
        }
        out.push_str(&format!(
            "\n### {}{tag} +{} -{}\n",
            str_of(f, "path"),
            f["additions"].as_u64().unwrap_or(0),
            f["deletions"].as_u64().unwrap_or(0)
        ));
        for h in f["hunks"].as_array().into_iter().flatten() {
            for l in h["lines"].as_array().into_iter().flatten() {
                let c = str_of(l, "content");
                let line = match str_of(l, "type") {
                    "header" => c.to_owned(),
                    "add" => format!("+{c}"),
                    "remove" => format!("-{c}"),
                    _ => format!(" {c}"),
                };
                out.push_str(&line);
                out.push('\n');
            }
        }
    }
    clip(&out)
}

/// The activity log (`lagent logs`): Paseo's curator over the projected timeline.
pub fn logs_text(timeline: &Value, tail: usize) -> Result<String, ToolError> {
    let rows: Vec<TimelineRow> = serde_json::from_value(timeline["rows"].clone())
        .map_err(|e| bad(format!("Lucina Agents sent a timeline this tool can't read ({e})")))?;
    let items: Vec<_> = project_rows(&rows).into_iter().map(|e| e.item).collect();
    if items.is_empty() {
        return Ok("(nothing in the log yet)".to_owned());
    }
    let text = curate_activity(&items, &CurateOptions { max_items: tail, ..CurateOptions::default() });
    Ok(clip_tail(&text, MAX_TEXT))
}

// -- errors -----------------------------------------------------------------------

/// A refusal the model can act on.
fn refusal(e: BackendError) -> ToolError {
    let msg = e.message.clone();
    bad(match e.code.as_str() {
        "agents_not_configured" => {
            "Lucina Agents isn't connected to this Lucina MCP server yet (no agents token). Ask the owner.".to_owned()
        }
        "AGENT_NOT_FOUND" => format!("{msg}. agents_list (or agents_search) shows the ids."),
        "AGENT_AMBIGUOUS" => format!("{msg}. Give more characters of the id."),
        "WORKSPACE_NOT_FOUND" => format!("{msg}. workspaces_list shows the workspaces."),
        "TERMINAL_NOT_FOUND" => format!("{msg}. terminal_list shows the terminals you can use."),
        "TERMINAL_FORBIDDEN" | "TERMINAL_EXITED" | "WORKSPACE_SCRIPT_START_FAILED" | "WORKSPACE_SCRIPT_STOP_FAILED" => msg,
        "WORKSPACE_GUARD" => format!(
            "{msg}. From the MCP, agents run in a worktree: create one with workspace_create and pass its workspace_id."
        ),
        "PERMISSION_FORBIDDEN" | "OWNER_ONLY" => {
            format!("Only the owner can do that (Secretary app, or lagent on nezopt): {msg}")
        }
        "INVALID_STATE" | "INVALID_REQUEST" | "UNKNOWN_PROVIDER" | "PROVIDER_ERROR" | "FILES_FAILED"
        | "WORKSPACE_INVALID" | "WORKSPACE_GIT_FAILED" | "PERMISSION_NOT_FOUND" => format!("{}: {msg}", e.code),
        _ if e.status == 404 || e.status == 405 => {
            "Lucina Agents isn't available on this server yet (no /v1/agents route).".to_owned()
        }
        _ => return e.into(),
    })
}

/// A 404/405 that means the route is not there yet (as opposed to `<KIND>_NOT_FOUND`).
fn route_missing(e: &BackendError, found_code: &str) -> bool {
    (e.status == 404 || e.status == 405) && e.code != found_code
}

// -- tools --------------------------------------------------------------------------

impl Tools<'_> {
    fn agents_headers(&self) -> HeaderMap {
        let c = self.caller;
        let mut h = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&format!("{}:{}", c.id, c.name)) {
            h.insert("x-lucina-mcp-caller", v);
        }
        h.insert("x-lucina-delegation-depth", HeaderValue::from(u16::from(c.depth)));
        if let Ok(v) = HeaderValue::from_str(&format!("mcp:{}", c.name)) {
            h.insert("x-secretary-caller", v);
        }
        h
    }

    async fn areq(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        timeout: Duration,
    ) -> Result<Value, BackendError> {
        self.app
            .backend
            .send_agents(method, &format!("/v1/agents{path}"), query, body, timeout, self.agents_headers())
            .await
    }

    /// The permission mode for an MCP run of `provider` (from `/providers`, see [`pick_mode`]).
    async fn safe_mode(&self, provider: &str, mode: Option<&str>) -> Result<Option<String>, ToolError> {
        let providers = self.aget("/providers", &[]).await?;
        let info = providers.as_array().into_iter().flatten().find(|p| p["id"] == provider).ok_or_else(|| {
            let ids: Vec<&str> = providers
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|p| p["id"].as_str())
                .filter(|id| !UNSUPERVISED.contains(id))
                .collect();
            bad(format!("No provider {provider:?}. Available: {}.", ids.join(", ")))
        })?;
        pick_mode(info, mode)
    }

    async fn aget(&self, path: &str, query: &[(&str, String)]) -> Result<Value, ToolError> {
        self.areq(Method::GET, path, query, None, CALL_TIMEOUT).await.map_err(refusal)
    }

    async fn apost(&self, path: &str, body: &Value) -> Result<Value, ToolError> {
        self.areq(Method::POST, path, &[], Some(body), CALL_TIMEOUT).await.map_err(refusal)
    }

    /// Waits up to `secs` in slices, reporting progress between them.
    async fn wait_agent(&self, id: &str, secs: u64, tick: &mut (dyn FnMut(u64) + Send)) -> Result<Value, ToolError> {
        let started = Instant::now();
        loop {
            let left = secs.saturating_sub(started.elapsed().as_secs());
            let slice = left.clamp(1, WAIT_SLICE_SECS);
            let v = self
                .areq(
                    Method::POST,
                    &format!("/{}/wait", seg(id)),
                    &[("timeoutMs", (slice * 1000).to_string())],
                    Some(&json!({})),
                    Duration::from_secs(slice + 15),
                )
                .await
                .map_err(refusal)?;
            if v["status"] != "timeout" || started.elapsed().as_secs() >= secs {
                return Ok(v);
            }
            tick(started.elapsed().as_secs());
        }
    }

    async fn items(&self, archived: bool) -> Result<Vec<Value>, ToolError> {
        let q: &[(&str, String)] = if archived { &[("archived", "true".into())] } else { &[] };
        let v = self.aget("/items", q).await?;
        Ok(v["agents"].as_array().cloned().unwrap_or_default())
    }

    /// Every agents-scope tool. `tick` reports progress while waiting.
    pub async fn agents(&self, name: &str, args: &JsonObject, tick: &mut (dyn FnMut(u64) + Send)) -> Result<Outcome, ToolError> {
        let c = self.caller;
        match name {
            "agents_list" => {
                let a: ListArgs = parse(args)?;
                let mut items = self.items(a.archived.unwrap_or(false)).await?;
                if let Some(s) = a.status.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    items.retain(|v| v["status"] == s || v["bucket"] == s);
                }
                if a.mine == Some(true) {
                    items.retain(|v| v["labels"][LABEL_TOKEN].as_str() == Some(c.id.as_str()));
                }
                items.sort_by(|x, y| str_of(y, "updatedAt").cmp(str_of(x, "updatedAt")));
                let total = items.len();
                items.truncate(a.limit.unwrap_or(30).clamp(1, 200) as usize);
                if items.is_empty() {
                    return Ok(Outcome::text("No agents match.".into()));
                }
                let mut out = format!("{total} agent(s), newest first:\n");
                for v in &items {
                    out.push_str(&item_line(v));
                    out.push('\n');
                }
                if total > items.len() {
                    out.push_str(&format!("… and {} more (raise limit).\n", total - items.len()));
                }
                Ok(Outcome::text(clip(&out)))
            }
            "agents_search" => {
                let a: SearchArgs = parse(args)?;
                let q = text("q", &a.q, 200)?.to_lowercase();
                let limit = a.limit.unwrap_or(20).clamp(1, 100);
                // agentd's search (phase 6): names with typos, plus what was said.
                let query = [("q", q.clone()), ("limit", limit.to_string()), ("archived", a.archived.unwrap_or(true).to_string())];
                match self.areq(Method::GET, "/search", &query, None, CALL_TIMEOUT).await {
                    Ok(v) => {
                        let results = v["results"].as_array().cloned().unwrap_or_default();
                        if results.is_empty() {
                            return Ok(Outcome::text(format!("No agent matches {q:?}.")));
                        }
                        let mut out = format!("{} match(es):\n", results.len());
                        for r in &results {
                            out.push_str(&format!(
                                "- {} {} · {} · {}\n",
                                str_of(r, "agentId").chars().take(8).collect::<String>(),
                                r["title"].as_str().unwrap_or("(untitled)"),
                                str_of(r, "provider"),
                                str_of(r, "status")
                            ));
                            for m in r["matches"].as_array().into_iter().flatten().take(3) {
                                out.push_str(&format!("    {}: {}\n", str_of(m, "field"), str_of(m, "snippet").replace('\n', " ")));
                            }
                        }
                        return Ok(Outcome::text(clip(&out)));
                    }
                    Err(e) if route_missing(&e, "") => {}
                    Err(e) => return Err(refusal(e)),
                }
                let mut items = self.items(false).await?;
                if a.archived.unwrap_or(true) {
                    items.extend(self.items(true).await?);
                }
                let words: Vec<&str> = q.split_whitespace().collect();
                items.retain(|v| {
                    let hay = [
                        str_of(v, "title"),
                        str_of(v, "id"),
                        str_of(v, "cwd"),
                        str_of(v, "provider"),
                        str_of(v, "model"),
                    ]
                    .join(" ")
                        + " "
                        + &v["labels"].as_object().map(|l| l.values().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default();
                    let hay = hay.to_lowercase();
                    words.iter().all(|w| hay.contains(w))
                });
                items.sort_by(|x, y| str_of(y, "updatedAt").cmp(str_of(x, "updatedAt")));
                items.truncate(a.limit.unwrap_or(20).clamp(1, 100) as usize);
                let mut out = if items.is_empty() {
                    format!("No agent matches {q:?}.")
                } else {
                    let mut s = format!("{} match(es):\n", items.len());
                    for v in &items {
                        s.push_str(&item_line(v));
                        s.push('\n');
                    }
                    s
                };
                out.push_str("\n(Searches titles, ids, directories, providers and labels; searching what agents said arrives with phase 6.)");
                Ok(Outcome::text(clip(&out)))
            }
            "agent_run" => {
                let a: RunArgs = parse(args)?;
                may_delegate(c)?;
                let prompt = text("prompt", &a.prompt, MAX_PROMPT_CHARS)?;
                let provider = provider_id(&a.provider)?.to_owned();
                let model = a.model.as_deref().map(str::trim).filter(|m| !m.is_empty());
                if model.is_some_and(|m| !word(m, 80, b"._:/-")) {
                    return Err(bad("`model` is a model id such as claude-opus-5-5"));
                }
                let mode = a.mode.as_deref().map(str::trim).filter(|m| !m.is_empty());
                if mode.is_some_and(|m| !word(m, 40, b"_-")) {
                    return Err(bad("`mode` is a permission mode id such as plan, default or acceptEdits"));
                }
                let title = opt_text("title", a.title.as_deref(), 120)?;
                let workspace = a.workspace_id.as_deref().map(workspace_ref).transpose()?;
                let cwd = a.cwd.as_deref().map(|p| abs_path("cwd", p)).transpose()?;
                if workspace.is_none() && cwd.is_none() {
                    return Err(bad(
                        "Say where it works: `workspace_id` (create a worktree with workspace_create) or an absolute `cwd`.",
                    ));
                }
                let labels = run_labels(c, a.labels)?;
                let mode = self.safe_mode(&provider, mode).await?;
                let running = self
                    .items(false)
                    .await?
                    .iter()
                    .filter(|v| v["labels"][LABEL_TOKEN].as_str() == Some(c.id.as_str()))
                    .filter(|v| matches!(str_of(v, "status"), "running" | "initializing"))
                    .count();
                let cfg = &self.app.cfg;
                if running >= cfg.agents_max_running {
                    return Err(bad(format!(
                        "This connection already has {running} agents running (the limit is {}). Wait for one (agent_wait) or stop one (agent_stop).",
                        cfg.agents_max_running
                    )));
                }
                if !self.app.agents.reserve(&c.id, cfg.agents_runs_per_hour) {
                    return Err(bad(format!(
                        "This connection has started {} agents in the last hour, the limit. Try later.",
                        cfg.agents_runs_per_hour
                    )));
                }
                let mut body = json!({
                    "provider": provider,
                    "cwd": cwd.clone().unwrap_or_default(),
                    "prompt": prompt,
                    "labels": labels,
                    "idempotencyKey": uuid::Uuid::new_v4().to_string(),
                });
                let optional = [
                    ("model", model.map(str::to_owned)),
                    ("title", title),
                    ("modeId", mode.clone()),
                    ("workspaceId", workspace.map(str::to_owned)),
                ];
                for (k, v) in optional.into_iter().filter_map(|(k, v)| v.map(|v| (k, v))) {
                    body[k] = json!(v);
                }
                let snap = match self.apost("", &body).await {
                    Ok(s) => s,
                    Err(e) => {
                        self.app.agents.release(&c.id);
                        return Err(e);
                    }
                };
                let id = str_of(&snap, "id").to_owned();
                if !word(&id, 64, b"-") {
                    return Err(bad("Lucina Agents returned no agent id"));
                }
                let mut out = format!(
                    "Started agent {} ({}) with {}{} in {}.",
                    short(&snap),
                    id,
                    provider_model(&snap),
                    mode.map(|m| format!(", mode {m}")).unwrap_or_default(),
                    str_of(&snap, "cwd"),
                );
                let secs = wait_secs(a.wait_seconds, 30);
                if secs > 0 {
                    let w = self.wait_agent(&id, secs, tick).await?;
                    out.push(' ');
                    out.push_str(&wait_text(&w, secs));
                } else {
                    out.push_str(&format!(" Call agent_wait with agent_id \"{id}\" for its answer."));
                }
                Ok(Outcome { text: out, target: Some(id) })
            }
            "agent_send" => {
                let a: SendArgs = parse(args)?;
                let id = agent_ref(&a.agent_id)?;
                let prompt = text("prompt", &a.prompt, MAX_PROMPT_CHARS)?;
                let behavior = if a.steer == Some(true) { "steer" } else { "interrupt" };
                let v = self
                    .apost(&format!("/{}/send", seg(id)), &json!({"prompt": prompt, "activeTurnBehavior": behavior}))
                    .await?;
                let full = v["agentId"].as_str().unwrap_or(id).to_owned();
                let mut out = match v["disposition"].as_str() {
                    Some("steered") => "Added to its running turn.".to_owned(),
                    Some("out_of_band") => "Handled as a command, outside the conversation.".to_owned(),
                    _ => "Sent; it started a new turn.".to_owned(),
                };
                let secs = wait_secs(a.wait_seconds, 0);
                if secs > 0 {
                    let w = self.wait_agent(&full, secs, tick).await?;
                    out.push(' ');
                    out.push_str(&wait_text(&w, secs));
                }
                Ok(Outcome { text: out, target: Some(full) })
            }
            "agent_wait" => {
                let a: WaitArgs = parse(args)?;
                let id = agent_ref(&a.agent_id)?;
                let secs = wait_secs(a.wait_seconds, 45).max(1);
                let w = self.wait_agent(id, secs, tick).await?;
                Ok(Outcome::text(wait_text(&w, secs)))
            }
            "agent_logs" => {
                let a: LogsArgs = parse(args)?;
                let id = agent_ref(&a.agent_id)?;
                let v = self
                    .areq(Method::GET, &format!("/{}/timeline", seg(id)), &[], None, Duration::from_secs(60))
                    .await
                    .map_err(refusal)?;
                Ok(Outcome::text(logs_text(&v, a.tail.unwrap_or(40) as usize)?))
            }
            "agent_inspect" => {
                let a: AgentArgs = parse(args)?;
                let id = agent_ref(&a.agent_id)?;
                let v = self.aget(&format!("/{}", seg(id)), &[]).await?;
                Ok(Outcome::text(clip(&snapshot_text(&v))))
            }
            "agent_stop" => {
                let a: AgentArgs = parse(args)?;
                let id = agent_ref(&a.agent_id)?;
                let v = self.apost(&format!("/{}/stop", seg(id)), &json!({})).await?;
                let full = v["agentId"].as_str().unwrap_or(id).to_owned();
                let text = if v["stopped"] == true {
                    format!("Stopped agent {full}. Its work so far stays in its workspace.")
                } else {
                    format!("Agent {full} was not running.")
                };
                Ok(Outcome { text, target: Some(full) })
            }
            "agent_archive" => {
                let a: AgentArgs = parse(args)?;
                let id = agent_ref(&a.agent_id)?;
                let v = self.apost(&format!("/{}/archive", seg(id)), &json!({})).await?;
                let full = v["agentId"].as_str().unwrap_or(id).to_owned();
                Ok(Outcome {
                    text: format!(
                        "Archived agent {full} ({}). It is hidden from the lists; its workspace and history stay.",
                        str_of(&v, "archivedAt")
                    ),
                    target: Some(full),
                })
            }
            "workspaces_list" => {
                let a: WorkspacesArgs = parse(args)?;
                let v = self.aget("/workspaces", &[]).await?;
                let q = a.q.as_deref().map(|q| q.trim().to_lowercase()).filter(|q| !q.is_empty());
                let list: Vec<&Value> = v["workspaces"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|w| {
                        q.as_deref().is_none_or(|q| {
                            ["projectDisplayName", "title", "name", "branch", "cwd"]
                                .iter()
                                .any(|k| str_of(w, k).to_lowercase().contains(q))
                        })
                    })
                    .collect();
                if list.is_empty() {
                    return Ok(Outcome::text("No workspaces. workspace_create makes a worktree for a repository.".into()));
                }
                let mut out = format!("{} workspace(s):\n", list.len());
                for w in list {
                    out.push_str(&workspace_line(w));
                    out.push('\n');
                }
                Ok(Outcome::text(clip(&out)))
            }
            "workspace_create" => {
                let a: WorkspaceCreateArgs = parse(args)?;
                let path = abs_path("repo_path", &a.repo_path)?;
                let mut body = json!({"isolation": "worktree", "path": path, "mode": "branch-off"});
                if let Some(b) = a.branch.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
                    if !word(b, 60, b"-") || b.bytes().any(|c| c.is_ascii_uppercase()) {
                        return Err(bad("`branch` is a-z, 0-9 and '-', up to 60"));
                    }
                    body["worktreeSlug"] = json!(b);
                }
                if let Some(b) = a.base.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
                    if !word(b, 120, b"-_./") || b.contains("..") {
                        return Err(bad("`base` is a branch or ref name"));
                    }
                    body["base"] = json!(b);
                }
                if let Some(t) = opt_text("title", a.title.as_deref(), 120)? {
                    body["title"] = json!(t);
                }
                if let Some(p) = opt_text("prompt", a.prompt.as_deref(), MAX_PROMPT_CHARS)? {
                    body["prompt"] = json!(p);
                }
                let v = self.apost("/workspaces", &body).await?;
                let w = &v["workspace"];
                let id = str_of(w, "workspaceId").to_owned();
                let mut out = format!("Created workspace {id}:\n{}\n", workspace_line(w));
                if let Some(r) = v["setupSkippedReason"].as_str() {
                    out.push_str(&format!("Setup scripts did not run: {r}\n"));
                }
                out.push_str(&format!("Start an agent in it with agent_run and workspace_id \"{id}\"."));
                Ok(Outcome { text: out, target: Some(id) })
            }
            "workspace_diff" => {
                let a: DiffArgs = parse(args)?;
                let id = workspace_ref(&a.workspace_id)?;
                let mode = a.mode.unwrap_or(DiffMode::All).as_str();
                let only = a.path.as_deref().map(rel_path).transpose()?;
                let v = self.aget(&format!("/workspaces/{}/diff", seg(id)), &[("mode", mode.to_owned())]).await?;
                Ok(Outcome::text(diff_text(&v, mode, only.as_deref())))
            }
            "files_list" => {
                let a: FilesArgs = parse(args)?;
                let id = workspace_ref(&a.workspace_id)?;
                let path = a.path.as_deref().map(rel_path).transpose()?.unwrap_or_default();
                let v = self.aget(&format!("/workspaces/{}/files", seg(id)), &[("path", path)]).await?;
                let mut out = format!("{}/\n", str_of(&v, "path"));
                for e in v["entries"].as_array().into_iter().flatten() {
                    if e["kind"] == "directory" {
                        out.push_str(&format!("- {}/\n", str_of(e, "name")));
                    } else {
                        out.push_str(&format!("- {} ({} B)\n", str_of(e, "name"), e["size"].as_u64().unwrap_or(0)));
                    }
                }
                Ok(Outcome::text(clip(&out)))
            }
            "file_read" => {
                let a: FileReadArgs = parse(args)?;
                let id = workspace_ref(&a.workspace_id)?;
                let path = rel_path(&a.path)?;
                if path.is_empty() {
                    return Err(bad("`path` names a file"));
                }
                let v = self
                    .aget(&format!("/workspaces/{}/file", seg(id)), &[("path", path.clone()), ("maxBytes", "2000000".into())])
                    .await?;
                if v["kind"] != "text" {
                    return Ok(Outcome::text(format!(
                        "{path} is not a text file ({}, {} bytes); not shown.",
                        str_of(&v, "mimeType"),
                        v["size"].as_u64().unwrap_or(0)
                    )));
                }
                let content = str_of(&v, "content");
                let total = content.chars().count();
                let from = a.offset.unwrap_or(0).min(total);
                let page: String = content.chars().skip(from).take(MAX_TEXT).collect();
                let end = from + page.chars().count();
                let mut out = format!("{path} (characters {from}-{end} of {total}):\n{page}");
                if end < total {
                    out.push_str(&format!("\n[more: call file_read again with offset {end}]"));
                }
                Ok(Outcome::text(out))
            }
            "permissions_list" => {
                let a: PermissionsArgs = parse(args)?;
                let q: &[(&str, String)] = if a.all == Some(true) { &[("all", "true".into())] } else { &[] };
                let v = self.aget("/permissions", q).await?;
                let list = v["permissions"].as_array().cloned().unwrap_or_default();
                let mut out = if list.is_empty() {
                    "No permission requests are waiting.".to_owned()
                } else {
                    let mut s = format!("{} permission request(s):\n", list.len());
                    for p in &list {
                        s.push_str(&permission_line(p));
                        s.push('\n');
                    }
                    s
                };
                out.push_str("\nThis list is read only. ");
                out.push_str(OWNER_ANSWERS);
                Ok(Outcome::text(clip(&out)))
            }
            "agents_usage" => {
                let _: NoArgs = parse(args)?;
                let agents = self.aget("", &[]).await?;
                let providers = self.aget("/providers", &[]).await?;
                let mut out = usage_text(&agents, &providers);
                // Schedules, from agentd's `/usage` (phase 6; left out on an older daemon).
                if let Ok(u) = self.areq(Method::GET, "/usage", &[], None, CALL_TIMEOUT).await {
                    let n = |k: &str| u["schedules"][k].as_u64().unwrap_or(0);
                    out.push_str(&format!("Schedules: {} active, {} paused, {} completed.\n", n("active"), n("paused"), n("completed")));
                }
                Ok(Outcome::text(out))
            }
            "terminal_list" => {
                let a: WorkspaceArgs = parse(args)?;
                let q: Vec<(&str, String)> = match a.workspace_id.as_deref() {
                    Some(w) => vec![("workspaceId", workspace_ref(w)?.to_owned())],
                    None => Vec::new(),
                };
                let v = self.areq(Method::GET, "/terminals", &q, None, CALL_TIMEOUT).await.map_err(|e| no_terminals(e, &[]))?;
                let list = v["terminals"].as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    return Ok(Outcome::text(
                        "No terminals you can use. You see the terminals of agents you started and of scripts you ran (script_start).".into(),
                    ));
                }
                let mut out = format!("{} terminal(s):\n", list.len());
                for t in &list {
                    out.push_str(&terminal_line(t));
                    out.push('\n');
                }
                Ok(Outcome::text(clip(&out)))
            }
            "terminal_read" => {
                let a: TerminalArgs = parse(args)?;
                let id = terminal_ref(&a.terminal_id)?;
                let n = a.lines.unwrap_or(200).clamp(1, 2000);
                let v = self
                    .areq(Method::GET, &format!("/terminals/{}/capture", seg(id)), &[("start", format!("-{n}"))], None, CALL_TIMEOUT)
                    .await
                    .map_err(|e| no_terminals(e, &["TERMINAL_NOT_FOUND"]))?;
                let mut lines: Vec<&str> = v["lines"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                while lines.last().is_some_and(|l| l.is_empty()) {
                    lines.pop();
                }
                let body = clip_tail(&lines.join("\n"), MAX_TEXT);
                Ok(Outcome { text: format!("Terminal {id}, last {} line(s) of {}:\n{body}", lines.len(), v["totalLines"].as_u64().unwrap_or(0)), target: Some(id.to_owned()) })
            }
            "terminal_send" => {
                let a: TerminalSendArgs = parse(args)?;
                let id = terminal_ref(&a.terminal_id)?;
                if a.keys.is_empty() || a.keys.chars().count() > 4000 {
                    return Err(bad("`keys` is 1 to 4000 characters"));
                }
                let body = json!({"keys": a.keys, "literal": a.literal.unwrap_or(false)});
                let v = self
                    .areq(Method::POST, &format!("/terminals/{}/input", seg(id)), &[], Some(&body), CALL_TIMEOUT)
                    .await
                    .map_err(|e| no_terminals(e, &["TERMINAL_NOT_FOUND", "TERMINAL_EXITED"]))?;
                Ok(Outcome {
                    text: format!("Typed {} character(s) into terminal {id}. terminal_read shows what it printed.", v["keysSent"].as_u64().unwrap_or(0)),
                    target: Some(id.to_owned()),
                })
            }
            "scripts_list" => {
                let a: WorkspaceArgs = parse(args)?;
                let id = workspace_ref(a.workspace_id.as_deref().ok_or_else(|| bad("`workspace_id` is required"))?)?;
                let v = self
                    .areq(Method::GET, &format!("/workspaces/{}/scripts", seg(id)), &[], None, CALL_TIMEOUT)
                    .await
                    .map_err(|e| no_terminals(e, &["WORKSPACE_NOT_FOUND"]))?;
                let list = v["scripts"].as_array().cloned().unwrap_or_default();
                if list.is_empty() {
                    return Ok(Outcome::text(format!("Workspace {id} defines no scripts (paseo.json `scripts`, .lucina/agents.toml `[scripts.*]`).")));
                }
                let mut out = format!("{} script(s) in {id}:\n", list.len());
                for x in &list {
                    out.push_str(&script_line(x));
                    out.push('\n');
                }
                out.push_str("Services are opened by the owner in the Secretary app; their output is in terminal_read.");
                Ok(Outcome { text: clip(&out), target: Some(id.to_owned()) })
            }
            "script_start" | "script_stop" => {
                let a: ScriptArgs = parse(args)?;
                let id = workspace_ref(&a.workspace_id)?;
                let script = script_ref(&a.script)?;
                let verb = if name == "script_start" { "start" } else { "stop" };
                let v = self
                    .areq(Method::POST, &format!("/workspaces/{}/scripts/{}/{verb}", seg(id), seg(script)), &[], Some(&json!({})), CALL_TIMEOUT)
                    .await
                    .map_err(|e| no_terminals(e, &["WORKSPACE_NOT_FOUND"]))?;
                let what = if verb == "start" { "Started" } else { "Stopped" };
                Ok(Outcome { text: format!("{what} {script} in {id}:\n{}", script_line(&v)), target: Some(format!("{id}/{script}")) })
            }
            "schedules_list" => {
                let _: NoArgs = parse(args)?;
                let v = match self.areq(Method::GET, "/schedules", &[], None, CALL_TIMEOUT).await {
                    Err(e) if route_missing(&e, "SCHEDULE_NOT_FOUND") => return Err(bad(PHASE6_SCHEDULES)),
                    r => r.map_err(refusal)?,
                };
                let list = v["schedules"].as_array().cloned().unwrap_or_default();
                let mut out = format!("{} schedule(s):\n", list.len());
                for s in &list {
                    out.push_str(&schedule_line(c, s));
                    out.push('\n');
                }
                Ok(Outcome::text(clip(&out)))
            }
            "schedule_add" => {
                let a: ScheduleAddArgs = parse(args)?;
                may_delegate(c)?;
                let cron = text("cron", &a.cron, 100)?;
                let provider = provider_id(&a.provider)?.to_owned();
                let prompt = text("prompt", &a.prompt, MAX_PROMPT_CHARS)?;
                // Each run is unattended: never a mode that skips the owner's permission answers.
                let mode = self.safe_mode(&provider, None).await?;
                let mut body = json!({"cron": cron, "provider": provider, "prompt": prompt, "labels": run_labels(c, None)?});
                if let Some(m) = mode {
                    body["modeId"] = json!(m);
                }
                if let Some(w) = a.workspace_id.as_deref().map(workspace_ref).transpose()? {
                    body["workspaceId"] = json!(w);
                }
                if let Some(p) = a.cwd.as_deref().map(|p| abs_path("cwd", p)).transpose()? {
                    body["cwd"] = json!(p);
                }
                if let Some(t) = opt_text("title", a.title.as_deref(), 120)? {
                    body["title"] = json!(t);
                }
                let v = match self.areq(Method::POST, "/schedules", &[], Some(&body), CALL_TIMEOUT).await {
                    Err(e) if route_missing(&e, "SCHEDULE_NOT_FOUND") => return Err(bad(PHASE6_SCHEDULES)),
                    r => r.map_err(refusal)?,
                };
                let id = str_of(&v, "id").to_owned();
                let next = v["nextRunAt"].as_str().map(|n| format!(", next run {n}")).unwrap_or_default();
                Ok(Outcome { text: format!("Added schedule {id} ({}{next}).", cadence_text(&v).unwrap_or_else(|| cron.to_owned())), target: Some(id) })
            }
            "schedule_remove" => {
                let a: ScheduleArgs = parse(args)?;
                let id = a.schedule_id.trim();
                if !word(id, 64, b"-_") {
                    return Err(bad("`schedule_id` comes from schedules_list"));
                }
                let path = format!("/schedules/{}", seg(id));
                let s = match self.areq(Method::GET, &path, &[], None, CALL_TIMEOUT).await {
                    Err(e) if route_missing(&e, "SCHEDULE_NOT_FOUND") => return Err(bad(PHASE6_SCHEDULES)),
                    r => r.map_err(refusal)?,
                };
                if !may_change_schedule(c, &s) {
                    return Err(bad(
                        "That schedule was not created by this connection, so it can't change it. The owner can, in the Secretary app.",
                    ));
                }
                self.areq(Method::DELETE, &path, &[], None, CALL_TIMEOUT).await.map_err(refusal)?;
                Ok(Outcome { text: format!("Removed schedule {id}."), target: Some(id.to_owned()) })
            }
            _ => Err(bad(format!("unknown tool {name}"))),
        }
    }
}

/// Token use of the agents' last turns, by provider, and the providers.
fn usage_text(agents: &Value, providers: &Value) -> String {
    let mut by: BTreeMap<String, (u64, u64, u64, u64, f64)> = BTreeMap::new();
    for a in agents["agents"].as_array().into_iter().flatten() {
        let e = by.entry(str_of(a, "provider").to_owned()).or_default();
        e.0 += 1;
        if matches!(str_of(a, "status"), "running" | "initializing") {
            e.1 += 1;
        }
        let u = &a["lastUsage"];
        e.2 += u["inputTokens"].as_u64().unwrap_or(0);
        e.3 += u["outputTokens"].as_u64().unwrap_or(0);
        e.4 += u["totalCostUsd"].as_f64().or_else(|| u["costUsd"].as_f64()).unwrap_or(0.0);
    }
    let mut out = String::from("Current agents by provider (tokens are each agent's last turn):\n");
    if by.is_empty() {
        out.push_str("- none\n");
    }
    for (p, (n, running, i, o, cost)) in &by {
        out.push_str(&format!("- {p}: {n} agent(s), {running} running, {i} input / {o} output tokens"));
        if *cost > 0.0 {
            out.push_str(&format!(", ${cost:.2}"));
        }
        out.push('\n');
    }
    let ids: Vec<String> = providers
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["id"].as_str())
        .map(|id| if UNSUPERVISED.contains(&id) { format!("{id} (not from the MCP)") } else { id.to_owned() })
        .collect();
    out.push_str(&format!("Providers: {}.\n", ids.join(", ")));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::Scopes;

    fn caller(depth: u8) -> Caller {
        Caller {
            name: "hermes".into(),
            kind: CallerKind::ServiceToken,
            scopes: Scopes::parse_list("read,agents").unwrap(),
            id: "tok_hermes".into(),
            depth,
        }
    }

    /// Claude's and Codex's modes as lucina-agentd reports them (captured 2026-10-28).
    fn claude() -> Value {
        json!({"id": "claude", "defaultModeId": "default", "modes": [
            {"id": "plan", "colorTier": "safe"}, {"id": "default", "colorTier": "safe"},
            {"id": "acceptEdits", "colorTier": "moderate"}, {"id": "auto", "colorTier": "moderate"},
            {"id": "bypassPermissions", "colorTier": "dangerous"}]})
    }

    #[test]
    fn modes_never_skip_permission_prompts() {
        assert_eq!(pick_mode(&claude(), None).unwrap().as_deref(), Some("default"));
        assert_eq!(pick_mode(&claude(), Some("acceptEdits")).unwrap().as_deref(), Some("acceptEdits"));
        let e = pick_mode(&claude(), Some("bypassPermissions")).unwrap_err().0;
        assert!(e.contains("skips permission prompts") && e.contains("plan, default"), "{e}");
        assert!(pick_mode(&claude(), Some("yolo")).unwrap_err().0.contains("no mode"));
        // A dangerous default (the mock's load-test) is passed over for a safe one.
        let mock = json!({"id": "mock", "defaultModeId": "load-test", "modes": [
            {"id": "load-test", "colorTier": "dangerous"}, {"id": "approval-test", "colorTier": "safe"}]});
        assert_eq!(pick_mode(&mock, None).unwrap().as_deref(), Some("approval-test"));
        let only_bypass = json!({"id": "x", "defaultModeId": "full", "modes": [{"id": "full", "colorTier": "dangerous"}]});
        assert!(pick_mode(&only_bypass, None).is_err());
        // ACP providers have no modes.
        assert_eq!(pick_mode(&json!({"id": "opencode", "modes": []}), None).unwrap(), None);
        assert!(pick_mode(&json!({"id": "opencode", "modes": []}), Some("plan")).is_err());
    }

    #[test]
    fn unsupervised_providers_are_refused() {
        assert!(provider_id("claude").is_ok());
        assert!(provider_id("zcode").unwrap_err().0.contains("without permission prompts"));
        assert!(provider_id("pi").is_err());
        assert!(provider_id("Claude").is_err());
        assert!(provider_id("claude/opus").is_err());
    }

    #[test]
    fn depth_is_limited_to_one() {
        assert!(may_delegate(&caller(0)).is_ok());
        assert!(may_delegate(&caller(1)).unwrap_err().0.contains("depth is limited to 1"));
        let l = run_labels(&caller(0), None).unwrap();
        assert_eq!(l[LABEL_DEPTH], "1");
        assert_eq!(l[LABEL_TOKEN], "tok_hermes");
        assert_eq!(l[LABEL_CALLER], "hermes");
        assert_eq!(l[LABEL_ORIGIN], "mcp:hermes");
    }

    #[test]
    fn callers_cannot_forge_reserved_labels() {
        let forge = |k: &str| run_labels(&caller(0), Some([(k.to_owned(), "0".to_owned())].into()));
        assert!(forge(LABEL_DEPTH).is_err());
        assert!(forge(LABEL_TOKEN).is_err());
        assert!(forge("paseo.parent-agent-id").is_err());
        assert!(forge("Ticket").is_err());
        let ok = forge("ticket").unwrap();
        assert_eq!(ok["ticket"], "0");
        assert_eq!(ok[LABEL_DEPTH], "1");
        let many: BTreeMap<String, String> = (0..9).map(|i| (format!("k{i}"), "v".into())).collect();
        assert!(run_labels(&caller(0), Some(many)).is_err());
    }

    #[test]
    fn schedules_change_only_when_created_here() {
        let c = caller(0);
        assert!(may_change_schedule(&c, &json!({"id": "s1", "labels": {LABEL_TOKEN: "tok_hermes"}})));
        assert!(!may_change_schedule(&c, &json!({"id": "s1", "labels": {LABEL_TOKEN: "tok_other"}})));
        assert!(!may_change_schedule(&c, &json!({"id": "s1"})));
        assert!(!may_change_schedule(&c, &json!({"id": "s1", "createdBy": "mcp:hermes"})));
    }

    #[test]
    fn run_budget_per_token() {
        let log = RunLog::default();
        assert!(log.reserve("a", 2));
        assert!(log.reserve("a", 2));
        assert!(!log.reserve("a", 2));
        assert!(log.reserve("b", 2));
        log.release("a");
        assert!(log.reserve("a", 2));
    }

    #[test]
    fn paths_and_ids() {
        assert!(abs_path("cwd", "/home/nez/Projects/x").is_ok());
        assert!(abs_path("cwd", "relative").is_err());
        assert!(abs_path("cwd", "/home/nez/../root").is_err());
        assert!(rel_path("../etc/passwd").is_err());
        assert!(rel_path("/etc/passwd").is_err());
        assert_eq!(rel_path("src/a.txt").unwrap(), "src/a.txt");
        assert!(agent_ref("c0282964-bacb").is_ok());
        assert!(agent_ref("../x").is_err());
        assert!(workspace_ref("wks_42a3dadd481e4d7f").is_ok());
        assert!(workspace_ref("wks/../x").is_err());
        assert_eq!(wait_secs(Some(500), 30), MAX_WAIT_SECS);
    }

    #[test]
    fn wait_results_read_well() {
        let snap = json!({"id": "a1", "pendingPermissions": [{"id": "perm-1", "title": "Bash", "description": "rm -rf build", "status": "pending"}]});
        let t = wait_text(&json!({"status": "permission", "final": snap}), 10);
        assert!(t.contains("perm-1") && t.contains("Only the owner"), "{t}");
        let t = wait_text(&json!({"status": "timeout", "final": {"id": "a1"}, "lastMessage": "working"}), 45);
        assert!(t.contains("after 45 s") && t.contains("agent_wait") && t.ends_with("working"), "{t}");
        let t = wait_text(&json!({"status": "error", "error": "boom", "final": {"id": "a1"}}), 1);
        assert!(t.contains("boom"));
        let long = "x".repeat(20_000);
        assert!(wait_text(&json!({"status": "idle", "final": {}, "lastMessage": long}), 1).len() < 7000);
    }

    #[test]
    fn diffs_read_as_unified_text() {
        // agentd's shape (captured 2026-10-28).
        let v = json!({"cwd": "/w", "diffTooLarge": false, "error": null, "files": [
            {"additions": 1, "deletions": 0, "isNew": true, "isDeleted": false, "path": "new.txt",
             "hunks": [{"lines": [{"content": "@@ -0,0 +1 @@", "type": "header"}, {"content": "new", "type": "add"}]}]},
            {"additions": 1, "deletions": 1, "isNew": false, "isDeleted": false, "path": "src/a.txt",
             "hunks": [{"lines": [{"content": "@@ -1 +1,2 @@", "type": "header"}, {"content": "hello", "type": "context"},
                                  {"content": "old", "type": "remove"}, {"content": "changed", "type": "add"}]}]}]});
        let t = diff_text(&v, "all", None);
        assert!(t.starts_with("2 file(s) changed, +2 -1 (all) in /w"), "{t}");
        assert!(t.contains("### new.txt (new) +1 -0\n@@ -0,0 +1 @@\n+new\n"), "{t}");
        assert!(t.contains(" hello\n-old\n+changed\n"), "{t}");
        assert!(!diff_text(&v, "all", Some("src/a.txt")).contains("new.txt"));
    }

    #[test]
    fn logs_merge_streamed_chunks() {
        // Timeline rows as agentd streams them: assistant text in chunks.
        let row = |seq: u64, item: Value| json!({"seq": seq, "timestamp": "2026-10-28T08:37:32.738Z", "item": item, "turnId": "t1"});
        let v = json!({"epoch": "e", "nextSeq": 4, "rows": [
            row(1, json!({"type": "user_message", "text": "hello", "messageId": "m0"})),
            row(2, json!({"type": "assistant_message", "text": "## Cycle", "messageId": "m1"})),
            row(3, json!({"type": "assistant_message", "text": " 1 done", "messageId": "m1"})),
        ]});
        let t = logs_text(&v, 0).unwrap();
        assert!(t.contains("[User] hello"), "{t}");
        assert!(t.contains("## Cycle 1 done"), "{t}");
        assert_eq!(logs_text(&json!({"rows": []}), 0).unwrap(), "(nothing in the log yet)");
        assert!(logs_text(&json!({"rows": [{"bad": 1}]}), 0).is_err());
    }

    #[test]
    fn usage_sums_by_provider() {
        let agents = json!({"agents": [
            {"provider": "claude", "status": "running", "lastUsage": {"inputTokens": 10, "outputTokens": 5}},
            {"provider": "claude", "status": "idle", "lastUsage": {"inputTokens": 1, "outputTokens": 1, "totalCostUsd": 0.5}},
            {"provider": "codex", "status": "idle"}]});
        let t = usage_text(&agents, &json!([{"id": "claude"}, {"id": "zcode"}]));
        assert!(t.contains("- claude: 2 agent(s), 1 running, 11 input / 6 output tokens, $0.50"), "{t}");
        assert!(t.contains("zcode (not from the MCP)"), "{t}");
    }

    #[test]
    fn audit_lines_hold_no_prompt_text() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = JsonObject::new();
        args.insert("prompt".into(), json!("deploy the secret sauce"));
        args.insert("agent_id".into(), json!("abc"));
        audit(dir.path(), "agent_send", &caller(0), &args, Ok(Some("abc-full")));
        audit(dir.path(), "agent_run", &caller(1), &args, Err("depth"));
        let raw = std::fs::read_to_string(dir.path().join(AUDIT_FILE)).unwrap();
        assert!(!raw.contains("secret sauce"));
        let lines: Vec<Value> = raw.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["target"], "abc-full");
        assert_eq!(lines[0]["promptChars"], 23);
        assert_eq!(lines[1]["ok"], false);
        assert_eq!(lines[1]["depth"], 1);
        assert_eq!(lines[1]["target"], "abc");
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join(AUDIT_FILE)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
