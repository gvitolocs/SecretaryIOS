//! Cheap bounded PDF Info-dictionary metadata; no page content extraction.
use crate::model::Metadata;
/// Only inspect an indirect object explicitly referenced by the final trailer's
/// /Info. Unsupported strings, streams and compressed objects use the filename.
pub fn parse(b: &[u8]) -> Metadata {
    let mut m = Metadata::default();
    let s = String::from_utf8_lossy(b);
    let Some(trailer) = s
        .rsplit_once("trailer")
        .map(|(_, t)| t)
        .filter(|t| t.contains("/Info"))
    else {
        return m;
    };
    let Some(info) = trailer.split_once("/Info").map(|(_, s)| s) else {
        return m;
    };
    let words: Vec<_> = info.split_whitespace().take(3).collect();
    if words.len() != 3
        || words[2] != "R"
        || words[..2]
            .iter()
            .any(|w| !w.chars().all(|c| c.is_ascii_digit()))
    {
        return m;
    }
    let marker = format!("{} {} obj", words[0], words[1]);
    let Some(obj) = s
        .split_once(&marker)
        .and_then(|(_, s)| s.split_once("endobj").map(|(s, _)| s))
    else {
        return m;
    };
    if obj.contains("stream") {
        return m;
    }
    fn value(s: &str, key: &str) -> Option<String> {
        let (_, r) = s.split_once(key)?;
        let r = r.trim_start().strip_prefix('(')?;
        let (v, _) = r.split_once(')')?;
        if v.contains(['\\', '(']) {
            None
        } else {
            Some(v.to_owned())
        }
    }
    m.title = value(obj, "/Title");
    if let Some(a) = value(obj, "/Author") {
        m.authors.push(a);
    }
    m
}
