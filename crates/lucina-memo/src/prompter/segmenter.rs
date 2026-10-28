//! Cuts the live microphone stream into speech-to-text jobs.
//!
//! Audio arrives as 16 kHz mono PCM. A simple energy detector (20 ms frames,
//! an adaptive noise floor) tells speech from silence. The open segment is
//! everything since the last pause:
//!
//! - every [`STEP_MS`] of new audio while he speaks, the whole open segment is
//!   transcribed again as a *partial* (the words so far);
//! - a pause of [`PAUSE_MS`] after some speech closes the segment as a *final*
//!   (transcribed once more, then kept); after [`MAX_SEGMENT_MS`] without a
//!   pause it is cut at the quietest moment of its last [`CUT_SEARCH_MS`], so a
//!   word is not split in two (whisper derails on a half word);
//! - silence alone is never sent (whisper invents words on silence).
//!
//! Pure and synchronous, so the timing rules are unit-tested.
pub const RATE: usize = 16_000;
const FRAME: usize = RATE / 50; // 20 ms
pub const STEP_MS: u64 = 500;
pub const PAUSE_MS: u64 = 600;
pub const MAX_SEGMENT_MS: u64 = 12_000;
pub const CUT_SEARCH_MS: u64 = 2_500;
/// Speech needed before a segment is worth transcribing.
const MIN_SPEECH_MS: u64 = 240;
/// Silence kept before speech starts (word onsets are quiet).
const LEAD_MS: u64 = 300;
/// Absolute floor for speech energy (RMS of i16 samples).
const MIN_RMS: f32 = 260.0;
const NOISE_FACTOR: f32 = 2.5;

#[derive(Debug, Clone, PartialEq)]
pub enum JobKind {
    Partial,
    Final,
}

/// One speech-to-text request: the samples and where they sit in the stream.
#[derive(Debug, Clone)]
pub struct Job {
    pub kind: JobKind,
    pub audio: Vec<i16>,
    pub start_ms: u64,
    pub end_ms: u64,
}

pub struct Segmenter {
    seg: Vec<i16>,
    /// Per 20 ms frame of `seg`: its energy and whether it was speech.
    levels: Vec<(f32, bool)>,
    seg_start: u64,
    total: u64,
    frame: Vec<i16>,
    noise: f32,
    speech_ms: u64,
    silence_ms: u64,
    last_partial: u64,
}

impl Default for Segmenter {
    fn default() -> Self {
        Self::new()
    }
}

fn ms(samples: u64) -> u64 {
    samples * 1000 / RATE as u64
}

fn samples(ms: u64) -> u64 {
    ms * RATE as u64 / 1000
}

impl Segmenter {
    pub fn new() -> Self {
        Self {
            seg: Vec::new(),
            levels: Vec::new(),
            seg_start: 0,
            total: 0,
            frame: Vec::with_capacity(FRAME),
            noise: 150.0,
            speech_ms: 0,
            silence_ms: 0,
            last_partial: 0,
        }
    }

    /// Milliseconds of audio received so far.
    pub fn audio_ms(&self) -> u64 {
        ms(self.total)
    }

    /// Add samples; updates the speech detector frame by frame.
    pub fn push(&mut self, pcm: &[i16]) {
        for &s in pcm {
            self.frame.push(s);
            if self.frame.len() == FRAME {
                let rms = (self
                    .frame
                    .iter()
                    .map(|&x| (x as f32) * (x as f32))
                    .sum::<f32>()
                    / FRAME as f32)
                    .sqrt();
                let speech = rms > MIN_RMS.max(self.noise * NOISE_FACTOR);
                if speech {
                    self.speech_ms += 20;
                    self.silence_ms = 0;
                } else {
                    self.silence_ms += 20;
                    // The floor follows the room slowly, and falls fast after a loud bump.
                    self.noise = if rms < self.noise {
                        0.8 * self.noise + 0.2 * rms
                    } else {
                        0.98 * self.noise + 0.02 * rms
                    };
                }
                self.seg.extend_from_slice(&self.frame);
                self.levels.push((rms, speech));
                self.frame.clear();
            }
        }
        self.total += pcm.len() as u64;
    }

