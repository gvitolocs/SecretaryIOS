//! Jev's check of every item Lucina puts on the shopping list.
//!
//! One TypeSafe Jev call per item decides, as typed answers:
//! - is it something for the home shopping list (food, drinks, household
//!   consumables, personal care, baby and pet supplies) at all;
//! - its catalog section;
//! - which catalog item it is, if any. Catalog ids are also the keys of the
//!   item illustrations (`assets/icons/items/<id>.svg`), so a bound item gets
//!   its picture in every client.
//!
//! An exact catalog name or alias needs no call. An implausible item is not
//! added: Lucina asks, and offers the closest catalog item ("Lavatrici" →
//! "Detersivo per lavatrice?"). When Jev is off or failing, items are added
//! as before.

use lucina_grocery::catalog::{fold, CatalogIndex};
use lucina_jev::{Jev, Question};
use serde_json::{json, Value};

/// Below this "is it a shopping item?" probability Lucina asks instead of adding.
pub const PLAUSIBLE_MIN: f64 = 0.4;
/// A free-text item is bound to a catalog item from this confidence up.
pub const BIND_MIN: f64 = 0.75;
/// A section is set from this confidence up.
pub const SECTION_MIN: f64 = 0.5;
/// A closest item is suggested from this confidence up.
pub const SUGGEST_MIN: f64 = 0.3;

/// What to do with one item.
#[derive(Debug, Clone, PartialEq)]
pub enum Gate {
    /// Add it, bound to a catalog item and/or in a section when known.
    Add {
        catalog_id: Option<String>,
        section: Option<String>,
        /// How it was decided: `catalog` (exact name), `jev`, `fallback`.
        by: &'static str,
    },
    /// Not added: the question for the owner, and the closest catalog item.
    Ask {
        question: String,
        suggestion: Option<String>,
    },
}

impl Gate {
    /// The `add` op of `lucina_grocery` for `text` (the owner's words stay the typed name).
    pub fn add_op(&self, text: &str) -> Option<Value> {
        let Gate::Add {
            catalog_id,
            section,
            ..
        } = self
        else {
            return None;
        };
        let mut op = json!({"opId": uuid::Uuid::new_v4().to_string(), "type": "add", "text": text});
        if let Some(id) = catalog_id {
            // The catalog id is also the illustration key: clients draw its art from it.
            op["catalogId"] = json!(id);
        }
        if let (None, Some(s)) = (catalog_id, section) {
            op["section"] = json!(s);
        }
        Some(op)
    }
}

fn label(names: &std::collections::HashMap<String, String>, fallback: &str) -> String {
    let mut parts: Vec<&str> = ["it", "en", "da"]
        .iter()
        .filter_map(|l| names.get(*l).map(String::as_str))
        .collect();
    parts.dedup();
    if parts.is_empty() {
        fallback.to_owned()
    } else {
        parts.join(" / ")
    }
}

fn item_question(section: &str) -> String {
    format!("item_{section}")
}

/// The fan-out questions: plausibility, section, and one item choice per section.
pub fn questions(idx: &CatalogIndex) -> Vec<(String, Question)> {
    let mut qs = vec![(
        "plausible".to_owned(),
        Question::noul_with(
            "Is `item` something people put on their home shopping list?",
            "Yes: food, drinks, a household consumable (detergent, paper, cleaning products, batteries), personal care, baby or pet supplies.",
            "No: an appliance, furniture, electronics, clothes, a person, a place, a task, or a mis-heard word that names no product.",
        ),
    )];
    let sections: Vec<(String, String)> = idx
        .sections()
        .iter()
        .map(|s| (s.id.clone(), label(&s.names, &s.id)))
        .collect();
    if sections.len() >= 2 {
        qs.push((
            "section".to_owned(),
            Question::choice(
                "In which supermarket section does `item` belong?",
                sections.clone(),
            ),
        ));
    }
    for (sid, sname) in &sections {
        let mut opts: Vec<(String, String)> = idx
            .items()
            .filter(|i| i.section.as_deref() == Some(sid.as_str()))
            .map(|i| (i.id.clone(), label(&i.names, &i.id)))
            .collect();
        opts.sort();
        opts.truncate(lucina_jev::MAX_OPTIONS - 1);
        if opts.is_empty() {
            continue;
        }
        opts.push((
            "none".into(),
            "None of these: a different product.".into(),
        ));
        qs.push((
            item_question(sid),
            Question::choice(
                format!("Which item of the section \"{sname}\" is `item`, or the product it most likely means if it is a mis-heard, shortened or wrong word (\"lavatrice\" can mean laundry detergent)? Answer none when no item here fits."),
                opts,
            ),
        ));
    }
    qs
}

