//! Faces: YuNet (MIT) finds them, SFace (Apache-2.0) turns each one into a 128-value vector.
//! Both are the OpenCV zoo exports, on ONNX Runtime, CPU.
//!
//! - **Detector** (`yunet_2023mar.onnx`): input `input`, BGR float 0..255 (no normalisation),
//!   NCHW, **fixed 640×640** (ONNX Runtime refuses any other size, unlike OpenCV's own runtime).
//!   Outputs `cls_*`, `obj_*`, `bbox_*`, `kps_*` for strides 8, 16 and 32, decoded in [`decode`].
//!   A picture is first brought to at most 960 px on its long side (never enlarged). One pass sees
//!   the whole picture shrunk to 640 px (big faces); when the picture is larger than a window,
//!   640×640 windows at its corners see it at full detail (small faces). Detections of windows
//!   that touch a window edge inside the picture are dropped (a cut-off face), the rest are merged
//!   with non-maximum suppression.
//! - **Recogniser** (`sface_2021dec.onnx`): input `data`, RGB float 0..255, 1×3×112×112, the face
//!   aligned to the 112×112 template from YuNet's five landmarks by a least-squares similarity
//!   transform ([`similarity`], [`align`]); output `fc1`, 128 values, L2-normalised here. Two
//!   faces of one person have a cosine of about 0.363 or more (OpenCV's threshold).
//!
//! [`FaceModel`] is the seam the media service and its tests use; [`YuNetSface`] is the real one.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Mutex;

use image::imageops::FilterType;
use image::RgbImage;
use ort::session::Session;
use ort::value::Tensor;

use crate::runtime::{failed, init_runtime, session, unavailable};
use crate::siglip::SiglipConfig;
use crate::vector::normalize;
use crate::MlError;

/// Stored with every face and scan record: faces of different models are never compared.
pub const MODEL: &str = "yunet-2023mar+sface-2021dec";
pub const DIM: usize = 128;
/// YuNet's detection score (`sqrt(cls · obj)`) a face needs.
pub const SCORE_MIN: f32 = 0.85;
pub const NMS_IOU: f32 = 0.3;
/// Faces whose shorter side is below this many pixels (in the picture YuNet looked at) are dropped.
pub const MIN_FACE_PX: f32 = 24.0;
/// The long side a picture is brought down to before detection.
pub const DETECT_LONG: u32 = 960;
/// YuNet's fixed input size.
pub const WINDOW: u32 = 640;
pub const ALIGN_PX: u32 = 112;
/// Most faces kept from one picture.
pub const MAX_FACES: usize = 50;
const STRIDES: [usize; 3] = [8, 16, 32];
/// A window detection this close to a window edge inside the picture is a cut-off face.
const EDGE_PX: f32 = 3.0;

/// SFace's alignment template (right eye, left eye, nose, mouth corners), in 112×112 pixels.
pub const TEMPLATE: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

/// A face in a picture: the box and the five landmarks, as fractions (0..1) of the picture's
/// width and height, as the picture is shown (the renditions are already upright).
#[derive(Clone, Debug, PartialEq)]
pub struct Detection {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
    /// Right eye, left eye, nose, right mouth corner, left mouth corner (as YuNet orders them:
    /// "right" is the person's right, so it is on the viewer's left).
    pub landmarks: [[f32; 2]; 5],
}

/// A detected face with its unit vector.
#[derive(Clone, Debug)]
pub struct Found {
    pub det: Detection,
    pub vec: Vec<f32>,
}

