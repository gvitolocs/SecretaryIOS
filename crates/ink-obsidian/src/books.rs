//! Book references in notes. Titles are matched with ink-library's title and
//! author keys. A note that mentions a book is a relation, never evidence that
//! the owner read it.
//!
//! - Frontmatter: `lucina_book_id`, `isbn`, `asin`, `book`/`books` (titles or
//!   wikilinks), and `title` + `author`/`authors` for a note about a book.
//! - Wikilinks: a link whose target name or alias is a book title.
//! - Mentions: plain text containing a title key with at least two significant
//!   words (case/diacritic-insensitive), or a multi-word title written exactly.
//!   Single-word titles are matched only through frontmatter or wikilinks.
//! - ISBN-10/13 (checksum-validated; ISBN-10 needs an `ISBN` label) and ASINs.
use crate::parse::{wikilinks, LinkKind, ParsedNote};
use ink_library::{
    model::Book,
    search::{author_key, clean_title, normalize, title_key, words},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use unicode_normalization::char::is_combining_mark;

pub const MAX_EXCERPT_CHARS: usize = 240;

/// The library fields needed for matching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryBook {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub isbn: Option<String>,
    #[serde(default)]
    pub asin: Option<String>,
}
impl From<&Book> for LibraryBook {
    fn from(b: &Book) -> Self {
        Self {
            id: b.id.clone(),
            title: b.title.clone(),
            authors: b.authors.clone(),
            isbn: b.isbn.clone(),
            asin: b.asin.clone(),
        }
    }
}

