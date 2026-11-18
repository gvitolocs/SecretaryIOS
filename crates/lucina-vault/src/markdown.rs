//! A note as blocks and styled runs, the same on every platform: a native view on
//! Juno, in SwiftUI or on Android draws these and never parses Markdown itself. Also
//! handles what Obsidian adds to Markdown: wikilinks, embeds, tags, `==highlight==`,
//! `%%comments%%` and callouts. The view is for reading only; the file stays as it is.
use crate::scan::percent_decode;
use ink_obsidian::parse::split_frontmatter;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub text: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub strike: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub code: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub mark: bool,
    /// A vault link's target as written, `#subpath` included, or a URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// True when `link` is a web address or other outside link.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub external: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Block {
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph {
        spans: Vec<Span>,
    },
    Code {
        language: String,
        text: String,
    },
    Math {
        text: String,
    },
    Quote {
        blocks: Vec<Block>,
    },
    Callout {
        kind: String,
        title: Vec<Span>,
        foldable: bool,
        blocks: Vec<Block>,
    },
    List {
        ordered: bool,
        start: u64,
        items: Vec<Item>,
    },
    Table {
        aligns: Vec<String>,
        head: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
    Rule,
    /// `![[x]]` or `![a](x)`: `target` as written. The caller resolves and draws it.
    Embed {
        target: String,
        alt: String,
        external: bool,
    },
    Html {
        text: String,
    },
    FootnoteDefinition {
        label: String,
        blocks: Vec<Block>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    /// `Some(done)` for a task.
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Default, Clone, Copy)]
struct Style {
    bold: bool,
    italic: bool,
    strike: bool,
}

/// Open containers while reading the event stream. Text goes to the innermost one.
enum Frame {
    Root(Vec<Block>),
    Quote(Vec<Block>),
    Footnote(String, Vec<Block>),
    Item(Option<bool>, Vec<Block>),
    List(bool, u64, Vec<Item>),
}

/// A table part-way through the parse, in the four pieces the `Table` events build up: the column
/// alignments, the header row, the body rows, and the flag saying the row about to close is still
/// the header (`TableHead`), so the head is taken once and not pushed again as a body row.
type TableParse = (Vec<String>, Vec<Vec<Span>>, Vec<Vec<Vec<Span>>>, bool);

struct Reader {
    stack: Vec<Frame>,
    spans: Vec<Span>,
    style: Style,
    link: Option<(String, bool)>,
    image: Option<(String, String)>,
    code: Option<(String, String)>,
    heading: Option<u8>,
    /// Inside `%%`, which may span lines and blocks.
    comment: bool,
    table: Option<TableParse>,
    row: Vec<Vec<Span>>,
}

fn level(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}
fn is_external(url: &str) -> bool {
    url.split_once(':').is_some_and(|(s, _)| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    })
}

impl Reader {
    fn push_block(&mut self, b: Block) {
        match self.stack.last_mut() {
            Some(Frame::Root(v) | Frame::Quote(v) | Frame::Footnote(_, v) | Frame::Item(_, v)) => {
                v.push(b)
            }
            Some(Frame::List(..)) | None => {}
        }
    }

    fn span(&self, text: &str) -> Span {
        Span {
            text: unescaped(text),
            bold: self.style.bold,
            italic: self.style.italic,
            strike: self.style.strike,
            link: self.link.as_ref().map(|l| l.0.clone()),
            external: self.link.as_ref().is_some_and(|l| l.1),
            ..Default::default()
        }
    }

