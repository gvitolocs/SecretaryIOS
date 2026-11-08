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

/// One item: a plain name, or a name with its amount and details.
#[derive(Deserialize, JsonSchema, Clone, Debug, PartialEq)]
#[serde(untagged)]
enum ItemArg {
    /// The item's name, e.g. "latte" or "pane integrale". Several items in one
    /// string ("latte, uova e 2 kg di farina") are split into separate items.
    Name(String),
    Detailed {
        /// The item's name, in the singular catalog name when known ("baguette").
        name: String,
        /// Amount (> 0).
        quantity: Option<f64>,
        /// Unit for the amount: "kg", "g", "l", "pz".
        unit: Option<String>,
        /// Free-text detail, e.g. "senza lattosio".
        spec: Option<String>,
    },
}

#[derive(Deserialize, JsonSchema)]
struct Args {
    action: Action,
    /// The items, one entry per item, never command words ("add", "to the
    /// shopping list"). add: names or {name, quantity, unit, spec}; purchase
    /// (mark as bought) and remove: names.
    items: Option<Vec<ItemArg>>,
    /// List name or id; default: the default list ("Casa").
    list: Option<String>,
}

/// One item to add, after splitting.
#[derive(Debug, Clone, PartialEq)]
struct Planned {
    name: String,
    quantity: Option<f64>,
    unit: Option<String>,
    spec: Option<String>,
}

impl Planned {
    /// The grocery `add` op for this item (before the gate adds a catalog link).
    fn op(&self) -> Value {
        let mut op = json!({"opId": uuid::Uuid::new_v4().to_string(), "type": "add", "text": self.name});
        if let Some(q) = self.quantity.filter(|q| q.is_finite() && *q > 0.0) {
            op["quantity"] = json!(q);
        }
        if let Some(u) = self.unit.as_deref().filter(|u| !u.trim().is_empty()) {
            op["unit"] = json!(u.trim());
        }
        if let Some(s) = self.spec.as_deref().filter(|s| !s.trim().is_empty()) {
            op["spec"] = json!(s.trim());
        }
        op
    }
}

/// Items as the add action puts them on the list: each string split into its
/// items (docs/design/shopping-labels.md: never one item made of a sentence),
/// detailed items as given. At most 50.
fn plan_items(idx: &lucina_grocery::CatalogIndex, items: &[ItemArg]) -> Vec<Planned> {
    let mut out = Vec::new();
    for item in items {
        match item {
            ItemArg::Name(text) => {
                for p in lucina_grocery::split_text_items(idx, text) {
                    out.push(Planned {
                        name: p.name,
                        quantity: p.quantity.map(f64::from),
                        unit: p.unit,
                        spec: None,
                    });
                }
            }
            ItemArg::Detailed {
                name,
                quantity,
                unit,
                spec,
            } => {
                let name = name.trim();
                if !name.is_empty() {
                    out.push(Planned {
                        name: name.to_owned(),
                        quantity: *quantity,
                        unit: unit.clone(),
                        spec: spec.clone(),
                    });
                }
            }
        }
    }
    out.truncate(50);
    out
}

fn item_name(item: &ItemArg) -> &str {
    match item {
        ItemArg::Name(n) | ItemArg::Detailed { name: n, .. } => n,
    }
}

pub struct ShoppingTool;

