//! Matching the names a question uses against the titles of a calendar: "Teresa" finds "Therese
//! zoom", "café" finds "Cafe meetup", "meeting" is not required in the title.
//!
//! - [`fold`] / [`normalize`]: lowercase, no diacritics (è→e, ø→o, å→a, ß→ss), words split on
//!   anything that is not a letter or a digit.
//! - [`word_like`]: two words are the same name when they fold alike, or when their
//!   [`skeleton`]s are (th→t, ph→f, y→i, z→s, doubled letters single, one final vowel dropped:
//!   Therese, Teresa, Theresa and Tereza all become "teres"), or one edit apart for longer ones.
//!   Words under four letters stay exact.
//! - [`alias_statement`]: "meeting Therese è Teresa" says it is one person spelled two ways.
//!   [`PersonAliases`] keeps such pairs (a small JSON file the server writes) so the next
//!   question that names only one of them finds the other.

use std::path::Path;

use serde::{Deserialize, Serialize};
use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

/// Words a title need not contain, folded: what kind of event it is ("the Therese call").
const GENERIC: &[&str] = &[
    "meeting",
    "meetings",
    "zoom",
    "call",
    "calls",
    "meet",
    "teams",
    "riunione",
    "riunioni",
    "incontro",
    "incontri",
    "appuntamento",
    "appuntamenti",
    "chiamata",
    "chiamate",
    "videochiamata",
    "videocall",
    // møde, mødet, møder
    "mode",
    "modet",
    "moder",
];

/// Question and glue words that are never a name (folded). `e`, `o`, `is`, `or` and `aka` are
/// the alias connectors; `e` is also the folded "è".
const GLUE: &[&str] = &[
    "con", "per", "da", "in", "at", "on", "for", "e", "o", "or", "is", "aka", "and", "og", "il",
    "la", "lo", "le", "gli", "un", "una", "uno", "a", "an", "to", "al", "alla", "dei", "dal",
    "dalla", "this", "that", "questo", "questa", "when", "where", "what", "who", "which", "time",
    "you", "know", "tell", "me", "dove", "quando", "che", "cosa", "chi", "quale", "ora", "hvor",
    "hvornar", "hvad", "hvem", "tid", "mit", "min", "mia", "mio", "my",
];

/// Spellings of "is" between two names.
const CONNECTORS: &[&str] = &["è", "e'", "é", "is", "=", "o", "or", "aka", "a.k.a"];
/// Connectors that do not claim the two names are alike by themselves.
const EITHER: &[&str] = &["o", "or"];

/// Lowercase without diacritics: "Jørgen" → "jorgen", "café" → "cafe", "Straße" → "strasse".
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.to_lowercase().nfd().filter(|c| !is_combining_mark(*c)) {
        match c {
            'ø' => out.push('o'),
            'æ' => out.push_str("ae"),
            'œ' => out.push_str("oe"),
            'ß' => out.push_str("ss"),
            'ł' => out.push('l'),
            'đ' | 'ð' => out.push('d'),
            'þ' => out.push_str("th"),
            'ı' => out.push('i'),
            c => out.push(c),
        }
    }
    out
}

/// [`fold`] with every run of non-letters and non-digits as one space.
pub fn normalize(s: &str) -> String {
    fold(s)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The name stripped of spelling variants (input folded): "therese", "teresa", "theresa" and
/// "tereza" are all "teres".
pub fn skeleton(w: &str) -> String {
    let mut s = w.to_owned();
    for (from, to) in [
        ("ph", "f"),
        ("th", "t"),
        ("ck", "k"),
        ("q", "k"),
        ("y", "i"),
        ("z", "s"),
    ] {
        s = s.replace(from, to);
    }
    // Hard c: "Carla" and "Karla".
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<char> = Vec::with_capacity(chars.len());
    for (i, &c) in chars.iter().enumerate() {
        let c = match (c, chars.get(i + 1)) {
            ('c', Some('a' | 'o' | 'u')) => 'k',
            _ => c,
        };
        if out.last() != Some(&c) {
            out.push(c);
        }
    }
    if out.len() > 2 && matches!(out.last(), Some('a' | 'e' | 'i' | 'o' | 'u')) {
        out.pop();
    }
    out.into_iter().collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut row = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            row.push(sub.min(prev[j + 1] + 1).min(row[j] + 1));
        }
        prev = row;
    }
    prev[b.len()]
}

