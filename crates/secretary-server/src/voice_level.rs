//! The level of a voice recording and the lift of quiet speech (`docs/lucinavoice-protocol.md` section 10.7).
//!
//! The rules are `voice_rules.rs` (Verus-checked: integers, frames of 20 ms, a noise gate, a gain of at most +30 dB, a limiter at
//! -1 dBFS). This file is the plain glue around them: s16le bytes in and out, and the dBFS numbers for logs and telemetry. It is
//! shared by path with lucina-server (`/v1/share` voice notes), so it uses `crate::voice_rules` only and nothing else of this
//! crate. Never audio or text in a log: the numbers only.

use crate::voice_rules as rules;

/// More samples than this are not analysed (about 1.7 hours at 16 kHz); the recording goes on unchanged.
pub const MAX_SAMPLES: usize = 100_000_000;
/// The floor of every dBFS number (digital silence).
pub const FLOOR_DBFS: f64 = -120.0;

/// What a recording measures, in dBFS (and the gain chosen for it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Level {
    /// The largest sample.
    pub peak_dbfs: f64,
    /// The RMS of the whole recording.
    pub rms_dbfs: f64,
    /// The RMS of the 20 ms frames above the noise gate (the floor when there are none).
    pub speech_rms_dbfs: f64,
    /// The gain that [`Analysis::normalised`] applies to speech frames, in dB (0 = none).
    pub gain_db: f64,
    /// Nothing to hear: the peak is under -60 dBFS, or no frame is above the gate.
    pub silent: bool,
}

impl Level {
    /// The four numbers for a log line or a record, rounded to 0.1 dB.
    pub fn rounded(&self) -> (f64, f64, f64, f64) {
        let r = |x: f64| (x * 10.0).round() / 10.0;
        (
            r(self.peak_dbfs),
            r(self.rms_dbfs),
            r(self.speech_rms_dbfs),
            r(self.gain_db),
        )
    }
}

/// A recording, measured.
pub struct Analysis {
    pub level: Level,
    plan: rules::Plan,
    frames: Vec<u64>,
    /// The recording was too long to analyse.
    skipped: bool,
}

fn db_amp(x: f64) -> f64 {
    if x <= 0.0 {
        FLOOR_DBFS
    } else {
        (20.0 * (x / 32768.0).log10()).max(FLOOR_DBFS)
    }
}

fn db_power(mean_square: f64) -> f64 {
    if mean_square <= 0.0 {
        FLOOR_DBFS
    } else {
        (10.0 * (mean_square / 1_073_741_824.0).log10()).max(FLOOR_DBFS)
    }
}

/// Measure `samples` (16 kHz mono).
pub fn analyse(samples: &[i16]) -> Analysis {
    if samples.len() > MAX_SAMPLES {
        return Analysis {
            level: Level {
                peak_dbfs: FLOOR_DBFS,
                rms_dbfs: FLOOR_DBFS,
                speech_rms_dbfs: FLOOR_DBFS,
                gain_db: 0.0,
                silent: false,
            },
            plan: rules::Plan {
                silent: false,
                gain_q8: rules::UNITY_Q8,
                gate_ms: 0,
                speech_ms: 0,
            },
            frames: Vec::new(),
            skipped: true,
        };
    }
    let peak = rules::peak_of_range(samples, 0, samples.len());
    let frames = rules::frame_energies(samples);
    let plan = rules::plan(&frames, peak);
    let mean = if frames.is_empty() {
        0.0
    } else {
        frames.iter().map(|&m| m as f64).sum::<f64>() / frames.len() as f64
    };
    let level = Level {
        peak_dbfs: db_amp(f64::from(peak)),
        rms_dbfs: db_power(mean),
        speech_rms_dbfs: db_power(plan.speech_ms as f64),
        gain_db: db_amp(f64::from(plan.gain_q8) * 128.0),
        silent: plan.silent,
    };
    Analysis {
        level,
        plan,
        frames,
        skipped: false,
    }
}

