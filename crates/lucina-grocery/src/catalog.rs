//! Shopping catalog (`assets/catalog/catalog.v1.json`): free text to a
//! catalog item id, section and icon (`src/grocery/shopping-catalog.js`).
//!
//! Lookup order: `LUCINA_SHOPPING_CATALOG_PATH`, `data/shopping-catalog.v1.json`,
//! `/home/nez/Projects/Lucina/SecretaryIOS/assets/catalog/catalog.v1.json`; re-read
//! when the file's mtime changes (checked at most every 30 s). Aliases may be
//! an array or, as in catalog v1, an object of per-language arrays.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

const RECHECK: Duration = Duration::from_secs(30);

/// The catalog `normalise()` (SecretaryIOS `docs/catalog.md`, Node
/// `foldCatalogText`): lowercase, fold æ ø å œ ß ð þ ł, strip diacritics,
/// every run of non-letters/digits becomes one space, trim.
pub fn fold(text: &str) -> String {
    let mut special = String::with_capacity(text.len());
    for c in text.to_lowercase().chars() {
        match c {
            'æ' => special.push_str("ae"),
            'ø' => special.push_str("oe"),
            'å' => special.push_str("aa"),
            'œ' => special.push_str("oe"),
            'ß' => special.push_str("ss"),
            'ð' => special.push('d'),
            'þ' => special.push_str("th"),
            'ł' => special.push('l'),
            c => special.push(c),
        }
    }
    let mut out = String::new();
    let mut gap = false;
    for c in special.nfd().filter(|c| !is_combining_mark(*c)) {
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogItem {
    pub id: String,
    pub name: Option<String>,
    pub names: HashMap<String, String>,
    pub section: Option<String>,
    pub icon: Option<String>,
}

impl CatalogItem {
    /// Display name: `name`, else the Italian, English, then any name.
    pub fn display_name(&self) -> Option<String> {
        self.name
            .clone()
            .or_else(|| self.names.get("it").cloned())
            .or_else(|| self.names.get("en").cloned())
            .or_else(|| self.names.values().next().cloned())
    }
}

/// A catalog section (`{id, names, icon, order}`; localised files have `name`).
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogSection {
    pub id: String,
    pub name: Option<String>,
    pub names: HashMap<String, String>,
    pub icon: Option<String>,
    pub order: f64,
}

#[derive(Debug, Default)]
pub struct CatalogIndex {
    pub version: Option<Value>,
    items: HashMap<String, CatalogItem>,
    sections: Vec<CatalogSection>,
    /// Folded id, name and per-language names; they win over aliases.
    by_name: HashMap<String, String>,
    by_alias: HashMap<String, String>,
}

/// Alias strings: an array, or the master's `{it, en, da, all}` object of arrays.
fn alias_terms(v: Option<&Value>) -> Vec<String> {
    let strs = |a: &Vec<Value>| -> Vec<String> {
        a.iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| x.to_string())
            })
            .collect()
    };
    match v {
        Some(Value::Array(a)) => strs(a),
        Some(Value::Object(o)) => o
            .values()
            .filter_map(Value::as_array)
            .flat_map(strs)
            .collect(),
        _ => Vec::new(),
    }
}

