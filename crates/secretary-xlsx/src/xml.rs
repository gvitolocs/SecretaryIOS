//! A small XML tokenizer with exact byte spans. The patcher splices the original text, so it needs
//! to know where every tag starts and ends; OOXML parts are plain enough (no DTD) that a scanner
//! over tags, text, comments, CDATA and processing instructions covers them.

use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok<'a> {
    /// `<name attrs>` or `<name attrs/>` (`empty`); `start..end` covers the whole tag.
    Open {
        name: &'a str,
        attrs: &'a str,
        empty: bool,
        start: usize,
        end: usize,
    },
    /// `</name>`.
    Close {
        name: &'a str,
        start: usize,
        end: usize,
    },
    /// Character data between tags, still escaped.
    Text { start: usize, end: usize },
    /// `<![CDATA[..]]>`: `start..end` is the content.
    CData { start: usize, end: usize },
    /// Comments, `<?..?>`, `<!DOCTYPE ..>`.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlError(pub String, pub usize);

impl std::fmt::Display for XmlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at byte {}", self.0, self.1)
    }
}

pub struct Scanner<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> Scanner<'a> {
    pub fn new(s: &'a str) -> Self {
        Scanner { s, pos: 0 }
    }

    /// A scanner starting at `pos`, which must be a tag or text boundary.
    pub fn at(s: &'a str, pos: usize) -> Self {
        Scanner { s, pos }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    /// Continue from `pos` (a boundary found by an earlier token or [`text_until_close`]).
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }

    fn find_from(&self, from: usize, pat: &str) -> Option<usize> {
        self.s[from..].find(pat).map(|i| i + from)
    }

    fn err(&self, msg: &str) -> XmlError {
        XmlError(msg.into(), self.pos)
    }
}

impl<'a> Iterator for Scanner<'a> {
    type Item = Result<Tok<'a>, XmlError>;

    fn next(&mut self) -> Option<Self::Item> {
        let s = self.s;
        if self.pos >= s.len() {
            return None;
        }
        let start = self.pos;
        if !s[start..].starts_with('<') {
            let end = self.find_from(start, "<").unwrap_or(s.len());
            self.pos = end;
            return Some(Ok(Tok::Text { start, end }));
        }
        let rest = &s[start..];
        if rest.starts_with("<!--") {
            let Some(e) = self.find_from(start + 4, "-->") else {
                return Some(Err(self.err("unterminated comment")));
            };
            self.pos = e + 3;
            return Some(Ok(Tok::Other));
        }
        if rest.starts_with("<![CDATA[") {
            let Some(e) = self.find_from(start + 9, "]]>") else {
                return Some(Err(self.err("unterminated CDATA")));
            };
            self.pos = e + 3;
            return Some(Ok(Tok::CData {
                start: start + 9,
                end: e,
            }));
        }
        if rest.starts_with("<?") {
            let Some(e) = self.find_from(start + 2, "?>") else {
                return Some(Err(self.err("unterminated processing instruction")));
            };
            self.pos = e + 2;
            return Some(Ok(Tok::Other));
        }
        if rest.starts_with("<!") {
            let Some(e) = self.find_from(start + 2, ">") else {
                return Some(Err(self.err("unterminated declaration")));
            };
            self.pos = e + 1;
            return Some(Ok(Tok::Other));
        }
        // A tag: find its `>` outside quoted attribute values.
        let b = s.as_bytes();
        let mut i = start + 1;
        let mut quote = 0u8;
        while i < b.len() {
            let c = b[i];
            if quote != 0 {
                if c == quote {
                    quote = 0;
                }
            } else if c == b'"' || c == b'\'' {
                quote = c;
            } else if c == b'>' {
                break;
            }
            i += 1;
        }
        if i >= b.len() {
            return Some(Err(self.err("unterminated tag")));
        }
        let end = i + 1;
        self.pos = end;
        if b[start + 1] == b'/' {
            let name = s[start + 2..i].trim();
            return Some(Ok(Tok::Close { name, start, end }));
        }
        let empty = b[i - 1] == b'/';
        let inner = &s[start + 1..if empty { i - 1 } else { i }];
        let name_end = inner
            .find(|c: char| c.is_ascii_whitespace())
            .unwrap_or(inner.len());
        Some(Ok(Tok::Open {
            name: &inner[..name_end],
            attrs: &inner[name_end..],
            empty,
            start,
            end,
        }))
    }
}

