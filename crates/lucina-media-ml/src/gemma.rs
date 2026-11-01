//! EmbeddingGemma 2 (onnx-community/embeddinggemma-2-ONNX, fp32, Apache-2.0) on ONNX Runtime, CPU.
//!
//! One Gemma language model embeds both pictures and text into the same 768-value space; a vision
//! encoder in front of it turns a picture into soft tokens. The files (in the model folder):
//! `onnx/vision_encoder.onnx` (+ `_data`), `onnx/model.onnx` (+ `_data`) and `tokenizer.json`. The
//! `*_quantized.onnx` files do not load on ONNX Runtime 1.22 and are never used.
//!
//! Tensors (checked against the sessions by `tests/real_gemma.rs`):
//!
//! - `vision_encoder.onnx`: `pixel_values` `[1, N, 768]` f32 and `pixel_position_ids` `[1, N, 2]`
//!   i64 in, `image_features` `[N / 9, 512]` f32 out.
//! - `model.onnx`: `input_ids` `[1, L]` i64, `attention_mask` `[1, L]` i64 (all ones),
//!   `image_features` `[n, 512]`, `video_features` `[0, 512]`, `audio_features` `[0, 512]` f32 in,
//!   `sentence_embedding` `[1, 768]` f32 out (unit length after [`normalize`]).
//!
//! - Pictures: RGB, resized (bicubic, Catmull-Rom; upscaling allowed, aspect kept) to the largest
//!   size whose area fits `budget × 9` patches of 16×16 with both sides a multiple of 48, scaled
//!   0..1 (no mean or std), cut into 16×16 patches flattened as (y, x, channel) = 768 values,
//!   with `(x, y)` position ids in row-major order. `budget` tokens-ish: 70 gives about 62 soft
//!   tokens per picture. The language model sees `<bos> <boi> <image_soft_token>×n <eoi> <eos>`.
//! - Text: `task: search result | query: ` in front of a query, never lower-cased, then the
//!   tokenizer's own `<bos> … <eos>`; the same language model with no image features.
//! - Match score: no learned scale; `clamp((cos − 0.62) / 0.18, 0, 1)`.
//!
//! The language model (about 1.1 GB) is shared by the picture and the text path and loaded once;
//! the vision encoder lives on its own. Both are dropped after a while without use
//! ([`Gemma::release_idle`]): the language model when idle, the vision encoder when the worker
//! has nothing left to embed.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use image::imageops::FilterType;
use image::RgbImage;
use ort::memory::Allocator;
use ort::session::Session;
use ort::value::Tensor;

use crate::runtime::{failed, init_runtime, session, unavailable};
use crate::vector::normalize;
use crate::{Embedder, MlError};

/// The model id without a budget; vectors are stored under [`model_id`].
pub const MODEL_PREFIX: &str = "embeddinggemma-2-768";
pub const DIM: usize = 768;
/// Soft-token budget of a picture: 70 (about 62 tokens). The supported steps are 70, 140, 280,
/// 560 and 1120; any value from [`MIN_BUDGET`] to [`MAX_BUDGET`] is accepted.
pub const DEFAULT_BUDGET: usize = 70;
pub const MIN_BUDGET: usize = 8;
pub const MAX_BUDGET: usize = 1120;
/// Task prefix of a search query (the model card's retrieval prompt).
pub const QUERY_PREFIX: &str = "task: search result | query: ";
/// Cosine at which the score starts to rise, and the span over which it reaches 1.
pub const REL_FLOOR: f32 = 0.62;
pub const REL_SPAN: f32 = 0.18;
/// Scales a cosine difference between category prompts into the zero-shot softmax (SigLIP2 uses
/// 100). Gemma's image-text cosines sit around 0.65 and its categories are about 1.3 to 1.6 times
/// further apart: on the public XM3600 pictures the mean gap between a picture's two best
/// categories is 0.0311 against SigLIP2's 0.0198 (equivalent scale 64) and the spread across
/// categories 0.0273 against 0.0212 (equivalent scale 78); 70 sits between.
pub const ZERO_SHOT_SCALE: f32 = 70.0;
/// Text-to-picture cosine below which a photo is not a match (score ≈ 0.17).
pub const DEFAULT_TEXT_MIN: f32 = 0.65;
/// Picture-to-picture cosine below which a photo is not "similar".
pub const DEFAULT_SIMILAR_MIN: f32 = 0.75;

