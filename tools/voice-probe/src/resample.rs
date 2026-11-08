//! Linear resampling of s16 audio.

/// Output length for `src_len` samples: nearest integer of `src_len * dst / src`.
pub fn out_len(src_len: u64, src_rate: u32, dst_rate: u32) -> u64 {
    (src_len * dst_rate as u64 + src_rate as u64 / 2) / src_rate as u64
}

/// Resample `src` from `src_rate` to `dst_rate` by linear interpolation.
pub fn linear(src: &[i16], src_rate: u32, dst_rate: u32) -> Vec<i16> {
    if src_rate == dst_rate || src.is_empty() {
        return src.to_vec();
    }
    let n = out_len(src.len() as u64, src_rate, dst_rate) as usize;
    let mut out = Vec::with_capacity(n);
    let ratio = src_rate as f64 / dst_rate as f64;
    for i in 0..n {
        let pos = i as f64 * ratio;
        let i0 = pos as usize;
        if i0 + 1 >= src.len() {
            out.push(*src.last().unwrap());
            continue;
        }
        let frac = pos - i0 as f64;
        let s = src[i0] as f64 * (1.0 - frac) + src[i0 + 1] as f64 * frac;
        out.push(s.round() as i16);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_len_rounds() {
        // 1 s at 10909 Hz -> exactly 16000 samples at 16 kHz.
        assert_eq!(out_len(10909, 10909, 16000), 16000);
        // 1 s at 44100 Hz -> 16000.
        assert_eq!(out_len(44100, 44100, 16000), 16000);
        // Same rate: unchanged.
        assert_eq!(out_len(1234, 8000, 8000), 1234);
        // Half: 100 samples at 8000 -> 2000 at 16000.
        assert_eq!(out_len(100, 8000, 16000), 200);
    }

    #[test]
    fn resampled_length() {
        let src: Vec<i16> = (0..10909).map(|i| (i % 1000) as i16).collect();
        let out = linear(&src, 10909, 16000);
        assert_eq!(out.len(), 16000);
        let out = linear(&src, 10909, 10909);
        assert_eq!(out.len(), 10909);
    }

    #[test]
    fn resample_values() {
        // A constant signal stays constant; a step stays at the right level.
        let const_sig = vec![42i16; 100];
        assert_eq!(linear(&const_sig, 1000, 16000), vec![42; 1600]);
        let step: Vec<i16> = (0..200).map(|i| if i < 100 { 0 } else { 1000 }).collect();
        let out = linear(&step, 1000, 1000);
        assert_eq!(out[99], 0);
        assert_eq!(out[100], 1000);
    }
}
