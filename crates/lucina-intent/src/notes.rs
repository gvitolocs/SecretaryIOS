//! The notes bridge's parser and texts (`src/notes-bridge.js`): "salva
//! questo come nota X: …", "memorizza il briefing nel contesto 'X'", "cosa
//! c'è nella nota X", "mostrami le note", multi-part saves.

use std::sync::OnceLock;

use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use crate::jsre::{group, Re};

pub const MAX_CONTEXT_CHARS: usize = 6000;
pub const MAX_CONTEXT_NOTES: usize = 2;
pub const MAX_NOTE_BYTES: usize = 256 * 1024;

/// `(?<![\p{L}\p{N}_])(?:src)(?![\p{L}\p{N}_])`
pub fn words_src(src: &str) -> String {
    format!(r"(?<![\p{{L}}\p{{N}}_])(?:{src})(?![\p{{L}}\p{{N}}_])")
}

fn words(src: &str) -> Re {
    Re::new(&words_src(src), "iu")
}

const QUOTE_OPEN: &str = "['\"“‘«]";
const QUOTE_CLOSE: &str = "['\"”’»]";
const NOUN: &str = r"(?:nota|note|noten|notat(?:et)?|appunt[oi]|briefing|contesto|context|kontekst(?:en)?|documento|document|dokument(?:et)?|memo)";
/// Words that are never a note name ("read the document I sent you").
const NAME_STOP: &[&str] = &[
    "per", "di", "del", "della", "e", "che", "con", "for", "and", "the", "of", "to", "og", "til",
    "med", "questo", "questa", "this", "it", "dette", "denne", "qui", "here", "her", "i", "you",
    "me", "we", "he", "she", "they", "that", "which", "what", "my", "your", "a", "an", "cui", "io",
    "tu", "noi", "lui", "lei", "mi", "ti", "ci", "mia", "mio", "tua", "tuo", "jeg", "du", "vi",
    "han", "hun", "de", "det", "som", "min", "mit", "din", "dit",
];
const SAVE_VERBS: [(&str, &str); 3] = [
    (
        "it",
        r"salva(?:mi|lo|la)?|memorizza(?:lo|la)?|archivia(?:lo|la)?|conserva(?:lo|la)?|annota(?:lo|la)?|registra(?:lo|la)?|tieni(?:lo|la)?|metti(?:lo|la)?|ricorda(?:ti|lo|la)?",
    ),
    ("en", r"save|store|keep|memori[sz]e|put|remember"),
    ("da", r"gem|husk|læg"),
];

fn name() -> String {
    format!(
        r#"(?:{QUOTE_OPEN}([^'"“”‘’«»\n]{{1,80}}){QUOTE_CLOSE}|([\p{{L}}\p{{N}}][\p{{L}}\p{{N}}_-]{{0,63}}))"#
    )
}

