//! Energy voice-activity detector for the LucinaVoice socket
//! ([`crate::voice_ws`]): 16 kHz mono s16le in, "speech started" and
//! "end of speech" out. Pure and allocation-light; no I/O.
//!
//! - The audio is cut into 20 ms frames ([`FRAME_SAMPLES`]); each frame's RMS
//!   is compared with a threshold of `max(`[`ABS_MIN_RMS`]`, floor × `[`RATIO`]`)`.
//! - `floor` is the noise floor. It starts as the quietest of the first
//!   [`CALIBRATION_FRAMES`] frames (capped at [`FLOOR_MAX`], so speech from the
//!   very first frame does not hide itself), then follows the frames that are
//!   *below* the threshold: down quickly, up slowly. A noise that is already
//!   above the threshold stays "speech" until the client ends the turn.
//! - Speech starts once about [`ONSET_FRAMES`] (300 ms) were above the threshold
//!   (a leaky count: a quiet frame between two words costs one, not all).
//! - After speech, [`ENDPOINT_FRAMES`] (700 ms) of consecutive quiet frames
//!   are the end of speech.
//! - [`Vad::trim_len`] says how much of the recording to keep when the turn
//!   ends some other way (push-to-talk): up to the last loud frame plus
//!   [`TAIL_PAD_SAMPLES`] (200 ms).

/// 20 ms at 16 kHz.
pub const FRAME_SAMPLES: usize = 320;
/// Loud frames needed before speech counts as started (300 ms).
pub const ONSET_FRAMES: u32 = 15;
/// Quiet frames after speech that end it (700 ms).
pub const ENDPOINT_FRAMES: u32 = 35;
/// Frames used to measure the first noise floor (200 ms).
pub const CALIBRATION_FRAMES: usize = 10;
/// A frame below this RMS (of 32768) is never speech.
pub const ABS_MIN_RMS: f32 = 200.0;
/// Speech must be this many times louder than the noise floor.
pub const RATIO: f32 = 3.0;
/// The initial noise floor is at most this.
pub const FLOOR_MAX: f32 = 400.0;
const FLOOR_MIN: f32 = 15.0;
/// Kept after the last loud frame when trimming (200 ms).
pub const TAIL_PAD_SAMPLES: usize = 3200;

/// What one [`Vad::feed`] call saw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Update {
    /// Speech started in this chunk of audio.
    pub speech_started: bool,
    /// The end of speech was found in this chunk. The detector stops there:
    /// later audio is ignored.
    pub endpoint: bool,
}

#[derive(Debug, Default)]
pub struct Vad {
    /// Bytes of an unfinished frame.
    carry: Vec<u8>,
    /// Whole frames seen.
    frames: usize,
    floor: f32,
    calib_min: f32,
    onset: u32,
    speaking: bool,
    quiet: u32,
    /// Sample index just after the last loud frame.
    last_loud_end: Option<usize>,
    endpoint: bool,
}

fn rms(frame: &[i16]) -> f32 {
    let sum: f64 = frame.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum / frame.len().max(1) as f64).sqrt() as f32
}

impl Vad {
    pub fn new() -> Self {
        Self {
            calib_min: f32::MAX,
            ..Self::default()
        }
    }

    /// Feed raw s16le bytes (any length; a split sample or frame is carried over).
    pub fn feed(&mut self, pcm: &[u8]) -> Update {
        let mut up = Update::default();
        if self.endpoint {
            return up;
        }
        self.carry.extend_from_slice(pcm);
        let frame_bytes = FRAME_SAMPLES * 2;
        let mut used = 0;
        while self.carry.len() - used >= frame_bytes {
            let (pairs, _) = self.carry[used..used + frame_bytes].as_chunks::<2>();
            let frame: Vec<i16> = pairs.iter().map(|b| i16::from_le_bytes(*b)).collect();
            used += frame_bytes;
            self.frame(&frame, &mut up);
            if self.endpoint {
                break;
            }
        }
        self.carry.drain(..used);
        up
    }

