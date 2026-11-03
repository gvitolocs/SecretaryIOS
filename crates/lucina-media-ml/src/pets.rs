//! Pets: YOLOX-s (Apache-2.0, Megvii) finds the cats and dogs in a picture, DINOv2-small (Apache-2.0, Meta) turns
//! each crop into a 384-value vector, so the same animal can be grouped like a person's faces (`pets_store`).
//! Both run on ONNX Runtime, CPU. Picked on 3 Nov 2026 over MegaDescriptor-T-224 (non-commercial): on 92 labelled
//! crops of the owner's 708 camera pet photos DINOv2 reached precision 0.96 and recall 0.96 at cosine 0.58, against
//! 0.63 / 0.76 (`docs/media-library.md`, "Pets").
//!
//! - **Detector** (`yolox_s.onnx`, the YOLOX 0.1.1rc0 release export): input `images`, 1×3×640×640, BGR float
//!   0..255, the picture letterboxed (padding 114); output `output`, 1×8400×85 raw, decoded here on the stride 8,
//!   16 and 32 grids. COCO classes 15 (cat) and 16 (dog), score `obj · cls`, class-wise NMS.
//! - **Embedder** (`dinov2_small.onnx`, onnx-community/dinov2-small): input `pixel_values` 1×3×224×224 RGB, the crop
//!   resized to 256 on its short side (Catmull-Rom) and centre-cropped, ImageNet mean and std; output
//!   `last_hidden_state`, of which the first token (CLS, 384 values) is the vector, L2-normalised here.

use std::path::PathBuf;
use std::sync::Mutex;

use image::imageops::FilterType;
use image::{DynamicImage, RgbImage};
use ort::session::Session;
use ort::value::Tensor;

use crate::runtime::{failed, init_runtime, session, unavailable};
use crate::siglip::SiglipConfig;
use crate::vector::normalize;
use crate::MlError;

/// Stored with every crop and scan record: vectors of different models are never compared.
pub const MODEL: &str = "yolox-s+dinov2-small";
pub const DIM: usize = 384;
/// Cosine at or above which two crops are neighbours: above the best F1 (0.58) of the proving run, since a wrong
/// merge costs the owner more than a split he merges by hand.
pub const MATCH: f32 = 0.62;
/// Crops a new pet needs, counting the one that starts it.
pub const MIN_CROPS: usize = 3;
/// YOLOX's `obj · cls` a cat or a dog needs.
pub const SCORE_MIN: f32 = 0.35;
pub const NMS_IOU: f32 = 0.45;
/// Animals looked at per picture.
pub const MAX_PETS: usize = 6;
/// Crops whose shorter side is below this (in the picture's pixels) are dropped.
pub const MIN_PET_PX: f32 = 40.0;
const SIDE: usize = 640;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Species {
    Cat,
    Dog,
}

impl Species {
    pub fn as_str(self) -> &'static str {
        match self {
            Species::Cat => "cat",
            Species::Dog => "dog",
        }
    }
}

/// One animal: its box as fractions of the picture, YOLOX's score and what it is.
#[derive(Clone, Debug, PartialEq)]
pub struct PetDetection {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
    pub species: Species,
}

/// A detected animal with its unit vector.
#[derive(Clone, Debug)]
pub struct Found {
    pub det: PetDetection,
    pub vec: Vec<f32>,
}

