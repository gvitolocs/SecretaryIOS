//! Page model, text layout and grayscale rendering for the Lucina Ink reader (Kindle PW3 panel,
//! 1072×1448 at 300 dpi), plus the [`reader::Reader`] loop that drives an `ink_kindle_hal::Display`.
pub mod dirty;
pub mod font;
pub mod layout;
pub mod reader;
pub mod render;

pub use font::{Face, Fonts};
pub use layout::{paginate, Anchor, Margins, Page, Pagination, Style, FONT_SIZES};
pub use reader::{Reader, StepReport};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("font: {0}")]
    Font(String),
    #[error(transparent)]
    Display(#[from] ink_kindle_hal::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
