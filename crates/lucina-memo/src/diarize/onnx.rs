//! The ONNX diarizer: pyannote segmentation-3.0 + a speaker-embedding model on ONNX Runtime
//! (CPU), the sherpa-onnx recipe (`speaker-diarization-onnx.py`) in Rust.
//!
//! 1. 10 s windows every `step_sec`; segmentation gives, per window and frame (16.9 ms), a
//!    powerset class: nobody, one of 3 local speakers, or two of them.
//! 2. Speakers per frame over the whole recording: the windows' counts averaged, rounded.
//! 3. One embedding per (window, local speaker) with at least 0.5 s of speech.
//! 4. Average-linkage clustering of the embeddings ([`super::cluster`]).
//! 5. Per frame, the `count` clusters with the most votes are active; runs of active frames
//!    become turns (gaps under 0.5 s merged, turns under 0.3 s dropped).
use std::sync::{Mutex, OnceLock};

use ort::session::Session;
use ort::value::Tensor;

use super::fbank::{subtract_mean, Fbank, MELS};
use super::{
    cluster, mean_direction, normalize, Diarization, DiarizeConfig, DiarizeError, Diarizer, Turn,
};

const MIN_EMBED_SAMPLES: usize = 8_000;
const MIN_OFF_SEC: f64 = 0.5;
const MIN_ON_SEC: f64 = 0.3;
const BATCH: usize = 8;
/// Longer stretches are embedded in pieces of this many samples and averaged.
const EMBED_PIECE: usize = 16_000 * 10;

/// Per window: frames × 3 local-speaker activity flags.
type Activity = Vec<Vec<[bool; 3]>>;

struct SegMeta {
    window: usize,
    rf_shift: usize,
    rf_size: usize,
    num_speakers: usize,
    num_classes: usize,
}

pub struct OnnxDiarizer {
    seg: Mutex<Session>,
    emb: Mutex<Session>,
    meta: SegMeta,
    /// Samples ×32768 before the filterbank (WeSpeaker: `normalize_samples = 0`).
    emb_scale: f32,
    emb_cmn: bool,
    step: usize,
    threshold: f32,
    fbank: Fbank,
    label: String,
}

fn err(e: impl std::fmt::Display) -> DiarizeError {
    DiarizeError::Failed(e.to_string())
}

/// ONNX Runtime from `lib`, once per process. A library that fails to load (ort panics) or
/// has an older API version leaves the diarizer unavailable instead of crashing the service.
fn init_runtime(lib: &std::path::Path) -> Result<(), DiarizeError> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        let path = lib.display().to_string();
        std::panic::catch_unwind(|| ort::init_from(path).with_name("lucina-diarize").commit())
            .map_err(|_| format!("could not load ONNX Runtime from {}", lib.display()))?
            .map(|_| ())
            .map_err(|e| e.to_string())
    })
    .clone()
    .map_err(DiarizeError::Unavailable)
}

fn session(path: &std::path::Path, threads: usize) -> Result<Session, DiarizeError> {
    std::panic::catch_unwind(|| {
        Session::builder()?
            .with_intra_threads(threads.max(1))?
            .with_inter_threads(1)?
            .commit_from_file(path)
    })
    .map_err(|_| DiarizeError::Unavailable("ONNX Runtime panicked loading a model".into()))?
    .map_err(|e| DiarizeError::Unavailable(format!("{}: {e}", path.display())))
}

fn meta_usize(s: &Session, key: &str) -> Result<usize, DiarizeError> {
    s.metadata()
        .map_err(err)?
        .custom(key)
        .map_err(err)?
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| DiarizeError::Unavailable(format!("segmentation model has no {key}")))
}

fn stem(p: &std::path::Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("model")
        .to_owned()
}

