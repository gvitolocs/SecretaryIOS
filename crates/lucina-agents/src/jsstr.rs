//! JavaScript string semantics, for the texts that must match Paseo byte for byte.
//!
//! JS measures and cuts strings in UTF-16 code units, and its `\s` / `trim()` use
//! the ECMAScript WhiteSpace + LineTerminator set, which differs from Rust's
//! `char::is_whitespace` (JS includes U+FEFF, Rust includes U+0085).

/// `\s` in a JS regex, and the characters `String.prototype.trim` removes.
pub fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// `s.trim()`.
pub fn trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// `s.trimEnd()`.
pub fn trim_end(s: &str) -> &str {
    s.trim_end_matches(is_js_space)
}

/// `s.length`.
pub fn len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `s.slice(start, end)` with non-negative bounds (UTF-16 code units, clamped).
/// A cut through a surrogate pair keeps the lone half as U+FFFD, which is what
/// JSON.stringify and most renderers make of it.
pub fn slice(s: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    let end = end.min(units.len());
    let start = start.min(end);
    String::from_utf16_lossy(&units[start..end])
}

/// `s.slice(start)`.
pub fn slice_from(s: &str, start: usize) -> String {
    slice(s, start, usize::MAX)
}

/// `s.replace(/\s+/g, " ")`.
pub fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for c in s.chars() {
        if is_js_space(c) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out
}

/// `dropTrailingHighSurrogate(s.slice(start, end))` (Paseo `summary-text.js`):
/// a cut that would end on the first half of a pair drops that half instead.
pub fn slice_whole(s: &str, start: usize, end: usize) -> String {
    let units: Vec<u16> = s.encode_utf16().collect();
    let end = end.min(units.len());
    let start = start.min(end);
    let mut cut = &units[start..end];
    if cut.last().is_some_and(|u| (0xD800..=0xDBFF).contains(u)) {
        cut = &cut[..cut.len() - 1];
    }
    String::from_utf16_lossy(cut)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_whitespace_set() {
        assert!(is_js_space('\u{FEFF}'));
        assert!(!is_js_space('\u{0085}'));
        assert_eq!(trim("\u{FEFF} a \u{00A0}"), "a");
        assert_eq!(collapse_spaces("a \t\n b\u{3000}c"), "a b c");
    }

    #[test]
    fn utf16_lengths_and_slices() {
        assert_eq!(len("é😀"), 3);
        assert_eq!(slice("ab😀cd", 0, 3), "ab\u{FFFD}");
        assert_eq!(slice("ab😀cd", 2, 4), "😀");
        assert_eq!(slice_from("abc", 1), "bc");
        assert_eq!(slice("abc", 5, 9), "");
        assert_eq!(slice_whole("ab😀cd", 0, 3), "ab");
        assert_eq!(slice_whole("ab😀cd", 0, 4), "ab😀");
    }
}
