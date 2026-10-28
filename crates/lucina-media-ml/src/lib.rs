//! Smart search for Lucina's media library: what a photo shows, as a vector.
//!
//! - [`siglip`]: SigLIP2 (image and text towers, ONNX, CPU) turns a photo or a query into a
//!   768-number unit vector; a photo and a text about it point the same way. The text tower is
//!   multilingual, so Italian and Danish queries work like English ones.
//! - [`store`]: the vectors in the catalog (`ml_embeddings`, f16), one row per item and model.
//! - [`index`]: every vector of a model in memory, searched by brute force (a 10k-photo library
//!   is about 30 MB of f32 and a scan takes a few milliseconds; no vector database).
//! - [`categories`]: photo categories (screenshot, receipt, document, ...) from a typed feature record
//!   (zero-shot scores from the vectors, size, format, source, name hint) and a `Decider` that turns
//!   the record into a category; [`categories_store`] keeps the decisions and the owner's corrections.
//! - [`thermal`]: the CPU temperature, so background work pauses when nezopt runs hot.
//! - [`faces`]: YuNet finds faces, SFace turns each into a 128-value vector (ONNX, CPU);
//!   [`faces_store`] keeps faces, people and scan records in the catalog; [`cluster`] groups face
//!   vectors into people, incrementally.
//!
//! The HTTP routes and the background worker live in `lucina-media` (`smart.rs`), which knows
//! the catalog's items and renditions; this crate knows nothing about them.

pub mod categories;
pub mod categories_jev;
pub mod categories_store;
pub mod cluster;
pub mod faces;
pub mod faces_store;
pub mod index;
pub(crate) mod runtime;
pub mod siglip;
pub mod store;
pub mod thermal;
pub mod vector;

use image::RgbImage;

/// Drops everything this crate keeps about an item: its vectors and failures in every model, and
/// its category decision, its faces (people left with no face are dropped unless named; covers are fixed). The catalog's
/// purge calls it when an item is deleted for good. Face crops under the data folder belong to the
/// service, which removes the ones whose face is gone (`lucina-media`'s `faces::prune`).
pub fn forget_item(c: &rusqlite::Connection, item_id: &str) -> rusqlite::Result<()> {
    store::forget(c, item_id)?;
    faces_store::forget(c, item_id)?;
    categories_store::forget(c, item_id)?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum MlError {
    /// The runtime or a model is missing or failed to load: smart search is off, nothing else breaks.
    #[error("smart search unavailable: {0}")]
    Unavailable(String),
    #[error("embedding failed: {0}")]
    Failed(String),
}

/// Turns pictures and text into comparable unit vectors.
pub trait Embedder: Send + Sync {
    /// Stored with every vector; vectors of different models are never compared.
    fn model(&self) -> &str;
    fn dim(&self) -> usize;
    /// One unit vector per picture, in order.
    fn embed_images(&self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError>;
    /// The unit vector of a query.
    fn embed_text(&self, text: &str) -> Result<Vec<f32>, MlError>;
    /// A cosine similarity between a picture and a text as a 0..1 match score.
    fn relevance(&self, cosine: f32) -> f32;
}
