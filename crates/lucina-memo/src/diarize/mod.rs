//! Speaker diarization on the CPU, in Rust: who spoke when.
//!
//! [`onnx::OnnxDiarizer`] runs pyannote segmentation-3.0 over 10 s windows (local speakers per
//! frame), a speaker-embedding model (WeSpeaker ResNet34 or 3D-Speaker ERes2Net) over each
//! local speaker's audio, average-linkage clustering of the embeddings ([`cluster`]), and
//! turns per cluster (the sherpa-onnx recipe, ported). ONNX Runtime is loaded at run time
//! from `LUCINA_DIARIZE_ORT_LIB`; nothing is downloaded or linked at build time. Model files:
//! `docs/meetings.md` (one-time download).
//!
//! [`profiles`] keeps a voice per named person and matches clusters to them (cosine
//! similarity, kernel MT5); unknown voices that recur are asked about once (MT6).
//!
//! The pipeline sees only the [`Diarizer`] trait, so tests run with a fake.
pub mod cluster;
pub mod fbank;
pub mod memo;
pub mod onnx;
pub mod profiles;
pub mod wav;

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// One stretch of speech by one cluster, seconds from the start of the audio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub start: f64,
    pub end: f64,
    /// Cluster index, `0..centroids.len()`, numbered by first appearance.
    pub speaker: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Diarization {
    pub turns: Vec<Turn>,
    /// Per cluster: the mean voice embedding (unit length).
    pub centroids: Vec<Vec<f32>>,
    /// e.g. `pyannote-segmentation-3.0 + wespeaker_en_voxceleb_resnet34_LM`.
    pub engine: String,
}

impl Diarization {
    /// Seconds each cluster speaks.
    pub fn talk_time(&self) -> Vec<f64> {
        let mut t = vec![0.0; self.centroids.len()];
        for turn in &self.turns {
            if let Some(x) = t.get_mut(turn.speaker) {
                *x += (turn.end - turn.start).max(0.0);
            }
        }
        t
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DiarizeError {
    /// Models or ONNX Runtime missing (not configured, MyBook locked): skip or wait.
    #[error("diarizer unavailable: {0}")]
    Unavailable(String),
    #[error("diarization failed: {0}")]
    Failed(String),
}

/// 16 kHz mono samples in, speaker turns out. Blocking (run it in `spawn_blocking`).
pub trait Diarizer: Send + Sync {
    fn diarize(&self, samples: &[f32]) -> Result<Diarization, DiarizeError>;
    /// One voice embedding (unit length) for a stretch of one speaker's speech.
    fn embed(&self, samples: &[f32]) -> Result<Vec<f32>, DiarizeError>;
    fn label(&self) -> String;
}

pub const SAMPLE_RATE: usize = 16_000;

#[derive(Debug, Clone)]
pub struct DiarizeConfig {
    /// pyannote segmentation-3.0 ONNX (sherpa-onnx export, `model.onnx`).
    pub segmentation: PathBuf,
    /// Speaker embedding ONNX (sherpa-onnx's WeSpeaker / 3D-Speaker exports).
    pub embedding: PathBuf,
    /// `libonnxruntime.so` (1.22 or newer).
    pub ort_lib: PathBuf,
    pub threads: usize,
    /// Hop between segmentation windows (pyannote uses 1 s; 2 s halves the embedding work).
    pub step_sec: f64,
    /// Clusters merge while their average cosine distance is at most this.
    pub cluster_threshold: f32,
    /// A cluster is a known person when its cosine similarity to the profile is at least this.
    pub match_threshold: f32,
    /// An unknown voice heard in this many recordings is asked about (once).
    pub ask_after: u32,
}

impl DiarizeConfig {
    /// None when `LUCINA_DIARIZE=off`.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        if get("LUCINA_DIARIZE").is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "off" | "0" | "false"
            )
        }) {
            return None;
        }
        let home = get("HOME").unwrap_or_else(|| "/home/nez".into());
        let dir =
            get("LUCINA_DIARIZE_MODELS").unwrap_or_else(|| format!("{home}/models/diarization"));
        let num = |k: &str, d: f64| {
            get(k)
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
                .unwrap_or(d)
        };
        Some(Self {
            segmentation: PathBuf::from(get("LUCINA_DIARIZE_SEGMENTATION_MODEL").unwrap_or_else(
                || format!("{dir}/sherpa-onnx-pyannote-segmentation-3-0/model.onnx"),
            )),
            embedding: PathBuf::from(
                get("LUCINA_DIARIZE_EMBEDDING_MODEL")
                    .unwrap_or_else(|| format!("{dir}/wespeaker_en_voxceleb_resnet34_LM.onnx")),
            ),
            ort_lib: PathBuf::from(
                get("LUCINA_DIARIZE_ORT_LIB")
                    .unwrap_or_else(|| format!("{dir}/onnxruntime/lib/libonnxruntime.so")),
            ),
            threads: num("LUCINA_DIARIZE_THREADS", 4.0) as usize,
            step_sec: num("LUCINA_DIARIZE_STEP_SEC", 2.0).clamp(0.5, 10.0),
            cluster_threshold: num("LUCINA_DIARIZE_CLUSTER_THRESHOLD", 0.6).min(2.0) as f32,
            match_threshold: num("LUCINA_VOICE_MATCH_THRESHOLD", 0.6).min(1.0) as f32,
            ask_after: num("LUCINA_VOICE_ASK_AFTER", 2.0) as u32,
        })
    }

    /// The files are there (the models may live on the MyBook).
    pub fn missing(&self) -> Option<PathBuf> {
        [&self.segmentation, &self.embedding, &self.ort_lib]
            .into_iter()
            .find(|p| !p.is_file())
            .cloned()
    }
}

