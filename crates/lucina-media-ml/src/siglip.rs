//! SigLIP2 (google/siglip2-base-patch16-256, Apache-2.0) on ONNX Runtime, CPU.
//!
//! The ONNX export is onnx-community/siglip2-base-patch16-256-ONNX: `onnx/vision_model.onnx`
//! (fp32, 372 MB) and `onnx/text_model.onnx` (fp32, 1.1 GB, most of it the 256k-token Gemma
//! vocabulary), plus `tokenizer.json`. Both towers end in a pooled 768-value vector; the two
//! spaces are shared, so a photo and a caption of it have a high cosine.
//!
//! - Pictures: resized to 256×256 (no crop, aspect not kept: the model was trained that way),
//!   RGB scaled to −1..1 (mean 0.5, std 0.5), channels first.
//! - Text: lower-cased (SigLIP2 was trained on lower-case captions), Gemma tokens with the end
//!   token, padded with 0 to 64 tokens, no attention mask.
//! - Match score: `sigmoid(scale · cos + bias)` with the checkpoint's learned `logit_scale`
//!   (exp 4.7265 ≈ 112.9) and `logit_bias` (−16.77).
//!
//! The text tower is loaded on the first query and dropped after a while without one
//! ([`Siglip::release_idle`]); the vision tower only lives while the worker embeds.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use image::imageops::FilterType;
use image::RgbImage;
use ort::session::Session;
use ort::value::Tensor;

use crate::vector::normalize;
use crate::{Embedder, MlError};

pub const MODEL: &str = "siglip2-base-patch16-256";
pub const DIM: usize = 768;
pub const IMAGE_PX: u32 = 256;
pub const TEXT_LEN: usize = 64;
pub const LOGIT_SCALE: f32 = 112.901_18;
pub const LOGIT_BIAS: f32 = -16.771_803;
const PAD_ID: i64 = 0;
const EOS_ID: i64 = 1;

#[derive(Clone, Debug)]
pub struct SiglipConfig {
    /// `libonnxruntime.so` (1.22): `LUCINA_MEDIA_ML_ORT_LIB`.
    pub ort_lib: PathBuf,
    /// The export's folder (`onnx/`, `tokenizer.json`): `LUCINA_MEDIA_ML_MODEL_DIR`.
    pub model_dir: PathBuf,
    /// ONNX Runtime threads per model: `LUCINA_MEDIA_ML_THREADS` (default 3, a quarter of nezopt).
    pub threads: usize,
}

impl SiglipConfig {
    pub fn from_env() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/nez".into());
        let path = |key: &str, default: String| {
            std::env::var(key)
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(default))
        };
        Self {
            ort_lib: path(
                "LUCINA_MEDIA_ML_ORT_LIB",
                format!("{home}/models/diarization/onnxruntime/lib/libonnxruntime.so"),
            ),
            model_dir: path(
                "LUCINA_MEDIA_ML_MODEL_DIR",
                format!("{home}/models/siglip2-base-256"),
            ),
            threads: std::env::var("LUCINA_MEDIA_ML_THREADS")
                .ok()
                .and_then(|s| s.parse().ok())
                .filter(|n| *n > 0)
                .unwrap_or(3),
        }
    }

    pub fn vision_path(&self) -> PathBuf {
        self.model_dir.join("onnx/vision_model.onnx")
    }
    pub fn text_path(&self) -> PathBuf {
        self.model_dir.join("onnx/text_model.onnx")
    }
    pub fn tokenizer_path(&self) -> PathBuf {
        self.model_dir.join("tokenizer.json")
    }

    /// The first file that is missing, if any.
    pub fn missing(&self) -> Option<PathBuf> {
        [
            self.ort_lib.clone(),
            self.vision_path(),
            self.text_path(),
            self.tokenizer_path(),
        ]
        .into_iter()
        .find(|p| !p.is_file())
    }
}

fn unavailable(e: impl std::fmt::Display) -> MlError {
    MlError::Unavailable(e.to_string())
}
fn failed(e: impl std::fmt::Display) -> MlError {
    MlError::Failed(e.to_string())
}

