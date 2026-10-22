//! The owner's language (Italian by default; it follows his last messages)
//! and the few fixed strings: button labels and template fallbacks.

use crate::model::Action;

pub const LANGS: [&str; 3] = ["it", "en", "da"];

const IT: &[&str] = &[
    "che", "non", "per", "una", "sono", "come", "anche", "della", "questo", "quando", "grazie",
    "ciao", "fammi", "puoi", "domani", "oggi", "cosa", "perché", "ancora", "sei", "il", "lo",
    "gli", "di", "è", "ho", "mi", "ti", "ma", "si", "nel", "alla", "dove", "va", "bene", "allora",
];
const EN: &[&str] = &[
    "the", "and", "you", "that", "this", "with", "what", "have", "for", "are", "can", "please",
    "thanks", "today", "tomorrow", "how", "why", "when", "is", "it", "my", "me", "do", "does",
    "not", "of", "to", "will", "would", "should", "there", "about",
];
const DA: &[&str] = &[
    "og", "ikke", "det", "jeg", "du", "der", "har", "hvad", "hvordan", "tak", "hej", "også", "kan",
    "skal", "vil", "med", "til", "på", "er", "en", "et", "den", "som", "mig", "dig", "hvorfor",
    "hvornår", "godt", "være", "fra", "efter",
];

/// The language of a text by stopword counts (`None` when nothing matches).
pub fn detect(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '\'' && c != 'æ' && c != 'ø' && c != 'å')
        .filter(|w| !w.is_empty())
        .collect();
    let count = |list: &[&str]| words.iter().filter(|w| list.contains(w)).count();
    let scores = [("it", count(IT)), ("en", count(EN)), ("da", count(DA))];
    let best = scores.iter().max_by_key(|(_, n)| *n)?;
    let tie = scores.iter().filter(|(_, n)| *n == best.1).count() > 1;
    (best.1 >= 2 && !tie).then_some(best.0)
}

/// The language of his last messages (newest first), else `fallback`.
/// The most recent clear message decides; two of them must agree when the
/// newest is short.
pub fn follow_owner<'a>(
    recent_owner_texts: impl IntoIterator<Item = &'a str>,
    fallback: &str,
) -> String {
    let mut votes: Vec<&'static str> = Vec::new();
    for t in recent_owner_texts.into_iter().take(8) {
        if let Some(l) = detect(t) {
            votes.push(l);
            if votes.len() == 3 {
                break;
            }
        }
    }
    let pick = match votes.as_slice() {
        [] => None,
        [a] => Some(*a),
        [a, b, ..] if a == b => Some(*a),
        all => LANGS
            .iter()
            .copied()
            .max_by_key(|l| all.iter().filter(|v| *v == l).count()),
    };
    pick.unwrap_or(if LANGS.contains(&fallback) {
        fallback
    } else {
        "it"
    })
    .to_owned()
}

/// The three feedback buttons on every outreach.
pub fn feedback_actions(lang: &str) -> Vec<Action> {
    let labels = match lang {
        "en" => ["Useful", "Not now", "Stop these"],
        "da" => ["Nyttigt", "Ikke nu", "Stop med det"],
        _ => ["Utile", "Non ora", "Basta così"],
    };
    ["useful", "not_now", "stop"]
        .into_iter()
        .zip(labels)
        .map(|(id, label)| Action {
            id: id.into(),
            label: label.into(),
            route: None,
            feedback: true,
        })
        .collect()
}

/// A content button that opens a note in the app.
pub fn open_note_action(lang: &str, slug: &str) -> Action {
    Action {
        id: "open_note".into(),
        label: match lang {
            "en" => "Open the note",
            "da" => "Åbn noten",
            _ => "Apri la nota",
        }
        .into(),
        route: Some(format!("/notes/{slug}")),
        feedback: false,
    }
}

pub fn open_calendar_action(lang: &str) -> Action {
    Action {
        id: "open_calendar".into(),
        label: match lang {
            "en" => "Calendar",
            "da" => "Kalender",
            _ => "Calendario",
        }
        .into(),
        route: Some("/calendar".into()),
        feedback: false,
    }
}

pub fn telegram_button(lang: &str) -> &'static str {
    match lang {
        "en" => "Open in Secretary",
        "da" => "Åbn i Secretary",
        _ => "Apri in Secretary",
    }
}

pub fn pulse_title(lang: &str) -> &'static str {
    match lang {
        "en" => "Daily Pulse",
        "da" => "Dagens puls",
        _ => "Daily Pulse",
    }
}

pub fn pulse_headers(lang: &str) -> [&'static str; 3] {
    match lang {
        "en" => ["Today", "Done", "Keep an eye on"],
        "da" => ["I dag", "Klaret", "Hold øje med"],
        _ => ["Oggi", "Fatto", "Da tenere d'occhio"],
    }
}

pub fn notify_title(lang: &str) -> &'static str {
    match lang {
        "en" => "Lucina reached out",
        "da" => "Lucina skriver",
        _ => "Lucina ti scrive",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_and_follows() {
        assert_eq!(
            detect("ciao, puoi controllare se il sito è ancora giù?"),
            Some("it")
        );
        assert_eq!(
            detect("can you check if the site is still down?"),
            Some("en")
        );
        assert_eq!(detect("hej, kan du tjekke om det virker igen?"), Some("da"));
        assert_eq!(detect("ok"), None);
        assert_eq!(
            follow_owner(
                [
                    "ok",
                    "thanks, can you check it for me?",
                    "is the build done?"
                ],
                "it"
            ),
            "en"
        );
        assert_eq!(follow_owner(["👍"], "it"), "it");
        assert_eq!(follow_owner(Vec::<&str>::new(), "xx"), "it");
        assert_eq!(feedback_actions("da")[2].label, "Stop med det");
        assert!(feedback_actions("it").iter().all(|a| a.feedback));
    }
}
