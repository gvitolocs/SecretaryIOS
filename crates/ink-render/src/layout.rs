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
use crate::images::{ImageSize, Images, NoImages};
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
    /// A picture drawn in a `w × h` pixel box, centred in the column.
    Image {
        block: u32,
        w: u32,
        h: u32,
    },
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
    /// Only some chapters are laid out ([`crate::Reader::set_style_quick`]): page numbers are not
    /// the book's, and the footer leaves them out.
    pub partial: bool,
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

/// Box (pixels) a picture of intrinsic `size` is drawn in, aspect ratio kept. A `full_page` picture (cover,
/// image-only chapter) fills `measure × max_h`, upscaled if needed; other pictures are shown at twice their
/// intrinsic size (they are made for ~150 dpi screens, the panel is 300 dpi), shrunk to fit `measure × max_h`.
/// A zero-sized picture gives `(0, 0)`: unknown, laid out as the placeholder.
pub fn image_box(size: ImageSize, measure: f32, max_h: i32, full_page: bool) -> (u32, u32) {
    if size.w == 0 || size.h == 0 {
        return (0, 0);
    }
    let max_w = f64::from(measure).floor().max(1.0);
    let max_h = f64::from(max_h.max(1));
    let (w, h) = (f64::from(size.w), f64::from(size.h));
    let mut s = (max_w / w).min(max_h / h);
    if !full_page {
        s = s.min(2.0);
    }
    let w = (w * s).round().clamp(1.0, max_w) as u32;
    let h = (h * s).round().clamp(1.0, max_h) as u32;
    (w, h)
}

/// A chapter whose only content is one picture (blank text and rules aside): the picture fills the page.
fn full_page_picture(chapter: &Chapter) -> bool {
    let mut images = 0;
    for b in &chapter.blocks {
        match b {
            Block::Image { .. } => images += 1,
            Block::Rule => {}
            b if b.text().trim().is_empty() => {}
            _ => return false,
        }
    }
    images == 1
}

/// Lay out a chapter into lines (no pagination); pictures are placeholders.
pub fn layout_chapter(
    fonts: &mut Fonts,
    style: &Style,
    measure: f32,
    chapter: &Chapter,
) -> Vec<Line> {
    layout_chapter_with(fonts, style, measure, 0, chapter, 0, &mut NoImages)
}

/// Lay out chapter `chapter_index` into lines (no pagination), sizing pictures with `images`. `max_h` is the
/// page's content height (pictures are never taller).
pub fn layout_chapter_with(
    fonts: &mut Fonts,
    style: &Style,
    measure: f32,
    max_h: i32,
    chapter: &Chapter,
    chapter_index: u32,
    images: &mut dyn Images,
) -> Vec<Line> {
    let mut out = Vec::new();
    let mut lay = ChapterLayout::new(style, measure, max_h, chapter, chapter_index);
    for bi in 0..chapter.blocks.len() {
        lay.block(fonts, images, bi, &mut out);
    }
    out
}

/// Which spacing group a block is in (blank space where body text, quotes, lists and preformatted
/// text meet).
fn group_of(block: &Block) -> Option<u8> {
    match block {
        Block::Paragraph { quote: false, .. } => Some(0u8),
        Block::Paragraph { quote: true, .. } => Some(1),
        Block::ListItem { .. } => Some(2),
        Block::Preformatted { .. } => Some(3),
        _ => None,
    }
}

/// Whether a block gives at least one line (a text block of blanks gives none).
fn has_lines(block: &Block) -> bool {
    match block {
        Block::Image { .. } | Block::Rule => true,
        Block::Preformatted { text } => text.chars().any(|c| !c.is_whitespace() || c == '\u{a0}'),
        b => b
            .spans()
            .iter()
            .any(|s| s.text.chars().any(|c| !c.is_whitespace() || c == '\u{a0}')),
    }
}