/// ONNX Runtime from `lib`, once per process (it cannot be reloaded). A library that fails to
/// load leaves smart search off instead of crashing the service.
fn init_runtime(lib: &Path) -> Result<(), MlError> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        let path = lib.display().to_string();
        std::panic::catch_unwind(|| ort::init_from(path).with_name("lucina-media-ml").commit())
            .map_err(|_| format!("could not load ONNX Runtime from {}", lib.display()))?
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .clone()
    .map_err(MlError::Unavailable)
}

fn session(path: &Path, threads: usize) -> Result<Session, MlError> {
    std::panic::catch_unwind(|| {
        Session::builder()?
            .with_intra_threads(threads.max(1))?
            .with_inter_threads(1)?
            .commit_from_file(path)
    })
    .map_err(|_| MlError::Unavailable("ONNX Runtime panicked loading a model".into()))?
    .map_err(|e| MlError::Unavailable(format!("{}: {e}", path.display())))
}

/// The pooled output: `pooler_output` by name, else the second output (the transformers export
/// order is `last_hidden_state`, `pooler_output`).
fn pooled(outputs: &ort::session::SessionOutputs, rows: usize) -> Result<Vec<Vec<f32>>, MlError> {
    let value = match outputs.get("pooler_output") {
        Some(v) => v,
        None if outputs.len() > 1 => &outputs[1],
        None => return Err(failed("model has no pooled output")),
    };
    let (shape, data) = value.try_extract_tensor::<f32>().map_err(failed)?;
    let dim = *shape.last().unwrap_or(&0) as usize;
    if shape.len() != 2 || shape[0] as usize != rows || dim == 0 {
        return Err(failed(format!("unexpected output shape {shape:?}")));
    }
    Ok(data
        .chunks_exact(dim)
        .map(|row| {
            let mut v = row.to_vec();
            normalize(&mut v);
            v
        })
        .collect())
}

/// One picture as the vision tower's input: 3×256×256, −1..1, channels first.
pub fn preprocess(img: &RgbImage) -> Vec<f32> {
    let small = image::imageops::resize(img, IMAGE_PX, IMAGE_PX, FilterType::Triangle);
    let n = (IMAGE_PX * IMAGE_PX) as usize;
    let mut out = vec![0f32; 3 * n];
    for (i, p) in small.pixels().enumerate() {
        for c in 0..3 {
            out[c * n + i] = p[c] as f32 / 127.5 - 1.0;
        }
    }
    out
}

/// Token ids for a query: lower case, the end token last, padded or cut to [`TEXT_LEN`].
pub fn text_ids(tok: &tokenizers::Tokenizer, text: &str) -> Result<Vec<i64>, MlError> {
    let enc = tok.encode(text.to_lowercase(), true).map_err(failed)?;
    let mut ids: Vec<i64> = enc.get_ids().iter().map(|i| *i as i64).collect();
    if ids.last() != Some(&EOS_ID) {
        ids.push(EOS_ID);
    }
    if ids.len() > TEXT_LEN {
        ids.truncate(TEXT_LEN - 1);
        ids.push(EOS_ID);
    }
    ids.resize(TEXT_LEN, PAD_ID);
    Ok(ids)
}

pub fn relevance(cosine: f32) -> f32 {
    1.0 / (1.0 + (-(LOGIT_SCALE * cosine + LOGIT_BIAS)).exp())
}

struct TextTower {
    session: Session,
    used: Instant,
}

pub struct Siglip {
    cfg: SiglipConfig,
    vision: Mutex<Option<Session>>,
    text: Mutex<Option<TextTower>>,
    tokenizer: tokenizers::Tokenizer,
}

impl Siglip {
    /// Checks the files and the runtime and reads the tokenizer; the towers load on first use.
    pub fn load(cfg: SiglipConfig) -> Result<Self, MlError> {
        if let Some(p) = cfg.missing() {
            return Err(unavailable(format!("{} not found", p.display())));
        }
        init_runtime(&cfg.ort_lib)?;
        let tokenizer =
            tokenizers::Tokenizer::from_file(cfg.tokenizer_path()).map_err(unavailable)?;
        Ok(Self {
            cfg,
            vision: Mutex::new(None),
            text: Mutex::new(None),
            tokenizer,
        })
    }

