//! The reading loop on top of a [`Display`]: page turns with dirty-region partial updates, font changes
//! with a full refresh, and the reading anchor kept across re-pagination.
use crate::dirty;
use crate::font::Fonts;
use crate::images::{ImageRequest, Images, NoImages, PageImages};
use crate::layout::{
    break_of, pages_at, paginate_chapter_with, paginate_with, Anchor, BookMap, LineContent, PageNumbers,
    Pagination, Paginator, Style,
};
use crate::render::render_page_with;
use crate::Result;
use ink_epub::{Block, Book, Chapter};
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

/// A book's chapters, read when they are wanted, for a book too large to keep in memory or to read at once.
pub trait ChapterStore {
    fn chapter(&mut self, index: usize) -> Option<Chapter>;
}

/// How many chapters stay in memory, the one being read among them.
const LOADED_CHAPTERS: usize = 3;

/// The book as a window: one chapter laid out, and the rest read when reading gets there ([`Reader::open_windowed`]).
struct Window {
    store: Box<dyn ChapterStore>,
    map: Option<BookMap>,
    chapter: usize,
    /// The chapters in memory, the last used at the end.
    loaded: Vec<usize>,
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
    /// After [`Reader::set_style_quick`]: the rest of the book being paginated, and the time it took
    /// so far (ms).
    lazy: Option<(Paginator, f64)>,
    /// The page on the panel was drawn without its number (a partial layout).
    numbers_stale: bool,
    /// How long the last pagination of the rest took in all (ms).
    rest_ms: f64,
    window: Option<Window>,
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
            lazy: None,
            numbers_stale: false,
            rest_ms: 0.0,
            window: None,
        };
        let mut rep = r.show(true)?;
        rep.layout_ms = layout_ms;
        Ok((r, rep))
    }

    /// Opens a book of which `book` only has to say the chapters' titles and hrefs: the chapter holding `anchor` is read
    /// from `store` and laid out, and shown with a full refresh, whatever the size of the book. The others follow as reading
    /// gets there. With `map` the page numbers are the book's at once; without, the footer leaves them out, as
    /// [`Reader::set_style_quick`] does, until [`Reader::set_map`].
    #[allow(clippy::too_many_arguments)]
    pub fn open_windowed(
        book: Book,
        store: Box<dyn ChapterStore>,
        map: Option<BookMap>,
        fonts: Fonts,
        style: Style,
        display: D,
        anchor: Option<Anchor>,
        images: Box<dyn Images>,
    ) -> Result<(Self, StepReport)> {
        let size = display.size();
        let anchor = anchor.unwrap_or_default();
        let t = Instant::now();
        let mut r = Self {
            book,
            fonts,
            style,
            pagination: Pagination {
                size,
                style,
                pages: Vec::new(),
                partial: true,
                numbers: None,
            },
            page: 0,
            anchor,
            display,
            front: GrayImage::new(size, 0xff),
            back: GrayImage::new(size, 0xff),
            turn_mode: WaveformMode::Gl16,
            max_rects: 4,
            images,
            last_images: PageImages::default(),
            lazy: None,
            numbers_stale: false,
            rest_ms: 0.0,
            window: Some(Window {
                store,
                map,
                chapter: 0,
                loaded: Vec::new(),
            }),
        };
        let chapter = (anchor.chapter as usize).min(r.book.chapters.len().saturating_sub(1));
        r.lay_out_chapter(chapter);
        r.page = r.pagination.page_for(anchor);
        let layout_ms = ms(t);
        let mut rep = r.show(true)?;
        rep.layout_ms = layout_ms;
        Ok((r, rep))
    }

    /// The window's chapter store, to open the book again on another display. The reader is done with after this.
    pub fn take_store(&mut self) -> Option<Box<dyn ChapterStore>> {
        self.window.take().map(|w| w.store)
    }

    /// The page's place in what is laid out, as opposed to [`Reader::page`], its place in the book.
    pub fn window_index(&self) -> usize {
        self.page
    }

    /// Whether only a window of the book is laid out.
    pub fn is_windowed(&self) -> bool {
        self.window.is_some()
    }

    /// The book's page numbers, when they become known. Takes effect at the next drawing.
    pub fn set_map(&mut self, map: Option<BookMap>) {
        if let Some(window) = self.window.as_mut() {
            window.map = map;
            let chapter = window.chapter;
            self.pagination.numbers = self.numbers_of(chapter);
            self.pagination.partial = self.pagination.numbers.is_none();
        }
    }

    fn numbers_of(&self, chapter: usize) -> Option<PageNumbers> {
        let map = self.window.as_ref()?.map.as_ref()?;
        Some(PageNumbers {
            first: *map.first_page.get(chapter)?,
            total: map.total,
        })
    }

    /// Reads a chapter if it is not in memory, and lets go of the one not used for longest.
    fn load_chapter(&mut self, chapter: usize) {
        let Some(window) = self.window.as_mut() else {
            return;
        };
        window.loaded.retain(|&c| c != chapter);
        window.loaded.push(chapter);
        if self.book.chapters[chapter].blocks.is_empty() {
            if let Some(loaded) = window.store.chapter(chapter) {
                self.book.chapters[chapter].blocks = loaded.blocks;
            }
        }
        while window.loaded.len() > LOADED_CHAPTERS {
            let old = window.loaded.remove(0);
            self.book.chapters[old].blocks = Vec::new();
        }
    }

    /// Makes `chapter` the window.
    fn lay_out_chapter(&mut self, chapter: usize) {
        self.load_chapter(chapter);
        let size = self.display.size();
        let pages = paginate_chapter_with(
            &mut self.fonts,
            &self.style,
            size,
            &self.book,
            chapter,
            self.images.as_mut(),
        );
        if let Some(window) = self.window.as_mut() {
            window.chapter = chapter;
        }
        let numbers = self.numbers_of(chapter);
        self.pagination = Pagination {
            size,
            style: self.style,
            pages,
            partial: numbers.is_none(),
            numbers,
        };
    }

    /// The next chapter with pages from `chapter` on, going `forward` or back. The window stays as it is if there is none.
    fn move_window(&mut self, forward: bool) -> bool {
        let Some(mut chapter) = self.window.as_ref().map(|w| w.chapter) else {
            return false;
        };
        let last = self.book.chapters.len().saturating_sub(1);
        let current = chapter;
        loop {
            if (forward && chapter >= last) || (!forward && chapter == 0) {
                if chapter != current {
                    self.lay_out_chapter(current);
                }
                return false;
            }
            chapter = if forward { chapter + 1 } else { chapter - 1 };
            self.lay_out_chapter(chapter);
            if !self.pagination.pages.is_empty() {
                return true;
            }
        }
    }

    /// The page in the book, when the numbers are known; else in what is laid out.
    pub fn page(&self) -> usize {
        self.page + self.pagination.numbers.map_or(0, |n| n.first as usize)
    }
    pub fn page_count(&self) -> usize {
        self.pagination
            .numbers
            .map_or(self.pagination.pages.len(), |n| n.total as usize)
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
            page: self.page(),
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
        self.numbers_stale = self.pagination.partial;
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
        self.complete_layout();
        if let Some(map) = self.window.as_ref().and_then(|w| w.map.as_ref()) {
            let (chapter, index) = map.locate(page as u32);
            if self.window.as_ref().map(|w| w.chapter) != Some(chapter) {
                self.lay_out_chapter(chapter);
            }
            self.page = (index as usize).min(self.pagination.pages.len().saturating_sub(1));
            self.anchor = self.pagination.anchor_of(self.page);
            return self.show(false);
        }
        self.page = page.min(self.pagination.pages.len().saturating_sub(1));
        self.anchor = self.pagination.anchor_of(self.page);
        self.show(false)
    }

    /// Jump to a position (e.g. a TOC entry: `Anchor { chapter, offset: 0 }`).
    pub fn goto_anchor(&mut self, a: Anchor) -> Result<StepReport> {
        self.complete_layout();
        if self.window.as_ref().is_some_and(|w| w.chapter != a.chapter as usize)
            && (a.chapter as usize) < self.book.chapters.len()
        {
            self.lay_out_chapter(a.chapter as usize);
        }
        self.page = self.pagination.page_for(a);
        self.anchor = a;
        self.show(false)
    }

    pub fn next_page(&mut self) -> Result<Option<StepReport>> {
        self.complete_layout();
        if self.page + 1 >= self.pagination.pages.len() {
            if !self.move_window(true) {
                return Ok(None);
            }
            self.page = 0;
        } else {
            self.page += 1;
        }
        self.anchor = self.pagination.anchor_of(self.page);
        self.show(false).map(Some)
    }

    pub fn prev_page(&mut self) -> Result<Option<StepReport>> {
        self.complete_layout();
        if self.page == 0 {
            if !self.move_window(false) {
                return Ok(None);
            }
            self.page = self.pagination.pages.len().saturating_sub(1);
        } else {
            self.page -= 1;
        }
        self.anchor = self.pagination.anchor_of(self.page);
        self.show(false).map(Some)
    }

    /// Re-paginate with a new style, keep the reading anchor, show with a full refresh.
    pub fn set_style(&mut self, style: Style) -> Result<StepReport> {
        let anchor = self.anchor;
        self.style = style;
        self.lazy = None;
        let t = Instant::now();
        if let Some(window) = self.window.as_mut() {
            // Its numbers were for the other style: the caller knows if it has them for this one.
            window.map = None;
            let chapter = window.chapter;
            self.lay_out_chapter(chapter);
            let layout_ms = ms(t);
            self.page = self.pagination.page_for(anchor);
            let mut rep = self.show(true)?;
            rep.layout_ms = layout_ms;
            return Ok(rep);
        }
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

    /// [`Reader::set_style`] the KindleOS way, for a page on the panel at once on any book: lays out
    /// only from the anchor's paragraph to fill the page that starts at the line holding the anchor
    /// (and the next one, for the turn), and shows it with a full refresh. The rest of the book is
    /// paginated later, a little at a time ([`Reader::layout_step`], or at once with
    /// [`Reader::complete_layout`] / [`Reader::finish_layout`]; navigation completes it first), with a
    /// page break at that line, so the page shown stays as it is (the one before it may be short). Until
    /// then `page` and `page_count` count those two pages and the footer leaves its number out. A new
    /// style change drops the unfinished pagination.
    pub fn set_style_quick(&mut self, style: Style) -> Result<StepReport> {
        if self.window.is_some() {
            return self.set_style(style);
        }
        let t = Instant::now();
        let size = self.display.size();
        let pages = pages_at(
            &mut self.fonts,
            &style,
            size,
            &self.book,
            self.anchor,
            self.images.as_mut(),
            2,
        );
        let Some(first) = pages.first() else {
            return self.set_style(style);
        };
        let lazy = Paginator::new(style, size, break_of(first));
        self.style = style;
        self.pagination = Pagination {
            size,
            style,
            pages,
            partial: true,
            numbers: None,
        };
        self.lazy = Some((lazy, 0.0));
        let layout_ms = ms(t);
        self.page = self.pagination.page_for(self.anchor);
        let mut rep = self.show(true)?;
        rep.layout_ms = layout_ms;
        Ok(rep)
    }

    /// The rest of the book still has to be paginated after [`Reader::set_style_quick`].
    pub fn layout_pending(&self) -> bool {
        self.lazy.is_some()
    }

    /// Paginate more of the rest for about `budget` (at least one paragraph), without drawing; true when
    /// the whole book is paginated (also when nothing was pending). Call it while the reader is idle.
    pub fn layout_step(&mut self, budget: std::time::Duration) -> bool {
        self.advance(Some(Instant::now() + budget))
    }

    /// After [`Reader::set_style_quick`]: paginate the rest at once, without drawing (the next page shown
    /// has the book's numbers), keeping the anchor; the time the whole pagination took, `None` when
    /// nothing was pending.
    pub fn complete_layout(&mut self) -> Option<f64> {
        let ms = self.lazy.as_ref().map(|(_, ms)| *ms)?;
        let t = Instant::now();
        self.advance(None);
        Some(ms + self::ms(t))
    }

    /// After [`Reader::set_style_quick`]: paginate what is left, then update what changed on the panel
    /// (the footer's number) with a partial update. `None` when the page on the panel already has its
    /// number.
    pub fn finish_layout(&mut self) -> Result<Option<StepReport>> {
        self.complete_layout();
        let layout_ms = self.rest_ms;
        if !self.numbers_stale {
            return Ok(None);
        }
        let mut rep = self.show(false)?;
        rep.layout_ms = layout_ms;
        Ok(Some(rep))
    }

    /// Run the pagination of the rest until `deadline`; when it is done, its pages replace the partial
    /// ones (the anchor's page keeps its place). True when nothing is left.
    fn advance(&mut self, deadline: Option<Instant>) -> bool {
        let Some((mut p, mut spent)) = self.lazy.take() else {
            return true;
        };
        let t = Instant::now();
        let done = p.step(&mut self.fonts, &self.book, self.images.as_mut(), deadline);
        spent += ms(t);
        if !done {
            self.lazy = Some((p, spent));
            return false;
        }
        self.pagination = p.into_pagination();
        self.page = self.pagination.page_for(self.anchor);
        self.numbers_stale = true;
        self.rest_ms = spent;
        true
    }

    /// How long the last pagination of the rest after [`Reader::set_style_quick`] took in all (ms).
    pub fn rest_layout_ms(&self) -> f64 {
        self.rest_ms
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