/// Finds animals and turns them into comparable unit vectors.
pub trait PetModel: Send + Sync {
    fn model(&self) -> &str;
    fn dim(&self) -> usize;
    /// The animals of one picture, best first.
    fn detect(&self, img: &RgbImage) -> Result<Vec<PetDetection>, MlError>;
    /// One vector per crop, in order.
    fn embed(&self, crops: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError>;
}

/// The crop of a detection, with a tenth of its size around it, inside the picture. `None` when too small.
pub fn crop_box(d: &PetDetection, w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    let (wf, hf) = (w as f32, h as f32);
    let (bw, bh) = (d.w * wf, d.h * hf);
    if bw.min(bh) < MIN_PET_PX {
        return None;
    }
    let x0 = (d.x * wf - bw * 0.1).max(0.0);
    let y0 = (d.y * hf - bh * 0.1).max(0.0);
    let x1 = (d.x * wf + bw * 1.1).min(wf);
    let y1 = (d.y * hf + bh * 1.1).min(hf);
    Some((x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32))
}

/// The animals of a picture with their unit vectors: detect, crop, embed.
pub fn find_pets(m: &dyn PetModel, img: &RgbImage) -> Result<Vec<Found>, MlError> {
    let mut crops = Vec::new();
    let mut kept = Vec::new();
    for d in m.detect(img)?.into_iter().take(MAX_PETS) {
        if let Some((x, y, w, h)) = crop_box(&d, img.width(), img.height()) {
            crops.push(image::imageops::crop_imm(img, x, y, w, h).to_image());
            kept.push(d);
        }
    }
    if crops.is_empty() {
        return Ok(Vec::new());
    }
    let vecs = m.embed(&crops)?;
    if vecs.len() != kept.len() {
        return Err(failed("the model answered a different number of vectors"));
    }
    Ok(kept
        .into_iter()
        .zip(vecs)
        .map(|(det, vec)| Found { det, vec })
        .collect())
}

/// YOLOX's preprocessing: letterboxed into 640×640 (padding 114), BGR, 0..255, CHW. Returns the scale.
pub fn letterbox(img: &RgbImage) -> (Vec<f32>, f32) {
    let r = (SIDE as f32 / img.width() as f32).min(SIDE as f32 / img.height() as f32);
    let (w, h) = (
        ((img.width() as f32 * r) as u32).max(1),
        ((img.height() as f32 * r) as u32).max(1),
    );
    let small = image::imageops::resize(img, w, h, FilterType::Triangle);
    let plane = SIDE * SIDE;
    let mut v = vec![114.0f32; 3 * plane];
    for (x, y, p) in small.enumerate_pixels() {
        let i = y as usize * SIDE + x as usize;
        v[i] = p[2] as f32;
        v[plane + i] = p[1] as f32;
        v[2 * plane + i] = p[0] as f32;
    }
    (v, r)
}

fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let iw = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
    let ih = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
    let i = iw * ih;
    let u = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - i;
    if u <= 0.0 {
        0.0
    } else {
        i / u
    }
}

/// The raw 8400×85 output to cats and dogs in the picture (`r` from [`letterbox`], `w`×`h` the picture).
pub fn decode(out: &[f32], r: f32, w: u32, h: u32) -> Vec<PetDetection> {
    let mut cands: Vec<(Species, f32, [f32; 4])> = Vec::new();
    let mut k = 0;
    for s in [8usize, 16, 32] {
        let g = SIDE / s;
        for gy in 0..g {
            for gx in 0..g {
                let Some(o) = out.get(k * 85..(k + 1) * 85) else {
                    return Vec::new();
                };
                k += 1;
                for (cls, sp) in [(15usize, Species::Cat), (16, Species::Dog)] {
                    let score = o[4] * o[5 + cls];
                    if score < SCORE_MIN {
                        continue;
                    }
                    let cx = (o[0] + gx as f32) * s as f32;
                    let cy = (o[1] + gy as f32) * s as f32;
                    let bw = o[2].exp() * s as f32;
                    let bh = o[3].exp() * s as f32;
                    cands.push((
                        sp,
                        score,
                        [
                            (cx - bw / 2.0) / r,
                            (cy - bh / 2.0) / r,
                            (cx + bw / 2.0) / r,
                            (cy + bh / 2.0) / r,
                        ],
                    ));
                }
            }
        }
    }
    cands.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut keep: Vec<(Species, f32, [f32; 4])> = Vec::new();
    for c in cands {
        if keep.iter().all(|k| k.0 != c.0 || iou(&k.2, &c.2) < NMS_IOU) {
            keep.push(c);
        }
    }
    let (wf, hf) = (w as f32, h as f32);
    keep.into_iter()
        .filter_map(|(species, score, b)| {
            let x0 = b[0].clamp(0.0, wf);
            let y0 = b[1].clamp(0.0, hf);
            let (x1, y1) = (b[2].clamp(0.0, wf), b[3].clamp(0.0, hf));
            (x1 > x0 && y1 > y0).then(|| PetDetection {
                x: x0 / wf,
                y: y0 / hf,
                w: (x1 - x0) / wf,
                h: (y1 - y0) / hf,
                score,
                species,
            })
        })
        .collect()
}

