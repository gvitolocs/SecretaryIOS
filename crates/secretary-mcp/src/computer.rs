//! `computer_*` and `android_*`: Lucina's own computers, driven directly by
//! the owner's agents (secretary-server `/v1/computer/*` → Lucina).
//!
//! - `computer_task`: a whole job for Lucina's computer-use engine on her
//!   Linux desktop. A task runs up to 200 s and MCP clients give up near two
//!   minutes, so it starts a job and waits at most `wait_seconds` (≤ 50);
//!   `computer_task_status` waits for it again, `computer_task_cancel` stops it.
//! - `computer_bash` / `computer_screenshot` / `computer_tools` /
//!   `computer_call` on the desktop, and the same four as `android_*` on the
//!   Android sandbox.
//!
//! Scope `computer`, which only the owner grants (consent page or
//! `secretary-mcp token|oauth add-scope`); agent-scoped tokens never get it.
//! The sandboxes are isolated containers with no host mounts, so the tools are
//! annotated as non-destructive, as the owner wants them on "Always allow".
//! While the owner drives a sandbox from the apps' live view, every mutating
//! call is refused (`owner_driving`); screenshots still work.

use std::time::{Duration, Instant};

use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Method;
use rmcp::model::{ContentBlock, JsonObject};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::backend::{BResult, Backend};
use crate::tools::{ToolError, Tools};

/// Longest a single tool call waits on a job.
pub const MAX_WAIT_SECS: u64 = 50;
const POLL_EVERY: Duration = Duration::from_secs(3);
/// Text returned to the model from one call.
const MAX_TEXT: usize = 12_000;
const CALL_TIMEOUT: Duration = Duration::from_secs(125);

/// Arguments of `computer_task`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskArgs {
    /// The whole job in words, e.g. "open example.com in Chromium and tell me the page title".
    pub task: String,
    /// Seconds to wait for the result before returning the job id (0-50, default 45).
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

/// Arguments of `computer_task_status` / `computer_task_cancel`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JobArgs {
    /// The job id `computer_task` returned.
    pub job_id: String,
    /// `computer_task_status` only: seconds to wait for it to finish (0-50, default 45).
    #[serde(default)]
    pub wait_seconds: Option<u64>,
}

/// Arguments of `computer_bash` / `android_bash`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BashArgs {
    /// One shell command, run as user `cua` inside the sandbox.
    pub command: String,
    /// Seconds before the command is stopped (default: the sandbox's own limit).
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

/// Arguments of `computer_tools` / `android_tools`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolsArgs {
    /// A substring of a tool name, to get the full input schemas of the matching tools.
    #[serde(default)]
    pub name: Option<String>,
}

/// Arguments of `computer_call` / `android_call`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CallArgs {
    /// A driver tool from `computer_tools`, e.g. click, type_text, press_key, scroll, launch_app, get_window_state, browser_navigate (browser_* tools need start_session first). The workspace uses Chromium only; never install or launch Firefox. Android is separate.
    pub tool: String,
    /// The tool's arguments. Most take the pid and window_id that a screenshot lists.
    #[serde(default)]
    pub arguments: Option<JsonObject>,
}

/// No arguments.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError(msg.into())
}

fn parse<T: serde::de::DeserializeOwned>(args: &JsonObject) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(args.clone())).map_err(|e| bad(format!("Invalid arguments: {e}")))
}

fn clip(s: &str) -> String {
    if s.chars().count() <= MAX_TEXT {
        return s.to_owned();
    }
    s.chars().take(MAX_TEXT).collect::<String>() + "\n[truncated]"
}

fn job_id_ok(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn wait(v: Option<u64>) -> Duration {
    Duration::from_secs(v.unwrap_or(45).min(MAX_WAIT_SECS))
}

/// A refusal the model can act on.
fn refusal(e: crate::backend::BackendError) -> ToolError {
    match e.code.as_str() {
        "owner_driving" => bad(
            "The owner is driving this computer from the Secretary app's live view right now. Nothing was done; try again after they give it back (computer_screenshot still works).",
        ),
        "busy" => bad("Lucina's desktop is already running another computer task; wait for it (computer_task_status) or cancel it."),
        "quota_exhausted" => bad("Lucina's computer-use model is out of quota right now; no task was run. Try again later, or drive the desktop yourself with computer_call."),
        "computer_use_disabled" | "not_enabled" => bad("Lucina's computer-use engine is switched off on this server."),
        "job_not_found" => bad("No such computer job (jobs are kept for an hour)."),
        _ => e.into(),
    }
}

impl Backend {
    /// A `/v1/computer/*` call named as the MCP client (`X-Secretary-Caller: mcp:<name>`).
    pub async fn computer(&self, method: Method, path: &str, body: Option<&Value>, caller: &str) -> BResult<Value> {
        let mut h = HeaderMap::new();
        if let Ok(v) = HeaderValue::from_str(&format!("mcp:{caller}")) {
            h.insert("x-secretary-caller", v);
        }
        self.send_device(method, path, body, CALL_TIMEOUT, h).await
    }
}

/// The sandbox a tool acts on.
fn sandbox_of(name: &str) -> &'static str {
    if name.starts_with("android_") {
        "android"
    } else {
        "workspace"
    }
}

