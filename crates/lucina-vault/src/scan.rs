//! One note's links, tags, headings and block ids, as Obsidian's metadata cache
//! reads them. The rules below come from `tests/fixture/obsidian-golden.json`, not
//! from the Markdown specification. For example, `%%comment%%` keeps its links and
//! tags, `<!-- -->` hides them, `(#x)` is no tag and `[#x](y)` is one. Lines are 0-based.
use ink_obsidian::parse::{split_frontmatter, FmValue, Frontmatter};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    /// The link as Obsidian reports it: trimmed, percent-decoded, `#subpath` included.
    pub link: String,
    pub display: Option<String>,
    pub embed: bool,
    pub wiki: bool,
    pub line: u32,
    /// The markup as written.
    pub original: String,
}
impl Link {
    /// Path part, not trimmed again: `[[Alpha #Section]]` looks for "Alpha ".
    pub fn path(&self) -> &str {
        self.link.split('#').next().unwrap_or("")
    }
    pub fn subpath(&self) -> Option<&str> {
        self.link.split_once('#').map(|(_, s)| s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heading {
    pub text: String,
    pub level: u8,
    pub line: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    pub links: Vec<Link>,
    /// `tags:` of the frontmatter, then inline, without `#`, as written. Repeats are kept.
    pub tags: Vec<String>,
    /// `up: "[[Alpha]]"`, `related: ["[[Beta]]"]`: the value's link.
    pub frontmatter_links: Vec<(String, String)>,
    pub aliases: Vec<String>,
    pub headings: Vec<Heading>,
    pub blocks: Vec<String>,
    pub properties: Vec<(String, FmValue)>,
    /// Line the body starts on.
    pub body_line: u32,
}

/// Tag characters end at whitespace, ASCII punctuation other than `_ - /`, and the
/// Unicode punctuation blocks. A tag needs one character that is not a digit.
fn tag_end(c: char) -> bool {
    c.is_whitespace()
        || matches!(c, '\u{2000}'..='\u{206F}' | '\u{2E00}'..='\u{2E7F}')
        || "'!\"#$%&()*+,.:;<=>?@^`{|}~[]\\".contains(c)
}

fn frontmatter_tags(fm: &Frontmatter, out: &mut Vec<String>) {
    let Some(v) = fm.get("tags") else { return };
    for t in v.values() {
        let t = t.trim().trim_start_matches('#');
        // `tags: a, b` and `tags: [ltx-2.5]` are no tags in Obsidian: the whole value must be a valid name.
        if t.chars().any(|c| !c.is_ascii_digit()) && !t.contains(tag_end) {
            out.push(t.to_owned());
        }
    }
}

/// `"[[x]]"` as a YAML string. Unquoted, YAML reads `[[x]]` as nested lists and Obsidian sees no link.
fn quoted_wikilink(v: &str) -> Option<&str> {
    let v = v.trim();
    let inner = v
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .or_else(|| v.strip_prefix('\'').and_then(|r| r.strip_suffix('\'')))?;
    let inner = inner.trim().strip_prefix("[[")?.strip_suffix("]]")?;
    (!inner.contains("[[")).then(|| inner.split('|').next().unwrap_or(inner).trim())
}

/// Frontmatter links from the raw lines, where the quotes still show. `FmValue` has dropped them.
fn frontmatter_links(text: &str, end_line: u32, out: &mut Vec<(String, String)>) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut key = String::new();
    let mut item = 0;
    for l in text
        .lines()
        .take(end_line.saturating_sub(2) as usize)
        .skip(1)
    {
        let t = l.trim();
        if let Some(v) = t.strip_prefix("- ") {
            if let Some(link) = quoted_wikilink(v) {
                out.push((format!("{key}.{item}"), link.to_owned()));
            }
            item += 1;
        } else if !l.starts_with([' ', '\t']) {
            let Some((k, v)) = t.split_once(':') else {
                continue;
            };
            key = k.trim().to_owned();
            item = 0;
            let v = v.trim();
            if let Some(flow) = v.strip_prefix('[').and_then(|f| f.strip_suffix(']')) {
                for (i, part) in flow.split(',').enumerate() {
                    if let Some(link) = quoted_wikilink(part) {
                        out.push((format!("{key}.{i}"), link.to_owned()));
                    }
                }
            } else if let Some(link) = quoted_wikilink(v) {
                out.push((key.clone(), link.to_owned()));
            }
        }
    }
}

const HTML_BLOCK: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "dd",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "iframe",
    "li",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "script",
    "section",
    "style",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "ul",
];

fn html_block_start(t: &str) -> bool {
    let Some(rest) = t.strip_prefix('<') else {
        return false;
    };
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    let name: String = rest
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    !name.is_empty() && HTML_BLOCK.contains(&name.to_ascii_lowercase().as_str())
}

/// YAML Obsidian refuses, which costs the note its whole frontmatter: tags, aliases
/// and links. Not a YAML parser: the mistakes hand-written notes actually make.
fn yaml_broken(text: &str, end_line: u32) -> bool {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text
        .lines()
        .take(end_line.saturating_sub(2) as usize)
        .skip(1)
        .collect();
    let rest_closes = |from: usize, close: char| lines[from..].iter().any(|l| l.contains(close));
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.is_empty() || t.starts_with('#') || l.starts_with([' ', '\t']) || t.starts_with("- ") {
            continue;
        }
        let Some((_, v)) = t.split_once(':') else {
            return true;
        };
        let v = v.trim();
        let (open, close) = match v.chars().next() {
            Some('[') => ('[', ']'),
            Some('{') => ('{', '}'),
            Some(q @ ('"' | '\'')) => {
                if v.len() < 2 || !v.ends_with(q) {
                    return true;
                }
                continue;
            }
            _ => {
                if v.split_once(" #").map_or(v, |(a, _)| a).contains(": ") {
                    return true;
                }
                continue;
            }
        };
        let (mut depth, mut end) = (0i32, None);
        for (j, c) in v.char_indices() {
            if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
                if depth == 0 {
                    end = Some(j);
                    break;
                }
            }
        }
        match end {
            Some(j) if !v[j + 1..].trim().is_empty() => return true,
            None if !rest_closes(i + 1, close) => return true,
            _ => {}
        }
    }
    false
}

