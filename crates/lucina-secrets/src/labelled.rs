//! Secrets said in words: "the Wi-Fi password is Cavolo-23", "la password è \"cavolfiore\"",
//! "adgangskode: hemlig99". The redactor's detector ([`crate::redact`]) only knows token shapes and
//! `password=…` pairs, so a password in a sentence used to reach the model.
//!
//! This pass is deterministic and runs before any cloud model sees the owner's own text (his chat
//! messages and his notes). It is **not** in the global redactor: it would mask Lucina's own
//! sentences and agents' prose ("the password is stored in Infisical"), so its callers pick the text.
//! A plain lowercase word after a label counts only when it ends the sentence and isn't an ordinary
//! word ([`ORDINARY`]). What it still can't see: a password with no label at all.

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::Regex;

/// The masked form of a span: the detector's own label, so nothing new reaches the prompts.
pub const MASK: &str = "[secret:credential]";

/// A value in `text[start..end]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub start: usize,
    pub end: usize,
}

fn pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(concat!(
            r#"(?iu)(?:^|[^\p{L}\p{N}_])"#,
            r#"(?:pass(?:word|wd|phrase|code)|pwd|psk|pin(?:[ -]?code)?|api[ _-]?key|apikey|access[ _-]?key|"#,
            r#"secret(?:[ _-]?key)?|token|parola[ ]d'ordine|chiave(?:[ ]api)?|segreto|codice[ ]segreto|"#,
            r#"credenziali|adgangskode|kodeord|hemmelighed|nøgle)"#,
            // Up to a few words of what it is for: "password del wifi di casa è …", no sentence end in between.
            r#"(?:[ \t]+[^\s:=.!?,;]{1,20}){0,5}?"#,
            // A separator: `:` / `=` / "is" / "è" / "er".
            r#"(?:[ \t]*[:=][ \t]*|[ \t]+(?:is|è|e'|er|sarebbe|was)[ \t]+)"#,
            r#"(?:"(?P<dq>[^"\n]{4,256})"|'(?P<sq>[^'\n]{4,256})'|`(?P<bq>[^`\n]{4,256})`|«(?P<gq>[^»\n]{4,256})»|(?P<bare>[^\s"'`«»]{4,256}))"#,
        ))
        .expect("labelled regex")
    })
}

/// Ordinary words after "the password is …": not values. Italian, English, Danish.
const ORDINARY: &[&str] = &[
    "wrong",
    "right",
    "correct",
    "incorrect",
    "invalid",
    "valid",
    "expired",
    "required",
    "needed",
    "missing",
    "stored",
    "saved",
    "changed",
    "reset",
    "ready",
    "here",
    "there",
    "secure",
    "safe",
    "unsafe",
    "weak",
    "strong",
    "long",
    "short",
    "same",
    "different",
    "empty",
    "blank",
    "unknown",
    "known",
    "unset",
    "gone",
    "lost",
    "old",
    "new",
    "fine",
    "good",
    "working",
    "broken",
    "locked",
    "active",
    "revoked",
    "rotated",
    "visible",
    "hidden",
    "encrypted",
    "something",
    "nothing",
    "still",
    "already",
    "probably",
    "maybe",
    "that",
    "this",
    "what",
    "which",
    "where",
    "none",
    "null",
    "undefined",
    "true",
    "false",
    "yours",
    "mine",
    "sbagliata",
    "sbagliato",
    "giusta",
    "giusto",
    "corretta",
    "corretto",
    "errata",
    "errato",
    "scaduta",
    "scaduto",
    "valida",
    "valido",
    "richiesta",
    "richiesto",
    "necessaria",
    "necessario",
    "mancante",
    "salvata",
    "salvato",
    "cambiata",
    "cambiato",
    "pronta",
    "pronto",
    "sicura",
    "sicuro",
    "debole",
    "forte",
    "lunga",
    "lungo",
    "corta",
    "corto",
    "stessa",
    "stesso",
    "diversa",
    "diverso",
    "vuota",
    "vuoto",
    "nota",
    "noto",
    "persa",
    "perso",
    "vecchia",
    "vecchio",
    "nuova",
    "nuovo",
    "qui",
    "questa",
    "questo",
    "quella",
    "quello",
    "dove",
    "cosa",
    "forse",
    "ancora",
    "bloccata",
    "bloccato",
    "scaduto",
    "attiva",
    "attivo",
    "nascosta",
    "nascosto",
    "cifrata",
    "cifrato",
    "niente",
    "nulla",
    "tua",
    "tuo",
    "mia",
    "mio",
    "forkert",
    "rigtig",
    "rigtigt",
    "korrekt",
    "ugyldig",
    "gyldig",
    "udløbet",
    "påkrævet",
    "mangler",
    "gemt",
    "ændret",
    "klar",
    "sikker",
    "usikker",
    "svag",
    "stærk",
    "lang",
    "kort",
    "samme",
    "tom",
    "ukendt",
    "kendt",
    "væk",
    "gammel",
    "ny",
    "her",
    "der",
    "måske",
    "stadig",
    "låst",
    "aktiv",
    "skjult",
    "krypteret",
    "ingenting",
];