    fn frame(&mut self, frame: &[i16], up: &mut Update) {
        let level = rms(frame);
        self.frames += 1;
        if self.frames <= CALIBRATION_FRAMES {
            self.calib_min = self.calib_min.min(level);
            if self.frames == CALIBRATION_FRAMES {
                self.floor = self.calib_min.clamp(FLOOR_MIN, FLOOR_MAX);
            }
            return;
        }
        let threshold = ABS_MIN_RMS.max(self.floor * RATIO);
        if level >= threshold {
            self.last_loud_end = Some(self.frames * FRAME_SAMPLES);
            self.quiet = 0;
            if !self.speaking {
                self.onset += 1;
                if self.onset >= ONSET_FRAMES {
                    self.speaking = true;
                    up.speech_started = true;
                }
            }
            return;
        }
        // A quiet frame: the floor follows it, down fast, up slowly.
        self.floor = if level < self.floor {
            self.floor * 0.8 + level * 0.2
        } else {
            self.floor + (level - self.floor) * 0.02
        }
        .clamp(FLOOR_MIN, FLOOR_MAX * 2.0);
        if self.speaking {
            self.quiet += 1;
            if self.quiet >= ENDPOINT_FRAMES {
                self.endpoint = true;
                up.endpoint = true;
            }
        } else {
            self.onset = self.onset.saturating_sub(1);
        }
    }

    /// Speech was found (not just a noise burst).
    pub fn heard_speech(&self) -> bool {
        self.speaking
    }

    /// Samples consumed in whole frames so far.
    pub fn position_samples(&self) -> usize {
        self.frames * FRAME_SAMPLES
    }

    /// How many bytes of a `total`-byte recording to keep: everything when
    /// no speech was found, else up to the last loud frame plus the tail pad.
    pub fn trim_len(&self, total: usize) -> usize {
        match self.last_loud_end.filter(|_| self.speaking) {
            Some(end) => total.min((end + TAIL_PAD_SAMPLES) * 2),
            None => total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: usize = 16_000;

    fn bytes(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn silence(ms: usize) -> Vec<i16> {
        vec![0; RATE * ms / 1000]
    }

    /// A 440 Hz tone of the given amplitude (RMS = amp / sqrt 2).
    fn tone(ms: usize, amp: f32) -> Vec<i16> {
        (0..RATE * ms / 1000)
            .map(|i| {
                (amp * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / RATE as f32).sin()) as i16
            })
            .collect()
    }

    /// Deterministic uniform noise in ±amp (RMS = amp / sqrt 3).
    fn noise(ms: usize, amp: i32, seed: u32) -> Vec<i16> {
        let mut x = seed;
        (0..RATE * ms / 1000)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((x >> 8) as i32 % (2 * amp + 1) - amp) as i16
            })
            .collect()
    }

    /// Feeds 20 ms at a time; the sample positions of the events.
    fn run(audio: &[i16]) -> (Option<usize>, Option<usize>, Vad) {
        let mut vad = Vad::new();
        let (mut started, mut end) = (None, None);
        for piece in audio.chunks(FRAME_SAMPLES) {
            let up = vad.feed(&bytes(piece));
            if up.speech_started {
                started = Some(vad.position_samples());
            }
            if up.endpoint {
                end = Some(vad.position_samples());
            }
        }
        (started, end, vad)
    }

    fn cat(parts: &[Vec<i16>]) -> Vec<i16> {
        parts.concat()
    }

    #[test]
    fn silence_and_quiet_noise_are_not_speech() {
        let (s, e, vad) = run(&silence(3000));
        assert!(s.is_none() && e.is_none() && !vad.heard_speech());
        // RMS about 58, under the absolute minimum.
        let (s, e, vad) = run(&noise(3000, 100, 1));
        assert!(s.is_none() && e.is_none() && !vad.heard_speech());
        assert_eq!(vad.trim_len(1000), 1000);
    }

    #[test]
    fn speech_starts_after_300_ms_and_ends_after_700_ms() {
        let audio = cat(&[silence(500), tone(1000, 8000.0), silence(1500)]);
        let (started, end, vad) = run(&audio);
        let ms = |n: usize| n * 1000 / RATE;
        // Tone starts at 500 ms: speech counts from 800 ms (one frame of slack).
        let started = ms(started.unwrap());
        assert!((800..=840).contains(&started), "{started}");
        // Tone ends at 1500 ms: the endpoint is 700 ms later.
        let end = ms(end.unwrap());
        assert!((2200..=2240).contains(&end), "{end}");
        assert!(vad.heard_speech());
    }