/// How a note refers to a book, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum How {
    Frontmatter,
    Isbn,
    Asin,
    Wikilink,
    Mention,
}
impl How {
    pub fn as_str(self) -> &'static str {
        match self {
            How::Frontmatter => "frontmatter",
            How::Isbn => "isbn",
            How::Asin => "asin",
            How::Wikilink => "wikilink",
            How::Mention => "mention",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BookRef {
    pub book_id: String,
    pub note_path: String,
    pub note_title: String,
    pub how: How,
    /// At most 240 characters of the line around the match.
    pub excerpt: String,
    /// 1-based line in the note file.
    pub line: u32,
}

const STOPWORDS: &[&str] = &[
    "and", "the", "for", "with", "from", "into", "that", "this", "von", "und", "der", "die", "das",
    "des", "del", "della", "dello", "dei", "degli", "delle", "per", "con", "che", "non", "una",
    "uno", "och", "med", "til", "det", "den", "les", "une", "aux", "sur", "som",
];
/// Words of a title key with 3+ characters that are not common function words.
pub fn significant_words(key: &str) -> usize {
    key.split_whitespace()
        .filter(|w| w.chars().count() >= 3 && !STOPWORDS.contains(w))
        .count()
}

fn isbn10_ok(d: &[u8]) -> bool {
    d.len() == 10
        && d[..9].iter().all(u8::is_ascii_digit)
        && (d[9].is_ascii_digit() || d[9] == b'X')
        && d.iter()
            .enumerate()
            .map(|(i, c)| (10 - i as u32) * if *c == b'X' { 10 } else { u32::from(c - b'0') })
            .sum::<u32>()
            % 11
            == 0
}
fn isbn13_ok(d: &[u8]) -> bool {
    d.len() == 13
        && d.iter().all(u8::is_ascii_digit)
        && d.iter()
            .enumerate()
            .map(|(i, c)| u32::from(c - b'0') * if i % 2 == 0 { 1 } else { 3 })
            .sum::<u32>()
            % 10
            == 0
}
fn isbn10_to_13(d: &[u8]) -> String {
    let mut s = format!("978{}", std::str::from_utf8(&d[..9]).unwrap_or(""));
    let sum: u32 = s
        .bytes()
        .enumerate()
        .map(|(i, c)| u32::from(c - b'0') * if i % 2 == 0 { 1 } else { 3 })
        .sum();
    s.push(char::from(b'0' + ((10 - sum % 10) % 10) as u8));
    s
}
/// Canonical ISBN-13 for a valid ISBN-10/13 in any punctuation, else `None`.
pub fn canonical_isbn(s: &str) -> Option<String> {
    let d: Vec<u8> = s
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if isbn13_ok(&d) {
        Some(String::from_utf8(d).unwrap_or_default())
    } else if isbn10_ok(&d) {
        Some(isbn10_to_13(&d))
    } else {
        None
    }
}
fn is_asin(s: &[u8]) -> bool {
    s.len() == 10
        && s.starts_with(b"B0")
        && s.iter()
            .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase())
}

/// ISBNs written in a line, as (canonical ISBN-13, byte start, byte end).
pub fn find_isbns(line: &str) -> Vec<(String, usize, usize)> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() || (i > 0 && b[i - 1].is_ascii_alphanumeric()) {
            i += 1;
            continue;
        }
        let start = i;
        let mut digits = Vec::new();
        let mut ends = Vec::new();
        let mut j = i;
        while j < b.len() && digits.len() < 13 {
            let c = b[j];
            if c.is_ascii_digit() || (digits.len() == 9 && matches!(c, b'X' | b'x')) {
                digits.push(c.to_ascii_uppercase());
                ends.push(j + 1);
            } else if matches!(c, b'-' | b' ')
                && b[j - 1].is_ascii_alphanumeric()
                && b.get(j + 1).is_some_and(u8::is_ascii_digit)
            {
            } else {
                break;
            }
            j += 1;
        }
        let bounded = |end: usize| b.get(end).is_none_or(|c| !c.is_ascii_alphanumeric());
        let label = line[..start]
            .trim_end()
            .trim_end_matches(':')
            .trim_end()
            .to_ascii_uppercase();
        let labelled = ["ISBN", "ISBN-10", "ISBN-13", "ISBN10", "ISBN13"]
            .iter()
            .any(|l| label.ends_with(l));
        if digits.len() == 13
            && bounded(ends[12])
            && isbn13_ok(&digits)
            && (labelled || digits.starts_with(b"978") || digits.starts_with(b"979"))
        {
            out.push((
                String::from_utf8(digits.clone()).unwrap_or_default(),
                start,
                ends[12],
            ));
        } else if digits.len() >= 10 && labelled && bounded(ends[9]) && isbn10_ok(&digits[..10]) {
            out.push((isbn10_to_13(&digits[..10]), start, ends[9]));
        }
        i = j.max(i + 1);
    }
    out
}

/// Word tokens of a line (folded like `title_key`) with their byte spans.
fn tokens(line: &str) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in line
        .char_indices()
        .chain(std::iter::once((line.len(), ' ')))
    {
        let w = i < line.len() && (c.is_alphanumeric() || is_combining_mark(c));
        match (w, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                for t in words(&line[s..i]).split_whitespace() {
                    out.push((t.to_owned(), s, i));
                }
                start = None;
            }
            _ => {}
        }
    }
    out
}

/// At most 240 characters of `line` around the byte range, whitespace collapsed.
pub fn excerpt(line: &str, start: usize, end: usize) -> String {
    let chars: Vec<usize> = line.char_indices().map(|(i, _)| i).collect();
    let (from, to) = if chars.len() <= MAX_EXCERPT_CHARS {
        (0, line.len())
    } else {
        let s = chars.iter().position(|i| *i >= start).unwrap_or(0);
        let e = chars.iter().position(|i| *i >= end).unwrap_or(chars.len());
        let pad = MAX_EXCERPT_CHARS.saturating_sub(e.saturating_sub(s)) / 2;
        let from = s.saturating_sub(pad).min(chars.len() - MAX_EXCERPT_CHARS);
        let to = from + MAX_EXCERPT_CHARS;
        (chars[from], chars.get(to).copied().unwrap_or(line.len()))
    };
    line[from..to]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_EXCERPT_CHARS)
        .collect()
}

