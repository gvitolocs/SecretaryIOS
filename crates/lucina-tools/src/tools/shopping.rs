//! `shopping`: the owner's shopping list (default list unless another list
//! is named) through the grocery database: list, add, purchase, remove.
//! "purchase" only marks an item as bought; it never buys anything.

use lucina_grocery::catalog::fold;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, Effect, SideEffect, Tool, ToolError, ToolResult};

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Action {
    List,
    Add,
    Purchase,
    Remove,
}

#[derive(Deserialize, JsonSchema)]
struct Args {
    action: Action,
    /// Item names (add, purchase = mark as bought, remove).
    items: Option<Vec<String>>,
    /// List name or id; default: the default list ("Casa").
    list: Option<String>,
}

pub struct ShoppingTool;

async fn resolve_list(ctx: &Ctx, repo: &lucina_grocery::GroceryRepo, wanted: Option<&str>) -> Result<String, ToolError> {
    let Some(w) = wanted.map(str::trim).filter(|s| !s.is_empty() && *s != "default") else {
        return Ok("default".into());
    };
    let lists = repo.list_lists(&ctx.env.grocery_user_id).await.map_err(|e| ToolError::new(e.to_string()))?;
    lists
        .iter()
        .find(|l| l["id"] == json!(w) || fold(l["name"].as_str().unwrap_or("")) == fold(w))
        .and_then(|l| l["id"].as_str().map(str::to_owned))
        .ok_or_else(|| ToolError::new(format!("no shopping list named {w:?}")))
}

fn names(items: &[Value], status: &str) -> Vec<Value> {
    items
        .iter()
        .filter(|i| i["status"] == status)
        .map(|i| json!({"id": i["id"], "name": i["name"], "spec": i["spec"], "quantity": i["quantity"], "unit": i["unit"]}))
        .collect()
}

#[async_trait::async_trait]
impl Tool for ShoppingTool {
    fn name(&self) -> &'static str {
        "shopping"
    }
    fn description(&self) -> &'static str {
        "The owner's shopping list: list items, add items, mark items as bought (purchase: bookkeeping only, never buys anything), remove items."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let repo = ctx.env.grocery.as_ref().ok_or_else(|| ToolError::new("grocery_unavailable: the grocery database is not configured"))?;
        let user = &ctx.env.grocery_user_id;
        let list_id = resolve_list(ctx, repo, a.list.as_deref()).await?;
        let modifier = if matches!(ctx.channel.as_str(), "web" | "ios" | "telegram" | "discord" | "alexa") { ctx.channel.as_str() } else { "lucina" };
        let items: Vec<String> = a.items.unwrap_or_default().into_iter().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).take(50).collect();
        let state = if a.action == Action::List {
            repo.list_state(user, &list_id).await.map_err(|e| ToolError::new(e.to_string()))?
        } else {
            if items.is_empty() {
                return Err(ToolError::new("items is required"));
            }
            let ops: Vec<Value> = if a.action == Action::Add {
                items.iter().map(|n| json!({"opId": uuid::Uuid::new_v4().to_string(), "type": "add", "name": n})).collect()
            } else {
                let current = repo.list_state(user, &list_id).await.map_err(|e| ToolError::new(e.to_string()))?;
                let idx = repo.catalog_index();
                let mut ops = Vec::new();
                let mut missing = Vec::new();
                for n in &items {
                    let cid = idx.find(n).map(|e| e.id.clone());
                    let hit = current["items"].as_array().into_iter().flatten().find(|i| {
                        fold(i["name"].as_str().unwrap_or("")) == fold(n) || (cid.is_some() && i["catalogId"].as_str() == cid.as_deref())
                    });
                    match hit {
                        Some(i) => ops.push(json!({
                            "opId": uuid::Uuid::new_v4().to_string(),
                            "type": if a.action == Action::Purchase { "purchase" } else { "remove" },
                            "itemId": i["id"],
                        })),
                        None => missing.push(n.clone()),
                    }
                }
                if ops.is_empty() {
                    return Err(ToolError::new(format!("not on the list: {}", missing.join(", "))));
                }
                ops
            };
            let out = repo.apply_ops(user, &list_id, &Value::Array(ops), None, modifier).await.map_err(|e| ToolError::new(e.to_string()))?;
            json!({"list": out.list, "revision": out.revision, "items": out.items, "rejected": out.rejected})
        };
        let id = state["list"]["id"].as_str().unwrap_or_default().to_owned();
        if a.action != Action::List {
            ctx.push_effect(Effect::Shopping { list_id: id.clone() });
        }
        let all = state["items"].as_array().cloned().unwrap_or_default();
        Ok(json!({
            "list": {"id": id, "name": state["list"]["name"]},
            "needed": names(&all, "needed"),
            "recentlyBought": names(&all, "recent"),
            "rejected": state.get("rejected").cloned().unwrap_or(json!([])),
        }))
    }
}
