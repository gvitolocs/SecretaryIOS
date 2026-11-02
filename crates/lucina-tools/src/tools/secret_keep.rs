//! `secret_keep` and `secret_scan`: Lucina moves a secret she found into Infisical **by reference**.
//! She names a note or a chat message and the secret; a local step on the owner's machine reads the
//! value with the local model, stores it and clears it from the source. Only names and an outcome
//! come back here, so the value never reaches the model. Owner turns only.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, Origin, SideEffect, Tool, ToolError, ToolResult};

/// The server's local step.
#[async_trait::async_trait]
pub trait SecretKeeper: Send + Sync {
    /// Queue a move. A body of references and names, never a value.
    async fn keep(&self, session_id: &str, body: Value) -> Result<Value, String>;
    async fn status(&self, job: &str) -> Result<Value, String>;
    /// Where a secret may be: references only.
    async fn scan(&self, session_id: &str) -> Result<Value, String>;
}

fn owner_only(ctx: &Ctx) -> Result<(), ToolError> {
    if ctx.origin != Origin::Owner {
        return Err(ToolError::new(
            "only the owner's own turns may move secrets",
        ));
    }
    Ok(())
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct KeepArgs {
    /// Where the secret is: note:<slug> or chat:<item id>. From secret_scan, or the note he named.
    #[serde(default)]
    source: Option<String>,
    /// The secret's name, UPPER_SNAKE, e.g. GLM_API_KEY. Without a project it is stored as KEPT_<name>.
    #[serde(default)]
    name: Option<String>,
    /// Which secret in that text, in words, e.g. "the Telegram bot token". Never the value.
    #[serde(default)]
    what: Option<String>,
    /// The Infisical project, only if he said one.
    #[serde(default)]
    project: Option<String>,
    /// The environment slug, only if he said one.
    #[serde(default)]
    environment: Option<String>,
    /// The folder, default "/".
    #[serde(default)]
    path: Option<String>,
    /// The note is nothing but credentials (a list of passwords): store its whole text as one secret.
    #[serde(default)]
    whole_note: bool,
    /// Instead of the above: how an earlier move went.
    #[serde(default)]
    job: Option<String>,
}

pub struct SecretKeepTool {
    pub keeper: Arc<dyn SecretKeeper>,
}

#[async_trait::async_trait]
impl Tool for SecretKeepTool {
    fn name(&self) -> &'static str {
        "secret_keep"
    }
    fn description(&self) -> &'static str {
        "Move a secret that already sits in one of Giuseppe's notes or chat messages into Infisical, and clear it \
         from there. Give a reference and a name; a local step reads it with the local model on his machine, stores \
         it and removes it from the source. You never see the value and never need it: the answer is names and an \
         outcome. It only creates new secrets, so a name that exists with another value fails. If you don't know \
         where it is, call secret_scan first. Don't ask him to paste anything or to confirm: he asked for this. \
         It runs in the background: say it's on its way; a receipt appears in his chat when it's done."
    }
    fn parameters(&self) -> Value {
        schema_for::<KeepArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    /// Names only.
    fn summarize(&self, args: &Value) -> String {
        format!(
            "secret_keep {} {}",
            args["source"].as_str().unwrap_or(""),
            args["name"].as_str().unwrap_or("")
        )
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        owner_only(ctx)?;
        let a: KeepArgs = super::args(args)?;
        if let Some(job) = a.job {
            return self.keeper.status(&job).await.map_err(ToolError::new);
        }
        let source = a
            .source
            .ok_or_else(|| ToolError::new("source is required: note:<slug> or chat:<item id>"))?;
        let name = a.name.ok_or_else(|| ToolError::new("name is required"))?;
        let body = json!({
            "source": source, "name": name, "what": a.what.unwrap_or_default(),
            "project": a.project.unwrap_or_default(), "environment": a.environment.unwrap_or_default(),
            "path": a.path.unwrap_or_default(), "whole": a.whole_note,
        });
        let mut out = self
            .keeper
            .keep(&ctx.session_id, body)
            .await
            .map_err(ToolError::new)?;
        out["note"] = json!("Queued. The local step runs in the background; a receipt appears in his chat. Tell him in one short sentence.");
        Ok(out)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ScanArgs {}

pub struct SecretScanTool {
    pub keeper: Arc<dyn SecretKeeper>,
}

#[async_trait::async_trait]
impl Tool for SecretScanTool {
    fn name(&self) -> &'static str {
        "secret_scan"
    }
    fn description(&self) -> &'static str {
        "Find where a secret may sit in Giuseppe's notes and chat messages. Deterministic patterns on his machine; \
         the answer is references (note:<slug>, chat:<item id>), note titles and why, never the text. Then call \
         secret_keep for each one he wants moved."
    }
    fn parameters(&self) -> Value {
        schema_for::<ScanArgs>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    fn summarize(&self, _args: &Value) -> String {
        "secret_scan".to_owned()
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        owner_only(ctx)?;
        let _: ScanArgs = super::args(args)?;
        self.keeper
            .scan(&ctx.session_id)
            .await
            .map_err(ToolError::new)
    }
}