/// Lookup tables over the library. `fingerprint` changes when any matched field does.
pub struct LibraryIndex {
    books: BTreeMap<String, LibraryBook>,
    by_key: BTreeMap<String, Vec<String>>,
    /// First token → (title key tokens, book id), for unique keys with 2+ significant words.
    phrases: BTreeMap<String, Vec<(Vec<String>, String)>>,
    /// Exact multi-word display titles of books whose key is too weak to fold.
    exact: Vec<(String, String)>,
    isbns: BTreeMap<String, Vec<String>>,
    asins: BTreeMap<String, Vec<String>>,
    pub fingerprint: String,
}

impl LibraryIndex {
    pub fn new(books: &[LibraryBook]) -> Self {
        let mut sorted: Vec<_> = books.to_vec();
        sorted.sort_by(|a, b| a.id.cmp(&b.id));
        sorted.dedup_by(|a, b| a.id == b.id);
        let fingerprint = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&sorted).unwrap_or_default())
        );
        let mut ix = LibraryIndex {
            books: BTreeMap::new(),
            by_key: BTreeMap::new(),
            phrases: BTreeMap::new(),
            exact: Vec::new(),
            isbns: BTreeMap::new(),
            asins: BTreeMap::new(),
            fingerprint,
        };
        for b in sorted {
            let key = title_key(&b.title);
            if !key.is_empty() {
                ix.by_key.entry(key).or_default().push(b.id.clone());
            }
            if let Some(i) = b.isbn.as_deref().and_then(canonical_isbn) {
                ix.isbns.entry(i).or_default().push(b.id.clone());
            }
            if let Some(a) = b.asin.as_deref().map(|a| a.trim().to_uppercase()) {
                if !a.is_empty() {
                    ix.asins.entry(a).or_default().push(b.id.clone());
                }
            }
            ix.books.insert(b.id.clone(), b);
        }
        for (key, ids) in &ix.by_key {
            if ids.len() != 1 {
                continue;
            }
            if significant_words(key) >= 2 {
                let toks: Vec<String> = key.split_whitespace().map(str::to_owned).collect();
                ix.phrases
                    .entry(toks[0].clone())
                    .or_default()
                    .push((toks, ids[0].clone()));
            } else {
                let title = clean_title(&ix.books[&ids[0]].title);
                if words(&title).split_whitespace().count() >= 2 {
                    ix.exact.push((title, ids[0].clone()));
                }
            }
        }
        for v in ix.phrases.values_mut() {
            // Longest phrase first, so the most specific title wins at a position.
            v.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(a.1.cmp(&b.1)));
        }
        ix
    }

    pub fn len(&self) -> usize {
        self.books.len()
    }
    pub fn is_empty(&self) -> bool {
        self.books.is_empty()
    }
    pub fn contains(&self, id: &str) -> bool {
        self.books.contains_key(id)
    }

    /// A unique book whose title `text` names, optionally confirmed by author.
    /// Needs a key with 2+ significant words, or text equal to the main or full title.
    pub fn match_title(&self, text: &str, authors: Option<&[String]>) -> Option<&str> {
        let text = clean_title(text);
        let key = title_key(&text);
        if key.is_empty() {
            return None;
        }
        let full = normalize(&text);
        let strong = significant_words(&key) >= 2 || full == key;
        let cands: Vec<&LibraryBook> = self
            .by_key
            .get(&key)?
            .iter()
            .filter_map(|id| self.books.get(id))
            .filter(|b| strong || normalize(&clean_title(&b.title)) == full)
            .collect();
        let wanted = authors.map(author_key).filter(|a| !a.is_empty());
        let hits: Vec<_> = match wanted {
            Some(a) => cands
                .into_iter()
                .filter(|b| author_key(&b.authors) == a)
                .collect(),
            None => cands,
        };
        (hits.len() == 1).then(|| hits[0].id.as_str())
    }
    fn unique<'a>(map: &'a BTreeMap<String, Vec<String>>, key: &str) -> Option<&'a str> {
        map.get(key).filter(|v| v.len() == 1).map(|v| v[0].as_str())
    }
    pub fn match_isbn(&self, s: &str) -> Option<&str> {
        Self::unique(&self.isbns, &canonical_isbn(s)?)
    }
    pub fn match_asin(&self, s: &str) -> Option<&str> {
        Self::unique(&self.asins, &s.trim().to_uppercase())
    }

    /// Plain-text title mentions in one line: (book id, byte start, byte end).
    pub fn mentions(&self, line: &str) -> Vec<(String, usize, usize)> {
        let mut out = Vec::new();
        let toks = tokens(line);
        let mut i = 0;
        while i < toks.len() {
            let hit = self.phrases.get(&toks[i].0).and_then(|cands| {
                cands.iter().find(|(k, _)| {
                    i + k.len() <= toks.len()
                        && toks[i..i + k.len()].iter().zip(k).all(|(a, b)| a.0 == *b)
                })
            });
            if let Some((k, id)) = hit {
                out.push((id.clone(), toks[i].1, toks[i + k.len() - 1].2));
                i += k.len();
            } else {
                i += 1;
            }
        }
        for (title, id) in &self.exact {
            let mut from = 0;
            while let Some(p) = line[from..].find(title.as_str()) {
                let s = from + p;
                let e = s + title.len();
                let before = line[..s].chars().next_back();
                let after = line[e..].chars().next();
                if before.is_none_or(|c| !c.is_alphanumeric())
                    && after.is_none_or(|c| !c.is_alphanumeric())
                {
                    out.push((id.clone(), s, e));
                }
                from = e;
            }
        }
        out
    }
}