fn format_job(v: &Value) -> String {
    let id = v["jobId"].as_str().unwrap_or("?");
    let secs = v["elapsedMs"].as_u64().unwrap_or(0) / 1000;
    match v["status"].as_str().unwrap_or("") {
        "running" => format!(
            "Still running on Lucina's desktop (job {id}, {secs} s so far). Call computer_task_status with job_id \"{id}\" to wait for it, or computer_screenshot to watch."
        ),
        "done" => {
            let r = &v["result"];
            let status = r["status"].as_str().unwrap_or("unknown");
            let reply = r["reply"].as_str().unwrap_or("").trim();
            let tools: Vec<&str> = r["executedTools"]
                .as_array()
                .map(|a| a.iter().filter_map(|t| t.as_str().or_else(|| t["name"].as_str())).collect())
                .unwrap_or_default();
            let mut out = format!("Computer task {status} in {secs} s (job {id}).");
            if !tools.is_empty() {
                out.push_str(&format!(" Observed tool calls: {}.", tools.join(", ")));
            }
            if status != "completed" {
                out.push_str(" Not every step was confirmed: check with computer_screenshot.");
            }
            if !reply.is_empty() {
                out.push_str("\n\n");
                out.push_str(&clip(reply));
            }
            out
        }
        "cancelled" => format!("Job {id} was cancelled after {secs} s."),
        _ => {
            let code = v["error"]["code"].as_str().unwrap_or("failed");
            let msg = v["error"]["message"].as_str().unwrap_or("");
            if code == "owner_driving" {
                format!("Job {id} stopped after {secs} s: the owner took over the desktop from the live view.")
            } else {
                format!("Job {id} failed after {secs} s ({code}): {msg}")
            }
        }
    }
}