/// DINOv2's preprocessing: short side to 256, centre 224×224, ImageNet mean and std, CHW.
pub fn dino_input(crop: &RgbImage) -> Vec<f32> {
    let (cw, ch) = crop.dimensions();
    let s = 256.0 / cw.min(ch).max(1) as f32;
    let (rw, rh) = (
        ((cw as f32 * s).round() as u32).max(224),
        ((ch as f32 * s).round() as u32).max(224),
    );
    let resized =
        DynamicImage::ImageRgb8(crop.clone()).resize_exact(rw, rh, FilterType::CatmullRom);
    let centre = resized
        .crop_imm((rw - 224) / 2, (rh - 224) / 2, 224, 224)
        .to_rgb8();
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let n = 224 * 224;
    let mut v = vec![0f32; 3 * n];
    for (x, y, p) in centre.enumerate_pixels() {
        let i = y as usize * 224 + x as usize;
        for c in 0..3 {
            v[c * n + i] = (p[c] as f32 / 255.0 - MEAN[c]) / STD[c];
        }
    }
    v
}

#[derive(Clone, Debug)]
pub struct PetsConfig {
    /// `libonnxruntime.so` (1.22): `LUCINA_MEDIA_ML_ORT_LIB`, the same as smart search.
    pub ort_lib: PathBuf,
    /// The folder with `yolox_s.onnx` and `dinov2_small.onnx`: `LUCINA_MEDIA_PETS_MODEL_DIR`
    /// (default `~/models/pets`).
    pub model_dir: PathBuf,
    /// ONNX Runtime threads per model: `LUCINA_MEDIA_ML_THREADS` (default 3).
    pub threads: usize,
}

impl PetsConfig {
    pub fn from_env() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/nez".into());
        let s = SiglipConfig::from_env();
        Self {
            ort_lib: s.ort_lib,
            model_dir: std::env::var("LUCINA_MEDIA_PETS_MODEL_DIR")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(format!("{home}/models/pets"))),
            threads: s.threads,
        }
    }
    pub fn detector_path(&self) -> PathBuf {
        self.model_dir.join("yolox_s.onnx")
    }
    pub fn embedder_path(&self) -> PathBuf {
        self.model_dir.join("dinov2_small.onnx")
    }
    pub fn missing(&self) -> Option<PathBuf> {
        [
            self.ort_lib.clone(),
            self.detector_path(),
            self.embedder_path(),
        ]
        .into_iter()
        .find(|p| !p.is_file())
    }
}

/// The real models.
pub struct YoloxDino {
    detector: Mutex<Session>,
    embedder: Mutex<Session>,
}

impl YoloxDino {
    pub fn load(cfg: PetsConfig) -> Result<Self, MlError> {
        if let Some(p) = cfg.missing() {
            return Err(unavailable(format!("{} is missing", p.display())));
        }
        init_runtime(&cfg.ort_lib)?;
        Ok(Self {
            detector: Mutex::new(session(&cfg.detector_path(), cfg.threads)?),
            embedder: Mutex::new(session(&cfg.embedder_path(), cfg.threads)?),
        })
    }
}

