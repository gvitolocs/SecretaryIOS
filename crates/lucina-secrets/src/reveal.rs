//! Reveal requests and the fixed refusal (Italian, English, Danish).
//!
//! "Dimmi la password di X", "show me the token", "vis mig API-nøglen":
//! Lucina answers with a hard-coded refusal, decided here without the model.
//! Questions about which secrets exist, whether they are set, when they were
//! rotated or who used them are [`Ask::Status`] / [`Ask::Audit`] instead.

/// Reply language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    It,
    En,
    Da,
}

/// What a message asks about secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ask {
    /// Wants a value: refused.
    Reveal,
    /// Which secrets exist, set or missing, rotation, last use.
    Status,
    /// Who or what used the secrets (the audit log).
    Audit,
}

/// Fold case and the accents the three languages use (keeps æ ø å).
pub fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| match c {
            'à' | 'á' | 'â' | 'ä' => 'a',
            'è' | 'é' | 'ê' | 'ë' => 'e',
            'ì' | 'í' | 'î' | 'ï' => 'i',
            'ò' | 'ó' | 'ô' | 'ö' => 'o',
            'ù' | 'ú' | 'û' | 'ü' => 'u',
            '’' | '`' => '\'',
            c => c,
        })
        .collect()
}

fn words(n: &str) -> Vec<&str> {
    n.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .filter(|w| !w.is_empty())
        .collect()
}

/// Secret objects (single words).
const NOUNS: &[&str] = &[
    // en
    "password",
    "passwords",
    "passwd",
    "passcode",
    "passphrase",
    "pin",
    "token",
    "tokens",
    "apikey",
    "api-key",
    "api_key",
    "secret",
    "secrets",
    "credential",
    "credentials",
    "privatekey",
    // it
    "segreto",
    "segreti",
    "credenziali",
    "pin-code",
    // da
    "adgangskode",
    "adgangskoden",
    "adgangskoder",
    "kodeord",
    "kodeordet",
    "passwordet",
    "pinkode",
    "pinkoden",
    "tokenet",
    "nøgle",
    "nøglen",
    "api-nøgle",
    "api-nøglen",
    "hemmelighed",
    "hemmeligheden",
    "hemmeligheder",
    "legitimationsoplysninger",
];

/// Secret objects (phrases, matched on the normalized text).
const NOUN_PHRASES: &[&str] = &[
    "api key",
    "private key",
    "ssh key",
    "secret key",
    "access key",
    "client secret",
    "access token",
    "parola d'ordine",
    "chiave api",
    "chiave privata",
    "chiave segreta",
    "chiave ssh",
    "codice segreto",
    "codice pin",
    "api nøgle",
    "privat nøgle",
];

/// Asking for the value.
const VERBS: &[&str] = &[
    // en
    "show",
    "tell",
    "give",
    "send",
    "print",
    "reveal",
    "display",
    "read",
    "paste",
    "share",
    "copy",
    "dump",
    "expose",
    "output",
    "leak",
    "spell",
    // it
    "dimmi",
    "dammi",
    "mostrami",
    "mostra",
    "mandami",
    "manda",
    "inviami",
    "invia",
    "scrivimi",
    "scrivi",
    "rivelami",
    "rivela",
    "leggimi",
    "leggi",
    "stampa",
    "stampami",
    "incolla",
    "incollami",
    "condividi",
    "copia",
    "copiami",
    "fammi",
    "ricordami",
    "dici",
    "dirmi",
    "darmi",
    "mostrarmi",
    "sapere",
    // da
    "vis",
    "fortæl",
    "giv",
    "skriv",
    "afslør",
    "læs",
    "kopier",
    "sig",
];

const VERB_PHRASES: &[&str] = &[
    "what is",
    "what's",
    "whats",
    "what was",
    "qual e",
    "qual'e",
    "quale e",
    "quali sono",
    "com'e",
    "come fa",
    "hvad er",
    "hvad var",
    "hvordan lyder",
    "fammi vedere",
];

