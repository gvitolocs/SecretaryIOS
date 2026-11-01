//! The deterministic grocery path of a chat turn (`src/grocery/index.js`
//! `process`): runs before the planner. Shopping-list replies name the item
//! object in the message's language (it/en/da), the rest is English, as in Node.
//!
//! Not ported yet (port plan phase 6): the live catalog/offer adapters
//! (ShelfAtlas, Salling, chain JSON, eTilbudsavis, RapidAPI) and receipt
//! connectors. Without them a purchase is priced only from the price
//! history, and offer/price questions answer that the live catalog is not
//! configured.

use chrono::Utc;
use lucina_grocery::{GroceryError, GroceryRepo, Price, StoreRow};
use lucina_intent::grocery::{
    decide_store_action, parse_grocery_intent, resolve_store_context, store_from_text, Store,
};
use serde_json::json;

/// A handled grocery turn.
#[derive(Debug, Clone)]
pub struct Handled {
    pub intent: String,
    pub reply: String,
    /// The default list a shopping change touched (timeline `meta.listId`).
    pub list_id: Option<String>,
}

fn row(s: &Store) -> StoreRow {
    StoreRow {
        id: s.id.clone(),
        chain_slug: s.chain_slug.clone(),
        display_name: s.display_name.clone(),
    }
}

fn store(r: &StoreRow) -> Store {
    Store {
        id: r.id.clone(),
        chain_slug: r.chain_slug.clone(),
        display_name: r.display_name.clone(),
    }
}

/// Shopping-list replies in the language of the owner's message (Node `SHOPPING_REPLIES`).
fn shopping_reply(lang: &str, kind: &str, name: &str) -> String {
    match (lang, kind) {
        ("it", "added") => format!("Ho aggiunto {name} alla lista della spesa."),
        ("it", "already") => format!("{name} è già nella lista della spesa."),
        ("it", "removed") => format!("Ho tolto {name} dalla lista della spesa."),
        ("it", "not_found") => format!("{name} non è nella lista della spesa."),
        ("it", "list") => format!("Lista della spesa: {name}."),
        ("it", "empty") => "La lista della spesa è vuota.".into(),
        ("it", "finished") => {
            format!("Ho segnato {name} come finito e l'ho aggiunto alla lista della spesa.")
        }
        ("da", "added") => format!("Jeg har tilføjet {name} til indkøbslisten."),
        ("da", "already") => format!("{name} står allerede på indkøbslisten."),
        ("da", "removed") => format!("Jeg har fjernet {name} fra indkøbslisten."),
        ("da", "not_found") => format!("{name} står ikke på indkøbslisten."),
        ("da", "list") => format!("Indkøbsliste: {name}."),
        ("da", "empty") => "Indkøbslisten er tom.".into(),
        ("da", "finished") => {
            format!("{name} er sluppet op, så jeg har sat det på indkøbslisten.")
        }
        (_, "added") => format!("Added {name} to your shopping list."),
        (_, "already") => format!("{name} is already on your shopping list."),
        (_, "removed") => format!("Removed {name} from your shopping list."),
        (_, "not_found") => format!("{name} is not on your shopping list."),
        (_, "list") => format!("Shopping list: {name}."),
        (_, "empty") => "Your shopping list is empty.".into(),
        (_, _) => format!("Marked {name} as finished and added it to your shopping list."),
    }
}

fn dkk(v: f64) -> String {
    format!("{v:.2} DKK")
}

fn handled(intent: &str, reply: impl Into<String>) -> Option<Handled> {
    Some(Handled {
        intent: intent.into(),
        reply: reply.into(),
        list_id: None,
    })
}

const VERIFIED: [&str; 4] = [
    "receipt_verified",
    "catalog_verified",
    "offer_verified",
    "historical_snapshot",
];

