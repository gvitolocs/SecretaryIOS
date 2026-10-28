//! Smart search for Lucina's media library: what a photo shows, as a vector.
//!
//! - [`siglip`]: SigLIP2 (image and text towers, ONNX, CPU) turns a photo or a query into a
//!   768-number unit vector; a photo and a text about it point the same way. The text tower is
//!   multilingual, so Italian and Danish queries work like English ones.
//! - [`gemma`]: EmbeddingGemma 2 (one Gemma language model for pictures and text, ONNX, CPU), the
//!   default model; vectors of the two models are never mixed, the service answers from the old
//!   model's index while the new one catches up (`lucina-media`'s `smart.rs`).
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
pub mod gemma;
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
///
/// Everything after `relevance` has a default that fits SigLIP2, so a model only says where it
/// differs (thresholds, query rules, the zero-shot scale, memory it can give back).
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

    /// A query as it is embedded and cached: trimmed, and lower-cased unless the model was
    /// trained on mixed case.
    fn normalize_query(&self, text: &str) -> String {
        text.trim().to_lowercase()
    }
    /// Text-to-picture cosine below which a photo is not a match.
    fn default_text_min(&self) -> f32 {
        0.09
    }
    /// Picture-to-picture cosine below which a photo is not "similar".
    fn default_similar_min(&self) -> f32 {
        0.6
    }
    /// Scales a cosine difference between category prompts into the zero-shot softmax
    /// (`categories::ZERO_SHOT_SCALE` for SigLIP2).
    fn zero_shot_scale(&self) -> f32 {
        categories::ZERO_SHOT_SCALE
    }
    /// The vector of a category prompt: a description of what a picture shows, not a query.
    fn embed_label(&self, text: &str) -> Result<Vec<f32>, MlError> {
        self.embed_text(text)
    }
    /// Gives back memory: parts idle for `idle` are dropped, the picture part too when `vision`
    /// (the worker has nothing left to embed). They load again on use.
    fn release_idle(&self, _idle: std::time::Duration, _vision: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An embedder that only says what is required: everything else is SigLIP2's default.
    struct Bare;

    impl Embedder for Bare {
        fn model(&self) -> &str {
            "bare"
        }
        fn dim(&self) -> usize {
            1
        }
        fn embed_images(&self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError> {
            Ok(images.iter().map(|_| vec![1.0]).collect())
        }
        fn embed_text(&self, text: &str) -> Result<Vec<f32>, MlError> {
            Ok(vec![text.len() as f32])
        }
        fn relevance(&self, cosine: f32) -> f32 {
            cosine
        }
    }

    #[test]
    fn an_embedder_that_says_nothing_extra_gets_the_siglip_defaults() {
        let e = Bare;
        assert_eq!(e.normalize_query("  A Red CAR "), "a red car");
        assert_eq!(e.default_text_min(), 0.09);
        assert_eq!(e.default_similar_min(), 0.6);
        assert_eq!(e.zero_shot_scale(), categories::ZERO_SHOT_SCALE);
        assert_eq!(e.embed_label("abc").unwrap(), vec![3.0]);
        e.release_idle(std::time::Duration::ZERO, true);
    }

    #[test]
    fn the_two_models_differ_where_they_should() {
        // Gemma keeps the case of a query and has its own cosine band; SigLIP2 does not.
        assert_eq!(gemma::DEFAULT_TEXT_MIN, 0.65);
        assert_eq!(gemma::DEFAULT_SIMILAR_MIN, 0.75);
        const { assert!(gemma::ZERO_SHOT_SCALE < categories::ZERO_SHOT_SCALE) };
        assert_ne!(gemma::model_id(gemma::DEFAULT_BUDGET), siglip::MODEL);
    }
}
