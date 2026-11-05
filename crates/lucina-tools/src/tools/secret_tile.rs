//! `secret_tile`: the owner wants to give Lucina a key, a password or a token, so she opens the
//! "Add secret" card in his chat now (web, Mac and iPhone apps). He pastes the value there and it
//! goes straight to Infisical: no model, chat or memory ever has it. She picks the name and the
//! place; the server fills in a place already in use when she leaves it out. Owner turns only.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, Origin, SideEffect, Tool, ToolError, ToolResult};

/// The server's queue of cards, for the owner's own requests.
#[async_trait::async_trait]
pub trait SecretCardOpener: Send + Sync {
    /// Names only, never a value. `Err`: why it was refused, for the model.
    async fn open(&self, session_id: &str, body: Value) -> Result<Value, String>;
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Args {
    /// The secret's name in UPPER_SNAKE, e.g. GOOGLE_MAPS_SERVER_KEY. When an open card or an earlier secret
    /// already names it, use that name.
    name: String,
    /// The Infisical project, e.g. personal or lucina. Leave it out unless he said one or a place in use fits:
    /// the card then goes where this name, or his own secrets, already live.
    #[serde(default)]
    project: Option<String>,
    /// The environment slug, e.g. prod.
    #[serde(default)]
    environment: Option<String>,
    /// The folder, default "/".
    #[serde(default)]
    path: Option<String>,
}

pub struct SecretCardTool {
    pub opener: Arc<dyn SecretCardOpener>,
}

#[async_trait::async_trait]
impl Tool for SecretCardTool {
    fn name(&self) -> &'static str {
        "secret_tile"
    }
    fn description(&self) -> &'static str {
        "Open the Add secret card in Giuseppe's chat right now, when he wants to give you a key, password or token. \
         He pastes the value in the card and it goes straight to Infisical; you never see it. Pick the name yourself \
         (reuse the name of an open card or an earlier secret when it is the same one) and leave the place out unless \
         you know it. Secrets only ever go to Infisical: never offer an env file, a config file or the chat."
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
            "secret_tile {} {}/{}",
            args["name"].as_str().unwrap_or(""),
            args["project"].as_str().unwrap_or(""),
            args["environment"].as_str().unwrap_or("")
        )
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        if ctx.origin != Origin::Owner {
            return Err(ToolError::new(
                "only the owner's own turns open a secret card",
            ));
        }
        let a: Args = super::args(args)?;
        let body = json!({
            "name": a.name, "project": a.project.unwrap_or_default(),
            "environment": a.environment.unwrap_or_default(), "path": a.path.unwrap_or_default(),
        });
        let mut out = self
            .opener
            .open(&ctx.session_id, body)
            .await
            .map_err(ToolError::new)?;
        out["note"] = json!(
            "The card is open at the bottom of his chat. Tell him in one short sentence to paste it there and where it goes. \
             Don't ask for the value and don't mention any file."
        );
        Ok(out)
    }
}
