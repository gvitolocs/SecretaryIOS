//! catalog-check: validates the Secretary shopping catalog and keeps the
//! localised files in sync with the master.
//!
//! Usage: catalog-check [--write] [CATALOG_DIR]
//!
//! CATALOG_DIR defaults to `assets/catalog` of this repository. Without
//! `--write` the localised files `catalog.{it,en,da}.json` are only compared
//! with what the master would generate; with `--write` they are regenerated
//! first. Exit code 0 = no errors (warnings allowed), 1 = errors, 2 = usage/IO.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

const LANGS: [&str; 3] = ["it", "en", "da"];

/// Lucina's canonical grocery products (`Lucina/src/grocery/products.js`).
/// Catalog id = Lucina id with `_` replaced by `-`. Every Lucina alias must
/// resolve (normalised) to that catalog item in at least one language.
const LUCINA: &[(&str, &[&str])] = &[
    ("chicken_breast", &["chicken breast", "chicken fillet", "kyllingebryst", "kyllinge brystfilet"]),
    ("chicken", &["chicken", "kylling"]),
    ("mozzarella", &["mozzarella"]),
    ("ham", &["ham", "prosciutto cotto", "skinke"]),
    ("prosciutto", &["prosciutto", "parma ham", "parmaskinke"]),
    ("pasta", &["pasta", "spaghetti", "penne"]),
    ("rice", &["rice", "ris"]),
    ("milk", &["milk", "latte", "mælk"]),
    ("yogurt", &["yogurt", "yoghurt"]),
    ("rye_bread", &["rye bread", "pane di segale", "rugbrød"]),
    ("eggs", &["egg", "eggs", "æg"]),
];

// ---------- master format ----------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Master {
    version: u32,
    #[serde(default)]
    #[allow(dead_code)]
    note: Option<String>,
    sections: Vec<Section>,
    items: Vec<Item>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(deny_unknown_fields)]
struct Names {
    it: Option<String>,
    en: Option<String>,
    da: Option<String>,
}

