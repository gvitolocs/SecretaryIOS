//! Small, strict Markdown reader: frontmatter (`key: value`, flow and block
//! lists), title, tags and links. Fenced code and inline code are ignored.
//! Unsupported YAML (nested maps, block scalars, anchors) is counted, not guessed.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FmValue {
    Scalar(String),
    List(Vec<String>),
}
impl FmValue {
    pub fn values(&self) -> Vec<&str> {
        match self {
            FmValue::Scalar(s) => vec![s.as_str()],
            FmValue::List(v) => v.iter().map(String::as_str).collect(),
        }
    }
    pub fn first(&self) -> Option<&str> {
        self.values().into_iter().find(|s| !s.trim().is_empty())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frontmatter {
    pub fields: BTreeMap<String, FmValue>,
    /// 1-based line of each key.
    pub lines: BTreeMap<String, u32>,
    pub unsupported: usize,
}
impl Frontmatter {
    pub fn get(&self, key: &str) -> Option<&FmValue> {
        self.fields.get(key)
    }
    pub fn scalar(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(FmValue::first).map(str::trim)
    }
}

/// Split leading `---` frontmatter from the body. Returns the 1-based line on
/// which the body starts. Without a closing `---`/`...` there is no frontmatter.
pub fn split_frontmatter(text: &str) -> (Option<Frontmatter>, &str, u32) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut parts = text.split_inclusive('\n');
    let Some(first) = parts.next() else {
        return (None, text, 1);
    };
    if first.trim_end() != "---" {
        return (None, text, 1);
    }
    let mut offset = first.len();
    let mut lines = Vec::new();
    for (i, l) in parts.enumerate() {
        offset += l.len();
        let t = l.trim_end();
        if t == "---" || t == "..." {
            return (
                Some(parse_frontmatter(&lines)),
                &text[offset..],
                i as u32 + 3,
            );
        }
        lines.push((i as u32 + 2, t));
    }
    (None, text, 1)
}

fn parse_frontmatter(lines: &[(u32, &str)]) -> Frontmatter {
    let mut fm = Frontmatter::default();
    let mut list_key: Option<String> = None;
    for &(n, l) in lines {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if l.starts_with([' ', '\t']) || t.starts_with("- ") || t == "-" {
            if let (Some(k), Some(item)) = (&list_key, t.strip_prefix('-')) {
                if let Some(FmValue::List(v)) = fm.fields.get_mut(k) {
                    let item = unquote(strip_comment(item.trim()));
                    if !item.is_empty() {
                        v.push(item);
                    }
                    continue;
                }
            }
            fm.unsupported += 1;
            continue;
        }
        list_key = None;
        let Some((k, v)) = t.split_once(':') else {
            fm.unsupported += 1;
            continue;
        };
        let k = unquote(k.trim());
        let v = v.trim();
        if k.is_empty() {
            fm.unsupported += 1;
            continue;
        }
        let value = if v.is_empty() {
            list_key = Some(k.clone());
            FmValue::List(vec![])
        } else if v.starts_with("[[") {
            // Obsidian users often leave wikilinks unquoted; keep them verbatim.
            FmValue::Scalar(v.to_owned())
        } else if let Some(inner) = v.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
            FmValue::List(split_flow(inner))
        } else if v.starts_with(['{', '|', '>', '&', '*', '!', '%', '@', '`']) {
            fm.unsupported += 1;
            continue;
        } else {
            FmValue::Scalar(unquote(strip_comment(v)))
        };
        fm.lines.insert(k.clone(), n);
        fm.fields.insert(k, value);
    }
    fm
}
fn strip_comment(v: &str) -> &str {
    if v.starts_with(['"', '\'']) {
        v
    } else {
        v.find(" #").map(|i| v[..i].trim_end()).unwrap_or(v)
    }
}
pub fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        let mut out = String::new();
        let mut chars = s[1..s.len() - 1].chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(c) => out.push(c),
                    None => {}
                }
            } else {
                out.push(c);
            }
        }
        out
    } else if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        s[1..s.len() - 1].replace("''", "'")
    } else {
        s.to_owned()
    }
}
fn split_flow(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match (quote, c) {
            (None, '"' | '\'') => {
                quote = Some(c);
                cur.push(c);
            }
            (Some(q), c) if c == q => {
                quote = None;
                cur.push(c);
            }
            (None, ',') => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.iter()
        .map(|s| unquote(s))
        .filter(|s| !s.is_empty())
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    Wiki,
    Embed,
    Markdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawLink {
    pub kind: LinkKind,
    /// Path part of the target, without `#heading` or `|alias`.
    pub target: String,
    pub heading: Option<String>,
    pub alias: Option<String>,
    pub line: u32,
    /// The link markup as written, for excerpts.
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedNote {
    pub title: String,
    pub frontmatter: Frontmatter,
    pub tags: Vec<String>,
    pub links: Vec<RawLink>,
    /// Body lines outside fenced code (inline code blanked), with line numbers.
    pub lines: Vec<(u32, String)>,
}
impl ParsedNote {
    pub fn line(&self, n: u32) -> Option<&str> {
        self.lines
            .binary_search_by_key(&n, |(i, _)| *i)
            .ok()
            .map(|i| self.lines[i].1.as_str())
    }
}

pub fn stem(rel: &str) -> &str {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    match name.rsplit_once('.') {
        Some((s, ext)) if ext.eq_ignore_ascii_case("md") && !s.is_empty() => s,
        _ => name,
    }
}

pub fn parse_note(rel: &str, text: &str) -> ParsedNote {
    let (fm, body, start) = split_frontmatter(text);
    let fm = fm.unwrap_or_default();
    let mut fence: Option<&str> = None;
    let mut heading = None;
    let mut tags = BTreeSet::new();
    let mut links = Vec::new();
    let mut lines = Vec::new();
    for (i, line) in body.lines().enumerate() {
        let n = start + i as u32;
        let t = line.trim_start();
        if let Some(f) = fence {
            if t.starts_with(f) {
                fence = None;
            }
            continue;
        }
        if t.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if t.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }
        let masked = mask_code(line);
        if heading.is_none() && line.len() - t.len() <= 3 {
            if let Some(h) = masked.trim_start().strip_prefix("# ") {
                let h = h.trim().trim_end_matches('#').trim();
                if !h.is_empty() {
                    heading = Some(h.to_owned());
                }
            }
        }
        wikilinks(&masked, n, &mut links);
        markdown_links(&masked, n, &mut links);
        inline_tags(&masked, &mut tags);
        lines.push((n, masked));
    }
    for key in ["tags", "tag"] {
        if let Some(v) = fm.get(key) {
            for t in v.values() {
                for t in t.split([',', ' ']) {
                    let t = t.trim().trim_start_matches('#');
                    if !t.is_empty() {
                        tags.insert(t.to_lowercase());
                    }
                }
            }
        }
    }
    let title = fm
        .scalar("title")
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .or(heading)
        .unwrap_or_else(|| stem(rel).to_owned());
    ParsedNote {
        title,
        frontmatter: fm,
        tags: tags.into_iter().collect(),
        links,
        lines,
    }
}

/// Blank balanced inline code spans, keeping one character per character.
fn mask_code(line: &str) -> String {
    let ticks = line.matches('`').count() / 2 * 2;
    let mut seen = 0;
    let mut code = false;
    line.chars()
        .map(|c| {
            if c == '`' && seen < ticks {
                seen += 1;
                code = !code;
                ' '
            } else if code {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Wikilinks `[[Target]]`, `[[Target|alias]]`, `[[Target#Heading]]`, embeds `![[…]]`.
pub fn wikilinks(line: &str, n: u32, out: &mut Vec<RawLink>) {
    let mut from = 0;
    while let Some(i) = line[from..].find("[[") {
        let start = from + i;
        let Some(j) = line[start + 2..].find("]]") else {
            break;
        };
        let inner = &line[start + 2..start + 2 + j];
        if let Some(k) = inner.rfind("[[") {
            // `[[a [[b]]`: restart at the inner opening.
            from = start + 2 + k;
            continue;
        }
        let end = start + 2 + j + 2;
        from = end;
        if inner.trim().is_empty() || inner.contains('\n') {
            continue;
        }
        let embed = start > 0 && line.as_bytes()[start - 1] == b'!';
        let (target, alias) = match inner.split_once('|') {
            Some((t, a)) => (
                t.trim_end_matches('\\'),
                Some(a.trim()).filter(|a| !a.is_empty()),
            ),
            None => (inner, None),
        };
        let (path, heading) = match target.split_once('#') {
            Some((p, h)) => (p, Some(h.trim()).filter(|h| !h.is_empty())),
            None => (target, None),
        };
        out.push(RawLink {
            kind: if embed {
                LinkKind::Embed
            } else {
                LinkKind::Wiki
            },
            target: path.trim().to_owned(),
            heading: heading.map(str::to_owned),
            alias: alias.map(str::to_owned),
            line: n,
            raw: line[start - usize::from(embed)..end].to_owned(),
        });
    }
}

fn has_scheme(url: &str) -> bool {
    url.split_once(':').is_some_and(|(s, _)| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}
fn percent_decode(s: &str) -> String {
    fn hex(c: u8) -> Option<u8> {
        (c as char).to_digit(16).map(|d| d as u8)
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_owned())
}

/// Markdown links whose target is a `.md` file: `[text](path.md#heading)`.
pub fn markdown_links(line: &str, n: u32, out: &mut Vec<RawLink>) {
    let mut from = 0;
    while let Some(i) = line[from..].find("](") {
        let open = from + i + 2;
        let Some(j) = line[open..].find(')') else {
            break;
        };
        from = open + j + 1;
        let Some(lb) = line[..open - 2].rfind('[') else {
            continue;
        };
        let mut url = line[open..open + j].trim();
        if let Some(u) = url.strip_prefix('<').and_then(|u| u.strip_suffix('>')) {
            url = u;
        } else {
            url = url.split_whitespace().next().unwrap_or("");
        }
        if url.is_empty() || url.starts_with('#') || has_scheme(url) {
            continue;
        }
        let url = percent_decode(url);
        let (path, heading) = match url.split_once('#') {
            Some((p, h)) => (p.to_owned(), Some(h.to_owned()).filter(|h| !h.is_empty())),
            None => (url.clone(), None),
        };
        if !crate::walk::is_markdown(&path) {
            continue;
        }
        let start = if lb > 0 && line.as_bytes()[lb - 1] == b'!' {
            lb - 1
        } else {
            lb
        };
        out.push(RawLink {
            kind: LinkKind::Markdown,
            target: path,
            heading,
            alias: Some(line[lb + 1..open - 2].trim().to_owned()).filter(|a| !a.is_empty()),
            line: n,
            raw: line[start..from].to_owned(),
        });
    }
}

fn tag_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '/')
}
fn inline_tags(line: &str, tags: &mut BTreeSet<String>) {
    let mut prev: Option<char> = None;
    for (i, c) in line.char_indices() {
        if c == '#' && prev.is_none_or(|p| p.is_whitespace() || matches!(p, '(' | ',')) {
            let tag: String = line[i + 1..].chars().take_while(|c| tag_char(*c)).collect();
            let tag = tag.trim_end_matches('/');
            if tag.chars().any(char::is_alphabetic) {
                tags.insert(tag.to_lowercase());
            }
        }
        prev = Some(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frontmatter_forms() {
        let text = "---\ntitle: \"Quoted: title\"\nauthor: Ann O'Neil # comment\ntags: [a, \"b c\", '#d']\nbooks:\n  - \"[[First Book]]\"\n  - Second Book\nnested:\n  key: value\nbook: [[Linked Book]]\nnote: |\n  block\n---\n# Heading\nBody";
        let (fm, body, start) = split_frontmatter(text);
        let fm = fm.unwrap();
        assert_eq!(fm.scalar("title"), Some("Quoted: title"));
        assert_eq!(fm.scalar("author"), Some("Ann O'Neil"));
        assert_eq!(
            fm.get("tags"),
            Some(&FmValue::List(vec!["a".into(), "b c".into(), "#d".into()]))
        );
        assert_eq!(
            fm.get("books").unwrap().values(),
            vec!["[[First Book]]", "Second Book"]
        );
        assert_eq!(fm.scalar("book"), Some("[[Linked Book]]"));
        assert_eq!(fm.lines["book"], 10);
        assert!(fm.get("note").is_none());
        assert_eq!(fm.unsupported, 3);
        assert_eq!(start, 14);
        assert_eq!(body, "# Heading\nBody");
        assert!(split_frontmatter("---\ntitle: x\nno close").0.is_none());
        assert!(split_frontmatter("title: x\n---\n").0.is_none());
    }
    #[test]
    fn titles_tags_and_links() {
        let n = parse_note(
            "Folder/Stem.md",
            "Intro #Idea and #books/fiction, not#this #123 `#code [[Code]]`\n# The Title\n```\n[[Fenced]] #fenced\n```\nSee [[A/B#Head|alias]] ![[img.png]] [[C\\|x]] [x](../Other%20Note.md#h) [web](https://x.md) [[]]",
        );
        assert_eq!(n.title, "The Title");
        assert_eq!(n.tags, vec!["books/fiction", "idea"]);
        let t: Vec<_> = n
            .links
            .iter()
            .map(|l| {
                (
                    l.kind,
                    l.target.as_str(),
                    l.heading.as_deref(),
                    l.alias.as_deref(),
                    l.line,
                )
            })
            .collect();
        assert_eq!(
            t,
            vec![
                (LinkKind::Wiki, "A/B", Some("Head"), Some("alias"), 6),
                (LinkKind::Embed, "img.png", None, None, 6),
                (LinkKind::Wiki, "C", None, Some("x"), 6),
                (
                    LinkKind::Markdown,
                    "../Other Note.md",
                    Some("h"),
                    Some("x"),
                    6
                ),
            ]
        );
        assert_eq!(n.links[1].raw, "![[img.png]]");
        assert_eq!(parse_note("Folder/Stem.md", "no heading").title, "Stem");
        assert_eq!(
            parse_note("a.md", "---\ntitle: FM\ntags: x, y\n---\n# H").title,
            "FM"
        );
        assert_eq!(
            parse_note("a.md", "---\ntags: [x, \"#y\"]\n---\n").tags,
            vec!["x", "y"]
        );
    }
}
