//! Unicode folding and term search with explicit, durable source references.
use crate::model::*;
use serde::{Deserialize, Serialize};
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

pub fn fold(s: &str) -> String {
    s.to_lowercase()
        .nfkd()
        .filter(|c| !is_combining_mark(*c))
        .collect()
}
/// Remove invisible formatting marks that Kindle sometimes prefixes to titles.
pub fn clean_title(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '\u{feff}' | '\u{200b}'..='\u{200f}' | '\u{2060}'..='\u{2064}' | '\u{202a}'..='\u{202e}')).collect::<String>().trim().to_owned()
}
/// Folded alphanumeric words joined by single spaces (the token form of `title_key`).
pub fn words(s: &str) -> String {
    fold(s)
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn normalize(s: &str) -> String {
    let s = fold(s);
    let s = s
        .strip_prefix("l'")
        .or_else(|| s.strip_prefix("l’"))
        .unwrap_or(&s);
    let s = words(s);
    let mut words: Vec<_> = s.split_whitespace().collect();
    if words
        .first()
        .is_some_and(|w| ["the", "a", "il", "la", "lo", "gli", "le", "i"].contains(w))
    {
        words.remove(0);
    }
    words.join(" ")
}
/// A lossy matching key, never a replacement for the display title. Strip
/// balanced trailing edition/series parentheses, then the first subtitle
/// separator (`:`, `_ `, ` - `). A period is deliberately not a separator.
pub fn title_key(title: &str) -> String {
    let clean = clean_title(title);
    let mut title = clean.as_str();
    while title.ends_with(')') {
        let mut depth = 0usize;
        let mut start = None;
        for (i, c) in title.char_indices().rev() {
            match c {
                ')' => depth += 1,
                '(' => {
                    depth -= 1;
                    if depth == 0 {
                        start = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(start) = start else { break };
        title = title[..start].trim_end();
    }
    let end = [":", "_ ", " - "]
        .iter()
        .filter_map(|sep| title.find(sep))
        .min()
        .unwrap_or(title.len());
    normalize(&title[..end])
}
pub fn author_key(authors: &[String]) -> String {
    words(authors.first().map(String::as_str).unwrap_or(""))
}
pub fn book_key(title: &str, authors: &[String]) -> String {
    format!("{}|{}", title_key(title), author_key(authors))
}
pub fn matches(text: &str, query: &str) -> bool {
    let text = fold(text);
    fold(query).split_whitespace().all(|t| text.contains(t))
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub book_id: String,
    pub title: String,
    pub authors: Vec<String>,
    pub highlight_id: Option<String>,
    pub snippet: String,
    pub location: Option<LocRange>,
    pub page: Option<String>,
    pub source: String,
}
pub fn search(state: &LibraryState, q: &str, limit: usize) -> Vec<SearchResult> {
    let mut out = Vec::new();
    for b in state.books.values() {
        if matches(
            &format!("{} {} {}", b.title, b.authors.join(" "), b.tags.join(" ")),
            q,
        ) {
            out.push(SearchResult {
                book_id: b.id.clone(),
                title: b.title.clone(),
                authors: b.authors.clone(),
                highlight_id: None,
                snippet: b.title.clone(),
                location: None,
                page: None,
                source: "library".into(),
            });
        }
    }
    for h in state.highlights.values().flatten() {
        if matches(
            &format!("{} {}", h.text, h.note.as_deref().unwrap_or("")),
            q,
        ) {
            if let Some(b) = state.books.get(&h.book_id) {
                out.push(SearchResult {
                    book_id: b.id.clone(),
                    title: b.title.clone(),
                    authors: b.authors.clone(),
                    highlight_id: Some(h.id.clone()),
                    snippet: format!("{} {}", h.text, h.note.as_deref().unwrap_or(""))
                        .chars()
                        .take(400)
                        .collect(),
                    location: h.location.clone(),
                    page: h.page.clone(),
                    source: h.source.clone(),
                });
            }
        }
    }
    out.truncate(limit);
    out
}