/// A GFM delimiter row: `--- | :---:` or `|-|`.
fn table_delimiter(line: &str) -> bool {
    let t = line.trim();
    t.contains('-')
        && (t.contains('|') || t.starts_with(':') || t.starts_with('-'))
        && t.trim_matches('|').split('|').all(|c| {
            let c = c.trim().trim_start_matches(':').trim_end_matches(':');
            !c.is_empty() && c.chars().all(|x| x == '-')
        })
}
/// Table cells split at every `|` not escaped, even inside `[[a|b]]` or code.
fn table_cells(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let b = line.as_bytes();
    for (i, c) in b.iter().enumerate() {
        if *c == b'|' && (i == 0 || b[i - 1] != b'\\') {
            out.push(&line[start..i]);
            start = i + 1;
        }
    }
    out.push(&line[start..]);
    out
}

/// Removes block-quote and list markers from the start of a line.
fn strip_containers(line: &str) -> &str {
    let mut t = line.trim_start();
    loop {
        if let Some(r) = t.strip_prefix('>') {
            t = r.trim_start();
        } else if let Some(r) = t
            .strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| t.strip_prefix("+ "))
        {
            t = r.trim_start();
        } else if let Some(r) = ordered_marker(t) {
            t = r.trim_start();
        } else {
            return t;
        }
    }
}
fn ordered_marker(t: &str) -> Option<&str> {
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 || digits > 9 {
        return None;
    }
    let r = &t[digits..];
    r.strip_prefix(". ").or_else(|| r.strip_prefix(") "))
}
fn is_list_item(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("- ")
        || t.starts_with("* ")
        || t.starts_with("+ ")
        || t == "-"
        || ordered_marker(t).is_some()
}

fn fence_open(line: &str) -> Option<(char, usize)> {
    if line.len() - line.trim_start().len() > 3
        && !is_list_item(line)
        && !line.trim_start().starts_with('>')
    {
        return None;
    }
    let t = strip_containers(line);
    let c = t.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let n = t.chars().take_while(|x| *x == c).count();
    if n < 3 || (c == '`' && t[n..].contains('`')) {
        return None;
    }
    Some((c, n))
}
fn fence_close(line: &str, open: (char, usize)) -> bool {
    let t = line.trim_start();
    let t = t.strip_prefix('>').map_or(t, str::trim_start);
    let n = t.chars().take_while(|x| *x == open.0).count();
    n >= open.1 && t[n * open.0.len_utf8()..].trim().is_empty()
}

