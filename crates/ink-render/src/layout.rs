//! Text layout and pagination.
//!
//! * Words are measured with the font's advances and kerning; lines are filled greedily.
//! * Justification is hyphen-free: the slack of a full line is spread over its word gaps. A line whose
//!   gaps would grow beyond [`Style::max_gap_ratio`] × the space width is set ragged instead, which
//!   avoids "rivers" on narrow lines. A word wider than the line is split between characters.
//! * Pages hold whole lines. Headings are kept with the following line. Every chapter starts a page.
//! * Positions are [`Anchor`]s (chapter + character offset, see `ink_epub::doc`), independent of fonts,
//!   so a reading position maps onto the new pagination after any style change.
//!
//! Runs reference the book's text by (block, span, byte range) instead of copying it, which keeps the
//! pagination of a 2 MB book to a few MB of memory.
use crate::font::{Face, Fonts};
use ink_epub::{Block, Book, Chapter, ListMarker, Span, SpanStyle};
use ink_kindle_hal::Size;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Margins {
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
    pub left: u32,
}

/// Body font sizes offered by `font+` / `font-` (pixels at 300 dpi; 40 px ≈ 9.6 pt).
pub const FONT_SIZES: &[u16] = &[28, 32, 36, 40, 44, 50, 56, 64, 72];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    /// Body text size in pixels.
    pub font_px: u16,
    /// Line box height as a multiple of the font size.
    pub line_height: f32,
    pub margins: Margins,
    /// Extra space between paragraphs, in lines.
    pub paragraph_spacing: f32,
    /// First-line indent, in ems (not after headings or at chapter start).
    pub indent_em: f32,
    pub justify: bool,
    /// Above this gap/space ratio a justified line is set ragged instead.
    pub max_gap_ratio: f32,
    /// Footer (page number, chapter) text size; fixed so it does not move with the body size.
    pub footer_px: u16,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            font_px: 40,
            line_height: 1.4,
            margins: Margins {
                top: 64,
                right: 72,
                bottom: 100,
                left: 72,
            },
            paragraph_spacing: 0.0,
            indent_em: 1.5,
            justify: true,
            // Without hyphenation, long Italian words leave big gaps; a ragged line mid-paragraph looks worse.
            max_gap_ratio: 8.0,
            footer_px: 26,
        }
    }
}

impl Style {
    /// Next size up/down in [`FONT_SIZES`].
    pub fn step_font(&self, delta: i32) -> u16 {
        let i = FONT_SIZES
            .iter()
            .position(|&s| s >= self.font_px)
            .unwrap_or(FONT_SIZES.len() - 1) as i32;
        FONT_SIZES[(i + delta).clamp(0, FONT_SIZES.len() as i32 - 1) as usize]
    }
    fn heading_px(&self, level: u8) -> u16 {
        let scale = match level {
            1 => 1.6,
            2 => 1.35,
            3 => 1.17,
            _ => 1.0,
        };
        (f32::from(self.font_px) * scale).round() as u16
    }
}

/// Reading position: chapter index and character offset within the chapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Anchor {
    pub chapter: u32,
    pub offset: u32,
}

