//! One timeline from two tracks: the owner's microphone (every line is the owner's) and the
//! system audio (everyone else, diarized into clusters).
//!
//! - A system segment's words get the speaker whose turn overlaps each word most (kernel MT7);
//!   a segment that spans two speakers is split where the speaker changes.
//! - Track offsets (`start_offset_ms`) put both tracks on the shared recording clock.
//! - A microphone line that repeats a system line at the same time is the remote voices
//!   coming out of the owner's speakers (echo): it is dropped.
//! - Consecutive lines of one speaker less than 1.5 s apart are joined (up to 60 s).
use crate::diarize::Turn;
use crate::kernel::overlap_ms;
use crate::model::{Segment, Word};
use crate::search::fold;

use super::kernel::speaker_for;

/// One line of the merged timeline, before names, roles and kinds.
#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub start: f64,
    pub end: f64,
    pub speaker: String,
    pub track: &'static str,
    pub text: String,
    pub words: Vec<Word>,
}

const JOIN_GAP: f64 = 1.5;
const JOIN_MAX: f64 = 60.0;
const ECHO_OVERLAP: f64 = 0.5;
const ECHO_SIMILAR: f64 = 0.6;

fn ms(s: f64) -> i64 {
    (s * 1000.0).round() as i64
}

fn shifted(w: &Word, off: f64) -> Word {
    Word {
        start: w.start + off,
        end: w.end + off,
        text: w.text.clone(),
    }
}

/// The owner's lines (mic track), shifted by the track offset.
pub fn mic_parts(segments: &[Segment], offset: f64, owner: &str) -> Vec<Part> {
    segments
        .iter()
        .filter(|s| !s.text.trim().is_empty())
        .map(|s| Part {
            start: s.start + offset,
            end: s.end + offset,
            speaker: owner.to_owned(),
            track: "mic",
            text: s.text.trim().to_owned(),
            words: s.words.iter().map(|w| shifted(w, offset)).collect(),
        })
        .collect()
}

/// The cluster speaking during `[a, b)` (turns are on the system track's own clock).
fn cluster_at(turns: &[Turn], clusters: usize, a: f64, b: f64) -> Option<usize> {
    let mut overlaps = vec![0i64; clusters];
    for t in turns {
        if let Some(o) = overlaps.get_mut(t.speaker) {
            *o += overlap_ms(ms(a), ms(b.max(a + 0.001)), ms(t.start), ms(t.end));
        }
    }
    speaker_for(&overlaps)
}

/// The nearest turn's cluster (for speech the diarizer did not mark).
fn nearest(turns: &[Turn], a: f64, b: f64) -> Option<usize> {
    turns
        .iter()
        .min_by(|x, y| {
            let dx = (x.start - b).max(a - x.end).max(0.0);
            let dy = (y.start - b).max(a - y.end).max(0.0);
            dx.total_cmp(&dy)
        })
        .map(|t| t.speaker)
}

/// System lines with speakers. `ids[c]` is cluster `c`'s speaker id (`S1`, or `me` when the
/// cluster is the owner's voice). Without turns everything is `ids[0]` (or `S1`).
pub fn system_parts(
    segments: &[Segment],
    turns: &[Turn],
    ids: &[String],
    offset: f64,
) -> Vec<Part> {
    let id = |c: Option<usize>| {
        c.and_then(|c| ids.get(c).cloned())
            .or_else(|| ids.first().cloned())
            .unwrap_or_else(|| "S1".into())
    };
    let mut out = Vec::new();
    for s in segments.iter().filter(|s| !s.text.trim().is_empty()) {
        let fallback =
            cluster_at(turns, ids.len(), s.start, s.end).or_else(|| nearest(turns, s.start, s.end));
        if s.words.is_empty() {
            out.push(Part {
                start: s.start + offset,
                end: s.end + offset,
                speaker: id(fallback),
                track: "system",
                text: s.text.trim().to_owned(),
                words: Vec::new(),
            });
            continue;
        }
        // Each word's cluster; unmarked words take the previous word's (or the segment's).
        let mut current = fallback;
        let mut runs: Vec<(Option<usize>, Vec<Word>)> = Vec::new();
        for w in &s.words {
            if let Some(c) = cluster_at(turns, ids.len(), w.start, w.end) {
                current = Some(c);
            }
            match runs.last_mut() {
                Some((c, ws)) if *c == current => ws.push(w.clone()),
                _ => runs.push((current, vec![w.clone()])),
            }
        }
        for (c, ws) in runs {
            let text = ws
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            out.push(Part {
                start: ws.first().map_or(s.start, |w| w.start) + offset,
                end: ws.last().map_or(s.end, |w| w.end) + offset,
                speaker: id(c),
                track: "system",
                text,
                words: ws.iter().map(|w| shifted(w, offset)).collect(),
            });
        }
    }
    out
}

fn word_set(text: &str) -> std::collections::HashSet<String> {
    fold(text).split_whitespace().map(str::to_owned).collect()
}

/// Jaccard similarity of the two lines' words.
pub fn similarity(a: &str, b: &str) -> f64 {
    let (x, y) = (word_set(a), word_set(b));
    if x.is_empty() || y.is_empty() {
        return 0.0;
    }
    x.intersection(&y).count() as f64 / x.union(&y).count() as f64
}

/// A mic line that a system line already has at the same time (speaker echo).
fn is_echo(mic: &Part, system: &[Part]) -> bool {
    let len = (mic.end - mic.start).max(0.001);
    system.iter().any(|s| {
        let o = (mic.end.min(s.end) - mic.start.max(s.start)).max(0.0);
        o / len >= ECHO_OVERLAP && similarity(&mic.text, &s.text) >= ECHO_SIMILAR
    })
}