struct Patterns {
    save_named: Vec<(&'static str, Re)>,
    save_unnamed: Vec<(&'static str, Re)>,
    read: Vec<(&'static str, Re)>,
    list: Vec<(&'static str, Re)>,
    noun: Re,
    first_part: Re,
    later_part: Re,
    later_part_lead: Re,
    store_like: Re,
}

fn p() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| {
        let name = name();
        Patterns {
            save_named: SAVE_VERBS
                .iter()
                .map(|(lang, verbs)| {
                    (*lang, words(&format!(r"(?:{verbs})(?:[^\n.:;]{{0,60}}?)\s(?:come|as|som|nel(?:la)?|in|into|under|i|sotto)\s+(?:(?:il|la|lo|l'|the|a|an|en|et)\s*)?(?:{NOUN})\s+{name}")))
                })
                .collect(),
            save_unnamed: SAVE_VERBS
                .iter()
                .map(|(lang, verbs)| {
                    (*lang, words(&format!(r"(?:{verbs})\s+(?:(?:questo|questa|il|la|this|the|a|dette|denne|en|et)\s+)?(?:come\s+|as\s+(?:a\s+)?|som\s+)?(?:{NOUN})\s*:")))
                })
                .collect(),
            read: vec![
                ("it", words(&format!(r"(?:cosa\s+c['’]?\s*è|(?:cosa|cos['’])\s*ho(?:\s+(?:scritto|salvato|messo|annotato))?|cosa\s+dice|che\s+cosa\s+c['’]?\s*è|leggi(?:mi)?|mostrami|fammi\s+vedere|apri|riassumi(?:mi)?|ricordati|ricordi|cosa\s+sai)\s+(?:di\s+|del(?:la|lo)?\s+|nel(?:la|lo)?\s+|l['’]\s*|la\s+|il\s+|lo\s+|in\s+|sul(?:la)?\s+)?(?:mia\s+|mio\s+)?(?:{NOUN})\s+{name}"))),
                ("en", words(&format!(r"(?:what(?:['’]s|\s+is)\s+in|what\s+(?:do\s+i\s+have|did\s+i\s+(?:write|save|put))\s+in|what\s+does|read|show(?:\s+me)?|open|summari[sz]e|remember|recall|what\s+do\s+you\s+know\s+about)\s+(?:the\s+|my\s+)?(?:{NOUN})\s+{name}"))),
                ("da", words(&format!(r"(?:hvad\s+står\s+der\s+i|hvad\s+har\s+jeg(?:\s+(?:skrevet|gemt))?\s+i|vis(?:\s+mig)?|læs|åbn|husk|opsummer)\s+(?:min\s+|mit\s+)?(?:{NOUN})\s+{name}"))),
            ],
            list: vec![
                ("it", words(r"(?:mostrami|elenca(?:mi)?|fammi\s+vedere|quali\s+sono)\s+(?:tutte\s+)?(?:le\s+)?(?:mie\s+)?note|(?:che|quali)\s+note\s+(?:ho|abbiamo|hai)|lista\s+(?:delle\s+)?note|^\s*le\s+mie\s+note\s*[?.!]*$")),
                ("en", words(r"(?:show|list)(?:\s+me)?\s+(?:all\s+)?(?:my\s+|the\s+)?notes|what\s+notes\s+(?:do\s+i\s+have|are\s+there)|^\s*my\s+notes\s*[?.!]*$")),
                ("da", words(r"(?:vis|list)(?:\s+mig)?\s+(?:alle\s+)?(?:mine\s+)?noter|hvilke\s+noter\s+har\s+jeg|^\s*mine\s+noter\s*[?.!]*$")),
            ],
            noun: words(NOUN),
            first_part: words(r"parte\s+1\s+di\s+\d|prima\s+parte|part\s+1\s+of\s+\d|first\s+part|del\s+1\s+af\s+\d|første\s+del"),
            later_part: words(r"(?:second[ao]|terz[ao]|quart[ao]|ultima)\s+parte|parte\s+[2-9](?:\s+di\s+\d)?|(?:second|third|fourth|last|final)\s+part|part\s+[2-9](?:\s+of\s+\d)?|(?:anden|tredje|sidste)\s+del|del\s+[2-9](?:\s+af\s+\d)?"),
            later_part_lead: Re::new(r"^(?:\s*lucina[,:]?\s*)?[^\n.:]{0,40}?(?:second[ao]|terz[ao]|quart[ao]|ultima|parte\s+[2-9]|second|third|last|final|part\s+[2-9]|anden|tredje|sidste|del\s+[2-9])[^\n.:]{0,120}[.:]\s*", "iu"),
            store_like: words(&format!(r"(?:memorizza|salva|conserva|archivia|ricorda(?:ti)?|store|save|keep|memori[sz]e|gem|husk)\s+(?:[^\n.]{{0,40}}\s)?(?:{NOUN})")),
        }
    })
}

/// A notes request.
#[derive(Debug, Clone, PartialEq)]
pub enum NotesIntent {
    /// Save (content may be empty: ask for it). `more`: "parte 1 di N".
    Save {
        name: String,
        slug: String,
        title: String,
        content: String,
        more: bool,
        lang: &'static str,
    },
    Read {
        name: String,
        lang: &'static str,
    },
    List {
        lang: &'static str,
    },
    /// The next part of a multi-part save.
    Part {
        content: String,
        more: bool,
        lang: &'static str,
    },
}

impl NotesIntent {
    pub fn lang(&self) -> &'static str {
        match self {
            Self::Save { lang, .. }
            | Self::Read { lang, .. }
            | Self::List { lang }
            | Self::Part { lang, .. } => lang,
        }
    }
}

fn pick_name(c: &crate::jsre::Caps<'_>) -> String {
    let quoted = group(c, 1).trim();
    if !quoted.is_empty() {
        return quoted.to_owned();
    }
    let bare = group(c, 2).trim();
    if bare.is_empty() || NAME_STOP.contains(&bare.to_lowercase().as_str()) {
        return String::new();
    }
    bare.to_owned()
}

/// "CRAFT-IT4SD — Meeting!" → "craft-it4sd-meeting" (secretary-server's rules).
pub fn slugify(text: &str) -> String {
    let folded: String = text
        .nfkd()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .collect::<String>()
        .to_lowercase()
        .replace('æ', "ae")
        .replace('ø', "o")
        .replace('ß', "ss");
    let mut out = String::new();
    let mut dash = false;
    for c in folded.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.push(c);
        } else {
            dash = true;
        }
    }
    let s: String = out.chars().take(64).collect();
    let s = s.trim_end_matches('-').to_owned();
    if ["trash", "new", "search"].contains(&s.as_str()) {
        format!("{s}-note")
    } else {
        s
    }
}