/// Finds faces and turns them into comparable unit vectors.
pub trait FaceModel: Send + Sync {
    /// Stored with every face; faces of different models are never compared.
    fn model(&self) -> &str;
    fn dim(&self) -> usize;
    /// The faces of one picture, best first.
    fn detect(&self, img: &RgbImage) -> Result<Vec<Detection>, MlError>;
    /// One vector per aligned 112×112 face crop, in order.
    fn embed(&self, aligned: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError>;
}

/// The faces of a picture with their unit vectors: detect, align each face, embed.
pub fn find_faces(m: &dyn FaceModel, img: &RgbImage) -> Result<Vec<Found>, MlError> {
    let dets = m.detect(img)?;
    let (w, h) = (img.width() as f32, img.height() as f32);
    let mut crops = Vec::with_capacity(dets.len());
    let mut kept = Vec::with_capacity(dets.len());
    for d in dets.into_iter().take(MAX_FACES) {
        let px = d.landmarks.map(|[x, y]| [x * w, y * h]);
        if let Some(c) = align(img, &px) {
            crops.push(c);
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
        .filter_map(|(det, mut vec)| {
            (vec.len() == m.dim() && normalize(&mut vec)).then_some(Found { det, vec })
        })
        .collect())
}

// ---- decoding

/// A detection in the pixels of the image the network saw.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PxBox {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub score: f32,
    pub lm: [[f32; 2]; 5],
}

impl PxBox {
    fn w(&self) -> f32 {
        self.x2 - self.x1
    }
    fn h(&self) -> f32 {
        self.y2 - self.y1
    }
    fn shifted(mut self, dx: f32, dy: f32) -> Self {
        self.x1 += dx;
        self.x2 += dx;
        self.y1 += dy;
        self.y2 += dy;
        for p in self.lm.iter_mut() {
            p[0] += dx;
            p[1] += dy;
        }
        self
    }
    fn scaled(mut self, kx: f32, ky: f32) -> Self {
        self.x1 *= kx;
        self.x2 *= kx;
        self.y1 *= ky;
        self.y2 *= ky;
        for p in self.lm.iter_mut() {
            p[0] *= kx;
            p[1] *= ky;
        }
        self
    }
}

/// One output scale of YuNet: a grid of `cols` cells per row, one entry per cell.
pub(crate) struct Head<'a> {
    pub stride: usize,
    pub cols: usize,
    pub cls: &'a [f32],
    pub obj: &'a [f32],
    pub bbox: &'a [f32],
    pub kps: &'a [f32],
}

/// Decodes one scale. For cell `i` at row `r`, column `c`: score `sqrt(clamp(cls)·clamp(obj))`;
/// centre `((c + bbox[4i])·s, (r + bbox[4i+1])·s)`; size `(exp(bbox[4i+2])·s, exp(bbox[4i+3])·s)`;
/// landmark `n` at `((kps[10i+2n] + c)·s, (kps[10i+2n+1] + r)·s)`. Cells below `min_score` are skipped.
pub(crate) fn decode(head: &Head, min_score: f32) -> Result<Vec<PxBox>, MlError> {
    let n = head.cls.len();
    if head.cols == 0
        || head.obj.len() != n
        || head.bbox.len() != 4 * n
        || head.kps.len() != 10 * n
        || !n.is_multiple_of(head.cols)
    {
        return Err(failed(format!(
            "unexpected YuNet output sizes at stride {}",
            head.stride
        )));
    }
    let s = head.stride as f32;
    let mut out = Vec::new();
    for i in 0..n {
        let score = (head.cls[i].clamp(0.0, 1.0) * head.obj[i].clamp(0.0, 1.0)).sqrt();
        if score.is_nan() || score < min_score {
            continue;
        }
        let (r, c) = ((i / head.cols) as f32, (i % head.cols) as f32);
        let b = &head.bbox[4 * i..4 * i + 4];
        let (cx, cy) = ((c + b[0]) * s, (r + b[1]) * s);
        let (w, h) = (b[2].exp() * s, b[3].exp() * s);
        let k = &head.kps[10 * i..10 * i + 10];
        let mut lm = [[0f32; 2]; 5];
        for (j, p) in lm.iter_mut().enumerate() {
            *p = [(k[2 * j] + c) * s, (k[2 * j + 1] + r) * s];
        }
        out.push(PxBox {
            x1: cx - w / 2.0,
            y1: cy - h / 2.0,
            x2: cx + w / 2.0,
            y2: cy + h / 2.0,
            score,
            lm,
        });
    }
    Ok(out)
}

