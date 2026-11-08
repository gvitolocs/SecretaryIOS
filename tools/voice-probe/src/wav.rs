//! WAV parsing: PCM s16 mono (any rate) and IMA-ADPCM mono (format tag 0x11).

use anyhow::{anyhow, bail, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Pcm16,
    ImaAdpcm,
}

/// A parsed mono WAV.
#[derive(Debug)]
pub struct Wav {
    pub codec: Codec,
    pub rate: u32,
    /// PCM s16 samples (codec == Pcm16) or the raw `data` chunk bytes (codec == ImaAdpcm).
    pub pcm: Vec<i16>,
    /// Raw `data` chunk bytes (codec == ImaAdpcm).
    pub data: Vec<u8>,
    /// IMA-ADPCM block size in bytes (WAV `nBlockAlign`), 8..=4096 (codec == ImaAdpcm).
    pub block: u16,
}

impl Wav {
    /// Decoded sample count for an IMA-ADPCM payload: full blocks hold
    /// `2 * (block - 4) + 1` samples; a final partial block of at least 4 bytes
    /// contributes `1 + 2 * (rest - 4)`; under 4 bytes it is dropped.
    pub fn adpcm_sample_count(data: &[u8], block: u16) -> u64 {
        let block = block as u64;
        let per_block = 2 * (block - 4) + 1;
        let (full, rest) = (data.len() as u64 / block, data.len() as u64 % block);
        full * per_block + if rest >= 4 { 1 + 2 * (rest - 4) } else { 0 }
    }

    /// IMA-ADPCM samples per full block: `2 * (block - 4) + 1`.
    pub fn adpcm_samples_per_block(block: u16) -> u64 {
        2 * (block as u64 - 4) + 1
    }

    /// Duration in milliseconds (rounded up).
    pub fn duration_ms(&self) -> u64 {
        let samples = if self.codec == Codec::Pcm16 {
            self.pcm.len() as u64
        } else {
            Self::adpcm_sample_count(&self.data, self.block)
        };
        samples * 1000 / self.rate as u64
    }
}

fn u16le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn u32le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn tag(b: &[u8], off: usize) -> [u8; 4] {
    b[off..off + 4].try_into().unwrap()
}

/// Parse a mono WAV. Accepts PCM s16 (tag 1) and IMA-ADPCM (tag 0x11); other
/// chunks (LIST, fact, …) are skipped.
pub fn parse(buf: &[u8]) -> Result<Wav> {
    if buf.len() < 12 || &buf[0..4] != b"RIFF" || &buf[8..12] != b"WAVE" {
        bail!("not a RIFF/WAVE file");
    }
    let mut codec = None;
    let mut rate: u32 = 0;
    let mut block: u16 = 0;
    let mut data: Option<Vec<u8>> = None;

    let mut off = 12;
    while off + 8 <= buf.len() {
        let id = tag(buf, off);
        let size = u32le(buf, off + 4) as usize;
        let body = off + 8;
        if body.saturating_add(size) > buf.len() {
            bail!("chunk {id:?} runs past end of file");
        }
        match &id {
            b"fmt " => {
                if size < 16 {
                    bail!("fmt chunk too short");
                }
                let fmt = u16le(buf, body);
                let channels = u16le(buf, body + 2);
                rate = u32le(buf, body + 4);
                block = u16le(buf, body + 12);
                let bits = u16le(buf, body + 14);
                if channels != 1 {
                    bail!("only mono WAV is supported (got {channels} channels)");
                }
                match fmt {
                    1 => {
                        if bits != 16 {
                            bail!("only 16-bit PCM is supported (got {bits} bits)");
                        }
                        codec = Some(Codec::Pcm16);
                    }
                    0x11 => {
                        if !(8..=4096).contains(&block) {
                            bail!("IMA-ADPCM block {block} outside 8..=4096");
                        }
                        codec = Some(Codec::ImaAdpcm);
                    }
                    other => bail!("unsupported WAV format tag 0x{other:04x}"),
                }
            }
            b"data" => {
                data = Some(buf[body..body + size].to_vec());
            }
            _ => {} // skip LIST, fact, …
        }
        off = body + size + (size & 1); // chunks are word-aligned
    }

    let codec = codec.ok_or_else(|| anyhow!("missing fmt chunk"))?;
    let data = data.ok_or_else(|| anyhow!("missing data chunk"))?;
    if rate == 0 {
        bail!("missing sample rate");
    }

    match codec {
        Codec::Pcm16 => {
            if data.len() % 2 != 0 {
                bail!("PCM data chunk is not an even number of bytes");
            }
            let pcm: Vec<i16> = data.as_chunks::<2>().0.iter().map(|c| i16::from_le_bytes(*c)).collect();
            Ok(Wav { codec, rate, pcm, data: Vec::new(), block: 0 })
        }
        Codec::ImaAdpcm => Ok(Wav { codec, rate, pcm: Vec::new(), data, block }),
    }
}

