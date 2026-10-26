//! Rasterising a laid-out page into an 8-bit grayscale image (255 = white paper, 0 = black ink).
use crate::font::{Face, Fonts};
use crate::layout::{span_text, LineContent, Page, Pagination, RunSrc};
use ink_epub::{Block, Book, ListMarker};
use ink_kindle_hal::{GrayImage, Rect};

/// Draw `text` with its pen starting at `x` on `baseline`; returns the pen position after it.
pub fn draw_text(
    img: &mut GrayImage,
    fonts: &mut Fonts,
    face: Face,
    px: u16,
    x: f32,
    baseline: i32,
    text: &str,
) -> f32 {
    let fake_bold = fonts.synthetic_bold(face);
    let mut pen = x;
    let mut prev = None;
    for c in text.chars() {
        if let Some(p) = prev {
            pen += fonts.kern(face, px, p, c);
        }
        let adv = fonts.advance(face, px, c);
        let g = fonts.glyph(face, px, c);
        if g.width > 0 {
            let x0 = (pen + g.xmin as f32).round() as i32;
            let y0 = baseline - g.height as i32 - g.ymin;
            for row in 0..g.height {
                let line = &g.bitmap[row * g.width..(row + 1) * g.width];
                for (col, &cov) in line.iter().enumerate() {
                    if cov > 0 {
                        img.darken(x0 + col as i32, y0 + row as i32, cov);
                        if fake_bold {
                            img.darken(x0 + col as i32 + 1, y0 + row as i32, cov);
                        }
                    }
                }
            }
        }
        pen += adv;
        prev = Some(c);
    }
    pen
}

/// Truncate `text` with an ellipsis so it fits in `max` pixels.
pub fn fit_text(fonts: &mut Fonts, face: Face, px: u16, text: &str, max: f32) -> String {
    if fonts.measure(face, px, text) <= max {
        return text.to_owned();
    }
    let ell = fonts.measure(face, px, "\u{2026}");
    let mut out = String::new();
    let mut w = 0.0;
    for c in text.chars() {
        let a = fonts.advance(face, px, c);
        if w + a + ell > max {
            break;
        }
        w += a;
        out.push(c);
    }
    out.trim_end().to_owned() + "\u{2026}"
}

fn draw_rect_outline(img: &mut GrayImage, r: Rect, v: u8, t: u32) {
    img.fill_rect(Rect::new(r.x, r.y, r.w, t), v);
    img.fill_rect(Rect::new(r.x, r.bottom().saturating_sub(t), r.w, t), v);
    img.fill_rect(Rect::new(r.x, r.y, t, r.h), v);
    img.fill_rect(Rect::new(r.right().saturating_sub(t), r.y, t, r.h), v);
}

/// Render page `index` of `pag` (with footer) into `img`, which must be `pag.size`.
pub fn render_page(
    img: &mut GrayImage,
    fonts: &mut Fonts,
    book: &Book,
    pag: &Pagination,
    index: usize,
) {
    img.fill(0xff);
    let style = &pag.style;
    let Some(page) = pag.pages.get(index) else {
        return;
    };
    let left = style.margins.left as f32;
    let top = style.margins.top as i32;
    let chapter = &book.chapters[page.chapter as usize];
    for pl in &page.lines {
        let y = top + pl.y;
        let line = &pl.line;
        match &line.content {
            LineContent::Text(runs) => {
                let baseline = y + line.ascent;
                for run in runs {
                    match run.src {
                        RunSrc::Text {
                            block,
                            span,
                            start,
                            end,
                        } => {
                            let t = &span_text(chapter, block, span)[start as usize..end as usize];
                            draw_text(img, fonts, run.face, run.px, left + run.x, baseline, t);
                        }
                        RunSrc::Marker(m) => {
                            let t = match m {
                                ListMarker::Bullet => "\u{2022}".to_owned(),
                                ListMarker::Number(n) => format!("{n}."),
                            };
                            draw_text(img, fonts, run.face, run.px, left + run.x, baseline, &t);
                        }
                    }
                }
            }
            LineContent::Image { block } => {
                let w = pag.content_width() as u32;
                let bw = w * 2 / 3;
                let r = Rect::new(
                    style.margins.left + (w - bw) / 2,
                    y.max(0) as u32,
                    bw,
                    line.height as u32,
                );
                img.fill_rect(r, 0xee);
                draw_rect_outline(img, r, 0x88, 2);
                let alt = match &chapter.blocks[*block as usize] {
                    Block::Image { alt, src } if alt.is_empty() => {
                        src.rsplit('/').next().unwrap_or("").to_owned()
                    }
                    Block::Image { alt, .. } => alt.clone(),
                    _ => String::new(),
                };
                let px = style.footer_px;
                let label = fit_text(
                    fonts,
                    Face::Italic,
                    px,
                    &format!("[image] {alt}"),
                    bw as f32 - 24.0,
                );
                let lw = fonts.measure(Face::Italic, px, &label);
                draw_text(
                    img,
                    fonts,
                    Face::Italic,
                    px,
                    r.x as f32 + (bw as f32 - lw) / 2.0,
                    y + line.height / 2 + i32::from(px) / 3,
                    &label,
                );
            }
            LineContent::Rule => {
                let w = pag.content_width() as u32;
                img.fill_rect(
                    Rect::new(
                        style.margins.left + w / 3,
                        (y + line.height / 2).max(0) as u32,
                        w / 3,
                        2,
                    ),
                    0x80,
                );
            }
        }
    }
    draw_footer(img, fonts, book, pag, page, index);
}

fn draw_footer(
    img: &mut GrayImage,
    fonts: &mut Fonts,
    book: &Book,
    pag: &Pagination,
    page: &Page,
    index: usize,
) {
    let style = &pag.style;
    let px = style.footer_px;
    let baseline = pag.size.h as i32 - (style.margins.bottom as i32 / 2) + i32::from(px) / 3;
    let right_edge = (pag.size.w - style.margins.right) as f32;
    let num = format!("{} / {}", index + 1, pag.pages.len());
    let nw = fonts.measure(Face::Regular, px, &num);
    draw_text(
        img,
        fonts,
        Face::Regular,
        px,
        right_edge - nw,
        baseline,
        &num,
    );
    let title = book.chapters[page.chapter as usize]
        .title
        .clone()
        .or_else(|| book.title.clone())
        .unwrap_or_default();
    if !title.is_empty() {
        let max = pag.content_width() * 0.6;
        let t = fit_text(fonts, Face::Italic, px, &title, max);
        draw_text(
            img,
            fonts,
            Face::Italic,
            px,
            style.margins.left as f32,
            baseline,
            &t,
        );
    }
}