/// Two folded words are the same name: equal, or alike after [`skeleton`] (one edit apart when
/// the skeleton has four letters or more). Words under four letters must be equal.
pub fn word_like(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if a.chars().count() < 4 || b.chars().count() < 4 {
        return false;
    }
    let (sa, sb) = (skeleton(a), skeleton(b));
    let (la, lb) = (sa.chars().count(), sb.chars().count());
    if la < 3 || lb < 3 {
        return false;
    }
    sa == sb || (la >= 4 && lb >= 4 && edit_distance(&sa, &sb) <= 1)
}

/// The word says what kind of event it is, or is glue: not a name to look for.
fn skipped(folded: &str) -> bool {
    GENERIC.contains(&folded)
        || GLUE.contains(&folded)
        || crate::calendar_event::FILLER
            .iter()
            .any(|f| fold(f) == folded)
}

/// The words of `text`, with apostrophes kept inside a word ("l'amica" stays one token).
fn tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace('=', " = ")
        .split_whitespace()
        .map(|t| {
            t.trim_matches(|c: char| {
                !c.is_alphanumeric() && c != '=' && c != '\'' && c != '’' && c != '.'
            })
            .trim_end_matches('.')
            .to_owned()
        })
        .filter(|t| !t.is_empty())
        .collect()
}

fn name_word(token: &str) -> Option<String> {
    let f = normalize(token);
    (f.chars().count() >= 3 && !f.contains(' ') && !skipped(&f)).then_some(f)
}

/// "X è Y", "X is Y", "X = Y", "X o Y", "X or Y", "X aka Y": one name on each side of the
/// connector, the nearest one on the left (the generic words between do not count) and the only
/// one on the right. The names come back as written, lowercased.
fn parse_alias(text: &str) -> Option<(String, String, &'static str)> {
    let toks = tokens(text);
    for (i, t) in toks.iter().enumerate() {
        let Some(conn) = CONNECTORS.iter().find(|c| **c == t.as_str()) else {
            continue;
        };
        let left = toks[..i].iter().rev().find_map(|l| {
            match name_word(l) {
                Some(_) => Some(Some(l)),
                // Only generic words and glue are skipped; another word ends the search.
                None if skipped(&normalize(l)) => None,
                None => Some(None),
            }
        });
        let Some(Some(left)) = left else { continue };
        let mut right = toks[i + 1..].iter().filter(|r| name_word(r).is_some());
        let (Some(right), None) = (right.next(), right.next()) else {
            continue;
        };
        if normalize(left) == normalize(right) {
            continue;
        }
        return Some((left.clone(), right.clone(), conn));
    }
    None
}

/// The two names the text says are one, when it says so ("meeting therese è teresa").
pub fn alias_statement(text: &str) -> Option<(String, String)> {
    parse_alias(text).map(|(a, b, _)| (a, b))
}

/// [`alias_statement`] when it is worth keeping: "è", "is", "=" and "aka" say it outright; "o"
/// and "or" only when the two names are spelled alike.
pub fn alias_to_remember(text: &str) -> Option<(String, String)> {
    let (a, b, conn) = parse_alias(text)?;
    (!EITHER.contains(&conn) || word_like(&normalize(&a), &normalize(&b))).then_some((a, b))
}

/// Names that are one person, as the owner told Lucina ("therese è teresa"). Folded words.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonAliases {
    groups: Vec<Vec<String>>,
}

impl PersonAliases {
    const MAX_GROUPS: usize = 200;
    const MAX_GROUP: usize = 8;

