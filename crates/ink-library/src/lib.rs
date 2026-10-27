//! Read-only Kindle metadata and clippings import. No DRM decryption, network,
//! sidecar parsing, or inferred reading activity.
pub mod clippings;
pub mod epub;
pub mod import;
pub mod mobi;
pub mod model;
pub mod pdf;
pub mod scan;
pub mod search;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