impl PetModel for YoloxDino {
    fn model(&self) -> &str {
        MODEL
    }
    fn dim(&self) -> usize {
        DIM
    }
    fn detect(&self, img: &RgbImage) -> Result<Vec<PetDetection>, MlError> {
        let (input, r) = letterbox(img);
        let mut s = self.detector.lock().unwrap_or_else(|p| p.into_inner());
        let out = s
            .run(ort::inputs!["images" => Tensor::from_array(([1usize, 3, SIDE, SIDE], input)).map_err(failed)?])
            .map_err(failed)?;
        let (_, data) = out["output"].try_extract_tensor::<f32>().map_err(failed)?;
        Ok(decode(data, r, img.width(), img.height()))
    }
    fn embed(&self, crops: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError> {
        let mut s = self.embedder.lock().unwrap_or_else(|p| p.into_inner());
        let mut out = Vec::with_capacity(crops.len());
        for c in crops {
            let o = s
                .run(ort::inputs!["pixel_values" => Tensor::from_array(([1usize, 3, 224, 224], dino_input(c))).map_err(failed)?])
                .map_err(failed)?;
            let (shape, data) = o["last_hidden_state"]
                .try_extract_tensor::<f32>()
                .map_err(failed)?;
            if shape.len() != 3 || shape[2] as usize != DIM {
                return Err(failed(format!(
                    "unexpected last_hidden_state shape {shape:?}"
                )));
            }
            let mut v = data[..DIM].to_vec();
            if !normalize(&mut v) {
                return Err(failed("a zero vector"));
            }
            out.push(v);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One raw row per anchor; `set` puts a cat at grid cell (gx, gy) of stride 32.
    fn raw() -> Vec<f32> {
        vec![0.0; 8400 * 85]
    }

    #[test]
    fn a_cat_is_decoded_into_picture_fractions() {
        let mut out = raw();
        // Stride 32 starts after 80×80 + 40×40 anchors; cell (10, 5) of the 20×20 grid.
        let k = 80 * 80 + 40 * 40 + 5 * 20 + 10;
        let o = &mut out[k * 85..(k + 1) * 85];
        o[0] = 0.5;
        o[1] = 0.5;
        o[2] = (128.0f32 / 32.0).ln();
        o[3] = (64.0f32 / 32.0).ln();
        o[4] = 0.9;
        o[5 + 15] = 0.9;
        // A 1280×640 picture letterboxed at r = 0.5: box centre (336, 176) in 640 space → (672, 352).
        let d = decode(&out, 0.5, 1280, 640);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].species, Species::Cat);
        assert!(
            (d[0].x - (672.0 - 128.0) / 1280.0).abs() < 1e-3,
            "{:?}",
            d[0]
        );
        assert!((d[0].w - 256.0 / 1280.0).abs() < 1e-3);
        assert!((d[0].score - 0.81).abs() < 1e-4);
        // Below the score, nothing; a person (class 0) is never a pet.
        let mut low = raw();
        low[k * 85 + 4] = 0.5;
        low[k * 85 + 5] = 1.0;
        low[k * 85 + 5 + 15] = 0.4;
        assert!(decode(&low, 0.5, 1280, 640).is_empty());
    }

    #[test]
    fn overlapping_boxes_of_one_kind_are_one_animal() {
        let mut out = raw();
        for (k, score) in [(100usize, 0.9f32), (101, 0.8)] {
            let o = &mut out[k * 85..(k + 1) * 85];
            o[2] = 3.0;
            o[3] = 3.0;
            o[4] = 1.0;
            o[5 + 16] = score;
        }
        let d = decode(&out, 1.0, 640, 640);
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0].species, Species::Dog);
    }

    #[test]
    fn crops_have_a_margin_and_small_animals_are_dropped() {
        let d = PetDetection {
            x: 0.25,
            y: 0.25,
            w: 0.5,
            h: 0.5,
            score: 0.9,
            species: Species::Cat,
        };
        assert_eq!(crop_box(&d, 1000, 800), Some((200, 160, 600, 480)));
        let tiny = PetDetection {
            w: 0.02,
            h: 0.02,
            ..d.clone()
        };
        assert_eq!(crop_box(&tiny, 1000, 800), None);
        let edge = PetDetection {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
            ..d
        };
        assert_eq!(crop_box(&edge, 1000, 800), Some((0, 0, 1000, 800)));
    }

    #[test]
    fn dino_input_is_224_normalised() {
        let img = RgbImage::from_pixel(300, 200, image::Rgb([124, 116, 104]));
        let v = dino_input(&img);
        assert_eq!(v.len(), 3 * 224 * 224);
        // The ImageNet mean colour is about 0 after normalisation.
        assert!(v.iter().all(|x| x.abs() < 0.02), "{}", v[0]);
    }
}
