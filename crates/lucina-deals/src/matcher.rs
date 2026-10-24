//! Heading matching (`matcher.js`) and grocery chain keys (`nearby.js`).

use std::sync::OnceLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization;

use crate::sources::{Deal, DealSource};
use crate::synonyms::Rule;

/// Fold a chain name or slug to one comparable key: "REMA 1000" / "rema" → "rema".
pub fn chain_key(value: &str) -> String {
    let folded: String = value
        .to_lowercase()
        .replace('æ', "ae")
        .replace('ø', "oe")
        .replace('å', "aa")
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .collect();
    match folded.as_str() {
        "rema1000" => "rema".into(),
        "coop365discount" | "coop365" => "365discount".into(),
        "fotex" => "foetex".into(),
        "lovbjerg" => "loevbjerg".into(),
        "dagligbrugsen" | "lokalbrugsen" => "brugsen".into(),
        _ => folded,
    }
}

pub const GROCERY_CHAINS: &[&str] = &[
    "foetex",
    "netto",
    "bilka",
    "rema",
    "lidl",
    "spar",
    "meny",
    "kvickly",
    "superbrugsen",
    "brugsen",
    "365discount",
    "loevbjerg",
    "aldi",
    "fakta",
    "irma",
    "minkoebmand",
    "letkoeb",
    "abcflowermarket",
];

pub fn is_grocery_chain(key: &str) -> bool {
    GROCERY_CHAINS.contains(&key)
}

/// Fold accents without losing Danish letters; "yoghurt" reads as "yogurt".
pub fn normalize(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.to_lowercase().chars() {
        if c.is_ascii() || matches!(c, 'æ' | 'ø' | 'å') {
            out.push(c);
        } else {
            out.extend(c.nfd().filter(|m| !('\u{300}'..='\u{36f}').contains(m)));
        }
    }
    out.replace("yoghurt", "yogurt")
}

fn token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[\p{L}\p{N}]+").expect("token regex"))
}

fn letters_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\p{L}{2,}$").expect("letters regex"))
}

pub fn tokens(value: &str) -> Vec<String> {
    let n = normalize(value);
    token_re()
        .find_iter(&n)
        .map(|m| m.as_str().to_owned())
        .collect()
}

pub const GLOBAL_EXCLUDES: &[&str] = &[
    "pizza",
    "toast",
    "panini",
    "sandwich",
    "chips",
    "snack",
    "dressing",
    "shampoo",
    "rejeost",
    "skinkeost",
    "schnitzler",
    "schnitzel",
];

/// Exact words outrank head-final Danish compounds; phrases require adjacent words.
pub fn term_score(text: &str, term: &str) -> i32 {
    let words = tokens(text);
    let needle = tokens(term);
    if needle.is_empty() {
        return 0;
    }
    if words.len() >= needle.len() {
        for i in 0..=words.len() - needle.len() {
            if needle.iter().enumerate().all(|(j, w)| words[i + j] == *w) {
                return 1000 - i.min(400) as i32;
            }
        }
    }
    if needle.len() == 1 {
        let head = &needle[0];
        if let Some(i) = words.iter().position(|w| {
            w.strip_suffix(head.as_str())
                .is_some_and(|prefix| letters_re().is_match(prefix))
        }) {
            return 500 - i.min(400) as i32;
        }
    }
    0
}

/// Match headings only; food waste must also have an allowed category.
pub fn match_deal(rule: &Rule, deal: &Deal) -> i32 {
    let name = deal.name_text();
    if GLOBAL_EXCLUDES
        .iter()
        .copied()
        .chain(rule.exclude.iter().map(String::as_str))
        .any(|term| term_score(&name, term) != 0)
    {
        return 0;
    }
    if deal.source == DealSource::FoodWaste {
        if let Some(categories) = rule.categories.as_ref().filter(|c| !c.is_empty()) {
            let category = normalize(deal.category.as_deref().unwrap_or(""));
            if !categories.iter().any(|c| category.contains(&normalize(c))) {
                return 0;
            }
        }
    }
    rule.terms
        .iter()
        .map(|term| term_score(&name, term))
        .fold(0, i32::max)
}