const PATCH: usize = 16;
/// Patches merged into one soft token: 3×3.
const POOL: usize = 3;
const PATCH_VALUES: usize = PATCH * PATCH * 3;
const FEATURE_DIM: usize = 512;
const BOS: i64 = 2;
const EOS: i64 = 1;
const BEGIN_IMAGE: i64 = 255_999;
const IMAGE_TOKEN: i64 = 258_880;
const END_IMAGE: i64 = 258_882;
/// Queries are cut to this many tokens (the form field allows 300 characters).
const MAX_TEXT_TOKENS: usize = 256;

/// The id stored with every vector: the budget changes the vectors, so it is part of the model.
pub fn model_id(budget: usize) -> String {
    format!("{MODEL_PREFIX}-b{budget}")
}

/// The budget of a stored model id, `None` for any other model.
pub fn budget_of(model: &str) -> Option<usize> {
    model
        .strip_prefix(MODEL_PREFIX)?
        .strip_prefix("-b")?
        .parse()
        .ok()
        .filter(|b| (MIN_BUDGET..=MAX_BUDGET).contains(b))
}

#[derive(Clone, Debug)]
pub struct GemmaConfig {
    /// `libonnxruntime.so` (1.22): `LUCINA_MEDIA_ML_ORT_LIB`, the same as the other models.
    pub ort_lib: PathBuf,
    /// The export's folder (`onnx/`, `tokenizer.json`): `LUCINA_MEDIA_ML_GEMMA_DIR`.
    pub model_dir: PathBuf,
    /// ONNX Runtime threads per session: `LUCINA_MEDIA_ML_THREADS` (default 3).
    pub threads: usize,
    /// Picture budget: `LUCINA_MEDIA_ML_GEMMA_BUDGET` (default [`DEFAULT_BUDGET`]).
    pub budget: usize,
}

impl GemmaConfig {
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
                "LUCINA_MEDIA_ML_GEMMA_DIR",
                format!("{home}/models/embeddinggemma-2-onnx"),
            ),
            threads: std::env::var("LUCINA_MEDIA_ML_THREADS")
                .ok()
                .and_then(|s| s.parse().ok())
                .filter(|n| *n > 0)
                .unwrap_or(3),
            budget: std::env::var("LUCINA_MEDIA_ML_GEMMA_BUDGET")
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .filter(|b| (MIN_BUDGET..=MAX_BUDGET).contains(b))
                .unwrap_or(DEFAULT_BUDGET),
        }
    }

    /// The same files with another budget (a stored model id of an older budget).
    pub fn with_budget(mut self, budget: usize) -> Self {
        self.budget = budget;
        self
    }

    pub fn vision_path(&self) -> PathBuf {
        self.model_dir.join("onnx/vision_encoder.onnx")
    }
    pub fn lm_path(&self) -> PathBuf {
        self.model_dir.join("onnx/model.onnx")
    }
    pub fn tokenizer_path(&self) -> PathBuf {
        self.model_dir.join("tokenizer.json")
    }

    /// The first file that is missing, if any (the `_data` files hold the weights).
    pub fn missing(&self) -> Option<PathBuf> {
        [
            self.ort_lib.clone(),
            self.vision_path(),
            self.model_dir.join("onnx/vision_encoder.onnx_data"),
            self.lm_path(),
            self.model_dir.join("onnx/model.onnx_data"),
            self.tokenizer_path(),
        ]
        .into_iter()
        .find(|p| !p.is_file())
    }
}

// ---- pictures ----------------------------------------------------------------------------------

