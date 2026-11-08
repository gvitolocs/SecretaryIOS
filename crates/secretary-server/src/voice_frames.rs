//! The pure part of the LucinaVoice socket's streaming input (`docs/lucinavoice-protocol.md`
//! section 10): the IMA-ADPCM block decoder, the streaming block assembler, the streaming
//! resampler to 16 kHz, the WAV headers, the validation of `start`'s keys, and the phase
//! machine of one connection's turn.
//!
//! No I/O, no async, no external crates, no panics. The rules that must hold whatever the client
//! sends (the ADPCM step, the block maths, the assembler, the resampler, the byte cap and the phase
//! machine) are in [`crate::voice_rules`], which Verus checks (`scripts/verify-shipped.sh`); they are
//! re-exported here. What is left is the WAV headers and the validation of `start`.
//! `voice_ws.rs` drives it; `voice_note.rs` builds the note's file with the headers.

/// `rate` of `ima-adpcm`: 8000 to 22050.
pub const MAX_RATE: u32 = 22_050;
/// A command keeps section 2's 1 MiB of decoded 16 kHz audio.
pub const COMMAND_CAP: usize = 1024 * 1024;
/// A note keeps at most 10 minutes of decoded 16 kHz audio (bytes of s16le).
pub const NOTE_CAP: usize = 10 * 60 * 16_000 * 2;
/// The longest recording the server transcribes itself for a note: 120 s (bytes of s16le).
pub const NOTE_STT_MAX: usize = 120 * 16_000 * 2;

pub use crate::voice_rules::{
    audio_room, clamp_index, clamp_sample, decode_block, decode_block_range, expand, index_delta,
    partial_samples, samples_per_block, step, total_samples, Action, AdpcmDecoder, Event, Phase,
    Resampler, Step, MAX_BLOCK, MAX_RESAMPLE_RATE, MIN_BLOCK, MIN_RATE, OUT_RATE, STEP_TABLE,
};

fn put_u16(h: &mut Vec<u8>, v: u16) {
    h.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(h: &mut Vec<u8>, v: u32) {
    h.extend_from_slice(&v.to_le_bytes());
}

/// The 44-byte header of a mono s16le WAV of `data_len` bytes at `rate`.
pub fn wav_pcm16_header(data_len: u32, rate: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    put_u32(&mut h, data_len.saturating_add(36));
    h.extend_from_slice(b"WAVEfmt ");
    put_u32(&mut h, 16);
    put_u16(&mut h, 1); // PCM
    put_u16(&mut h, 1); // mono
    put_u32(&mut h, rate);
    put_u32(&mut h, rate.saturating_mul(2)); // byte rate
    put_u16(&mut h, 2); // block align
    put_u16(&mut h, 16); // bits per sample
    h.extend_from_slice(b"data");
    put_u32(&mut h, data_len);
    h
}

/// The 60-byte header of a mono IMA-ADPCM WAV (the layout Fortuna writes): `RIFF`, `fmt ` of 20
/// bytes (tag 0x0011, 1 channel, `rate`, byte rate `rate * block / samples_per_block` rounded
/// down, block align, 4 bits, cbSize 2, samples per block), `fact` (the samples in the data),
/// `data`. A `data_len` that is odd is followed by one pad byte in the file, which the RIFF size
/// counts.
#[allow(clippy::manual_checked_ops)]
pub fn wav_adpcm_header(rate: u32, block: u32, data_len: u32) -> Vec<u8> {
    let spb = samples_per_block(block as usize) as u64;
    let byte_rate = if spb == 0 {
        0
    } else {
        (rate as u64 * block as u64 / spb) as u32
    };
    let samples = total_samples(data_len as usize, block as usize);
    let samples = if samples > u32::MAX as usize {
        u32::MAX
    } else {
        samples as u32
    };
    let pad = data_len & 1;
    let mut h = Vec::with_capacity(60);
    h.extend_from_slice(b"RIFF");
    put_u32(&mut h, data_len.saturating_add(pad).saturating_add(52));
    h.extend_from_slice(b"WAVEfmt ");
    put_u32(&mut h, 20);
    put_u16(&mut h, 0x0011);
    put_u16(&mut h, 1);
    put_u32(&mut h, rate);
    put_u32(&mut h, byte_rate);
    put_u16(&mut h, if block > 0xFFFF { 0xFFFF } else { block as u16 });
    put_u16(&mut h, 4);
    put_u16(&mut h, 2);
    put_u16(&mut h, if spb > 0xFFFF { 0xFFFF } else { spb as u16 });
    h.extend_from_slice(b"fact");
    put_u32(&mut h, 4);
    put_u32(&mut h, samples);
    h.extend_from_slice(b"data");
    put_u32(&mut h, data_len);
    h
}

// -- start ---------------------------------------------------------------------------------------

/// The keys of a `start` message as the socket read them (a missing or `null` key is `None`).
#[derive(Debug, Clone, Copy, Default)] //@omit
pub struct StartRaw<'a> {
    pub mode: Option<&'a str>,
    pub purpose: Option<&'a str>,
    pub codec: Option<&'a str>,
    pub rate: Option<u32>,
    pub block: Option<u32>,
    pub thread: Option<&'a str>,
    pub channel: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub reply_to: Option<&'a str>,
    pub source: Option<&'a str>,
    pub text: Option<bool>,
    pub speak: Option<bool>,
}

