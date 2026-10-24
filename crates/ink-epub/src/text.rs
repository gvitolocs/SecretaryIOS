//! Plain text and Markdown into the block model.
//!
//! TXT: UTF-8 (BOM stripped) or Windows-1252 fallback. Blank-line separated paragraphs (wrapped lines are
//! joined); if the file has no blank lines, every line is a paragraph. Lines such as `CHAPTER IV` or
//! `Prologue` start a new chapter.
//!
//! Markdown (CommonMark subset): ATX headings (`#` starts a new chapter), paragraphs, `*`/`_` emphasis,
//! `**`/`__` strong, inline code as plain text, links as their text, bullet/numbered lists, block quotes,
//! fenced code, thematic breaks and standalone images (placeholders).
use crate::doc::{Block, Book, Chapter, ListMarker, SourceFormat, Span, SpanStyle};

pub fn decode(bytes: &[u8]) -> String {
    let b = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    match std::str::from_utf8(b) {
        Ok(s) => s.replace("\r\n", "\n").replace('\r', "\n"),
        Err(_) => encoding_rs::WINDOWS_1252
            .decode_without_bom_handling(b)
            .0
            .replace("\r\n", "\n")
            .replace('\r', "\n"),
    }
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_chapter_heading(line: &str) -> bool {
    let l = line.trim();
    if l.is_empty() || l.chars().count() > 80 {
        return false;
    }
    let lower = l.to_lowercase();
    let first = lower.split_whitespace().next().unwrap_or("");
    let word = first.trim_end_matches(['.', ':']);
    matches!(
        word,
        "chapter" | "part" | "book" | "prologue" | "epilogue" | "capitolo" | "kapitel" | "chapitre"
    ) && (lower.split_whitespace().count() <= 8)
}

struct Chapters {
    book: Book,
    cur: Chapter,
}

impl Chapters {
    fn new(format: SourceFormat, title: &str) -> Self {
        let mut book = Book::new(format);
        book.title = Some(title.to_owned());
        Self {
            book,
            cur: Chapter::default(),
        }
    }
    fn new_chapter(&mut self) {
        if !self.cur.blocks.is_empty() {
            let mut c = std::mem::take(&mut self.cur);
            c.title = c.first_heading();
            self.book.chapters.push(c);
        }
    }
    fn push(&mut self, b: Block) {
        self.cur.blocks.push(b);
    }
    fn finish(mut self) -> Book {
        self.new_chapter();
        if self.book.chapters.is_empty() {
            self.book.chapters.push(Chapter::default());
        }
        self.book
    }
}

pub fn from_txt(bytes: &[u8], title: &str) -> Book {
    let text = decode(bytes);
    let mut out = Chapters::new(SourceFormat::Txt, title);
    let has_blank = text.lines().any(|l| l.trim().is_empty());
    let paragraphs: Vec<String> = if has_blank {
        let mut v = Vec::new();
        let mut cur = Vec::new();
        for l in text.lines() {
            if l.trim().is_empty() {
                if !cur.is_empty() {
                    v.push(collapse(&cur.join(" ")));
                    cur.clear();
                }
            } else if is_chapter_heading(l) && cur.is_empty() {
                v.push(collapse(l));
            } else {
                cur.push(l);
            }
        }
        if !cur.is_empty() {
            v.push(collapse(&cur.join(" ")));
        }
        v
    } else {
        text.lines()
            .map(collapse)
            .filter(|l| !l.is_empty())
            .collect()
    };
    for p in paragraphs {
        if is_chapter_heading(&p) {
            out.new_chapter();
            out.push(Block::Heading {
                level: 1,
                spans: vec![Span::plain(p)],
            });
        } else {
            out.push(Block::Paragraph {
                spans: vec![Span::plain(p)],
                quote: false,
            });
        }
    }
    out.finish()
}

/// Inline Markdown → styled spans.
pub fn inline(s: &str) -> Vec<Span> {
    let chars: Vec<char> = s.chars().collect();
    let mut spans: Vec<Span> = Vec::new();
    let mut style = SpanStyle::default();
    let push = |spans: &mut Vec<Span>, c: char, style: SpanStyle| match spans.last_mut() {
        Some(sp) if sp.style == style => sp.text.push(c),
        _ => spans.push(Span {
            text: c.to_string(),
            style,
        }),
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let prev_alnum = i > 0 && chars[i - 1].is_alphanumeric();
        let next = chars.get(i + 1).copied();
        match c {
            '\\' if next.is_some_and(|n| n.is_ascii_punctuation()) => {
                push(&mut spans, next.unwrap_or(' '), style);
                i += 2;
            }
            '*' | '_' if next == Some(c) => {
                style.bold = !style.bold;
                i += 2;
            }
            '*' => {
                style.italic = !style.italic;
                i += 1;
            }
            '_' if !(prev_alnum && next.is_some_and(char::is_alphanumeric)) => {
                style.italic = !style.italic;
                i += 1;
            }
            '`' => {
                let end = chars[i + 1..].iter().position(|&x| x == '`');
                match end {
                    Some(e) => {
                        for &x in &chars[i + 1..i + 1 + e] {
                            push(&mut spans, x, style);
                        }
                        i += e + 2;
                    }
                    None => {
                        push(&mut spans, c, style);
                        i += 1;
                    }
                }
            }
            '!' | '[' => {
                // [text](url) or ![alt](src): keep the text.
                let start = if c == '!' { i + 1 } else { i };
                let close = (c == '[' || next == Some('['))
                    .then(|| chars[start..].iter().position(|&x| x == ']'))
                    .flatten()
                    .map(|p| start + p);
                match close {
                    Some(cl) if chars.get(cl + 1) == Some(&'(') => {
                        if let Some(pe) = chars[cl..].iter().position(|&x| x == ')') {
                            for x in inline(&chars[start + 1..cl].iter().collect::<String>()) {
                                for ch in x.text.chars() {
                                    push(&mut spans, ch, style);
                                }
                            }
                            i = cl + pe + 1;
                            continue;
                        }
                        push(&mut spans, c, style);
                        i += 1;
                    }
                    _ => {
                        push(&mut spans, c, style);
                        i += 1;
                    }
                }
            }
            c if c.is_whitespace() => {
                if !spans.last().is_some_and(|s| s.text.ends_with(' ')) && !spans.is_empty() {
                    push(&mut spans, ' ', style);
                }
                i += 1;
            }
            c => {
                push(&mut spans, c, style);
                i += 1;
            }
        }
    }
    if let Some(last) = spans.last_mut() {
        let t = last.text.trim_end().len();
        last.text.truncate(t);
    }
    spans.retain(|s| !s.text.is_empty());
    spans
}

fn list_marker(line: &str) -> Option<(ListMarker, u8, &str)> {
    let indent = line.len() - line.trim_start().len();
    let depth = (indent / 2).min(7) as u8 + 1;
    let t = line.trim_start();
    for b in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(b) {
            return Some((ListMarker::Bullet, depth, rest));
        }
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let rest = &t[digits..];
        if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some((ListMarker::Number(t[..digits].parse().ok()?), depth, r));
        }
    }
    None
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let t = line.trim_start();
    let level = t.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&level) {
        let rest = &t[level..];
        if rest.is_empty() || rest.starts_with(' ') {
            return Some((level as u8, rest.trim().trim_end_matches('#').trim()));
        }
    }
    None
}