/// What a run draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunSrc {
    /// Bytes `start..end` of span `span` of block `block` (`span == u16::MAX`: preformatted text).
    Text {
        block: u32,
        span: u16,
        start: u32,
        end: u32,
    },
    Marker(ListMarker),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Run {
    /// Pen x relative to the left margin.
    pub x: f32,
    pub face: Face,
    pub px: u16,
    pub src: RunSrc,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LineContent {
    Text(Vec<Run>),
    Image { block: u32 },
    Rule,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub content: LineContent,
    pub height: i32,
    /// Baseline offset from the top of the line box.
    pub ascent: i32,
    /// Space above the line; dropped at the top of a page.
    pub space_before: i32,
    pub keep_with_next: bool,
    /// Chapter offsets covered by this line.
    pub start: u32,
    pub end: u32,
    /// Width used by the line (justified lines: the full measure).
    pub width: f32,
    pub justified: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlacedLine {
    /// Top of the line box relative to the top margin.
    pub y: i32,
    pub line: Line,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub chapter: u32,
    pub start: u32,
    pub end: u32,
    pub lines: Vec<PlacedLine>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pagination {
    pub size: Size,
    pub style: Style,
    pub pages: Vec<Page>,
}

impl Pagination {
    /// Page containing `a` (the last page starting at or before it).
    pub fn page_for(&self, a: Anchor) -> usize {
        let key = (a.chapter, a.offset);
        let idx = self.pages.partition_point(|p| (p.chapter, p.start) <= key);
        idx.saturating_sub(1)
    }
    pub fn anchor_of(&self, page: usize) -> Anchor {
        let p = &self.pages[page.min(self.pages.len().saturating_sub(1))];
        Anchor {
            chapter: p.chapter,
            offset: p.start,
        }
    }
    pub fn content_width(&self) -> f32 {
        (self.size.w - self.style.margins.left - self.style.margins.right) as f32
    }
}

pub fn span_text(chapter: &Chapter, block: u32, span: u16) -> &str {
    match &chapter.blocks[block as usize] {
        Block::Preformatted { text } => text,
        b => &b.spans()[span as usize].text,
    }
}

#[derive(Debug, Clone)]
struct Piece {
    span: u16,
    start: u32,
    end: u32,
    face: Face,
    width: f32,
}

#[derive(Debug, Clone)]
struct Word {
    pieces: Vec<Piece>,
    width: f32,
    start: u32,
    end: u32,
    break_after: bool,
}

struct Para<'a> {
    block: u32,
    /// (span index, text, style)
    spans: Vec<(u16, &'a str, SpanStyle)>,
    offset: u32,
    px: u16,
    bold: bool,
    left: f32,
    first_indent: f32,
    justify: bool,
    marker: Option<ListMarker>,
    space_before: i32,
    keep_with_next: bool,
}

fn words(fonts: &mut Fonts, p: &Para) -> Vec<Word> {
    let mut out: Vec<Word> = Vec::new();
    let mut cur: Option<Word> = None;
    let mut off = p.offset;
    for &(si, text, style) in &p.spans {
        let mut face = Face::from_style(style);
        if p.bold {
            face = face.bolder();
        }
        let mut prev: Option<char> = None;
        for (bi, c) in text.char_indices() {
            let here = off;
            off += 1;
            if c.is_whitespace() && c != '\u{a0}' {
                if let Some(w) = cur.take() {
                    out.push(w);
                }
                if c == '\n' {
                    if let Some(w) = out.last_mut() {
                        w.break_after = true;
                    }
                }
                prev = None;
                continue;
            }
            let w = cur.get_or_insert_with(|| Word {
                pieces: Vec::new(),
                width: 0.0,
                start: here,
                end: here,
                break_after: false,
            });
            let adv = fonts.advance(face, p.px, c);
            let same = w
                .pieces
                .last()
                .is_some_and(|pc| pc.span == si && pc.face == face);
            if !same {
                w.pieces.push(Piece {
                    span: si,
                    start: bi as u32,
                    end: bi as u32,
                    face,
                    width: 0.0,
                });
                prev = None;
            }
            let k = prev.map_or(0.0, |pc| fonts.kern(face, p.px, pc, c));
            let pc = w.pieces.last_mut().expect("piece just ensured");
            pc.end = (bi + c.len_utf8()) as u32;
            pc.width += k + adv;
            w.width += k + adv;
            w.end = here + 1;
            prev = Some(c);
        }
        // A span boundary is not a word boundary ("<b>bo</b>ld"); `prev` resets for kerning only.
    }
    if let Some(w) = cur.take() {
        out.push(w);
    }
    out
}

/// Split a word wider than `max` into pieces that fit (character boundaries, no hyphen).
fn split_word(
    fonts: &mut Fonts,
    chapter: &Chapter,
    block: u32,
    px: u16,
    w: Word,
    max: f32,
) -> Vec<Word> {
    let mut out = Vec::new();
    let mut cur = Word {
        pieces: Vec::new(),
        width: 0.0,
        start: w.start,
        end: w.start,
        break_after: false,
    };
    let mut off = w.start;
    for pc in &w.pieces {
        let text = &span_text(chapter, block, pc.span)[pc.start as usize..pc.end as usize];
        for (bi, c) in text.char_indices() {
            let adv = fonts.advance(pc.face, px, c);
            if cur.width + adv > max && !cur.pieces.is_empty() {
                let next_start = off;
                out.push(std::mem::replace(
                    &mut cur,
                    Word {
                        pieces: Vec::new(),
                        width: 0.0,
                        start: next_start,
                        end: next_start,
                        break_after: false,
                    },
                ));
            }
            let b = pc.start + bi as u32;
            match cur.pieces.last_mut() {
                Some(last) if last.span == pc.span && last.face == pc.face && last.end == b => {
                    last.end = b + c.len_utf8() as u32;
                    last.width += adv;
                }
                _ => cur.pieces.push(Piece {
                    span: pc.span,
                    start: b,
                    end: b + c.len_utf8() as u32,
                    face: pc.face,
                    width: adv,
                }),
            }
            cur.width += adv;
            off += 1;
            cur.end = off;
        }
    }
    cur.break_after = w.break_after;
    out.push(cur);
    out
}

fn marker_text(m: ListMarker) -> String {
    match m {
        ListMarker::Bullet => "\u{2022}".into(),
        ListMarker::Number(n) => format!("{n}."),
    }
}

/// Lay out one paragraph-like block into lines.
fn layout_para(
    fonts: &mut Fonts,
    chapter: &Chapter,
    style: &Style,
    measure: f32,
    p: Para,
    out: &mut Vec<Line>,
) {
    let (asc, desc) = fonts.line_metrics(p.px);
    let height = (f32::from(p.px) * style.line_height).round() as i32;
    // Centre the glyph box in the line box.
    let ascent = ((height as f32 - (asc + desc)) / 2.0 + asc).round() as i32;
    let space_face = if p.bold { Face::Bold } else { Face::Regular };
    let space = fonts.advance(space_face, p.px, ' ');
    let max_line = (measure - p.left - p.first_indent.max(0.0)).max(f32::from(p.px));
    let mut ws = Vec::new();
    for w in words(fonts, &p) {
        if w.width > max_line {
            ws.extend(split_word(fonts, chapter, p.block, p.px, w, max_line));
        } else {
            ws.push(w);
        }
    }
    if ws.is_empty() {
        return;
    }
    let first_line_idx = out.len();
    let mut i = 0;
    while i < ws.len() {
        let first = out.len() == first_line_idx;
        let indent = if first { p.first_indent } else { 0.0 };
        let left = p.left + indent;
        let avail = measure - left;
        // greedy fill
        let mut j = i;
        let mut natural = 0.0;
        while j < ws.len() {
            let add = if j == i {
                ws[j].width
            } else {
                space + ws[j].width
            };
            if j > i && natural + add > avail {
                break;
            }
            natural += add;
            j += 1;
            if ws[j - 1].break_after {
                break;
            }
        }
        let line_words = &ws[i..j];
        let last_of_para = j == ws.len();
        let forced = line_words.last().is_some_and(|w| w.break_after);
        let gaps = line_words.len().saturating_sub(1);
        let mut gap = space;
        let mut justified = false;
        if p.justify && !last_of_para && !forced && gaps > 0 {
            let g = space + (avail - natural) / gaps as f32;
            if g <= space * style.max_gap_ratio {
                gap = g;
                justified = true;
            }
        }
        let mut runs = Vec::new();
        if first {
            if let Some(m) = p.marker {
                let t = marker_text(m);
                let mw = fonts.measure(Face::Regular, p.px, &t);
                runs.push(Run {
                    x: (p.left - mw - space).max(0.0),
                    face: Face::Regular,
                    px: p.px,
                    src: RunSrc::Marker(m),
                });
            }
        }
        let mut x = left;
        for (k, w) in line_words.iter().enumerate() {
            if k > 0 {
                x += gap;
            }
            for pc in &w.pieces {
                runs.push(Run {
                    x,
                    face: pc.face,
                    px: p.px,
                    src: RunSrc::Text {
                        block: p.block,
                        span: pc.span,
                        start: pc.start,
                        end: pc.end,
                    },
                });
                x += pc.width;
            }
        }
        out.push(Line {
            content: LineContent::Text(runs),
            height,
            ascent,
            space_before: if first { p.space_before } else { 0 },
            keep_with_next: p.keep_with_next,
            start: line_words[0].start,
            end: line_words[line_words.len() - 1].end,
            width: x - left,
            justified,
        });
        i = j;
    }
}

fn spans_of(spans: &[Span]) -> Vec<(u16, &str, SpanStyle)> {
    spans
        .iter()
        .enumerate()
        .map(|(i, s)| (i as u16, s.text.as_str(), s.style))
        .collect()
}

/// Lay out a chapter into lines (no pagination).
pub fn layout_chapter(
    fonts: &mut Fonts,
    style: &Style,
    measure: f32,
    chapter: &Chapter,
) -> Vec<Line> {
    let mut out = Vec::new();
    let body_line = (f32::from(style.font_px) * style.line_height).round() as i32;
    let em = f32::from(style.font_px);
    let para_gap = (style.paragraph_spacing * body_line as f32).round() as i32;
    let starts = chapter.block_starts();
    let mut after_heading = true; // chapter start counts as "after a heading": no indent
    let mut extra_space = 0;
    let mut prev_group: Option<u8> = None;
    for (bi, block) in chapter.blocks.iter().enumerate() {
        let offset = starts[bi];
        // Blank space where body text, quotes, lists and preformatted text meet.
        let group = match block {
            Block::Paragraph { quote: false, .. } => Some(0u8),
            Block::Paragraph { quote: true, .. } => Some(1),
            Block::ListItem { .. } => Some(2),
            Block::Preformatted { .. } => Some(3),
            _ => None,
        };
        if group.is_some() && prev_group.is_some() && group != prev_group {
            extra_space += body_line / 3;
        }
        prev_group = group;
        let base = Para {
            block: bi as u32,
            spans: Vec::new(),
            offset,
            px: style.font_px,
            bold: false,
            left: 0.0,
            first_indent: 0.0,
            justify: style.justify,
            marker: None,
            space_before: para_gap + extra_space,
            keep_with_next: false,
        };
        extra_space = 0;
        match block {
            Block::Heading { level, spans } => {
                let px = style.heading_px(*level);
                layout_para(
                    fonts,
                    chapter,
                    style,
                    measure,
                    Para {
                        spans: spans_of(spans),
                        px,
                        bold: true,
                        justify: false,
                        space_before: if out.is_empty() { 0 } else { body_line },
                        keep_with_next: true,
                        ..base
                    },
                    &mut out,
                );
                extra_space = body_line / 2;
                after_heading = true;
            }
            Block::Paragraph { spans, quote } => {
                let left = if *quote { 2.0 * em } else { 0.0 };
                layout_para(
                    fonts,
                    chapter,
                    style,
                    measure - if *quote { 2.0 * em } else { 0.0 },
                    Para {
                        spans: spans_of(spans),
                        left,
                        first_indent: if after_heading || *quote {
                            0.0
                        } else {
                            style.indent_em * em
                        },
                        ..base
                    },
                    &mut out,
                );
                after_heading = false;
            }
            Block::ListItem {
                marker,
                depth,
                spans,
            } => {
                layout_para(
                    fonts,
                    chapter,
                    style,
                    measure,
                    Para {
                        spans: spans_of(spans),
                        left: 1.6 * em * f32::from(*depth),
                        marker: Some(*marker),
                        justify: false,
                        ..base
                    },
                    &mut out,
                );
                after_heading = false;
            }
            Block::Preformatted { text } => {
                layout_para(
                    fonts,
                    chapter,
                    style,
                    measure,
                    Para {
                        spans: vec![(u16::MAX, text.as_str(), SpanStyle::default())],
                        px: (f32::from(style.font_px) * 0.85).round() as u16,
                        left: em,
                        justify: false,
                        ..base
                    },
                    &mut out,
                );
                after_heading = true;
            }
            Block::Image { .. } => {
                out.push(Line {
                    content: LineContent::Image { block: bi as u32 },
                    height: body_line * 4,
                    ascent: 0,
                    space_before: base.space_before + body_line / 2,
                    keep_with_next: false,
                    start: offset,
                    end: offset + 1,
                    width: measure,
                    justified: false,
                });
                extra_space = body_line / 2;
                after_heading = true;
            }
            Block::Rule => {
                out.push(Line {
                    content: LineContent::Rule,
                    height: body_line,
                    ascent: 0,
                    space_before: base.space_before,
                    keep_with_next: false,
                    start: offset,
                    end: offset + 1,
                    width: measure,
                    justified: false,
                });
                after_heading = true;
            }
        }
    }
    out
}

/// Height available for lines (between the top margin and the footer).
pub fn content_height(size: Size, style: &Style) -> i32 {
    size.h as i32 - style.margins.top as i32 - style.margins.bottom as i32
}

/// Paginate a whole book.
pub fn paginate(fonts: &mut Fonts, style: &Style, size: Size, book: &Book) -> Pagination {
    let measure = (size.w - style.margins.left - style.margins.right) as f32;
    let max_h = content_height(size, style);
    let mut pages = Vec::new();
    for (ci, chapter) in book.chapters.iter().enumerate() {
        let lines = layout_chapter(fonts, style, measure, chapter);
        let chapter_len = chapter.char_len();
        let first_page = pages.len();
        let mut page = Page {
            chapter: ci as u32,
            start: 0,
            end: chapter_len,
            lines: Vec::new(),
        };
        let mut y = 0;
        let mut it = lines.into_iter().peekable();
        while let Some(line) = it.next() {
            let sb = if page.lines.is_empty() {
                0
            } else {
                line.space_before
            };
            let mut need = sb + line.height;
            if line.keep_with_next {
                if let Some(n) = it.peek() {
                    need += n.space_before + n.height;
                }
            }
            if !page.lines.is_empty() && y + need > max_h {
                let next = Page {
                    chapter: ci as u32,
                    start: line.start,
                    end: chapter_len,
                    lines: Vec::new(),
                };
                pages.push(std::mem::replace(&mut page, next));
                y = 0;
            }
            let sb = if page.lines.is_empty() {
                0
            } else {
                line.space_before
            };
            y += sb;
            let h = line.height;
            page.lines.push(PlacedLine { y, line });
            y += h;
        }
        pages.push(page);
        // Each page ends where the next one starts.
        for k in first_page..pages.len().saturating_sub(1) {
            pages[k].end = pages[k + 1].start;
        }
        if let Some(p) = pages.get_mut(first_page) {
            p.start = 0;
        }
    }
    Pagination {
        size,
        style: *style,
        pages,
    }
}
