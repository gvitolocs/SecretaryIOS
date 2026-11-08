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
pub(crate) fn shopping_reply(lang: &str, kind: &str, name: &str) -> String {
    // The item names follow the message's language, the reply is English or Danish, never Italian
    // (owner rule 2026-11-02).
    let lang = if lang == "da" { "da" } else { "en" };
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

/// The items of an add's text; the text itself when nothing is left after splitting.
pub(crate) fn shopping_parts(
    idx: &lucina_grocery::CatalogIndex,
    typed: &str,
) -> Vec<lucina_grocery::ItemPart> {
    let parts = lucina_grocery::split_text_items(idx, typed);
    if parts.is_empty() && !typed.trim().is_empty() {
        return vec![lucina_grocery::ItemPart {
            name: typed.trim().to_owned(),
            quantity: None,
            unit: None,
        }];
    }
    parts
}

/// The items of a chat message's product text: [`shopping_parts`], each cleaned the way the
/// intent parser cleans a single product (`clean_product_text`: a leading article, "at Netto",
/// "today"), so "togli il latte e le uova" names "latte" and "uova". Grammar only: the names
/// are still matched exactly.
pub(crate) fn chat_parts(
    idx: &lucina_grocery::CatalogIndex,
    typed: &str,
) -> Vec<lucina_grocery::ItemPart> {
    shopping_parts(idx, typed)
        .into_iter()
        .map(|mut p| {
            let clean = lucina_intent::grocery::clean_product_text(&p.name);
            if !clean.is_empty() {
                p.name = clean;
            }
            p
        })
        .collect()
}

/// "4 Baguette", "500 g Burro", "Ricotta".
fn part_label(p: &lucina_grocery::ItemPart, name: &str) -> String {
    match (p.quantity, &p.unit) {
        (Some(q), Some(u)) => format!("{q} {u} {name}"),
        (Some(q), None) => format!("{q} {name}"),
        _ => name.to_owned(),
    }
}

/// "a", "a and b", "a, b and c" (Danish "og").
fn join_names(lang: &str, names: &[String]) -> String {
    let and = if lang == "da" { "og" } else { "and" };
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} {and} {last}", rest.join(", ")),
    }
}

/// The reply to an add of several items: what was added, what was already
/// there, then any question about an item that was not added.
pub(crate) fn add_reply(
    lang: &str,
    added: &[String],
    already: &[String],
    questions: &[String],
) -> String {
    let mut out: Vec<String> = Vec::new();
    if !added.is_empty() {
        out.push(shopping_reply(lang, "added", &join_names(lang, added)));
    }
    match already {
        [] => {}
        [one] => out.push(shopping_reply(lang, "already", one)),
        many => out.push(if lang == "da" {
            format!("{} står allerede på indkøbslisten.", join_names(lang, many))
        } else {
            format!("{} are already on your shopping list.", join_names(lang, many))
        }),
    }
    out.extend(questions.iter().cloned());
    out.join(" ")
}

fn names_push(names: &mut Vec<String>, name: String) {
    if !names.contains(&name) {
        names.push(name);
    }
}