impl Analysis {
    /// The recording with its speech lifted, or `None` when it needs no gain (and for a silent one). Quiet speech is brought
    /// towards -20 dBFS; frames at or under the noise gate are not boosted; nothing passes -1 dBFS. Use it for the speech-to-text
    /// only, never for the stored file.
    pub fn normalised(&self, samples: &[i16]) -> Option<Vec<i16>> {
        if self.skipped || self.plan.silent || self.plan.gain_q8 <= rules::UNITY_Q8 {
            return None;
        }
        if self.frames.len() != samples.len() / rules::FRAME {
            return None;
        }
        Some(rules::apply_plan(
            samples,
            &self.frames,
            self.plan.gate_ms,
            self.plan.gain_q8,
        ))
    }
}

/// s16le bytes to samples (a last odd byte is dropped).
pub fn samples_of(pcm: &[u8]) -> Vec<i16> {
    pcm.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect()
}

/// Samples to s16le bytes.
pub fn pcm_of(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// The samples of a 16-bit PCM WAV file (RIFF chunks walked to `data`; mono as written by ffmpeg `-ac 1`), and its sample rate.
/// `None` for anything else.
pub fn wav_samples(wav: &[u8]) -> Option<(Vec<i16>, u32)> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return None;
    }
    let (mut at, mut rate, mut fmt_ok) = (12usize, 0u32, false);
    while at + 8 <= wav.len() {
        let id = &wav[at..at + 4];
        let size = u32::from_le_bytes([wav[at + 4], wav[at + 5], wav[at + 6], wav[at + 7]]) as usize;
        let body = at + 8;
        if id == b"fmt " && body + 16 <= wav.len() {
            let tag = u16::from_le_bytes([wav[body], wav[body + 1]]);
            let channels = u16::from_le_bytes([wav[body + 2], wav[body + 3]]);
            rate = u32::from_le_bytes([wav[body + 4], wav[body + 5], wav[body + 6], wav[body + 7]]);
            let bits = u16::from_le_bytes([wav[body + 14], wav[body + 15]]);
            fmt_ok = tag == 1 && channels == 1 && bits == 16;
        } else if id == b"data" {
            if !fmt_ok {
                return None;
            }
            // A streamed file may carry a data size of 0 or 0xffffffff: take what is there.
            let end = if size == 0 || size == u32::MAX as usize || body + size > wav.len() {
                wav.len()
            } else {
                body + size
            };
            return Some((samples_of(&wav[body..end]), rate));
        }
        at = body.checked_add(size)?.checked_add(size & 1)?;
    }
    None
}