/// The text after the command: after its colon on the same line, else the
/// following lines. `end` is the byte end of the command in `message`.
fn content_after(message: &str, end: usize) -> String {
    let rest = &message[end..];
    let line_end = rest.find('\n');
    let rest_of_line = match line_end {
        Some(i) => &rest[..i],
        None => rest,
    };
    if let Some(colon) = rest_of_line.find(':') {
        if !rest_of_line[colon + 1..].trim().is_empty() {
            return rest[colon + 1..].trim().to_owned();
        }
    }
    match line_end {
        Some(i) => rest[i + 1..].trim().to_owned(),
        None => String::new(),
    }
}

fn first_heading(content: &str) -> String {
    let Some(line) = content.split('\n').find(|l| !l.trim().is_empty()) else {
        return String::new();
    };
    crate::re!(r"^#{1,3}\s+(.+)$")
        .exec(line)
        .map(|c| group(&c, 1).trim().chars().take(200).collect())
        .unwrap_or_default()
}

fn noun_title(lang: &str, noun: &str) -> &'static str {
    let noun = noun.to_lowercase();
    match (lang, noun.as_str()) {
        (_, "briefing") => "Briefing",
        ("it", "contesto") => "Contesto",
        ("it", "documento") => "Documento",
        ("en", "context") => "Context",
        ("en", "document") => "Document",
        ("da", "kontekst") => "Kontekst",
        ("da", "dokument") => "Dokument",
        ("en" | "da", _) => "Note",
        _ => "Nota",
    }
}

fn title_for(name: &str, content: &str, lang: &str, noun: &str) -> String {
    let heading = first_heading(content);
    if !heading.is_empty() {
        return heading;
    }
    if !name.is_empty() && !crate::re!(r"^[a-z0-9-]+$").test(name) {
        return name.chars().take(200).collect();
    }
    let label = noun_title(lang, noun);
    if !name.is_empty() {
        return format!("{label} {name}");
    }
    let first = content
        .split('\n')
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let cleaned: String = crate::re!(r"[#*_>`]")
        .replace_all(first, "")
        .trim()
        .chars()
        .take(80)
        .collect();
    if cleaned.is_empty() {
        label.to_owned()
    } else {
        cleaned
    }
}

fn head(message: &str) -> &str {
    match message.char_indices().nth(600) {
        Some((i, _)) => &message[..i],
        None => message,
    }
}

/// `parseNotesIntent`.
pub fn parse_notes_intent(text: &str, pending_part: bool) -> Option<NotesIntent> {
    let message = text.trim();
    if message.is_empty() {
        return None;
    }
    let head = head(message);
    let p = p();
    for (lang, re) in &p.save_named {
        let Some(m) = re.exec(head) else { continue };
        let name = pick_name(&m);
        if name.is_empty() {
            continue;
        }
        let whole = m.get(0).expect("match");
        let noun = p.noun.find(whole.as_str()).map(|x| x.2).unwrap_or("");
        let content = content_after(message, whole.end());
        let remember = crate::re!(r"^(?:ricorda|remember|husk)", "iu").test(whole.as_str());
        if content.is_empty() && remember {
            return Some(NotesIntent::Read { name, lang });
        }
        return Some(NotesIntent::Save {
            slug: slugify(&name),
            title: title_for(&name, &content, lang, noun),
            more: p.first_part.test(head),
            name,
            content,
            lang,
        });
    }
    for (lang, re) in &p.save_unnamed {
        let Some((start, end, matched)) = re.find(head) else {
            continue;
        };
        let _ = start;
        let end = if matched.ends_with(':') { end - 1 } else { end };
        let content = content_after(message, end);
        let title = title_for("", &content, lang, "");
        return Some(NotesIntent::Save {
            name: String::new(),
            slug: String::new(),
            title,
            content,
            more: p.first_part.test(head),
            lang,
        });
    }
    if pending_part && p.later_part.test(head) {
        let lang = if crate::re!(r"\b(?:parte|second[ao]|terz[ao]|ultima)\b", "i").test(head) {
            "it"
        } else if crate::re!(r"\b(?:part|second|third|last|final)\b", "i").test(head) {
            "en"
        } else {
            "da"
        };
        let stripped = p.later_part_lead.replace(message, "");
        let content = stripped.trim();
        return Some(NotesIntent::Part {
            content: if content.is_empty() {
                message.to_owned()
            } else {
                content.to_owned()
            },
            more: false,
            lang,
        });
    }
    for (lang, re) in &p.read {
        let Some(m) = re.exec(head) else { continue };
        let name = pick_name(&m);
        if !name.is_empty() {
            return Some(NotesIntent::Read { name, lang });
        }
    }
    if lucina_store::timeutil::utf16_len(message) <= 120 {
        for (lang, re) in &p.list {
            if re.test(message) {
                return Some(NotesIntent::List { lang });
            }
        }
    }
    None
}