/// The reply to a remove of one or more items: what was removed, then what was not on the list.
pub(crate) fn remove_reply(lang: &str, removed: &[String], missing: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    if !removed.is_empty() {
        out.push(shopping_reply(lang, "removed", &join_names(lang, removed)));
    }
    match missing {
        [] => {}
        [one] => out.push(shopping_reply(lang, "not_found", one)),
        many => out.push(if lang == "da" {
            format!("{} står ikke på indkøbslisten.", join_names(lang, many))
        } else {
            format!("{} are not on your shopping list.", join_names(lang, many))
        }),
    }
    out.join(" ")
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
            // One item per part: "Add to shopping list a, b e 4 c" is three items, never
            // one item made of the sentence (docs/design/shopping-labels.md).
            let idx = repo.catalog_index();
            let parts = chat_parts(&idx, &typed);
            let (mut added, mut already, mut questions) = (Vec::new(), Vec::new(), Vec::new());
            let mut list_id = None;
            for p in &parts {
                let gate =
                    lucina_tools::shopping_gate::check(lucina_jev::shared(), &idx, &p.name, lang)
                        .await;
                if let lucina_tools::shopping_gate::Gate::Ask { question, .. } = &gate {
                    questions.push(question.clone());
                    continue;
                }
                let mut extra = gate.add_op(&p.name).unwrap_or_default();
                if let Some(q) = p.quantity {
                    extra["quantity"] = json!(q);
                }
                if let Some(u) = &p.unit {
                    extra["unit"] = json!(u);
                }
                let row = repo
                    .add_item_with(
                        user_id,
                        &lucina_grocery::exact_canonical_product(&p.name),
                        &p.name,
                        modified_by,
                        Some(lang),
                        &extra,
                    )
                    .await?;
                let label = part_label(p, row["name"].as_str().unwrap_or(&p.name));
                if row["merged"] == json!(true) {
                    already.push(label);
                } else {
                    added.push(label);
                }
                if list_id.is_none() {
                    list_id = row["listId"].as_str().map(str::to_owned);
                }
            }
            Ok(Some(Handled {
                intent: intent.into(),
                reply: add_reply(lang, &added, &already, &questions),
                list_id,
            }))
        }
        "grocery.shopping_list.remove" => {
            let product = product.expect("remove has a product");
            let typed = if parsed.raw_product.is_empty() {
                product.name.clone()
            } else {
                parsed.raw_product.clone()
            };
            let lang = lucina_grocery::message_language(text, None);
            // One name per item, like the add: "togli latte e uova" removes two items, and
            // each is matched exactly (docs/design/shopping-labels.md).
            let parts = chat_parts(&repo.catalog_index(), &typed);
            let (mut removed, mut missing) = (Vec::new(), Vec::new());
            let mut list_id = None;
            for p in &parts {
                let (n, list, names) = repo
                    .remove_items_named_in(user_id, &p.name, modified_by, Some(lang))
                    .await?;
                if n == 0 {
                    missing.push(p.name.clone());
                    continue;
                }
                list_id.get_or_insert(list);
                if names.is_empty() {
                    names_push(&mut removed, p.name.clone());
                }
                for name in names {
                    names_push(&mut removed, name);
                }
            }
            if parts.is_empty() {
                missing.push(typed);
            }
            Ok(Some(Handled {
                intent: intent.into(),
                reply: remove_reply(lang, &removed, &missing),
                list_id,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> lucina_grocery::CatalogIndex {
        let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/catalog/catalog.v1.json"))
            .expect("catalog");
        lucina_grocery::CatalogIndex::build(&serde_json::from_str(&raw).expect("catalog json"))
    }

    /// The message that made one item of a sentence on 2026-11-03.
    #[test]
    fn the_owners_message_adds_five_items() {
        let text = "Add to shopping list patatine fritte, ricotta, redbull, mortadella e 4 baguettes.";
        let parsed = lucina_intent::grocery::parse_grocery_intent(text).expect("grocery intent");
        assert_eq!(parsed.intent, "grocery.shopping_list.add");
        let idx = catalog();
        let parts = chat_parts(&idx, &parsed.raw_product);
        let got: Vec<(String, Option<u32>, Option<String>)> = parts
            .iter()
            .map(|p| (p.name.clone(), p.quantity, idx.find(&p.name).map(|i| i.id.clone())))
            .collect();
        let want: Vec<(String, Option<u32>, Option<String>)> = [
            ("patatine fritte", None, "french-fries"),
            ("ricotta", None, "ricotta"),
            ("redbull", None, "energy-drink"),
            ("mortadella", None, "mortadella"),
            ("baguettes", Some(4), "baguette"),
        ]
        .iter()
        .map(|(n, q, id)| (n.to_string(), *q, Some(id.to_string())))
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn one_item_stays_one_item() {
        let idx = catalog();
        for (text, name) in [("add milk to my shopping list", "milk"), ("aggiungi Pane di segale tostato alla lista", "Pane di segale tostato")] {
            let parsed = lucina_intent::grocery::parse_grocery_intent(text).expect(text);
            let parts = chat_parts(&idx, &parsed.raw_product);
            assert_eq!(parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), [name], "{text}");
        }
    }

    /// A remove is split like an add: one exact name per item, never the sentence.
    #[test]
    fn a_remove_names_each_item() {
        let idx = catalog();
        for (text, want) in [
            ("togli latte e uova dalla lista della spesa", vec!["latte", "uova"]),
            ("togli il latte e le uova dalla lista della spesa", vec!["latte", "uova"]),
            ("remove milk, eggs and 2 baguettes from my shopping list", vec!["milk", "eggs", "baguettes"]),
            ("remove Red Bull from the list", vec!["Red Bull"]),
        ] {
            let parsed = lucina_intent::grocery::parse_grocery_intent(text).expect(text);
            assert_eq!(parsed.intent, "grocery.shopping_list.remove", "{text}");
            let parts = chat_parts(&idx, &parsed.raw_product);
            assert_eq!(parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), want, "{text}");
        }
        // A Danish remove reaches this arm through Jev; its text splits the same way.
        let parts = chat_parts(&idx, "mælk og æg");
        assert_eq!(parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["mælk", "æg"]);
    }

    #[test]
    fn remove_replies_name_what_went_and_what_was_missing() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            remove_reply("it", &names(&["Latte", "Uova"]), &[]),
            "Removed Latte and Uova from your shopping list."
        );
        assert_eq!(
            remove_reply("en", &names(&["Milk"]), &names(&["kiwi", "lime"])),
            "Removed Milk from your shopping list. kiwi and lime are not on your shopping list."
        );
        assert_eq!(
            remove_reply("da", &[], &names(&["kiwi"])),
            "kiwi står ikke på indkøbslisten."
        );
    }

    #[test]
    fn replies_name_every_item() {
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            add_reply("it", &names(&["Patatine fritte", "Ricotta", "4 Baguette"]), &names(&["Mortadella"]), &[]),
            "Added Patatine fritte, Ricotta and 4 Baguette to your shopping list. Mortadella is already on your shopping list."
        );
        assert_eq!(
            add_reply("da", &names(&["Mælk", "6 Æg"]), &names(&["Kaffe", "Te"]), &names(&["Hvad mente du?"])),
            "Jeg har tilføjet Mælk og 6 Æg til indkøbslisten. Kaffe og Te står allerede på indkøbslisten. Hvad mente du?"
        );
        let p = lucina_grocery::ItemPart { name: "burro".into(), quantity: Some(500), unit: Some("g".into()) };
        assert_eq!(part_label(&p, "Burro"), "500 g Burro");
    }
}
