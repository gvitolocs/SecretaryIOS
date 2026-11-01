//! [`Vocab::correct`]: conservative whole-word and word-span replacement.
//!
//! A span is one to four consecutive words (separated only by spaces, hyphens
//! or apostrophes). Against each entry's word and `soundsLike` it is either an
//! exact match (case-insensitive, always taken), a close phonetic match
//! (key distance below a threshold that depends on the length), or nothing.
//! Close but not close enough, and "two entries are equally close", are
//! reported as ambiguous and left alone.
use std::cell::RefCell;
use std::collections::HashMap;

use crate::phonetic::{fold, key, ratio};
use crate::{stop, Ambiguity, Corrected, Vocab};

const MAX_SPAN: usize = 4;
/// Tolerance for float comparisons of ratios.
const EPS: f32 = 1e-4;
/// Two entries within this much of each other's score are "equally close".
const TIE_GAP: f32 = 0.12;
/// A heard word shorter than this is never fuzzy-matched.
const MIN_HEARD_CHARS: usize = 4;
/// A key shorter than this is only matched exactly.
const MIN_KEY: usize = 4;
/// A multi-word span only matches a target with a key at least this long.
const MIN_KEY_SPAN: usize = 6;

/// `(accept, border)` for a pair whose longer key has `n` characters: a ratio
/// up to `accept` is replaced, up to `border` is reported as ambiguous.
fn limits(n: usize) -> (f32, f32) {
    match n {
        0..=3 => (0.0, 0.0),
        4..=5 => (0.10, 0.20),
        6..=8 => (0.22, 0.30),
        _ => (0.28, 0.38),
    }
}

struct Tok {
    start: usize,
    end: usize,
    lower: String,
    folded: String,
}

/// Tokens (runs of letters and digits) and, for each, whether it is joined to
/// the next one by a gap of only whitespace, hyphens or apostrophes.
fn tokenize(text: &str) -> (Vec<Tok>, Vec<bool>) {
    let mut toks: Vec<Tok> = Vec::new();
    let mut start: Option<usize> = None;
    let push = |toks: &mut Vec<Tok>, s: usize, e: usize| {
        let raw = &text[s..e];
        toks.push(Tok {
            start: s,
            end: e,
            lower: raw.to_lowercase(),
            folded: fold(raw),
        });
    };
    for (i, c) in text.char_indices() {
        if c.is_alphanumeric() {
            start.get_or_insert(i);
        } else if let Some(s) = start.take() {
            push(&mut toks, s, i);
        }
    }
    if let Some(s) = start {
        push(&mut toks, s, text.len());
    }
    let joinable = toks
        .windows(2)
        .map(|w| {
            text[w[0].end..w[1].start]
                .chars()
                .all(|c| c.is_whitespace() || matches!(c, '-' | '\'' | '\u{2019}' | '\u{2010}'))
        })
        .chain(std::iter::once(false))
        .collect();
    (toks, joinable)
}

struct Target {
    entry: usize,
    /// The alias as typed, lowercase, words joined by single spaces.
    exact: String,
    key: Vec<char>,
}

#[derive(Clone, Copy)]
struct Cand {
    entry: usize,
    /// -1 for an exact alias match, else the key ratio.
    score: f32,
    accept: f32,
}

struct Matcher<'a> {
    vocab: &'a Vocab,
    targets: Vec<Target>,
    cache: RefCell<HashMap<String, Vec<Cand>>>,
}

