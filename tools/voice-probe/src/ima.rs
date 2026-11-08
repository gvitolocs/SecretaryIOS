//! IMA-ADPCM decoding (ffmpeg `adpcm_ima_wav` block layout, mono).
//!
//! Each block of `block` bytes: i16 LE first sample, u8 step index (>88 is 88),
//! a zero byte, then `block - 4` bytes of 4-bit codes, low nibble first, one
//! sample each. A block holds `2 * (block - 4) + 1` samples. At the end, a
//! partial block of at least 4 bytes is decoded for the codes it has; under 4
//! bytes it is dropped.

/// The standard IMA step table. The index is clamped to 0..=88, so entries
/// beyond index 88 are never read.
pub const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408,
    449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066,
    2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630,
    9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];

struct State {
    predictor: i32,
    index: u8,
}

impl State {
    /// Decode one 4-bit code like the server (ffmpeg `adpcm_ima_wav`): the change is
    /// `((2 * magnitude + 1) * step) / 8`, the sign is bit 3.
    fn sample(&mut self, code: u8) -> i16 {
        let step = STEP_TABLE[self.index as usize] as i64;
        let mag = (code & 7) as i64;
        let diff = ((2 * mag + 1) * step) / 8;
        let next = if code & 8 != 0 { self.predictor as i64 - diff } else { self.predictor as i64 + diff };
        self.predictor = next.clamp(-32768, 32767) as i32;
        let delta: i8 = match code & 7 {
            4 => 2,
            5 => 4,
            6 => 6,
            7 => 8,
            _ => -1,
        };
        self.index = (self.index as i16 + delta as i16).clamp(0, 88) as u8;
        self.predictor as i16
    }
}

/// Decode one block of `len` bytes (>= 4) starting at `data[pos..]`: first
/// sample, index, zero, then low-first nibbles. Pushes `1 + 2 * (len - 4)`
/// samples.
fn decode_block(data: &[u8], pos: usize, len: usize, out: &mut Vec<i16>) {
    let first = i16::from_le_bytes([data[pos], data[pos + 1]]);
    let index = data[pos + 2].min(88);
    let mut st = State { predictor: first as i32, index };
    out.push(first);
    for b in &data[pos + 4..pos + len] {
        out.push(st.sample(b & 0x0f)); // low nibble first
        out.push(st.sample(b >> 4));
    }
}

/// Decode an IMA-ADPCM payload into s16 samples.
pub fn decode(data: &[u8], block: u16) -> Vec<i16> {
    let block = block as usize;
    assert!(block >= 8);
    let mut out = Vec::new();
    let full = data.len() / block;
    for k in 0..full {
        decode_block(data, k * block, block, &mut out);
    }
    let rest = data.len() % block;
    if rest >= 4 {
        decode_block(data, full * block, rest, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn fixture(name: &str) -> Vec<u8> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
        std::fs::read(&p).unwrap_or_else(|e| panic!("reading {}: {e}", p.display()))
    }

    /// t.wav is `ffmpeg -f lavfi -i "sine=f=440:r=10909:d=1" -ac 1 -c:a adpcm_ima_wav -block_size 256`,
    /// t.raw is `ffmpeg -i t.wav -f s16le t.raw`: the decoder's ground truth.
    #[test]
    fn decodes_ffmpeg_fixture_bit_exactly() {
        let wav = crate::wav::parse(&fixture("t.wav")).expect("fixture WAV");
        assert_eq!(wav.rate, 10909);
        assert_eq!(wav.block, 256);
        let decoded = decode(&wav.data, wav.block);
        let raw = fixture("t.raw");
        assert_eq!(raw.len() % 2, 0);
        let expected: Vec<i16> = raw.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c)).collect();
        assert_eq!(decoded.len(), expected.len());
        for (i, (a, b)) in decoded.iter().zip(&expected).enumerate() {
            assert_eq!(*a, *b, "sample {i} differs");
        }
    }

    #[test]
    fn partial_tail_rules() {
        // An 8-byte block holds 1 + 2 * (8 - 4) = 9 samples.
        let data = [1u8, 0, 5, 0, 0x12, 0x34, 1, 2, 9, 9, 9];
        // Block of 8 plus a 3-byte tail: the tail is dropped.
        assert_eq!(decode(&data, 8).len(), 9);
        // A 4-byte tail decodes 1 + 2 * (4 - 4) = 1 sample.
        let mut data2 = data[..8].to_vec();
        data2.extend_from_slice(&[2u8, 0, 6, 0]);
        assert_eq!(decode(&data2, 8).len(), 10);
        // A 7-byte tail decodes 1 + 2 * 3 = 7 samples.
        data2.extend_from_slice(&[0x56, 0x12, 0x34]);
        assert_eq!(decode(&data2, 8).len(), 16);
    }

    #[test]
    fn index_is_clamped() {
        // Index byte above 88 is taken as 88.
        let mut data = vec![0u8; 8];
        data[2] = 200;
        let out = decode(&data, 8);
        assert_eq!(out.len(), 9);
        // Codes of 0xF from index 88 (adjustment +2): the index must never overflow.
        let mut data = vec![0u8; 8];
        data[2] = 88;
        data[4..].fill(0xff);
        let out = decode(&data, 8);
        assert_eq!(out.len(), 9);
    }
}