/// "store/memorize this briefing/context": never a log or harness request.
pub fn looks_like_store_note(text: &str) -> bool {
    let head = head(text);
    let p = p();
    p.save_named.iter().any(|(_, r)| r.test(head))
        || p.save_unnamed.iter().any(|(_, r)| r.test(head))
        || p.store_like.test(head)
}

// ---------------------------------------------------------------- replies

/// `{title, slug, chars}` of a server note.
pub struct NoteView {
    pub title: String,
    pub slug: String,
    pub chars: usize,
}

pub fn view(note: &Value) -> NoteView {
    NoteView {
        title: note["title"].as_str().unwrap_or("").to_owned(),
        slug: note["slug"].as_str().unwrap_or("").to_owned(),
        chars: note["content"]
            .as_str()
            .map(|c| c.chars().count())
            .unwrap_or(0),
    }
}

pub fn reply_saved(lang: &str, n: &NoteView, more: bool) -> String {
    match lang {
        "en" => format!("Done: I saved the note \"{}\" (name: {}, {} characters). It's in the Secretary app under Notes, and I can recall it any time: ask me \"what's in the note {}\".{}", n.title, n.slug, n.chars, n.slug, if more { " Waiting for the next part." } else { "" }),
        "da" => format!("Klaret: jeg har gemt noten \"{}\" (navn: {}, {} tegn). Den ligger i Secretary-appen under Noter, og jeg kan finde den frem når som helst: spørg \"hvad står der i noten {}\".{}", n.title, n.slug, n.chars, n.slug, if more { " Jeg venter på næste del." } else { "" }),
        _ => format!("Fatto: ho salvato la nota \"{}\" (nome: {}, {} caratteri). La trovi nell'app Secretary in Note e la ritrovo quando vuoi: chiedimi \"cosa c'è nella nota {}\".{}", n.title, n.slug, n.chars, n.slug, if more { " Aspetto la parte successiva." } else { "" }),
    }
}

pub fn reply_appended(lang: &str, n: &NoteView, more: bool) -> String {
    match lang {
        "en" => format!(
            "Added to the note \"{}\" ({}): now {} characters.{}",
            n.title,
            n.slug,
            n.chars,
            if more {
                " Waiting for the next part."
            } else {
                " It's complete in the Secretary app under Notes."
            }
        ),
        "da" => format!(
            "Tilføjet til noten \"{}\" ({}): nu {} tegn.{}",
            n.title,
            n.slug,
            n.chars,
            if more {
                " Jeg venter på næste del."
            } else {
                " Den er komplet i Secretary-appen under Noter."
            }
        ),
        _ => format!(
            "Aggiunto alla nota \"{}\" ({}): ora {} caratteri.{}",
            n.title,
            n.slug,
            n.chars,
            if more {
                " Aspetto la parte successiva."
            } else {
                " È completa nell'app Secretary, in Note."
            }
        ),
    }
}

pub fn reply_existed(lang: &str, n: &NoteView) -> String {
    match lang {
        "en" => format!(
            "The note \"{}\" already existed, so I added the text at the end (now {} characters).",
            n.slug, n.chars
        ),
        "da" => format!(
            "Noten \"{}\" fandtes allerede, så jeg tilføjede teksten til sidst (nu {} tegn).",
            n.slug, n.chars
        ),
        _ => format!(
            "La nota \"{}\" esisteva già, quindi ho aggiunto il testo in fondo (ora {} caratteri).",
            n.slug, n.chars
        ),
    }
}