fn ask_text(lang: &str, text: &str, suggestion: Option<&str>) -> String {
    match (lang, suggestion) {
        ("it", Some(s)) => format!("«{text}» non mi sembra una cosa da lista della spesa, quindi non l'ho aggiunto. Intendevi «{s}»?"),
        ("it", None) => format!("«{text}» non mi sembra una cosa da lista della spesa, quindi non l'ho aggiunto. Cosa intendevi?"),
        ("da", Some(s)) => format!("“{text}” ligner ikke noget til indkøbslisten, så jeg har ikke tilføjet det. Mente du “{s}”?"),
        ("da", None) => format!("“{text}” ligner ikke noget til indkøbslisten, så jeg har ikke tilføjet det. Hvad mente du?"),
        (_, Some(s)) => format!("“{text}” doesn't look like a shopping-list item, so I didn't add it. Did you mean “{s}”?"),
        (_, None) => format!("“{text}” doesn't look like a shopping-list item, so I didn't add it. What did you mean?"),
    }
}

/// Decide from Jev's answers (pure, for tests).
pub fn decide(
    idx: &CatalogIndex,
    d: &lucina_jev::Decision,
    text: &str,
    lang: &str,
) -> Gate {
    let plausible = d.yes("plausible").unwrap_or(1.0);
    let section = d.choice("section");
    // Binding uses the item of the section Jev picked; a suggestion for a
    // mis-heard word takes the surest item over every section.
    let item = section
        .and_then(|(s, _)| d.choice(&item_question(s)))
        .filter(|(id, _)| *id != "none");
    let closest = idx
        .sections()
        .iter()
        .filter_map(|s| d.choice(&item_question(&s.id)))
        .filter(|(id, _)| *id != "none")
        .max_by(|a, b| a.1.total_cmp(&b.1));
    if plausible < PLAUSIBLE_MIN {
        let suggestion = closest
            .filter(|(_, c)| *c >= SUGGEST_MIN)
            .and_then(|(id, _)| idx.item(id))
            .map(|i| {
                i.names
                    .get(lang)
                    .cloned()
                    .or_else(|| i.display_name())
                    .unwrap_or_else(|| i.id.clone())
            });
        return Gate::Ask {
            question: ask_text(lang, text, suggestion.as_deref()),
            suggestion,
        };
    }
    Gate::Add {
        catalog_id: item
            .filter(|(_, c)| *c >= BIND_MIN)
            .map(|(id, _)| id.to_owned()),
        section: section
            .filter(|(_, c)| *c >= SECTION_MIN)
            .map(|(s, _)| s.to_owned()),
        by: "jev",
    }
}