fn is_rule(line: &str) -> bool {
    let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    t.len() >= 3
        && (t.chars().all(|c| c == '-')
            || t.chars().all(|c| c == '*')
            || t.chars().all(|c| c == '_'))
}

fn standalone_image(line: &str) -> Option<Block> {
    let t = line.trim();
    let rest = t.strip_prefix("![")?;
    let close = rest.find("](")?;
    let src = rest[close + 2..].strip_suffix(')')?;
    Some(Block::Image {
        alt: rest[..close].to_owned(),
        src: src.split_whitespace().next().unwrap_or("").to_owned(),
    })
}

pub fn from_markdown(bytes: &[u8], fallback_title: &str) -> Book {
    let text = decode(bytes);
    let mut out = Chapters::new(SourceFormat::Markdown, fallback_title);
    let mut title_set = false;
    let mut para: Vec<&str> = Vec::new();
    let mut quote = false;
    let lines: Vec<&str> = text.lines().collect();
    let flush = |out: &mut Chapters, para: &mut Vec<&str>, quote: bool| {
        if !para.is_empty() {
            let spans = inline(&para.join(" "));
            if !spans.is_empty() {
                out.push(Block::Paragraph { spans, quote });
            }
            para.clear();
        }
    };
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            flush(&mut out, &mut para, quote);
            let fence = &t[..3];
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim().starts_with(fence) {
                body.push(lines[i]);
                i += 1;
            }
            if !body.is_empty() {
                out.push(Block::Preformatted {
                    text: body.join("\n"),
                });
            }
            i += 1;
            continue;
        }
        if t.is_empty() {
            flush(&mut out, &mut para, quote);
        } else if let Some((level, h)) = heading(line) {
            flush(&mut out, &mut para, quote);
            if level == 1 {
                out.new_chapter();
                if !title_set {
                    out.book.title = Some(h.to_owned());
                    title_set = true;
                }
            }
            let spans = inline(h);
            if !spans.is_empty() {
                out.push(Block::Heading { level, spans });
            }
        } else if is_rule(line) {
            flush(&mut out, &mut para, quote);
            out.push(Block::Rule);
        } else if let Some(img) = standalone_image(line) {
            flush(&mut out, &mut para, quote);
            out.push(img);
        } else if let Some((marker, depth, rest)) = list_marker(line) {
            flush(&mut out, &mut para, quote);
            out.push(Block::ListItem {
                marker,
                depth,
                spans: inline(rest),
            });
        } else if let Some(rest) = t.strip_prefix('>') {
            if !quote {
                flush(&mut out, &mut para, quote);
                quote = true;
            }
            let r = rest.trim();
            if r.is_empty() {
                flush(&mut out, &mut para, quote);
            } else {
                para.push(r);
            }
        } else {
            if quote {
                flush(&mut out, &mut para, quote);
                quote = false;
            }
            para.push(t);
        }
        i += 1;
    }
    flush(&mut out, &mut para, quote);
    out.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_emphasis_links_code() {
        let s = inline("A *b* **c _d_** `e*f` [g](http://x) snake_case \\*lit\\*");
        let v: Vec<(&str, bool, bool)> = s
            .iter()
            .map(|s| (s.text.as_str(), s.style.bold, s.style.italic))
            .collect();
        assert_eq!(
            v,
            vec![
                ("A ", false, false),
                ("b", false, true),
                (" ", false, false),
                ("c ", true, false),
                ("d", true, true),
                (" e*f g snake_case *lit*", false, false),
            ]
        );
    }

    #[test]
    fn chapter_heading_detection() {
        assert!(is_chapter_heading("CHAPTER IV."));
        assert!(is_chapter_heading("Chapter 12: The End"));
        assert!(is_chapter_heading("Prologue"));
        assert!(!is_chapter_heading(
            "Chapter and verse were quoted at length by the vicar, who had a great deal to say about it and more"
        ));
        assert!(!is_chapter_heading("The chapter ended."));
    }
}