/// The size a `h`×`w` picture is resized to for at most `max_patches` patches: the largest size
/// that keeps the aspect ratio and fits, rounded down to multiples of 48 (a soft token covers 3×3
/// patches of 16 pixels). The processor's own rule (`get_aspect_ratio_preserving_size`), including
/// its handling of a side that rounds to zero. Returns `(height, width)`.
pub fn target_size(h: usize, w: usize, max_patches: usize) -> (usize, usize) {
    let side = POOL * PATCH;
    if h == 0 || w == 0 {
        return (side, side);
    }
    let f = ((max_patches * PATCH * PATCH) as f64 / (h * w) as f64).sqrt();
    let mut th = ((f * h as f64) / side as f64).floor() as usize * side;
    let mut tw = ((f * w as f64) / side as f64).floor() as usize * side;
    let max_side = (max_patches / (POOL * POOL)) * side;
    if th == 0 && tw == 0 {
        th = side;
        tw = side;
    } else if th == 0 {
        th = side;
        tw = ((w as f64 / h as f64).floor() as usize * side).min(max_side);
    } else if tw == 0 {
        tw = side;
        th = ((h as f64 / w as f64).floor() as usize * side).min(max_side);
    }
    (th, tw)
}

/// A picture as the vision encoder's input.
#[derive(Clone, Debug, PartialEq)]
pub struct Patches {
    /// `patches × 768` values, 0..1, each patch flattened as (y, x, channel).
    pub values: Vec<f32>,
    /// `patches × 2` ids, `(x, y)` of each patch, row-major.
    pub positions: Vec<i64>,
    pub patches: usize,
    /// Soft tokens the encoder returns: patches / 9.
    pub tokens: usize,
}

/// One picture for a `budget` (soft tokens-ish; `budget × 9` patches at most).
pub fn preprocess(img: &RgbImage, budget: usize) -> Patches {
    let (h, w) = (img.height() as usize, img.width() as usize);
    let (th, tw) = target_size(h, w, budget * POOL * POOL);
    let resized;
    let src = if (th, tw) == (h, w) {
        img
    } else {
        resized = image::imageops::resize(img, tw as u32, th as u32, FilterType::CatmullRom);
        &resized
    };
    let (ph, pw) = (th / PATCH, tw / PATCH);
    let patches = ph * pw;
    let mut values = vec![0f32; patches * PATCH_VALUES];
    let mut positions = vec![0i64; patches * 2];
    for py in 0..ph {
        for px in 0..pw {
            let idx = py * pw + px;
            positions[idx * 2] = px as i64;
            positions[idx * 2 + 1] = py as i64;
            for y in 0..PATCH {
                for x in 0..PATCH {
                    let p = src.get_pixel((px * PATCH + x) as u32, (py * PATCH + y) as u32);
                    for c in 0..3 {
                        values[idx * PATCH_VALUES + (y * PATCH + x) * 3 + c] = p[c] as f32 / 255.0;
                    }
                }
            }
        }
    }
    Patches {
        values,
        positions,
        patches,
        tokens: patches / (POOL * POOL),
    }
}

/// The language model's token ids for a picture of `tokens` soft tokens.
pub fn image_ids(tokens: usize) -> Vec<i64> {
    let mut ids = Vec::with_capacity(tokens + 4);
    ids.push(BOS);
    ids.push(BEGIN_IMAGE);
    ids.extend(std::iter::repeat_n(IMAGE_TOKEN, tokens));
    ids.push(END_IMAGE);
    ids.push(EOS);
    ids
}

// ---- text --------------------------------------------------------------------------------------

/// What is embedded for a search query: the task prefix and the text as typed.
pub fn query_text(text: &str) -> String {
    format!("{QUERY_PREFIX}{text}")
}

/// Token ids of `text` as given (the tokenizer adds `<bos>` and `<eos>`), cut to
/// [`MAX_TEXT_TOKENS`] with the end token last.
pub fn text_ids(tok: &tokenizers::Tokenizer, text: &str) -> Result<Vec<i64>, MlError> {
    let enc = tok.encode(text, true).map_err(failed)?;
    let mut ids: Vec<i64> = enc.get_ids().iter().map(|i| *i as i64).collect();
    if ids.len() > MAX_TEXT_TOKENS {
        ids.truncate(MAX_TEXT_TOKENS - 1);
        ids.push(EOS);
    }
    Ok(ids)
}

/// A cosine between a picture and a text as a 0..1 score.
pub fn relevance(cosine: f32) -> f32 {
    ((cosine - REL_FLOOR) / REL_SPAN).clamp(0.0, 1.0)
}