    /// Plain text from Markdown: find what only Obsidian knows.
    fn text(&mut self, t: &str) {
        if let Some((_, alt)) = &mut self.image {
            alt.push_str(t);
            return;
        }
        let mut rest = t;
        let mut plain = String::new();
        while !rest.is_empty() {
            if self.comment {
                match rest.find("%%") {
                    Some(e) => {
                        rest = &rest[e + 2..];
                        self.comment = false;
                        continue;
                    }
                    None => break,
                }
            }
            if let Some(r) = rest.strip_prefix("%%") {
                self.flush_plain(&mut plain);
                self.comment = true;
                rest = r;
                continue;
            }
            if let Some(r) = rest.strip_prefix("==") {
                if let Some(e) = r.find("==").filter(|e| *e > 0) {
                    self.flush_plain(&mut plain);
                    let mut s = self.span(&r[..e]);
                    s.mark = true;
                    self.spans.push(s);
                    rest = &r[e + 2..];
                    continue;
                }
            }
            let embed = rest.starts_with("![[");
            if embed || rest.starts_with("[[") {
                let r = &rest[if embed { 3 } else { 2 }..];
                if let Some(e) = r.find("]]").filter(|e| *e > 0 && !r[..*e].contains("[[")) {
                    let inner = &r[..e];
                    let (target, alias) = match inner.split_once('|') {
                        Some((t, a)) => (t.strip_suffix('\\').unwrap_or(t).trim(), Some(a.trim())),
                        None => (inner.trim(), None),
                    };
                    self.flush_plain(&mut plain);
                    if embed {
                        self.flush_paragraph();
                        self.push_block(Block::Embed {
                            target: target.to_owned(),
                            alt: alias.unwrap_or("").to_owned(),
                            external: false,
                        });
                    } else {
                        let shown = alias.map_or_else(|| display_of(target), str::to_owned);
                        let mut s = self.span(&shown);
                        s.link = Some(target.to_owned());
                        s.external = false;
                        self.spans.push(s);
                    }
                    rest = &r[e + 2..];
                    continue;
                }
            }
            if rest.starts_with('#') && plain.chars().next_back().is_none_or(char::is_whitespace) {
                let body = &rest[1..];
                let end = body
                    .find(|c: char| {
                        c.is_whitespace()
                            || "'!\"#$%&()*+,.:;<=>?@^`{|}~[]\\".contains(c)
                            || matches!(c, '\u{2000}'..='\u{206F}' | '\u{2E00}'..='\u{2E7F}')
                    })
                    .unwrap_or(body.len());
                let tag = &body[..end];
                if tag.chars().any(|c| !c.is_ascii_digit()) {
                    self.flush_plain(&mut plain);
                    let mut s = self.span(&rest[..=end]);
                    s.tag = Some(tag.to_owned());
                    self.spans.push(s);
                    rest = &body[end..];
                    continue;
                }
            }
            let c = rest.chars().next().unwrap_or(' ');
            plain.push(c);
            rest = &rest[c.len_utf8()..];
        }
        self.flush_plain(&mut plain);
    }

    fn flush_plain(&mut self, plain: &mut String) {
        if plain.is_empty() {
            return;
        }
        let s = self.span(plain);
        plain.clear();
        // Keep runs whole, so the views wrap words and not styles.
        if let Some(last) = self.spans.last_mut() {
            let same = Span {
                text: String::new(),
                ..last.clone()
            } == Span {
                text: String::new(),
                ..s.clone()
            };
            if same && last.tag.is_none() {
                last.text.push_str(&s.text);
                return;
            }
        }
        self.spans.push(s);
    }

    fn flush_paragraph(&mut self) {
        let spans = std::mem::take(&mut self.spans);
        if spans.iter().any(|s| !s.text.trim().is_empty()) {
            self.push_block(Block::Paragraph { spans });
        }
    }

    fn event(&mut self, e: Event<'_>) {
        match e {
            Event::Text(t) => {
                if let Some((_, code)) = &mut self.code {
                    code.push_str(&as_written(&t));
                } else {
                    self.text(&t);
                }
            }
            Event::Code(t) => {
                if self.comment {
                    return;
                }
                let mut s = self.span("");
                s.text = as_written(&t);
                s.code = true;
                self.spans.push(s);
            }
            // Obsidian shows a single newline as a line break unless strictLineBreaks is on.
            Event::SoftBreak | Event::HardBreak => {
                let s = self.span("\n");
                self.spans.push(s);
            }
            Event::Rule => self.push_block(Block::Rule),
            Event::Html(t) | Event::InlineHtml(t) => {
                if !t.trim_start().starts_with("<!--") {
                    if self.spans.is_empty() {
                        self.push_block(Block::Html {
                            text: t.to_string(),
                        });
                    } else {
                        let s = self.span(&t);
                        self.spans.push(s);
                    }
                }
            }
            Event::InlineMath(t) => {
                let mut s = self.span(&t);
                s.code = true;
                self.spans.push(s);
            }
            Event::DisplayMath(t) => {
                self.flush_paragraph();
                self.push_block(Block::Math {
                    text: t.to_string(),
                });
            }
            Event::FootnoteReference(l) => {
                let s = self.span(&format!("[{l}]"));
                self.spans.push(s);
            }
            Event::TaskListMarker(done) => {
                if let Some(Frame::Item(task, _)) = self.stack.last_mut() {
                    *task = Some(done);
                }
            }
            Event::Start(t) => self.start(t),
            Event::End(t) => self.end(t),
        }
    }

