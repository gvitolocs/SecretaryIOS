//! Book sources for the Lucina Ink reader: DRM-free EPUB 2/3, plain text and Markdown, all converted to
//! one simplified block model ([`doc`]). DRM-protected EPUBs are refused with [`Unsupported::Drm`]; no
//! decryption is attempted. ZIP input is bounded (entry count, per-entry and total expanded size,
//! compression ratio) so a hostile file cannot exhaust the Kindle's 512 MB of RAM.
pub mod doc;
pub mod epub;
pub mod text;
mod xhtml;

pub use doc::{Block, Book, Chapter, ListMarker, SourceFormat, Span, SpanStyle, TocEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsupported {
    /// Adobe ADEPT / other DRM (`META-INF/rights.xml`, or `encryption.xml` beyond font obfuscation).
    Drm,
    /// ZIP-level (password) encryption.
    EncryptedZip,
    /// Not an EPUB/TXT/Markdown file.
    Format,
}

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Unsupported::Drm => {
                "DRM-protected EPUB: Lucina Ink only opens DRM-free books and never decrypts"
            }
            Unsupported::EncryptedZip => "password-encrypted ZIP entries are not supported",
            Unsupported::Format => "unsupported file type (expected .epub, .txt or .md)",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unsupported: {0}")]
    Unsupported(Unsupported),
    #[error("invalid book: {0}")]
    Invalid(String),
    #[error("limit exceeded: {0}")]
    Limit(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

/// Open a book by file extension (`.epub`, `.txt`, `.md`/`.markdown`).
pub fn open(path: &std::path::Path) -> Result<Book> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled")
        .to_owned();
    match ext.as_str() {
        "epub" => epub::read(std::fs::File::open(path)?, &epub::Limits::default()),
        "txt" | "text" => Ok(text::from_txt(&read_capped(path)?, &stem)),
        "md" | "markdown" => Ok(text::from_markdown(&read_capped(path)?, &stem)),
        _ => Err(Error::Unsupported(Unsupported::Format)),
    }
}

/// Plain-text/Markdown inputs are read whole; cap them like EPUB content.
pub const MAX_TEXT_BYTES: u64 = 64 * 1024 * 1024;

fn read_capped(path: &std::path::Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let f = std::fs::File::open(path)?;
    let mut b = Vec::new();
    f.take(MAX_TEXT_BYTES + 1).read_to_end(&mut b)?;
    if b.len() as u64 > MAX_TEXT_BYTES {
        return Err(Error::Limit("text file larger than 64 MiB".into()));
    }
    Ok(b)
}