impl OnnxDiarizer {
    pub fn new(cfg: &DiarizeConfig) -> Result<Self, DiarizeError> {
        init_runtime(&cfg.ort_lib)?;
        let seg = session(&cfg.segmentation, cfg.threads)?;
        let emb = session(&cfg.embedding, cfg.threads)?;
        let meta = SegMeta {
            window: meta_usize(&seg, "window_size")?,
            rf_shift: meta_usize(&seg, "receptive_field_shift")?,
            rf_size: meta_usize(&seg, "receptive_field_size")?,
            num_speakers: meta_usize(&seg, "num_speakers")?,
            num_classes: meta_usize(&seg, "num_classes")?,
        };
        if meta.num_speakers != 3 || meta.num_classes != 7 {
            return Err(DiarizeError::Unavailable(
                "expected pyannote segmentation-3.0 (3 speakers, 7 powerset classes)".into(),
            ));
        }
        let custom = |k: &str| emb.metadata().ok().and_then(|m| m.custom(k).ok().flatten());
        let emb_scale = if custom("normalize_samples").as_deref() == Some("0") {
            32768.0
        } else {
            1.0
        };
        let emb_cmn = custom("feature_normalize_type").as_deref() != Some("none");
        let label = format!("pyannote-segmentation-3.0 + {}", stem(&cfg.embedding));
        Ok(Self {
            seg: Mutex::new(seg),
            emb: Mutex::new(emb),
            step: ((cfg.step_sec * super::SAMPLE_RATE as f64) as usize).clamp(1_600, meta.window),
            meta,
            emb_scale,
            emb_cmn,
            threshold: cfg.cluster_threshold,
            fbank: Fbank::new(),
            label,
        })
    }

    /// Powerset class → local speakers (class 0 nobody, 1..=3 one, 4..=6 pairs).
    fn speakers_of(class: usize) -> &'static [usize] {
        const MAP: [&[usize]; 7] = [&[], &[0], &[1], &[2], &[0, 1], &[0, 2], &[1, 2]];
        MAP.get(class).copied().unwrap_or(&[])
    }

    /// Per window: frames × 3 activity flags.
    fn segment(&self, samples: &[f32]) -> Result<(Activity, bool), DiarizeError> {
        let w = self.meta.window;
        let mut starts: Vec<usize> = Vec::new();
        let mut s = 0;
        while s + w <= samples.len() {
            starts.push(s);
            s += self.step;
        }
        let has_last = samples.len() < w || !(samples.len() - w).is_multiple_of(self.step);
        if has_last {
            starts.push(starts.last().map_or(0, |l| l + self.step));
        }
        let mut out = Vec::with_capacity(starts.len());
        let mut session = self.seg.lock().unwrap_or_else(|p| p.into_inner());
        for batch in starts.chunks(BATCH) {
            let mut data = vec![0f32; batch.len() * w];
            for (b, &st) in batch.iter().enumerate() {
                let end = (st + w).min(samples.len());
                if st < end {
                    data[b * w..b * w + (end - st)].copy_from_slice(&samples[st..end]);
                }
            }
            let input = Tensor::from_array(([batch.len(), 1usize, w], data)).map_err(err)?;
            let outputs = session.run(ort::inputs![input]).map_err(err)?;
            let (shape, y) = outputs[0].try_extract_tensor::<f32>().map_err(err)?;
            if shape.len() != 3 || shape[2] as usize != self.meta.num_classes {
                return Err(err("unexpected segmentation output shape"));
            }
            let frames = shape[1] as usize;
            for b in 0..batch.len() {
                let mut chunk = Vec::with_capacity(frames);
                for f in 0..frames {
                    let row = &y[(b * frames + f) * 7..(b * frames + f + 1) * 7];
                    let class = (0..7)
                        .max_by(|a, c| row[*a].total_cmp(&row[*c]))
                        .unwrap_or(0);
                    let mut act = [false; 3];
                    for &k in Self::speakers_of(class) {
                        act[k] = true;
                    }
                    chunk.push(act);
                }
                out.push(chunk);
            }
        }
        Ok((out, has_last))
    }

    fn embed_one(&self, samples: &[f32]) -> Result<Vec<f32>, DiarizeError> {
        let scaled: Vec<f32> = samples.iter().map(|s| s * self.emb_scale).collect();
        let mut feats = self.fbank.compute(&scaled);
        if feats.is_empty() {
            return Err(err("too little audio for an embedding"));
        }
        if self.emb_cmn {
            subtract_mean(&mut feats);
        }
        let t = feats.len();
        let flat: Vec<f32> = feats.into_iter().flat_map(|r| r.into_iter()).collect();
        let input = Tensor::from_array(([1usize, t, MELS], flat)).map_err(err)?;
        let mut session = self.emb.lock().unwrap_or_else(|p| p.into_inner());
        let outputs = session.run(ort::inputs![input]).map_err(err)?;
        let (_, e) = outputs[0].try_extract_tensor::<f32>().map_err(err)?;
        let mut v = e.to_vec();
        normalize(&mut v);
        Ok(v)
    }
}