/// Write `pcm` (s16le mono) as a minimal RIFF/WAVE file at `rate`.
pub fn write_wav(pcm: &[i16], rate: u32) -> Vec<u8> {
    let n = pcm.len() * 2;
    let mut out = Vec::with_capacity(44 + n);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + n as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(n as u32).to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm_wav(rate: u32, samples: &[i16]) -> Vec<u8> {
        write_wav(samples, rate)
    }

    #[test]
    fn parses_plain_pcm() {
        let buf = pcm_wav(44100, &[1, -2, 3, -4]);
        let w = parse(&buf).unwrap();
        assert_eq!(w.codec, Codec::Pcm16);
        assert_eq!(w.rate, 44100);
        assert_eq!(w.pcm, vec![1, -2, 3, -4]);
    }

    #[test]
    fn skips_unknown_chunks() {
        let mut buf = pcm_wav(10909, &[5, 6]);
        // Insert a LIST chunk and a fact chunk between fmt and data.
        let list = b"LIST\x06\x00\x00\x00INFOab".to_vec();
        let fact = b"fact\x04\x00\x00\x00\x04\x00\x00\x00".to_vec();
        let pos = buf.len() - (8 + 2 * 2); // before "data"
        buf.splice(pos..pos, fact.iter().chain(list.iter()).copied());
        let w = parse(&buf).unwrap();
        assert_eq!(w.rate, 10909);
        assert_eq!(w.pcm, vec![5, 6]);
    }

    #[test]
    fn rejects_stereo_and_bad_fmt() {
        // Stereo PCM.
        let mut b = pcm_wav(16000, &[1, 2]);
        b[22] = 2; // channels
        assert!(parse(&b).is_err());
        // 8-bit PCM.
        let mut b = pcm_wav(16000, &[1, 2]);
        b[34] = 8; // bits per sample
        assert!(parse(&b).is_err());
        // Not a WAV.
        assert!(parse(b"not a wav at all").is_err());
    }

    fn adpcm_wav(rate: u32, block: u16, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + 4 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&20u32.to_le_bytes());
        out.extend_from_slice(&0x11u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&((rate * (2 * (block as u32 - 4) + 1)) / block as u32).to_le_bytes());
        out.extend_from_slice(&block.to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&(2 * (block - 4) + 1).to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn parses_ima_adpcm_with_fact_and_list() {
        // Block of 8 bytes: first sample, index, zero, 2 nibble bytes.
        let data = [0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let mut buf = adpcm_wav(10909, 8, &data);
        // Add a fact chunk before data (as ffmpeg does).
        let pos = buf.len() - (8 + 8);
        buf.splice(pos..pos, b"fact\x04\x00\x00\x00\x05\x00\x00\x00".iter().copied());
        let w = parse(&buf).unwrap();
        assert_eq!(w.codec, Codec::ImaAdpcm);
        assert_eq!(w.rate, 10909);
        assert_eq!(w.block, 8);
        assert_eq!(w.data, data);
    }

    #[test]
    fn adpcm_sample_counts() {
        // 256-byte blocks hold 2 * 252 + 1 = 505 samples.
        assert_eq!(Wav::adpcm_samples_per_block(256), 505);
        assert_eq!(Wav::adpcm_sample_count(&vec![0; 512], 256), 1010);
        // Partial tail: 4 extra bytes -> 1 sample; 3 bytes -> dropped.
        assert_eq!(Wav::adpcm_sample_count(&vec![0; 260], 256), 505 + 1);
        assert_eq!(Wav::adpcm_sample_count(&vec![0; 259], 256), 505);
    }
}