/// The name without its namespace prefix.
pub fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// The attributes of a tag in order: (qualified name, unescaped value).
pub fn attrs(raw: &str) -> Vec<(&str, String)> {
    let mut out = Vec::new();
    let b = raw.as_bytes();
    let mut i = 0;
    while i < b.len() {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let ns = i;
        while i < b.len() && b[i] != b'=' && !b[i].is_ascii_whitespace() {
            i += 1;
        }
        let name = &raw[ns..i];
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() || b[i] != b'=' {
            break;
        }
        i += 1;
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let q = b[i];
        if q != b'"' && q != b'\'' {
            break;
        }
        let vs = i + 1;
        let Some(len) = raw[vs..].find(q as char) else {
            break;
        };
        out.push((name, unescape(&raw[vs..vs + len]).into_owned()));
        i = vs + len + 1;
    }
    out
}

/// One attribute's unescaped value, matched on its local name.
pub fn attr(raw: &str, name: &str) -> Option<String> {
    attrs(raw)
        .into_iter()
        .find(|(n, _)| local(n) == name)
        .map(|(_, v)| v)
}

/// Resolve the five predefined entities and character references.
pub fn unescape(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let Some(semi) = after.find(';').filter(|&k| k <= 10) else {
            out.push('&');
            rest = after;
            continue;
        };
        let ent = &after[..semi];
        let ch = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                u32::from_str_radix(&ent[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if ent.starts_with('#') => ent[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

/// Escape character data. Control characters XML 1.0 cannot carry are dropped.
pub fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// Escape an attribute value for `"…"`.
pub fn escape_attr(s: &str) -> String {
    escape_text(s)
        .replace('"', "&quot;")
        .replace('\n', "&#10;")
        .replace('\r', "&#13;")
        .replace('\t', "&#9;")
}

/// The unescaped text content between `from` (just after an opening tag) and its matching close,
/// skipping nested elements named in `skip` (e.g. `rPh`, phonetic runs). Returns the text and the
/// position after the close tag.
pub fn text_until_close(s: &str, from: usize, skip: &[&str]) -> Result<(String, usize), XmlError> {
    let mut sc = Scanner { s, pos: from };
    let mut depth = 0usize;
    let mut skipping = 0usize;
    let mut out = String::new();
    while let Some(t) = sc.next() {
        match t? {
            Tok::Open { name, empty, .. } => {
                if skipping > 0 || skip.contains(&local(name)) {
                    if !empty {
                        skipping += 1;
                    }
                } else if !empty {
                    depth += 1;
                }
            }
            Tok::Close { .. } => {
                if skipping > 0 {
                    skipping -= 1;
                } else if depth == 0 {
                    return Ok((out, sc.pos()));
                } else {
                    depth -= 1;
                }
            }
            Tok::Text { start, end } if skipping == 0 => out.push_str(&unescape(&s[start..end])),
            Tok::CData { start, end } if skipping == 0 => out.push_str(&s[start..end]),
            _ => {}
        }
    }
    Err(XmlError("unterminated element".into(), sc.pos()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_and_attrs() {
        let s = r#"<?xml version="1.0"?><a x="1 &amp; 2" y='a>b'><b/>t&lt;x<!-- c --></a>"#;
        let toks: Vec<_> = Scanner::new(s).map(|t| t.unwrap()).collect();
        match &toks[1] {
            Tok::Open {
                name,
                attrs: a,
                empty,
                start,
                end,
            } => {
                assert_eq!(*name, "a");
                assert!(!empty);
                assert_eq!(&s[*start..*end], r#"<a x="1 &amp; 2" y='a>b'>"#);
                assert_eq!(
                    attrs(a),
                    vec![("x", "1 & 2".to_string()), ("y", "a>b".to_string())]
                );
            }
            t => panic!("{t:?}"),
        }
        assert!(matches!(
            toks[2],
            Tok::Open {
                name: "b",
                empty: true,
                ..
            }
        ));
        assert!(matches!(toks[3], Tok::Text { .. }));
        assert!(matches!(toks.last(), Some(Tok::Close { name: "a", .. })));
    }

    #[test]
    fn text_skips_phonetics() {
        let s = "<si><r><t>Ab</t></r><rPh><t>x</t></rPh><t xml:space=\"preserve\"> &#x41;</t></si>";
        let (t, end) = text_until_close(s, 4, &["rPh"]).unwrap();
        assert_eq!(t, "Ab A");
        assert_eq!(end, s.len());
    }

    #[test]
    fn escaping() {
        assert_eq!(escape_text("a<b&c\u{1}"), "a&lt;b&amp;c");
        assert_eq!(escape_attr("\"x\""), "&quot;x&quot;");
        assert_eq!(unescape("&bogus; &amp"), "&bogus; &amp");
    }
}