/// What laying out a block takes from the blocks before it. It follows from the block just before
/// (and whether any block before gave a line), so a chapter can be laid out from any block on and give
/// exactly the lines a layout from its start gives ([`ChapterLayout::start_at_block`]).
#[derive(Debug, Clone, Copy)]
struct Flow {
    after_heading: bool,
    extra_space: i32,
    prev_group: Option<u8>,
    any_lines: bool,
}

/// One chapter being laid out a block at a time.
struct ChapterLayout<'a> {
    style: &'a Style,
    measure: f32,
    max_h: i32,
    chapter: &'a Chapter,
    index: u32,
    full_page: bool,
    body_line: i32,
    em: f32,
    para_gap: i32,
    starts: Vec<u32>,
    flow: Flow,
}

impl<'a> ChapterLayout<'a> {
    fn new(style: &'a Style, measure: f32, max_h: i32, chapter: &'a Chapter, index: u32) -> Self {
        let body_line = (f32::from(style.font_px) * style.line_height).round() as i32;
        Self {
            style,
            measure,
            max_h,
            chapter,
            index,
            full_page: full_page_picture(chapter),
            body_line,
            em: f32::from(style.font_px),
            para_gap: (style.paragraph_spacing * body_line as f32).round() as i32,
            starts: chapter.block_starts(),
            // The chapter start counts as "after a heading": no indent.
            flow: Flow {
                after_heading: true,
                extra_space: 0,
                prev_group: None,
                any_lines: false,
            },
        }
    }

    /// Ready to lay out block `bi` without laying out the blocks before it.
    fn start_at_block(&mut self, images: &mut dyn Images, bi: usize) {
        let Some(prev) = bi.checked_sub(1).map(|k| &self.chapter.blocks[k]) else {
            return;
        };
        let extra_space = match prev {
            Block::Heading { .. } => self.body_line / 2,
            Block::Image { src, .. } => {
                let known = images
                    .size(self.index, src)
                    .map(|sz| image_box(sz, self.measure, self.max_h, self.full_page))
                    .is_some_and(|(w, _)| w > 0);
                if self.full_page && known {
                    0
                } else {
                    self.body_line / 2
                }
            }
            _ => 0,
        };
        self.flow = Flow {
            after_heading: !matches!(prev, Block::Paragraph { .. } | Block::ListItem { .. }),
            extra_space,
            prev_group: group_of(prev),
            any_lines: self.chapter.blocks[..bi].iter().any(has_lines),
        };
    }

    /// Lay out block `bi` into `out`.
    fn block(
        &mut self,
        fonts: &mut Fonts,
        images: &mut dyn Images,
        bi: usize,
        out: &mut Vec<Line>,
    ) {
        let (style, measure, chapter) = (self.style, self.measure, self.chapter);
        let (body_line, em) = (self.body_line, self.em);
        let block = &chapter.blocks[bi];
        let offset = self.starts[bi];
        let before = out.len();
        let f = &mut self.flow;
        let group = group_of(block);
        if group.is_some() && f.prev_group.is_some() && group != f.prev_group {
            f.extra_space += body_line / 3;
        }
        f.prev_group = group;
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
            space_before: self.para_gap + f.extra_space,
            keep_with_next: false,
        };
        f.extra_space = 0;
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
                        space_before: if f.any_lines { body_line } else { 0 },
                        keep_with_next: true,
                        ..base
                    },
                    out,
                );
                f.extra_space = body_line / 2;
                f.after_heading = true;
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
                        first_indent: if f.after_heading || *quote {
                            0.0
                        } else {
                            style.indent_em * em
                        },
                        ..base
                    },
                    out,
                );
                f.after_heading = false;
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
                    out,
                );
                f.after_heading = false;
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
                    out,
                );
                f.after_heading = true;
            }
            Block::Image { src, .. } => {
                let known = images
                    .size(self.index, src)
                    .map(|sz| image_box(sz, measure, self.max_h, self.full_page))
                    .filter(|&(w, _)| w > 0);
                let line = match known {
                    Some((w, h)) => Line {
                        content: LineContent::Image {
                            block: bi as u32,
                            w,
                            h,
                        },
                        height: h as i32,
                        ascent: 0,
                        space_before: if self.full_page {
                            0
                        } else {
                            base.space_before + body_line / 2
                        },
                        keep_with_next: false,
                        start: offset,
                        end: offset + 1,
                        width: w as f32,
                        justified: false,
                    },
                    // Placeholder box: two thirds of the column, four lines high.
                    None => Line {
                        content: LineContent::Image {
                            block: bi as u32,
                            w: measure as u32 * 2 / 3,
                            h: (body_line * 4) as u32,
                        },
                        height: body_line * 4,
                        ascent: 0,
                        space_before: base.space_before + body_line / 2,
                        keep_with_next: false,
                        start: offset,
                        end: offset + 1,
                        width: measure,
                        justified: false,
                    },
                };
                f.extra_space = if self.full_page && known.is_some() {
                    0
                } else {
                    body_line / 2
                };
                out.push(line);
                f.after_heading = true;
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
                f.after_heading = true;
            }
        }
        f.any_lines |= out.len() > before;
    }
}