/// Loads the ONNX diarizer (once per process; the ONNX Runtime library cannot be reloaded).
pub fn load(cfg: &DiarizeConfig) -> Result<Arc<dyn Diarizer>, DiarizeError> {
    if let Some(p) = cfg.missing() {
        return Err(DiarizeError::Unavailable(format!(
            "{} not found (docs/meetings.md, model download)",
            p.display()
        )));
    }
    Ok(Arc::new(onnx::OnnxDiarizer::new(cfg)?))
}

/// Cosine similarity of two vectors (0 when either is empty or zero).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut ab, mut aa, mut bb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        ab += (*x as f64) * (*y as f64);
        aa += (*x as f64) * (*x as f64);
        bb += (*y as f64) * (*y as f64);
    }
    if aa <= 0.0 || bb <= 0.0 {
        return 0.0;
    }
    (ab / (aa.sqrt() * bb.sqrt())) as f32
}

/// `v / |v|` (unchanged when zero).
pub fn normalize(v: &mut [f32]) {
    let n = v
        .iter()
        .map(|x| (*x as f64) * (*x as f64))
        .sum::<f64>()
        .sqrt();
    if n > 0.0 {
        for x in v.iter_mut() {
            *x = (*x as f64 / n) as f32;
        }
    }
}

/// The unit-length mean of unit-length copies of `vs`.
pub fn mean_direction(vs: &[&[f32]]) -> Vec<f32> {
    let Some(first) = vs.first() else {
        return Vec::new();
    };
    let dim = first.len();
    let mut acc = vec![0f32; dim];
    for v in vs.iter().filter(|v| v.len() == dim) {
        let mut u = v.to_vec();
        normalize(&mut u);
        for (a, x) in acc.iter_mut().zip(u) {
            *a += x;
        }
    }
    normalize(&mut acc);
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosines() {
        assert!((cosine(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 3.0]).abs() < 1e-6);
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 0.0]), 0.0);
        let m = mean_direction(&[&[1.0, 0.0], &[0.0, 1.0]]);
        assert!((m[0] - m[1]).abs() < 1e-6 && (m[0] * m[0] + m[1] * m[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn config_defaults_and_off() {
        let c = DiarizeConfig::from_lookup(|k| (k == "HOME").then(|| "/h".into())).unwrap();
        assert_eq!(
            c.segmentation,
            PathBuf::from("/h/models/diarization/sherpa-onnx-pyannote-segmentation-3-0/model.onnx")
        );
        assert_eq!(c.match_threshold, 0.6);
        assert!(c.missing().is_some());
        assert!(
            DiarizeConfig::from_lookup(|k| (k == "LUCINA_DIARIZE").then(|| "off".into())).is_none()
        );
    }
}