/// `process({userId, text, recentContext, channel})`: `Ok(None)` when the
/// message is not a grocery intent; `Err` when the database failed.
pub async fn process(
    repo: Option<&GroceryRepo>,
    user_id: &str,
    text: &str,
    recent_context: &str,
    channel: &str,
) -> Result<Option<Handled>, GroceryError> {
    let modified_by = if lucina_grocery::MODIFIERS.contains(&channel) {
        channel
    } else {
        "lucina"
    };
    let Some(parsed) = parse_grocery_intent(text) else {
        // A store mention is context for later turns ("I'm going to Føtex").
        if let (Some(repo), Some(s)) = (repo, store_from_text(text)) {
            if lucina_intent::re!(r"\b(going to|at|in|på vej til|er i)\b", "i").test(text) {
                repo.save_store_context(user_id, &row(&s), 0.96, "message_context")
                    .await?;
            }
        }
        return Ok(None);
    };
    let intent = parsed.intent;
    // Deal questions reach here only when the deals engine could not answer
    // (crate::deals: no home coordinates); Node's own reply for that case.
    if intent.starts_with("grocery.deals.") {
        return Ok(handled(
            intent,
            "Nearby deal lookup is unavailable. Check home coordinates and deal sources.",
        ));
    }
    let Some(repo) = repo else {
        return Ok(handled(intent, "Grocery accounting is not configured yet. Set GROCERY_DATABASE_URL and run `npm run grocery:migrate` once."));
    };
    let persisted = repo
        .store_context(user_id)
        .await?
        .map(|(s, c)| (store(&s), c));
    let resolution = resolve_store_context(parsed.store.as_ref(), recent_context, persisted);
    let offer_question = intent == "grocery.offer.search" || intent == "grocery.price.query";
    if parsed.store.is_some() && !offer_question {
        if let Some(s) = &resolution.store {
            repo.save_store_context(user_id, &row(s), resolution.confidence, &resolution.source)
                .await?;
        }
    }
    let product = parsed.product.clone();
    match intent {
        "grocery.purchase" => {
            if parsed.raw_product.is_empty() {
                return Ok(handled(intent, "What did you buy?"));
            }
            if let Some(question) = decide_store_action(&resolution) {
                return Ok(handled(intent, question));
            }
            let product = product.expect("purchase has a product");
            let s = resolution.store.clone().expect("auto store");
            // Prices: history for past purchases (no receipts or catalog adapters here).
            let historical = if parsed.occurred_at.date_naive() < Utc::now().date_naive() {
                repo.historical_price(&product.id, &s.id, parsed.occurred_at)
                    .await?
            } else {
                None
            };
            let Some(mut price) = historical else {
                return Ok(handled(intent, format!(
                    "I found {}, but not a reliable automatic price at {}. Was it a different product or store?",
                    product.name, s.display_name
                )));
            };
            price.verification_status = "historical_snapshot".into();
            price.confidence = 0.9;
            let res_json = json!({"store": {"id": s.id, "chainSlug": s.chain_slug, "displayName": s.display_name}, "confidence": resolution.confidence, "source": resolution.source});
            repo.record_purchase(
                user_id,
                &row(&s),
                parsed.occurred_at,
                &product,
                &parsed.raw_product,
                parsed.quantity,
                parsed.unit.as_deref(),
                &price,
                res_json,
            )
            .await?;
            repo.save_store_context(user_id, &row(&s), resolution.confidence, &resolution.source)
                .await?;
            Ok(handled(
                intent,
                purchase_reply(&product.name, &parsed, &s.display_name, &price),
            ))
        }
        "grocery.consume" | "grocery.finished" => {
            if parsed.raw_product.is_empty() {
                return Ok(handled(intent, "Which pantry item should I update?"));
            }
            let product = product.expect("pantry intent has a product");
            let finished = intent == "grocery.finished";
            let event = if finished { "finished" } else { "consumed" };
            repo.add_pantry_event(
                user_id,
                &product,
                event,
                parsed.quantity,
                parsed.unit.as_deref(),
                parsed.occurred_at,
            )
            .await?;
            if finished {
                let lang = lucina_grocery::message_language(text, None);
                let added = repo
                    .add_item_in(
                        user_id,
                        &product,
                        &parsed.raw_product,
                        modified_by,
                        Some(lang),
                    )
                    .await?;
                let name = added["name"].as_str().unwrap_or(&product.name).to_owned();
                return Ok(Some(Handled {
                    intent: intent.into(),
                    reply: shopping_reply(lang, "finished", &name),
                    list_id: added["listId"].as_str().map(str::to_owned),
                }));
            }
            Ok(handled(
                intent,
                format!("Updated your pantry: {} consumed.", product.name),
            ))
        }
        "grocery.shopping_list.add" => {
            let product = product.expect("add has a product");
            let lang = lucina_grocery::message_language(text, None);
            // Jev checks the item first (TypeSafe; off: added as before).
            let typed = if parsed.raw_product.trim().is_empty() {
                product.name.clone()
            } else {
                parsed.raw_product.clone()
            };
            let gate = lucina_tools::shopping_gate::check(
                lucina_jev::shared(),
                &repo.catalog_index(),
                &typed,
                lang,
            )
            .await;
            if let lucina_tools::shopping_gate::Gate::Ask { question, .. } = &gate {
                return Ok(handled(intent, question.clone()));
            }
            let extra = gate.add_op(&typed).unwrap_or_default();
            let added = repo
                .add_item_with(
                    user_id,
                    &product,
                    &parsed.raw_product,
                    modified_by,
                    Some(lang),
                    &extra,
                )
                .await?;
            let name = added["name"].as_str().unwrap_or(&product.name).to_owned();
            let kind = if added["merged"] == json!(true) {
                "already"
            } else {
                "added"
            };
            Ok(Some(Handled {
                intent: intent.into(),
                reply: shopping_reply(lang, kind, &name),
                list_id: added["listId"].as_str().map(str::to_owned),
            }))
        }
        "grocery.shopping_list.remove" => {
            let product = product.expect("remove has a product");
            let name = if parsed.raw_product.is_empty() {
                product.name.clone()
            } else {
                parsed.raw_product.clone()
            };
            let lang = lucina_grocery::message_language(text, None);
            let (removed, list_id, names) = repo
                .remove_items_named_in(user_id, &name, modified_by, Some(lang))
                .await?;
            let reply = if removed > 0 {
                let shown = if names.is_empty() {
                    product.name.clone()
                } else {
                    names.join(", ")
                };
                shopping_reply(lang, "removed", &shown)
            } else {
                shopping_reply(lang, "not_found", &name)
            };
            Ok(Some(Handled {
                intent: intent.into(),
                reply,
                list_id: (removed > 0).then_some(list_id),
            }))
        }
        "grocery.shopping_list.query" => {
            let lang = lucina_grocery::message_language(text, None);
            let items = repo.legacy_list_in(user_id, Some(lang)).await?;
            let names: Vec<String> = items
                .iter()
                .filter(|i| i["status"] == json!("needed"))
                .take(20)
                .filter_map(|i| {
                    let name = i["name"].as_str()?;
                    Some(match i["spec"].as_str().filter(|s| !s.is_empty()) {
                        Some(spec) => format!("{name} ({spec})"),
                        None => name.to_owned(),
                    })
                })
                .collect();
            Ok(handled(
                intent,
                if names.is_empty() {
                    shopping_reply(lang, "empty", "")
                } else {
                    shopping_reply(lang, "list", &names.join(", "))
                },
            ))
        }
        "grocery.expense.query" | "grocery.analytics.query" => {
            let s = repo.spend_summary(user_id).await?;
            Ok(handled(
                intent,
                format!(
                    "You have spent {} on groceries this month ({} verified; {} inferred).",
                    dkk(s.total_dkk),
                    dkk(s.verified_dkk),
                    dkk(s.inferred_dkk)
                ),
            ))
        }
        "grocery.pantries.query" => {
            let pantry = repo.pantry(user_id).await?;
            Ok(handled(
                intent,
                if pantry.is_empty() {
                    "Your pantry is empty so far.".to_owned()
                } else {
                    format!(
                        "Pantry: {}.",
                        pantry
                            .iter()
                            .take(8)
                            .map(|(p, s)| format!("{p} ({s})"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
            ))
        }
        "grocery.offer.search" | "grocery.price.query" => Ok(handled(
            intent,
            "Live grocery catalog lookup is not configured yet.",
        )),
        _ => Ok(None),
    }
}

fn purchase_reply(
    name: &str,
    parsed: &lucina_intent::grocery::GroceryParse,
    store: &str,
    price: &Price,
) -> String {
    let qualifier = if VERIFIED.contains(&price.verification_status.as_str()) {
        price.verification_status.replacen('_', " ", 1)
    } else {
        "catalog-inferred price".into()
    };
    let quantity = match parsed.quantity {
        Some(q) => format!(" {}{}", js_num(q), parsed.unit.as_deref().unwrap_or("")),
        None => String::new(),
    };
    format!(
        "Added {name}{quantity} from {store} — {} ({qualifier}).",
        dkk(price.price_dkk)
    )
}

fn js_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}
