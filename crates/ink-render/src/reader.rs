//! The reading loop on top of a [`Display`]: page turns with dirty-region partial updates, font changes
//! with a full refresh, and the reading anchor kept across re-pagination.
use crate::dirty;
use crate::font::Fonts;
use crate::layout::{paginate, Anchor, Pagination, Style};
use crate::render::render_page;
use crate::Result;
use ink_epub::Book;
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
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

impl<D: Display> Reader<D> {
    /// Paginate the book and show the first page with a full refresh.
    pub fn open(
        book: Book,
        mut fonts: Fonts,
        style: Style,
        display: D,
    ) -> Result<(Self, StepReport)> {
        let size = display.size();
        let t = Instant::now();
        let pagination = paginate(&mut fonts, &style, size, &book);
        let layout_ms = ms(t);
        let mut r = Self {
            book,
            fonts,
            style,
            pagination,
            page: 0,
            anchor: Anchor::default(),
            display,
            front: GrayImage::new(size, 0xff),
            back: GrayImage::new(size, 0xff),
            turn_mode: WaveformMode::Gl16,
            max_rects: 4,
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

    fn report(&self, updates: Vec<UpdateRecord>, render_ms: f64, update_ms: f64) -> StepReport {
        StepReport {
            page: self.page,
            pages: self.page_count(),
            anchor: self.current_anchor(),
            updates,
            layout_ms: 0.0,
            render_ms,
            update_ms,
        }
    }

    fn show(&mut self, full: bool) -> Result<StepReport> {
        let t = Instant::now();
        render_page(
            &mut self.back,
            &mut self.fonts,
            &self.book,
            &self.pagination,
            self.page,
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
        Ok(self.report(updates, render_ms, update_ms))
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
        self.pagination = paginate(
            &mut self.fonts,
            &self.style,
            self.display.size(),
            &self.book,
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
