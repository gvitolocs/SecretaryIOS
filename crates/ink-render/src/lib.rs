//! Page model, text layout and grayscale rendering for the Lucina Ink reader (Kindle PW3 panel,
//! 1072×1448 at 300 dpi), plus the [`reader::Reader`] loop that drives an `ink_kindle_hal::Display`,
//! the app's own system screens ([`screens`]: splash and sleep picture) and the [`session::Session`]
//! state machine (splash → reading ⇄ sleeping, idle timeout, sleep-aware position saving).
pub mod dirty;
pub mod font;
pub mod images;
pub mod layout;
pub mod reader;
pub mod render;
pub mod screens;
pub mod session;

pub use font::{Face, Fonts};
pub use images::{DrawOutcome, ImageRequest, ImageSize, Images, NoImages, PageImages};
pub use layout::{
    image_box, paginate, paginate_with, Anchor, Margins, Page, Pagination, Style, FONT_SIZES,
};
pub use reader::{Reader, StepReport};
pub use screens::{Background, ScreenKind, ScreenSource, SystemScreens};
pub use session::{
    AnchorStore, Clock, FileAnchorStore, MemoryAnchorStore, Outcome, ScreenReport, Session,
    SessionConfig, SessionState, SleepReason, SystemClock, DEFAULT_IDLE_MS,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("font: {0}")]
    Font(String),
    #[error("book: {0}")]
    Book(String),
    #[error(transparent)]
    Display(#[from] ink_kindle_hal::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