fn iou(a: &PxBox, b: &PxBox) -> f32 {
    let iw = (a.x2.min(b.x2) - a.x1.max(b.x1)).max(0.0);
    let ih = (a.y2.min(b.y2) - a.y1.max(b.y1)).max(0.0);
    let inter = iw * ih;
    let union = a.w() * a.h() + b.w() * b.h() - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Non-maximum suppression: best score first; a box overlapping a kept one by more than `thr`
/// (intersection over union) is dropped.
pub(crate) fn nms(mut v: Vec<PxBox>, thr: f32) -> Vec<PxBox> {
    v.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<PxBox> = Vec::new();
    for b in v {
        if kept.iter().all(|k| iou(k, &b) <= thr) {
            kept.push(b);
        }
    }
    kept
}

// ---- looking at a whole picture

/// The offsets of the 640-px windows along a side of `len` px: none to slide when it fits, else
/// both ends (a side barely over 640 gets one centred window; the shrunk pass sees the rest).
fn starts(len: u32) -> Vec<u32> {
    if len <= WINDOW {
        vec![0]
    } else if len - WINDOW < 32 {
        vec![(len - WINDOW) / 2]
    } else {
        vec![0, len - WINDOW]
    }
}

/// Finds the faces of `img` by running `run` (the network, on an image of at most 640×640, boxes
/// in that image's pixels) over the passes described in the module doc.
pub(crate) fn detect_tiled(
    img: &RgbImage,
    run: &mut dyn FnMut(&RgbImage) -> Result<Vec<PxBox>, MlError>,
) -> Result<Vec<Detection>, MlError> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Ok(Vec::new());
    }
    let long = w.max(h);
    let canvas: Cow<RgbImage> = if long > DETECT_LONG {
        let k = DETECT_LONG as f32 / long as f32;
        let nw = ((w as f32 * k).round() as u32).max(1);
        let nh = ((h as f32 * k).round() as u32).max(1);
        Cow::Owned(image::imageops::resize(img, nw, nh, FilterType::Triangle))
    } else {
        Cow::Borrowed(img)
    };
    let (cw, ch) = canvas.dimensions();
    let mut all: Vec<PxBox> = Vec::new();
    if cw.max(ch) <= WINDOW {
        all.extend(run(&canvas)?);
    } else {
        // The whole picture, shrunk to fit one window.
        let k = WINDOW as f32 / cw.max(ch) as f32;
        let small = image::imageops::resize(
            &*canvas,
            ((cw as f32 * k).round() as u32).max(1),
            ((ch as f32 * k).round() as u32).max(1),
            FilterType::Triangle,
        );
        let (kx, ky) = (
            cw as f32 / small.width() as f32,
            ch as f32 / small.height() as f32,
        );
        all.extend(run(&small)?.into_iter().map(|b| b.scaled(kx, ky)));
    }
    if cw > WINDOW || ch > WINDOW {
        let (tw, th) = (cw.min(WINDOW), ch.min(WINDOW));
        for y0 in starts(ch) {
            for x0 in starts(cw) {
                let tile = image::imageops::crop_imm(&*canvas, x0, y0, tw, th).to_image();
                for b in run(&tile)? {
                    let cut = (x0 > 0 && b.x1 < EDGE_PX)
                        || (y0 > 0 && b.y1 < EDGE_PX)
                        || (x0 + tw < cw && b.x2 > tw as f32 - EDGE_PX)
                        || (y0 + th < ch && b.y2 > th as f32 - EDGE_PX);
                    if !cut {
                        all.push(b.shifted(x0 as f32, y0 as f32));
                    }
                }
            }
        }
    }
    all.retain(|b| b.score >= SCORE_MIN && b.w().min(b.h()) >= MIN_FACE_PX);
    let (cwf, chf) = (cw as f32, ch as f32);
    Ok(nms(all, NMS_IOU)
        .into_iter()
        .take(MAX_FACES)
        .map(|b| {
            let (x1, y1) = (b.x1.max(0.0), b.y1.max(0.0));
            let (x2, y2) = (b.x2.min(cwf), b.y2.min(chf));
            Detection {
                x: x1 / cwf,
                y: y1 / chf,
                w: ((x2 - x1) / cwf).max(0.0),
                h: ((y2 - y1) / chf).max(0.0),
                score: b.score,
                landmarks: b
                    .lm
                    .map(|[x, y]| [(x / cwf).clamp(0.0, 1.0), (y / chf).clamp(0.0, 1.0)]),
            }
        })
        .collect())
}

// ---- alignment

/// The least-squares similarity transform (rotation, uniform scale, translation; Umeyama 1991)
/// taking `src` onto `dst`, as `[a, -b, tx, b, a, ty]`: `x' = a·x − b·y + tx`, `y' = b·x + a·y + ty`.
/// `None` when the source points are all the same point.
pub fn similarity(src: &[[f32; 2]; 5], dst: &[[f32; 2]; 5]) -> Option<[f32; 6]> {
    let n = src.len() as f64;
    let mean = |p: &[[f32; 2]; 5]| {
        (
            p.iter().map(|q| q[0] as f64).sum::<f64>() / n,
            p.iter().map(|q| q[1] as f64).sum::<f64>() / n,
        )
    };
    let ((sx, sy), (dx, dy)) = (mean(src), mean(dst));
    let (mut num_a, mut num_b, mut den) = (0f64, 0f64, 0f64);
    for (s, d) in src.iter().zip(dst) {
        let (px, py) = (s[0] as f64 - sx, s[1] as f64 - sy);
        let (qx, qy) = (d[0] as f64 - dx, d[1] as f64 - dy);
        num_a += px * qx + py * qy;
        num_b += px * qy - py * qx;
        den += px * px + py * py;
    }
    if !(den.is_finite() && den > 1e-9) {
        return None;
    }
    let (a, b) = (num_a / den, num_b / den);
    let tx = dx - (a * sx - b * sy);
    let ty = dy - (b * sx + a * sy);
    Some([
        a as f32, -b as f32, tx as f32, b as f32, a as f32, ty as f32,
    ])
}

