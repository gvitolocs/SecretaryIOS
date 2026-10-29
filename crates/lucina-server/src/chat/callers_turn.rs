//! "Who called me from <number>?" and pasted lists of numbers, on the JSON
//! planner path, MCP turns included (read-only): every number gets Jev's
//! P(spam), who is likely calling and P(pick up) in one table, in the
//! owner's language ([`crate::callers`], `lucina_phone_core::caller`).
//!
//! - **Routing:** a phone number with a caller word ("who called", "spam",
//!   "chi ha chiamato", "hvem ringede", "Jev", …) or a message that is only
//!   numbers routes directly. A number with other words asks Jev one yes/no
//!   question (from `LUCINA_JEV_FIRST_MIN`). "Call / text / save <number>"
//!   is never this turn.
//! - **Answer:** deterministic (the numbers are Jev's, never the text
//!   model's), with a "Look up online" button that hands the numbers to her
//!   browser (`lucina_intent::computer_route`, read-only, quoted sources).

use std::sync::OnceLock;

use lucina_jev::Question;
use lucina_phone_core::caller::{self, LineType};
use lucina_tools::tools::callers::{ScreenRequest, ScreenResponse};
use regex::Regex;
use serde_json::{json, Value};

use super::{Finish, Turn};

pub const ACTION: &str = "caller_screen";
/// Numbers handed to the browser button.
const MAX_WEB: usize = 5;

fn rx(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("callers turn regex"))
}

/// Words that ask who a caller is or whether a call is spam (en / it / da).
pub fn lookup_cue(text: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    rx(
        &R,
        r"(?i)\b(?:who\s+(?:called|calls|rang|is\s+calling|is\s+this|is\s+it|owns)|called\s+me|calling\s+me|rang\s+me|missed\s+calls?|unknown\s+(?:numbers?|callers?)|callers?|inbound|spam|scam\w*|telemarket\w*|robocall\w*|pick\s+(?:it\s+)?up|jev|chi\s+(?:mi\s+)?ha\s+chiamato|chi\s+(?:è|e')|mi\s+ha\s+chiamato|chiamat[aeio]|numer[oi]\s+sconosciut[oi]|truff\w*|rispondere|hvem\s+(?:der\s+)?(?:ringede|har\s+ringet|ringer|er\s+det)|opkald\w*|ringede|ukendt\s+nummer|svindel|tage\s+den)\b",
    )
    .is_match(text)
}

/// "Call / text / save <number>": an action on a number, not a lookup.
fn action_on_number(text: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    rx(
        &R,
        r"(?i)^\s*(?:please\s+|per\s+favore\s+|ok\s+)?(?:call|ring|dial|phone|text|sms|message|whatsapp|send|save|add|block|report|chiama|telefona|scrivi|manda|salva|aggiungi|blocca|ring\s+til|send\s+sms|skriv|gem|tilføj|bloker)\b",
    )
    .is_match(text)
}

/// A message that is only phone numbers (a pasted list), give or take a few words.
pub fn only_numbers(text: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    let rest = rx(&R, r"(?:\+|\b00)?\(?\d[\d \u{a0}().-]{6,20}\d\b").replace_all(text, " ");
    let letters = rest.chars().filter(|c| c.is_alphabetic()).count();
    letters <= 12 && !caller::find_numbers(text).is_empty()
}

/// Jev's yes/no for a number with other words; `false` when Jev is off or failed.
async fn jev_says_lookup(text: &str) -> bool {
    let jev = lucina_jev::shared();
    if !jev.available() {
        return false;
    }
    let q = [(
        "caller",
        Question::noul("Is the owner asking who is behind a phone number (a caller), or whether a call from it is spam, a scam or worth picking up?"),
    )];
    let Ok(d) = jev
        .decide("caller_route", json!({"message": text}), &q)
        .await
    else {
        return false;
    };
    let p = d.yes("caller").unwrap_or(0.0);
    let min = crate::jev_route::jev_first_min(ACTION);
    tracing::info!(target: "lucina_jev::route", p, min, ms = d.elapsed_ms, "jev caller route");
    p >= min
}

/// Should this message be a caller screen? (Jev only for the unclear ones.)
pub async fn routes(text: &str) -> bool {
    if caller::find_numbers(text).is_empty()
        || action_on_number(text)
        || lucina_intent::computer_route::wants_computer(text)
    {
        return false;
    }
    lookup_cue(text) || only_numbers(text) || jev_says_lookup(text).await
}

/// The text of the "Look up online" button: a read-only browser task for her computer.
pub fn browser_request(numbers: &[String], lang: &str) -> String {
    let list = numbers
        .iter()
        .take(MAX_WEB)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    match lang {
        "it" => format!("Usa il tuo browser per cercare sul web chi ha chiamato da {list} (cita il testo di ogni pagina con il suo URL; solo lettura)."),
        "da" => format!("Brug din browser og find på nettet hvem der har ringet fra {list} (citér teksten fra hver side med dens URL; kun læsning)."),
        _ => format!("Use your browser to search the web for who called from {list} (quote each page's text with its URL; read only)."),
    }
}

fn web_button(numbers: &[String], lang: &str) -> Value {
    let label = match lang {
        "it" => "Cerca online",
        "da" => "Slå op online",
        _ => "Look up online",
    };
    json!({"id": "callers-web", "label": label, "emoji": "🔎", "kind": "reply",
        "payload": {"text": browser_request(numbers, lang)}})
}

