//! Shopping-list item → search terms (`synonyms.js`): the default rules plus
//! the owner's override file (`data/grocery/deal-synonyms.json`), reloaded
//! when its mtime changes. A malformed edit keeps the last good rules.
//!
//! The file is an array of patches (or `{ "rules": [...] }`). A patch with
//! the `id` of a default rule replaces fields of it; `terms`, `exclude` and
//! `names` replace the list, `add_<field>` appends and `remove_<field>`
//! removes. A patch with a new `id` adds a rule (it needs terms).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use lucina_grocery::resolve_canonical_product;

use crate::js::{self, JsValue};
use crate::matcher::normalize;

#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub id: String,
    pub label: Option<String>,
    pub names: Vec<String>,
    pub terms: Vec<String>,
    pub exclude: Vec<String>,
    pub categories: Option<Vec<String>>,
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).to_owned()).collect()
}

fn rule(
    id: &str,
    label: &str,
    names: &[&str],
    terms: &[&str],
    exclude: &[&str],
    categories: &[&str],
) -> Rule {
    Rule {
        id: id.into(),
        label: Some(label.into()),
        names: strings(names),
        terms: strings(terms),
        exclude: strings(exclude),
        categories: Some(strings(categories)),
    }
}

pub fn default_rules() -> Vec<Rule> {
    vec![
        rule(
            "prosciutto_crudo",
            "Prosciutto crudo",
            &[
                "prosciutto",
                "prosciutto crudo",
                "parmaskinke",
                "serranoskinke",
                "serrano",
                "jamón",
                "jamon",
                "lufttørret skinke",
                "parma ham",
            ],
            &[
                "parmaskinke",
                "serranoskinke",
                "serrano",
                "jamón",
                "jamon",
                "lufttørret skinke",
                "parma ham",
                "prosciutto",
            ],
            &["prosciutto cotto", "kogt skinke", "hamburgerryg"],
            &["pålæg", "tapas"],
        ),
        rule(
            "ham",
            "Ham",
            &[
                "prosciutto cotto",
                "ham",
                "skinke",
                "kogt skinke",
                "hamburgerryg",
                "skinke i skiver",
            ],
            &["skinke", "hamburgerryg", "kogt skinke", "ham"],
            &[
                "parma",
                "parmaskinke",
                "serrano",
                "serranoskinke",
                "jamón",
                "ibérico",
                "lufttørret",
                "hel skinke",
                "ribbenssteg",
                "schnitzel",
                "flødekartofler",
            ],
            &["pålæg", "tapas"],
        ),
        rule(
            "milk",
            "Milk",
            &[
                "latte",
                "milk",
                "mælk",
                "letmælk",
                "sødmælk",
                "minimælk",
                "skummetmælk",
            ],
            &["mælk", "letmælk", "sødmælk", "milk"],
            &[
                "kokosmælk",
                "kakaomælk",
                "chokolademælk",
                "tykmælk",
                "kærnemælk",
                "havre",
                "havremælk",
                "mælkechokolade",
            ],
            &["mejeri"],
        ),
        rule(
            "rye_bread",
            "Rye bread",
            &["pane di segale", "rye bread", "rugbrød"],
            &["rugbrød", "kernerugbrød", "solsikkerugbrød", "rye bread"],
            &[],
            &["rugbrød"],
        ),
        rule(
            "yogurt",
            "Yogurt",
            &["yogurt", "yoghurt", "skyr", "ymer", "græsk yoghurt"],
            &["yoghurt", "yogurt", "skyr", "ymer"],
            &[],
            &["yoghurt", "skyr"],
        ),
    ]
}

/// A rule for one list item; `known` when it came from the rule set (Node's
/// `allRules.includes(rule)`), not from the canonical-product fallback.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub rule: Rule,
    pub known: bool,
}

/// Unknown items retain canonical aliases so the list remains extensible.
pub fn resolve_rule(name: &str, rules: &[Rule]) -> Resolved {
    let wanted = normalize(name);
    if let Some(found) = rules
        .iter()
        .find(|r| r.names.iter().any(|n| normalize(n) == wanted))
    {
        return Resolved {
            rule: found.clone(),
            known: true,
        };
    }
    let product = resolve_canonical_product(name);
    let mut terms = vec![name.to_owned()];
    for alias in product.aliases {
        if !terms.contains(&alias) {
            terms.push(alias);
        }
    }
    Resolved {
        rule: Rule {
            id: product.id,
            label: Some(name.to_owned()),
            names: vec![name.to_owned()],
            terms,
            exclude: Vec::new(),
            categories: None,
        },
        known: false,
    }
}