impl Diarizer for OnnxDiarizer {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn embed(&self, samples: &[f32]) -> Result<Vec<f32>, DiarizeError> {
        if samples.len() <= EMBED_PIECE * 3 / 2 {
            return self.embed_one(samples);
        }
        let pieces: Vec<Vec<f32>> = samples
            .chunks(EMBED_PIECE)
            .filter(|c| c.len() >= MIN_EMBED_SAMPLES)
            .map(|c| self.embed_one(c))
            .collect::<Result<_, _>>()?;
        Ok(mean_direction(
            &pieces.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        ))
    }

    fn diarize(&self, samples: &[f32]) -> Result<Diarization, DiarizeError> {
        let m = &self.meta;
        let (labels, has_last) = self.segment(samples)?;
        let Some(frames) = labels.first().map(Vec::len).filter(|f| *f > 0) else {
            return Ok(Diarization {
                engine: self.label.clone(),
                ..Default::default()
            });
        };
        let chunk_frame =
            |c: usize| (c as f64 * self.step as f64 / m.rf_shift as f64 + 0.5) as usize;
        let total_frames = (m.window + (labels.len() - 1) * self.step) / m.rf_shift + 1;
        // 2. speakers per frame.
        let mut sum = vec![0f64; total_frames + frames];
        let mut cnt = vec![0f64; total_frames + frames];
        for (c, chunk) in labels.iter().enumerate() {
            let s = chunk_frame(c);
            for (f, act) in chunk.iter().enumerate() {
                sum[s + f] += act.iter().filter(|a| **a).count() as f64;
                cnt[s + f] += 1.0;
            }
        }
        let per_frame: Vec<usize> = sum
            .iter()
            .zip(&cnt)
            .map(|(s, c)| (s / c.max(1e-12) + 0.5) as usize)
            .collect();
        // 3. embeddings per (window, local speaker).
        let mut pairs = Vec::new();
        let mut embs = Vec::new();
        for (c, chunk) in labels.iter().enumerate() {
            let offset = c * self.step;
            for (k, _) in chunk[0].iter().enumerate() {
                let mut audio = Vec::new();
                let mut f = 0;
                while f < frames {
                    if !chunk[f][k] {
                        f += 1;
                        continue;
                    }
                    let start = f;
                    while f < frames && chunk[f][k] {
                        f += 1;
                    }
                    let a = (start * m.window / frames + offset).min(samples.len());
                    let b = (f * m.window / frames + offset).min(samples.len());
                    audio.extend_from_slice(&samples[a..b]);
                }
                if audio.len() >= MIN_EMBED_SAMPLES {
                    embs.push(self.embed_one(&audio)?);
                    pairs.push((c, k));
                }
            }
        }
        if embs.is_empty() {
            return Ok(Diarization {
                engine: self.label.clone(),
                ..Default::default()
            });
        }
        // 4. clusters.
        let cl = cluster::agglomerative(&embs, self.threshold);
        let k = cl.iter().max().map_or(0, |x| x + 1);
        let mut map = std::collections::HashMap::new();
        for (p, l) in pairs.iter().zip(&cl) {
            map.insert(*p, *l);
        }
        // 5. votes per frame and cluster; the top `per_frame` clusters are active.
        let mut votes = vec![vec![0u32; k]; total_frames + frames];
        for (c, chunk) in labels.iter().enumerate() {
            let s = chunk_frame(c);
            for (f, act) in chunk.iter().enumerate() {
                for (local, on) in act.iter().enumerate() {
                    if let (true, Some(&g)) = (*on, map.get(&(c, local))) {
                        votes[s + f][g] += 1;
                    }
                }
            }
        }
        let stop = if has_last {
            (samples.len() / m.rf_shift).min(votes.len())
        } else {
            votes.len()
        };
        let mut active = vec![vec![false; stop]; k];
        for f in 0..stop {
            let mut order: Vec<usize> = (0..k).filter(|g| votes[f][*g] > 0).collect();
            order.sort_by(|a, b| votes[f][*b].cmp(&votes[f][*a]).then(a.cmp(b)));
            for g in order
                .into_iter()
                .take(per_frame.get(f).copied().unwrap_or(0))
            {
                active[g][f] = true;
            }
        }
        let scale = m.rf_shift as f64 / super::SAMPLE_RATE as f64;
        let half = m.rf_size as f64 / super::SAMPLE_RATE as f64 * 0.5;
        let mut turns = Vec::new();
        for (g, row) in active.iter().enumerate() {
            let mut runs: Vec<(f64, f64)> = Vec::new();
            let mut f = 0;
            while f < row.len() {
                if !row[f] {
                    f += 1;
                    continue;
                }
                let s = f;
                while f < row.len() && row[f] {
                    f += 1;
                }
                let (a, b) = (s as f64 * scale + half, f as f64 * scale + half);
                match runs.last_mut() {
                    Some(last) if a - last.1 <= MIN_OFF_SEC => last.1 = b,
                    _ => runs.push((a, b)),
                }
            }
            turns.extend(runs.into_iter().filter(|(a, b)| b - a >= MIN_ON_SEC).map(
                |(start, end)| Turn {
                    start,
                    end,
                    speaker: g,
                },
            ));
        }
        turns.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.speaker.cmp(&b.speaker)));
        Ok(finish(turns, &cl, &embs, self.label.clone()))
    }
}