    fn start(&mut self, t: Tag<'_>) {
        // A tight list item's text has no paragraph of its own to end it.
        if matches!(
            t,
            Tag::List(_)
                | Tag::CodeBlock(_)
                | Tag::BlockQuote(_)
                | Tag::Table(_)
                | Tag::Heading { .. }
        ) {
            self.flush_paragraph();
        }
        match t {
            Tag::Emphasis => self.style.italic = true,
            Tag::Strong => self.style.bold = true,
            Tag::Strikethrough => self.style.strike = true,
            Tag::Link { dest_url, .. } => {
                let ext = is_external(&dest_url);
                let d = if ext {
                    dest_url.to_string()
                } else {
                    percent_decode(&dest_url)
                };
                self.link = Some((d, ext));
            }
            Tag::Image { dest_url, .. } => {
                let ext = is_external(&dest_url);
                let d = if ext {
                    dest_url.to_string()
                } else {
                    percent_decode(&dest_url)
                };
                self.image = Some((d, String::new()));
            }
            Tag::CodeBlock(kind) => {
                let language = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(l) => l.to_string(),
                    pulldown_cmark::CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((language, String::new()));
            }
            Tag::Heading { level: l, .. } => self.heading = Some(level(l)),
            Tag::BlockQuote(_) => self.stack.push(Frame::Quote(Vec::new())),
            Tag::FootnoteDefinition(l) => {
                self.stack.push(Frame::Footnote(l.to_string(), Vec::new()))
            }
            Tag::List(start) => {
                self.stack
                    .push(Frame::List(start.is_some(), start.unwrap_or(1), Vec::new()))
            }
            Tag::Item => self.stack.push(Frame::Item(None, Vec::new())),
            Tag::Table(aligns) => {
                let a = aligns
                    .iter()
                    .map(|x| {
                        match x {
                            pulldown_cmark::Alignment::Left => "left",
                            pulldown_cmark::Alignment::Right => "right",
                            pulldown_cmark::Alignment::Center => "center",
                            pulldown_cmark::Alignment::None => "",
                        }
                        .to_owned()
                    })
                    .collect();
                self.table = Some((a, Vec::new(), Vec::new(), true));
            }
            _ => {}
        }
    }

    fn end(&mut self, t: TagEnd) {
        match t {
            TagEnd::Emphasis => self.style.italic = false,
            TagEnd::Strong => self.style.bold = false,
            TagEnd::Strikethrough => self.style.strike = false,
            TagEnd::Link => self.link = None,
            TagEnd::Image => {
                if let Some((target, alt)) = self.image.take() {
                    self.flush_paragraph();
                    let external = is_external(&target);
                    self.push_block(Block::Embed {
                        target,
                        alt,
                        external,
                    });
                }
            }
            TagEnd::CodeBlock => {
                if let Some((language, text)) = self.code.take() {
                    self.push_block(Block::Code { language, text });
                }
            }
            TagEnd::Paragraph => self.flush_paragraph(),
            TagEnd::Heading(_) => {
                let spans = std::mem::take(&mut self.spans);
                let level = self.heading.take().unwrap_or(1);
                self.push_block(Block::Heading { level, spans });
            }
            TagEnd::BlockQuote(_) => {
                self.flush_paragraph();
                if let Some(Frame::Quote(blocks)) = self.stack.pop() {
                    let b = callout(blocks);
                    self.push_block(b);
                }
            }
            TagEnd::FootnoteDefinition => {
                self.flush_paragraph();
                if let Some(Frame::Footnote(label, blocks)) = self.stack.pop() {
                    self.push_block(Block::FootnoteDefinition { label, blocks });
                }
            }
            TagEnd::Item => {
                self.flush_paragraph();
                if let Some(Frame::Item(task, blocks)) = self.stack.pop() {
                    if let Some(Frame::List(_, _, items)) = self.stack.last_mut() {
                        items.push(Item { task, blocks });
                    }
                }
            }
            TagEnd::List(_) => {
                if let Some(Frame::List(ordered, start, items)) = self.stack.pop() {
                    self.push_block(Block::List {
                        ordered,
                        start,
                        items,
                    });
                }
            }
            TagEnd::TableCell => {
                let spans = std::mem::take(&mut self.spans);
                self.row.push(spans);
            }
            TagEnd::TableHead => {
                let row = std::mem::take(&mut self.row);
                if let Some(t) = &mut self.table {
                    t.1 = row;
                    t.3 = false;
                }
            }
            TagEnd::TableRow => {
                let row = std::mem::take(&mut self.row);
                if let Some(t) = &mut self.table {
                    t.2.push(row);
                }
            }
            TagEnd::Table => {
                if let Some((aligns, head, rows, _)) = self.table.take() {
                    self.push_block(Block::Table { aligns, head, rows });
                }
            }
            _ => {}
        }
    }
}

