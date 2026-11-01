//! The reading loop on top of a [`Display`]: page turns with dirty-region partial updates, font changes
//! with a full refresh, and the reading anchor kept across re-pagination.
use crate::dirty;
use crate::font::Fonts;
use crate::images::{ImageRequest, Images, NoImages, PageImages};
use crate::layout::{paginate_with, Anchor, LineContent, Pagination, Style};
use crate::render::render_page_with;
use crate::Result;
use ink_epub::{Block, Book};
use ink_kindle_hal::{Direction, Display, Gesture, GrayImage, UpdateRecord, WaveformMode};
use std::time::Instant;

/// What one navigation step did.
#[derive(Debug, Clone, PartialEq)]
pub struct StepReport {
    pub page: usize,
    pub pages: usize,
    pub anchor: Anchor,
    /// Updates actually issued (after the refresh policy).
    pub updates: Vec<UpdateRecord>,
    /// Re-pagination time (font changes / open), ms.
    pub layout_ms: f64,
    /// Rasterising the page, ms.
    pub render_ms: f64,
    /// Diff + framebuffer copy + update calls, ms.
    pub update_ms: f64,
    /// How the shown page's pictures came out (default when nothing was rendered).
    pub images: PageImages,
}

pub struct Reader<D: Display> {
    pub book: Book,
    pub fonts: Fonts,
    style: Style,
    pagination: Pagination,
    page: usize,
    /// Reading position. Set to the page start by navigation and deliberately *not* moved by style
    /// changes, so font+ then font- returns to exactly the same page instead of drifting backwards.
    anchor: Anchor,
    display: D,
    /// What the panel shows (mirror of the framebuffer).
    front: GrayImage,
    back: GrayImage,
    /// Waveform for page turns.
    pub turn_mode: WaveformMode,
    /// Most partial updates per page turn.
    pub max_rects: usize,
    /// The book's pictures (sizes for the layout, pixels, prefetch).
    images: Box<dyn Images>,
    /// How the pictures of the page on the panel came out.
    last_images: PageImages,
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

impl<D: Display> Reader<D> {
    /// Paginate the book and show the first page with a full refresh.
    pub fn open(book: Book, fonts: Fonts, style: Style, display: D) -> Result<(Self, StepReport)> {
        Self::open_at(book, fonts, style, display, None)
    }

    /// Paginate the book and show the page holding `anchor` (a saved reading position; `None` = the
    /// start) with a full refresh. An anchor past the end of the book lands on the last page.
    pub fn open_at(
        book: Book,
        fonts: Fonts,
        style: Style,
        display: D,
        anchor: Option<Anchor>,
    ) -> Result<(Self, StepReport)> {
        Self::open_at_with(book, fonts, style, display, anchor, Box::new(NoImages))
    }

    /// [`Reader::open_at`] with the book's pictures supplied by `images`.
    pub fn open_at_with(
        book: Book,
        mut fonts: Fonts,
        style: Style,
        display: D,
        anchor: Option<Anchor>,
        mut images: Box<dyn Images>,
    ) -> Result<(Self, StepReport)> {
        let size = display.size();
        let t = Instant::now();
        let pagination = paginate_with(&mut fonts, &style, size, &book, images.as_mut());
        let layout_ms = ms(t);
        let anchor = anchor.unwrap_or_default();
        let page = pagination.page_for(anchor);
        let mut r = Self {
            book,
            fonts,
            style,
            pagination,
            page,
            anchor,
            display,
            front: GrayImage::new(size, 0xff),
            back: GrayImage::new(size, 0xff),
            turn_mode: WaveformMode::Gl16,
            max_rects: 4,
            images,
            last_images: PageImages::default(),
        };
        let mut rep = r.show(true)?;
        rep.layout_ms = layout_ms;
        Ok((r, rep))
    }

    pub fn page(&self) -> usize {
        self.page
    }
    pub fn page_count(&self) -> usize {
        self.pagination.pages.len()
    }
    pub fn pagination(&self) -> &Pagination {
        &self.pagination
    }
    pub fn style(&self) -> &Style {
        &self.style
    }
    pub fn current_anchor(&self) -> Anchor {
        self.anchor
    }
    pub fn display(&self) -> &D {
        &self.display
    }
    pub fn display_mut(&mut self) -> &mut D {
        &mut self.display
    }
    /// The image currently on the panel.
    pub fn screen(&self) -> &GrayImage {
        &self.front
    }
    /// The book's pictures source.
    pub fn images_mut(&mut self) -> &mut dyn Images {
        self.images.as_mut()
    }
    /// Some picture of the page on the panel was still being prepared: call [`Reader::redraw`] once it is.
    pub fn pending_images(&self) -> bool {
        self.last_images.pending > 0
    }

