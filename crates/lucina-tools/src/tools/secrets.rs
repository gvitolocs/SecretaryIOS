//! `secrets_status` and `secrets_audit`: what the owner may know about
//! Lucina's secrets. Names, set or missing, last change (rotation), last
//! use and by which tool, and the audit log. Never a value: there is no tool
//! that returns one. Owner only ([`crate::OWNER_ONLY_TOOLS`]).

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

const NOT_CONFIGURED: &str =
    "secrets are not configured on this host (LUCINA_SECRETS_CONFIG is not set)";

#[derive(Deserialize, JsonSchema)]
struct StatusArgs {}

pub struct SecretsStatusTool;

#[async_trait::async_trait]
impl Tool for SecretsStatusTool {
    fn name(&self) -> &'static str {
        "secrets_status"
    }
    fn description(&self) -> &'static str {
        "Lucina's secrets in Infisical (owner only): each secret's name, whether it is set, when it last \
         changed (rotation), when and by which tool it was last used, and which tool settings use it. \
         It never returns values, and nothing else can: if the owner asks for a password, token or key, \
         say you cannot show secret values and point to Infisical."
    }
    fn parameters(&self) -> Value {
        schema_for::<StatusArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let _: StatusArgs = super::args(args)?;
        let store = ctx
            .env
            .secrets
            .as_ref()
            .ok_or_else(|| ToolError::new(NOT_CONFIGURED))?;
        let items = store
            .status()
            .await
            .map_err(|e| ToolError::new(format!("secrets status unavailable: {e}")))?;
        let scope = &store.config().scope;
        Ok(json!({
            "folder": format!("{}/{}{}", scope.project, scope.environment, scope.path),
            "secrets": items,
            "note": "Names and metadata only; values are never available to Lucina's model.",
        }))
    }
}

#[derive(Deserialize, JsonSchema)]
struct AuditArgs {
    /// How many entries, newest first (default 20, at most 100).
    #[schemars(range(min = 1, max = 100))]
    limit: Option<u32>,
    /// Only this secret name (e.g. "TODOIST_TOKEN").
    name: Option<String>,
    /// Only this tool (e.g. "weather").
    tool: Option<String>,
}

pub struct SecretsAuditTool;

#[async_trait::async_trait]
impl Tool for SecretsAuditTool {
    fn name(&self) -> &'static str {
        "secrets_audit"
    }
    fn description(&self) -> &'static str {
        "The audit log of Lucina's secret use (owner only): when each secret was resolved, by which \
         tool, whether it worked or why not. Names only, never values."
    }
    fn parameters(&self) -> Value {
        schema_for::<AuditArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: AuditArgs = super::args(args)?;
        let store = ctx
            .env
            .secrets
            .as_ref()
            .ok_or_else(|| ToolError::new(NOT_CONFIGURED))?;
        let entries = store.audit_entries(
            a.limit.unwrap_or(20).clamp(1, 100) as usize,
            a.name.as_deref().map(str::trim).filter(|s| !s.is_empty()),
            a.tool.as_deref().map(str::trim).filter(|s| !s.is_empty()),
        );
        Ok(json!({"entries": entries}))
    }
}
