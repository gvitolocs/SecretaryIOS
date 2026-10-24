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
pub fn normalize(s: &str) -> String {
    let s: String = fold(s)
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    let mut words: Vec<_> = s.split_whitespace().collect();
    if words
        .first()
        .is_some_and(|w| ["the", "a", "il", "la", "lo", "gli", "le"].contains(w))
    {
        words.remove(0);
    }
    words.join(" ")
}
pub fn book_key(title: &str, authors: &[String]) -> String {
    format!(
        "{}|{}",
        normalize(title),
        normalize(authors.first().map(String::as_str).unwrap_or(""))
    )
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