    /// The pictures of page `page` with the boxes they are drawn in (empty past the end).
    pub fn page_images(&self, page: usize) -> Vec<ImageRequest> {
        let Some(p) = self.pagination.pages.get(page) else {
            return Vec::new();
        };
        let chapter = &self.book.chapters[p.chapter as usize];
        p.lines
            .iter()
            .filter_map(|pl| match pl.line.content {
                LineContent::Image { block, w, h } => match &chapter.blocks[block as usize] {
                    Block::Image { src, .. } => Some(ImageRequest {
                        chapter: p.chapter,
                        block,
                        src: src.clone(),
                        w,
                        h,
                    }),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    /// Pictures of the pages likely shown next (`page+1`, `page+2`, `page-1`), each once.
    fn wanted_images(&self) -> Vec<ImageRequest> {
        let mut pages = vec![self.page + 1, self.page + 2];
        if let Some(p) = self.page.checked_sub(1) {
            pages.push(p);
        }
        let mut out: Vec<ImageRequest> = Vec::new();
        for p in pages {
            for r in self.page_images(p) {
                if !out.iter().any(|o| o.chapter == r.chapter && o.src == r.src) {
                    out.push(r);
                }
            }
        }
        out
    }

    fn report(&self, updates: Vec<UpdateRecord>, render_ms: f64, update_ms: f64) -> StepReport {
        StepReport {
            page: self.page,
            pages: self.page_count(),
            anchor: self.current_anchor(),
            updates,
            layout_ms: 0.0,
            render_ms,
            update_ms,
            images: self.last_images,
        }
    }

    fn show(&mut self, full: bool) -> Result<StepReport> {
        let t = Instant::now();
        self.last_images = render_page_with(
            &mut self.back,
            &mut self.fonts,
            &self.book,
            &self.pagination,
            self.page,
            self.images.as_mut(),
        );
        let render_ms = ms(t);
        let t = Instant::now();
        let mut updates = Vec::new();
        if full {
            self.display
                .frame_mut()
                .blit(&self.back, self.back.size.rect());
            updates.push(self.display.full_refresh()?);
        } else {
            let rects = dirty::diff(&self.front, &self.back, self.max_rects);
            {
                let mut frame = self.display.frame_mut();
                for r in &rects {
                    frame.blit(&self.back, *r);
                }
            }
            if !rects.is_empty() {
                updates = self.display.update_regions(&rects, self.turn_mode)?;
            }
        }
        std::mem::swap(&mut self.front, &mut self.back);
        let update_ms = ms(t);
        let wanted = self.wanted_images();
        self.images.prefetch(&wanted);
        Ok(self.report(updates, render_ms, update_ms))
    }

    /// Re-render the current page and update only the changed regions, keeping page and anchor. For a
    /// picture that has finished decoding since the page was shown.
    pub fn redraw(&mut self) -> Result<StepReport> {
        self.show(false)
    }

    /// Redraw the current page with a full-screen GC16 flash, keeping page and anchor. Used after something
    /// else (a sleep screen) took over the panel: the page is re-rendered, not diffed against a stale mirror.
    pub fn redraw_full(&mut self) -> Result<StepReport> {
        self.show(true)
    }

    /// The fonts and the display at once (to draw a system screen with the reader's fonts).
    pub fn fonts_and_display(&mut self) -> (&mut Fonts, &mut D) {
        (&mut self.fonts, &mut self.display)
    }

    /// Go to `page` with a partial (dirty-region) update.
    pub fn goto_page(&mut self, page: usize) -> Result<StepReport> {
        self.page = page.min(self.page_count().saturating_sub(1));
        self.anchor = self.pagination.anchor_of(self.page);
        self.show(false)
    }

    /// Jump to a position (e.g. a TOC entry: `Anchor { chapter, offset: 0 }`).
    pub fn goto_anchor(&mut self, a: Anchor) -> Result<StepReport> {
        self.page = self.pagination.page_for(a);
        self.anchor = a;
        self.show(false)
    }

    pub fn next_page(&mut self) -> Result<Option<StepReport>> {
        if self.page + 1 >= self.page_count() {
            return Ok(None);
        }
        self.goto_page(self.page + 1).map(Some)
    }

    pub fn prev_page(&mut self) -> Result<Option<StepReport>> {
        if self.page == 0 {
            return Ok(None);
        }
        self.goto_page(self.page - 1).map(Some)
    }

    /// Re-paginate with a new style, keep the reading anchor, show with a full refresh.
    pub fn set_style(&mut self, style: Style) -> Result<StepReport> {
        let anchor = self.anchor;
        self.style = style;
        let t = Instant::now();
        self.pagination = paginate_with(
            &mut self.fonts,
            &self.style,
            self.display.size(),
            &self.book,
            self.images.as_mut(),
        );
        let layout_ms = ms(t);
        self.page = self.pagination.page_for(anchor);
        let mut rep = self.show(true)?;
        rep.layout_ms = layout_ms;
        Ok(rep)
    }

    pub fn set_font_px(&mut self, px: u16) -> Result<StepReport> {
        let style = Style {
            font_px: px,
            ..self.style
        };
        self.set_style(style)
    }

    /// `delta` steps through [`crate::layout::FONT_SIZES`].
    pub fn step_font(&mut self, delta: i32) -> Result<StepReport> {
        self.set_font_px(self.style.step_font(delta))
    }

    /// Kindle-style navigation: tap the left third or swipe right for back, tap elsewhere or swipe left
    /// for forward. Long-press and vertical swipes are not bound yet.
    pub fn handle_gesture(&mut self, g: Gesture) -> Result<Option<StepReport>> {
        let w = self.display.size().w as i32;
        match g {
            Gesture::Tap { pos } if pos.x < w / 3 => self.prev_page(),
            Gesture::Tap { .. } => self.next_page(),
            Gesture::Swipe {
                dir: Direction::Left,
                ..
            } => self.next_page(),
            Gesture::Swipe {
                dir: Direction::Right,
                ..
            } => self.prev_page(),
            _ => Ok(None),
        }
    }
}
