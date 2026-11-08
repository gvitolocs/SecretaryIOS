//! Voice Learn in lucina-memo (docs/lucinavoice-protocol.md section 8).
//!
//! Every speech-to-text path here goes through the same two steps, with the
//! vocabulary of [`lucina_vocab`] (re-read when its file changes; a missing file
//! changes nothing):
//!
//! 1. **Bias:** [`prompt`] puts the taught words into Whisper's initial prompt.
//!    whisper.cpp keeps the *end* of an over-long prompt, so the vocabulary goes
//!    last and the total stays under [`PROMPT_CHARS`].
//! 2. **Post-correction:** [`correct_transcript`] replaces close mishearings by
//!    the canonical word in every segment (and in the word timings), and records
//!    what it did in [`Transcript::corrections`].
//!
//! The paths: `POST /v1/memos/stt` and the prompter's live engine
//! ([`crate::prompter::engine::Engine::transcribe`]), the memo worker, the
//! iPhone-transcript upgrade and meetings (all [`crate::transcribe`]'s `run`),
//! and the iPhone's own transcript when it is stored.
use lucina_vocab::{Ambiguity, Vocab};

use crate::model::{Correction, Transcript, Word};

/// Whisper's initial prompt is at most 224 tokens; stay under it with room for odd names.
pub const PROMPT_CHARS: usize = 500;
/// Always leave the vocabulary at least this much, however long the caller's prompt is.
const MIN_VOCAB_CHARS: usize = 200;
/// Corrections kept on one transcript.
const MAX_CORRECTIONS: usize = 200;

/// `base` (the caller's own prompt: "Lucina", a video's title and tags, the
/// app's terms) followed by the vocabulary's words. Empty when both are.
pub fn prompt(vocab: &Vocab, base: &str) -> String {
    let base = base.trim();
    let used = base.chars().count() + if base.is_empty() { 0 } else { 2 };
    let budget = PROMPT_CHARS.saturating_sub(used).max(MIN_VOCAB_CHARS);
    let words = vocab.prompt(budget);
    match (base.is_empty(), words.is_empty()) {
        (_, true) => base.to_owned(),
        (true, false) => words,
        (false, false) => format!("{base}, {words}"),
    }
}

/// Correct every segment of `t` (text and word timings), set
/// [`Transcript::corrections`] and return what was too close to call.
pub fn correct_transcript(vocab: &Vocab, t: &mut Transcript) -> Vec<Ambiguity> {
    let mut ambiguous: Vec<Ambiguity> = Vec::new();
    let mut corrections: Vec<Correction> = Vec::new();
    if vocab.entries.is_empty() {
        return ambiguous;
    }
    for s in &mut t.segments {
        let c = vocab.correct(&s.text);
        if !c.corrections.is_empty() {
            s.text = c.text;
        }
        correct_words(vocab, &mut s.words);
        for (from, to) in c.corrections {
            let c = Correction { from, to };
            if corrections.len() < MAX_CORRECTIONS && !corrections.contains(&c) {
                corrections.push(c);
            }
        }
        for a in c.ambiguous {
            if !ambiguous.contains(&a) {
                ambiguous.push(a);
            }
        }
    }
    t.corrections = corrections;
    ambiguous
}