impl<'a> Matcher<'a> {
    fn new(vocab: &'a Vocab) -> Self {
        let mut targets: Vec<Target> = Vec::new();
        for (entry, e) in vocab.entries.iter().enumerate() {
            for a in e.aliases() {
                let (toks, _) = tokenize(a);
                let exact = toks
                    .iter()
                    .map(|t| t.lower.as_str())
                    .collect::<Vec<_>>()
                    .join(" ");
                if exact.is_empty() || targets.iter().any(|t| t.entry == entry && t.exact == exact)
                {
                    continue;
                }
                targets.push(Target {
                    entry,
                    exact,
                    key: key(a),
                });
            }
        }
        Self {
            vocab,
            targets,
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Candidates (one per entry, its best score) for the tokens `toks`.
    fn eval(&self, toks: &[Tok]) -> Vec<Cand> {
        let joined = toks
            .iter()
            .map(|t| t.lower.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let ck = format!("{}|{joined}", toks.len());
        if let Some(c) = self.cache.borrow().get(&ck) {
            return c.clone();
        }
        let mut best: HashMap<usize, Cand> = HashMap::new();
        let mut offer = |c: Cand| {
            let slot = best.entry(c.entry).or_insert(c);
            if c.score < slot.score {
                *slot = c;
            }
        };
        for t in &self.targets {
            if t.exact == joined {
                offer(Cand {
                    entry: t.entry,
                    score: -1.0,
                    accept: 0.0,
                });
            }
        }
        let letters: String = toks.iter().map(|t| t.folded.as_str()).collect();
        let eligible = if toks.len() == 1 {
            letters.chars().count() >= MIN_HEARD_CHARS && !stop::is_common(&letters)
        } else {
            letters.chars().count() > MIN_HEARD_CHARS
                && !toks.iter().all(|t| stop::is_common(&t.folded))
        };
        if eligible {
            let heard = key(&letters);
            if heard.len() >= MIN_KEY {
                for t in &self.targets {
                    if t.key.len() < MIN_KEY || (toks.len() > 1 && t.key.len() < MIN_KEY_SPAN) {
                        continue;
                    }
                    let n = heard.len().max(t.key.len());
                    let (accept, border) = limits(n);
                    // An indel costs at least 0.8, so a length gap bounds the distance.
                    let gap = heard.len().abs_diff(t.key.len()) as f32 * 0.8;
                    if gap / n as f32 > border + EPS {
                        continue;
                    }
                    let r = ratio(&heard, &t.key);
                    if r <= border + EPS {
                        offer(Cand {
                            entry: t.entry,
                            score: r,
                            accept,
                        });
                    }
                }
            }
        }
        let mut out: Vec<Cand> = best.into_values().collect();
        out.sort_by(|a, b| a.score.total_cmp(&b.score).then(a.entry.cmp(&b.entry)));
        self.cache.borrow_mut().insert(ck, out.clone());
        out
    }

    /// Candidates for the span, dropping any entry that matches the span minus
    /// its first or last word at least as well ("l'Horus" is "Horus", not a
    /// five-letter mishearing).
    fn eval_span(&self, toks: &[Tok]) -> Vec<Cand> {
        let mut c = self.eval(toks);
        if toks.len() > 1 && !c.is_empty() {
            let head = self.eval(&toks[..toks.len() - 1]);
            let tail = self.eval(&toks[1..]);
            c.retain(|x| {
                !head
                    .iter()
                    .chain(tail.iter())
                    .any(|s| s.entry == x.entry && s.score <= x.score + EPS)
            });
        }
        c
    }

    fn word(&self, entry: usize) -> &str {
        &self.vocab.entries[entry].word
    }
}

enum Decision {
    Replace(usize),
    Ambiguous(Vec<usize>),
}

fn decide(cands: &[Cand]) -> Decision {
    let exact: Vec<&Cand> = cands.iter().filter(|c| c.score < 0.0).collect();
    if !exact.is_empty() {
        return if exact.len() == 1 {
            Decision::Replace(exact[0].entry)
        } else {
            Decision::Ambiguous(exact.iter().map(|c| c.entry).collect())
        };
    }
    let best = cands[0];
    let close: Vec<usize> = cands
        .iter()
        .filter(|c| c.score - best.score < TIE_GAP)
        .map(|c| c.entry)
        .collect();
    if close.len() > 1 {
        Decision::Ambiguous(close)
    } else if best.score <= best.accept + EPS {
        Decision::Replace(best.entry)
    } else {
        Decision::Ambiguous(vec![best.entry])
    }
}

pub(crate) fn run(vocab: &Vocab, text: &str) -> Corrected {
    let mut out = Corrected {
        text: text.to_owned(),
        ..Corrected::default()
    };
    if vocab.entries.is_empty() || text.is_empty() {
        return out;
    }
    let m = Matcher::new(vocab);
    let (toks, joinable) = tokenize(text);
    let mut result = String::with_capacity(text.len());
    let mut copied = 0usize;
    let mut i = 0usize;
    while i < toks.len() {
        // The best span starting here: lowest score, shorter on ties (longer for exact).
        let mut pick: Option<(usize, Vec<Cand>)> = None;
        for len in 1..=MAX_SPAN.min(toks.len() - i) {
            if len > 1 && !joinable[i + len - 2] {
                break;
            }
            let c = m.eval_span(&toks[i..i + len]);
            let Some(top) = c.first() else { continue };
            let better = match &pick {
                None => true,
                Some((_, p)) => {
                    top.score < p[0].score - EPS || (top.score < 0.0 && p[0].score < 0.0)
                }
            };
            if better {
                pick = Some((len, c));
            }
        }
        let Some((len, cands)) = pick else {
            i += 1;
            continue;
        };
        let (s, e) = (toks[i].start, toks[i + len - 1].end);
        let heard = &text[s..e];
        match decide(&cands) {
            Decision::Replace(entry) => {
                let word = m.word(entry);
                if heard != word {
                    result.push_str(&text[copied..s]);
                    result.push_str(word);
                    copied = e;
                    out.corrections.push((heard.to_owned(), word.to_owned()));
                }
            }
            Decision::Ambiguous(entries) => {
                let mut candidates: Vec<String> = Vec::new();
                for en in entries {
                    let w = m.word(en).to_owned();
                    if !candidates.contains(&w) {
                        candidates.push(w);
                    }
                }
                // The text already reads as one of the candidates: nothing to ask.
                if !candidates.iter().any(|w| w == heard) {
                    out.ambiguous.push(Ambiguity {
                        heard: heard.to_owned(),
                        candidates,
                    });
                }
            }
        }
        i += len;
    }
    result.push_str(&text[copied..]);
    out.text = result;
    out
}