/// Height available for lines (between the top margin and the footer).
pub fn content_height(size: Size, style: &Style) -> i32 {
    size.h as i32 - style.margins.top as i32 - style.margins.bottom as i32
}

/// Paginate a whole book; pictures are placeholders.
pub fn paginate(fonts: &mut Fonts, style: &Style, size: Size, book: &Book) -> Pagination {
    paginate_with(fonts, style, size, book, &mut NoImages)
}

/// Paginate a whole book, sizing pictures with `images`. Anchors mean the same with and without pictures
/// (a picture covers one offset); only the page boundaries move.
pub fn paginate_with(
    fonts: &mut Fonts,
    style: &Style,
    size: Size,
    book: &Book,
    images: &mut dyn Images,
) -> Pagination {
    let mut p = Paginator::new(*style, size, None);
    p.step(fonts, book, images, None);
    p.into_pagination()
}

/// The pages of chapter `ci` alone. Chapters are laid out independently, so these are exactly that
/// chapter's pages in [`paginate_with`]'s result (which is every chapter's, in order).
pub fn paginate_chapter_with(
    fonts: &mut Fonts,
    style: &Style,
    size: Size,
    book: &Book,
    ci: usize,
    images: &mut dyn Images,
) -> Vec<Page> {
    let measure = (size.w - style.margins.left - style.margins.right) as f32;
    let max_h = content_height(size, style);
    let chapter = &book.chapters[ci];
    let lines = layout_chapter_with(fonts, style, measure, max_h, chapter, ci as u32, images);
    let mut placer = Placer::new(ci as u32, chapter.char_len(), max_h, 0, None);
    let mut it = lines.into_iter().peekable();
    while let Some(line) = it.next() {
        placer.place(line, it.peek());
    }
    placer.finish(true)
}

/// Fills pages with lines, one chapter at a time: a page ends when the next line (with the one it keeps
/// with) does not fit, or at the line starting at `break_at.0` (the shown page of a quick style change,
/// [`pages_at`]; that page starts at `break_at.1`). The space above a line is dropped at the top of a
/// page.
struct Placer {
    chapter: u32,
    chapter_len: u32,
    max_h: i32,
    break_at: Option<(u32, u32)>,
    page: Page,
    y: i32,
    pages: Vec<Page>,
}

impl Placer {
    fn new(
        chapter: u32,
        chapter_len: u32,
        max_h: i32,
        start: u32,
        break_at: Option<(u32, u32)>,
    ) -> Self {
        Self {
            chapter,
            chapter_len,
            max_h,
            break_at,
            page: Page {
                chapter,
                start,
                end: chapter_len,
                lines: Vec::new(),
            },
            y: 0,
            pages: Vec::new(),
        }
    }