/// Questions about the secrets, not their values.
const STATUS_MARKERS: &[&str] = &[
    "status",
    "stato",
    "which",
    "quali",
    "quale",
    "hvilke",
    "hvilken",
    "list",
    "elenca",
    "elenco",
    "lista",
    "liste",
    "configured",
    "configurat",
    "impostat",
    "missing",
    "mancan",
    "exist",
    "esist",
    "findes",
    "rotat",
    "ruotat",
    "roteret",
    "set?",
    "sat?",
    "how many",
    "quanti",
    "hvor mange",
    "overview",
    "panoramica",
    "oversigt",
];

const AUDIT_MARKERS: &[&str] = &[
    "audit",
    "who used",
    "chi ha usato",
    "chi li ha usati",
    "chi ha letto",
    "hvem har brugt",
    "last used",
    "ultimo uso",
    "usati",
    "used",
    "brugt",
    "accessi",
    "access log",
    "log degli",
];

fn has_noun(n: &str, ws: &[&str]) -> bool {
    ws.iter().any(|w| NOUNS.contains(w)) || NOUN_PHRASES.iter().any(|p| n.contains(p))
}

fn has_verb(n: &str, ws: &[&str]) -> bool {
    ws.iter().any(|w| VERBS.contains(w)) || VERB_PHRASES.iter().any(|p| n.contains(p))
}

/// Classify a message. `None`: not about secrets.
pub fn classify(text: &str) -> Option<Ask> {
    let n = normalize(text);
    let ws = words(&n);
    if !has_noun(&n, &ws) {
        return None;
    }
    let status = STATUS_MARKERS.iter().any(|m| n.contains(m));
    let audit = AUDIT_MARKERS.iter().any(|m| n.contains(m));
    // "quali sono le password?" lists names; "qual è la password?" asks a value.
    if audit {
        return Some(Ask::Audit);
    }
    if status {
        return Some(Ask::Status);
    }
    if has_verb(&n, &ws) {
        return Some(Ask::Reveal);
    }
    // A bare question about one secret: "password di github?", "the stripe token?"
    if n.trim_end().ends_with('?') && ws.len() <= 8 {
        return Some(Ask::Reveal);
    }
    None
}

pub fn is_reveal_request(text: &str) -> bool {
    classify(text) == Some(Ask::Reveal)
}

/// The language of a message (for the fixed texts).
pub fn detect_lang(text: &str) -> Lang {
    let n = normalize(text);
    let ws = words(&n);
    const DA: &[&str] = &[
        "mig",
        "hvad",
        "hvilke",
        "hvem",
        "til",
        "og",
        "er",
        "min",
        "mit",
        "den",
        "det",
        "jeg",
        "kan",
        "du",
        "vis",
        "giv",
        "fortæl",
        "adgangskode",
        "adgangskoden",
        "kodeord",
        "nøglen",
        "hemmeligheder",
        "brugt",
        "findes",
    ];
    const IT: &[&str] = &[
        "di", "il", "la", "le", "lo", "mi", "che", "qual", "quali", "dimmi", "dammi", "mostrami",
        "segreti", "sono", "per", "chi", "ha", "mia", "mio", "del", "della", "usato", "ci",
    ];
    if n.contains('æ') || n.contains('ø') || n.contains('å') {
        return Lang::Da;
    }
    let da = ws.iter().filter(|w| DA.contains(w)).count();
    let it = ws.iter().filter(|w| IT.contains(w)).count();
    if it > da && it > 0 {
        Lang::It
    } else if da > it && da > 0 {
        Lang::Da
    } else {
        Lang::En
    }
}