impl Tools<'_> {
    async fn computer_req(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value, ToolError> {
        self.app
            .backend
            .computer(method, path, body, &self.caller.name)
            .await
            .map_err(refusal)
    }

    /// Polls a job until it ends or `limit` passes; `tick` runs between polls.
    async fn wait_job(&self, id: &str, limit: Duration, tick: &mut (dyn FnMut(u64) + Send)) -> Result<Value, ToolError> {
        let started = Instant::now();
        loop {
            let v = self.computer_req(Method::GET, &format!("/v1/computer/task/{id}"), None).await?;
            if v["status"].as_str() != Some("running") || started.elapsed() + POLL_EVERY > limit {
                return Ok(v);
            }
            tokio::time::sleep(POLL_EVERY).await;
            tick(started.elapsed().as_secs());
        }
    }

    /// Every `computer_*` / `android_*` tool. `tick` reports progress while waiting.
    pub async fn computer(
        &self,
        name: &str,
        args: &JsonObject,
        tick: &mut (dyn FnMut(u64) + Send),
    ) -> Result<Vec<ContentBlock>, ToolError> {
        let sandbox = sandbox_of(name);
        let text = |s: String| Ok(vec![ContentBlock::text(s)]);
        match name {
            "computer_task" => {
                let a: TaskArgs = parse(args)?;
                let task = a.task.trim();
                if task.is_empty() || task.chars().count() > 16_000 {
                    return Err(bad("`task` must be 1-16000 characters"));
                }
                let v = self
                    .computer_req(Method::POST, "/v1/computer/task", Some(&json!({"task": task})))
                    .await?;
                let id = v["jobId"].as_str().filter(|i| job_id_ok(i)).ok_or_else(|| bad("Lucina returned no job id"))?;
                let v = self.wait_job(id, wait(a.wait_seconds), tick).await?;
                text(format_job(&v))
            }
            "computer_task_status" | "computer_task_cancel" => {
                let a: JobArgs = parse(args)?;
                if !job_id_ok(&a.job_id) {
                    return Err(bad("`job_id` is the id computer_task returned"));
                }
                let v = if name == "computer_task_cancel" {
                    self.computer_req(Method::POST, &format!("/v1/computer/task/{}/cancel", a.job_id), Some(&json!({})))
                        .await?;
                    self.wait_job(&a.job_id, Duration::from_secs(20), tick).await?
                } else {
                    self.wait_job(&a.job_id, wait(a.wait_seconds), tick).await?
                };
                text(format_job(&v))
            }
            "computer_bash" | "android_bash" => {
                let a: BashArgs = parse(args)?;
                let cmd = a.command.trim();
                if cmd.is_empty() || cmd.len() > 8000 {
                    return Err(bad("`command` must be 1-8000 bytes"));
                }
                let mut body = json!({"sandbox": sandbox, "op": "bash", "command": cmd});
                if let Some(t) = a.timeout_seconds {
                    body["timeoutSecs"] = json!(t.clamp(1, 110));
                }
                let v = self.computer_req(Method::POST, "/v1/computer/call", Some(&body)).await?;
                text(clip(v["text"].as_str().unwrap_or("")))
            }
            "computer_screenshot" | "android_screenshot" => {
                let _: NoArgs = parse(args)?;
                let v = self
                    .computer_req(Method::POST, "/v1/computer/call", Some(&json!({"sandbox": sandbox, "op": "screenshot"})))
                    .await?;
                let mut out = Vec::new();
                if let (Some(data), mime) = (
                    v["image"]["data"].as_str(),
                    v["image"]["mimeType"].as_str().unwrap_or("image/png"),
                ) {
                    out.push(ContentBlock::image(data, mime));
                }
                out.push(ContentBlock::text(clip(v["text"].as_str().unwrap_or("No window list."))));
                Ok(out)
            }
            "computer_tools" | "android_tools" => {
                let a: ToolsArgs = parse(args)?;
                let mut body = json!({"sandbox": sandbox, "op": "tools"});
                if let Some(n) = a.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                    if n.len() > 64 {
                        return Err(bad("`name` is a short substring"));
                    }
                    body["tool"] = json!(n);
                }
                let v = self.computer_req(Method::POST, "/v1/computer/call", Some(&body)).await?;
                text(clip(v["text"].as_str().unwrap_or("")))
            }
            "computer_call" | "android_call" => {
                let a: CallArgs = parse(args)?;
                let tool = a.tool.trim();
                if tool.is_empty() || tool.len() > 64 || !tool.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    return Err(bad("`tool` is a driver tool name from computer_tools"));
                }
                let body = json!({
                    "sandbox": sandbox,
                    "op": "tool",
                    "tool": tool,
                    "arguments": a.arguments.unwrap_or_default(),
                });
                let v = self.computer_req(Method::POST, "/v1/computer/call", Some(&body)).await?;
                let mut out = Vec::new();
                if let Some(data) = v["image"]["data"].as_str() {
                    out.push(ContentBlock::image(data, v["image"]["mimeType"].as_str().unwrap_or("image/png")));
                }
                out.push(ContentBlock::text(clip(v["text"].as_str().unwrap_or("done"))));
                Ok(out)
            }
            _ => Err(bad(format!("unknown tool {name}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jobs_read_well() {
        let running = json!({"jobId": "abc", "status": "running", "elapsedMs": 45000});
        assert!(format_job(&running).contains("computer_task_status"));
        let done = json!({"jobId": "abc", "status": "done", "elapsedMs": 61000,
            "result": {"status": "completed", "reply": "The title is Example Domain.", "executedTools": ["terminal", {"name": "get_desktop_state"}]}});
        let t = format_job(&done);
        assert!(t.contains("completed in 61 s"), "{t}");
        assert!(t.contains("terminal, get_desktop_state"));
        assert!(t.ends_with("The title is Example Domain."));
        let partial = json!({"jobId": "abc", "status": "done", "elapsedMs": 1, "result": {"status": "partial", "reply": ""}});
        assert!(format_job(&partial).contains("computer_screenshot"));
        let stopped = json!({"jobId": "abc", "status": "failed", "elapsedMs": 9000, "error": {"code": "owner_driving", "message": "x"}});
        assert!(format_job(&stopped).contains("took over"));
    }

    #[test]
    fn sandboxes_and_ids() {
        assert_eq!(sandbox_of("android_call"), "android");
        assert_eq!(sandbox_of("computer_call"), "workspace");
        assert!(job_id_ok("0123abcdef"));
        assert!(!job_id_ok("../x"));
        assert!(!job_id_ok(""));
        assert_eq!(wait(Some(500)), Duration::from_secs(MAX_WAIT_SECS));
        assert_eq!(wait(None), Duration::from_secs(45));
    }
}
