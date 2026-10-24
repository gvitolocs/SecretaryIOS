//! Bounded English/Italian My Clippings parser; unknown metadata remains visible.
use crate::{model::*, search::book_key, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 200_000;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClippingEntry {
    pub title: String,
    pub authors: Vec<String>,
    pub highlight: Highlight,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClippingsParse {
    pub entries: Vec<ClippingEntry>,
    pub warnings: Vec<String>,
}
pub fn dedup_key(title: &str, authors: &[String], h: &Highlight) -> String {
    // Preserve punctuation/case in quoted text; normalize only whitespace.
    format!(
        "{}|{:?}|{:?}|{}",
        book_key(title, authors),
        h.kind,
        h.location,
        h.text.split_whitespace().collect::<Vec<_>>().join(" ")
    )
}
fn number_after(s: &str, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let tail = s.split_once(key)?.1.trim_start();
        let v: String = tail
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        (!v.is_empty()).then_some(v)
    })
}
fn date(raw: &str) -> Option<String> {
    let mut s = raw.trim().to_owned();
    for (it, en) in [
        ("gennaio", "January"),
        ("febbraio", "February"),
        ("marzo", "March"),
        ("aprile", "April"),
        ("maggio", "May"),
        ("giugno", "June"),
        ("luglio", "July"),
        ("agosto", "August"),
        ("settembre", "September"),
        ("ottobre", "October"),
        ("novembre", "November"),
        ("dicembre", "December"),
    ] {
        s = s.replace(it, en);
    }
    // Kindle dates lack a time zone: retain the raw date and serialize a naive
    // ISO local datetime, never invent a UTC offset.
    let s = s
        .split_once(' ')
        .map(|(_, r)| r)
        .unwrap_or(&s)
        .trim_start_matches(',')
        .trim();
    for fmt in [
        "%d %B %Y %H:%M:%S",
        "%B %e, %Y %I:%M:%S %p",
        "%d %B %Y %I:%M:%S %p",
    ] {
        if let Ok(d) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Some(d.format("%Y-%m-%dT%H:%M:%S").to_string());
        }
    }
    None
}
pub fn parse(bytes: &[u8]) -> Result<ClippingsParse> {
    if bytes.len() > MAX_BYTES {
        return Err(Error::Invalid("clippings exceed 64 MiB".into()));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::Invalid("clippings must be UTF-8".into()))?
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n");
    let mut out = ClippingsParse::default();
    let mut unique = BTreeMap::new();
    let mut chunk = Vec::new();
    let mut count = 0;
    for line in text.lines().chain(std::iter::once("==========")) {
        if line.trim() != "==========" {
            chunk.push(line);
            continue;
        }
        let leading = chunk.iter().take_while(|l| l.trim().is_empty()).count();
        chunk.drain(..leading);
        if chunk.is_empty() {
            continue;
        }
        count += 1;
        if count > MAX_ENTRIES {
            return Err(Error::Invalid("too many clippings".into()));
        }
        let heading = chunk[0].trim();
        let (title, authors) = if heading.ends_with(')') {
            if let Some(i) = heading.rfind(" (") {
                (
                    heading[..i].to_owned(),
                    heading[i + 2..heading.len() - 1]
                        .split(';')
                        .map(|a| a.trim().to_owned())
                        .collect(),
                )
            } else {
                (heading.to_owned(), vec![])
            }
        } else {
            (heading.to_owned(), vec![])
        };
        let meta = chunk.get(1).copied().unwrap_or("");
        let lower = meta.to_lowercase();
        let kind = if lower.contains("your note") || lower.contains("la tua nota") {
            HighlightKind::Note
        } else if lower.contains("your bookmark") || lower.contains("il tuo segnalibro") {
            HighlightKind::Bookmark
        } else {
            if !lower.contains("your highlight") && !lower.contains("la tua evidenziazione") {
                out.warnings
                    .push(format!("entry {count}: unknown metadata"));
            }
            HighlightKind::Highlight
        };
        let location = number_after(&lower, &["location ", "posizione "]).and_then(|v| {
            let (a, b) = v.split_once('-').unwrap_or((&v, &v));
            Some(LocRange {
                start: a.parse().ok()?,
                end: b.parse().ok()?,
            })
        });
        let raw = meta
            .split_once("Added on ")
            .or_else(|| meta.split_once("Aggiunto in data "))
            .map(|(_, r)| r.trim())
            .unwrap_or("")
            .to_owned();
        let added = date(&raw);
        if added.is_none() {
            out.warnings
                .push(format!("entry {count}: unparseable date"));
        }
        let mut h = Highlight {
            id: String::new(),
            book_id: String::new(),
            kind,
            text: chunk
                .iter()
                .skip(2)
                .copied()
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_owned(),
            note: None,
            location,
            page: number_after(&lower, &["page ", "pagina "]),
            added_at: added,
            added_at_raw: raw,
            source: "kindle-clippings".into(),
            provenance: Provenance {
                file: "documents/My Clippings.txt".into(),
                entry_index: count - 1,
                device: None,
            },
            dedup_key: String::new(),
        };
        h.dedup_key = dedup_key(&title, &authors, &h);
        h.id = crate::import::stable_id("hl_", &h.dedup_key);
        unique.entry(h.dedup_key.clone()).or_insert(ClippingEntry {
            title,
            authors,
            highlight: h,
        });
        chunk.clear();
    }
    out.entries = unique.into_values().collect();
    attach_notes(&mut out.entries);
    Ok(out)
}
pub fn attach_notes(entries: &mut [ClippingEntry]) {
    let mut notes: BTreeMap<(String, u32), BTreeSet<String>> = BTreeMap::new();
    for e in entries
        .iter()
        .filter(|e| e.highlight.kind == HighlightKind::Note)
    {
        if let Some(loc) = &e.highlight.location {
            notes
                .entry((book_key(&e.title, &e.authors), loc.end))
                .or_default()
                .insert(e.highlight.text.clone());
        }
    }
    for e in entries
        .iter_mut()
        .filter(|e| e.highlight.kind == HighlightKind::Highlight)
    {
        if let Some(loc) = &e.highlight.location {
            if let Some(texts) = notes.get(&(book_key(&e.title, &e.authors), loc.end)) {
                e.highlight.note = Some(texts.iter().cloned().collect::<Vec<_>>().join("\n"));
            }
        }
    }
}