    #[test]
    fn a_click_is_not_speech() {
        let audio = cat(&[silence(500), tone(100, 12000.0), silence(2000)]);
        let (s, e, vad) = run(&audio);
        assert!(s.is_none() && e.is_none() && !vad.heard_speech());
    }

    #[test]
    fn pauses_between_words_do_not_end_speech() {
        // Words of 400 ms with 300 ms gaps, then a 900 ms gap.
        let word = tone(400, 6000.0);
        let audio = cat(&[
            silence(300),
            word.clone(),
            silence(300),
            word.clone(),
            silence(300),
            word,
            silence(900),
        ]);
        let (started, end, _) = run(&audio);
        let ms = |n: usize| n * 1000 / RATE;
        assert!(started.is_some());
        // Only the last gap ends it: the last word ends at 2100 ms.
        let end = ms(end.unwrap());
        assert!((2780..=2820).contains(&end), "{end}");
    }

    #[test]
    fn steady_background_noise_is_calibrated_away() {
        // RMS about 260: above the absolute minimum, but it is the floor.
        let bg = |ms, seed| noise(ms, 450, seed);
        let audio = cat(&[bg(1500, 3), tone(800, 9000.0), bg(1500, 4)]);
        let (started, end, _) = run(&audio);
        let ms = |n: usize| n * 1000 / RATE;
        let started = ms(started.expect("speech over the noise"));
        assert!((1800..=1900).contains(&started), "{started}");
        let end = ms(end.expect("end of speech"));
        assert!((3000..=3100).contains(&end), "{end}");
        // The noise alone never triggers.
        let (s, e, _) = run(&bg(4000, 5));
        assert!(s.is_none() && e.is_none());
    }

    #[test]
    fn speech_in_the_first_frames_is_found() {
        let audio = cat(&[tone(1000, 8000.0), silence(1000)]);
        let (started, end, _) = run(&audio);
        assert!(started.is_some() && end.is_some());
    }

    #[test]
    fn odd_chunk_sizes_give_the_same_events() {
        let audio = bytes(&cat(&[silence(400), tone(900, 7000.0), silence(1200)]));
        let whole = {
            let mut v = Vad::new();
            (v.feed(&audio), v.position_samples())
        };
        for size in [1usize, 3, 641, 8192] {
            let mut v = Vad::new();
            let (mut started, mut ended) = (false, false);
            for piece in audio.chunks(size) {
                let up = v.feed(piece);
                started |= up.speech_started;
                ended |= up.endpoint;
            }
            assert!(started && ended, "chunk size {size}");
            assert_eq!(v.position_samples(), whole.1, "chunk size {size}");
        }
        assert!(whole.0.endpoint);
    }

    #[test]
    fn trim_keeps_speech_and_a_short_tail() {
        // 300 ms silence, 1 s tone, 2 s silence, as a push-to-talk recording.
        let audio = cat(&[silence(300), tone(1000, 8000.0), silence(2000)]);
        let total = audio.len() * 2;
        let mut vad = Vad::new();
        // Stop feeding before the endpoint, like a button released at 1.9 s.
        let upto = RATE * 1900 / 1000;
        for piece in audio[..upto].chunks(FRAME_SAMPLES) {
            vad.feed(&bytes(piece));
        }
        assert!(vad.heard_speech());
        let kept = vad.trim_len(total) / 2;
        // Tone ends at 1300 ms: 200 ms of tail stays.
        let kept_ms = kept * 1000 / RATE;
        assert!((1480..=1520).contains(&kept_ms), "{kept_ms}");
        assert_eq!(vad.trim_len(100), 100);
    }

    #[test]
    fn nothing_is_fed_after_the_endpoint() {
        let audio = cat(&[tone(800, 8000.0), silence(1000)]);
        let (_, end, mut vad) = run(&audio);
        assert!(end.is_some());
        let pos = vad.position_samples();
        assert_eq!(vad.feed(&bytes(&tone(500, 8000.0))), Update::default());
        assert_eq!(vad.position_samples(), pos);
    }
}