/// The face cut out of `img` (landmarks in its pixels) as SFace wants it: 112×112, warped with
/// [`similarity`] onto [`TEMPLATE`], bilinear, black outside the picture.
pub fn align(img: &RgbImage, landmarks: &[[f32; 2]; 5]) -> Option<RgbImage> {
    let m = similarity(landmarks, &TEMPLATE)?;
    let (a, b, tx, ty) = (m[0] as f64, -(m[1] as f64), m[2] as f64, m[5] as f64);
    let s2 = a * a + b * b;
    if s2 < 1e-12 {
        return None;
    }
    let (w, h) = (img.width() as i64, img.height() as i64);
    let px = |x: i64, y: i64| -> [f32; 3] {
        if x < 0 || y < 0 || x >= w || y >= h {
            [0.0; 3]
        } else {
            let p = img.get_pixel(x as u32, y as u32);
            [p[0] as f32, p[1] as f32, p[2] as f32]
        }
    };
    let mut out = RgbImage::new(ALIGN_PX, ALIGN_PX);
    for v in 0..ALIGN_PX {
        for u in 0..ALIGN_PX {
            // Inverse of x' = a·x − b·y + tx, y' = b·x + a·y + ty.
            let (du, dv) = (u as f64 - tx, v as f64 - ty);
            let x = (a * du + b * dv) / s2;
            let y = (-b * du + a * dv) / s2;
            let (x0, y0) = (x.floor(), y.floor());
            let (fx, fy) = ((x - x0) as f32, (y - y0) as f32);
            let (x0, y0) = (x0 as i64, y0 as i64);
            let (p00, p10, p01, p11) = (
                px(x0, y0),
                px(x0 + 1, y0),
                px(x0, y0 + 1),
                px(x0 + 1, y0 + 1),
            );
            let mut rgb = [0u8; 3];
            for c in 0..3 {
                let top = p00[c] * (1.0 - fx) + p10[c] * fx;
                let bottom = p01[c] * (1.0 - fx) + p11[c] * fx;
                rgb[c] = (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8;
            }
            out.put_pixel(u, v, image::Rgb(rgb));
        }
    }
    Some(out)
}

// ---- the real models

#[derive(Clone, Debug)]
pub struct FacesConfig {
    /// `libonnxruntime.so` (1.22): `LUCINA_MEDIA_ML_ORT_LIB`, the same as smart search.
    pub ort_lib: PathBuf,
    /// The folder with `yunet_2023mar.onnx` and `sface_2021dec.onnx`: `LUCINA_MEDIA_FACES_MODEL_DIR`.
    pub model_dir: PathBuf,
    /// ONNX Runtime threads per model: `LUCINA_MEDIA_ML_THREADS` (default 3).
    pub threads: usize,
}

impl FacesConfig {
    pub fn from_env() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/nez".into());
        let s = SiglipConfig::from_env();
        Self {
            ort_lib: s.ort_lib,
            model_dir: std::env::var("LUCINA_MEDIA_FACES_MODEL_DIR")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(format!("{home}/models/faces-opencv"))),
            threads: s.threads,
        }
    }

    pub fn detector_path(&self) -> PathBuf {
        self.model_dir.join("yunet_2023mar.onnx")
    }
    pub fn recogniser_path(&self) -> PathBuf {
        self.model_dir.join("sface_2021dec.onnx")
    }

    /// The first file that is missing, if any.
    pub fn missing(&self) -> Option<PathBuf> {
        [
            self.ort_lib.clone(),
            self.detector_path(),
            self.recogniser_path(),
        ]
        .into_iter()
        .find(|p| !p.is_file())
    }
}

pub struct YuNetSface {
    cfg: FacesConfig,
    detector: Mutex<Option<Session>>,
    recogniser: Mutex<Option<Session>>,
}

impl YuNetSface {
    /// Checks the files and the runtime; the networks load on first use.
    pub fn load(cfg: FacesConfig) -> Result<Self, MlError> {
        if let Some(p) = cfg.missing() {
            return Err(unavailable(format!("{} not found", p.display())));
        }
        init_runtime(&cfg.ort_lib)?;
        Ok(Self {
            cfg,
            detector: Mutex::new(None),
            recogniser: Mutex::new(None),
        })
    }