/// A validated `start` (protocol section 10.1).
#[derive(Debug, Clone, PartialEq, Eq)] //@omit
pub struct StartOpts {
    /// `mode: "vad"`.
    pub vad: bool,
    /// `purpose: "note"`.
    pub note: bool,
    /// `codec: "ima-adpcm"`.
    pub adpcm: bool,
    /// The client's sample rate (16000 for `pcm16`).
    pub rate: u32,
    /// `ima-adpcm`: the block size in bytes; 0 for `pcm16`.
    pub block: u32,
    /// `thread: "main"` (always for a note).
    pub main: bool,
    /// `ios` or `web`, when given; the upgrade's header wins over it.
    pub channel: Option<String>,
    pub session_id: Option<String>,
    pub reply_to: Option<String>,
    pub source: String,
    /// Command only: send `delta` and `answer`.
    pub text: bool,
    /// Command only: speak the reply.
    pub speak: bool,
}

fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

fn is_source_byte(b: u8) -> bool {
    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
}

fn bytes_ok(s: &str, min: usize, max: usize, source: bool) -> bool {
    let b = s.as_bytes();
    if b.len() < min || b.len() > max {
        return false;
    }
    let mut i = 0;
    while i < b.len() {
        let ok = if source {
            is_source_byte(b[i])
        } else {
            is_id_byte(b[i])
        };
        if !ok {
            return false;
        }
        i += 1;
    }
    true
}

/// `[A-Za-z0-9_-]{min,max}`.
pub fn id_ok(s: &str, min: usize, max: usize) -> bool {
    bytes_ok(s, min, max, false)
}

/// `[a-z0-9-]{1,32}`.
pub fn source_ok(s: &str) -> bool {
    bytes_ok(s, 1, 32, true)
}