/// A bare value: not an ordinary word, a link or a reference.
fn bare_ok(text: &str, end: usize, v: &str) -> bool {
    let v_trimmed = v.trim_end_matches(['.', ',', ';', '!', '?', ')']);
    if v_trimmed.chars().count() < 4
        || v.contains("://")
        || v.starts_with("secret:")
        || v.starts_with('[')
    {
        return false;
    }
    let plain_word =
        v_trimmed.chars().all(|c| c.is_alphabetic()) && v_trimmed.chars().all(char::is_lowercase);
    if !plain_word {
        return true;
    }
    if ORDINARY.contains(&v_trimmed) {
        return false;
    }
    // A plain word counts only where the sentence ends: "the token is in the app" is prose.
    let rest = text[end..].trim_start_matches([' ', '\t']);
    rest.is_empty() || rest.starts_with(['\n', '\r'])
}

/// The values said in words, left to right, never overlapping.
pub fn detect(text: &str) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    for caps in pattern().captures_iter(text) {
        let quoted = ["dq", "sq", "bq", "gq"].iter().find_map(|n| caps.name(n));
        let (start, end) = match quoted {
            Some(m) => (m.start(), m.end()),
            None => {
                let Some(m) = caps.name("bare") else { continue };
                if !bare_ok(text, m.end(), m.as_str()) {
                    continue;
                }
                let kept = m.as_str().trim_end_matches(['.', ',', ';', '!', '?', ')']);
                (m.start(), m.start() + kept.len())
            }
        };
        // A placeholder, or a token shape, which the redactor's detector labels itself.
        if text[..start].ends_with("[secret:")
            || !crate::redact::detect(&text[start..end]).is_empty()
        {
            continue;
        }
        if out.last().is_none_or(|f| start >= f.end) {
            out.push(Found { start, end });
        }
    }
    out
}

/// `text` with the values said in words masked.
pub fn mask(text: &str) -> Cow<'_, str> {
    let found = detect(text);
    if found.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for f in found {
        out.push_str(&text[at..f.start]);
        out.push_str(MASK);
        at = f.end;
    }
    out.push_str(&text[at..]);
    Cow::Owned(out)
}

/// A note that is nothing but its secret: the title names a credential ("glm api", "Password",
/// "rustdex password") and the body is one short word-like value. What the sweep offers to move.
pub fn title_names_secret(title: &str) -> bool {
    let t = title.to_lowercase();
    let words: Vec<&str> = t
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    const WORDS: &[&str] = &[
        "password",
        "passwords",
        "passwd",
        "passphrase",
        "pwd",
        "psk",
        "apikey",
        "token",
        "credenziali",
        "credentials",
        "segreto",
        "segreti",
        "adgangskode",
        "kodeord",
    ];
    words.iter().any(|w| WORDS.contains(w))
        || t.contains("api key")
        || words.last().is_some_and(|w| *w == "api")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(text: &str) -> Vec<&str> {
        detect(text)
            .into_iter()
            .map(|f| &text[f.start..f.end])
            .collect()
    }

    #[test]
    fn values_said_in_words() {
        for (text, want) in [
            ("the wifi password is Cavolo-23", "Cavolo-23"),
            ("la password del wifi di casa è Cavolo-23.", "Cavolo-23"),
            ("la password è \"due parole\" grazie", "due parole"),
            ("password: hunter2", "hunter2"),
            ("psk = sup3rsecret", "sup3rsecret"),
            ("adgangskode er Hemlig99!", "Hemlig99"),
            ("API key: abc123def", "abc123def"),
            ("la password è cavolfiore", "cavolfiore"),
            ("the pin is 482913", "482913"),
            ("token: `a b c d`", "a b c d"),
        ] {
            assert_eq!(values(text), [want], "{text}");
        }
    }

    #[test]
    fn prose_about_passwords_is_left_alone() {
        for text in [
            "the password is wrong",
            "la password è sbagliata",
            "the password is stored in Infisical",
            "password expired",
            "how do I reset my password?",
            "the token is in the app",
            "adgangskoden er forkert",
            "the secret is safe",
            "chiave: https://example.com/doc",
            "[secret:credential] the password is [secret:credential]",
            "il token del bot",
            "",
        ] {
            assert!(values(text).is_empty(), "{text}: {:?}", values(text));
        }
    }

    #[test]
    fn mask_replaces_only_the_value() {
        assert_eq!(
            mask("la password del wifi è Cavolo-23, grazie"),
            format!("la password del wifi è {MASK}, grazie")
        );
        assert_eq!(mask("nothing here"), "nothing here");
    }

    #[test]
    fn a_token_shape_stays_the_detectors() {
        assert!(values(
            "usa questo token per GitHub: ghp_abcdefghijklmnopqrstuvwxyz0123456789 grazie"
        )
        .is_empty());
    }

    #[test]
    fn more_than_one() {
        let t = "gmail password: Abc-1234\nspotify password: Def-5678";
        assert_eq!(values(t), ["Abc-1234", "Def-5678"]);
    }

    #[test]
    fn titles_that_name_a_secret() {
        for t in [
            "Password",
            "rustdex password",
            "glm api",
            "dsh tunell api",
            "Il mio token",
        ] {
            assert!(title_names_secret(t), "{t}");
        }
        for t in ["Spesa", "API design notes", "Lista della spesa", ""] {
            assert!(!title_names_secret(t), "{t}");
        }
    }
}