// ---- the model ---------------------------------------------------------------------------------

struct LanguageModel {
    session: Session,
    used: Instant,
}

pub struct Gemma {
    cfg: GemmaConfig,
    model: String,
    vision: Mutex<Option<Session>>,
    lm: Mutex<Option<LanguageModel>>,
    tokenizer: tokenizers::Tokenizer,
}

impl Gemma {
    /// Checks the files and the runtime and reads the tokenizer; the sessions load on first use.
    pub fn load(cfg: GemmaConfig) -> Result<Self, MlError> {
        if let Some(p) = cfg.missing() {
            return Err(unavailable(format!("{} not found", p.display())));
        }
        init_runtime(&cfg.ort_lib)?;
        let tokenizer =
            tokenizers::Tokenizer::from_file(cfg.tokenizer_path()).map_err(unavailable)?;
        Ok(Self {
            model: model_id(cfg.budget),
            cfg,
            vision: Mutex::new(None),
            lm: Mutex::new(None),
            tokenizer,
        })
    }

    pub fn budget(&self) -> usize {
        self.cfg.budget
    }

    pub fn lm_loaded(&self) -> bool {
        self.lm.lock().unwrap_or_else(|p| p.into_inner()).is_some()
    }

    pub fn vision_loaded(&self) -> bool {
        self.vision
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }

    /// The token ids a query is embedded from (the prefix included).
    pub fn query_ids(&self, text: &str) -> Result<Vec<i64>, MlError> {
        text_ids(&self.tokenizer, &query_text(text))
    }