    /// Drops both networks (about 40 MB) until the next picture.
    pub fn release(&self) {
        *self.detector.lock().unwrap_or_else(|p| p.into_inner()) = None;
        *self.recogniser.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// The network's names and shapes, for the ignored real-model test.
    pub fn describe(&self) -> Result<String, MlError> {
        let mut out = String::new();
        for (name, path, slot) in [
            ("YuNet", self.cfg.detector_path(), &self.detector),
            ("SFace", self.cfg.recogniser_path(), &self.recogniser),
        ] {
            let mut g = slot.lock().unwrap_or_else(|p| p.into_inner());
            if g.is_none() {
                *g = Some(session(&path, self.cfg.threads)?);
            }
            let s = g.as_ref().expect("loaded above");
            out.push_str(&format!("{name}:\n"));
            for i in s.inputs.iter() {
                out.push_str(&format!("  in  {} {:?}\n", i.name, i.input_type));
            }
            for o in s.outputs.iter() {
                out.push_str(&format!("  out {} {:?}\n", o.name, o.output_type));
            }
        }
        Ok(out)
    }

    /// One YuNet pass over an image of at most 640×640; boxes in its pixels.
    fn run_detector(&self, img: &RgbImage) -> Result<Vec<PxBox>, MlError> {
        let win = WINDOW as usize;
        let mut data = vec![0f32; 3 * win * win];
        for (x, y, p) in img.enumerate_pixels() {
            let i = y as usize * win + x as usize;
            // BGR, 0..255.
            data[i] = p[2] as f32;
            data[win * win + i] = p[1] as f32;
            data[2 * win * win + i] = p[0] as f32;
        }
        let input = Tensor::from_array(([1usize, 3, win, win], data)).map_err(failed)?;
        let mut guard = self.detector.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            *guard = Some(session(&self.cfg.detector_path(), self.cfg.threads)?);
        }
        let s = guard.as_mut().expect("loaded above");
        let outputs = s.run(ort::inputs!["input" => input]).map_err(failed)?;
        let mut boxes = Vec::new();
        for stride in STRIDES {
            let get = |kind: &str| -> Result<&[f32], MlError> {
                let name = format!("{kind}_{stride}");
                let v = outputs
                    .get(name.as_str())
                    .ok_or_else(|| failed(format!("YuNet has no output {name}")))?;
                let (_, d) = v.try_extract_tensor::<f32>().map_err(failed)?;
                Ok(d)
            };
            let head = Head {
                stride,
                cols: win / stride,
                cls: get("cls")?,
                obj: get("obj")?,
                bbox: get("bbox")?,
                kps: get("kps")?,
            };
            if head.cls.len() != (win / stride) * (win / stride) {
                return Err(failed(format!(
                    "YuNet output at stride {stride} has {} cells",
                    head.cls.len()
                )));
            }
            boxes.extend(decode(&head, SCORE_MIN)?);
        }
        Ok(boxes)
    }
}

impl FaceModel for YuNetSface {
    fn model(&self) -> &str {
        MODEL
    }

    fn dim(&self) -> usize {
        DIM
    }

    fn detect(&self, img: &RgbImage) -> Result<Vec<Detection>, MlError> {
        detect_tiled(img, &mut |tile| self.run_detector(tile))
    }

