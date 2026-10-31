//! Messages the owner asks Lucina to pass on to another assistant: "tell Claude that…",
//! "can you say that to Claude in the right chat?", "chiedi a Grok…", "spørg Grok…", "post it
//! in Lucina agents", "ok then use Lucina agents". A deterministic route (intent
//! `agent_message`), so no other shortcut (a link in the message, a plugin word) can take it.
//!
//! Links are removed before matching: a URL that contains "claude" or "grok" is not a request.
//! Talking *about* an assistant ("tell me about Claude", "is Grok better than…") is not one
//! either: the verb must be aimed at it.
//!
//! These patterns use the regex engine's own `\b` (not [`crate::jsre`]'s lookaround form): long
//! messages would otherwise run into the backtracking limit and never match.

use std::sync::OnceLock;

use fancy_regex::Regex;

/// Who the message is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Claude,
    Grok,
    Agents,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::Claude => "claude",
            Target::Grok => "grok",
            Target::Agents => "agents",
        }
    }
}

/// The text without its links.
fn without_links(text: &str) -> String {
    text.split_whitespace()
        .filter(|w| !w.contains("://") && !w.starts_with("www."))
        .collect::<Vec<_>>()
        .join(" ")
}

// Verbs that pass something on, it/en/da (accented forms are followed by a space, not `\b`).
const EN_VERB: &str = r"(?:tell|say|ask|message|msg|ping|text|notify|inform|email|write|send|post|report|forward|pass|relay|mention|explain|let)";
const IT_VERB: &str = r"(?:d[iì]'?|dire|dici|digli|dille|dillo|chiedi|chiedigli|chiedere|domanda|scrivi|scrivigli|scrivere|manda|mandare|mandagli|invia|inviare|segnala|segnalalo|segnalare|riferisci|riferire|comunica|inoltra|avvisa|fai\s+sapere|posta|pubblica)";
const DA_VERB: &str = r"(?:sig|fort[æa]l|sp[øo]rg|skriv|send|meld|giv\s+besked)";

fn test(re: &Regex, t: &str) -> bool {
    re.is_match(t).unwrap_or(false)
}

fn compile(src: &str) -> Regex {
    Regex::new(&format!("(?i){src}")).expect("reach regex")
}

fn mentions(t: &str, target: &str) -> bool {
    static CLAUDE: OnceLock<Vec<Regex>> = OnceLock::new();
    static GROK: OnceLock<Vec<Regex>> = OnceLock::new();
    static AGENTS: OnceLock<Vec<Regex>> = OnceLock::new();
    let (cell, who) = match target {
        "claude" => (&CLAUDE, r"claude(?:\s+code)?"),
        "grok" => (&GROK, r"grok(?:\s*bot)?"),
        _ => (&AGENTS, r"lucina[\s-]*agents?"),
    };
    let checks = cell.get_or_init(|| directed(who).iter().map(|p| compile(p)).collect());
    let named = compile_once(target);
    test(named, t) && (checks.iter().any(|r| test(r, t)) || (target == "agents" && agents_imperative(t)))
}

fn compile_once(target: &str) -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    static G: OnceLock<Regex> = OnceLock::new();
    static A: OnceLock<Regex> = OnceLock::new();
    match target {
        "claude" => C.get_or_init(|| compile(r"\bclaude(?:\s+code)?\b")),
        "grok" => G.get_or_init(|| compile(r"\bgrok(?:\s*bot)?\b")),
        _ => A.get_or_init(|| compile(r"\blucina[\s-]*agents?\b")),
    }
}

/// The patterns of a passing-on verb aimed at the assistant named by `who`.
fn directed(who: &str) -> Vec<String> {
    vec![
        // "tell Claude …", "ask Grok …", "message claude code …"
        format!(r"\b(?:{EN_VERB})\s+(?:it\s+|this\s+|that\s+)?(?:to\s+)?{who}\b"),
        // "say that … to Claude", "post this in the Claude chat", "report it to grok"
        format!(r"\b(?:{EN_VERB})\b[^.?!\n]{{0,160}}\b(?:to|with|in|on|for)\s+(?:the\s+)?(?:appropriate\s+|right\s+|proper\s+)?(?:chat\s+|project\s+|session\s+)*(?:with\s+)?{who}\b"),
        // "chiedi a Claude", "scrivi a grok", "dì a Claude che …"
        format!(r"(?:^|[\s,.;:!?(])(?:{IT_VERB})\s+(?:a|ad|al|con)\s+{who}\b"),
        // "dillo … a Claude", "segnala … nella chat di Claude"
        format!(r"(?:^|[\s,.;:!?(])(?:{IT_VERB})(?:\s|$)[^.?!\n]{{0,160}}(?:\s(?:a|ad|al|su|in|nel|nella|con|di)\s+){who}\b"),
        // "spørg Grok", "sig til Claude", "fortæl Claude"
        format!(r"(?:^|[\s,.;:!?(])(?:sp[øo]rg|sig\s+til|fort[æa]l)\s+{who}\b"),
        // "skriv … til Claude", "giv besked til Grok"
        format!(r"(?:^|[\s,.;:!?(])(?:{DA_VERB})(?:\s|$)[^.?!\n]{{0,160}}\s(?:til|på|i)\s+{who}\b"),
    ]
}

