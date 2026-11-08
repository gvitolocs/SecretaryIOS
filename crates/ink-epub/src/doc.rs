//! Simplified document model shared by every source format and consumed by the layout engine.
//!
//! Positions inside a chapter are **character offsets** (Unicode scalar values) into the concatenation
//! of its blocks' texts; images and rules count as one character. They do not depend on fonts or page
//! size, which is what keeps a reading position stable across re-pagination.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct SpanStyle {
    pub bold: bool,
    pub italic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// Whitespace-collapsed text; `'\n'` is a forced line break (`<br>`).
    pub text: String,
    pub style: SpanStyle,
}

impl Span {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            style: SpanStyle::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListMarker {
    Bullet,
    Number(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Paragraph {
        spans: Vec<Span>,
        /// Inside a blockquote: indented.
        quote: bool,
    },
    ListItem {
        marker: ListMarker,
        /// Nesting depth, 1 = top level.
        depth: u8,
        spans: Vec<Span>,
    },
    /// Whitespace-preserving text (code, verse); lines separated by `'\n'`.
    Preformatted {
        text: String,
    },
    /// A picture. ink-epub only records `src` (as written in the book) and `alt`; the pixels are supplied by
    /// the app through `ink_render::Images`.
    Image {
        alt: String,
        src: String,
    },
    Rule,
}

impl Block {
    pub fn spans(&self) -> &[Span] {
        match self {
            Block::Heading { spans, .. }
            | Block::Paragraph { spans, .. }
            | Block::ListItem { spans, .. } => spans,
            _ => &[],
        }
    }
    /// Plain text of the block (images/rules: empty).
    pub fn text(&self) -> String {
        match self {
            Block::Preformatted { text } => text.clone(),
            _ => self.spans().iter().map(|s| s.text.as_str()).collect(),
        }
    }
    /// Length in the chapter's offset space.
    pub fn char_len(&self) -> u32 {
        match self {
            Block::Image { .. } | Block::Rule => 1,
            Block::Preformatted { text } => text.chars().count() as u32,
            _ => self
                .spans()
                .iter()
                .map(|s| s.text.chars().count() as u32)
                .sum(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Chapter {
    pub title: Option<String>,
    /// Source document (EPUB spine item path), empty for TXT/Markdown.
    pub href: String,
    pub blocks: Vec<Block>,
}

impl Chapter {
    /// Offset of each block's first character, plus the chapter length as the last element.
    pub fn block_starts(&self) -> Vec<u32> {
        let mut v = Vec::with_capacity(self.blocks.len() + 1);
        let mut o = 0u32;
        for b in &self.blocks {
            v.push(o);
            o += b.char_len();
        }
        v.push(o);
        v
    }
    pub fn char_len(&self) -> u32 {
        self.blocks.iter().map(Block::char_len).sum()
    }
    pub fn first_heading(&self) -> Option<String> {
        self.blocks.iter().find_map(|b| match b {
            Block::Heading { .. } => Some(b.text().trim().to_owned()).filter(|t| !t.is_empty()),
            _ => None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TocEntry {
    pub title: String,
    /// 0 = top level.
    pub depth: u8,
    /// Spine/chapter index the entry points to, if it resolves.
    pub chapter: Option<usize>,
    pub href: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    Epub,
    Txt,
    Markdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Book {
    pub format: SourceFormat,
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub language: Option<String>,
    pub chapters: Vec<Chapter>,
    pub toc: Vec<TocEntry>,
    /// Non-fatal problems (malformed chapter XML, missing spine items, …).
    pub warnings: Vec<String>,
}

impl Book {
    pub fn new(format: SourceFormat) -> Self {
        Self {
            format,
            title: None,
            authors: Vec::new(),
            language: None,
            chapters: Vec::new(),
            toc: Vec::new(),
            warnings: Vec::new(),
        }
    }
    pub fn char_len(&self) -> u64 {
        self.chapters.iter().map(|c| u64::from(c.char_len())).sum()
    }
}
