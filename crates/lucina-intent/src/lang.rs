//! A small deterministic language detector for the owner's messages (English,
//! Italian, Danish). It scores function words, so one foreign word inside a
//! sentence ("You know what time is the meeting therese è teresa") does not
//! flip the result. No model call: the reply and plan prompts use it to pin
//! "Reply language: ..." to the latest message instead of the language of
//! earlier turns.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    It,
    Da,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::It => "it",
            Lang::Da => "da",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::It => "Italian",
            Lang::Da => "Danish",
        }
    }
}

// Words that are shared between the languages (a, i, in, me, no, on, for, at,
// come, per, de, e) are left out on purpose.
const EN: &[&str] = &[
    "the",
    "you",
    "your",
    "what",
    "when",
    "where",
    "which",
    "who",
    "why",
    "how",
    "is",
    "are",
    "was",
    "were",
    "be",
    "been",
    "have",
    "has",
    "had",
    "do",
    "does",
    "did",
    "don",
    "doesn",
    "didn",
    "can",
    "could",
    "will",
    "would",
    "should",
    "shall",
    "may",
    "might",
    "my",
    "mine",
    "we",
    "they",
    "he",
    "she",
    "it",
    "this",
    "that",
    "these",
    "those",
    "there",
    "here",
    "with",
    "from",
    "about",
    "into",
    "of",
    "to",
    "and",
    "or",
    "but",
    "not",
    "if",
    "then",
    "so",
    "please",
    "thanks",
    "thank",
    "yes",
    "know",
    "tell",
    "show",
    "give",
    "send",
    "check",
    "time",
    "meeting",
    "today",
    "tomorrow",
    "tonight",
    "yesterday",
    "now",
    "just",
    "also",
    "all",
    "any",
    "some",
    "more",
    "me",
    "its",
    "what's",
    "whats",
    "let",
    "make",
    "need",
    "want",
    "get",
    "go",
    "see",
    "look",
    "call",
    "open",
    "close",
    "tonight",
    "morning",
    "afternoon",
    "evening",
    "again",
    "already",
    "still",
    "very",
    "much",
    "many",
    "than",
    "because",
    "after",
    "before",
    "an",
    "am",
    "i'm",
    "im",
    "ive",
    "i've",
];

const IT: &[&str] = &[
    "il",
    "lo",
    "la",
    "le",
    "gli",
    "un",
    "uno",
    "una",
    "di",
    "del",
    "dello",
    "della",
    "dei",
    "degli",
    "delle",
    "dell",
    "da",
    "dal",
    "dalla",
    "nel",
    "nella",
    "nei",
    "nelle",
    "nell",
    "sul",
    "sulla",
    "al",
    "alla",
    "ai",
    "alle",
    "all",
    "che",
    "chi",
    "cosa",
    "come",
    "perché",
    "perche",
    "quando",
    "dove",
    "quale",
    "quali",
    "quanto",
    "quanti",
    "non",
    "è",
    "sono",
    "sei",
    "siamo",
    "siete",
    "ho",
    "hai",
    "ha",
    "abbiamo",
    "hanno",
    "ero",
    "era",
    "sarà",
    "sarebbe",
    "mi",
    "ti",
    "ci",
    "vi",
    "si",
    "io",
    "tu",
    "lui",
    "lei",
    "noi",
    "voi",
    "loro",
    "mio",
    "mia",
    "miei",
    "tuo",
    "tua",
    "suo",
    "sua",
    "questo",
    "questa",
    "questi",
    "queste",
    "quello",
    "quella",
    "per",
    "con",
    "su",
    "tra",
    "fra",
    "ma",
    "anche",
    "ancora",
    "già",
    "più",
    "molto",
    "poco",
    "tutto",
    "tutti",
    "niente",
    "nulla",
    "sì",
    "ciao",
    "grazie",
    "prego",
    "scusa",
    "scusami",
    "buongiorno",
    "buonasera",
    "oggi",
    "domani",
    "ieri",
    "stasera",
    "adesso",
    "ora",
    "subito",
    "stai",
    "sta",
    "stanno",
    "parlando",
    "parli",
    "dimmi",
    "fammi",
    "fai",
    "fare",
    "puoi",
    "puoi",
    "potresti",
    "vorrei",
    "voglio",
    "devo",
    "dobbiamo",
    "cancella",
    "annulla",
    "elimina",
    "aggiungi",
    "metti",
    "ricordami",
    "avvisami",
    "riunione",
    "appuntamento",
    "calendario",
    "spesa",
    "comprare",
    "compra",
    "comprami",
    "uova",
    "pane",
    "latte",
    "limoni",
    "dentista",
    "chiamare",
    "promemoria",
    "foto",
    "tre",
    "hanno",
    "comune",
    "integrale",
    "lista",
    "mangiare",
    "andare",
    "vado",
    "faccio",
];

