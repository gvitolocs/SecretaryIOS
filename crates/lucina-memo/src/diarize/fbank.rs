//! Kaldi-compatible log-mel filterbank (80 bins, 25 ms frames every 10 ms, Povey window,
//! pre-emphasis 0.97, DC removal, no dither, `snip_edges`), the input of the WeSpeaker and
//! 3D-Speaker embedding models (as sherpa-onnx computes it).
use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};

pub const MELS: usize = 80;
const FRAME: usize = 400;
const SHIFT: usize = 160;
const NFFT: usize = 512;

pub struct Fbank {
    /// Per mel bin: (first FFT bin, weights).
    banks: Vec<(usize, Vec<f32>)>,
    window: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
}

fn mel(f: f64) -> f64 {
    1127.0 * (1.0 + f / 700.0).ln()
}

impl Default for Fbank {
    fn default() -> Self {
        Self::new()
    }
}

impl Fbank {
    pub fn new() -> Self {
        let (lo, hi) = (mel(20.0), mel(8000.0));
        let delta = (hi - lo) / (MELS as f64 + 1.0);
        let bin_hz = 16_000.0 / NFFT as f64;
        let banks = (0..MELS)
            .map(|b| {
                let left = lo + b as f64 * delta;
                let center = left + delta;
                let right = center + delta;
                let mut first = None;
                let mut w = Vec::new();
                for i in 0..NFFT / 2 {
                    let m = mel(i as f64 * bin_hz);
                    if m > left && m < right {
                        first.get_or_insert(i);
                        w.push(if m <= center {
                            ((m - left) / (center - left)) as f32
                        } else {
                            ((right - m) / (right - center)) as f32
                        });
                    } else if first.is_some() {
                        break;
                    }
                }
                (first.unwrap_or(0), w)
            })
            .collect();
        let window = (0..FRAME)
            .map(|i| {
                let a = 2.0 * std::f64::consts::PI * i as f64 / (FRAME as f64 - 1.0);
                (0.5 - 0.5 * a.cos()).powf(0.85) as f32
            })
            .collect();
        Self {
            banks,
            window,
            fft: RealFftPlanner::<f32>::new().plan_fft_forward(NFFT),
        }
    }

    /// Frames × [`MELS`] log energies of `samples` (already scaled as the model wants).
    pub fn compute(&self, samples: &[f32]) -> Vec<[f32; MELS]> {
        if samples.len() < FRAME {
            return Vec::new();
        }
        let n = 1 + (samples.len() - FRAME) / SHIFT;
        let mut input = self.fft.make_input_vec();
        let mut spectrum = self.fft.make_output_vec();
        let mut out = Vec::with_capacity(n);
        for f in 0..n {
            let frame = &samples[f * SHIFT..f * SHIFT + FRAME];
            let mean = frame.iter().sum::<f32>() / FRAME as f32;
            let mut x: Vec<f32> = frame.iter().map(|s| s - mean).collect();
            for i in (1..FRAME).rev() {
                x[i] -= 0.97 * x[i - 1];
            }
            x[0] -= 0.97 * x[0];
            input.iter_mut().for_each(|v| *v = 0.0);
            for i in 0..FRAME {
                input[i] = x[i] * self.window[i];
            }
            if self.fft.process(&mut input, &mut spectrum).is_err() {
                out.push([f32::EPSILON.ln(); MELS]);
                continue;
            }
            let mut row = [0f32; MELS];
            for (b, (first, w)) in self.banks.iter().enumerate() {
                let e: f32 = w
                    .iter()
                    .enumerate()
                    .map(|(k, wk)| wk * spectrum[first + k].norm_sqr())
                    .sum();
                row[b] = e.max(f32::EPSILON).ln();
            }
            out.push(row);
        }
        out
    }
}

/// Subtracts each bin's mean over time (cepstral mean normalisation).
pub fn subtract_mean(feats: &mut [[f32; MELS]]) {
    if feats.is_empty() {
        return;
    }
    let mut mean = [0f64; MELS];
    for r in feats.iter() {
        for (m, v) in mean.iter_mut().zip(r) {
            *m += *v as f64;
        }
    }
    let n = feats.len() as f64;
    for r in feats.iter_mut() {
        for (v, m) in r.iter_mut().zip(mean) {
            *v -= (m / n) as f32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_and_a_tone_lands_in_its_band() {
        let fb = Fbank::new();
        assert!(fb.compute(&[0.0; 399]).is_empty());
        let tone: Vec<f32> = (0..16_000)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 16_000.0).sin() * 1000.0)
            .collect();
        let f = fb.compute(&tone);
        assert_eq!(f.len(), 1 + (16_000 - 400) / 160);
        // The loudest bin is the one whose centre is near 1 kHz (mel ≈ 1000).
        let row = f[10];
        let best = (0..MELS)
            .max_by(|a, b| row[*a].total_cmp(&row[*b]))
            .unwrap();
        let (lo, hi) = (mel(20.0), mel(8000.0));
        let centre = lo + (best as f64 + 1.0) * (hi - lo) / 81.0;
        assert!((centre - mel(1000.0)).abs() < 40.0, "bin {best}");
        let mut g = f.clone();
        subtract_mean(&mut g);
        let m: f32 = g.iter().map(|r| r[best]).sum::<f32>() / g.len() as f32;
        assert!(m.abs() < 1e-3);
    }
}
