//! `about_me`: Lucina's memory about Giuseppe ("About you"). She looks things up in it and keeps it
//! true herself: a new fact he tells her, a change, something that stopped holding, something that was
//! wrong. The summary of what holds is already in her context every turn; this is for detail, history
//! and changes. Owner turns only; never a secret.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, Origin, SideEffect, Tool, ToolError, ToolResult};

/// The server's store.
#[async_trait::async_trait]
pub trait AboutMemory: Send + Sync {
    /// `action`: lookup, add, update, confirm, end, retract. `Err`: why, for the model.
    async fn act(&self, session_id: &str, action: &str, body: Value) -> Result<Value, String>;
}

#[derive(Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Action {
    /// Find facts by words (empty: all that hold). `history: true` adds what no longer holds.
    Lookup,
    /// Something new about him.
    Add,
    /// A fact changed: new text, category or how long it holds.
    Update,
    /// He said it again, or it is clearly still true.
    Confirm,
    /// It held and stopped holding (moved, finished, ended).
    End,
    /// It was never true ("that's wrong").
    Retract,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Args {
    action: Action,
    /// The fact's id, e.g. af-0123456789abcdef (update, confirm, end, retract).
    #[serde(default)]
    id: Option<String>,
    /// The fact in one sentence about him, third person: "He studies X at Aarhus University." Never a password, key or number like an ID.
    #[serde(default)]
    text: Option<String>,
    /// identity, education, work, money, health, home, people, preferences, schedule or other.
    #[serde(default)]
    category: Option<String>,
    /// The last day it holds, YYYY-MM-DD, when it has one.
    #[serde(default)]
    valid_until: Option<String>,
    /// What ends it, when it holds until something happens: "until he graduates".
    #[serde(default)]
    valid_until_condition: Option<String>,
    /// Lookup: words to look for.
    #[serde(default)]
    query: Option<String>,
    /// Lookup: include what no longer holds.
    #[serde(default)]
    history: bool,
    /// End or retract: why, in a few words.
    #[serde(default)]
    reason: Option<String>,
}

pub struct AboutMeTool {
    pub memory: Arc<dyn AboutMemory>,
}

#[async_trait::async_trait]
impl Tool for AboutMeTool {
    fn name(&self) -> &'static str {
        "about_me"
    }
    fn description(&self) -> &'static str {
        "Your memory about Giuseppe as a person (studies, work, money, health, home, people, preferences, schedule). What \
         holds is already in your context; use this to look up detail or history, and to keep it true yourself: add what he \
         tells you about himself, update what changed, end what stopped holding (with the date it holds until when there is \
         one), retract what he says is wrong. One fact per call, a short sentence in the third person. Never a secret: those \
         go to Infisical."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn summarize(&self, args: &Value) -> String {
        format!(
            "about_me {} {}",
            args["action"].as_str().unwrap_or(""),
            args["id"].as_str().unwrap_or("")
        )
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        if ctx.origin != Origin::Owner {
            return Err(ToolError::new(
                "only the owner's own turns read or change the memory about him",
            ));
        }
        let a: Args = super::args(args)?;
        let action = match a.action {
            Action::Lookup => "lookup",
            Action::Add => "add",
            Action::Update => "update",
            Action::Confirm => "confirm",
            Action::End => "end",
            Action::Retract => "retract",
        };
        let body = json!({
            "id": a.id, "text": a.text, "category": a.category, "validUntil": a.valid_until,
            "validUntilCondition": a.valid_until_condition, "query": a.query, "history": a.history, "reason": a.reason,
        });
        self.memory
            .act(&ctx.session_id, action, body)
            .await
            .map_err(ToolError::new)
    }
}