/// The merged timeline, in time order (the mic first at equal starts).
pub fn merge(mic: Vec<Part>, system: Vec<Part>) -> Vec<Part> {
    let mic: Vec<Part> = mic.into_iter().filter(|p| !is_echo(p, &system)).collect();
    let mut all: Vec<Part> = mic.into_iter().chain(system).collect();
    all.sort_by(|a, b| {
        a.start
            .total_cmp(&b.start)
            .then_with(|| (a.track != "mic").cmp(&(b.track != "mic")))
    });
    let mut out: Vec<Part> = Vec::with_capacity(all.len());
    for p in all {
        match out.last_mut() {
            Some(last)
                if last.speaker == p.speaker
                    && p.start - last.end <= JOIN_GAP
                    && p.end - last.start <= JOIN_MAX =>
            {
                last.end = last.end.max(p.end);
                last.text.push(' ');
                last.text.push_str(&p.text);
                last.words.extend(p.words);
            }
            _ => out.push(p),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(start: f64, end: f64, text: &str) -> Word {
        Word {
            start,
            end,
            text: text.into(),
        }
    }

    fn seg(start: f64, end: f64, text: &str, words: Vec<Word>) -> Segment {
        Segment {
            start,
            end,
            text: text.into(),
            speaker: None,
            words,
        }
    }

    fn turn(start: f64, end: f64, speaker: usize) -> Turn {
        Turn {
            start,
            end,
            speaker,
        }
    }

    #[test]
    fn a_segment_across_two_speakers_is_split() {
        let s = seg(
            0.0,
            4.0,
            "Any questions? Yes, one.",
            vec![
                w(0.0, 0.5, "Any"),
                w(0.5, 1.4, "questions?"),
                w(2.1, 2.6, "Yes,"),
                w(2.6, 3.4, "one."),
            ],
        );
        let turns = [turn(0.0, 1.6, 0), turn(2.0, 3.6, 1)];
        let ids = vec!["S1".to_owned(), "S2".to_owned()];
        let p = system_parts(&[s], &turns, &ids, 0.25);
        assert_eq!(p.len(), 2);
        assert_eq!(
            (p[0].speaker.as_str(), p[0].text.as_str()),
            ("S1", "Any questions?")
        );
        assert_eq!(
            (p[1].speaker.as_str(), p[1].text.as_str()),
            ("S2", "Yes, one.")
        );
        assert_eq!((p[1].start, p[1].end), (2.35, 3.65));
        assert_eq!(p[1].words[0].start, 2.35);
    }

    #[test]
    fn unmarked_words_and_segments_follow_their_neighbours() {
        // "um" falls in a gap: it stays with the speaker before it.
        let s = seg(
            0.0,
            3.0,
            "so um right",
            vec![w(0.0, 0.5, "so"), w(1.0, 1.2, "um"), w(2.0, 2.5, "right")],
        );
        let turns = [turn(0.0, 0.8, 1), turn(1.9, 3.0, 1)];
        let ids = vec!["S1".to_owned(), "S2".to_owned()];
        let p = system_parts(&[s], &turns, &ids, 0.0);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].speaker, "S2");
        // No words, no overlap: the nearest turn.
        let p = system_parts(&[seg(10.0, 11.0, "later", vec![])], &turns, &ids, 0.0);
        assert_eq!(p[0].speaker, "S2");
        // No diarization at all: one speaker.
        let p = system_parts(&[seg(0.0, 1.0, "hi", vec![])], &[], &[], 0.0);
        assert_eq!(p[0].speaker, "S1");
    }

    #[test]
    fn merge_orders_drops_echo_and_joins() {
        let mic = mic_parts(
            &[
                seg(0.0, 2.0, "Hello everyone.", vec![]),
                seg(2.5, 3.0, "Let's start.", vec![]),
                // The professor's words leaking into the owner's microphone.
                seg(5.1, 7.9, "the exam is on friday", vec![]),
                seg(9.0, 10.0, "Can you repeat that?", vec![]),
            ],
            0.0,
            "me",
        );
        let system = vec![Part {
            start: 5.0,
            end: 8.0,
            speaker: "S1".into(),
            track: "system",
            text: "The exam is on Friday.".into(),
            words: vec![],
        }];
        let m = merge(mic, system);
        let lines: Vec<(&str, &str)> = m
            .iter()
            .map(|p| (p.speaker.as_str(), p.text.as_str()))
            .collect();
        assert_eq!(
            lines,
            [
                ("me", "Hello everyone. Let's start."),
                ("S1", "The exam is on Friday."),
                ("me", "Can you repeat that?")
            ]
        );
        assert!(similarity("The exam is on Friday.", "the exam is on friday") > 0.99);
        assert_eq!(similarity("", "x"), 0.0);
    }

    #[test]
    fn track_offsets_align_the_two_tracks() {
        // The mic started 2 s after the system audio: its 1 s is 3 s on the shared clock.
        let mic = mic_parts(&[seg(1.0, 2.0, "Mine.", vec![])], 2.0, "me");
        let sys = system_parts(
            &[seg(2.5, 3.5, "Theirs.", vec![])],
            &[turn(0.0, 10.0, 0)],
            &["S1".into()],
            0.0,
        );
        let m = merge(mic, sys);
        assert_eq!(m[0].text, "Theirs.");
        assert_eq!(m[1].start, 3.0);
    }
}