/// Book references made by one note itself, and the books it is a note about
/// (frontmatter `lucina_book_id` or `title` + author).
pub fn note_refs(lib: &LibraryIndex, path: &str, note: &ParsedNote) -> (Vec<BookRef>, Vec<String>) {
    let mut refs = Vec::new();
    let mut about = Vec::new();
    let push = |refs: &mut Vec<BookRef>, id: &str, how: How, excerpt: String, line: u32| {
        refs.push(BookRef {
            book_id: id.to_owned(),
            note_path: path.to_owned(),
            note_title: note.title.clone(),
            how,
            excerpt,
            line,
        });
    };
    let fm = &note.frontmatter;
    let fm_line = |k: &str| fm.lines.get(k).copied().unwrap_or(1);
    let fm_excerpt = |k: &str| {
        let v = fm.get(k).map(|v| v.values().join(", ")).unwrap_or_default();
        let s = format!("{k}: {v}");
        excerpt(&s, 0, s.len())
    };
    if let Some(id) = fm.scalar("lucina_book_id").filter(|id| lib.contains(id)) {
        push(
            &mut refs,
            id,
            How::Frontmatter,
            fm_excerpt("lucina_book_id"),
            fm_line("lucina_book_id"),
        );
        about.push(id.to_owned());
    }
    let authors: Vec<String> = ["author", "authors"]
        .iter()
        .filter_map(|k| fm.get(k))
        .flat_map(|v| v.values())
        .map(|a| strip_link(a).to_owned())
        .filter(|a| !a.is_empty())
        .collect();
    if let (Some(title), false) = (fm.scalar("title"), authors.is_empty()) {
        if let Some(id) = lib.match_title(title, Some(&authors)) {
            push(
                &mut refs,
                id,
                How::Frontmatter,
                fm_excerpt("title"),
                fm_line("title"),
            );
            about.push(id.to_owned());
        }
    }
    for key in ["book", "books"] {
        let Some(v) = fm.get(key) else { continue };
        for value in v.values() {
            let mut links = Vec::new();
            wikilinks(value, 0, &mut links);
            let names: Vec<String> = if links.is_empty() {
                vec![value.to_owned()]
            } else {
                links
                    .iter()
                    .map(|l| crate::parse::stem(&l.target).to_owned())
                    .collect()
            };
            for name in names {
                if let Some(id) = lib.match_title(&name, None) {
                    push(
                        &mut refs,
                        id,
                        How::Frontmatter,
                        fm_excerpt(key),
                        fm_line(key),
                    );
                }
            }
        }
    }
    if let Some(v) = fm.get("isbn") {
        for i in v.values() {
            if let Some(id) = lib.match_isbn(i) {
                push(
                    &mut refs,
                    id,
                    How::Frontmatter,
                    fm_excerpt("isbn"),
                    fm_line("isbn"),
                );
            }
        }
    }
    if let Some(v) = fm.get("asin") {
        for a in v.values() {
            if let Some(id) = lib.match_asin(a) {
                push(
                    &mut refs,
                    id,
                    How::Frontmatter,
                    fm_excerpt("asin"),
                    fm_line("asin"),
                );
            }
        }
    }
    for l in &note.links {
        if l.kind == LinkKind::Markdown {
            continue;
        }
        let mut names = vec![crate::parse::stem(&l.target)];
        names.extend(l.alias.as_deref());
        if let Some(id) = names.iter().find_map(|n| lib.match_title(n, None)) {
            let line = note.line(l.line).unwrap_or("");
            let (s, e) = line
                .find(&l.raw)
                .map(|s| (s, s + l.raw.len()))
                .unwrap_or((0, 0));
            push(&mut refs, id, How::Wikilink, excerpt(line, s, e), l.line);
        }
    }
    for (n, line) in &note.lines {
        for (id, s, e) in lib.mentions(line) {
            push(&mut refs, &id, How::Mention, excerpt(line, s, e), *n);
        }
        for (isbn, s, e) in find_isbns(line) {
            if let Some(id) = lib.match_isbn(&isbn) {
                push(&mut refs, id, How::Isbn, excerpt(line, s, e), *n);
            }
        }
        let b = line.as_bytes();
        for k in 0..b.len().saturating_sub(9) {
            if b[k] == b'B'
                && (k == 0 || !b[k - 1].is_ascii_alphanumeric())
                && b.get(k + 10).is_none_or(|c| !c.is_ascii_alphanumeric())
                && is_asin(&b[k..k + 10])
            {
                let word = std::str::from_utf8(&b[k..k + 10]).unwrap_or("");
                if let Some(id) = lib.match_asin(word) {
                    push(&mut refs, id, How::Asin, excerpt(line, k, k + 10), *n);
                }
            }
        }
    }
    about.sort();
    about.dedup();
    (refs, about)
}

