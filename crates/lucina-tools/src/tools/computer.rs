//! `computer`: Lucina's own isolated computers on nezopt (Cua sandboxes): the
//! Linux desktop `lucina-workspace` (browser, terminal, files) and the Android
//! `lucina-android`. Registered by the server when
//! `LUCINA_COMPUTER_USE_ENABLED=true` (crate `lucina_computer_use`).
//!
//! The desktop is a sandbox container on nezopt (not nezopt's host, not the
//! owner's Mac) and its only browser is Chromium; the sandbox layer refuses
//! Firefox launches and installs.
//!
//! `task` hands a whole job to her computer-use engine (Hermes + Cua, capped at
//! 200 s) and returns its receipts; the other actions call the sandbox directly.
//! Nothing here touches the host: the sandboxes have no host mounts and guest
//! paths naming host locations are refused.

use lucina_computer_use::{control, image_of, run_task, text_of, Cancel, Sandbox, SandboxId};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

/// Longest text a call returns to the model.
const MAX_TEXT: usize = 12_000;

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Action {
    Task,
    Bash,
    Screenshot,
    Tools,
    Tool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Which {
    Workspace,
    Android,
}

#[derive(Deserialize, JsonSchema)]
struct Args {
    /// `task`: describe a whole job in words; your computer-use engine does it on your Linux desktop and reports what it did (best for browsing, downloads, multi-step work). `bash`: one shell command in the sandbox. `screenshot`: what is on the screen now (windows and their state). `tools`: list the desktop's driver tools. `tool`: call one driver tool (click, type_text, press_key, launch_app, get_accessibility_tree; browser_* tools need start_session first). Chromium only; never install or launch Firefox.
    action: Action,
    /// For `task`: the job, e.g. "open example.com and tell me the page title".
    task: Option<String>,
    /// For `bash`: the command (runs as user `cua` inside the sandbox).
    command: Option<String>,
    /// For `tool`: the driver tool name from `tools`.
    tool: Option<String>,
    /// For `tool`: the tool's arguments.
    arguments: Option<Map<String, Value>>,
    /// Which computer: `workspace` (Linux desktop, default) or `android`.
    sandbox: Option<Which>,
}

fn clip(s: String) -> String {
    if s.chars().count() <= MAX_TEXT {
        return s;
    }
    s.chars().take(MAX_TEXT).collect::<String>() + "\n[truncated]"
}

fn need<'a>(v: &'a Option<String>, what: &str) -> Result<&'a str, ToolError> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ToolError::new(format!("{what} is required for this action")))
}

pub struct ComputerTool;

#[async_trait::async_trait]
impl Tool for ComputerTool {
    fn name(&self) -> &'static str {
        "computer"
    }

    fn description(&self) -> &'static str {
        "Your own isolated computers on nezopt: a Linux desktop (lucina-workspace, with Chromium, a terminal and files) and an Android sandbox (lucina-android). Use it yourself whenever a request needs a real browser or desktop: visiting a website, downloading and inspecting a file, filling a web page, checking how something looks. Prefer action `task` for multi-step work; use `bash`, `screenshot` and `tool` for quick direct steps. Nothing here can reach Giuseppe's own machines or files. The desktop is a sandbox container on nezopt, not nezopt's host and not Giuseppe's Mac. Its only browser is Chromium: for web pages prefer action `tool` with the driver's browser_* tools (start_session first, then browser_navigate), or click/type_text/press_key. Never install or launch Firefox."
    }

    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }

    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }

    fn summarize(&self, args: &Value) -> String {
        let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("");
        let what = [s("task"), s("command"), s("tool")]
            .into_iter()
            .find(|v| !v.is_empty())
            .unwrap_or("");
        let what: String = what.chars().take(120).collect();
        format!("computer {} {what}", s("action"))
    }

    async fn call(&self, args: Value, _ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let (sb, id) = match a.sandbox.unwrap_or(Which::Workspace) {
            Which::Workspace => (Sandbox::workspace(), SandboxId::Workspace),
            Which::Android => (Sandbox::android(), SandboxId::Android),
        };
        // While the owner drives from the live view, only looking is allowed.
        let mutating = matches!(a.action, Action::Bash | Action::Tool);
        let err = |e: lucina_computer_use::Error| ToolError::new(format!("{}: {e}", e.code()));
        if mutating {
            control().agent_call(id, "lucina").map_err(err)?;
        }
        match a.action {
            Action::Task => {
                let task = need(&a.task, "task")?;
                let r = run_task("lucina", task, &Cancel::new()).await.map_err(err)?;
                Ok(r.to_json())
            }
            Action::Bash => {
                let cmd = need(&a.command, "command")?;
                let r = sb.bash(cmd, None).await.map_err(err)?;
                Ok(json!({"sandbox": sb.space(), "output": clip(text_of(&r))}))
            }
            Action::Screenshot => {
                let r = sb
                    .call_tool("get_desktop_state", json!({}))
                    .await
                    .map_err(err)?;
                Ok(json!({
                    "sandbox": sb.space(),
                    "screen": clip(text_of(&r)),
                    "image": image_of(&r).is_some(),
                }))
            }
            Action::Tools => {
                let r = sb.tools(None).await.map_err(err)?;
                Ok(json!({"sandbox": sb.space(), "tools": clip(text_of(&r))}))
            }
            Action::Tool => {
                let tool = need(&a.tool, "tool")?;
                let r = sb
                    .call_tool(tool, Value::Object(a.arguments.unwrap_or_default()))
                    .await
                    .map_err(err)?;
                Ok(json!({
                    "sandbox": sb.space(),
                    "result": clip(text_of(&r)),
                    "image": image_of(&r).is_some(),
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_lists_every_action() {
        let s = ComputerTool.parameters().to_string();
        for a in ["task", "bash", "screenshot", "tools", "tool"] {
            assert!(s.contains(&format!("\"{a}\"")), "{a} missing from {s}");
        }
    }

    #[test]
    fn summary_names_the_action_and_what() {
        let s = ComputerTool.summarize(&json!({"action": "bash", "command": "uname -a"}));
        assert_eq!(s, "computer bash uname -a");
    }

    #[test]
    fn long_output_is_clipped() {
        let out = clip("x".repeat(MAX_TEXT + 10));
        assert!(out.ends_with("[truncated]"));
    }
}
