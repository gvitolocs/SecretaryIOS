//! Streaming XHTML → [`Block`]s. Tolerant: mismatched end tags are accepted, and on a hard XML error the
//! blocks parsed so far are kept and a warning is returned. DTDs are ignored (never expanded).
use crate::doc::{Block, ListMarker, Span, SpanStyle};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

pub(crate) struct Parsed {
    pub blocks: Vec<Block>,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Para,
    Heading(u8),
    Item(ListMarker, u8),
    Pre,
}

struct Builder {
    blocks: Vec<Block>,
    spans: Vec<Span>,
    pre: String,
    kind: Kind,
    item_open: Option<(ListMarker, u8)>,
    pending_space: bool,
    bold: u32,
    italic: u32,
    quote: u32,
    lists: Vec<(bool, u32)>,
    skip: u32,
    pre_depth: u32,
}

const SKIP: &[&[u8]] = &[b"head", b"script", b"style", b"noscript", b"template"];
const BLOCKS: &[&[u8]] = &[
    b"p",
    b"div",
    b"section",
    b"article",
    b"header",
    b"footer",
    b"aside",
    b"figure",
    b"figcaption",
    b"main",
    b"address",
    b"dl",
    b"dt",
    b"dd",
    b"table",
    b"tr",
    b"caption",
    b"nav",
    b"body",
    b"center",
];

fn heading_level(name: &[u8]) -> Option<u8> {
    match name {
        [b'h', d @ b'1'..=b'6'] => Some(d - b'0'),
        _ => None,
    }
}

pub(crate) fn named_entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        "nbsp" => "\u{a0}",
        "shy" => "", // soft hyphen: layout is hyphen-free
        "mdash" => "\u{2014}",
        "ndash" => "\u{2013}",
        "hellip" => "\u{2026}",
        "lsquo" => "\u{2018}",
        "rsquo" => "\u{2019}",
        "ldquo" => "\u{201c}",
        "rdquo" => "\u{201d}",
        "laquo" => "\u{ab}",
        "raquo" => "\u{bb}",
        "bull" => "\u{2022}",
        "middot" => "\u{b7}",
        "copy" => "\u{a9}",
        "reg" => "\u{ae}",
        "trade" => "\u{2122}",
        "deg" => "\u{b0}",
        "times" => "\u{d7}",
        "eacute" => "\u{e9}",
        "egrave" => "\u{e8}",
        "agrave" => "\u{e0}",
        "ograve" => "\u{f2}",
        "ugrave" => "\u{f9}",
        "igrave" => "\u{ec}",
        "ccedil" => "\u{e7}",
        "uuml" => "\u{fc}",
        "ouml" => "\u{f6}",
        "auml" => "\u{e4}",
        "szlig" => "\u{df}",
        "aelig" => "\u{e6}",
        "oslash" => "\u{f8}",
        "aring" => "\u{e5}",
        "thinsp" | "ensp" | "emsp" => " ",
        "zwnj" | "zwj" => "",
        _ => return None,
    })
}

fn attr(e: &BytesStart, names: &[&[u8]]) -> Option<String> {
    for a in e.attributes().with_checks(false).flatten() {
        let key = a.key.as_ref();
        let local = key.rsplit(|b| *b == b':').next().unwrap_or(key);
        if names.contains(&key) || names.contains(&local) {
            return Some(
                a.unescape_value()
                    .map(|v| v.into_owned())
                    .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned()),
            );
        }
    }
    None
}

impl Builder {
    fn new() -> Self {
        Self {
            blocks: Vec::new(),
            spans: Vec::new(),
            pre: String::new(),
            kind: Kind::Para,
            item_open: None,
            pending_space: false,
            bold: 0,
            italic: 0,
            quote: 0,
            lists: Vec::new(),
            skip: 0,
            pre_depth: 0,
        }
    }

    fn style(&self) -> SpanStyle {
        SpanStyle {
            bold: self.bold > 0,
            italic: self.italic > 0,
        }
    }

    fn has_text(&self) -> bool {
        self.spans.iter().any(|s| !s.text.is_empty())
    }

    fn append(&mut self, c: char) {
        let style = self.style();
        match self.spans.last_mut() {
            Some(s) if s.style == style => s.text.push(c),
            _ => self.spans.push(Span {
                text: c.to_string(),
                style,
            }),
        }
    }

    fn text(&mut self, s: &str) {
        if self.skip > 0 {
            return;
        }
        if self.pre_depth > 0 {
            self.pre.extend(s.chars().filter(|c| *c != '\r'));
            return;
        }
        for c in s.chars() {
            if c.is_whitespace() && c != '\u{a0}' {
                self.pending_space = true;
                continue;
            }
            let last_nl = self
                .spans
                .last()
                .and_then(|s| s.text.chars().last())
                .is_some_and(|c| c == '\n');
            if self.pending_space && self.has_text() && !last_nl {
                self.append(' ');
            }
            self.pending_space = false;
            self.append(c);
        }
    }