// `\[[x]]` and `\#x` are plain text to Obsidian, but the Markdown parser removes the backslash before
// we would see it. They cross it as private characters.
const ESC_BRACKET: &str = "\u{E000}";
const ESC_HASH: &str = "\u{E001}";
fn unescaped(s: &str) -> String {
    s.replace(ESC_BRACKET, "[").replace(ESC_HASH, "#")
}
fn as_written(s: &str) -> String {
    s.replace(ESC_BRACKET, "\\[").replace(ESC_HASH, "\\#")
}

/// `alpha#Section` shows as `alpha > Section`.
fn display_of(target: &str) -> String {
    let mut parts = target.split('#');
    let mut s = parts.next().unwrap_or("").to_owned();
    for p in parts {
        if s.is_empty() {
            s.push_str(p);
        } else {
            s.push_str(" > ");
            s.push_str(p);
        }
    }
    s
}

/// `> [!note]- Title`: a quote whose first line says so. The rest of that paragraph is body.
fn callout(mut blocks: Vec<Block>) -> Block {
    let parsed = match blocks.first() {
        Some(Block::Paragraph { spans }) => spans.first().and_then(|s| {
            let r = s.text.strip_prefix("[!")?;
            let (kind, after) = r.split_once(']')?;
            let (foldable, after) = match after.strip_prefix(['-', '+']) {
                Some(a) => (true, a),
                None => (false, after),
            };
            let mut all: Vec<Span> = spans.clone();
            all[0].text = after.trim_start().to_owned();
            let mut title = Vec::new();
            let mut body = Vec::new();
            for mut s in all {
                if !body.is_empty() {
                    body.push(s);
                } else if let Some((head, tail)) = s.text.clone().split_once('\n') {
                    let mut t = s.clone();
                    t.text = head.to_owned();
                    title.push(t);
                    s.text = tail.to_owned();
                    body.push(s);
                } else {
                    title.push(s);
                }
            }
            Some((kind.to_lowercase(), foldable, title, body))
        }),
        _ => None,
    };
    match parsed {
        Some((kind, foldable, title, body)) => {
            blocks.remove(0);
            let body: Vec<Span> = body.into_iter().filter(|s| !s.text.is_empty()).collect();
            if !body.is_empty() {
                blocks.insert(0, Block::Paragraph { spans: body });
            }
            Block::Callout {
                kind,
                title: title.into_iter().filter(|s| !s.text.is_empty()).collect(),
                foldable,
                blocks,
            }
        }
        None => Block::Quote { blocks },
    }
}