/// "use Lucina agents", "usa Lucina Agents", "brug Lucina agents", "post it in Lucina agents".
fn agents_imperative(t: &str) -> bool {
    static USE: OnceLock<Regex> = OnceLock::new();
    static POST: OnceLock<Regex> = OnceLock::new();
    test(USE.get_or_init(|| compile(
        r"(?:^|[\s,.;:!?(])(?:use|usa|usare|utilizza|brug|try|prova|via|through|tramite|con)\s+(?:the\s+|i\s+|gli\s+)?lucina[\s-]*agents?\b",
    )), t)
        || test(POST.get_or_init(|| compile(
            r"(?:^|[\s,.;:!?(])(?:post|write|send|put|run|start|launch|ask|scrivi|posta|manda|metti|lancia|avvia|chiedi|skriv|send|kør|start)(?:\s|$)[^.?!\n]{0,120}(?:\s(?:in|on|to|into|su|in|nel|nei|negli|a|ai|agli|til|på|i)\s+)(?:the\s+|i\s+|gli\s+)?lucina[\s-]*agents?\b",
        )), t)
}

/// The assistant the owner asks Lucina to message, or `None`.
pub fn target(text: &str) -> Option<Target> {
    let t = without_links(text);
    if t.is_empty() {
        return None;
    }
    // Lucina Agents first: "ask Claude in Lucina agents" is a run there.
    if mentions(&t, "agents") {
        return Some(Target::Agents);
    }
    if mentions(&t, "claude") {
        return Some(Target::Claude);
    }
    if mentions(&t, "grok") {
        return Some(Target::Grok);
    }
    None
}

/// Is this a request to message Claude, Grok or Lucina Agents?
pub fn is_request(text: &str) -> bool {
    target(text).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_to_pass_on() {
        for (text, want) in [
            ("I could open this link without being authenticated can you say that in the appropriate chat project to Claude? https://getsecretary.tech/v1/instagram/s/1793473200/89d1/x", Target::Claude),
            ("tell Claude that the gallery swipe is broken again", Target::Claude),
            ("Ask Claude code to look at the calendar tab", Target::Claude),
            ("report this to claude", Target::Claude),
            ("dì a Claude che il link Instagram si apre senza login", Target::Claude),
            ("chiedi a Claude se ha finito il deploy", Target::Claude),
            ("segnalalo a Claude per favore", Target::Claude),
            ("sig til Claude at kalenderen virker", Target::Claude),
            ("ask Grok what's new on X about Rust", Target::Grok),
            ("chiedi a Grok le ultime notizie", Target::Grok),
            ("send this to grok bot", Target::Grok),
            ("spørg Grok om vejret i Aarhus", Target::Grok),
            ("Ok then use Lucina agents", Target::Agents),
            ("usa Lucina Agents", Target::Agents),
            ("post it in Lucina agents", Target::Agents),
            ("scrivilo, anzi posta questo su lucina agents", Target::Agents),
            ("ask Claude in Lucina agents to check the logs", Target::Agents),
        ] {
            assert_eq!(target(text), Some(want), "{text}");
        }
    }

    #[test]
    fn not_requests() {
        for text in [
            "tell me about Claude",
            "is Grok better than ChatGPT?",
            "what happened to lucinapaseo?",
            "show me the latest instagram stories of natgeo",
            "https://claude.ai/code/session_123 look at this",
            "Claude is great",
            "what model are you, claude or deepseek?",
            "che ne pensi di Grok?",
            "I will enable Lucina agents soon don't worry",
            "",
        ] {
            assert_eq!(target(text), None, "{text}");
        }
    }
}