pub fn reply_ask_content(lang: &str, name: &str) -> String {
    match lang {
        "en" => format!(
            "What should I save{}? Send the text in the same message, after a colon.",
            if name.is_empty() {
                String::new()
            } else {
                format!(" in the note \"{name}\"")
            }
        ),
        "da" => format!(
            "Hvad skal jeg gemme{}? Send teksten i samme besked efter et kolon.",
            if name.is_empty() {
                String::new()
            } else {
                format!(" i noten \"{name}\"")
            }
        ),
        _ => format!(
            "Cosa devo salvare{}? Mandami il testo nello stesso messaggio, dopo i due punti.",
            if name.is_empty() {
                String::new()
            } else {
                format!(" nella nota \"{name}\"")
            }
        ),
    }
}

pub fn reply_too_long(lang: &str) -> &'static str {
    match lang {
        "en" => "That text is over 256 KB, the most a note holds: split it into several notes.",
        "da" => "Teksten er over 256 KB, det meste en note kan rumme: del den op i flere noter.",
        _ => "Il testo è più lungo di 256 KB, il massimo di una nota: dividilo in più note.",
    }
}

pub fn reply_failed(lang: &str, code: &str) -> String {
    match lang {
        "en" => format!("I couldn't save the note right now ({code}). Try again in a moment."),
        "da" => format!("Jeg kunne ikke gemme noten lige nu ({code}). Prøv igen om lidt."),
        _ => format!(
            "Non sono riuscita a salvare la nota in questo momento ({code}). Riprova tra poco."
        ),
    }
}

pub fn reply_not_found(lang: &str, name: &str, list: &str) -> String {
    let tail = |label: &str| {
        if list.is_empty() {
            String::new()
        } else {
            format!(" {label}: {list}.")
        }
    };
    match lang {
        "en" => format!("I can't find a note \"{name}\".{}", tail("Your notes")),
        "da" => format!(
            "Jeg kan ikke finde en note \"{name}\".{}",
            tail("Dine noter")
        ),
        _ => format!("Non trovo una nota \"{name}\".{}", tail("Le tue note")),
    }
}

pub fn reply_read_failed(lang: &str, code: &str) -> String {
    match lang {
        "en" => format!("I can't read the notes right now ({code})."),
        "da" => format!("Jeg kan ikke læse noterne lige nu ({code})."),
        _ => format!("Non riesco a leggere le note in questo momento ({code})."),
    }
}

pub fn reply_none(lang: &str) -> &'static str {
    match lang {
        "en" => "You have no notes yet. Tell me \"save this as note <name>: …\".",
        "da" => "Du har ingen noter endnu. Sig \"gem dette som note <navn>: …\".",
        _ => "Non hai ancora note. Puoi dirmi \"salva questo come nota <nome>: …\".",
    }
}

pub fn reply_list(lang: &str, count: usize, lines: &str) -> String {
    match lang {
        "en" => {
            format!("Your notes ({count}):\n{lines}\nThey're all in the Secretary app under Notes.")
        }
        "da" => {
            format!("Dine noter ({count}):\n{lines}\nDe ligger alle i Secretary-appen under Noter.")
        }
        _ => {
            format!("Le tue note ({count}):\n{lines}\nLe trovi tutte nell'app Secretary, in Note.")
        }
    }
}