    fn line_break(&mut self) {
        if self.pre_depth > 0 {
            self.pre.push('\n');
        } else if self.has_text() {
            self.append('\n');
            self.pending_space = false;
        }
    }

    fn flush(&mut self) {
        self.pending_space = false;
        if self.kind == Kind::Pre {
            let t = self.pre.trim_matches('\n').to_owned();
            self.pre.clear();
            if !t.trim().is_empty() {
                self.blocks.push(Block::Preformatted { text: t });
            }
            return;
        }
        while let Some(last) = self.spans.last_mut() {
            let t = last.text.trim_end_matches([' ', '\n']).len();
            last.text.truncate(t);
            if last.text.is_empty() {
                self.spans.pop();
            } else {
                break;
            }
        }
        if self.spans.is_empty() {
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        self.blocks.push(match self.kind {
            Kind::Heading(level) => Block::Heading { level, spans },
            Kind::Item(marker, depth) => {
                self.item_open = None;
                Block::ListItem {
                    marker,
                    depth,
                    spans,
                }
            }
            _ => Block::Paragraph {
                spans,
                quote: self.quote > 0,
            },
        });
    }

    fn para_kind(&self) -> Kind {
        match self.item_open {
            Some((m, d)) => Kind::Item(m, d),
            None => Kind::Para,
        }
    }

    fn image(&mut self, e: &BytesStart) {
        self.flush();
        self.blocks.push(Block::Image {
            alt: attr(e, &[b"alt", b"title"]).unwrap_or_default(),
            src: attr(e, &[b"src", b"href"]).unwrap_or_default(),
        });
    }

    fn start(&mut self, e: &BytesStart, empty: bool) {
        let lname = e.local_name().as_ref().to_ascii_lowercase();
        let name = lname.as_slice();
        if self.skip > 0 {
            if !empty {
                self.skip += 1;
            }
            return;
        }
        if SKIP.contains(&name) {
            if !empty {
                self.skip = 1;
            }
            return;
        }
        if let Some(level) = heading_level(name) {
            self.flush();
            self.kind = Kind::Heading(level);
            if empty {
                self.kind = self.para_kind();
            }
            return;
        }
        match name {
            b"br" => self.line_break(),
            b"hr" => {
                self.flush();
                self.blocks.push(Block::Rule);
            }
            b"img" | b"image" => self.image(e),
            b"li" => {
                self.flush();
                let depth = self.lists.len().clamp(1, 255) as u8;
                let marker = match self.lists.last_mut() {
                    Some((true, next)) => {
                        let n = *next;
                        *next += 1;
                        ListMarker::Number(n)
                    }
                    _ => ListMarker::Bullet,
                };
                if !empty {
                    self.item_open = Some((marker, depth));
                    self.kind = Kind::Item(marker, depth);
                }
            }
            b"ul" | b"ol" => {
                self.flush();
                let start = attr(e, &[b"start"])
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(1);
                if !empty {
                    self.lists.push((name == b"ol", start));
                }
            }
            b"blockquote" => {
                self.flush();
                if !empty {
                    self.quote += 1;
                }
            }
            b"pre" => {
                self.flush();
                if !empty {
                    self.pre_depth += 1;
                    self.kind = Kind::Pre;
                }
            }
            b"b" | b"strong" if !empty => self.bold += 1,
            b"i" | b"em" | b"cite" | b"dfn" | b"var" if !empty => self.italic += 1,
            b"td" | b"th" => self.pending_space = true,
            n if BLOCKS.contains(&n) => {
                self.flush();
                self.kind = self.para_kind();
            }
            _ => {}
        }
    }

    fn end(&mut self, lname: &[u8]) {
        let lname = lname.to_ascii_lowercase();
        let name = lname.as_slice();
        if self.skip > 0 {
            self.skip -= 1;
            return;
        }
        if heading_level(name).is_some() {
            self.flush();
            self.kind = self.para_kind();
            return;
        }
        match name {
            b"li" => {
                self.flush();
                self.item_open = None;
                self.kind = Kind::Para;
            }
            b"ul" | b"ol" => {
                self.flush();
                self.lists.pop();
            }
            b"blockquote" => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            b"pre" => {
                self.flush();
                self.pre_depth = self.pre_depth.saturating_sub(1);
                self.kind = self.para_kind();
            }
            b"b" | b"strong" => self.bold = self.bold.saturating_sub(1),
            b"i" | b"em" | b"cite" | b"dfn" | b"var" => self.italic = self.italic.saturating_sub(1),
            n if BLOCKS.contains(&n) => {
                self.flush();
                self.kind = self.para_kind();
            }
            _ => {}
        }
    }
}

pub(crate) fn parse(xml: &[u8]) -> Parsed {
    let mut r = Reader::from_reader(xml);
    {
        let c = r.config_mut();
        c.check_end_names = false;
        c.allow_unmatched_ends = true;
        c.allow_dangling_amp = true;
    }
    let mut b = Builder::new();
    let mut warning = None;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => b.start(&e, false),
            Ok(Event::Empty(e)) => b.start(&e, true),
            Ok(Event::End(e)) => b.end(e.local_name().as_ref()),
            Ok(Event::Text(t)) => match t.decode() {
                Ok(s) => b.text(&s),
                Err(e) => warning = Some(format!("text encoding: {e}")),
            },
            Ok(Event::CData(t)) => {
                if let Ok(s) = t.decode() {
                    b.text(&s)
                }
            }
            Ok(Event::GeneralRef(g)) => {
                if let Ok(Some(c)) = g.resolve_char_ref() {
                    b.text(c.encode_utf8(&mut [0; 4]));
                } else if let Ok(name) = g.decode() {
                    match named_entity(&name) {
                        Some(s) => b.text(s),
                        None => b.text(&format!("&{name};")),
                    }
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => {
                warning = Some(format!("XML error at byte {}: {e}", r.error_position()));
                break;
            }
        }
    }
    b.flush();
    Parsed {
        blocks: b.blocks,
        warning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(s: &str) -> Vec<Block> {
        let p = parse(s.as_bytes());
        assert!(p.warning.is_none(), "{:?}", p.warning);
        p.blocks
    }

    #[test]
    fn paragraphs_headings_emphasis_and_whitespace() {
        let b = blocks(
            "<?xml version='1.0'?><!DOCTYPE html><html xmlns='http://www.w3.org/1999/xhtml'>\
             <head><title>T</title><style>p{}</style></head><body>\
             <h1>  Chapter\n One </h1><p>Hello,   <em>brave</em>\n <strong>new <i>world</i></strong>&amp;&#x41;&nbsp;&mdash;&unknown;</p>\
             <p>line<br/>break</p><div>  </div></body></html>",
        );
        assert_eq!(
            b[0],
            Block::Heading {
                level: 1,
                spans: vec![Span::plain("Chapter One")]
            }
        );
        let Block::Paragraph { spans, quote } = &b[1] else {
            panic!()
        };
        assert!(!quote);
        let styled: Vec<(&str, bool, bool)> = spans
            .iter()
            .map(|s| (s.text.as_str(), s.style.bold, s.style.italic))
            .collect();
        assert_eq!(
            styled,
            vec![
                // a collapsed space takes the style of the text after it
                ("Hello,", false, false),
                (" brave", false, true),
                (" new", true, false),
                (" world", true, true),
                ("&A\u{a0}\u{2014}&unknown;", false, false)
            ]
        );
        assert_eq!(b[2].text(), "line\nbreak");
        assert_eq!(b.len(), 3);
    }

    #[test]
    fn lists_quotes_pre_images_rules() {
        let b = blocks(
            "<body><ol start='3'><li><p>three</p></li><li>four<ul><li>sub</li></ul></li></ol>\
             <blockquote><p>quoted</p></blockquote><pre>  a\n    b\n</pre>\
             <img src='i.png' alt='A map'/><hr/><table><tr><td>x</td><td>y</td></tr></table></body>",
        );
        assert_eq!(
            b,
            vec![
                Block::ListItem {
                    marker: ListMarker::Number(3),
                    depth: 1,
                    spans: vec![Span::plain("three")]
                },
                Block::ListItem {
                    marker: ListMarker::Number(4),
                    depth: 1,
                    spans: vec![Span::plain("four")]
                },
                Block::ListItem {
                    marker: ListMarker::Bullet,
                    depth: 2,
                    spans: vec![Span::plain("sub")]
                },
                Block::Paragraph {
                    spans: vec![Span::plain("quoted")],
                    quote: true
                },
                Block::Preformatted {
                    text: "  a\n    b".into()
                },
                Block::Image {
                    alt: "A map".into(),
                    src: "i.png".into()
                },
                Block::Rule,
                Block::Paragraph {
                    spans: vec![Span::plain("x y")],
                    quote: false
                },
            ]
        );
    }

    #[test]
    fn malformed_xml_keeps_parsed_blocks() {
        let p = parse(b"<body><p>ok</p><p>broken <!-- never closed");
        assert_eq!(p.blocks[0].text(), "ok");
        assert!(p.warning.is_some());
    }
}
