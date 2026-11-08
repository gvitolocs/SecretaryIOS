//! Pictures inside a book. ink-render does no image decoding: the app supplies each picture's size (for the
//! layout) and draws its pixels into the box the layout chose, through an [`Images`] source.
use ink_kindle_hal::{GrayImage, Rect};

/// Intrinsic pixel size of a picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageSize {
    pub w: u32,
    pub h: u32,
}

/// What drawing a picture did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrawOutcome {
    /// The picture is in the box.
    Drawn,
    /// Not decoded yet (being prepared): ink-render draws a light frame; the app redraws the page later.
    Pending,
    /// Unreadable / not found: ink-render draws the placeholder ("[image] alt") in the box.
    Missing,
}

/// A picture the reader is about to need, with the box it will be drawn in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRequest {
    pub chapter: u32,
    pub block: u32,
    pub src: String,
    pub w: u32,
    pub h: u32,
}

/// How the pictures of one rendered page came out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PageImages {
    pub drawn: u32,
    pub pending: u32,
    pub missing: u32,
}

impl PageImages {
    pub(crate) fn add(&mut self, o: DrawOutcome) {
        match o {
            DrawOutcome::Drawn => self.drawn += 1,
            DrawOutcome::Pending => self.pending += 1,
            DrawOutcome::Missing => self.missing += 1,
        }
    }
}

/// Pictures inside a book, supplied by the app; ink-render does no image decoding.
pub trait Images {
    /// Intrinsic size of picture `src` (the `Block::Image::src` string exactly as in the book) of chapter
    /// `chapter`. `None` = unknown: laid out and drawn as the placeholder.
    fn size(&mut self, chapter: u32, src: &str) -> Option<ImageSize>;
    /// Draw the picture scaled to exactly `dst` (the layout already kept its aspect ratio) into `img`.
    fn draw(&mut self, chapter: u32, src: &str, img: &mut GrayImage, dst: Rect) -> DrawOutcome;
    /// The pictures of the pages the reader will probably show next, most likely first. Called after every
    /// page is shown. Default: ignore.
    fn prefetch(&mut self, _wanted: &[ImageRequest]) {}
}

/// No pictures: every image is the placeholder.
pub struct NoImages;

impl Images for NoImages {
    fn size(&mut self, _chapter: u32, _src: &str) -> Option<ImageSize> {
        None
    }
    fn draw(&mut self, _chapter: u32, _src: &str, _img: &mut GrayImage, _dst: Rect) -> DrawOutcome {
        DrawOutcome::Missing
    }
}