/// A 16-bit mono PCM WAV file of `samples` at `rate`.
pub fn wav_of(samples: &[i16], rate: u32) -> Vec<u8> {
    let data = samples.len() as u32 * 2;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    out.extend_from_slice(&pcm_of(samples));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16 kHz speech-like bursts (a tone with a slow envelope) at `amp`, with `quiet` of room noise between them.
    fn speech(amp: f64, bursts: usize, noise: i16) -> Vec<i16> {
        let mut out = Vec::new();
        let mut seed: u32 = 12345;
        let mut rnd = |range: i16| -> i16 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            if range == 0 {
                0
            } else {
                ((seed >> 16) as i32 % (2 * i32::from(range) + 1) - i32::from(range)) as i16
            }
        };
        for _ in 0..bursts {
            for i in 0..8000 {
                let env = (std::f64::consts::PI * i as f64 / 8000.0).sin();
                let v = amp * env * (2.0 * std::f64::consts::PI * 300.0 * i as f64 / 16000.0).sin();
                out.push(v as i16 + rnd(noise));
            }
            for _ in 0..8000 {
                out.push(rnd(noise));
            }
        }
        out
    }

    fn speech_rms_dbfs(s: &[i16]) -> f64 {
        analyse(s).level.speech_rms_dbfs
    }

    #[test]
    fn the_dsi_note_is_measured_and_lifted_towards_minus_20() {
        // About -29.6 dBFS peak, -41.5 dBFS overall, like the DSi's note.
        let s = speech(32768.0 * 0.0331, 4, 3);
        let a = analyse(&s);
        assert!(!a.level.silent);
        assert!((a.level.peak_dbfs + 29.6).abs() < 1.5, "{:?}", a.level);
        assert!(a.level.rms_dbfs < -36.0 && a.level.rms_dbfs > -48.0, "{:?}", a.level);
        assert!(a.level.gain_db > 10.0 && a.level.gain_db <= 30.0, "{:?}", a.level);
        let out = a.normalised(&s).expect("a gain");
        assert_eq!(out.len(), s.len());
        let after = speech_rms_dbfs(&out);
        assert!((after + 20.0).abs() < 2.5, "speech rms after: {after}");
        let peak = out.iter().map(|v| i32::from(*v).abs()).max().unwrap();
        assert!(peak <= rules::CEILING, "peak {peak}");
    }

    #[test]
    fn the_gain_never_passes_30_db() {
        // A very quiet but not silent recording (peak -55 dBFS).
        let s = speech(32768.0 * 0.0018, 4, 0);
        let a = analyse(&s);
        assert!(!a.level.silent, "{:?}", a.level);
        assert!(a.level.gain_db <= 30.0, "{:?}", a.level);
        assert!(a.level.gain_db > 25.0, "{:?}", a.level);
    }

    #[test]
    fn loud_speech_and_speech_within_3_db_get_no_gain() {
        let loud = speech(32768.0 * 0.5, 3, 5);
        let a = analyse(&loud);
        assert_eq!(a.level.gain_db, 0.0);
        assert!(a.normalised(&loud).is_none());
        // Speech RMS near -22 dBFS: within 3 dB of the target.
        let near = speech(32768.0 * 0.18, 3, 5);
        let a = analyse(&near);
        assert!(a.level.speech_rms_dbfs > -23.5 && a.level.speech_rms_dbfs < -20.0, "{:?}", a.level);
        assert_eq!(a.level.gain_db, 0.0);
        assert!(a.normalised(&near).is_none());
    }

    #[test]
    fn silence_is_silent_and_gets_no_gain() {
        for s in [vec![0i16; 16000], vec![0i16; 100], Vec::new()] {
            let a = analyse(&s);
            assert!(a.level.silent);
            assert_eq!(a.level.gain_db, 0.0);
            assert!(a.normalised(&s).is_none());
        }
        // A hiss at -70 dBFS: peak under -60.
        let hiss = speech(0.0, 2, 8);
        assert!(analyse(&hiss).level.silent);
        // Constant noise with no speech above its own floor.
        let noise = speech(0.0, 2, 200);
        assert!(analyse(&noise).level.silent, "{:?}", analyse(&noise).level);
    }

    #[test]
    fn room_noise_between_the_words_is_not_boosted() {
        let s = speech(32768.0 * 0.0331, 4, 6);
        let a = analyse(&s);
        let out = a.normalised(&s).expect("a gain");
        // The noise-only frames (second half of each burst period) keep their size.
        let gap = |v: &[i16]| {
            let part: Vec<f64> = (0..4).flat_map(|b| &v[b * 16000 + 8500..b * 16000 + 15500]).map(|&x| f64::from(x)).collect();
            (part.iter().map(|x| x * x).sum::<f64>() / part.len() as f64).sqrt()
        };
        let (before, after) = (gap(&s), gap(&out));
        assert!(after < before * 1.5, "noise {before} -> {after}");
    }

    #[test]
    fn a_loud_peak_in_a_quiet_recording_is_limited_not_clipped() {
        let mut s = speech(32768.0 * 0.0331, 4, 3);
        s[4000] = 32767; // a click
        s[4001] = -32768;
        let a = analyse(&s);
        if let Some(out) = a.normalised(&s) {
            assert!(out.iter().all(|v| i32::from(*v).abs() <= rules::CEILING));
            assert_eq!(out.len(), s.len());
        }
    }

    #[test]
    fn wav_and_pcm_round_trip() {
        let s: Vec<i16> = vec![0, 1, -1, 32767, -32768, 1234];
        assert_eq!(samples_of(&pcm_of(&s)), s);
        let (back, rate) = wav_samples(&wav_of(&s, 16000)).unwrap();
        assert_eq!((back, rate), (s, 16000));
        assert!(wav_samples(b"not a wav").is_none());
        // A header that claims 0 bytes of data (a stream) takes what follows.
        let mut w = wav_of(&[5, 6, 7], 16000);
        w[40..44].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(wav_samples(&w).unwrap().0, vec![5, 6, 7]);
    }

    #[test]
    fn odd_trailing_byte_is_dropped() {
        assert_eq!(samples_of(&[1, 0, 2]), vec![1]);
    }
}
