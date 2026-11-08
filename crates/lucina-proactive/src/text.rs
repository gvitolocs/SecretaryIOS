//! Small text helpers: keywords for topic matching, clipping.

use std::collections::BTreeSet;

const STOP: &[&str] = &[
    // it
    "il", "lo", "la", "le", "gli", "un", "una", "uno", "di", "da", "del", "della", "dei", "delle",
    "con", "per", "su", "tra", "fra", "che", "non", "e", "ed", "o", "ma", "se", "mi", "ti", "ci",
    "si", "al", "alla", "nel", "nella", "sono", "sei", "è", "ho", "hai", "ha", "come", "cosa",
    "quando", "dove", "questo", "questa", "anche", "ancora", "poi", "fammi", "puoi", "sapere",
    "oggi", "domani", "ieri", "grazie", "ciao", "ok", "meeting", "riunione", "incontro",
    "chiamata", "call", // en
    "the", "a", "an", "and", "or", "of", "to", "in", "on", "at", "for", "with", "is", "are", "was",
    "be", "it", "this", "that", "my", "me", "you", "your", "can", "could", "please", "let", "know",
    "if", "still", "today", "tomorrow", "about", "what", "how", "when", "check", "from", "by",
    "we", "our", "us", // da
    "og", "i", "på", "til", "med", "det", "den", "en", "et", "er", "jeg", "du", "der", "har", "om",
    "ikke", "kan", "skal", "vil", "hvad", "hvordan", "møde",
];

/// Lowercase words of 3+ characters that are not stopwords (deduplicated).
pub fn keywords(text: &str) -> BTreeSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| {
            w.chars().count() >= 3 && !STOP.contains(w) && !w.chars().all(|c| c.is_ascii_digit())
        })
        .map(str::to_owned)
        .collect()
}

/// How many keywords two texts share.
pub fn overlap(a: &BTreeSet<String>, b: &BTreeSet<String>) -> usize {
    a.intersection(b).count()
}

/// Keywords of a slug (`craftit-meeting` → `craftit`).
pub fn slug_keywords(slug: &str) -> BTreeSet<String> {
    keywords(&slug.replace(['-', '_'], " "))
}

/// Lowercase letters and digits only (`CRAFT-IT4SD` → `craftit4sd`).
pub fn compact(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// Do two texts talk about the same thing? A shared keyword, or a
/// keyword of 5+ characters of one inside the compacted other
/// (`craftit-meeting` ~ `CRAFT-IT4SD meeting`).
pub fn related(a: &str, b: &str) -> bool {
    let (ka, kb) = (keywords(a), keywords(b));
    if overlap(&ka, &kb) > 0 {
        return true;
    }
    let (ca, cb) = (compact(a), compact(b));
    let inside = |ks: &BTreeSet<String>, other: &str| {
        ks.iter()
            .any(|k| k.chars().count() >= 5 && other.contains(k.as_str()))
    };
    inside(&ka, &cb) || inside(&kb, &ca)
}

/// At most `max` characters, with an ellipsis when cut.
pub fn clip(text: &str, max: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max {
        return t.to_owned();
    }
    let cut: String = t.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// A short, stable hex digest (dedup keys that must not carry text).
pub fn digest(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0u8]);
    }
    let out = h.finalize();
    out.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_and_overlap() {
        let a = keywords("CRAFT-IT4SD — meeting with Marianne & Jonas");
        assert!(a.contains("marianne") && a.contains("jonas") && !a.contains("meeting"));
        let b = slug_keywords("craftit-meeting");
        assert!(b.contains("craftit"));
        assert_eq!(overlap(&keywords("Call with Marianne"), &a), 1);
        assert!(related(
            "craftit meeting",
            "CRAFT-IT4SD — meeting with Marianne"
        ));
        assert!(related("Call with Marianne", "marianne notes"));
        assert!(!related("Dentist", "CRAFT-IT4SD — meeting"));
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip(" ab ", 4), "ab");
        assert_eq!(digest(&["a", "b"]).len(), 16);
        assert_ne!(digest(&["ab", ""]), digest(&["a", "b"]));
    }
}
