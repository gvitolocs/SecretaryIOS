//! JavaScript-compatible regular expressions on top of `fancy-regex`, so the
//! Node patterns can be copied verbatim.
//!
//! Translated so that matching behaves as in V8:
//! - `\b`/`\B` are ASCII word boundaries (JS never treats `è` or `å` as a word
//!   character, even with the `u` flag), written as lookarounds;
//! - `\d`, `\w` (and `\D`, `\W`) are ASCII classes;
//! - `\uXXXX` and `\u{X}` become `\x{X}`; identity escapes of quotes and `/`
//!   become the plain character.
//!
//! Flags: `i` (case-insensitive), `m`, `s`; `u` and `g` change nothing here.
//! Every public pattern is compiled once (see [`re!`]) and covered by tests.

use std::borrow::Cow;

/// Captures of a match on a `&str`.
pub type Caps<'t> = fancy_regex::Captures<'t, str>;

const WORD: &str = "A-Za-z0-9_";

fn boundary(negated: bool) -> String {
    let w = format!("[{WORD}]");
    if negated {
        format!("(?:(?<={w})(?={w})|(?<!{w})(?!{w}))")
    } else {
        format!("(?:(?<={w})(?!{w})|(?<!{w})(?={w}))")
    }
}

/// JS pattern source to a fancy-regex pattern (flags not included).
pub fn translate(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len() + 16);
    let mut in_class = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            let n = chars[i + 1];
            i += 2;
            match n {
                'b' if !in_class => out.push_str(&boundary(false)),
                'B' if !in_class => out.push_str(&boundary(true)),
                'b' => out.push_str("\\x08"),
                'd' if in_class => out.push_str("0-9"),
                'd' => out.push_str("[0-9]"),
                'D' if !in_class => out.push_str("[^0-9]"),
                'w' if in_class => out.push_str(WORD),
                'w' => out.push_str(&format!("[{WORD}]")),
                'W' if !in_class => out.push_str(&format!("[^{WORD}]")),
                'u' if i < chars.len() && chars[i] == '{' => {
                    let end = chars[i..].iter().position(|&x| x == '}').map(|p| i + p);
                    let end = end.expect("unterminated \\u{");
                    let hex: String = chars[i + 1..end].iter().collect();
                    out.push_str(&format!("\\x{{{hex}}}"));
                    i = end + 1;
                }
                'u' if i + 4 <= chars.len()
                    && chars[i..i + 4].iter().all(|x| x.is_ascii_hexdigit()) =>
                {
                    let hex: String = chars[i..i + 4].iter().collect();
                    out.push_str(&format!("\\x{{{hex}}}"));
                    i += 4;
                }
                '\'' | '"' | '/' | '`' | '’' | '‘' => out.push(n),
                _ => {
                    out.push('\\');
                    out.push(n);
                }
            }
            continue;
        }
        match c {
            '[' if !in_class => {
                in_class = true;
                out.push(c);
                // `[]` / `[^]` edge cases are not used by the Node patterns.
                if i + 1 < chars.len() && chars[i + 1] == '^' {
                    out.push('^');
                    i += 1;
                }
            }
            ']' if in_class => {
                in_class = false;
                out.push(c);
            }
            // A literal `[` inside a class must be escaped for the Rust parser.
            '[' if in_class => out.push_str("\\["),
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

/// A compiled JS-style regex.
pub struct Re {
    re: fancy_regex::Regex,
}

impl std::fmt::Debug for Re {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Re({})", self.re.as_str())
    }
}

impl Re {
    /// Compile `src` with JS `flags`. Panics on an invalid pattern (they are
    /// static and tested).
    pub fn new(src: &str, flags: &str) -> Self {
        Self::try_new(src, flags).unwrap_or_else(|e| panic!("bad regex {src:?}: {e}"))
    }

    pub fn try_new(src: &str, flags: &str) -> Result<Self, fancy_regex::Error> {
        let mut prefix = String::new();
        for f in ['i', 'm', 's'] {
            if flags.contains(f) {
                prefix.push(f);
            }
        }
        let body = translate(src);
        let pattern = if prefix.is_empty() {
            body
        } else {
            format!("(?{prefix}){body}")
        };
        let re = fancy_regex::RegexBuilder::new(&pattern)
            .backtrack_limit(2_000_000)
            .build()?;
        Ok(Self { re })
    }

    /// `re.test(s)`; a backtracking overflow counts as no match.
    pub fn test(&self, s: &str) -> bool {
        self.re.is_match(s).unwrap_or(false)
    }