/// Check the keys of a `start` against the table of section 10.1. The error is the message of
/// the `bad_request` the socket sends.
pub fn validate_start(r: &StartRaw) -> Result<StartOpts, &'static str> {
    let vad = match r.mode {
        None | Some("ptt") => false,
        Some("vad") => true,
        Some(_) => return Err("mode: ptt or vad"),
    };
    let note = match r.purpose {
        None | Some("command") => false,
        Some("note") => true,
        Some(_) => return Err("purpose: command or note"),
    };
    let adpcm = match r.codec {
        None | Some("pcm16") => false,
        Some("ima-adpcm") => true,
        Some(_) => return Err("codec: pcm16 or ima-adpcm"),
    };
    let rate = r.rate.unwrap_or(OUT_RATE);
    let block;
    if adpcm {
        if !(MIN_RATE..=MAX_RATE).contains(&rate) {
            return Err("rate: 8000 to 22050 for ima-adpcm");
        }
        match r.block {
            Some(b) if (MIN_BLOCK..=MAX_BLOCK).contains(&b) => block = b,
            _ => return Err("block: 8 to 4096 for ima-adpcm"),
        }
    } else {
        if rate != OUT_RATE {
            return Err("rate: 16000 for pcm16");
        }
        block = 0;
    }
    let main = match r.thread {
        None => note,
        Some("voice") => {
            if note {
                return Err("thread: a note goes to the main chat");
            }
            false
        }
        Some("main") => true,
        Some(_) => return Err("thread: voice or main"),
    };
    if vad && note {
        return Err("mode: a note ends when you let go, not on a pause");
    }
    let channel = match r.channel {
        None => None,
        Some("ios") => Some("ios".to_owned()),
        Some("web") => Some("web".to_owned()),
        Some(_) => return Err("channel: ios or web"),
    };
    let session_id = match r.session_id {
        None | Some("") => None,
        Some(s) if id_ok(s, 1, 64) => Some(s.to_owned()),
        Some(_) => return Err("sessionId: up to 64 of A-Z a-z 0-9 _ -"),
    };
    let reply_to = match r.reply_to {
        None => None,
        Some(s) if id_ok(s, 1, 64) => Some(s.to_owned()),
        Some(_) => return Err("replyTo: 1 to 64 of A-Z a-z 0-9 _ -"),
    };
    let source = match r.source {
        None => "voice-ws".to_owned(),
        Some(s) if source_ok(s) => s.to_owned(),
        Some(_) => return Err("source: 1 to 32 of a-z 0-9 -"),
    };
    let speak = r.speak.unwrap_or(true);
    let text = r.text.unwrap_or(false) || !speak;
    Ok(StartOpts {
        vad,
        note,
        adpcm,
        rate,
        block,
        main,
        channel,
        session_id,
        reply_to,
        source,
        text,
        speak,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WAV256: &[u8] = include_bytes!("../tests/fixtures/voice/ima256_10909.wav");
    const PCM256: &[u8] = include_bytes!("../tests/fixtures/voice/ima256_10909.s16le");
    const WAV128: &[u8] = include_bytes!("../tests/fixtures/voice/ima128_8000.wav");
    const PCM128: &[u8] = include_bytes!("../tests/fixtures/voice/ima128_8000.s16le");

    /// The `data` chunk of an ffmpeg `-bitexact` WAV: `fmt `, `fact`, `data` at 60.
    fn data_of(wav: &[u8]) -> &[u8] {
        assert_eq!(&wav[52..56], b"data");
        &wav[60..]
    }

    fn samples_of(pcm: &[u8]) -> Vec<i16> {
        pcm.chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    fn decode_all(data: &[u8], block: u32, cuts: &[usize]) -> Vec<i16> {
        let mut d = AdpcmDecoder::new(block);
        let mut out = Vec::new();
        let mut at = 0;
        for &c in cuts {
            let end = (at + c).min(data.len());
            d.push(&data[at..end], &mut out);
            at = end;
            assert!(d.buffered() < block as usize);
        }
        d.push(&data[at..], &mut out);
        assert!(d.buffered() < block as usize);
        d.finish(&mut out);
        out
    }

    #[test]
    fn decoder_is_bit_exact_with_ffmpeg() {
        let expect = samples_of(PCM256);
        assert_eq!(expect.len(), 33 * 505);
        let got = decode_all(data_of(WAV256), 256, &[]);
        assert_eq!(got, expect);
        // The clip is not silence: the comparison means something.
        assert!(got.iter().filter(|s| i32::from(**s).abs() > 1000).count() > 5000);

        let expect = samples_of(PCM128);
        let got = decode_all(data_of(WAV128), 128, &[]);
        assert_eq!(got.len(), expect.len());
        assert_eq!(got, expect);
    }

    #[test]
    fn frames_cut_anywhere_decode_the_same() {
        let data = data_of(WAV256);
        let whole = decode_all(data, 256, &[]);
        // One cut at every offset of the first two blocks and around the end, then odd sizes.
        for cut in (0..=520).chain(data.len() - 3..=data.len()) {
            assert_eq!(decode_all(data, 256, &[cut]), whole, "cut {cut}");
        }
        for size in [1usize, 2, 3, 7, 100, 255, 256, 257, 1000, 8192] {
            let cuts = vec![size; data.len() / size + 1];
            assert_eq!(decode_all(data, 256, &cuts), whole, "size {size}");
        }
        // Frame boundaries that start on the block boundary use the fast path too.
        assert_eq!(decode_all(data, 256, &[256, 512, 1]), whole);
    }

    #[test]
    fn a_partial_last_block_decodes_the_codes_it_has() {
        let data = data_of(WAV256);
        let whole = decode_all(&data[..256], 256, &[]);
        assert_eq!(whole.len(), 505);
        for len in 0..256usize {
            let got = decode_all(&data[..len], 256, &[]);
            assert_eq!(got.len(), partial_samples(len), "len {len}");
            if len >= 4 {
                assert_eq!(got, whole[..got.len()], "len {len}");
            }
        }
        assert_eq!(partial_samples(3), 0);
        assert_eq!(partial_samples(4), 1);
        assert_eq!(partial_samples(5), 3);
        assert_eq!(samples_per_block(256), 505);
        assert_eq!(total_samples(256 * 3 + 10, 256), 505 * 3 + 13);
        assert_eq!(total_samples(100, 0), 0);
    }

    #[test]
    fn index_and_predictor_stay_in_range() {
        // A header index above 88 is 88; the predictor never leaves i16, whatever the codes.
        let mut blk = vec![0xFF, 0x7F, 200, 0];
        blk.extend(std::iter::repeat_n(0x77u8, 60));
        let mut out = Vec::new();
        decode_block(&blk, &mut out);
        assert_eq!(out[0], 32767);
        assert!(out.iter().all(|s| *s == 32767));
        let mut blk = vec![0x00, 0x80, 88, 0];
        blk.extend(std::iter::repeat_n(0xFFu8, 60));
        let mut out = Vec::new();
        decode_block(&blk, &mut out);
        assert!(out.iter().all(|s| *s == -32768));
        // Step index at 88 as stated gives the largest step first.
        let mut a = Vec::new();
        decode_block(&[0, 0, 88, 9, 0x01], &mut a);
        assert_eq!(a, [0, 12287, 16011]);
        let (_, i) = expand(0, 0, 0x0F);
        assert_eq!(i, 8);
        assert_eq!(expand(0, 0, 0x00).1, 0);
        assert_eq!(expand(0, 88, 0x07).1, 88);
        for code in 0..16u8 {
            for idx in [-5, 0, 40, 88, 200] {
                let (p, i) = expand(-32768, idx, code);
                assert!((-32768..=32767).contains(&p) && (0..=88).contains(&i));
            }
        }
        // Noise decodes without a panic.
        let mut seed = 7u32;
        for len in [0usize, 1, 3, 4, 5, 9, 64] {
            let blk: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (seed >> 16) as u8
                })
                .collect();
            let mut out = Vec::new();
            decode_block(&blk, &mut out);
            assert_eq!(out.len(), partial_samples(len));
        }
    }

    fn resample(rate: u32, input: &[i16], chunk: usize) -> Vec<i16> {
        let mut r = Resampler::new(rate);
        let mut out = Vec::new();
        for c in input.chunks(chunk.max(1)) {
            r.push(c, &mut out);
        }
        out
    }

    #[test]
    fn resampler_is_the_identity_at_16000() {
        let x: Vec<i16> = (0..1000).map(|i| (i * 37 % 20000 - 10000) as i16).collect();
        for chunk in [1, 7, 333, 1000] {
            assert_eq!(resample(16_000, &x, chunk), x, "chunk {chunk}");
        }
        assert!(resample(16_000, &[], 4).is_empty());
    }

    #[test]
    fn resampler_lengths_and_chunking() {
        let x: Vec<i16> = (0..20_000)
            .map(|i| ((i as f64 * 0.05).sin() * 9000.0) as i16)
            .collect();
        for rate in [8000u32, 10909, 11025, 12000, 16000, 22050, 44100, 48000] {
            let one = resample(rate, &x, x.len());
            let want = x.len() as i64 * 16_000 / rate as i64;
            assert!(
                (one.len() as i64 - want).abs() <= 1,
                "rate {rate}: {} vs {want}",
                one.len()
            );
            for chunk in [1usize, 2, 5, 97, 1024, 8191] {
                assert_eq!(resample(rate, &x, chunk), one, "rate {rate} chunk {chunk}");
            }
        }
        // The first output is the first input; a ramp stays a ramp (no overshoot).
        let ramp: Vec<i16> = (0..2000).map(|i| (i * 10) as i16).collect();
        let up = resample(8000, &ramp, 100);
        assert_eq!(up[0], 0);
        assert!(up.windows(2).all(|w| w[1] >= w[0]));
        assert!(up.iter().all(|s| *s <= 19_990));
        // 8000 -> 16000 puts the midpoint between neighbours.
        assert_eq!(resample(8000, &[0, 100, 200], 3), [0, 50, 100, 150, 200]);
        // Down from 48000: every third sample.
        let d = resample(48_000, &[0, 1, 2, 3, 4, 5, 6], 7);
        assert_eq!(d, [0, 3, 6]);
        // An out-of-range rate is clamped, not a panic.
        assert!(!resample(1, &x, 100).is_empty());
        assert!(!resample(u32::MAX, &x, 100).is_empty());
    }

    #[test]
    fn resampler_length_is_exactly_floor_plus_one() {
        // The law Verus proves for voice_rules::Resampler::push, summed over a stream: N samples in give
        // floor((N - 1) * 16000 / rate) + 1 out, whatever the chunking.
        for rate in [8000u32, 10909, 12345, 16000, 22050, 48000] {
            for n in [1usize, 2, 3, 7, 100, 999, 4097] {
                let x = vec![100i16; n];
                let want = (n - 1) * 16_000 / rate as usize + 1;
                for chunk in [1usize, 3, 1000] {
                    assert_eq!(resample(rate, &x, chunk).len(), want, "rate {rate} n {n} chunk {chunk}");
                }
            }
        }
    }

    #[test]
    fn pcm_header_is_44_bytes() {
        let h = wav_pcm16_header(32_000, 16_000);
        assert_eq!(h.len(), 44);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), 32_036);
        assert_eq!(&h[8..16], b"WAVEfmt ");
        assert_eq!(u32::from_le_bytes(h[16..20].try_into().unwrap()), 16);
        assert_eq!(u16::from_le_bytes(h[20..22].try_into().unwrap()), 1);
        assert_eq!(u32::from_le_bytes(h[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(h[28..32].try_into().unwrap()), 32_000);
        assert_eq!(u16::from_le_bytes(h[32..34].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(h[34..36].try_into().unwrap()), 16);
        assert_eq!(&h[36..40], b"data");
        assert_eq!(u32::from_le_bytes(h[40..44].try_into().unwrap()), 32_000);
    }

    #[test]
    fn adpcm_header_matches_ffmpegs() {
        // ffmpeg's fmt/fact/data fields, all but the byte rate: ffmpeg writes its encoder's default
        // 128 kbit/s there (16000) whatever the stream, Fortuna writes rate * block / samples.
        for (wav, rate, block, samples) in [
            (WAV256, 10909u32, 256u32, 0x3fecu32),
            (WAV128, 8000, 128, 0x0c80),
        ] {
            let data_len = (wav.len() - 60) as u32;
            let mut mine = wav_adpcm_header(rate, block, data_len);
            assert_eq!(mine.len(), 60);
            // ffmpeg counts the frames it was fed in `fact`, not the padded blocks.
            mine[48..52].copy_from_slice(&samples.to_le_bytes());
            let mut theirs = wav[..60].to_vec();
            assert_eq!(
                &mine[28..32],
                &(rate * block / (2 * (block - 4) + 1)).to_le_bytes()
            );
            mine[28..32].copy_from_slice(&theirs[28..32]);
            theirs[4..8].copy_from_slice(&mine[4..8]);
            assert_eq!(mine, theirs, "rate {rate}");
            assert_eq!(
                u32::from_le_bytes(wav[4..8].try_into().unwrap()),
                data_len + 52
            );
        }
        // Fortuna: 10909 Hz, 256-byte blocks of 505 samples -> 5530 bytes/s.
        let h = wav_adpcm_header(10909, 256, 256 * 4);
        assert_eq!(u32::from_le_bytes(h[28..32].try_into().unwrap()), 5530);
        assert_eq!(u32::from_le_bytes(h[48..52].try_into().unwrap()), 505 * 4);
        // An odd data length pads to even in the RIFF size.
        let h = wav_adpcm_header(8000, 9, 19);
        assert_eq!(u32::from_le_bytes(h[4..8].try_into().unwrap()), 19 + 1 + 52);
        assert_eq!(u32::from_le_bytes(h[56..60].try_into().unwrap()), 19);
    }

    fn raw<'a>() -> StartRaw<'a> {
        StartRaw::default()
    }

    #[test]
    fn start_defaults_are_todays_turn() {
        let o = validate_start(&raw()).unwrap();
        assert_eq!(
            o,
            StartOpts {
                vad: false,
                note: false,
                adpcm: false,
                rate: 16_000,
                block: 0,
                main: false,
                channel: None,
                session_id: None,
                reply_to: None,
                source: "voice-ws".into(),
                text: false,
                speak: true,
            }
        );
        let o = validate_start(&StartRaw {
            mode: Some("vad"),
            ..raw()
        })
        .unwrap();
        assert!(o.vad && !o.note);
    }

    #[test]
    fn start_validation_table() {
        let bad = |r: StartRaw| validate_start(&r).unwrap_err();
        let adpcm = StartRaw {
            codec: Some("ima-adpcm"),
            rate: Some(10909),
            block: Some(256),
            ..raw()
        };
        // mode, purpose, codec
        assert_eq!(
            bad(StartRaw {
                mode: Some("x"),
                ..raw()
            }),
            "mode: ptt or vad"
        );
        assert_eq!(
            bad(StartRaw {
                purpose: Some("memo"),
                ..raw()
            }),
            "purpose: command or note"
        );
        assert_eq!(
            bad(StartRaw {
                codec: Some("opus"),
                ..raw()
            }),
            "codec: pcm16 or ima-adpcm"
        );
        // rate
        assert_eq!(
            bad(StartRaw {
                rate: Some(8000),
                ..raw()
            }),
            "rate: 16000 for pcm16"
        );
        assert!(validate_start(&StartRaw {
            rate: Some(16000),
            ..raw()
        })
        .is_ok());
        for r in [7999, 22051, 0, 48000] {
            assert!(
                validate_start(&StartRaw {
                    rate: Some(r),
                    ..adpcm
                })
                .is_err(),
                "{r}"
            );
        }
        for r in [8000, 10909, 16000, 22050] {
            assert!(
                validate_start(&StartRaw {
                    rate: Some(r),
                    ..adpcm
                })
                .is_ok(),
                "{r}"
            );
        }
        // block: required for ima-adpcm, 8..=4096; ignored for pcm16
        assert!(validate_start(&StartRaw {
            block: None,
            ..adpcm
        })
        .is_err());
        for b in [0, 7, 4097] {
            assert!(
                validate_start(&StartRaw {
                    block: Some(b),
                    ..adpcm
                })
                .is_err(),
                "{b}"
            );
        }
        for b in [8, 256, 4096] {
            assert_eq!(
                validate_start(&StartRaw {
                    block: Some(b),
                    ..adpcm
                })
                .unwrap()
                .block,
                b
            );
        }
        assert_eq!(
            validate_start(&StartRaw {
                block: Some(1),
                ..raw()
            })
            .unwrap()
            .block,
            0
        );
        // thread
        assert_eq!(
            bad(StartRaw {
                thread: Some("x"),
                ..raw()
            }),
            "thread: voice or main"
        );
        assert!(
            validate_start(&StartRaw {
                thread: Some("main"),
                ..raw()
            })
            .unwrap()
            .main
        );
        assert!(
            !validate_start(&StartRaw {
                thread: Some("voice"),
                ..raw()
            })
            .unwrap()
            .main
        );
        // a note is always main; voice + note and vad + note are refused
        let note = StartRaw {
            purpose: Some("note"),
            ..raw()
        };
        assert!(validate_start(&note).unwrap().main);
        assert!(
            validate_start(&StartRaw {
                thread: Some("main"),
                ..note
            })
            .unwrap()
            .main
        );
        assert!(validate_start(&StartRaw {
            thread: Some("voice"),
            ..note
        })
        .is_err());
        assert!(validate_start(&StartRaw {
            mode: Some("vad"),
            ..note
        })
        .is_err());
        assert!(validate_start(&StartRaw {
            mode: Some("ptt"),
            ..note
        })
        .is_ok());
        // channel
        assert_eq!(
            validate_start(&StartRaw {
                channel: Some("ios"),
                ..raw()
            })
            .unwrap()
            .channel
            .as_deref(),
            Some("ios")
        );
        for c in ["voice", "", "IOS", "android"] {
            assert!(
                validate_start(&StartRaw {
                    channel: Some(c),
                    ..raw()
                })
                .is_err(),
                "{c}"
            );
        }
        // sessionId: empty is none, 64 is the longest
        assert_eq!(
            validate_start(&StartRaw {
                session_id: Some(""),
                ..raw()
            })
            .unwrap()
            .session_id,
            None
        );
        let s64 = "a".repeat(64);
        assert!(validate_start(&StartRaw {
            session_id: Some(&s64),
            ..raw()
        })
        .is_ok());
        let s65 = "a".repeat(65);
        assert!(validate_start(&StartRaw {
            session_id: Some(&s65),
            ..raw()
        })
        .is_err());
        for s in ["a b", "a.b", "é", "a/b"] {
            assert!(
                validate_start(&StartRaw {
                    session_id: Some(s),
                    ..raw()
                })
                .is_err(),
                "{s}"
            );
        }
        assert!(validate_start(&StartRaw {
            session_id: Some("ios_chat-1"),
            ..raw()
        })
        .is_ok());
        // replyTo: 1..=64
        assert!(validate_start(&StartRaw {
            reply_to: Some(""),
            ..raw()
        })
        .is_err());
        assert!(validate_start(&StartRaw {
            reply_to: Some("msg_12-x"),
            ..raw()
        })
        .is_ok());
        assert!(validate_start(&StartRaw {
            reply_to: Some(&s65),
            ..raw()
        })
        .is_err());
        // source
        for s in ["ios-chat", "web-chat", "juno-chat", "fortuna-chat", "a"] {
            assert_eq!(
                validate_start(&StartRaw {
                    source: Some(s),
                    ..raw()
                })
                .unwrap()
                .source,
                s
            );
        }
        let s33 = "a".repeat(33);
        for s in ["", "iOS-chat", "ios_chat", "ios chat", &s33] {
            assert!(
                validate_start(&StartRaw {
                    source: Some(s),
                    ..raw()
                })
                .is_err(),
                "{s}"
            );
        }
        // text and speak: speak false implies text
        let o = validate_start(&StartRaw {
            speak: Some(false),
            ..raw()
        })
        .unwrap();
        assert!(o.text && !o.speak);
        let o = validate_start(&StartRaw {
            text: Some(true),
            ..raw()
        })
        .unwrap();
        assert!(o.text && o.speak);
        let o = validate_start(&StartRaw {
            text: Some(false),
            speak: Some(true),
            ..raw()
        })
        .unwrap();
        assert!(!o.text && o.speak);
    }

    #[test]
    fn phases() {
        use Action::*;
        use Event::*;
        use Phase::*;
        let all_phases = [Idle, Collecting, Processing];
        let all_events = [
            Start,
            Audio,
            End,
            Cancel,
            Endpoint,
            TurnFinished,
            NotePosted,
        ];
        for p in all_phases {
            for e in all_events {
                let s = step(p, e);
                // One turn at a time: a start is only ever accepted from Idle.
                if s.act == Listen {
                    assert_eq!((p, e, s.next), (Idle, Start, Collecting));
                }
                // Audio only while collecting.
                if s.act == Accept {
                    assert_eq!((p, e), (Collecting, Audio));
                }
                // A cancel always ends in Idle.
                if e == Cancel {
                    assert_eq!(s.next, Idle);
                }
                // The turn runs only from Collecting, after end or an endpoint.
                if s.act == Process {
                    assert_eq!(p, Collecting);
                    assert!(matches!(e, End | Endpoint));
                    assert_eq!(s.next, Processing);
                }
                // The slot is released only when a turn was processing.
                if s.act == Release {
                    assert_eq!(p, Processing);
                    assert!(matches!(e, TurnFinished | NotePosted));
                    assert_eq!(s.next, Idle);
                }
                // After a note is posted the phase is Idle, never anything else.
                if e == NotePosted && p == Processing {
                    assert_eq!(s.next, Idle);
                }
                // A phase changes only by the events above.
                if s.next != p {
                    assert!(matches!(
                        (p, e),
                        (Idle, Start)
                            | (Collecting, End | Endpoint | Cancel)
                            | (Processing, Cancel | TurnFinished | NotePosted)
                    ));
                }
            }
        }
        assert_eq!(
            step(Collecting, Start),
            Step {
                next: Collecting,
                act: RejectBusy
            }
        );
        assert_eq!(
            step(Processing, Start),
            Step {
                next: Processing,
                act: RejectBusy
            }
        );
        assert_eq!(step(Idle, Audio).act, Ignore);
        assert_eq!(step(Processing, Audio).act, Ignore);
        assert_eq!(step(Idle, End).act, Ignore);
        assert_eq!(step(Idle, Cancel).act, Ignore);
        assert_eq!(step(Collecting, Cancel).act, Abort);
        assert_eq!(step(Processing, Cancel).act, Abort);
        assert_eq!(step(Collecting, TurnFinished).next, Collecting);
        assert_eq!(step(Collecting, NotePosted).act, Ignore);
    }

    #[test]
    fn the_byte_cap() {
        // Room left: take it all, not full.
        assert_eq!(audio_room(0, 100, 1000), (100, false));
        assert_eq!(audio_room(900, 99, 1000), (99, false));
        // Exactly full: the turn runs.
        assert_eq!(audio_room(900, 100, 1000), (100, true));
        // Over: only what fits.
        assert_eq!(audio_room(900, 500, 1000), (100, true));
        assert_eq!(audio_room(1000, 10, 1000), (0, true));
        assert_eq!(audio_room(2000, 10, 1000), (0, true));
        assert_eq!(audio_room(0, 0, 0), (0, true));
        assert_eq!(NOTE_CAP, 19_200_000);
        assert_eq!(NOTE_STT_MAX, 3_840_000);
    }
}