impl Names {
    fn get(&self, lang: &str) -> Option<&str> {
        match lang {
            "it" => self.it.as_deref(),
            "en" => self.en.as_deref(),
            "da" => self.da.as_deref(),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Section {
    id: String,
    names: Names,
    icon: String,
    order: u32,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Aliases {
    #[serde(default)]
    it: Vec<String>,
    #[serde(default)]
    en: Vec<String>,
    #[serde(default)]
    da: Vec<String>,
    /// Language-neutral aliases (brands used as generics, loanwords); merged into every language.
    #[serde(default)]
    all: Vec<String>,
}

impl Aliases {
    fn get(&self, lang: &str) -> &[String] {
        match lang {
            "it" => &self.it,
            "en" => &self.en,
            "da" => &self.da,
            _ => &self.all,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    id: String,
    names: Names,
    #[serde(default)]
    aliases: Aliases,
    section: String,
    icon: String,
}

// ---------- localised (contract) format ----------

#[derive(Serialize)]
struct LocFile<'a> {
    version: u32,
    lang: &'a str,
    sections: Vec<LocSection<'a>>,
    items: Vec<LocItem<'a>>,
}

#[derive(Serialize)]
struct LocSection<'a> {
    id: &'a str,
    name: &'a str,
    icon: &'a str,
    order: u32,
}

#[derive(Serialize)]
struct LocNames<'a> {
    en: &'a str,
    it: &'a str,
    da: &'a str,
}

#[derive(Serialize)]
struct LocItem<'a> {
    id: &'a str,
    name: &'a str,
    names: LocNames<'a>,
    aliases: Vec<&'a str>,
    section: &'a str,
    icon: &'a str,
}

// ---------- normalisation (the rule clients must apply too) ----------

/// Lowercase, fold Danish/Norwegian letters (æ→ae, ø→oe, å→aa) and other
/// special letters, strip diacritics (NFD minus combining marks), turn every
/// run of non-alphanumerics into one space, trim.
pub fn normalise(s: &str) -> String {
    let mut pre = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            'æ' => pre.push_str("ae"),
            'ø' => pre.push_str("oe"),
            'å' => pre.push_str("aa"),
            'œ' => pre.push_str("oe"),
            'ß' => pre.push_str("ss"),
            'ð' => pre.push('d'),
            'þ' => pre.push_str("th"),
            'ł' => pre.push('l'),
            _ => pre.push(c),
        }
    }
    let mut out = String::with_capacity(pre.len());
    let mut gap = false;
    for c in pre.nfd().filter(|c| !is_combining_mark(*c)) {
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

fn is_kebab(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && !id.ends_with('-')
        && !id.contains("--")
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A valid icon is `svg:<kebab-key>` or exactly one extended grapheme cluster
/// that is not plain text (no letters/digits/ASCII).
fn icon_problem(icon: &str) -> Option<String> {
    if let Some(key) = icon.strip_prefix("svg:") {
        return if is_kebab(key) { None } else { Some(format!("bad svg key {key:?}")) };
    }
    let n = icon.graphemes(true).count();
    if n != 1 {
        return Some(format!("{icon:?} is {n} graphemes, expected 1"));
    }
    if icon.chars().any(|c| c.is_ascii() || c.is_alphanumeric()) {
        return Some(format!("{icon:?} is text, not an emoji"));
    }
    None
}

// ---------- report ----------

#[derive(Default)]
struct Report {
    errors: Vec<String>,
    warnings: Vec<String>,
}

impl Report {
    fn err(&mut self, m: impl Into<String>) {
        self.errors.push(m.into());
    }
    fn warn(&mut self, m: impl Into<String>) {
        self.warnings.push(m.into());
    }
}

/// Aliases of `item` for `lang` as published: language aliases then `all`,
/// trimmed, de-duplicated by normalised form, minus the item's own name.
fn published_aliases<'a>(item: &'a Item, lang: &str) -> Vec<&'a str> {
    let mut seen: HashSet<String> = HashSet::new();
    if let Some(n) = item.names.get(lang) {
        seen.insert(normalise(n));
    }
    let mut out = Vec::new();
    for a in item.aliases.get(lang).iter().chain(item.aliases.all.iter()) {
        let a = a.trim();
        if a.is_empty() {
            continue;
        }
        if seen.insert(normalise(a)) {
            out.push(a);
        }
    }
    out
}

fn render(master: &Master, lang: &str) -> String {
    let mut sections: Vec<&Section> = master.sections.iter().collect();
    sections.sort_by_key(|s| s.order);
    let file = LocFile {
        version: master.version,
        lang,
        sections: sections
            .iter()
            .map(|s| LocSection {
                id: &s.id,
                name: s.names.get(lang).unwrap_or(""),
                icon: &s.icon,
                order: s.order,
            })
            .collect(),
        items: master
            .items
            .iter()
            .map(|i| LocItem {
                id: &i.id,
                name: i.names.get(lang).unwrap_or(""),
                names: LocNames {
                    en: i.names.en.as_deref().unwrap_or(""),
                    it: i.names.it.as_deref().unwrap_or(""),
                    da: i.names.da.as_deref().unwrap_or(""),
                },
                aliases: published_aliases(i, lang),
                section: &i.section,
                icon: &i.icon,
            })
            .collect(),
    };
    // One section / item per line: small diffs, still a single static JSON document.
    let mut s = String::new();
    s.push_str(&format!(
        "{{\"version\":{},\"lang\":{},\n\"sections\":[\n",
        file.version,
        serde_json::to_string(lang).unwrap()
    ));
    let secs: Vec<String> = file.sections.iter().map(|x| serde_json::to_string(x).unwrap()).collect();
    s.push_str(&secs.join(",\n"));
    s.push_str("\n],\n\"items\":[\n");
    let items: Vec<String> = file.items.iter().map(|x| serde_json::to_string(x).unwrap()).collect();
    s.push_str(&items.join(",\n"));
    s.push_str("\n]}\n");
    s
}

fn check_master(m: &Master, r: &mut Report) {
    if m.version == 0 {
        r.err("version must be >= 1");
    }

    // sections
    let mut section_ids: HashMap<&str, &Section> = HashMap::new();
    let mut orders: HashSet<u32> = HashSet::new();
    for s in &m.sections {
        if !is_kebab(&s.id) {
            r.err(format!("section {:?}: id is not kebab-case", s.id));
        }
        if section_ids.insert(&s.id, s).is_some() {
            r.err(format!("section {:?}: duplicate id", s.id));
        }
        if !orders.insert(s.order) {
            r.err(format!("section {:?}: duplicate order {}", s.id, s.order));
        }
        for lang in LANGS {
            match s.names.get(lang) {
                Some(n) if !n.trim().is_empty() => {}
                _ => r.err(format!("section {:?}: missing {lang} name", s.id)),
            }
        }
        if let Some(p) = icon_problem(&s.icon) {
            r.err(format!("section {:?}: icon {p}", s.id));
        }
    }

    // items
    let mut ids: HashSet<&str> = HashSet::new();
    // per language: normalised term -> (item id, original text)
    let mut terms: BTreeMap<&str, HashMap<String, (&str, String)>> = BTreeMap::new();
    for lang in LANGS {
        terms.insert(lang, HashMap::new());
    }

    for it in &m.items {
        let id = it.id.as_str();
        if !is_kebab(id) {
            r.err(format!("item {id:?}: id is not kebab-case"));
        }
        if !ids.insert(id) {
            r.err(format!("item {id:?}: duplicate id"));
        }
        if !section_ids.contains_key(it.section.as_str()) {
            r.err(format!("item {id:?}: unknown section {:?}", it.section));
        }
        if let Some(p) = icon_problem(&it.icon) {
            r.err(format!("item {id:?}: icon {p}"));
        }
        for lang in LANGS {
            let name = match it.names.get(lang) {
                Some(n) if !n.trim().is_empty() => n,
                _ => {
                    r.err(format!("item {id:?}: missing {lang} name"));
                    continue;
                }
            };
            if name.trim() != name {
                r.err(format!("item {id:?}: {lang} name {name:?} has surrounding whitespace"));
            }
            let table = terms.get_mut(lang).unwrap();
            let mut own: HashSet<String> = HashSet::new();
            let candidates = std::iter::once((name, "name"))
                .chain(it.aliases.get(lang).iter().map(|a| (a.as_str(), "alias")))
                .chain(it.aliases.all.iter().map(|a| (a.as_str(), "alias(all)")));
            for (text, kind) in candidates {
                let n = normalise(text);
                if n.is_empty() {
                    r.err(format!("item {id:?}: empty {lang} {kind} {text:?}"));
                    continue;
                }
                if !own.insert(n.clone()) {
                    if kind != "alias(all)" {
                        r.warn(format!("item {id:?}: redundant {lang} {kind} {text:?}"));
                    }
                    continue;
                }
                if let Some((other, orig)) = table.get(&n) {
                    if *other != id {
                        r.err(format!(
                            "{lang}: {text:?} ({id}) collides with {orig:?} ({other}) as {n:?}"
                        ));
                    }
                } else {
                    table.insert(n, (id, text.to_string()));
                }
            }
        }
    }

    for s in &m.sections {
        if !m.items.iter().any(|i| i.section == s.id) {
            r.warn(format!("section {:?} has no items", s.id));
        }
    }

    // Lucina compatibility
    for (lucina_id, aliases) in LUCINA {
        let cid = lucina_id.replace('_', "-");
        let Some(item) = m.items.iter().find(|i| i.id == cid) else {
            r.err(format!("Lucina product {lucina_id:?} has no catalog item {cid:?}"));
            continue;
        };
        for a in *aliases {
            let n = normalise(a);
            let hit = LANGS.iter().any(|lang| {
                terms[lang].get(&n).map(|(owner, _)| *owner == item.id).unwrap_or(false)
            });
            if !hit {
                r.err(format!("Lucina alias {a:?} does not resolve to {cid:?}"));
            }
        }
    }
}

fn check_localised(m: &Master, dir: &Path, write: bool, r: &mut Report) {
    for lang in LANGS {
        let path = dir.join(format!("catalog.{lang}.json"));
        let expected = render(m, lang);
        if write {
            if let Err(e) = fs::write(&path, &expected) {
                r.err(format!("{}: write failed: {e}", path.display()));
            }
            continue;
        }
        let actual = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                r.err(format!("{}: {e} (run with --write)", path.display()));
                continue;
            }
        };
        if actual == expected {
            continue;
        }
        let exp_v: Value = serde_json::from_str(&expected).unwrap();
        match serde_json::from_str::<Value>(&actual) {
            Err(e) => r.err(format!("{}: invalid JSON: {e}", path.display())),
            Ok(v) if v == exp_v => r.warn(format!("{}: same content, different formatting", path.display())),
            Ok(v) => {
                let detail = first_difference(&v, &exp_v);
                r.err(format!("{}: out of date with master ({detail}); run with --write", path.display()));
            }
        }
    }
}