    /// The file at `path`; empty when it is missing or unreadable.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        lucina_store::atomic::write_atomic(path, &json)
    }

    /// `a` and `b` are one person. `false` when that was known already (or refused: same
    /// word, too short, store full).
    pub fn add(&mut self, a: &str, b: &str) -> bool {
        let (a, b) = (normalize(a), normalize(b));
        if a == b || a.is_empty() || b.is_empty() || a.contains(' ') || b.contains(' ') {
            return false;
        }
        let has = |g: &Vec<String>, w: &str| g.iter().any(|x| x == w);
        let mut merged: Vec<String> = Vec::new();
        let mut rest = Vec::new();
        for g in std::mem::take(&mut self.groups) {
            if has(&g, &a) || has(&g, &b) {
                for w in g {
                    if !merged.contains(&w) {
                        merged.push(w);
                    }
                }
            } else {
                rest.push(g);
            }
        }
        let known = merged.contains(&a) && merged.contains(&b);
        for w in [a, b] {
            if !merged.contains(&w) {
                merged.push(w);
            }
        }
        let fits = merged.len() <= Self::MAX_GROUP && rest.len() < Self::MAX_GROUPS;
        self.groups = rest;
        if fits {
            self.groups.push(merged);
        }
        fits && !known
    }

    /// The other spellings of `word` (folded), not `word` itself.
    pub fn alternatives(&self, word: &str) -> Vec<String> {
        let w = normalize(word);
        self.groups
            .iter()
            .find(|g| g.contains(&w))
            .map(|g| g.iter().filter(|x| **x != w).cloned().collect())
            .unwrap_or_default()
    }

    /// Save `a` = `b` in the file at `path`. `Ok(true)` when it changed.
    pub fn remember(path: &Path, a: &str, b: &str) -> std::io::Result<bool> {
        let mut store = Self::load(path);
        let changed = store.add(a, b);
        if changed {
            store.save(path)?;
        }
        Ok(changed)
    }
}

/// The alternatives for each name the topic asks for: every word that is not generic or glue,
/// with the spellings the message itself gave ("therese è teresa") and the ones in `aliases`.
/// Two words of the same person make one group, which one title word satisfies.
fn groups(topic: &str, aliases: &PersonAliases) -> Vec<Vec<String>> {
    let stated = alias_statement(topic).map(|(a, b)| (normalize(&a), normalize(&b)));
    let mut out: Vec<Vec<String>> = Vec::new();
    for tok in tokens(topic) {
        let w = normalize(&tok);
        if w.is_empty() || w.contains(' ') || skipped(&w) {
            continue;
        }
        let mut alts = vec![w.clone()];
        if let Some((a, b)) = &stated {
            for (x, y) in [(a, b), (b, a)] {
                if *x == w {
                    alts.push(y.clone());
                }
            }
        }
        for alt in alts.clone() {
            alts.extend(aliases.alternatives(&alt));
        }
        alts.sort();
        alts.dedup();
        if !out.contains(&alts) {
            out.push(alts);
        }
    }
    out
}