    /// Drops the text tower after `idle` without a query, and the vision tower when asked
    /// (`vision`: the worker has nothing left to embed). Frees 1.1 GB and 0.4 GB.
    pub fn release_idle(&self, idle: Duration, vision: bool) {
        let mut t = self.text.lock().unwrap_or_else(|p| p.into_inner());
        if t.as_ref().is_some_and(|t| t.used.elapsed() >= idle) {
            *t = None;
            tracing::info!("smart search: text model unloaded (idle)");
        }
        drop(t);
        if vision {
            let mut v = self.vision.lock().unwrap_or_else(|p| p.into_inner());
            if v.take().is_some() {
                tracing::info!("smart search: image model unloaded (idle)");
            }
        }
    }

    pub fn text_loaded(&self) -> bool {
        self.text
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }
}

impl Embedder for Siglip {
    fn model(&self) -> &str {
        MODEL
    }

    fn dim(&self) -> usize {
        DIM
    }

    fn embed_images(&self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError> {
        if images.is_empty() {
            return Ok(Vec::new());
        }
        let mut data = Vec::with_capacity(images.len() * 3 * (IMAGE_PX * IMAGE_PX) as usize);
        for img in images {
            data.extend(preprocess(img));
        }
        let px = IMAGE_PX as usize;
        let input = Tensor::from_array(([images.len(), 3usize, px, px], data)).map_err(failed)?;
        let mut guard = self.vision.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            *guard = Some(session(&self.cfg.vision_path(), self.cfg.threads)?);
        }
        let s = guard.as_mut().expect("loaded above");
        let outputs = s.run(ort::inputs![input]).map_err(failed)?;
        pooled(&outputs, images.len())
    }

    fn embed_text(&self, text: &str) -> Result<Vec<f32>, MlError> {
        let ids = text_ids(&self.tokenizer, text)?;
        let input = Tensor::from_array(([1usize, TEXT_LEN], ids)).map_err(failed)?;
        let mut guard = self.text.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            let t0 = Instant::now();
            *guard = Some(TextTower {
                session: session(&self.cfg.text_path(), self.cfg.threads)?,
                used: Instant::now(),
            });
            tracing::info!(
                ms = t0.elapsed().as_millis() as u64,
                "smart search: text model loaded"
            );
        }
        let t = guard.as_mut().expect("loaded above");
        t.used = Instant::now();
        let outputs = t.session.run(ort::inputs![input]).map_err(failed)?;
        pooled(&outputs, 1)?
            .pop()
            .ok_or_else(|| failed("no text vector"))
    }

    fn relevance(&self, cosine: f32) -> f32 {
        relevance(cosine)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preprocess_scales_to_minus_one_one_channels_first() {
        let mut img = RgbImage::new(40, 20);
        for p in img.pixels_mut() {
            *p = image::Rgb([255, 0, 128]);
        }
        let x = preprocess(&img);
        let n = (IMAGE_PX * IMAGE_PX) as usize;
        assert_eq!(x.len(), 3 * n);
        assert!((x[0] - 1.0).abs() < 1e-6);
        assert!((x[n] + 1.0).abs() < 1e-6);
        assert!((x[2 * n] - 0.003_921_6).abs() < 1e-4);
    }

    #[test]
    fn relevance_is_the_checkpoint_sigmoid() {
        let half = -LOGIT_BIAS / LOGIT_SCALE;
        assert!((relevance(half) - 0.5).abs() < 1e-5);
        assert!(relevance(0.0) < 1e-6);
        assert!(relevance(0.3) > 0.999);
        assert!(relevance(0.2) > relevance(0.1));
    }

    #[test]
    fn config_reports_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SiglipConfig {
            ort_lib: dir.path().join("libonnxruntime.so"),
            model_dir: dir.path().to_path_buf(),
            threads: 1,
        };
        assert_eq!(cfg.missing(), Some(dir.path().join("libonnxruntime.so")));
        assert!(matches!(Siglip::load(cfg), Err(MlError::Unavailable(_))));
    }
}