/// The same replacements on the word timings: the words of a replaced span
/// become one word with the span's first start and last end. Punctuation next
/// to the span stays.
pub fn correct_words(vocab: &Vocab, words: &mut Vec<Word>) {
    if words.is_empty() || vocab.entries.is_empty() {
        return;
    }
    let mut text = String::new();
    let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(words.len());
    for w in words.iter() {
        if !text.is_empty() {
            text.push(' ');
        }
        let start = text.len();
        text.push_str(&w.text);
        ranges.push((start, text.len()));
    }
    let (_, spans) = vocab.correct_with_spans(&text);
    if spans.is_empty() {
        return;
    }
    let mut out: Vec<Word> = Vec::with_capacity(words.len());
    let mut next = 0usize;
    for sp in spans {
        let Some(a) = ranges.iter().position(|r| r.1 > sp.start) else {
            break;
        };
        let Some(b) = ranges.iter().rposition(|r| r.0 < sp.end) else {
            break;
        };
        // Two spans inside one word, or a span that starts in a word already used: leave it.
        if a < next || b < a {
            continue;
        }
        out.extend(words[next..a].iter().cloned());
        let prefix = &words[a].text[..sp.start - ranges[a].0];
        let suffix = &words[b].text[(sp.end - ranges[b].0).min(words[b].text.len())..];
        out.push(Word {
            start: words[a].start,
            end: words[b].end,
            text: format!("{prefix}{}{suffix}", sp.to),
        });
        next = b + 1;
    }
    out.extend(words[next..].iter().cloned());
    *words = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Segment;
    use lucina_vocab::Entry;

    fn vocab() -> Vocab {
        let mut e = Entry::new("Skibigårdsvej");
        e.id = "1".into();
        e.updated_at = "2".into();
        let mut h = Entry::new("Horus");
        h.id = "2".into();
        h.updated_at = "3".into();
        Vocab {
            entries: vec![e, h],
        }
    }

    fn word(start: f64, end: f64, text: &str) -> Word {
        Word {
            start,
            end,
            text: text.into(),
        }
    }

    fn transcript(text: &str, words: Vec<Word>) -> Transcript {
        Transcript {
            language: "it".into(),
            engine: "t".into(),
            duration_sec: 5.0,
            segments: vec![Segment {
                start: 0.0,
                end: 5.0,
                text: text.into(),
                speaker: None,
                words,
            }],
            speakers: false,
            created_at: chrono::Utc::now(),
            corrections: vec![],
        }
    }

    #[test]
    fn prompt_puts_the_callers_words_first_and_the_vocabulary_last() {
        let v = vocab();
        assert_eq!(prompt(&v, "Lucina"), "Lucina, Horus, Skibigårdsvej");
        assert_eq!(prompt(&v, ""), "Horus, Skibigårdsvej");
        assert_eq!(prompt(&Vocab::default(), " Lucina "), "Lucina");
        assert_eq!(prompt(&Vocab::default(), ""), "");
        // A long caller prompt does not push the vocabulary out: it stays whole, at the end.
        let long = "x".repeat(2000);
        let p = prompt(&v, &long);
        assert!(p.ends_with(", Horus, Skibigårdsvej"), "{p}");
    }

    #[test]
    fn segment_text_and_words_are_corrected_together() {
        let v = vocab();
        let words = vec![
            word(0.0, 0.4, "Torno"),
            word(0.4, 0.5, "a"),
            word(0.5, 0.9, "skibi"),
            word(0.9, 1.6, "gaardsvej,"),
            word(1.6, 2.0, "poi"),
            word(2.0, 2.4, "orus."),
        ];
        let mut t = transcript("Torno a skibi gaardsvej, poi orus.", words);
        let amb = correct_transcript(&v, &mut t);
        assert!(amb.is_empty());
        assert_eq!(t.segments[0].text, "Torno a Skibigårdsvej, poi Horus.");
        let w = &t.segments[0].words;
        let texts: Vec<_> = w.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["Torno", "a", "Skibigårdsvej,", "poi", "Horus."]);
        assert_eq!((w[2].start, w[2].end), (0.5, 1.6));
        assert_eq!(
            t.corrections,
            vec![
                Correction {
                    from: "skibi gaardsvej".into(),
                    to: "Skibigårdsvej".into()
                },
                Correction {
                    from: "orus".into(),
                    to: "Horus".into()
                },
            ]
        );
    }

    #[test]
    fn an_untouched_transcript_stays_identical() {
        let v = vocab();
        let mut t = transcript("Che ore sono?", vec![word(0.0, 1.0, "Che")]);
        let before = t.clone();
        correct_transcript(&v, &mut t);
        assert_eq!(t, before);
        let mut t2 = before.clone();
        correct_transcript(&Vocab::default(), &mut t2);
        assert_eq!(t2, before);
    }
}