    /// Place `line`; `next` is the line after it (a heading keeps with it).
    fn place(&mut self, line: Line, next: Option<&Line>) {
        let sb = if self.page.lines.is_empty() {
            0
        } else {
            line.space_before
        };
        let mut need = sb + line.height;
        if line.keep_with_next {
            if let Some(n) = next {
                need += n.space_before + n.height;
            }
        }
        let forced = self.break_at.filter(|&(at, _)| at == line.start);
        if !self.page.lines.is_empty() && (self.y + need > self.max_h || forced.is_some()) {
            let next_page = Page {
                chapter: self.chapter,
                start: forced.map_or(line.start, |(_, page_start)| page_start),
                end: self.chapter_len,
                lines: Vec::new(),
            };
            self.pages
                .push(std::mem::replace(&mut self.page, next_page));
            self.y = 0;
        }
        let sb = if self.page.lines.is_empty() {
            0
        } else {
            line.space_before
        };
        self.y += sb;
        let h = line.height;
        self.page.lines.push(PlacedLine { y: self.y, line });
        self.y += h;
    }

    /// The pages closed so far.
    fn closed(&self) -> usize {
        self.pages.len()
    }

    /// The chapter's pages (with the one being filled); each ends where the next starts, and the first
    /// starts at 0 when `chapter_start`.
    fn finish(mut self, chapter_start: bool) -> Vec<Page> {
        self.pages.push(self.page);
        let mut pages = self.pages;
        for k in 0..pages.len().saturating_sub(1) {
            pages[k].end = pages[k + 1].start;
        }
        if chapter_start {
            if let Some(p) = pages.first_mut() {
                p.start = 0;
            }
        }
        pages
    }

    /// The first `n` closed pages, each ending where the next starts.
    fn first_closed(self, n: usize) -> Vec<Page> {
        let mut pages = self.pages;
        pages.push(self.page);
        for k in 0..pages.len().saturating_sub(1) {
            pages[k].end = pages[k + 1].start;
        }
        pages.truncate(n);
        pages
    }
}

/// The page on screen after a style change, KindleOS-style: it starts at the line holding `anchor` (or
/// the first line after it), and `n` pages from there, laid out from the anchor's paragraph only, not
/// from the chapter's start. These are exactly the pages a [`Paginator`] given [`break_of`] the first
/// one gives; the pages before it are left to the paginator (the one just before may be short). Within
/// the anchor's chapter only (fewer pages at its end).
pub fn pages_at(
    fonts: &mut Fonts,
    style: &Style,
    size: Size,
    book: &Book,
    anchor: Anchor,
    images: &mut dyn Images,
    n: usize,
) -> Vec<Page> {
    let measure = (size.w - style.margins.left - style.margins.right) as f32;
    let max_h = content_height(size, style);
    let ci = (anchor.chapter as usize).min(book.chapters.len().saturating_sub(1));
    let Some(chapter) = book.chapters.get(ci) else {
        return Vec::new();
    };
    let starts = chapter.block_starts();
    let nblocks = chapter.blocks.len();
    // The block holding the anchor (an offset past the end: the last block).
    let first = starts[..nblocks]
        .partition_point(|&o| o <= anchor.offset)
        .saturating_sub(1);
    let mut lay = ChapterLayout::new(style, measure, max_h, chapter, ci as u32);
    lay.start_at_block(images, first);
    let lines_before = lay.flow.any_lines;
    let mut lines: Vec<Line> = Vec::new();
    let mut bi = first;
    // Lay out blocks until the line holding the anchor is there.
    let mut at = None;
    while at.is_none() && bi < nblocks {
        lay.block(fonts, images, bi, &mut lines);
        bi += 1;
        at = lines.iter().position(|l| l.end > anchor.offset);
    }
    // Past the end: the chapter's last line (laid out from its block).
    let at = match at {
        Some(i) => i,
        None if lines.is_empty() => {
            return paginate_chapter_with(fonts, style, size, book, ci, images)
                .into_iter()
                .rev()
                .take(1)
                .collect();
        }
        None => lines.len() - 1,
    };
    let chapter_start = !lines_before && at == 0;
    // The page holds the anchor even when it falls in the blank before the line.
    let start = if chapter_start {
        0
    } else {
        lines[at].start.min(anchor.offset)
    };
    let mut placer = Placer::new(ci as u32, chapter.char_len(), max_h, start, None);
    let mut i = at;
    loop {
        // One line ahead for keep-with-next, laying out more blocks as they are needed.
        while i + 1 >= lines.len() && bi < nblocks {
            lay.block(fonts, images, bi, &mut lines);
            bi += 1;
        }
        if i >= lines.len() {
            return placer.finish(false).into_iter().take(n).collect();
        }
        let line = lines[i].clone();
        placer.place(line, lines.get(i + 1));
        if placer.closed() >= n {
            return placer.first_closed(n);
        }
        i += 1;
    }
}