fn first_difference(actual: &Value, expected: &Value) -> String {
    for key in ["version", "lang", "sections"] {
        if actual.get(key) != expected.get(key) {
            return format!("{key} differs");
        }
    }
    let empty = Vec::new();
    let a = actual.get("items").and_then(Value::as_array).unwrap_or(&empty);
    let e = expected.get("items").and_then(Value::as_array).unwrap_or(&empty);
    if a.len() != e.len() {
        return format!("{} items, master has {}", a.len(), e.len());
    }
    for (x, y) in a.iter().zip(e) {
        if x != y {
            return format!("item {} differs", y.get("id").unwrap_or(&Value::Null));
        }
    }
    "top-level keys differ".into()
}

fn print_stats(m: &Master) {
    let mut sections: Vec<&Section> = m.sections.iter().collect();
    sections.sort_by_key(|s| s.order);
    println!("{:<20} {:>5}  {:>8}  {:>6} {:>6} {:>6}", "section", "items", "fallback", "al.it", "al.en", "al.da");
    let mut tot = [0usize; 5];
    for s in sections {
        let items: Vec<&Item> = m.items.iter().filter(|i| i.section == s.id).collect();
        let fallback = items.iter().filter(|i| i.icon == s.icon).count();
        let al = |lang: &str| items.iter().map(|i| published_aliases(i, lang).len()).sum::<usize>();
        let row = [items.len(), fallback, al("it"), al("en"), al("da")];
        for (t, v) in tot.iter_mut().zip(row) {
            *t += v;
        }
        println!(
            "{} {:<18} {:>5}  {:>8}  {:>6} {:>6} {:>6}",
            s.icon, s.id, row[0], row[1], row[2], row[3], row[4]
        );
    }
    println!(
        "{:<20} {:>5}  {:>8}  {:>6} {:>6} {:>6}",
        "total", tot[0], tot[1], tot[2], tot[3], tot[4]
    );
    let distinct: HashSet<&str> = m.items.iter().map(|i| i.icon.as_str()).collect();
    println!("{} sections, {} items, {} distinct icons", m.sections.len(), m.items.len(), distinct.len());
}

