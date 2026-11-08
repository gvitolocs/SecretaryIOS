//! Sentence splitter for the spoken pipeline ([`crate::voice_pipe`]): reply
//! text arrives in fragments (SSE `delta`s), speech goes out in segments.
//!
//! - A **sentence end** is `.`, `!`, `?` or `…` (a run of them, optionally
//!   followed by closing quotes or brackets) followed by white space, or a
//!   newline. A mark that is the last character of the text so far is not
//!   yet an end: the next fragment may continue it (`3` `.5`), so it waits
//!   for the next character or [`Splitter::finish`]. A `.` inside a number
//!   (`3.5`) or a word (`example.com`) is never followed by white space and so
//!   never splits; an abbreviation (`Dr. Rossi`) is allowed to.
//! - The **first** segment may also end at a comma followed by white space
//!   once it is at least [`FIRST_COMMA_CHARS`] characters long, so a long
//!   opening sentence starts speaking sooner.
//! - A segment with fewer than two letters (`1.`, `-`) is not spoken alone:
//!   it is joined to the next one.
//! - Segments leave with markdown marks dropped, as [`crate::voice`]'s
//!   `speakable` does: leading `#`, and `*`, `` ` ``, `_` anywhere. Empty
//!   ones are skipped.

/// The first segment may end at a comma from this many characters.
pub const FIRST_COMMA_CHARS: usize = 60;

const END_MARKS: &[char] = &['.', '!', '?', '…'];
const CLOSERS: &[char] = &[')', ']', '"', '\'', '»', '”', '’'];

/// Reply text as it is read aloud: markdown marks dropped, lines trimmed.
pub fn plain(text: &str) -> String {
    text.lines()
        .map(|l| l.trim_start_matches('#').trim())
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .filter(|c| !matches!(c, '*' | '`' | '_'))
        .collect::<String>()
        .trim()
        .to_owned()
}

fn letters(s: &str) -> usize {
    s.chars().filter(|c| c.is_alphabetic()).count()
}

#[derive(Debug, Default)]
pub struct Splitter {
    /// Text not yet cut into a segment.
    buf: String,
    /// A segment too short to say alone, waiting for the next one.
    held: String,
    /// Segments handed out so far.
    emitted: usize,
}