    fn embed(&self, aligned: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError> {
        let px = ALIGN_PX as usize;
        let mut out = Vec::with_capacity(aligned.len());
        let mut guard = self.recogniser.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            *guard = Some(session(&self.cfg.recogniser_path(), self.cfg.threads)?);
        }
        let s = guard.as_mut().expect("loaded above");
        // The export takes one face at a time (its batch size is fixed at 1).
        for face in aligned {
            if face.dimensions() != (ALIGN_PX, ALIGN_PX) {
                return Err(failed("an aligned face is not 112×112"));
            }
            let mut data = vec![0f32; 3 * px * px];
            for (x, y, p) in face.enumerate_pixels() {
                let i = y as usize * px + x as usize;
                // RGB, 0..255.
                for c in 0..3 {
                    data[c * px * px + i] = p[c] as f32;
                }
            }
            let input = Tensor::from_array(([1usize, 3, px, px], data)).map_err(failed)?;
            let outputs = s.run(ort::inputs!["data" => input]).map_err(failed)?;
            let (shape, v) = outputs["fc1"].try_extract_tensor::<f32>().map_err(failed)?;
            if v.len() != DIM || shape.len() != 2 {
                return Err(failed(format!("unexpected SFace output shape {shape:?}")));
            }
            out.push(v.to_vec());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn zeros(cells: usize) -> (Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) {
        (
            vec![0.0; cells],
            vec![0.0; cells],
            vec![0.0; 4 * cells],
            vec![0.0; 10 * cells],
        )
    }

    #[test]
    fn decodes_one_hot_cell_on_a_stride_8_grid() {
        // A 4×3 grid; the face sits in row 1, column 2 (index 6).
        let (mut cls, mut obj, mut bbox, mut kps) = zeros(12);
        cls[6] = 0.81;
        obj[6] = 1.0; // score = sqrt(0.81 · 1.0) = 0.9
        bbox[24..28].copy_from_slice(&[0.5, 0.25, 4f32.ln(), 5f32.ln()]);
        for n in 0..5 {
            kps[60 + 2 * n] = n as f32 * 0.1;
            kps[60 + 2 * n + 1] = 0.5;
        }
        let head = Head {
            stride: 8,
            cols: 4,
            cls: &cls,
            obj: &obj,
            bbox: &bbox,
            kps: &kps,
        };
        let v = decode(&head, 0.85).unwrap();
        assert_eq!(v.len(), 1);
        let b = &v[0];
        // centre ((2 + 0.5)·8, (1 + 0.25)·8) = (20, 10); size (4·8, 5·8) = (32, 40).
        assert!((b.score - 0.9).abs() < 1e-6);
        assert!(
            (b.x1 - 4.0).abs() < 1e-4 && (b.x2 - 36.0).abs() < 1e-4,
            "{b:?}"
        );
        assert!(
            (b.y1 - -10.0).abs() < 1e-4 && (b.y2 - 30.0).abs() < 1e-4,
            "{b:?}"
        );
        // landmark n: ((0.1·n + 2)·8, (0.5 + 1)·8).
        for n in 0..5 {
            assert!((b.lm[n][0] - (n as f32 * 0.1 + 2.0) * 8.0).abs() < 1e-4);
            assert!((b.lm[n][1] - 12.0).abs() < 1e-4);
        }
        // Raising the bar drops it.
        assert!(decode(&head, 0.95).unwrap().is_empty());
    }

    #[test]
    fn decode_clamps_scores_and_checks_sizes() {
        let (mut cls, mut obj, bbox, kps) = zeros(4);
        cls[0] = 3.0; // clamps to 1
        obj[0] = 1.0;
        cls[1] = -2.0; // clamps to 0
        obj[1] = 1.0;
        cls[2] = f32::NAN;
        obj[2] = 1.0;
        let head = Head {
            stride: 16,
            cols: 2,
            cls: &cls,
            obj: &obj,
            bbox: &bbox,
            kps: &kps,
        };
        let v = decode(&head, 0.5).unwrap();
        assert_eq!(v.len(), 1);
        assert!((v[0].score - 1.0).abs() < 1e-6);
        // exp(0)·16 = 16 wide, centred on cell (0, 0) + 0 → (0, 0).
        assert!((v[0].x1 + 8.0).abs() < 1e-4 && (v[0].x2 - 8.0).abs() < 1e-4);
        let bad = Head {
            stride: 16,
            cols: 2,
            cls: &cls,
            obj: &obj,
            bbox: &bbox[..8],
            kps: &kps,
        };
        assert!(decode(&bad, 0.5).is_err());
        let odd = Head {
            stride: 16,
            cols: 3,
            cls: &cls,
            obj: &obj,
            bbox: &bbox,
            kps: &kps,
        };
        assert!(decode(&odd, 0.5).is_err());
    }

    fn bx(x1: f32, y1: f32, x2: f32, y2: f32, score: f32) -> PxBox {
        PxBox {
            x1,
            y1,
            x2,
            y2,
            score,
            lm: [[(x1 + x2) / 2.0, (y1 + y2) / 2.0]; 5],
        }
    }

    #[test]
    fn nms_keeps_the_best_of_overlapping_boxes() {
        let v = vec![
            bx(0.0, 0.0, 10.0, 10.0, 0.9),
            bx(1.0, 1.0, 11.0, 11.0, 0.95), // IoU with the first ≈ 0.68
            bx(50.0, 50.0, 60.0, 60.0, 0.86),
            bx(8.0, 8.0, 18.0, 18.0, 0.88), // IoU with the best ≈ 0.02: kept
        ];
        let k = nms(v, 0.3);
        let scores: Vec<f32> = k.iter().map(|b| b.score).collect();
        assert_eq!(scores, vec![0.95, 0.88, 0.86]);
    }

    #[test]
    fn similarity_recovers_a_known_transform() {
        // dst = template; src = template mapped by the inverse of (scale 0.5, rotate 30°, shift).
        let (s, th) = (0.5f32, 30f32.to_radians());
        let (a, b) = (s * th.cos(), s * th.sin());
        let (tx, ty) = (10.0f32, -4.0f32);
        // x' = a·x − b·y + tx  maps src → dst; build src from dst by inverting it.
        let s2 = a * a + b * b;
        let mut src = [[0f32; 2]; 5];
        for (i, d) in TEMPLATE.iter().enumerate() {
            let (du, dv) = (d[0] - tx, d[1] - ty);
            src[i] = [(a * du + b * dv) / s2, (-b * du + a * dv) / s2];
        }
        let m = similarity(&src, &TEMPLATE).unwrap();
        assert!((m[0] - a).abs() < 1e-4 && (m[3] - b).abs() < 1e-4, "{m:?}");
        assert!((m[1] + b).abs() < 1e-4 && (m[4] - a).abs() < 1e-4);
        assert!(
            (m[2] - tx).abs() < 1e-2 && (m[5] - ty).abs() < 1e-2,
            "{m:?}"
        );
        for (s, d) in src.iter().zip(&TEMPLATE) {
            let x = m[0] * s[0] + m[1] * s[1] + m[2];
            let y = m[3] * s[0] + m[4] * s[1] + m[5];
            assert!((x - d[0]).abs() < 1e-2 && (y - d[1]).abs() < 1e-2);
        }
        // Points that all coincide have no direction.
        assert!(similarity(&[[3.0, 3.0]; 5], &TEMPLATE).is_none());
    }

    #[test]
    fn align_samples_the_picture_through_the_transform() {
        // A picture whose red is x and green is y; landmarks = the template seen through
        // (scale 2, no rotation, shift (30, 20)): src = 2 · template + (30, 20)... as pixels.
        let mut img = RgbImage::new(256, 256);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = Rgb([x as u8, y as u8, 7]);
        }
        let lm = TEMPLATE.map(|[x, y]| [2.0 * x + 30.0, 2.0 * y + 20.0]);
        let out = align(&img, &lm).unwrap();
        assert_eq!(out.dimensions(), (112, 112));
        // Output pixel (u, v) is the picture at (2u + 30, 2v + 20).
        for (u, v) in [(0u32, 0u32), (56, 56), (100, 40), (111, 111)] {
            let p = out.get_pixel(u, v);
            let (ex, ey) = ((2 * u + 30) as f32, (2 * v + 20) as f32);
            assert!(
                (p[0] as f32 - ex).abs() <= 1.5,
                "x at ({u},{v}): {} vs {ex}",
                p[0]
            );
            assert!(
                (p[1] as f32 - ey).abs() <= 1.5,
                "y at ({u},{v}): {} vs {ey}",
                p[1]
            );
            assert_eq!(p[2], 7);
        }
        // A face half outside the picture is black there.
        let lm = TEMPLATE.map(|[x, y]| [x - 60.0, y]);
        let out = align(&img, &lm).unwrap();
        assert_eq!(out.get_pixel(0, 56).0, [0, 0, 0]);
        assert!(align(&img, &[[5.0, 5.0]; 5]).is_none());
    }

    #[test]
    fn windows_slide_only_over_pictures_bigger_than_one() {
        assert_eq!(starts(300), vec![0]);
        assert_eq!(starts(640), vec![0]);
        assert_eq!(starts(660), vec![10]);
        assert_eq!(starts(960), vec![0, 320]);
    }

    /// A network that "sees" nothing but reports the boxes it is given once per pass, with the
    /// pass's image size recorded.
    fn run_with<'a>(
        sizes: &'a mut Vec<(u32, u32)>,
        boxes: impl Fn(u32, u32) -> Vec<PxBox> + 'a,
    ) -> impl FnMut(&RgbImage) -> Result<Vec<PxBox>, MlError> + 'a {
        move |img| {
            sizes.push(img.dimensions());
            Ok(boxes(img.width(), img.height()))
        }
    }

    #[test]
    fn a_small_picture_is_one_pass_and_boxes_are_fractions() {
        let img = RgbImage::new(400, 200);
        let mut sizes = Vec::new();
        let mut run = run_with(&mut sizes, |_, _| vec![bx(100.0, 50.0, 200.0, 150.0, 0.9)]);
        let d = detect_tiled(&img, &mut run).unwrap();
        drop(run);
        assert_eq!(sizes, vec![(400, 200)]);
        assert_eq!(d.len(), 1);
        assert!((d[0].x - 0.25).abs() < 1e-6 && (d[0].w - 0.25).abs() < 1e-6);
        assert!((d[0].y - 0.25).abs() < 1e-6 && (d[0].h - 0.5).abs() < 1e-6);
        assert!((d[0].landmarks[0][0] - 0.375).abs() < 1e-6);
    }

    #[test]
    fn a_big_picture_gets_a_shrunk_pass_and_corner_windows() {
        // 1600×1200 → 960×720 canvas: one shrunk pass (640×480) and 2×2 windows of 640×640.
        let img = RgbImage::new(1600, 1200);
        let mut sizes = Vec::new();
        let mut run = run_with(&mut sizes, |_, _| Vec::new());
        assert!(detect_tiled(&img, &mut run).unwrap().is_empty());
        drop(run);
        assert_eq!(sizes[0], (640, 480));
        assert_eq!(sizes.len(), 5);
        assert!(sizes[1..].iter().all(|s| *s == (640, 640)));
    }

    #[test]
    fn boxes_are_mapped_back_and_cut_off_faces_dropped() {
        let img = RgbImage::new(1600, 1200); // canvas 960×720
        let mut pass = 0;
        let mut run = |im: &RgbImage| -> Result<Vec<PxBox>, MlError> {
            pass += 1;
            Ok(match pass {
                // The shrunk pass (k = 2/3): a big face, 240×240 px there = 360×360 on the canvas.
                1 => {
                    assert_eq!(im.dimensions(), (640, 480));
                    vec![bx(200.0, 100.0, 440.0, 340.0, 0.97)]
                }
                // Window (x0 0, y0 0): the same face again at full detail (a duplicate), a cut-off
                // piece of another against the inner right edge, and a face too small.
                2 => vec![
                    bx(310.0, 160.0, 600.0, 470.0, 0.9),
                    bx(600.0, 200.0, 640.0, 300.0, 0.99),
                    bx(10.0, 10.0, 30.0, 40.0, 0.99),
                ],
                // Window (x0 320, y0 0): a face fully inside it → canvas x 320 + 100.
                3 => vec![bx(100.0, 100.0, 200.0, 220.0, 0.92)],
                _ => Vec::new(),
            })
        };
        let d = detect_tiled(&img, &mut run).unwrap();
        // Big face (from the shrunk pass: 300..660 × 150..510 on the canvas) wins over its
        // duplicate; the window-3 face lands at x 420..520; the cut-off and the tiny one are gone.
        assert_eq!(d.len(), 2, "{d:?}");
        assert!((d[0].score - 0.97).abs() < 1e-6);
        assert!((d[0].x - 300.0 / 960.0).abs() < 1e-4 && (d[0].w - 360.0 / 960.0).abs() < 1e-4);
        assert!((d[0].y - 150.0 / 720.0).abs() < 1e-4);
        assert!((d[1].x - 420.0 / 960.0).abs() < 1e-4 && (d[1].y - 100.0 / 720.0).abs() < 1e-4);
    }

    /// Finds one face per picture at a fixed place; embeds a crop by its centre pixel.
    struct Fixed;
    impl FaceModel for Fixed {
        fn model(&self) -> &str {
            "fixed"
        }
        fn dim(&self) -> usize {
            3
        }
        fn detect(&self, _img: &RgbImage) -> Result<Vec<Detection>, MlError> {
            Ok(vec![
                Detection {
                    x: 0.3,
                    y: 0.3,
                    w: 0.4,
                    h: 0.4,
                    score: 0.9,
                    landmarks: [
                        [0.4, 0.45],
                        [0.6, 0.45],
                        [0.5, 0.55],
                        [0.43, 0.65],
                        [0.57, 0.65],
                    ],
                },
                // Degenerate landmarks: no alignment, no face.
                Detection {
                    x: 0.0,
                    y: 0.0,
                    w: 0.1,
                    h: 0.1,
                    score: 0.9,
                    landmarks: [[0.05, 0.05]; 5],
                },
            ])
        }
        fn embed(&self, aligned: &[RgbImage]) -> Result<Vec<Vec<f32>>, MlError> {
            Ok(aligned
                .iter()
                .map(|a| {
                    let p = a.get_pixel(56, 56);
                    vec![p[0] as f32, p[1] as f32, p[2] as f32 + 1.0]
                })
                .collect())
        }
    }

    #[test]
    fn find_faces_aligns_embeds_and_normalises() {
        let img = RgbImage::from_pixel(200, 200, Rgb([200, 100, 0]));
        let f = find_faces(&Fixed, &img).unwrap();
        assert_eq!(f.len(), 1);
        let v = &f[0].vec;
        assert_eq!(v.len(), 3);
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-5);
        assert!(v[0] > v[1] && v[1] > v[2]);
    }
}