fn default_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/catalog")
}

fn main() -> ExitCode {
    let mut write = false;
    let mut dir: Option<PathBuf> = None;
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--write" | "-w" => write = true,
            "-h" | "--help" => {
                println!("usage: catalog-check [--write] [CATALOG_DIR]");
                return ExitCode::SUCCESS;
            }
            _ if a.starts_with('-') => {
                eprintln!("unknown option {a}");
                return ExitCode::from(2);
            }
            _ => dir = Some(PathBuf::from(a)),
        }
    }
    let dir = dir.unwrap_or_else(default_dir);
    let master_path = dir.join("catalog.v1.json");
    let text = match fs::read_to_string(&master_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{}: {e}", master_path.display());
            return ExitCode::from(2);
        }
    };
    let master: Master = match serde_json::from_str(&text) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("{}: {e}", master_path.display());
            return ExitCode::from(1);
        }
    };

    let mut r = Report::default();
    check_master(&master, &mut r);
    if r.errors.is_empty() {
        check_localised(&master, &dir, write, &mut r);
    } else if write {
        r.warn("localised files not written because the master has errors");
    }

    print_stats(&master);
    for w in &r.warnings {
        println!("warning: {w}");
    }
    for e in &r.errors {
        println!("error: {e}");
    }
    if r.errors.is_empty() {
        println!(
            "OK: {} warnings{}",
            r.warnings.len(),
            if write { ", localised files written" } else { ", localised files up to date" }
        );
        ExitCode::SUCCESS
    } else {
        println!("FAILED: {} errors, {} warnings", r.errors.len(), r.warnings.len());
        ExitCode::from(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds() {
        assert_eq!(normalise("Rugbrød"), "rugbroed");
        assert_eq!(normalise("Æblejuice"), "aeblejuice");
        assert_eq!(normalise("Hindbær"), "hindbaer");
        assert_eq!(normalise("Blåbær"), "blaabaer");
        assert_eq!(normalise("Caffè  macinato"), "caffe macinato");
        assert_eq!(normalise("olio d’oliva"), "olio d oliva");
        assert_eq!(normalise("Coca-Cola"), "coca cola");
        assert_eq!(normalise("Crème fraîche"), "creme fraiche");
        assert_eq!(normalise("Müsli"), "musli");
    }

    #[test]
    fn icons() {
        assert!(icon_problem("🍌").is_none());
        assert!(icon_problem("🌶️").is_none());
        assert!(icon_problem("🧑‍🍳").is_none());
        assert!(icon_problem("svg:rye-bread").is_none());
        assert!(icon_problem("🍌🍌").is_some());
        assert!(icon_problem("a").is_some());
        assert!(icon_problem("").is_some());
    }
}