    /// The vision encoder: `patches` in, one row of 512 per soft token out.
    fn encode(&self, p: &Patches) -> Result<Vec<f32>, MlError> {
        let mut guard = self.vision.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            *guard = Some(session(&self.cfg.vision_path(), self.cfg.threads)?);
        }
        let s = guard.as_mut().expect("loaded above");
        let n = p.patches;
        let outputs = s
            .run(ort::inputs![
                "pixel_values" => Tensor::from_array(([1usize, n, PATCH_VALUES], p.values.clone())).map_err(failed)?,
                "pixel_position_ids" => Tensor::from_array(([1usize, n, 2usize], p.positions.clone())).map_err(failed)?
            ])
            .map_err(failed)?;
        let (shape, data) = outputs["image_features"]
            .try_extract_tensor::<f32>()
            .map_err(failed)?;
        if shape.len() != 2 || shape[0] as usize != p.tokens || shape[1] as usize != FEATURE_DIM {
            return Err(failed(format!("unexpected image_features shape {shape:?}")));
        }
        Ok(data.to_vec())
    }

    /// The language model on `ids` and `features` (`n × 512`, empty for text): the unit vector.
    fn run_lm(&self, ids: Vec<i64>, features: Vec<f32>) -> Result<Vec<f32>, MlError> {
        let len = ids.len();
        let rows = features.len() / FEATURE_DIM;
        let empty = || Tensor::<f32>::new(&Allocator::default(), [0usize, FEATURE_DIM]);
        let image_features = if rows == 0 {
            empty().map_err(failed)?
        } else {
            Tensor::from_array(([rows, FEATURE_DIM], features)).map_err(failed)?
        };
        let mut guard = self.lm.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            let t0 = Instant::now();
            *guard = Some(LanguageModel {
                session: session(&self.cfg.lm_path(), self.cfg.threads)?,
                used: Instant::now(),
            });
            tracing::info!(
                ms = t0.elapsed().as_millis() as u64,
                "smart search: Gemma language model loaded"
            );
        }
        let lm = guard.as_mut().expect("loaded above");
        lm.used = Instant::now();
        let outputs = lm
            .session
            .run(ort::inputs![
                "input_ids" => Tensor::from_array(([1usize, len], ids)).map_err(failed)?,
                "attention_mask" => Tensor::from_array(([1usize, len], vec![1i64; len])).map_err(failed)?,
                "image_features" => image_features,
                "video_features" => empty().map_err(failed)?,
                "audio_features" => empty().map_err(failed)?
            ])
            .map_err(failed)?;
        let (shape, data) = outputs["sentence_embedding"]
            .try_extract_tensor::<f32>()
            .map_err(failed)?;
        if shape.len() != 2 || shape[0] != 1 || shape[1] as usize != DIM {
            return Err(failed(format!(
                "unexpected sentence_embedding shape {shape:?}"
            )));
        }
        let mut v = data.to_vec();
        if !normalize(&mut v) {
            return Err(failed("the model returned a zero vector"));
        }
        Ok(v)
    }

    fn embed_image(&self, img: &RgbImage) -> Result<Vec<f32>, MlError> {
        let patches = preprocess(img, self.cfg.budget);
        let features = self.encode(&patches)?;
        self.run_lm(image_ids(patches.tokens), features)
    }

    /// Embeds `text` exactly as given (no prefix): for texts that are not search queries.
    pub fn embed_plain(&self, text: &str) -> Result<Vec<f32>, MlError> {
        self.run_lm(text_ids(&self.tokenizer, text)?, Vec::new())
    }

    /// The sessions' input and output names, types and shapes, one line each: what the ignored
    /// real-model test checks against the contract in the module documentation.
    pub fn io_signature(&self) -> Result<Vec<String>, MlError> {
        let mut out = Vec::new();
        let mut vision = self.vision.lock().unwrap_or_else(|p| p.into_inner());
        if vision.is_none() {
            *vision = Some(session(&self.cfg.vision_path(), self.cfg.threads)?);
        }
        let mut lm = self.lm.lock().unwrap_or_else(|p| p.into_inner());
        if lm.is_none() {
            *lm = Some(LanguageModel {
                session: session(&self.cfg.lm_path(), self.cfg.threads)?,
                used: Instant::now(),
            });
        }
        let sessions = [
            ("vision_encoder", vision.as_ref().expect("loaded above")),
            ("model", &lm.as_ref().expect("loaded above").session),
        ];
        for (name, s) in sessions {
            for i in &s.inputs {
                out.push(format!("{name} in  {} {:?}", i.name, i.input_type));
            }
            for o in &s.outputs {
                out.push(format!("{name} out {} {:?}", o.name, o.output_type));
            }
        }
        Ok(out)
    }

    /// Drops the language model after `idle` without use, and the vision encoder when asked
    /// (`vision`: the worker has nothing left to embed). Frees about 1.1 GB and 0.4 GB.
    pub fn release_idle(&self, idle: Duration, vision: bool) {
        let mut lm = self.lm.lock().unwrap_or_else(|p| p.into_inner());
        if lm.as_ref().is_some_and(|l| l.used.elapsed() >= idle) {
            *lm = None;
            tracing::info!("smart search: Gemma language model unloaded (idle)");
        }
        drop(lm);
        if vision {
            let mut v = self.vision.lock().unwrap_or_else(|p| p.into_inner());
            if v.take().is_some() {
                tracing::info!("smart search: Gemma vision encoder unloaded (idle)");
            }
        }
    }
}

impl Embedder for Gemma {
    fn model(&self) -> &str {
        &self.model
    }

    fn dim(&self) -> usize {
        DIM
    }

