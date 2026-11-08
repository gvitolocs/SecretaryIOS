//! Sample conversion between the I2S peripherals and the wire formats.
//!
//! - Microphone: an INMP441 sends 24-bit samples left-aligned in 32-bit I2S
//!   words, on one of the two slots (L/R pin). The wire format is s16le mono.
//! - Speaker: the server sends s16le mono; a MAX98357A takes stereo frames
//!   (and mixes L+R), so every sample goes out twice.

/// Right shift from an INMP441 word to 16 bits. 16 would be unity; 14 adds
/// 12 dB, since speech at a metre stays far below full scale.
pub const MIC_SHIFT: u32 = 14;

/// One microphone word to a 16-bit sample, saturating.
pub fn mic_sample(word: i32) -> i16 {
    (word >> MIC_SHIFT).clamp(i16::MIN as i32, i16::MAX as i32) as i16
}

/// Which slot (0 or 1) of interleaved stereo words (little-endian `i32`)
/// carries the microphone: the one with more energy. The unused slot reads
/// as zero or noise.
pub fn louder_slot(words: &[u8]) -> usize {
    let mut energy = [0u64; 2];
    for (i, w) in words.as_chunks::<4>().0.iter().enumerate() {
        let s = (i32::from_le_bytes([w[0], w[1], w[2], w[3]]) >> 16) as i64;
        energy[i % 2] += (s * s) as u64;
    }
    usize::from(energy[1] > energy[0])
}

/// Converts interleaved stereo microphone words (`words`, little-endian
/// `i32`) from `slot` into s16le mono in `out`. Returns the bytes written.
pub fn mic_to_s16le(words: &[u8], slot: usize, out: &mut [u8]) -> usize {
    let mut n = 0;
    for frame in words.as_chunks::<8>().0 {
        if n + 2 > out.len() {
            break;
        }
        let at = slot * 4;
        let word = i32::from_le_bytes([frame[at], frame[at + 1], frame[at + 2], frame[at + 3]]);
        out[n..n + 2].copy_from_slice(&mic_sample(word).to_le_bytes());
        n += 2;
    }
    n
}

/// s16le mono to s16le stereo frames (each sample twice). Returns the bytes
/// written; a trailing odd byte of `mono` is ignored.
pub fn mono_to_stereo(mono: &[u8], out: &mut [u8]) -> usize {
    let mut n = 0;
    for s in mono.as_chunks::<2>().0 {
        if n + 4 > out.len() {
            break;
        }
        out[n..n + 2].copy_from_slice(s);
        out[n + 2..n + 4].copy_from_slice(s);
        n += 4;
    }
    n
}

/// Fills `out` with stereo s16le frames of a square wave (beeps), keeping
/// the phase in `phase` across calls. Returns the bytes written.
pub fn tone(freq: u32, rate: u32, amplitude: i16, phase: &mut u32, out: &mut [u8]) -> usize {
    let half = (rate / freq / 2).max(1);
    let mut n = 0;
    while n + 4 <= out.len() {
        let s = if (*phase / half).is_multiple_of(2) {
            amplitude
        } else {
            -amplitude
        };
        out[n..n + 2].copy_from_slice(&s.to_le_bytes());
        out[n + 2..n + 4].copy_from_slice(&s.to_le_bytes());
        *phase = phase.wrapping_add(1);
        n += 4;
    }
    n
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn mic_samples_scale_and_saturate() {
        assert_eq!(mic_sample(0x0001_0000), 4);
        assert_eq!(mic_sample(i32::MAX), i16::MAX);
        assert_eq!(mic_sample(i32::MIN), i16::MIN);
    }

    #[test]
    fn picks_the_slot_with_signal() {
        let bytes = |w: &[i32]| {
            w.iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<std::vec::Vec<u8>>()
        };
        assert_eq!(
            louder_slot(&bytes(&[0, 5 << 20, 0, -(7 << 20), 1, 6 << 20])),
            1
        );
        assert_eq!(louder_slot(&bytes(&[5 << 20, 0, -(7 << 20), 0])), 0);
    }

    #[test]
    fn converts_one_slot_to_mono() {
        let mut words = [0u8; 16];
        words[4..8].copy_from_slice(&(100i32 << MIC_SHIFT).to_le_bytes());
        words[12..16].copy_from_slice(&(-3i32 << MIC_SHIFT).to_le_bytes());
        let mut out = [0u8; 4];
        assert_eq!(mic_to_s16le(&words, 1, &mut out), 4);
        assert_eq!(out, [100, 0, 0xFD, 0xFF]);
        assert_eq!(mic_to_s16le(&words, 0, &mut out), 4);
        assert_eq!(out, [0; 4]);
    }

    #[test]
    fn duplicates_mono_into_stereo() {
        let mut out = [0u8; 8];
        assert_eq!(mono_to_stereo(&[1, 2, 3, 4, 5], &mut out), 8);
        assert_eq!(out, [1, 2, 1, 2, 3, 4, 3, 4]);
        let mut small = [0u8; 6];
        assert_eq!(mono_to_stereo(&[1, 2, 3, 4], &mut small), 4);
    }

    #[test]
    fn tone_alternates_every_half_period() {
        let mut phase = 0;
        let mut out = [0u8; 16];
        // 1 kHz at 4 kHz: two samples high, two low.
        tone(1000, 4000, 100, &mut phase, &mut out);
        let s: [i16; 4] =
            core::array::from_fn(|i| i16::from_le_bytes([out[i * 4], out[i * 4 + 1]]));
        assert_eq!(s, [100, 100, -100, -100]);
        assert_eq!(phase, 4);
    }
}