/// Honcho line for a saved note (`summaryForMemory`).
pub fn summary_for_memory(note: &Value, source: &str) -> String {
    let content = note["content"].as_str().unwrap_or("");
    let flat = crate::re!(r"\s+").replace_all(content, " ");
    let flat = flat.trim();
    let head = if lucina_store::timeutil::utf16_len(flat) > 800 {
        format!("{}…", flat.chars().take(800).collect::<String>())
    } else {
        flat.to_owned()
    };
    let tags: Vec<&str> = note["tags"]
        .as_array()
        .map(|t| t.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let tags = if tags.is_empty() {
        String::new()
    } else {
        format!(" Tags: {}.", tags.join(", "))
    };
    format!(
        "Saved note \"{}\" (slug {}) from {source}, {} characters, visible in the Secretary app under Notes.{tags} Start: {head}",
        note["title"].as_str().unwrap_or(""),
        note["slug"].as_str().unwrap_or(""),
        content.chars().count()
    )
}

fn fold(text: &str) -> String {
    text.nfkd()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .collect::<String>()
        .to_lowercase()
}

/// Notes the prompt names: by slug (a whole token) or by title (8+ chars).
pub fn mentioned_notes<'a>(prompt: &str, notes: &'a [Value]) -> Vec<&'a Value> {
    let text = fold(prompt);
    if text.trim().is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for n in notes {
        let slug = n["slug"].as_str().unwrap_or("");
        let title = fold(n["title"].as_str().unwrap_or(""));
        let title = title.trim();
        let by_slug = !slug.is_empty()
            && Re::try_new(&words_src(&slug.replace('-', r"[-\s]?")), "iu")
                .map(|r| r.test(&text))
                .unwrap_or(false);
        let by_title = title.chars().count() >= 8 && text.contains(title);
        if by_slug || by_title {
            hits.push(n);
        }
        if hits.len() >= MAX_CONTEXT_NOTES {
            break;
        }
    }
    hits
}

/// `renderNoteContext`.
pub fn render_note_context(note: &Value, max_chars: usize) -> String {
    let content = note["content"].as_str().unwrap_or("");
    let total = content.chars().count();
    let body = if total > max_chars {
        format!(
            "{}\n[…truncated: {total} characters in total; the full note is in the Secretary app]",
            content.chars().take(max_chars).collect::<String>()
        )
    } else {
        content.to_owned()
    };
    let tags: Vec<&str> = note["tags"]
        .as_array()
        .map(|t| t.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let tags = if tags.is_empty() {
        String::new()
    } else {
        format!("; tags: {}", tags.join(", "))
    };
    let updated: String = note["updatedAt"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(10)
        .collect();
    format!(
        "### Note \"{}\" (slug: {}; saved by {}; updated {updated}{tags})\n{body}",
        note["title"].as_str().unwrap_or(""),
        note["slug"].as_str().unwrap_or(""),
        note["source"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("owner"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_reads_lists() {
        let i =
            parse_notes_intent("salva questo come nota spesa-casa: latte, pane", false).unwrap();
        assert_eq!(
            i,
            NotesIntent::Save {
                name: "spesa-casa".into(),
                slug: "spesa-casa".into(),
                title: "Nota spesa-casa".into(),
                content: "latte, pane".into(),
                more: false,
                lang: "it"
            }
        );
        let i = parse_notes_intent(
            "Memorizza il briefing nel contesto 'craftit-meeting'\n# CRAFT meeting\npunto 1",
            false,
        )
        .unwrap();
        match i {
            NotesIntent::Save {
                slug,
                title,
                content,
                ..
            } => {
                assert_eq!(slug, "craftit-meeting");
                assert_eq!(title, "CRAFT meeting");
                assert!(content.starts_with("# CRAFT meeting"));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse_notes_intent("cosa c'è nella nota craftit-meeting?", false),
            Some(NotesIntent::Read {
                name: "craftit-meeting".into(),
                lang: "it"
            })
        );
        assert_eq!(
            parse_notes_intent("what's in the note trip-plan", false),
            Some(NotesIntent::Read {
                name: "trip-plan".into(),
                lang: "en"
            })
        );
        assert_eq!(
            parse_notes_intent("mostrami le note", false),
            Some(NotesIntent::List { lang: "it" })
        );
        assert_eq!(
            parse_notes_intent("vis mine noter", false),
            Some(NotesIntent::List { lang: "da" })
        );
        assert!(parse_notes_intent("ciao come stai", false).is_none());
        match parse_notes_intent("save this note: buy a new kettle", false).unwrap() {
            NotesIntent::Save { content, slug, .. } => {
                assert_eq!(content, "buy a new kettle");
                assert_eq!(slug, "");
            }
            other => panic!("{other:?}"),
        }
        match parse_notes_intent("Seconda parte del briefing: altro testo", true).unwrap() {
            NotesIntent::Part { content, .. } => assert_eq!(content, "altro testo"),
            other => panic!("{other:?}"),
        }
        assert!(looks_like_store_note(
            "memorizza il briefing nel contesto 'x'"
        ));
        assert_eq!(slugify("CRAFT-IT4SD — Meeting!"), "craft-it4sd-meeting");
        assert_eq!(slugify("Æble Grød"), "aeble-grod");
        assert_eq!(slugify("new"), "new-note");
    }
}