    /// The next job, if one is due. Call when the speech-to-text engine is idle.
    pub fn next_job(&mut self) -> Option<Job> {
        let seg_ms = ms(self.seg.len() as u64);
        if self.speech_ms < MIN_SPEECH_MS {
            // Only silence (or a click) so far: keep a short lead-in, drop the rest.
            if self.speech_ms == 0 || self.silence_ms > PAUSE_MS {
                self.trim_to_lead();
            }
            return None;
        }
        let end = self.seg_start + self.seg.len() as u64;
        if self.silence_ms >= PAUSE_MS || seg_ms >= MAX_SEGMENT_MS {
            let frames = self.levels.len();
            let cut = if self.silence_ms >= PAUSE_MS {
                frames
            } else {
                let from = frames.saturating_sub((CUT_SEARCH_MS / 20) as usize).max(1);
                (from..frames)
                    .min_by(|&a, &b| self.levels[a].0.total_cmp(&self.levels[b].0))
                    .unwrap_or(frames)
            };
            let rest = self.seg.split_off(cut * FRAME);
            let rest_levels = self.levels.split_off(cut);
            let job = Job {
                kind: JobKind::Final,
                audio: std::mem::replace(&mut self.seg, rest),
                start_ms: ms(self.seg_start),
                end_ms: ms(self.seg_start + (cut * FRAME) as u64),
            };
            self.seg_start += (cut * FRAME) as u64;
            self.speech_ms = 20 * rest_levels.iter().filter(|l| l.1).count() as u64;
            self.levels = rest_levels;
            self.last_partial = end;
            return Some(job);
        }
        if end >= self.last_partial + samples(STEP_MS) {
            self.last_partial = end;
            return Some(Job {
                kind: JobKind::Partial,
                audio: self.seg.clone(),
                start_ms: ms(self.seg_start),
                end_ms: ms(end),
            });
        }
        None
    }

    fn trim_to_lead(&mut self) {
        let keep = samples(LEAD_MS) as usize;
        if self.seg.len() > keep {
            let drop = self.seg.len() - keep;
            self.seg.drain(..drop);
            self.levels.drain(..drop / FRAME);
            self.seg_start += drop as u64;
            self.speech_ms = 0;
        }
    }
}

const HALLUCINATIONS: &[&str] = &[
    "you",
    "thank you",
    "thanks for watching",
    "thank you for watching",
    "grazie",
    "grazie a tutti",
    "tak",
    "tak fordi du så med",
    "subtitles by the amaraorg community",
    "sottotitoli a cura di qtss",
];