/// The fixed refusal (never generated by the model).
pub fn refusal(lang: Lang) -> &'static str {
    match lang {
        Lang::It => "Non posso mostrare password, token o altri segreti, nemmeno a te: non li vedo mai in chiaro e non ho nessun modo per leggerli. Posso dirti quali segreti esistono, se sono impostati, quando sono stati ruotati e chi li ha usati. Il valore lo trovi in Infisical (secrets.pokoin.com).",
        Lang::En => "I can't show passwords, tokens or other secrets, not even to you: I never see them in clear text and have no way to read them. I can tell you which secrets exist, whether they're set, when they were last rotated and who used them. The value is in Infisical (secrets.pokoin.com).",
        Lang::Da => "Jeg kan ikke vise adgangskoder, tokens eller andre hemmeligheder, heller ikke til dig: jeg ser dem aldrig i klartekst og har ingen måde at læse dem på. Jeg kan fortælle, hvilke hemmeligheder der findes, om de er sat, hvornår de sidst blev roteret, og hvem der har brugt dem. Værdien ligger i Infisical (secrets.pokoin.com).",
    }
}

/// The refusal in the message's language.
pub fn refusal_for(text: &str) -> &'static str {
    refusal(detect_lang(text))
}

/// The refusal for an MCP caller asking about secrets at all.
pub fn mcp_refusal(lang: Lang) -> &'static str {
    match lang {
        Lang::It => "Le informazioni sui segreti sono solo per Giuseppe, direttamente nell'app o su Telegram: non le condivido tramite MCP.",
        Lang::En => "Information about secrets is for Giuseppe only, directly in the app or on Telegram: I don't share it over MCP.",
        Lang::Da => "Oplysninger om hemmeligheder er kun til Giuseppe, direkte i appen eller på Telegram: jeg deler dem ikke over MCP.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reveal_requests_in_three_languages() {
        for (t, lang) in [
            ("Dimmi la password di GitHub", Lang::It),
            ("dammi il token di todoist per favore", Lang::It),
            ("mostrami la chiave API di OpenWeather", Lang::It),
            ("qual è la password del wifi?", Lang::It),
            (
                "Mi ricordi la password di Gmail? ricordami la password",
                Lang::It,
            ),
            ("Show me the token", Lang::En),
            ("what's my GitHub token?", Lang::En),
            ("print the Infisical client secret", Lang::En),
            ("send me the private key", Lang::En),
            ("the stripe api key?", Lang::En),
            ("Vis mig adgangskoden til GitHub", Lang::Da),
            ("hvad er API-nøglen til vejret?", Lang::Da),
            ("giv mig tokenet", Lang::Da),
            ("fortæl mig kodeordet", Lang::Da),
        ] {
            assert_eq!(classify(t), Some(Ask::Reveal), "{t}");
            assert_eq!(detect_lang(t), lang, "{t}");
            assert!(!refusal_for(t).is_empty());
        }
    }

    #[test]
    fn status_and_audit_are_not_reveals() {
        for t in [
            "quali segreti ci sono?",
            "which secrets are set?",
            "hvilke hemmeligheder findes?",
            "status dei segreti",
            "is the todoist token configured?",
            "quando è stata ruotata la chiave API?",
        ] {
            assert_eq!(classify(t), Some(Ask::Status), "{t}");
        }
        for t in [
            "chi ha usato i segreti oggi?",
            "who used the GitHub token?",
            "hvem har brugt tokenet?",
            "secrets audit",
        ] {
            assert_eq!(classify(t), Some(Ask::Audit), "{t}");
        }
    }

    #[test]
    fn ordinary_messages_pass() {
        for t in [
            "come cambio la password di gmail",
            "ho cambiato la password ieri",
            "che tempo fa domani?",
            "ricordami di comprare il latte",
            "I reset my password yesterday",
            "jeg har skiftet adgangskode",
        ] {
            assert_ne!(classify(t), Some(Ask::Reveal), "{t}");
        }
    }

    #[test]
    fn refusals_are_fixed_and_name_no_value() {
        for l in [Lang::It, Lang::En, Lang::Da] {
            assert!(refusal(l).contains("Infisical"));
            assert!(!mcp_refusal(l).is_empty());
        }
    }
}