/// Check one item. `lang`: the owner's language for a question (`it`, `en`, `da`).
pub async fn check(jev: &Jev, idx: &CatalogIndex, text: &str, lang: &str) -> Gate {
    if let Some(i) = idx.find(text) {
        return Gate::Add {
            catalog_id: Some(i.id.clone()),
            section: None,
            by: "catalog",
        };
    }
    let fallback = Gate::Add {
        catalog_id: None,
        section: None,
        by: "fallback",
    };
    if !jev.available() || fold(text).is_empty() || idx.is_empty() {
        return fallback;
    }
    let owned = questions(idx);
    let qs: Vec<(&str, Question)> = owned.iter().map(|(n, q)| (n.as_str(), q.clone())).collect();
    match jev
        .decide("shopping", json!({"item": text, "list": "home groceries and household"}), &qs)
        .await
    {
        Ok(d) => {
            let g = decide(idx, &d, text, lang);
            tracing::info!(target: "lucina_jev::shopping", item = text, plausible = ?d.yes("plausible"),
                section = ?d.choice("section"), gate = ?g, ms = d.elapsed_ms, "jev shopping gate");
            g
        }
        Err(_) => fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_jev::{Answer, Decision};
    use std::collections::BTreeMap;

    fn idx() -> CatalogIndex {
        CatalogIndex::build(&json!({
            "sections": [
                {"id": "household", "names": {"it": "Casa", "en": "Household"}},
                {"id": "deli", "names": {"it": "Gastronomia", "en": "Deli"}}
            ],
            "items": [
                {"id": "laundry-detergent", "names": {"it": "Detersivo per lavatrice", "en": "Laundry detergent"}, "section": "household",
                 "aliases": {"it": ["detersivo lavatrice"]}},
                {"id": "sponges", "names": {"it": "Spugne", "en": "Sponges"}, "section": "household"},
                {"id": "prosciutto", "names": {"it": "Prosciutto crudo", "en": "Prosciutto"}, "section": "deli",
                 "aliases": {"it": ["crudo"]}}
            ]
        }))
    }

    fn choice(c: &str, conf: f64) -> Answer {
        Answer::Choice {
            choice: c.into(),
            confidence: conf,
            probabilities: BTreeMap::new(),
        }
    }

    fn decision(plausible: f64, item: (&str, f64)) -> Decision {
        let mut a = BTreeMap::new();
        a.insert("plausible".into(), Answer::Noul(plausible));
        a.insert("section".into(), choice("household", 0.9));
        a.insert("item_household".into(), choice(item.0, item.1));
        Decision {
            answers: a,
            ..Decision::default()
        }
    }

    #[test]
    fn an_implausible_item_is_asked_about_with_the_closest_catalog_item() {
        let g = decide(&idx(), &decision(0.08, ("laundry-detergent", 0.55)), "Lavatrici", "it");
        let Gate::Ask {
            question,
            suggestion,
        } = g
        else {
            panic!("{g:?}")
        };
        assert_eq!(suggestion.as_deref(), Some("Detersivo per lavatrice"));
        assert!(question.contains("«Lavatrici»"));
        assert!(question.contains("«Detersivo per lavatrice»"));
        assert!(g_add_op_is_none());
    }

    fn g_add_op_is_none() -> bool {
        Gate::Ask {
            question: String::new(),
            suggestion: None,
        }
        .add_op("x")
        .is_none()
    }

    #[test]
    fn a_plausible_item_is_bound_only_when_jev_is_sure() {
        let g = decide(&idx(), &decision(0.95, ("laundry-detergent", 0.9)), "Pastiglie bucato", "it");
        assert_eq!(
            g,
            Gate::Add {
                catalog_id: Some("laundry-detergent".into()),
                section: Some("household".into()),
                by: "jev"
            }
        );
        let op = g.add_op("Pastiglie bucato").unwrap();
        assert_eq!(op["catalogId"], "laundry-detergent");
        assert!(op.get("icon").is_none(), "no svg: icon is written");
        assert_eq!(op["text"], "Pastiglie bucato");
        assert!(op.get("section").is_none(), "the catalog item brings its section");

        let g = decide(&idx(), &decision(0.95, ("sponges", 0.5)), "Panni microfibra", "it");
        let op = g.add_op("Panni microfibra").unwrap();
        assert!(op.get("catalogId").is_none());
        assert_eq!(op["section"], "household");
    }

    #[test]
    fn questions_cover_every_section_with_a_none_option() {
        let qs = questions(&idx());
        let names: Vec<&str> = qs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["plausible", "section", "item_household", "item_deli"]);
        let Question::Choice { options, .. } = &qs[2].1 else {
            panic!()
        };
        assert_eq!(options.last().unwrap().0, "none");
        assert!(options.len() <= lucina_jev::MAX_OPTIONS);
    }

    #[tokio::test]
    async fn exact_catalog_names_need_no_call_and_jev_off_adds_as_before() {
        let off = Jev::new(lucina_jev::Config {
            url: "http://127.0.0.1:9/v1/systemone".into(),
            model: "jev-1.14.0".into(),
            key: None,
            timeout: std::time::Duration::from_secs(1),
            enabled: true,
        });
        let g = check(&off, &idx(), "detersivo lavatrice", "it").await;
        assert_eq!(
            g,
            Gate::Add {
                catalog_id: Some("laundry-detergent".into()),
                section: None,
                by: "catalog"
            }
        );
        let g = check(&off, &idx(), "Lavatrici", "it").await;
        assert!(matches!(g, Gate::Add { by: "fallback", .. }));
    }
}
