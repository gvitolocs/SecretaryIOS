//! Vault search, with the parts of Obsidian's query syntax a phone or a laptop needs:
//! every word must match, `"a phrase"`, `-not`, `tag:x`, `path:x`, `file:x`. Case and
//! accents are ignored. Notes are read from disk one by one and nothing is kept in
//! memory; `quick_switch` needs only the names.
use crate::vault::{note_name, Vault};
use ink_obsidian::walk::{read_note, Read};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub path: String,
    pub name: String,
    pub matches: u32,
    /// First matching lines: `(0-based line, text)`, shortened around the match.
    pub lines: Vec<(u32, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Term {
    Word(String),
    Not(String),
    Tag(String),
    Path(String),
    File(String),
}

/// Lower case without the common accents: enough for é = e, ø stays ø as Obsidian's does.
fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => {
                out.push('a')
            }
            'è' | 'é' | 'ê' | 'ë' | 'È' | 'É' | 'Ê' | 'Ë' => out.push('e'),
            'ì' | 'í' | 'î' | 'ï' | 'Ì' | 'Í' | 'Î' | 'Ï' => out.push('i'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' => out.push('o'),
            'ù' | 'ú' | 'û' | 'ü' | 'Ù' | 'Ú' | 'Û' | 'Ü' => out.push('u'),
            'ñ' | 'Ñ' => out.push('n'),
            'ç' | 'Ç' => out.push('c'),
            // Combining marks of decomposed names.
            '\u{300}'..='\u{36F}' => {}
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

fn parse(query: &str) -> Vec<Term> {
    let mut terms = Vec::new();
    let mut rest = query.trim();
    while !rest.is_empty() {
        let neg = rest.starts_with('-') && rest.len() > 1;
        if neg {
            rest = &rest[1..];
        }
        let (prefix, after) = match rest.split_once(':') {
            Some((p, a))
                if matches!(p, "tag" | "path" | "file") && !a.starts_with(char::is_whitespace) =>
            {
                (Some(p), a)
            }
            _ => (None, rest),
        };
        let (word, next) = if let Some(q) = after.strip_prefix('"') {
            match q.split_once('"') {
                Some((w, n)) => (w, n),
                None => (q, ""),
            }
        } else {
            match after.split_once(char::is_whitespace) {
                Some((w, n)) => (w, n),
                None => (after, ""),
            }
        };
        rest = next.trim_start();
        if word.is_empty() {
            continue;
        }
        let w = fold(word);
        terms.push(match (prefix, neg) {
            (Some("tag"), _) => Term::Tag(word.trim_start_matches('#').to_lowercase()),
            (Some("path"), _) => Term::Path(w),
            (Some("file"), _) => Term::File(w),
            (_, true) => Term::Not(w),
            (_, false) => Term::Word(w),
        });
    }
    terms
}

fn shorten(line: &str, at: usize) -> String {
    const SIDE: usize = 60;
    let chars: Vec<char> = line.chars().collect();
    let at = line[..at.min(line.len())].chars().count();
    let from = at.saturating_sub(SIDE);
    let to = (at + SIDE).min(chars.len());
    let mut s: String = chars[from..to].iter().collect();
    s = s.trim().to_owned();
    if from > 0 {
        s.insert(0, '…');
    }
    if to < chars.len() {
        s.push('…');
    }
    s
}

impl Vault {
    /// `each` may stop the search by returning false, so a new key stroke can cancel it.
    pub fn search_with(
        &self,
        query: &str,
        limit: usize,
        mut each: impl FnMut(usize) -> bool,
    ) -> Vec<Hit> {
        let terms = parse(query);
        if terms.is_empty() {
            return Vec::new();
        }
        let mut hits = Vec::new();
        for (i, n) in self.notes().enumerate() {
            if !each(i) {
                break;
            }
            let path = fold(&n.path);
            let name = fold(n.name());
            let mut metadata_ok = true;
            for t in &terms {
                metadata_ok &= match t {
                    Term::Tag(want) => n.scan.tags.iter().any(|x| {
                        let x = x.to_lowercase();
                        x.trim_end_matches('/') == *want || x.starts_with(&format!("{want}/"))
                    }),
                    Term::Path(w) => path.contains(w.as_str()),
                    Term::File(w) => name.contains(w.as_str()),
                    _ => true,
                };
            }
            if !metadata_ok {
                continue;
            }
            let words: Vec<&String> = terms
                .iter()
                .filter_map(|t| match t {
                    Term::Word(w) => Some(w),
                    _ => None,
                })
                .collect();
            let nots: Vec<&String> = terms
                .iter()
                .filter_map(|t| match t {
                    Term::Not(w) => Some(w),
                    _ => None,
                })
                .collect();
            if words.is_empty() && nots.is_empty() {
                hits.push((
                    0u32,
                    Hit {
                        path: n.path.clone(),
                        name: n.name().to_owned(),
                        matches: 1,
                        lines: Vec::new(),
                    },
                ));
                continue;
            }
            let Ok(Read::Text(text)) = read_note(&self.root, &n.path, self.opts.max_note_bytes)
            else {
                continue;
            };
            let folded = fold(&text);
            if nots.iter().any(|w| folded.contains(w.as_str())) {
                continue;
            }
            let in_name = words.iter().filter(|w| name.contains(w.as_str())).count();
            if !words
                .iter()
                .all(|w| folded.contains(w.as_str()) || path.contains(w.as_str()))
            {
                continue;
            }
            let mut matches = 0u32;
            let mut lines = Vec::new();
            for (ln, line) in text.lines().enumerate() {
                let f = fold(line);
                let mut first = None;
                for w in &words {
                    let c = f.matches(w.as_str()).count();
                    if c > 0 && first.is_none() {
                        first = f.find(w.as_str());
                    }
                    matches += c as u32;
                }
                if let (Some(at), true) = (first, lines.len() < 3) {
                    // `at` counts folded bytes: near enough to place a window on a short line.
                    let at = at.min(line.len());
                    let mut at = at;
                    while !line.is_char_boundary(at) {
                        at -= 1;
                    }
                    lines.push((ln as u32, shorten(line, at)));
                }
            }
            let rank = (in_name as u32) * 1_000_000 + matches;
            hits.push((
                rank,
                Hit {
                    path: n.path.clone(),
                    name: n.name().to_owned(),
                    matches: matches.max(1),
                    lines,
                },
            ));
        }
        hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));
        hits.truncate(limit);
        hits.into_iter().map(|(_, h)| h).collect()
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        self.search_with(query, limit, |_| true)
    }

    /// Notes by name for a quick switcher: name starts, then word starts, then contains,
    /// aliases counted. An empty query gives nothing.
    pub fn quick_switch(&self, query: &str, limit: usize) -> Vec<String> {
        let q = fold(query.trim());
        if q.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(u8, usize, &str)> = Vec::new();
        for n in self.notes() {
            let name = fold(note_name(&n.path));
            let score = |s: &str| -> Option<u8> {
                if s.starts_with(&q) {
                    Some(0)
                } else if s.contains(&format!(" {q}")) {
                    Some(1)
                } else if s.contains(&q) {
                    Some(2)
                } else {
                    None
                }
            };
            let best = score(&name)
                .or_else(|| n.scan.aliases.iter().find_map(|a| score(&fold(a))))
                .or_else(|| fold(&n.path).contains(&q).then_some(3));
            if let Some(b) = best {
                hits.push((b, n.path.len(), &n.path));
            }
        }
        hits.sort();
        hits.into_iter()
            .take(limit)
            .map(|(_, _, p)| p.to_owned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn query_parts() {
        assert_eq!(
            parse(r#"foo "two words" -bad tag:#x/y path:"a b" file:c"#),
            vec![
                Term::Word("foo".into()),
                Term::Word("two words".into()),
                Term::Not("bad".into()),
                Term::Tag("x/y".into()),
                Term::Path("a b".into()),
                Term::File("c".into()),
            ]
        );
        assert_eq!(fold("Café Crème"), "cafe creme");
        assert_eq!(fold("Cafe\u{301}"), "cafe");
    }
    #[test]
    fn shorten_keeps_char_boundaries() {
        let line = "é".repeat(300);
        let s = shorten(&line, 300);
        assert!(s.starts_with('…') && s.ends_with('…'));
    }
}