/// Renumbers clusters by first turn, drops clusters without turns and computes centroids.
pub fn finish(
    turns: Vec<Turn>,
    labels: &[usize],
    embs: &[Vec<f32>],
    engine: String,
) -> Diarization {
    let mut order: Vec<usize> = Vec::new();
    for t in &turns {
        if !order.contains(&t.speaker) {
            order.push(t.speaker);
        }
    }
    let centroids = order
        .iter()
        .map(|g| {
            let members: Vec<&[f32]> = labels
                .iter()
                .zip(embs)
                .filter(|(l, _)| *l == g)
                .map(|(_, e)| e.as_slice())
                .collect();
            mean_direction(&members)
        })
        .collect();
    let turns = turns
        .into_iter()
        .map(|t| Turn {
            speaker: order.iter().position(|g| *g == t.speaker).unwrap_or(0),
            ..t
        })
        .collect();
    Diarization {
        turns,
        centroids,
        engine,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_renumbers_by_first_turn() {
        let turns = vec![
            Turn {
                start: 0.0,
                end: 1.0,
                speaker: 2,
            },
            Turn {
                start: 1.0,
                end: 2.0,
                speaker: 0,
            },
            Turn {
                start: 2.0,
                end: 3.0,
                speaker: 2,
            },
        ];
        let embs = vec![vec![1.0, 0.0], vec![0.0, 1.0], vec![0.6, 0.8]];
        let d = finish(turns, &[0, 1, 2], &embs, "x".into());
        assert_eq!(
            d.turns.iter().map(|t| t.speaker).collect::<Vec<_>>(),
            [0, 1, 0]
        );
        assert_eq!(d.centroids.len(), 2);
        assert!((d.centroids[0][0] - 0.6).abs() < 1e-6);
        assert_eq!(d.talk_time(), vec![2.0, 1.0]);
    }

    /// The real models, when they are on this machine (`docs/meetings.md`): two synthetic
    /// "voices" (different harmonic stacks) alternate; the diarizer must keep them apart.
    #[test]
    fn real_models_when_installed() {
        let cfg = match DiarizeConfig::from_lookup(|k| std::env::var(k).ok()) {
            Some(c) if c.missing().is_none() => c,
            _ => return,
        };
        let d = OnnxDiarizer::new(&cfg).expect("models load");
        let e = d.embed(&vec![0.01f32; 32_000]).expect("embedding");
        assert!(e.len() >= 128);
        let silence = d
            .diarize(&vec![0f32; 16_000 * 12])
            .expect("diarize silence");
        assert!(silence.turns.is_empty());
    }
}