impl Splitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Segments handed out so far.
    pub fn emitted(&self) -> usize {
        self.emitted
    }

    /// Drop everything not handed out yet.
    pub fn clear(&mut self) {
        self.buf.clear();
        self.held.clear();
    }

    /// Add a fragment; the segments that are complete now, in order.
    pub fn push(&mut self, fragment: &str) -> Vec<String> {
        self.buf.push_str(fragment);
        let mut out = Vec::new();
        while let Some(cut) = self.next_cut() {
            let rest = self.buf.split_off(cut);
            let seg = std::mem::replace(&mut self.buf, rest);
            self.accept(&seg, &mut out);
        }
        out
    }

    /// The text is complete: the last segment, if any (a held short one is
    /// said together with whatever remains, or alone when nothing does).
    pub fn finish(&mut self) -> Option<String> {
        let mut out = Vec::new();
        let rest = std::mem::take(&mut self.buf);
        self.accept(&rest, &mut out);
        let tail = std::mem::take(&mut self.held);
        if !tail.is_empty() {
            let tail = plain(&tail).replace('\n', " ");
            if !tail.is_empty() {
                self.emitted += 1;
                out.push(tail);
            }
        }
        out.pop()
    }

    /// `seg` is a candidate segment: hand it out, or hold it while it is
    /// too short to be said alone.
    fn accept(&mut self, seg: &str, out: &mut Vec<String>) {
        let seg = seg.trim();
        let joined = if self.held.is_empty() {
            seg.to_owned()
        } else {
            format!("{} {}", std::mem::take(&mut self.held), seg)
        };
        if letters(&joined) < 2 {
            self.held = joined;
            return;
        }
        let text = plain(&joined).replace('\n', " ");
        if !text.is_empty() {
            self.emitted += 1;
            out.push(text);
        }
    }

    /// Byte index just after the first complete segment in `buf`.
    fn next_cut(&self) -> Option<usize> {
        let chars: Vec<(usize, char)> = self.buf.char_indices().collect();
        let mut i = 0;
        while i < chars.len() {
            let (at, c) = chars[i];
            if c == '\n' {
                if letters(&self.buf[..at]) > 0 {
                    return Some(at + 1);
                }
            } else if END_MARKS.contains(&c) {
                // The run of marks, then closers.
                let mut j = i + 1;
                while j < chars.len() && (END_MARKS.contains(&chars[j].1)) {
                    j += 1;
                }
                while j < chars.len() && CLOSERS.contains(&chars[j].1) {
                    j += 1;
                }
                match chars.get(j) {
                    Some((end, n)) if n.is_whitespace() => return Some(*end),
                    Some(_) => i = j - 1, // `3.5`, `a.b`: not an end
                    None => return None,  // may continue in the next fragment
                }
            } else if c == ',' && self.emitted == 0 && self.held.is_empty() {
                if let Some((end, n)) = chars.get(i + 1) {
                    if n.is_whitespace() && self.buf[..*end].chars().count() >= FIRST_COMMA_CHARS {
                        return Some(*end);
                    }
                }
            }
            i += 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All segments of `fragments`, pushed one by one then finished.
    fn run(fragments: &[&str]) -> Vec<String> {
        let mut s = Splitter::new();
        let mut out: Vec<String> = fragments.iter().flat_map(|f| s.push(f)).collect();
        out.extend(s.finish());
        out
    }

    #[test]
    fn splits_italian_and_english_sentences() {
        assert_eq!(
            run(&["Ciao Giuseppe! Oggi c'è il sole. Ti serve altro?"]),
            ["Ciao Giuseppe!", "Oggi c'è il sole.", "Ti serve altro?"]
        );
        assert_eq!(
            run(&["It is noon. ", "Anything else", " I can do?"]),
            ["It is noon.", "Anything else I can do?"]
        );
        assert_eq!(run(&["Boh… Non so."]), ["Boh…", "Non so."]);
        assert_eq!(run(&["Davvero?! Sì."]), ["Davvero?!", "Sì."]);
    }

    #[test]
    fn a_mark_at_the_end_waits_for_the_next_fragment() {
        let mut s = Splitter::new();
        assert!(s.push("Sono le tre.").is_empty());
        assert_eq!(s.push(" E"), ["Sono le tre."]);
        assert_eq!(s.finish().as_deref(), Some("E"));
        // The same text, cut anywhere, gives the same segments.
        let text = "Costa 3.5 euro. Va bene? Certo!";
        let whole = run(&[text]);
        assert_eq!(whole, ["Costa 3.5 euro.", "Va bene?", "Certo!"]);
        let pieces: Vec<String> = text.chars().map(String::from).collect();
        let refs: Vec<&str> = pieces.iter().map(String::as_str).collect();
        assert_eq!(run(&refs), whole);
    }

    #[test]
    fn numbers_and_names_do_not_split() {
        assert_eq!(
            run(&["Sono 3.5 chili e 12.30 euro."]),
            ["Sono 3.5 chili e 12.30 euro."]
        );
        assert_eq!(
            run(&["Vai su example.com adesso."]),
            ["Vai su example.com adesso."]
        );
        assert_eq!(run(&["Alle 3", ".5 volte"]), ["Alle 3.5 volte"]);
        // An abbreviation may split; nothing is lost.
        assert_eq!(run(&["Dr. Rossi arriva."]), ["Dr.", "Rossi arriva."]);
    }

    #[test]
    fn closing_quotes_stay_with_their_sentence() {
        assert_eq!(
            run(&["Ha detto \"vengo.\" Poi è partito (di corsa!) senza dire altro."]),
            [
                "Ha detto \"vengo.\"",
                "Poi è partito (di corsa!)",
                "senza dire altro."
            ]
        );
    }

    #[test]
    fn newlines_split_and_markdown_is_dropped() {
        assert_eq!(
            run(&["## Meteo\n**Oggi** sole `ok`\nDomani pioggia"]),
            ["Meteo", "Oggi sole ok", "Domani pioggia"]
        );
        assert_eq!(run(&["\n\n  \n"]), Vec::<String>::new());
        assert_eq!(run(&["***"]), Vec::<String>::new());
    }

    #[test]
    fn the_first_segment_may_end_at_a_late_comma() {
        let long = "Allora, ti dico che oggi la giornata sarà molto soleggiata e calda, con qualche nuvola";
        let out = run(&[long, " nel pomeriggio, poi più fresco."]);
        // "Allora," is under 60 chars; the comma after "calda" is past it.
        assert_eq!(
            out,
            [
                "Allora, ti dico che oggi la giornata sarà molto soleggiata e calda,",
                "con qualche nuvola nel pomeriggio, poi più fresco."
            ]
        );
        // Only the first segment: later commas do not split.
        let out = run(&[
            "Ok. Questa è una frase molto lunga che continua per parecchie parole, fino a diventare lunga, e poi finisce qui.",
        ]);
        assert_eq!(out.len(), 2);
        // A comma inside a number is not a cut either.
        assert_eq!(run(&["Vale 1,5 volte"]), ["Vale 1,5 volte"]);
    }

    #[test]
    fn tiny_segments_join_the_next_one() {
        assert_eq!(
            run(&["1. Prima cosa. 2. Seconda cosa."]),
            ["1. Prima cosa.", "2. Seconda cosa."]
        );
        assert_eq!(run(&["Sì."]), ["Sì."]);
        assert_eq!(run(&["- \nCiao."]), ["- Ciao."]);
        // A lone number at the end is still said.
        assert_eq!(run(&["Sono 42."]), ["Sono 42."]);
        assert_eq!(run(&["42."]), ["42."]);
    }

    #[test]
    fn plain_text_has_no_markdown_marks() {
        assert_eq!(
            plain("## Meteo\n**Oggi** sole `ok` e snake_case"),
            "Meteo\nOggi sole ok e snakecase"
        );
        assert_eq!(plain("  \n "), "");
        assert_eq!(plain("Ciao."), "Ciao.");
    }

    #[test]
    fn clear_drops_the_unsaid_text() {
        let mut s = Splitter::new();
        assert_eq!(
            s.push("Prima frase. Seconda frase in corso"),
            ["Prima frase."]
        );
        assert_eq!(s.emitted(), 1);
        s.clear();
        assert_eq!(s.finish(), None);
    }
}