/// A JS list field of a patch: arrays as they are, a string as its
/// characters (spread), anything else is not iterable (the load fails).
fn list_field(value: Option<&JsValue>) -> Result<Option<Vec<String>>, ()> {
    let text = |v: &JsValue| match v {
        JsValue::String(s) => s.clone(),
        JsValue::Number(n) => js::num_to_string(*n),
        JsValue::Bool(b) => b.to_string(),
        JsValue::Null => "null".into(),
        other => other.stringify(0),
    };
    match value {
        None | Some(JsValue::Null) | Some(JsValue::Bool(false)) => Ok(None),
        Some(JsValue::Number(n)) if *n == 0.0 || n.is_nan() => Ok(None),
        Some(JsValue::String(s)) if s.is_empty() => Ok(None),
        Some(JsValue::Array(items)) => Ok(Some(items.iter().map(text).collect())),
        Some(JsValue::String(s)) => Ok(Some(s.chars().map(String::from).collect())),
        Some(_) => Err(()),
    }
}

fn apply_patches(value: &JsValue) -> Result<Vec<Rule>, ()> {
    let mut next = default_rules();
    let patches: Vec<JsValue> = match value {
        JsValue::Array(items) => items.clone(),
        JsValue::Object(_) => match value.get("rules") {
            None => Vec::new(),
            Some(JsValue::Array(items)) => items.clone(),
            Some(JsValue::Null) | Some(JsValue::Bool(false)) => Vec::new(),
            Some(JsValue::String(s)) if s.is_empty() => Vec::new(),
            Some(_) => return Err(()),
        },
        // `null.rules` throws; other primitives have no `rules`.
        JsValue::Null => return Err(()),
        _ => Vec::new(),
    };
    for patch in &patches {
        if matches!(patch, JsValue::Null) {
            return Err(());
        }
        let id = match patch.get("id") {
            Some(JsValue::String(s)) if !s.is_empty() => s.clone(),
            Some(JsValue::Number(n)) if *n != 0.0 && !n.is_nan() => js::num_to_string(*n),
            Some(JsValue::Bool(true)) => "true".into(),
            _ => continue,
        };
        let index = next.iter().position(|r| r.id == id);
        let base = index.map(|i| next[i].clone()).unwrap_or(Rule {
            id: id.clone(),
            label: None,
            names: Vec::new(),
            terms: Vec::new(),
            exclude: Vec::new(),
            categories: None,
        });
        let mut merged = base.clone();
        merged.id = id;
        if let Some(label) = patch.get("label") {
            merged.label = label.as_str().map(str::to_owned);
        }
        if let Some(categories) = patch.get("categories") {
            merged.categories = list_field(Some(categories))?;
        }
        for field in ["terms", "exclude", "names"] {
            let own = list_field(patch.get(field))?;
            let old = match field {
                "terms" => &base.terms,
                "exclude" => &base.exclude,
                _ => &base.names,
            };
            let added = list_field(patch.get(&format!("add_{field}")))?.unwrap_or_default();
            let removed = list_field(patch.get(&format!("remove_{field}")))?.unwrap_or_default();
            let mut values: Vec<String> = Vec::new();
            for v in own.unwrap_or_else(|| old.clone()).into_iter().chain(added) {
                if !values.contains(&v) {
                    values.push(v);
                }
            }
            values.retain(|v| !removed.contains(v));
            match field {
                "terms" => merged.terms = values,
                "exclude" => merged.exclude = values,
                _ => merged.names = values,
            }
        }
        if merged.terms.is_empty() {
            continue;
        }
        match index {
            Some(i) => next[i] = merged,
            None => next.push(merged),
        }
    }
    Ok(next)
}

/// The rule set with hot reload of the owner's file.
#[derive(Debug)]
pub struct Synonyms {
    path: PathBuf,
    state: Mutex<(Option<SystemTime>, Arc<Vec<Rule>>)>,
}

impl Synonyms {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            state: Mutex::new((None, Arc::new(default_rules()))),
        }
    }

    /// The current rules (re-read when the file's mtime changed).
    pub fn rules(&self) -> Arc<Vec<Rule>> {
        let mut state = self.state.lock().expect("synonyms lock");
        let stamp = match std::fs::metadata(&self.path) {
            Ok(meta) => meta.modified().ok(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                *state = (None, Arc::new(default_rules()));
                return state.1.clone();
            }
            Err(_) => return state.1.clone(),
        };
        if stamp.is_some() && stamp == state.0 {
            return state.1.clone();
        }
        let loaded = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| JsValue::parse(&text).ok())
            .and_then(|value| apply_patches(&value).ok());
        match loaded {
            Some(rules) => *state = (stamp, Arc::new(rules)),
            None => tracing::warn!(
                "deal synonyms: malformed override file, keeping the last good rules"
            ),
        }
        state.1.clone()
    }
}
