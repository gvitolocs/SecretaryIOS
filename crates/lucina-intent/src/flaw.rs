//! The deterministic part of a flaw report (intent `flaw_report`): an explicit "flaw: …", which
//! works when Jev is out, and which clients he named. A plain "emoji are boxes" is Jev's to route
//! (`lucina-server::jev_route`).

use std::sync::OnceLock;

use fancy_regex::Regex;

/// "flaw: …", "bug: …", "difetto: …", "report a flaw", "segnala un difetto", "annota questo bug".
pub fn is_report(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)^\s*(?:flaw|bug|difetto|fejl)\s*[:–-]\s*\S|\b(?:report|record|log)\s+(?:a|this|the)\s+(?:flaw|bug)\b|\b(?:segnal[ao]|annota|registra)\s+(?:un|questo|il)\s+(?:difetto|bug)\b",
        )
        .expect("flaw regex")
    })
    .is_match(text)
    .unwrap_or(false)
}

/// The clients he named, as tracker platform names.
pub fn platforms(text: &str) -> Vec<&'static str> {
    const NAMES: &[(&str, &[&str])] = &[
        ("web", &["web", "browser", "sito", "pwa", "safari", "chrome"]),
        ("mac", &["mac", "macos", "macbook", "macs"]),
        ("android", &["android", "s20"]),
        ("ios", &["ios", "iphone"]),
        ("juno", &["juno"]),
    ];
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    NAMES
        .iter()
        .filter(|(_, names)| names.iter().any(|n| words.contains(n)))
        .map(|(p, _)| *p)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_reports() {
        for t in [
            "flaw: line breaks are lost",
            "Bug - the card stays active",
            "difetto: i bottoni restano",
            "please record this flaw",
            "segnala questo bug",
            "annota un difetto: quadratini",
        ] {
            assert!(is_report(t), "{t}");
        }
        for t in [
            "emoji are boxes",
            "what is a bug bounty",
            "the bug in the garden: what is it?",
            "ho visto un difetto",
            "flaw",
        ] {
            assert!(!is_report(t), "{t}");
        }
    }

    #[test]
    fn named_clients() {
        assert_eq!(platforms("gli emoji sono quadratini su Juno"), ["juno"]);
        assert_eq!(platforms("on the iPhone and on the Mac"), ["mac", "ios"]);
        assert_eq!(platforms("nell'app web, ma non su Android"), ["web", "android"]);
        assert!(platforms("emoji are boxes").is_empty());
        assert!(platforms("I macinated the junoesque iOSes").is_empty());
    }
}
