//! The 16 kHz mono 16-bit WAV that `transcribe::decode` writes, as samples in [-1, 1).
use std::path::Path;

use super::DiarizeError;

pub fn read(path: &Path) -> Result<Vec<f32>, DiarizeError> {
    let bytes = std::fs::read(path).map_err(|e| DiarizeError::Failed(format!("{}: {e}", path.display())))?;
    parse(&bytes)
}

pub fn parse(b: &[u8]) -> Result<Vec<f32>, DiarizeError> {
    let bad = |m: &str| DiarizeError::Failed(format!("wav: {m}"));
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(bad("not RIFF/WAVE"));
    }
    let mut at = 12;
    let mut format_ok = false;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let len = u32::from_le_bytes([b[at + 4], b[at + 5], b[at + 6], b[at + 7]]) as usize;
        let body = at + 8;
        let end = body.saturating_add(len).min(b.len());
        match id {
            b"fmt " => {
                if end - body < 16 {
                    return Err(bad("short fmt chunk"));
                }
                let f = &b[body..end];
                let format = u16::from_le_bytes([f[0], f[1]]);
                let channels = u16::from_le_bytes([f[2], f[3]]);
                let rate = u32::from_le_bytes([f[4], f[5], f[6], f[7]]);
                let bits = u16::from_le_bytes([f[14], f[15]]);
                // 1 = PCM; 0xFFFE = extensible (ffmpeg may write it), PCM inside.
                if !(format == 1 || format == 0xFFFE) || channels != 1 || bits != 16 || rate != 16_000 {
                    return Err(bad("expected 16 kHz mono 16-bit PCM"));
                }
                format_ok = true;
            }
            b"data" => {
                if !format_ok {
                    return Err(bad("data before fmt"));
                }
                return Ok(b[body..end]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| i16::from_le_bytes(*c) as f32 / 32768.0)
                    .collect());
            }
            _ => {}
        }
        at = body.saturating_add(len + (len & 1));
    }
    Err(bad("no data chunk"))
}

/// A 16 kHz mono 16-bit WAV of `samples` (tests and fixtures).
pub fn encode(samples: &[f32]) -> Vec<u8> {
    let data = samples.len() * 2;
    let mut o = Vec::with_capacity(44 + data);
    o.extend_from_slice(b"RIFF");
    o.extend_from_slice(&((36 + data) as u32).to_le_bytes());
    o.extend_from_slice(b"WAVEfmt ");
    o.extend_from_slice(&16u32.to_le_bytes());
    o.extend_from_slice(&1u16.to_le_bytes());
    o.extend_from_slice(&1u16.to_le_bytes());
    o.extend_from_slice(&16_000u32.to_le_bytes());
    o.extend_from_slice(&32_000u32.to_le_bytes());
    o.extend_from_slice(&2u16.to_le_bytes());
    o.extend_from_slice(&16u16.to_le_bytes());
    o.extend_from_slice(b"data");
    o.extend_from_slice(&(data as u32).to_le_bytes());
    for s in samples {
        o.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    o
}

#[cfg(test)]
mod tests {
    #[test]
    fn round_trip() {
        let s = [0.0, 0.5, -0.5, 0.25];
        let back = super::parse(&super::encode(&s)).unwrap();
        assert_eq!(back.len(), 4);
        assert!(back.iter().zip(s).all(|(a, b)| (a - b).abs() < 1e-3));
        assert!(super::parse(b"RIFF....WAVEjunk").is_err());
    }
}