    /// `s.match(re)` / `re.exec(s)` without `g`.
    pub fn exec<'t>(&self, s: &'t str) -> Option<Caps<'t>> {
        self.re.captures(s).ok().flatten()
    }

    /// The first match: `(byte start, byte end, text)`.
    pub fn find<'t>(&self, s: &'t str) -> Option<(usize, usize, &'t str)> {
        self.re
            .find(s)
            .ok()
            .flatten()
            .map(|m| (m.start(), m.end(), m.as_str()))
    }

    /// `s.replace(re, rep)` (first match), `rep` literal.
    pub fn replace<'t>(&self, s: &'t str, rep: &str) -> Cow<'t, str> {
        match self.find(s) {
            Some((a, b, _)) => Cow::Owned(format!("{}{}{}", &s[..a], rep, &s[b..])),
            None => Cow::Borrowed(s),
        }
    }

    /// `s.replace(/re/g, rep)`, `rep` literal.
    pub fn replace_all(&self, s: &str, rep: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        let mut pos = 0;
        while pos <= s.len() {
            let Some(m) = self.re.find_from_pos(s, pos).ok().flatten() else {
                break;
            };
            out.push_str(&s[last..m.start()]);
            out.push_str(rep);
            last = m.end();
            pos = if m.end() == m.start() {
                // Empty match: step one character.
                match s[m.end()..].chars().next() {
                    Some(c) => m.end() + c.len_utf8(),
                    None => break,
                }
            } else {
                m.end()
            };
        }
        out.push_str(&s[last..]);
        out
    }

    /// Every match (`s.matchAll(/re/g)`), as text.
    pub fn find_all<'t>(&self, s: &'t str) -> Vec<&'t str> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos <= s.len() {
            let Some(m) = self.re.find_from_pos(s, pos).ok().flatten() else {
                break;
            };
            out.push(m.as_str());
            pos = if m.end() == m.start() {
                match s[m.end()..].chars().next() {
                    Some(c) => m.end() + c.len_utf8(),
                    None => break,
                }
            } else {
                m.end()
            };
        }
        out
    }

    pub fn as_str(&self) -> &str {
        self.re.as_str()
    }
}

/// A lazily compiled static [`Re`]: `re!(r"\bciao\b", "i")`.
#[macro_export]
macro_rules! re {
    ($src:expr) => {
        $crate::re!($src, "")
    };
    ($src:expr, $flags:expr) => {{
        static RE: std::sync::OnceLock<$crate::jsre::Re> = std::sync::OnceLock::new();
        RE.get_or_init(|| $crate::jsre::Re::new($src, $flags))
    }};
}

/// Group `n` of a capture, or "".
pub fn group<'t>(c: &Caps<'t>, n: usize) -> &'t str {
    c.get(n).map(|m| m.as_str()).unwrap_or("")
}

/// JS `String.prototype.trim` (whitespace + line terminators + BOM).
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// JS `toLowerCase` for the cases that matter here.
pub fn lower(s: &str) -> String {
    s.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_word_boundaries_like_v8() {
        // `\b` never matches after "è" (not an ASCII word char).
        assert!(!Re::new(r"\bc'è\b", "i").test("cosa c'è nella lista"));
        assert!(Re::new(r"\bc'è", "i").test("cosa c'è nella lista"));
        assert!(Re::new(r"\bciao\b", "i").test("Ciao, Lucina"));
        assert!(!Re::new(r"\bham\b", "i").test("shampoo"));
        // Danish letters are not word characters for `\b` either.
        assert!(!Re::new(r"\bpå\b", "i").test("tag den på"));
        assert!(Re::new(r"\bp", "").test("på"));
    }

    #[test]
    fn unicode_lookarounds_and_escapes() {
        let w = Re::new(r"(?<![\p{L}\p{N}_])(?:træning)(?![\p{L}\p{N}_])", "iu");
        assert!(w.test("tag træning på"));
        assert!(!w.test("træningstøj"));
        assert!(Re::new(r"cosa c\'è", "i").test("Cosa c'è"));
        assert!(Re::new(r"er du vågen", "i").test("er du vågen"));
        assert!(Re::new(r"[\w-]+", "").test("a-b"));
        assert!(Re::new(r"^\d{4}-\d{2}$", "").test("2026-10"));
        assert_eq!(Re::new(r"\s+", "").replace_all("a  b\tc", " "), "a b c");
        assert_eq!(Re::new(r"x", "").replace("axbx", "-"), "a-bx");
        let r = Re::new(r"(?<!\breal\s)\bestate\b", "i");
        assert!(r.test("quest'estate"));
        assert!(!r.test("real estate"));
    }
}