impl CatalogIndex {
    pub fn build(data: &Value) -> Self {
        let mut idx = CatalogIndex {
            version: data.get("version").cloned(),
            ..Default::default()
        };
        for s in data
            .get("sections")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = s
                .get("id")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
            else {
                continue;
            };
            idx.sections.push(CatalogSection {
                id: id.to_owned(),
                name: s.get("name").and_then(Value::as_str).map(str::to_owned),
                names: s
                    .get("names")
                    .and_then(Value::as_object)
                    .map(|o| {
                        o.iter()
                            .filter_map(|(k, v)| v.as_str().map(|x| (k.clone(), x.to_owned())))
                            .collect()
                    })
                    .unwrap_or_default(),
                icon: s.get("icon").and_then(Value::as_str).map(str::to_owned),
                order: s.get("order").and_then(Value::as_f64).unwrap_or(0.0),
            });
        }
        idx.sections.sort_by(|a, b| {
            a.order
                .partial_cmp(&b.order)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut aliases: Vec<(String, Vec<String>)> = Vec::new();
        for item in data
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(id) = item.get("id").and_then(|v| match v {
                Value::String(s) if !s.is_empty() => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            }) else {
                continue;
            };
            let names: HashMap<String, String> = item
                .get("names")
                .and_then(Value::as_object)
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                        .collect()
                })
                .unwrap_or_default();
            let entry = CatalogItem {
                id: id.clone(),
                name: item.get("name").and_then(Value::as_str).map(str::to_owned),
                names: names.clone(),
                section: item
                    .get("section")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                icon: item.get("icon").and_then(Value::as_str).map(str::to_owned),
            };
            let mut terms = vec![id.clone()];
            terms.extend(entry.name.clone());
            terms.extend(names.values().cloned());
            for term in terms {
                let key = fold(&term);
                if !key.is_empty() {
                    idx.by_name.entry(key).or_insert_with(|| id.clone());
                }
            }
            aliases.push((id.clone(), alias_terms(item.get("aliases"))));
            idx.items.insert(id, entry);
        }
        // Names (any language, and the id) win over aliases: "latte" is
        // Milk's Italian name before it is anybody's alias.
        for (id, terms) in aliases {
            for term in terms {
                let key = fold(&term);
                if !key.is_empty() {
                    idx.by_alias.entry(key).or_insert_with(|| id.clone());
                }
            }
        }
        idx
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn item(&self, id: &str) -> Option<&CatalogItem> {
        self.items.get(id)
    }

    /// Every section, in catalog order.
    pub fn sections(&self) -> &[CatalogSection] {
        &self.sections
    }

    pub fn section(&self, id: &str) -> Option<&CatalogSection> {
        self.sections.iter().find(|s| s.id == id)
    }

    /// An already folded key: a name in any language (`"name"`) or an alias
    /// (`"alias"`); no plural guessing.
    pub fn lookup(&self, key: &str) -> Option<(&CatalogItem, &'static str)> {
        if key.is_empty() {
            return None;
        }
        if let Some(id) = self.by_name.get(key) {
            return self.items.get(id).map(|i| (i, "name"));
        }
        self.by_alias
            .get(key)
            .and_then(|id| self.items.get(id))
            .map(|i| (i, "alias"))
    }

    /// Exact folded match of an id, name, translation or alias (a trailing
    /// `s` is also tried). `None` when unknown.
    pub fn find(&self, text: &str) -> Option<&CatalogItem> {
        let key = fold(text);
        if key.is_empty() {
            return None;
        }
        let term = |k: &str| self.by_name.get(k).or_else(|| self.by_alias.get(k));
        let id = term(&key).or_else(|| key.strip_suffix('s').and_then(term))?;
        self.items.get(id)
    }

    /// Strict lookup for display: a name in any language (`exact: true`) or
    /// an alias (`exact: false`) after folding; no plural guessing.
    pub fn resolve(&self, text: &str) -> Option<(&CatalogItem, bool)> {
        let key = fold(text);
        if key.is_empty() {
            return None;
        }
        if let Some(id) = self.by_name.get(&key) {
            return self.items.get(id).map(|i| (i, true));
        }
        self.by_alias
            .get(&key)
            .and_then(|id| self.items.get(id))
            .map(|i| (i, false))
    }
}

struct Cached {
    index: Arc<CatalogIndex>,
    file: Option<PathBuf>,
    mtime: Option<SystemTime>,
    checked: Option<Instant>,
}

/// The catalog, reloaded on change.
pub struct Catalog {
    candidates: Vec<PathBuf>,
    cached: Mutex<Cached>,
    fixed: Option<Arc<CatalogIndex>>,
}

impl std::fmt::Debug for Catalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Catalog")
            .field("candidates", &self.candidates)
            .finish()
    }
}

impl Catalog {
    pub fn from_paths(candidates: Vec<PathBuf>) -> Self {
        Self {
            candidates,
            cached: Mutex::new(Cached {
                index: Arc::new(CatalogIndex::default()),
                file: None,
                mtime: None,
                checked: None,
            }),
            fixed: None,
        }
    }