/// Where [`pages_at`]'s first page breaks the chapter: (chapter, its first line's start, its start).
pub fn break_of(page: &Page) -> Option<(u32, u32, u32)> {
    page.lines
        .first()
        .map(|l| (page.chapter, l.line.start, page.start))
}

/// Paginates a book a little at a time ([`Paginator::step`] until a deadline), so the pages a quick
/// style change did not lay out are counted while the reader is idle, and a new style change simply
/// drops it. With `break_at` ([`break_of`]) a page starts at that line, as [`pages_at`] laid it out;
/// without, it is [`paginate_with`].
pub struct Paginator {
    style: Style,
    size: Size,
    measure: f32,
    max_h: i32,
    break_at: Option<(u32, u32, u32)>,
    /// The chapter being laid out, its next block and its lines not placed yet.
    chapter: usize,
    block: usize,
    flow: Option<Flow>,
    lines: std::collections::VecDeque<Line>,
    placer: Option<Placer>,
    pages: Vec<Page>,
}

impl Paginator {
    pub fn new(style: Style, size: Size, break_at: Option<(u32, u32, u32)>) -> Self {
        Self {
            style,
            size,
            measure: (size.w - style.margins.left - style.margins.right) as f32,
            max_h: content_height(size, &style),
            break_at,
            chapter: 0,
            block: 0,
            flow: None,
            lines: std::collections::VecDeque::new(),
            placer: None,
            pages: Vec::new(),
        }
    }

    /// Lay out until `deadline` (checked after each block) or the end of `book`; true when done.
    pub fn step(
        &mut self,
        fonts: &mut Fonts,
        book: &Book,
        images: &mut dyn Images,
        deadline: Option<std::time::Instant>,
    ) -> bool {
        while self.chapter < book.chapters.len() {
            let ci = self.chapter;
            let chapter = &book.chapters[ci];
            let style = self.style;
            let mut lay = ChapterLayout::new(&style, self.measure, self.max_h, chapter, ci as u32);
            if let Some(f) = self.flow {
                lay.flow = f;
            }
            let break_at = self
                .break_at
                .filter(|&(c, _, _)| c as usize == ci)
                .map(|(_, line, page)| (line, page));
            let max_h = self.max_h;
            let placer = self.placer.get_or_insert_with(|| {
                Placer::new(ci as u32, chapter.char_len(), max_h, 0, break_at)
            });
            while self.block < chapter.blocks.len() {
                let mut out = Vec::new();
                lay.block(fonts, images, self.block, &mut out);
                self.block += 1;
                self.lines.extend(out);
                // Keep one line back: it may keep with the next.
                while self.lines.len() > 1 {
                    let line = self.lines.pop_front().expect("two lines");
                    placer.place(line, self.lines.front());
                }
                if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                    self.flow = Some(lay.flow);
                    return false;
                }
            }
            while let Some(line) = self.lines.pop_front() {
                placer.place(line, self.lines.front());
            }
            let placer = self.placer.take().expect("the chapter's placer");
            self.pages.extend(placer.finish(true));
            self.chapter += 1;
            self.block = 0;
            self.flow = None;
        }
        true
    }

    /// Whether every chapter is laid out.
    pub fn done(&self, book: &Book) -> bool {
        self.chapter >= book.chapters.len()
    }

    /// The pages (call when [`Paginator::step`] returned true).
    pub fn into_pagination(self) -> Pagination {
        Pagination {
            size: self.size,
            style: self.style,
            pages: self.pages,
            partial: false,
        }
    }
}