const DA: &[&str] = &[
    "jeg",
    "du",
    "han",
    "hun",
    "vi",
    "i",
    "de",
    "det",
    "den",
    "en",
    "et",
    "er",
    "var",
    "har",
    "havde",
    "skal",
    "vil",
    "kan",
    "må",
    "ikke",
    "og",
    "eller",
    "men",
    "som",
    "hvad",
    "hvem",
    "hvor",
    "hvornår",
    "hvorfor",
    "hvordan",
    "hvilken",
    "hvilket",
    "min",
    "mit",
    "mine",
    "din",
    "dit",
    "dine",
    "vores",
    "til",
    "fra",
    "med",
    "på",
    "af",
    "om",
    "ud",
    "ind",
    "der",
    "her",
    "denne",
    "dette",
    "disse",
    "meget",
    "mere",
    "alle",
    "noget",
    "ingen",
    "også",
    "stadig",
    "nu",
    "lige",
    "bare",
    "tak",
    "hej",
    "godmorgen",
    "godnat",
    "ja",
    "nej",
    "kalenderen",
    "kalender",
    "morgen",
    "dag",
    "aften",
    "ugen",
    "mødet",
    "møde",
    "indkøb",
    "husk",
    "minde",
    "sig",
    "mig",
    "dig",
    "os",
    "kan",
    "gør",
    "gøre",
    "fortæl",
    "vis",
    "send",
    "slet",
    "tilføj",
];

fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    // Links and attachment markers are not the owner's words.
    text.split_whitespace()
        .filter(|w| !(w.contains("://") || w.starts_with("www.") || w.starts_with('📎')))
        .flat_map(|w| w.split(|c: char| !(c.is_alphabetic() || c == '\'')))
        .filter(|w| !w.is_empty())
        .flat_map(|w| {
            // Italian elision ("dell'ora", "l'altro"): score both halves; English
            // contractions keep the whole word ("what's").
            let lower = w.to_lowercase();
            let mut out = vec![lower.trim_matches('\'').to_owned()];
            if let Some((a, _)) = lower.split_once('\'') {
                out.push(a.to_owned());
            }
            out
        })
}

fn score(words: &[String], list: &[&str]) -> usize {
    words.iter().filter(|w| list.contains(&w.as_str())).count()
}

/// The language of `text`, or `None` when there is no clear winner (no
/// function words, or a tie). Letters that exist in one language only add to
/// its score (`æ ø å` Danish; `à ì ò ù` Italian).
pub fn detect(text: &str) -> Option<Lang> {
    let words: Vec<String> = tokens(text).collect();
    let en = score(&words, EN) as i32 * 2;
    let mut it = score(&words, IT) as i32 * 2;
    let mut da = score(&words, DA) as i32 * 2;
    for c in text.chars().flat_map(char::to_lowercase) {
        match c {
            'æ' | 'ø' | 'å' => da += 1,
            'à' | 'ì' | 'ò' | 'ù' => it += 1,
            _ => {}
        }
    }
    let mut ranked = [(Lang::En, en), (Lang::It, it), (Lang::Da, da)];
    ranked.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    let [(best, top), (_, second), _] = ranked;
    (top > 0 && top > second).then_some(best)
}

/// The prompt line that pins the reply language.
pub fn language_line(l: Lang) -> String {
    format!(
        "Reply language: {n} — the owner's latest message is in {n}; earlier turns in other languages do not change this.",
        n = l.name()
    )
}

/// [`language_line`] for the language of `text` (`None` when it is not clear:
/// the generic rule applies).
pub fn reply_language_line(text: &str) -> Option<String> {
    detect(text).map(language_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_owner_texts() {
        assert_eq!(
            detect("You know what time is the meeting therese è teresa"),
            Some(Lang::En)
        );
        assert_eq!(detect("Explode outlook in your sandbox"), Some(Lang::En));
        assert_eq!(detect("Cancella"), Some(Lang::It));
        assert_eq!(detect("Di cosa stai parlando?"), Some(Lang::It));
        assert_eq!(detect("hvad har jeg i kalenderen i morgen"), Some(Lang::Da));
    }

    #[test]
    fn more_cases() {
        assert_eq!(
            detect("Ho fatto la lavanderia, poi vado alla posta per il pacco"),
            Some(Lang::It)
        );
        assert_eq!(detect("what's on my calendar tomorrow?"), Some(Lang::En));
        assert_eq!(
            detect("Hvor er mødet i dag, og hvem kommer?"),
            Some(Lang::Da)
        );
        assert_eq!(detect("Cosa c'è sull'agenda di domani"), Some(Lang::It));
    }

    #[test]
    fn the_selftest_texts_that_came_back_in_danish_are_italian() {
        for t in [
            "[selftest] Comprare: latte, uova, pane integrale, 2 limoni",
            "[selftest] promemoria: chiamare il dentista domani alle 10",
            "[selftest] tre foto: cosa hanno in comune?",
        ] {
            assert_eq!(detect(t), Some(Lang::It), "{t}");
        }
    }

    #[test]
    fn links_are_not_words() {
        assert_eq!(
            detect("https://en.wikipedia.org/wiki/The_Little_Mermaid_(statue)"),
            None
        );
        assert_eq!(
            detect("cosa dice questo https://en.wikipedia.org/the/of/and"),
            Some(Lang::It)
        );
    }

    #[test]
    fn unclear_is_none() {
        assert_eq!(detect(""), None);
        assert_eq!(detect("ok"), None);
        assert_eq!(detect("📎 voice-20261028.aac"), None);
        assert_eq!(detect("12:30"), None);
    }

    #[test]
    fn prompt_line() {
        let l = reply_language_line("Explode outlook in your sandbox").unwrap();
        assert!(l.starts_with("Reply language: English"));
        assert!(l.contains("earlier turns in other languages do not change this"));
        assert!(reply_language_line("12:30").is_none());
    }
}