    /// The default lookup order relative to `data_dir`.
    pub fn default_paths(explicit: Option<PathBuf>, data_dir: &std::path::Path) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = explicit.into_iter().collect();
        v.push(data_dir.join("shopping-catalog.v1.json"));
        v.push(PathBuf::from(
            "/home/nez/Projects/Lucina/SecretaryIOS/assets/catalog/catalog.v1.json",
        ));
        v
    }

    /// A fixed catalog (tests).
    pub fn fixed(data: &Value) -> Self {
        Self {
            fixed: Some(Arc::new(CatalogIndex::build(data))),
            ..Self::from_paths(Vec::new())
        }
    }

    pub fn index(&self) -> Arc<CatalogIndex> {
        if let Some(f) = &self.fixed {
            return f.clone();
        }
        let mut c = self.cached.lock().unwrap_or_else(|e| e.into_inner());
        if c.checked.is_some_and(|t| t.elapsed() < RECHECK) {
            return c.index.clone();
        }
        c.checked = Some(Instant::now());
        for file in &self.candidates {
            let Ok(meta) = std::fs::metadata(file) else {
                continue;
            };
            let mtime = meta.modified().ok();
            if c.file.as_ref() == Some(file) && c.mtime == mtime {
                return c.index.clone();
            }
            match std::fs::read(file)
                .map_err(|e| e.to_string())
                .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string()))
            {
                Ok(data) => {
                    let index = Arc::new(CatalogIndex::build(&data));
                    tracing::info!(file = %file.display(), items = index.len(), "shopping catalog loaded");
                    c.index = index;
                    c.file = Some(file.clone());
                    c.mtime = mtime;
                }
                Err(e) => {
                    tracing::warn!(file = %file.display(), error = %e, "shopping catalog unreadable")
                }
            }
            return c.index.clone();
        }
        c.index = Arc::new(CatalogIndex::default());
        c.file = None;
        c.mtime = None;
        c.index.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn folds_and_matches_v1_and_legacy_shapes() {
        assert_eq!(fold("  Caffè  Latte! "), "caffe latte");
        assert_eq!(fold("Æbler"), "aebler");
        let idx = CatalogIndex::build(&json!({"version": 1, "items": [
            {"id": "apple", "names": {"it": "Mele", "en": "Apples", "da": "Æbler"},
             "aliases": {"it": ["mela"], "en": ["apple"]}, "section": "produce", "icon": "🍎"},
            {"id": "banana", "name": "Banane", "aliases": ["banana"], "section": "produce"}
        ]}));
        assert_eq!(idx.find("MELA").unwrap().id, "apple");
        assert_eq!(idx.find("æbler").unwrap().id, "apple");
        assert_eq!(idx.find("bananas").unwrap().id, "banana");
        assert_eq!(
            idx.find("apple").unwrap().display_name().as_deref(),
            Some("Mele")
        );
        assert!(idx.find("kiwi").is_none());
    }
}

#[cfg(test)]
mod fold_tests {
    use super::*;

    #[test]
    fn folds_like_node() {
        assert_eq!(fold("Rugbrød"), "rugbroed");
        assert_eq!(fold("Æble-grød!"), "aeble groed");
        assert_eq!(fold("Blåbær"), "blaabaer");
        assert_eq!(fold("Crème fraîche"), "creme fraiche");
        assert_eq!(fold("  Straße  "), "strasse");
        let idx = CatalogIndex::build(&serde_json::json!({"items": [
            {"id": "milk", "names": {"it": "Latte", "en": "Milk"}, "section": "dairy", "icon": "🥛"},
            {"id": "coffee", "names": {"it": "Caffè"}, "aliases": {"it": ["latte macchiato", "latte"]}}
        ]}));
        assert_eq!(
            idx.find("latte").map(|i| i.id.as_str()),
            Some("milk"),
            "names win over aliases"
        );
        assert_eq!(
            idx.resolve("latte macchiato")
                .map(|(i, e)| (i.id.as_str(), e)),
            Some(("coffee", false))
        );
        assert_eq!(
            idx.resolve("Milk").map(|(i, e)| (i.id.as_str(), e)),
            Some(("milk", true))
        );
        assert!(
            idx.resolve("milks").is_none(),
            "no plural guessing on display"
        );
    }
}