    fn embed_images(&self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError> {
        images.iter().map(|img| self.embed_image(img)).collect()
    }

    fn embed_text(&self, text: &str) -> Result<Vec<f32>, MlError> {
        self.run_lm(self.query_ids(text)?, Vec::new())
    }

    fn relevance(&self, cosine: f32) -> f32 {
        relevance(cosine)
    }

    fn normalize_query(&self, text: &str) -> String {
        text.trim().to_string()
    }

    /// Category prompts describe a picture; they are embedded as plain text, without the search
    /// task prefix (on crude pictures of six categories: 5 of 6 right either way, the mean gap
    /// of the right category over the best wrong one 0.028 plain against 0.016 with the
    /// prefix and 0.013 with the classification prefix).
    fn embed_label(&self, text: &str) -> Result<Vec<f32>, MlError> {
        self.embed_plain(text)
    }

    fn zero_shot_scale(&self) -> f32 {
        ZERO_SHOT_SCALE
    }

    fn default_text_min(&self) -> f32 {
        DEFAULT_TEXT_MIN
    }

    fn default_similar_min(&self) -> f32 {
        DEFAULT_SIMILAR_MIN
    }

    fn release_idle(&self, idle: Duration, vision: bool) {
        Gemma::release_idle(self, idle, vision);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multiple_of_48(n: usize) -> bool {
        n > 0 && n.is_multiple_of(48)
    }

    #[test]
    fn model_id_carries_the_budget() {
        assert_eq!(model_id(70), "embeddinggemma-2-768-b70");
        assert_ne!(model_id(70), model_id(140));
        assert_eq!(budget_of("embeddinggemma-2-768-b70"), Some(70));
        assert_eq!(budget_of("embeddinggemma-2-768-b1120"), Some(1120));
        assert_eq!(budget_of("embeddinggemma-2-768-b0"), None);
        assert_eq!(budget_of("embeddinggemma-2-768"), None);
        assert_eq!(budget_of("siglip2-base-patch16-256"), None);
    }

    #[test]
    fn sizes_are_multiples_of_48_within_the_budget_and_keep_the_aspect() {
        for budget in [70usize, 140, 280, 560, 1120] {
            for (h, w) in [
                (600usize, 800usize),
                (800, 600),
                (1000, 1000),
                (480, 640),
                (1080, 1920),
                (1920, 1080),
                (200, 1500),
                (300, 300),
            ] {
                let max = budget * 9;
                let (th, tw) = target_size(h, w, max);
                assert!(
                    multiple_of_48(th) && multiple_of_48(tw),
                    "{h}x{w} b{budget}"
                );
                assert!(
                    (th / PATCH) * (tw / PATCH) <= max,
                    "{h}x{w} b{budget} → {th}x{tw} exceeds {max} patches"
                );
                // The aspect is kept up to the rounding of one 48-pixel step.
                let (a, b) = (th as f64 / tw as f64, h as f64 / w as f64);
                let step = 48.0 / tw.min(th) as f64;
                assert!(
                    (a / b - 1.0).abs() < 2.5 * step,
                    "{h}x{w} b{budget}: {th}x{tw}"
                );
            }
        }
    }

    #[test]
    fn known_sizes_for_the_default_budget() {
        // 70 × 9 = 630 patches = 161 280 pixels.
        assert_eq!(target_size(480, 640, 630), (336, 432));
        assert_eq!(target_size(640, 480, 630), (432, 336));
        assert_eq!(target_size(400, 400, 630), (384, 384));
        // 62 soft tokens at most for the default budget.
        let (th, tw) = target_size(1080, 1920, 630);
        assert!((th / 48) * (tw / 48) <= 70);
    }

    #[test]
    fn small_pictures_are_upscaled_and_extreme_ones_get_a_valid_size() {
        let (th, tw) = target_size(48, 48, 630);
        assert!(th > 48 && tw > 48, "upscaled: {th}x{tw}");
        assert_eq!(target_size(1, 1, 630).0 % 48, 0);
        // A very wide strip: the short side rounds to zero and is lifted to one step.
        let (th, tw) = target_size(10, 4000, 630);
        assert_eq!(th, 48);
        assert!(multiple_of_48(tw) && tw <= 70 * 48);
        let (th, tw) = target_size(4000, 10, 630);
        assert_eq!(tw, 48);
        assert!(multiple_of_48(th) && th <= 70 * 48);
        assert_eq!(target_size(0, 5, 630), (48, 48));
    }

    #[test]
    fn patches_are_row_major_with_xy_positions_and_yxc_values() {
        // 96×48 (w×h) at a budget that keeps the size: every pixel encodes its own position.
        let mut img = RgbImage::new(96, 48);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = image::Rgb([x as u8, y as u8, 7]);
        }
        let max = (96 / PATCH) * (48 / PATCH);
        assert_eq!(target_size(48, 96, max), (48, 96));
        let p = preprocess(&img, max / 9);
        assert_eq!(p.patches, 18);
        assert_eq!(p.tokens, 2);
        assert_eq!(p.values.len(), 18 * 768);
        assert_eq!(p.positions.len(), 36);
        // Six patches per row, three rows: patch 7 is column 1, row 1.
        assert_eq!(&p.positions[0..4], &[0, 0, 1, 0]);
        assert_eq!(&p.positions[7 * 2..7 * 2 + 2], &[1, 1]);
        assert_eq!(&p.positions[17 * 2..17 * 2 + 2], &[5, 2]);
        // Patch 7 starts at pixel (16, 16); its value index is (y·16 + x)·3 + c.
        let base = 7 * 768;
        let at = |x: usize, y: usize, c: usize| p.values[base + (y * 16 + x) * 3 + c];
        assert!((at(0, 0, 0) - 16.0 / 255.0).abs() < 1e-6);
        assert!((at(0, 0, 1) - 16.0 / 255.0).abs() < 1e-6);
        assert!((at(0, 0, 2) - 7.0 / 255.0).abs() < 1e-6);
        assert!((at(3, 0, 0) - 19.0 / 255.0).abs() < 1e-6);
        assert!((at(0, 5, 1) - 21.0 / 255.0).abs() < 1e-6);
        assert!((at(15, 15, 0) - 31.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn values_are_zero_to_one_without_mean_and_std() {
        let img = RgbImage::from_pixel(100, 80, image::Rgb([255, 0, 51]));
        let p = preprocess(&img, 70);
        let (th, tw) = target_size(80, 100, 630);
        assert_eq!(p.patches, (th / 16) * (tw / 16));
        assert_eq!(p.tokens * 9, p.patches);
        assert!(p.values.iter().step_by(3).all(|v| (v - 1.0).abs() < 1e-3));
        assert!(p.values.iter().skip(1).step_by(3).all(|v| v.abs() < 1e-3));
        assert!(p
            .values
            .iter()
            .skip(2)
            .step_by(3)
            .all(|v| (v - 0.2).abs() < 1e-3));
    }

    #[test]
    fn image_token_ids_wrap_the_soft_tokens() {
        let ids = image_ids(3);
        assert_eq!(ids, [2, 255_999, 258_880, 258_880, 258_880, 258_882, 1]);
        assert_eq!(image_ids(62).len(), 66);
    }

    #[test]
    fn queries_get_the_task_prefix_and_keep_their_case() {
        assert_eq!(
            query_text("Red Car"),
            "task: search result | query: Red Car"
        );
        assert_eq!(QUERY_PREFIX, "task: search result | query: ");
    }

    #[test]
    fn relevance_maps_the_cosine_band_to_zero_one() {
        assert_eq!(relevance(0.5), 0.0);
        assert_eq!(relevance(0.62), 0.0);
        assert!((relevance(0.71) - 0.5).abs() < 1e-5);
        assert!((relevance(0.80) - 1.0).abs() < 1e-5);
        assert_eq!(relevance(0.95), 1.0);
        assert!(relevance(0.7) > relevance(0.65));
        // The default minimums are inside the band.
        assert!(relevance(DEFAULT_TEXT_MIN) > 0.0 && relevance(DEFAULT_TEXT_MIN) < 0.5);
        assert!(relevance(DEFAULT_SIMILAR_MIN) > 0.5);
    }

    #[test]
    fn config_reports_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = GemmaConfig {
            ort_lib: dir.path().join("libonnxruntime.so"),
            model_dir: dir.path().to_path_buf(),
            threads: 1,
            budget: 70,
        };
        assert_eq!(cfg.missing(), Some(dir.path().join("libonnxruntime.so")));
        assert!(matches!(Gemma::load(cfg), Err(MlError::Unavailable(_))));
    }

    #[test]
    fn config_finds_every_file_and_keeps_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        // Files that exist but are not models: load fails at the tokenizer, never panics.
        std::fs::create_dir_all(dir.path().join("onnx")).unwrap();
        for f in [
            "libonnxruntime.so",
            "onnx/vision_encoder.onnx",
            "onnx/vision_encoder.onnx_data",
            "onnx/model.onnx",
            "onnx/model.onnx_data",
            "tokenizer.json",
        ] {
            std::fs::write(dir.path().join(f), b"x").unwrap();
        }
        let cfg = GemmaConfig {
            ort_lib: dir.path().join("libonnxruntime.so"),
            model_dir: dir.path().to_path_buf(),
            threads: 1,
            budget: 140,
        };
        assert_eq!(cfg.missing(), None);
        assert_eq!(cfg.with_budget(70).budget, 70);
    }
}