/// The reply: one line per number when there are few, then the table.
pub fn render(r: &ScreenResponse) -> String {
    let lang = r.lang;
    let valid: Vec<_> = r
        .results
        .iter()
        .filter(|v| v.line != LineType::Invalid)
        .collect();
    let intro = match (lang, valid.len()) {
        ("it", 1) => "Ecco cosa ne pensa Jev:",
        ("it", _) => "Ecco cosa ne pensa Jev di questi numeri:",
        ("da", 1) => "Her er Jevs vurdering:",
        ("da", _) => "Her er Jevs vurdering af numrene:",
        (_, 1) => "Here's what Jev makes of it:",
        _ => "Here's what Jev makes of these numbers:",
    };
    let mut out = String::from(intro);
    out.push_str("\n\n");
    if valid.len() <= 3 {
        for v in &valid {
            out.push_str(&format!("**{}** — {}", v.display, v.label));
            if !v.reasons.is_empty() {
                out.push_str(&format!(" · {}", v.reasons.join("; ")));
            }
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(&r.table);
    out
}

pub(super) async fn try_turn(t: &Turn<'_>, _memory: &str) -> Option<Finish> {
    let st = t.st;
    if !st.cfg.callers_enabled {
        return None;
    }
    let text = t.message.as_str();
    if !routes(text).await {
        return None;
    }
    let numbers = caller::find_numbers(text);
    let lang = caller::lang(crate::rich::guess_language(text, ""));
    let req = ScreenRequest {
        numbers: numbers.iter().map(|n| n.e164.clone()).collect(),
        lang: Some(lang.to_owned()),
        ..ScreenRequest::default()
    };
    let r = match crate::callers::screen(st, &req).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "caller screen turn failed");
            return None;
        }
    };
    let sources: Vec<&str> = r.results.iter().map(|v| v.source.as_str()).collect();
    let mut f = Finish::reply(render(&r), ACTION).with(
        format!(
            "[caller screen: {} numbers, {}]",
            r.results.len(),
            sources.join(",")
        ),
        None,
    );
    let web: Vec<String> = r
        .results
        .iter()
        .filter(|v| v.line != LineType::Invalid && v.source != "contact")
        .map(|v| v.display.clone())
        .collect();
    if !web.is_empty() {
        f.routes.push(web_button(&web, lang));
    }
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_phone_core::caller::{parse, rules, verdict, Context};

    #[test]
    fn cues() {
        for t in [
            "who called me from +45 70 40 52 05?",
            "Chi mi ha chiamato dal 20682758?",
            "hvem ringede fra 70 40 52 20",
            "is +357 94 095733 a scam?",
            "Please use JEV directly to check whether it flags these recent inbound phone numbers as spam: +45 70 40 52 05, +357 94 095733",
            "should I pick up +44 7459 508537",
        ] {
            assert!(lookup_cue(t), "{t}");
        }
        for t in [
            "call +45 70 40 52 05",
            "ring til 20682758",
            "Chiama il 20682758",
            "save 20682758 as Jerry",
            "text +4520682758 I'm late",
        ] {
            assert!(action_on_number(t), "{t}");
        }
        assert!(!action_on_number("who called me from +45 70 40 52 05?"));
        assert!(only_numbers(
            "+45 70 40 52 05\n+357 94 095733\n+45 70 40 52 20"
        ));
        assert!(only_numbers("20682758?"));
        assert!(!only_numbers("my new number is 20682758, write it down"));
        assert!(!only_numbers("no numbers here"));
    }

    #[tokio::test]
    async fn routing_without_jev() {
        // Jev is not configured in tests: only the cues route.
        assert!(routes("who called me from +45 70 40 52 05?").await);
        assert!(routes("+45 70 40 52 05 +357 94 095733").await);
        assert!(!routes("call +45 70 40 52 05").await);
        assert!(!routes("who called me yesterday?").await);
        assert!(!routes("my new number is 20682758, write it down").await);
        // Her browser button goes to the computer, not here.
        assert!(!routes(&browser_request(&["+45 70 40 52 05".into()], "en")).await);
    }

    #[test]
    fn the_web_button_goes_to_her_computer() {
        for lang in ["en", "it", "da"] {
            let text = browser_request(&["+45 70 40 52 05".into(), "+357 94 095733".into()], lang);
            assert!(
                lucina_intent::computer_route::wants_computer(&text),
                "{text}"
            );
        }
        assert_eq!(
            web_button(&["+45 70 40 52 05".into()], "da")["label"],
            "Slå op online"
        );
    }

    #[test]
    fn rendering() {
        let ctx = Context::default();
        let one = |s: &str| {
            let n = parse(s);
            verdict(&n, &ctx, &rules(&n, &ctx), "rules", "en")
        };
        let r = ScreenResponse {
            lang: "en",
            results: vec![one("+45 70 40 52 05")],
            table: caller::table(&[one("+45 70 40 52 05")], "en"),
        };
        let text = render(&r);
        assert!(text.starts_with("Here's what Jev makes of it:\n\n**+45 70 40 52 05** — Unknown caller · Danish 70-number (company line)\n"));
        assert!(text.contains("| Number | Spam | Kind | Pick up |"));
        let many: Vec<_> = ["+4570405205", "+35794095733", "+4570405220", "+35794462808"]
            .iter()
            .map(|s| one(s))
            .collect();
        let r = ScreenResponse {
            lang: "en",
            table: caller::table(&many, "en"),
            results: many,
        };
        let text = render(&r);
        assert!(text.starts_with("Here's what Jev makes of these numbers:\n\n| Number"));
    }
}