fn atx_heading(line: &str) -> Option<(u8, &str)> {
    if line.len() - line.trim_start_matches(' ').len() > 3 {
        return None;
    }
    let t = line.trim_start_matches(' ');
    let n = t.chars().take_while(|c| *c == '#').count();
    if n == 0 || n > 6 {
        return None;
    }
    let rest = &t[n..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let rest = rest.trim();
    let closed = rest.trim_end_matches('#');
    let text = if closed.is_empty() || closed.ends_with([' ', '\t']) {
        closed.trim_end()
    } else {
        rest
    };
    Some((n as u8, text))
}
fn setext_level(line: &str) -> Option<u8> {
    let t = line.trim();
    if t.len() >= 1 && t.chars().all(|c| c == '=') {
        Some(1)
    } else if t.len() >= 2 && t.chars().all(|c| c == '-') {
        Some(2)
    } else {
        None
    }
}

fn block_id(line: &str) -> Option<&str> {
    let t = line.trim_end();
    let i = t.rfind('^')?;
    let id = &t[i + 1..];
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return None;
    }
    (i == 0 || t[..i].ends_with([' ', '\t'])).then_some(id)
}

fn has_scheme(url: &str) -> bool {
    url.split_once(':').is_some_and(|(s, _)| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}
pub(crate) fn percent_decode(s: &str) -> String {
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

/// `](dest "title")`, starting at the `(`. Returns the destination and the index
/// after the `)`. A bare destination holds no spaces and balanced parentheses.
fn link_destination(line: &str, open: usize) -> Option<(String, usize)> {
    let b = line.as_bytes();
    let mut i = open + 1;
    while b.get(i) == Some(&b' ') {
        i += 1;
    }
    let dest;
    if b.get(i) == Some(&b'<') {
        let end = line[i + 1..].find('>')? + i + 1;
        dest = &line[i + 1..end];
        i = end + 1;
    } else {
        let start = i;
        let mut depth = 0usize;
        while let Some(&c) = b.get(i) {
            match c {
                b'\\' => i += 1,
                b'(' => depth += 1,
                b')' if depth == 0 => break,
                b')' => depth -= 1,
                b' ' | b'\t' => break,
                _ => {}
            }
            i += 1;
        }
        dest = line.get(start..i)?;
    }
    while b.get(i) == Some(&b' ') {
        i += 1;
    }
    if let Some(q @ (b'"' | b'\'')) = b.get(i).copied() {
        let end = line[i + 1..].find(q as char)? + i + 1;
        i = end + 1;
        while b.get(i) == Some(&b' ') {
            i += 1;
        }
    }
    (b.get(i) == Some(&b')')).then(|| (dest.to_owned(), i + 1))
}

/// The `]` closing the text that starts after the `[` at `open`.
fn closing_bracket(line: &str, open: usize) -> Option<usize> {
    let b = line.as_bytes();
    let mut depth = 0usize;
    let mut i = open + 1;
    while let Some(&c) = b.get(i) {
        match c {
            b'\\' => i += 1,
            b'[' => depth += 1,
            b']' if depth == 0 => return Some(i),
            b']' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    None
}

#[derive(Default)]
struct Inline {
    comment: bool,
}

impl Inline {
    fn line(&mut self, line: &str, n: u32, out: &mut Scan) {
        let b = line.as_bytes();
        let mut i = 0;
        // `(..)` of a Markdown link already taken: its text is still read, its destination is not.
        let mut skips: Vec<(usize, usize)> = Vec::new();
        if self.comment {
            match line.find("-->") {
                Some(e) => {
                    i = e + 3;
                    self.comment = false;
                }
                None => return,
            }
        }
        while i < b.len() {
            if let Some(&(_, e)) = skips.iter().find(|(s, _)| *s == i) {
                i = e;
                continue;
            }
            match b[i] {
                b'\\' => {
                    i += 1;
                    while i < b.len() && !line.is_char_boundary(i + 1) {
                        i += 1;
                    }
                    i += 1;
                }
                b'`' => {
                    let run = b[i..].iter().take_while(|c| **c == b'`').count();
                    let mut j = i + run;
                    let mut closed = None;
                    while j < b.len() {
                        if b[j] == b'`' {
                            let r = b[j..].iter().take_while(|c| **c == b'`').count();
                            if r == run {
                                closed = Some(j + r);
                                break;
                            }
                            j += r;
                        } else {
                            j += 1;
                        }
                    }
                    i = closed.unwrap_or(i + run);
                }
                b'$' => i = math_end(line, i).unwrap_or(i + 1),
                b'<' => {
                    if line[i..].starts_with("<!--") {
                        match line[i + 4..].find("-->") {
                            Some(e) => i = i + 4 + e + 3,
                            None => {
                                self.comment = true;
                                return;
                            }
                        }
                    } else if let Some(e) = html_or_autolink_end(line, i) {
                        i = e;
                    } else {
                        i += 1;
                    }
                }
                b'^' if b.get(i + 1) == Some(&b'[') => i += 2,
                b'!' if line[i..].starts_with("![[") => i = self.wiki(line, i + 1, true, n, out),
                b'[' if line[i..].starts_with("[[") => i = self.wiki(line, i, false, n, out),
                b'!' if b.get(i + 1) == Some(&b'[') => {
                    i = md_link(line, i + 1, true, n, out, &mut skips).unwrap_or(i + 1)
                }
                b'[' => i = md_link(line, i, false, n, out, &mut skips).unwrap_or(i + 1),
                b'h' if (line[i..].starts_with("http://") || line[i..].starts_with("https://"))
                    && !prev_char(line, i).is_some_and(char::is_alphanumeric) =>
                {
                    i += line[i..]
                        .find(|c: char| c.is_whitespace() || c == '<')
                        .unwrap_or(line.len() - i);
                }
                b'#' if tag_may_start(line, i) => {
                    let rest = &line[i + 1..];
                    let end = rest.find(tag_end).unwrap_or(rest.len());
                    let mut tag = &rest[..end];
                    // `_#under_`: the last `_` closes the emphasis.
                    if prev_char(line, i) == Some('_') {
                        tag = tag.strip_suffix('_').unwrap_or(tag);
                    }
                    if tag.chars().any(|c| !c.is_ascii_digit()) {
                        out.tags.push(tag.to_owned());
                    }
                    i += 1 + end;
                }
                _ => i += 1,
            }
        }
    }

    fn wiki(&mut self, line: &str, open: usize, embed: bool, n: u32, out: &mut Scan) -> usize {
        let start = open + 2;
        let Some(close) = line[start..].find("]]") else {
            return start;
        };
        let inner = &line[start..start + close];
        if let Some(k) = inner.rfind("[[") {
            // `[[a [[b]]`: only the inner one is a link.
            return start + k;
        }
        let end = start + close + 2;
        if inner.is_empty() {
            return end;
        }
        let (target, display) = match inner.split_once('|') {
            Some((t, a)) => (t.strip_suffix('\\').unwrap_or(t), Some(a.trim().to_owned())),
            None => (inner, None),
        };
        out.links.push(Link {
            link: target.trim().to_owned(),
            display,
            embed,
            wiki: true,
            line: n,
            original: line[open - usize::from(embed)..end].to_owned(),
        });
        end
    }
}

fn md_link(
    line: &str,
    open: usize,
    embed: bool,
    n: u32,
    out: &mut Scan,
    skips: &mut Vec<(usize, usize)>,
) -> Option<usize> {
    let close = closing_bracket(line, open)?;
    if line.as_bytes().get(close + 1) != Some(&b'(') {
        return None;
    }
    let (dest, end) = link_destination(line, close + 1)?;
    if dest.is_empty() || has_scheme(&dest) {
        // Not a vault link, and its destination still holds no tags or links.
        skips.push((close + 1, end));
        return Some(open + 1);
    }
    out.links.push(Link {
        link: percent_decode(&dest),
        display: Some(line[open + 1..close].trim().to_owned()).filter(|d| !d.is_empty()),
        embed,
        wiki: false,
        line: n,
        original: line[open - usize::from(embed)..end].to_owned(),
    });
    skips.push((close + 1, end));
    Some(open + 1)
}

fn prev_char(line: &str, i: usize) -> Option<char> {
    line[..i].chars().next_back()
}
/// A tag starts a text run: after whitespace, or just inside emphasis, a highlight or link text.
fn tag_may_start(line: &str, i: usize) -> bool {
    match prev_char(line, i) {
        None => true,
        Some(p) if p.is_whitespace() => true,
        Some('[' | '*' | '_') => true,
        Some('=') => line[..i].ends_with("=="),
        Some('~') => line[..i].ends_with("~~"),
        _ => false,
    }
}
fn math_end(line: &str, i: usize) -> Option<usize> {
    if line[i..].starts_with("$$") {
        return line[i + 2..].find("$$").map(|e| i + 2 + e + 2);
    }
    let rest = &line[i + 1..];
    if rest.starts_with([' ', '\t']) || rest.is_empty() {
        return None;
    }
    let e = rest.find('$')?;
    if rest[..e].ends_with([' ', '\t']) || rest[e + 1..].starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(i + 1 + e + 1)
}
/// `<http://x/#y>` and `<span class="#z">`: not text.
fn html_or_autolink_end(line: &str, i: usize) -> Option<usize> {
    let rest = &line[i + 1..];
    let first = rest.chars().next()?;
    if !(first.is_ascii_alphabetic() || first == '/') {
        return None;
    }
    let e = rest.find('>')?;
    (!rest[..e].contains('<')).then_some(i + 1 + e + 1)
}

pub fn scan(text: &str) -> Scan {
    let (fm, body, start) = split_frontmatter(text);
    let body_line = start - 1;
    let mut out = Scan {
        body_line,
        ..Default::default()
    };
    if let Some(fm) = fm.as_ref().filter(|_| !yaml_broken(text, start)) {
        frontmatter_tags(fm, &mut out.tags);
        for key in ["aliases", "alias"] {
            if let Some(v) = fm.get(key) {
                out.aliases
                    .extend(v.values().into_iter().map(|a| a.trim().to_owned()));
            }
        }
        frontmatter_links(text, start, &mut out.frontmatter_links);
        for (k, v) in &fm.fields {
            out.properties.push((k.clone(), v.clone()));
        }
    }
    let lines: Vec<&str> = body.lines().collect();
    let mut fence: Option<(char, usize)> = None;
    let mut math = false;
    let mut html = false;
    let mut code = false;
    let mut in_list = false;
    let mut prev_blank = true;
    let mut table = false;
    let mut inline = Inline::default();
    for (i, &line) in lines.iter().enumerate() {
        let n = body_line + i as u32;
        if let Some(open) = fence {
            if fence_close(line, open) {
                fence = None;
            }
            continue;
        }
        let t = line.trim();
        if math {
            if t.ends_with("$$") {
                math = false;
            }
            continue;
        }
        if t.is_empty() {
            html = false;
            table = false;
            prev_blank = true;
            continue;
        }
        if html {
            continue;
        }
        if inline.comment {
            inline.line(line, n, &mut out);
            prev_blank = false;
            continue;
        }
        if let Some(open) = fence_open(line) {
            fence = Some(open);
            prev_blank = false;
            code = false;
            continue;
        }
        let indented = line.starts_with('\t') || line.starts_with("    ");
        if indented && (prev_blank || code) && !in_list && !t.starts_with('>') {
            code = true;
            continue;
        }
        code = false;
        if t == "$$" || (t.starts_with("$$") && !t[2..].contains("$$")) {
            math = true;
            continue;
        }
        if prev_blank && html_block_start(t) {
            html = true;
            continue;
        }
        if is_list_item(line) {
            in_list = true;
        } else if prev_blank && !indented {
            in_list = false;
        }
        let quoted = strip_containers(line);
        if let Some((level, text)) = atx_heading(quoted) {
            out.headings.push(Heading {
                text: text.to_owned(),
                level,
                line: n,
            });
        } else if !is_list_item(line) {
            if let Some(level) = lines.get(i + 1).and_then(|l| setext_level(l)) {
                if setext_level(line).is_none() {
                    out.headings.push(Heading {
                        text: t.to_owned(),
                        level,
                        line: n,
                    });
                }
            }
        }
        if let Some(id) = block_id(line) {
            out.blocks.push(id.to_owned());
        }
        if !table && line.contains('|') && lines.get(i + 1).is_some_and(|l| table_delimiter(l)) {
            table = true;
        }
        if table {
            if !table_delimiter(line) {
                for cell in table_cells(line) {
                    inline.line(cell, n, &mut out);
                }
            }
        } else {
            inline.line(line, n, &mut out);
        }
        prev_blank = false;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn heading_forms() {
        assert_eq!(atx_heading("## Closed ##"), Some((2, "Closed")));
        assert_eq!(atx_heading("# a#"), Some((1, "a#")));
        assert_eq!(atx_heading("#nope"), None);
        assert_eq!(atx_heading("    # code"), None);
        let s = scan("Setext\n===\n\ntext\n---\n");
        assert_eq!(s.headings.len(), 2);
        assert_eq!(s.headings[1].level, 2);
    }
    #[test]
    fn multi_byte_never_splits() {
        let s = scan("\\é #ü `é` <é> $é$ [é](é) [[é|é]] ^é é#é #é.é");
        assert_eq!(s.links.len(), 2);
        assert_eq!(s.tags, vec!["ü", "é"]);
    }
}