async fn resolve_list(
    ctx: &Ctx,
    repo: &lucina_grocery::GroceryRepo,
    wanted: Option<&str>,
) -> Result<String, ToolError> {
    let Some(w) = wanted
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "default")
    else {
        return Ok("default".into());
    };
    let lists = repo
        .list_lists(&ctx.env.grocery_user_id)
        .await
        .map_err(|e| ToolError::new(e.to_string()))?;
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
        "The owner's shopping list: list items, add items, mark items as bought (purchase: bookkeeping only, never buys anything), remove items. \
         Give each item separately, with its quantity, in the singular catalog name when you know it, and never command words: \
         'add patatine fritte, ricotta e 4 baguettes to the list' -> items [\"patatine fritte\", \"ricotta\", {\"name\": \"baguette\", \"quantity\": 4}]. \
         Items are labels: only an exact catalog name gets a catalog picture and section; anything else stays as the owner wrote it."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let repo = ctx.env.grocery.as_ref().ok_or_else(|| {
            ToolError::new("grocery_unavailable: the grocery database is not configured")
        })?;
        let user = &ctx.env.grocery_user_id;
        let list_id = resolve_list(ctx, repo, a.list.as_deref()).await?;
        let modifier = if matches!(
            ctx.channel.as_str(),
            "web" | "ios" | "telegram" | "discord" | "alexa"
        ) {
            ctx.channel.as_str()
        } else {
            "lucina"
        };
        let args_items = a.items.unwrap_or_default();
        let items: Vec<String> = args_items
            .iter()
            .map(|s| item_name(s).trim().to_owned())
            .filter(|s| !s.is_empty())
            .take(50)
            .collect();
        let mut asks: Vec<Value> = Vec::new();
        let state = if a.action == Action::List {
            repo.list_state(user, &list_id)
                .await
                .map_err(|e| ToolError::new(e.to_string()))?
        } else {
            if items.is_empty() {
                return Err(ToolError::new("items is required"));
            }
            let ops: Vec<Value> = if a.action == Action::Add {
                // Jev checks each item first: implausible ones are asked about, not added.
                let idx = repo.catalog_index();
                let lang = lucina_grocery::message_language(&items.join(", "), None);
                let mut ops = Vec::new();
                for p in plan_items(&idx, &args_items) {
                    let g = crate::shopping_gate::check(lucina_jev::shared(), &idx, &p.name, lang).await;
                    let mut op = p.op();
                    if g.apply_to(&mut op) {
                        ops.push(op);
                    } else if let crate::shopping_gate::Gate::Ask { question, suggestion } = g {
                        asks.push(json!({"item": p.name, "question": question, "suggestion": suggestion}));
                    }
                }
                if ops.is_empty() {
                    return Ok(json!({
                        "added": [],
                        "needsConfirmation": asks,
                        "note": "Nothing was added. Ask the owner the question(s) above, then add what they confirm.",
                    }));
                }
                ops
            } else {
                let current = repo
                    .list_state(user, &list_id)
                    .await
                    .map_err(|e| ToolError::new(e.to_string()))?;
                let idx = repo.catalog_index();
                let mut ops = Vec::new();
                let mut missing = Vec::new();
                for n in &items {
                    let cid = idx.find(n).map(|e| e.id.clone());
                    let hit = current["items"].as_array().into_iter().flatten().find(|i| {
                        fold(i["name"].as_str().unwrap_or("")) == fold(n)
                            || (cid.is_some() && i["catalogId"].as_str() == cid.as_deref())
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
                    return Err(ToolError::new(format!(
                        "not on the list: {}",
                        missing.join(", ")
                    )));
                }
                ops
            };
            let out = repo
                .apply_ops(user, &list_id, &Value::Array(ops), None, modifier)
                .await
                .map_err(|e| ToolError::new(e.to_string()))?;
            json!({"list": out.list, "revision": out.revision, "items": out.items, "rejected": out.rejected})
        };
        let id = state["list"]["id"].as_str().unwrap_or_default().to_owned();
        if a.action != Action::List {
            ctx.push_effect(Effect::Shopping {
                list_id: id.clone(),
            });
        }
        let all = state["items"].as_array().cloned().unwrap_or_default();
        Ok(json!({
            "list": {"id": id, "name": state["list"]["name"]},
            "needed": names(&all, "needed"),
            "recentlyBought": names(&all, "recent"),
            "rejected": state.get("rejected").cloned().unwrap_or(json!([])),
            "needsConfirmation": asks,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_grocery::CatalogIndex;

    fn catalog() -> CatalogIndex {
        let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/catalog/catalog.v1.json"))
            .expect("catalog");
        CatalogIndex::build(&serde_json::from_str(&raw).expect("catalog json"))
    }

    fn args(v: Value) -> Vec<ItemArg> {
        serde_json::from_value(v).expect("items")
    }

    #[test]
    fn a_sentence_becomes_separate_items_with_quantities() {
        let idx = catalog();
        let plan = plan_items(
            &idx,
            &args(json!(["Add to shopping list patatine fritte, ricotta, redbull, mortadella e 4 baguettes."])),
        );
        let got: Vec<(&str, Option<f64>, Option<&str>)> = plan
            .iter()
            .map(|p| (p.name.as_str(), p.quantity, idx.find(&p.name).map(|i| i.id.as_str())))
            .collect();
        assert_eq!(
            got,
            [
                ("patatine fritte", None, Some("french-fries")),
                ("ricotta", None, Some("ricotta")),
                ("redbull", None, Some("energy-drink")),
                ("mortadella", None, Some("mortadella")),
                ("baguettes", Some(4.0), Some("baguette")),
            ]
        );
        for p in &plan {
            assert!(!fold(&p.name).contains("shopping list"), "{p:?}");
        }
    }

    #[test]
    fn detailed_items_are_taken_as_given() {
        let idx = catalog();
        let plan = plan_items(
            &idx,
            &args(json!([{"name": "baguette", "quantity": 4}, {"name": "latte, uova", "spec": "bio"}, "  "])),
        );
        assert_eq!(plan.len(), 2);
        let op = plan[0].op();
        assert_eq!((op["text"].clone(), op["quantity"].clone()), (json!("baguette"), json!(4.0)));
        assert_eq!(plan[1].name, "latte, uova", "a detailed name is the owner's own label");
        assert_eq!(plan[1].op()["spec"], "bio");
    }

    #[test]
    fn units_reach_the_op() {
        let idx = catalog();
        let plan = plan_items(&idx, &args(json!(["500 g burro"])));
        let op = plan[0].op();
        assert_eq!((op["text"].clone(), op["quantity"].clone(), op["unit"].clone()), (json!("burro"), json!(500.0), json!("g")));
    }

    #[test]
    fn no_partial_catalog_links() {
        let idx = catalog();
        for t in ["Redbul", "Pane di segale tostato", "organic milk"] {
            assert!(idx.find(t).is_none(), "{t}");
        }
        assert_eq!(idx.find("Red Bull").map(|i| i.id.as_str()), Some("energy-drink"));
    }
}