/// The topic names something in `hay` (a title or a location): the whole topic is in it, or every
/// name in the topic (or one of its aliases) is in it as a word or a spelling variant of one.
pub fn topic_matches(topic: &str, hay: &str, aliases: &PersonAliases) -> bool {
    let (topic_n, hay_n) = (normalize(topic), normalize(hay));
    if !topic_n.is_empty() && hay_n.contains(&topic_n) {
        return true;
    }
    let groups = groups(topic, aliases);
    let words: Vec<&str> = hay_n.split(' ').collect();
    !groups.is_empty()
        && groups.iter().all(|alts| {
            alts.iter().any(|alt| {
                hay_n.contains(alt.as_str()) || words.iter().any(|hw| word_like(alt, hw))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_diacritics() {
        assert_eq!(fold("Jørgen"), "jorgen");
        assert_eq!(fold("CAFÉ è"), "cafe e");
        assert_eq!(fold("Åse Müller"), "ase muller");
        assert_eq!(normalize("Hack Night: Rust-meets AI!"), "hack night rust meets ai");
    }

    #[test]
    fn spelling_variants_of_a_name_are_alike() {
        for (a, b) in [
            ("therese", "teresa"),
            ("therese", "theresa"),
            ("teresa", "tereza"),
            ("tereza", "theresa"),
            ("carla", "karla"),
            ("stephen", "stefen"),
        ] {
            assert!(word_like(a, b), "{a} {b}");
        }
        for (a, b) in [
            ("teresa", "tessa"),
            ("teresa", "theater"),
            ("anna", "anne"),
            ("cena", "cane"),
            ("eva", "ava"),
        ] {
            assert!(!word_like(a, b), "{a} {b}");
        }
    }

    #[test]
    fn statements_that_two_names_are_one() {
        let pair = |t: &str| alias_statement(t);
        assert_eq!(
            pair("meeting therese è teresa"),
            Some(("therese".into(), "teresa".into()))
        );
        assert_eq!(
            pair("You know what time is the meeting Therese is Teresa?"),
            Some(("therese".into(), "teresa".into()))
        );
        assert_eq!(
            pair("Peppe = Giuseppe"),
            Some(("peppe".into(), "giuseppe".into()))
        );
        assert_eq!(
            pair("Peppe aka Giuseppe"),
            Some(("peppe".into(), "giuseppe".into()))
        );
        // The same name in two spellings of the letters is not two names.
        assert_eq!(pair("Jorgen = Jørgen"), None);
        assert_eq!(pair("Teresa o Therese"), Some(("teresa".into(), "therese".into())));
        // No statement: the question's own "is", more than one name on the right, no name left.
        assert_eq!(pair("what time is the meeting"), None);
        assert_eq!(pair("when is the Hack Night"), None);
        assert_eq!(pair("where is Anna"), None);
        assert_eq!(pair("hack night è rust meets ai"), None);
        // "o" between two unlike words is only an either-or, not worth keeping.
        assert!(alias_statement("cena o pranzo").is_some());
        assert_eq!(alias_to_remember("cena o pranzo"), None);
        assert!(alias_to_remember("teresa o therese").is_some());
        assert!(alias_to_remember("peppe è giuseppe").is_some());
    }

    #[test]
    fn the_topic_matches_titles_through_spelling_accents_and_generic_words() {
        let none = PersonAliases::default();
        assert!(topic_matches("meeting therese è teresa", "Therese zoom", &none));
        assert!(topic_matches("riunione con teresa", "Therese zoom", &none));
        assert!(topic_matches("theresa call", "Therese zoom", &none));
        assert!(topic_matches("café", "Cafe meetup", &none));
        assert!(topic_matches("Jørgen", "Jorgen", &none));
        assert!(topic_matches("hack night", "Hack Night: Rust meets AI", &none));
        assert!(!topic_matches("teresa", "Tessa party", &none));
        assert!(!topic_matches("teresa", "Theater", &none));
        assert!(!topic_matches("cena da anna", "Hack Night: Rust meets AI", &none));
        assert!(!topic_matches("meeting", "Tessa party", &none));
    }

    #[test]
    fn aliases_round_trip_through_the_file() {
        let dir = std::env::temp_dir().join(format!("lucina-aliases-{}", std::process::id()));
        let path = dir.join("person-aliases.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(PersonAliases::load(&path).alternatives("gianni").is_empty());
        assert!(PersonAliases::remember(&path, "Gianni", "Giovanni").unwrap());
        assert!(!PersonAliases::remember(&path, "giovanni", "gianni").unwrap());
        assert!(PersonAliases::remember(&path, "Therese", "Teresa").unwrap());
        assert!(PersonAliases::remember(&path, "Teresa", "Resa").unwrap());
        let store = PersonAliases::load(&path);
        assert_eq!(store.alternatives("Giovanni"), vec!["gianni".to_owned()]);
        assert_eq!(store.alternatives("teresa").len(), 2);
        // "gianni" alone now finds "Giovanni's birthday"; without the store it does not.
        assert!(topic_matches("gianni", "Giovanni birthday", &store));
        assert!(!topic_matches("gianni", "Giovanni birthday", &PersonAliases::default()));
        assert!(topic_matches("resa", "Therese zoom", &store));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