/// Whisper's text cleaned: no `[BLANK_AUDIO]`, `(music)` or `*laughs*` tags,
/// single spaces. Empty when nothing real is left.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0i32;
    let mut star = false;
    for c in text.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = (depth - 1).max(0),
            '*' => star = !star,
            _ if depth == 0 && !star => out.push(c),
            _ => {}
        }
    }
    let out = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().all(|c| !c.is_alphanumeric()) {
        return String::new();
    }
    // What whisper says on breath noise or room tone.
    let bare: String = out
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect();
    if HALLUCINATIONS.contains(&bare.trim()) {
        return String::new();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(ms_: u64, amp: f32) -> Vec<i16> {
        (0..samples(ms_))
            .map(|i| (amp * (i as f32 * 0.07).sin()) as i16)
            .collect()
    }

    fn feed(s: &mut Segmenter, pcm: &[i16]) -> Vec<Job> {
        // 100 ms chunks, like the browser's worklet, asking for a job after each.
        let mut jobs = vec![];
        for c in pcm.chunks(samples(100) as usize) {
            s.push(c);
            jobs.extend(s.next_job());
        }
        jobs
    }

    #[test]
    fn silence_never_becomes_a_job() {
        let mut s = Segmenter::new();
        assert!(feed(&mut s, &tone(10_000, 40.0)).is_empty());
        assert!(s.seg.len() <= samples(LEAD_MS) as usize + FRAME * 5);
        assert_eq!(s.audio_ms(), 10_000);
    }

    #[test]
    fn speech_gives_partials_every_step_then_a_final_at_the_pause() {
        let mut s = Segmenter::new();
        let mut pcm = tone(1000, 30.0);
        pcm.extend(tone(2000, 4000.0));
        pcm.extend(tone(1000, 30.0));
        let jobs = feed(&mut s, &pcm);
        let partials = jobs.iter().filter(|j| j.kind == JobKind::Partial).count();
        let finals: Vec<_> = jobs.iter().filter(|j| j.kind == JobKind::Final).collect();
        assert!((3..=5).contains(&partials), "{partials} partials");
        assert_eq!(finals.len(), 1);
        let f = finals[0];
        // The final holds the lead-in, the speech and the pause that closed it.
        assert!(
            f.start_ms <= 1000 && f.start_ms >= 1000 - LEAD_MS - 100,
            "{}",
            f.start_ms
        );
        assert!(
            f.end_ms >= 3000 + PAUSE_MS && f.end_ms <= 3000 + PAUSE_MS + 200,
            "{}",
            f.end_ms
        );
        // Partials grow from the same start.
        let p: Vec<_> = jobs.iter().filter(|j| j.kind == JobKind::Partial).collect();
        assert!(p
            .windows(2)
            .all(|w| w[0].start_ms == w[1].start_ms && w[0].end_ms < w[1].end_ms));
    }

    #[test]
    fn long_speech_is_cut_at_the_maximum() {
        let mut s = Segmenter::new();
        let jobs = feed(&mut s, &tone(MAX_SEGMENT_MS * 2 + 500, 4000.0));
        let finals: Vec<_> = jobs.iter().filter(|j| j.kind == JobKind::Final).collect();
        assert_eq!(finals.len(), 2);
        assert_eq!(
            finals[1].start_ms, finals[0].end_ms,
            "no audio lost between segments"
        );
    }

    #[test]
    fn a_forced_cut_lands_in_the_quietest_gap() {
        let mut s = Segmenter::new();
        // 10.5 s of speech, a 200 ms dip (too short for a pause), then more speech.
        let mut pcm = tone(10_500, 4000.0);
        pcm.extend(tone(200, 300.0));
        pcm.extend(tone(3_000, 4000.0));
        let jobs = feed(&mut s, &pcm);
        let f = jobs
            .iter()
            .find(|j| j.kind == JobKind::Final)
            .expect("a forced final");
        assert!(
            (10_500..=10_700).contains(&f.end_ms),
            "cut at {} ms",
            f.end_ms
        );
        assert_eq!(f.audio.len() as u64, samples(f.end_ms - f.start_ms));
        // The words after the gap stay in the open segment.
        assert!(s.speech_ms >= 1000);
        assert_eq!(s.seg_start, samples(f.end_ms));
    }

    #[test]
    fn a_click_is_not_speech() {
        let mut s = Segmenter::new();
        let mut pcm = tone(500, 30.0);
        pcm.extend(tone(60, 8000.0));
        pcm.extend(tone(2000, 30.0));
        assert!(feed(&mut s, &pcm).is_empty());
    }

    #[test]
    fn noise_floor_adapts_to_a_humming_room() {
        let mut s = Segmenter::new();
        // A steady hum above the absolute floor is learned as noise, not speech.
        let mut pcm = tone(3000, 500.0);
        let quiet_jobs = feed(&mut s, &pcm);
        pcm = tone(1500, 6000.0);
        pcm.extend(tone(1000, 500.0));
        let jobs = feed(&mut s, &pcm);
        assert!(quiet_jobs.len() <= 1, "{}", quiet_jobs.len());
        assert!(jobs.iter().any(|j| j.kind == JobKind::Final));
    }

    #[test]
    fn whisper_tags_are_removed() {
        assert_eq!(clean(" [BLANK_AUDIO] "), "");
        assert_eq!(
            clean("(music) Hello  there *laughs* friend."),
            "Hello there friend."
        );
        assert_eq!(clean(" ... "), "");
        assert_eq!(clean("Jev decides."), "Jev decides.");
        assert_eq!(clean(" Thank you."), "");
        assert_eq!(
            clean("Thank you for coming, Jev."),
            "Thank you for coming, Jev."
        );
    }
}