fn strip_link(s: &str) -> &str {
    let s = s.trim();
    let s = s
        .strip_prefix("[[")
        .and_then(|s| s.strip_suffix("]]"))
        .unwrap_or(s);
    s.split('|').next().unwrap_or(s).trim()
}

/// One reference per (book, note, how) at its first line; a mention is dropped
/// when the same note refers to the book more strongly. Sorted.
pub fn finalize(mut refs: Vec<BookRef>) -> Vec<BookRef> {
    refs.sort_by(|a, b| {
        (&a.book_id, &a.note_path, a.how, a.line).cmp(&(&b.book_id, &b.note_path, b.how, b.line))
    });
    refs.dedup_by(|a, b| a.book_id == b.book_id && a.note_path == b.note_path && a.how == b.how);
    let strong: std::collections::BTreeSet<(String, String)> = refs
        .iter()
        .filter(|r| r.how != How::Mention)
        .map(|r| (r.book_id.clone(), r.note_path.clone()))
        .collect();
    refs.retain(|r| {
        r.how != How::Mention || !strong.contains(&(r.book_id.clone(), r.note_path.clone()))
    });
    refs
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn lib() -> LibraryIndex {
        let b = |id: &str, t: &str, a: &str, isbn: Option<&str>, asin: Option<&str>| LibraryBook {
            id: id.into(),
            title: t.into(),
            authors: vec![a.into()],
            isbn: isbn.map(Into::into),
            asin: asin.map(Into::into),
        };
        LibraryIndex::new(&[
            b(
                "bk_1",
                "The Great Gatsby",
                "F. Scott Fitzgerald",
                None,
                None,
            ),
            b("bk_2", "Dune", "Frank Herbert", Some("0-441-17271-7"), None),
            b("bk_3", "Love", "Someone", None, Some("B0TESTASIN")),
            b("bk_4", "The Road", "Cormac McCarthy", None, None),
            b("bk_5", "Twin Title", "A", None, None),
            b("bk_6", "Twin Title", "B", None, None),
        ])
    }
    #[test]
    fn isbn_checks() {
        assert_eq!(
            canonical_isbn("0-441-17271-7").as_deref(),
            Some("9780441172719")
        );
        assert_eq!(
            canonical_isbn("978-0-441-17271-9").as_deref(),
            Some("9780441172719")
        );
        assert!(canonical_isbn("978-0-441-17271-8").is_none());
        let found =
            find_isbns("ISBN: 0-441-17271-7, also 9780441172719 and 1234567890 call 0441172717");
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|f| f.0 == "9780441172719"));
        assert!(find_isbns("x97804411727190").is_empty());
    }
    #[test]
    fn title_rules() {
        let l = lib();
        assert_eq!(l.match_title("Great Gatsby", None), Some("bk_1"));
        assert_eq!(l.match_title("the great gatsby", None), Some("bk_1"));
        assert_eq!(l.match_title("Dune", None), Some("bk_2"));
        assert_eq!(l.match_title("Dune", Some(&["Other".into()])), None);
        assert_eq!(l.match_title("Twin Title", None), None);
        assert_eq!(
            l.match_title("Twin Title", Some(&["B".into()])),
            Some("bk_6")
        );
        let m = |s: &str| l.mentions(s).into_iter().map(|m| m.0).collect::<Vec<_>>();
        assert_eq!(m("I loved The Great Gatsby's ending"), vec!["bk_1"]);
        assert_eq!(m("on the great  gatsby."), vec!["bk_1"]);
        assert!(m("a dune, Dune and love, Love").is_empty());
        assert_eq!(m("Walking The Road today"), vec!["bk_4"]);
        assert!(m("the road ahead").is_empty());
        assert!(m("twin title").is_empty());
    }
    #[test]
    fn final_refs_prefer_strong() {
        let r = |how, line| BookRef {
            book_id: "bk_1".into(),
            note_path: "a.md".into(),
            note_title: "a".into(),
            how,
            excerpt: String::new(),
            line,
        };
        let out = finalize(vec![
            r(How::Mention, 3),
            r(How::Wikilink, 9),
            r(How::Wikilink, 2),
        ]);
        assert_eq!(out, vec![r(How::Wikilink, 2)]);
        assert_eq!(
            finalize(vec![r(How::Mention, 3), r(How::Mention, 1)]),
            vec![r(How::Mention, 1)]
        );
    }
    proptest! {
        #[test]
        fn excerpts_are_bounded(line in "\\PC{0,600}", a in 0usize..700, len in 0usize..300) {
            let s = line.char_indices().map(|(i, _)| i).find(|i| *i >= a.min(line.len())).unwrap_or(line.len());
            let e = line.char_indices().map(|(i, _)| i).find(|i| *i >= (s + len).min(line.len())).unwrap_or(line.len());
            prop_assert!(excerpt(&line, s, e).chars().count() <= MAX_EXCERPT_CHARS);
        }
    }
}
