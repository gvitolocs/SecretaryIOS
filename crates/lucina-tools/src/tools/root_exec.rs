//! `root_exec`: Lucina asks to run commands as root on the nezopt host.
//!
//! Nothing runs until Giuseppe approves the exact commands on his phone
//! (Face ID). The tool asks at once and waits in the background; the result
//! arrives as an app notification. See the `lucina-approvals` crate. Not
//! offered to guarded MCP callers ([`crate::NO_MCP_TOOLS`]); registered by the
//! server only when approvals are configured (pinned key present).

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

#[derive(Deserialize, JsonSchema)]
struct Args {
    /// Why these commands are needed (one sentence, shown to Giuseppe on the approval prompt).
    reason: String,
    /// The commands in order. Each command is an argv array: the program first, then every
    /// argument as its own string (no shell, no pipes or `&&`; a path with spaces is one string).
    commands: Vec<Vec<String>>,
}

pub struct RootExecTool;

#[async_trait::async_trait]
impl Tool for RootExecTool {
    fn name(&self) -> &'static str {
        "root_exec"
    }
    fn description(&self) -> &'static str {
        "Ask to run commands as root on the nezopt host (systemd units, installing files under /etc, restarting \
         services). Nothing runs until Giuseppe approves these exact commands on his phone; this call returns at \
         once with an approval id and the result is sent to him as a notification. Never claim the commands ran: \
         say you asked for approval. The commands are argv arrays, not shell strings."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Device
    }
    fn summarize(&self, args: &Value) -> String {
        format!(
            "root_exec {} command(s)",
            args["commands"].as_array().map_or(0, Vec::len)
        )
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let approvals = ctx.env.approvals.clone().ok_or_else(|| {
            ToolError::new("root commands are not set up on this host (no approvals configuration)")
        })?;
        let who = lucina_approvals::Who::lucina(format!(
            "root_exec from chat (channel {}, session {})",
            ctx.channel, ctx.session_id
        ));
        let notifier = Notifier::from_env(ctx);
        let spawned = approvals
            .root_spawn(a.commands, a.reason, who, move |report| async move {
                notifier.send(&report).await;
            })
            .await
            .map_err(|e| ToolError::new(e.to_string()))?;
        Ok(json!({
            "status": if spawned.already_waiting { "already_waiting_for_approval" } else { "awaiting_owner_approval" },
            "approvalId": spawned.id,
            "note": "Not run yet. Giuseppe must approve these exact commands on his phone (Face ID); the result \
                     comes to him as a notification. Tell him you asked, and do not say it is done.",
        }))
    }
}

/// Posts the outcome to the owner's apps (`/v1/notify`, category `system`).
struct Notifier {
    http: reqwest::Client,
    base: Option<String>,
    token_file: Option<std::path::PathBuf>,
}

impl Notifier {
    fn from_env(ctx: &Ctx) -> Self {
        Self {
            http: ctx.env.http.clone(),
            base: ctx
                .env
                .secretary_url
                .as_deref()
                .map(|u| u.trim_end_matches('/').to_owned()),
            token_file: ctx.env.secretary_service_token_file.clone(),
        }
    }

    async fn send(&self, report: &lucina_approvals::RootReport) {
        let (Some(base), Some(file)) = (&self.base, &self.token_file) else {
            return;
        };
        let Some(token) = std::fs::read_to_string(file)
            .ok()
            .map(|t| t.trim().to_owned())
            .filter(|t| t.starts_with("svc_"))
        else {
            return;
        };
        let mut body = report.summary();
        if let Some(s) = report.steps.last().filter(|s| !s.ok) {
            let tail = s.stderr.trim();
            if !tail.is_empty() {
                let tail: String = tail
                    .chars()
                    .rev()
                    .take(200)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                body.push_str(&format!(" {tail}"));
            }
        }
        let mut body: String = body.chars().take(480).collect();
        lucina_secrets::redactor().redact_string(&mut body);
        let mut payload = json!({
            "category": "system",
            "title": "Root commands",
            "body": body,
            "route": "/",
        });
        if let Some(id) = &report.approval_id {
            payload["dedupKey"] = json!(format!("lucina:root:{id}"));
        }
        if let Err(e) = self
            .http
            .post(format!("{base}/v1/notify"))
            .bearer_auth(token)
            .json(&payload)
            .send()
            .await
        {
            tracing::warn!(error = %e.without_url(), "root_exec result notification failed");
        }
    }
}
