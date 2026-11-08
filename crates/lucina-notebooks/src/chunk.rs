//! Passages: a source's text cut into ~1200-character pieces on paragraph and sentence
//! boundaries, with a short overlap, each keeping the page (PDFs) or the second (videos,
//! audio) where it starts, so a citation can say "p. 7" or "12:40".

use crate::store::NewChunk;

/// A piece of extracted text and where it is in the original.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Part {
    pub text: String,
    pub page: Option<i64>,
    pub start_s: Option<f64>,
}

pub const TARGET: usize = 1200;
const MAX: usize = 1800;
const OVERLAP: usize = 180;
/// A page or segment change ends a passage that is already this long.
const BREAK_AT: usize = 400;

fn units(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split("\n\n") {
        let para = para.split_whitespace().collect::<Vec<_>>().join(" ");
        if para.is_empty() {
            continue;
        }
        if para.chars().count() <= TARGET {
            out.push(para);
            continue;
        }
        // Long paragraph: sentences, and hard cuts for run-on text.
        let mut cur = String::new();
        for word in para.split(' ') {
            cur.push_str(word);
            let ends = word.ends_with(['.', '!', '?', ';', ':']);
            if (ends && cur.chars().count() >= 80) || cur.chars().count() >= TARGET {
                out.push(std::mem::take(&mut cur));
            } else {
                cur.push(' ');
            }
        }
        let cur = cur.trim().to_owned();
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

/// The tail of `s` (whole words, at most `n` characters) to start the next passage with.
fn tail(s: &str, n: usize) -> String {
    let words: Vec<&str> = s.split(' ').collect();
    let mut out: Vec<&str> = Vec::new();
    let mut len = 0;
    for w in words.iter().rev() {
        if len + w.chars().count() + 1 > n {
            break;
        }
        len += w.chars().count() + 1;
        out.push(w);
    }
    out.reverse();
    out.join(" ")
}

pub fn chunk(parts: &[Part]) -> Vec<NewChunk> {
    let mut out: Vec<NewChunk> = Vec::new();
    let mut cur = NewChunk::default();
    let mut fresh = 0usize; // characters in `cur` that are not overlap
    let flush = |cur: &mut NewChunk, fresh: &mut usize, out: &mut Vec<NewChunk>| {
        if *fresh > 0 && !cur.text.trim().is_empty() {
            let text = cur.text.trim().to_owned();
            let next = NewChunk {
                text: tail(&text, OVERLAP),
                page: cur.page,
                start_s: cur.start_s,
            };
            out.push(NewChunk { text, ..cur.clone() });
            *cur = next;
        }
        *fresh = 0;
    };
    for part in parts {
        let moved = cur.page != part.page || (part.start_s.is_some() && cur.start_s.is_none());
        if moved && fresh >= BREAK_AT {
            flush(&mut cur, &mut fresh, &mut out);
        }
        for u in units(&part.text) {
            let ulen = u.chars().count();
            if fresh > 0 && cur.text.chars().count() + ulen > MAX.min(TARGET + ulen / 2) {
                flush(&mut cur, &mut fresh, &mut out);
            }
            if fresh == 0 {
                // A passage starts where its new text starts.
                cur.page = part.page;
                cur.start_s = part.start_s;
            }
            if !cur.text.is_empty() {
                cur.text.push(if part.start_s.is_some() { ' ' } else { '\n' });
            }
            cur.text.push_str(&u);
            fresh += ulen;
            if cur.text.chars().count() >= TARGET {
                flush(&mut cur, &mut fresh, &mut out);
            }
        }
    }
    flush(&mut cur, &mut fresh, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_passage() {
        let c = chunk(&[Part { text: "Hello.\n\nWorld.".into(), ..Default::default() }]);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].text, "Hello.\nWorld.");
    }

    #[test]
    fn pages_are_kept_and_long_text_is_cut_with_overlap() {
        let sentence = "Dynamic ARP inspection checks every ARP reply against the snooping table. ";
        let p1 = sentence.repeat(30);
        let p2 = "VLAN hopping relies on double tagging. ".repeat(5);
        let c = chunk(&[
            Part { text: p1, page: Some(1), ..Default::default() },
            Part { text: p2, page: Some(2), ..Default::default() },
        ]);
        assert!(c.len() >= 2, "{c:?}");
        assert!(c.iter().all(|x| x.text.chars().count() <= MAX), "{c:?}");
        assert_eq!(c[0].page, Some(1));
        let last = c.last().unwrap();
        assert_eq!(last.page, Some(2));
        assert!(last.text.contains("double tagging"));
        // The second passage starts with the end of the first.
        let head: String = c[1].text.chars().take(30).collect();
        assert!(c[0].text.contains(head.trim()), "{:?} / {:?}", c[0].text, c[1].text);
    }

    #[test]
    fn transcript_segments_keep_their_start() {
        let parts: Vec<Part> = (0..100)
            .map(|i| Part { text: format!("segment number {i} says something useful."), start_s: Some(i as f64 * 4.0), page: None })
            .collect();
        let c = chunk(&parts);
        assert!(c.len() > 1);
        assert_eq!(c[0].start_s, Some(0.0));
        assert!(c[1].start_s.unwrap() > 0.0);
        assert!(!c[0].text.contains('\n'));
    }

    #[test]
    fn empty_input_has_no_passages() {
        assert!(chunk(&[Part::default()]).is_empty());
        assert!(chunk(&[]).is_empty());
    }
}