/// A note's body as blocks; properties are left to `scan`.
pub fn blocks(text: &str) -> Vec<Block> {
    let (_, body, _) = split_frontmatter(text);
    let body = body.replace("\\[", ESC_BRACKET).replace("\\#", ESC_HASH);
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_MATH;
    let mut r = Reader {
        stack: vec![Frame::Root(Vec::new())],
        spans: Vec::new(),
        style: Style::default(),
        link: None,
        image: None,
        code: None,
        heading: None,
        comment: false,
        table: None,
        row: Vec::new(),
    };
    // Text events split at each `[`, and a wikilink would be cut in pieces: merge first.
    let mut pending = String::new();
    for e in Parser::new_ext(&body, opts) {
        match e {
            Event::Text(t) if r.code.is_none() => pending.push_str(&t),
            // Code in a link's alias: `[[a|b `c`]]` stays one link, as in Obsidian.
            Event::Code(t)
                if pending
                    .rfind("[[")
                    .is_some_and(|o| pending.rfind("]]").is_none_or(|c| c < o)) =>
            {
                pending.push_str(&t)
            }
            other => {
                if !pending.is_empty() {
                    let t = std::mem::take(&mut pending);
                    r.text(&t);
                }
                r.event(other);
            }
        }
    }
    if !pending.is_empty() {
        r.text(&pending);
    }
    r.flush_paragraph();
    match r.stack.pop() {
        Some(Frame::Root(v)) => v,
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn para(text: &str) -> Vec<Span> {
        match blocks(text).into_iter().next() {
            Some(Block::Paragraph { spans }) => spans,
            other => panic!("{other:?}"),
        }
    }
    #[test]
    fn wikilinks_tags_marks_comments() {
        let s = para("See [[A/B#Head|shown]] and [[C#D]] ==hot== %%hidden%% #tag/x **b** `c` [u](https://x.y) [v](a%20b.md)");
        let links: Vec<_> = s
            .iter()
            .filter_map(|x| {
                x.link
                    .as_ref()
                    .map(|l| (x.text.as_str(), l.as_str(), x.external))
            })
            .collect();
        assert_eq!(
            links,
            vec![
                ("shown", "A/B#Head", false),
                ("C > D", "C#D", false),
                ("u", "https://x.y", true),
                ("v", "a b.md", false)
            ]
        );
        assert!(s.iter().any(|x| x.mark && x.text == "hot"));
        assert!(s.iter().any(|x| x.tag.as_deref() == Some("tag/x")));
        assert!(s.iter().any(|x| x.bold && x.text == "b"));
        assert!(s.iter().any(|x| x.code && x.text == "c"));
        assert!(!s.iter().any(|x| x.text.contains("hidden")));
    }
    #[test]
    fn escapes_and_comments_as_obsidian_reads_them() {
        let s = para("\\[[Esc]] \\#esc `\\[[c]]` [[Real|a `b` c]]");
        assert!(s.iter().all(|x| x.tag.is_none()));
        let links: Vec<_> = s.iter().filter_map(|x| x.link.as_deref()).collect();
        assert_eq!(links, vec!["Real"]);
        assert!(s[0].text.starts_with("[[Esc]] #esc"));
        assert!(s.iter().any(|x| x.code && x.text == "\\[[c]]"));
        let b =
            blocks("before %%\nhidden [[x]]\n%% after\n\nnext\n\n%%\nwhole\n\nblocks\n%%\n\nlast");
        let text: String = b
            .iter()
            .map(|x| match x {
                Block::Paragraph { spans } => {
                    spans.iter().map(|s| s.text.as_str()).collect::<String>()
                }
                _ => String::new(),
            })
            .collect::<Vec<_>>()
            .join("|");
        assert_eq!(text, "before \n\n after|next|last");
    }
    #[test]
    fn embeds_become_blocks() {
        let b = blocks("before ![[pic.png|200]] after\n\n![alt](https://x.y/z.png)");
        assert!(
            matches!(&b[1], Block::Embed { target, alt, external: false } if target == "pic.png" && alt == "200")
        );
        assert!(matches!(&b[3], Block::Embed { external: true, .. }));
        assert_eq!(b.len(), 4);
    }
    #[test]
    fn containers() {
        let b = blocks("---\na: 1\n---\n# T\n\n> [!Note]- Title [[x]]\n> body\n\n> plain\n\n- [ ] one\n- [x] two\n  - deep\n\n1. a\n\n| h |\n|---|\n| c |\n\n---\n\n```rust\nfn\n```\n\n$$\nx^2\n$$\n");
        assert!(matches!(&b[0], Block::Heading { level: 1, .. }));
        match &b[1] {
            Block::Callout {
                kind,
                foldable,
                title,
                blocks,
            } => {
                assert_eq!(kind, "note");
                assert!(foldable);
                assert_eq!(title[0].text, "Title ");
                assert_eq!(title[1].link.as_deref(), Some("x"));
                assert!(
                    matches!(&blocks[0], Block::Paragraph { spans } if spans[0].text == "body"),
                    "{blocks:?}"
                );
            }
            o => panic!("{o:?}"),
        }
        assert!(matches!(&b[2], Block::Quote { .. }));
        match &b[3] {
            Block::List {
                ordered: false,
                items,
                ..
            } => {
                assert_eq!(items[0].task, Some(false));
                assert_eq!(items[1].task, Some(true));
                assert!(matches!(items[1].blocks[1], Block::List { .. }));
            }
            o => panic!("{o:?}"),
        }
        assert!(matches!(&b[4], Block::List { ordered: true, .. }));
        assert!(matches!(&b[5], Block::Table { .. }));
        assert!(matches!(&b[6], Block::Rule));
        assert!(
            matches!(&b[7], Block::Code { language, text } if language == "rust" && text == "fn\n")
        );
        assert!(matches!(&b[8], Block::Math { .. }));
    }
}
