//! `secret_request`: Lucina asks the owner for a secret **by name**. A card opens in his chat in
//! the Secretary app: he pastes the value there, presses OK, and it goes straight to Infisical.
//! The value never comes back here, so it can't reach the model. The server owns the queue (one
//! card at a time) and registers this tool on owner turns only.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

/// Where a request goes: the server's queue of cards.
#[async_trait::async_trait]
pub trait SecretAsker: Send + Sync {
    /// A body of names (never a value). `Err`: why it was refused, for the model.
    async fn ask(&self, session_id: &str, body: Value) -> Result<Value, String>;
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Args {
    /// The secret's name, e.g. GBGO_WIFI_PSK: letters, digits, _ . @ -
    name: String,
    /// The Infisical project, by name or slug.
    project: String,
    /// The environment slug, e.g. prod.
    environment: String,
    /// The folder, default "/".
    #[serde(default)]
    path: Option<String>,
    /// Why it is needed, one line. The owner reads it on the card. Never a value.
    reason: String,
    /// The project doesn't exist yet: the card also offers to create it.
    #[serde(default)]
    new_project: bool,
}

pub struct SecretRequestTool {
    pub asker: Arc<dyn SecretAsker>,
}

#[async_trait::async_trait]
impl Tool for SecretRequestTool {
    fn name(&self) -> &'static str {
        "secret_request"
    }
    fn description(&self) -> &'static str {
        "Ask Giuseppe for a secret (a password, API key, token) by name. He gets a card in the Secretary app, \
         pastes the value there and presses OK; it goes straight to Infisical and never to you, this chat or \
         your memory. Call it once per secret: he sees one card at a time and the next follows. NEVER ask him \
         to write a secret in chat, on any channel: use this tool. Then say in one short sentence that the card \
         is waiting in the app. You will not see the value; a receipt with the name appears after he saves."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    /// Names only.
    fn summarize(&self, args: &Value) -> String {
        format!(
            "secret_request {} {}/{}",
            args["name"].as_str().unwrap_or(""),
            args["project"].as_str().unwrap_or(""),
            args["environment"].as_str().unwrap_or("")
        )
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let mut body = json!({
            "name": a.name, "project": a.project, "environment": a.environment, "reason": a.reason,
            "asking_client": "Lucina", "new_project": a.new_project,
        });
        if let Some(p) = a.path {
            body["path"] = json!(p);
        }
        let answer = self.asker.ask(&ctx.session_id, body).await.map_err(ToolError::new)?;
        let note = match answer["status"].as_str() {
            Some("queued") => "Queued: his card for it follows the one he has open. Tell him the card comes in the app.",
            Some("already_asked") => "He already has this card (or just answered it). Don't ask again.",
            _ => "The card is in his chat in the app. Tell him so in one short sentence; don't ask for the value.",
        };
        Ok(json!({"status": answer["status"], "projectKnown": answer["projectKnown"], "note": note}))
    }
}
