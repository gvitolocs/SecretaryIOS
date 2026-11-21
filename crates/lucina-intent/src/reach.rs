//! Messages the owner asks Lucina to pass on to another assistant: "tell Claude that…",
//! "can you say that to Claude in the right chat?", "chiedi a Grok…", "spørg Grok…", "post it
//! in Lucina agents", "ok then use Lucina agents", "now start one new Lucina Agents run". A
//! deterministic route (intent `agent_message`), so no other shortcut (a link in the message, a
//! plugin word) can take it.
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
    Mistral,
    /// A report for "the agent", with nobody named: the report agent gets it.
    Report,
}

impl Target {
    pub fn as_str(self) -> &'static str {
        match self {
            Target::Claude => "claude",
            Target::Grok => "grok",
            Target::Agents => "agents",
            Target::Mistral => "mistral",
            Target::Report => "report",
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
    static MISTRAL: OnceLock<Vec<Regex>> = OnceLock::new();
    static GROK: OnceLock<Vec<Regex>> = OnceLock::new();
    static AGENTS: OnceLock<Vec<Regex>> = OnceLock::new();
    let (cell, who) = match target {
        "claude" => (&CLAUDE, r"claude(?:\s+code)?"),
        "grok" => (&GROK, r"grok(?:\s*bot)?"),
        "mistral" => (&MISTRAL, r"mistral"),
        _ => (&AGENTS, r"lucina[\s-]*agents?"),
    };
    let checks = cell.get_or_init(|| directed(who).iter().map(|p| compile(p)).collect());
    let named = compile_once(target);
    test(named, t)
        && (checks.iter().any(|r| test(r, t)) || (target == "agents" && agents_imperative(t)))
}

fn compile_once(target: &str) -> &'static Regex {
    static C: OnceLock<Regex> = OnceLock::new();
    static G: OnceLock<Regex> = OnceLock::new();
    static A: OnceLock<Regex> = OnceLock::new();
    static M: OnceLock<Regex> = OnceLock::new();
    match target {
        "mistral" => M.get_or_init(|| compile(r"\bmistral\b")),
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
        format!(
            r"\b(?:{EN_VERB})\b[^.?!\n]{{0,160}}\b(?:to|with|in|on|for)\s+(?:the\s+)?(?:appropriate\s+|right\s+|proper\s+)?(?:chat\s+|project\s+|session\s+)*(?:with\s+)?{who}\b"
        ),
        // "chiedi a Claude", "scrivi a grok", "dì a Claude che …"
        format!(r"(?:^|[\s,.;:!?(])(?:{IT_VERB})\s+(?:a|ad|al|con)\s+{who}\b"),
        // "dillo … a Claude", "segnala … nella chat di Claude"
        format!(
            r"(?:^|[\s,.;:!?(])(?:{IT_VERB})(?:\s|$)[^.?!\n]{{0,160}}(?:\s(?:a|ad|al|su|in|nel|nella|con|di)\s+){who}\b"
        ),
        // "spørg Grok", "sig til Claude", "fortæl Claude"
        format!(r"(?:^|[\s,.;:!?(])(?:sp[øo]rg|sig\s+til|fort[æa]l)\s+{who}\b"),
        // "skriv … til Claude", "giv besked til Grok"
        format!(r"(?:^|[\s,.;:!?(])(?:{DA_VERB})(?:\s|$)[^.?!\n]{{0,160}}\s(?:til|på|i)\s+{who}\b"),
    ]
}

/// "use Lucina agents", "usa Lucina Agents", "brug Lucina agents", "post it in Lucina agents",
/// "start one new Lucina Agents run".
fn agents_imperative(t: &str) -> bool {
    static USE: OnceLock<Regex> = OnceLock::new();
    static POST: OnceLock<Regex> = OnceLock::new();
    static RUN: OnceLock<Regex> = OnceLock::new();
    test(USE.get_or_init(|| compile(
        r"(?:^|[\s,.;:!?(])(?:use|usa|usare|utilizza|brug|try|prova|via|through|tramite|con)\s+(?:the\s+|i\s+|gli\s+)?lucina[\s-]*agents?\b",
    )), t)
        || test(POST.get_or_init(|| compile(
            r"(?:^|[\s,.;:!?(])(?:post|write|send|put|run|start|launch|ask|scrivi|posta|manda|metti|lancia|avvia|chiedi|skriv|send|kør|start)(?:\s|$)[^.?!\n]{0,120}(?:\s(?:in|on|to|into|su|in|nel|nei|negli|a|ai|agli|til|på|i)\s+)(?:the\s+|i\s+|gli\s+)?lucina[\s-]*agents?\b",
        )), t)
        || test(RUN.get_or_init(|| compile(&run_target())), t)
}

/// The words of a run order: what opens it, what may stand in front of that word, what is opened.
const RUN_VERB: &str = r"(?:start|begin|launch|create|trigger|dispatch|kick\s+off|spin\s+up|run|avvia|lancia|crea|inizia|kør|igangsæt)";
const RUN_LEAD: &str = r"(?:now|ok|okay|then|please|pls|just|so|and|also|first|go\s+ahead\s+and|quindi|allora|ora|pure|nu|bare)\s+";
const RUN_THING: &str =
    r"(?:runs?|tasks?|jobs?|sessions?|batches?|workflows?|agents?|deploy(?:ment)?s?)";

/// "start one new Lucina Agents run", "launch a Lucina Agents task", "crea una Lucina Agents run":
/// an order whose run is named as a Lucina Agents one. The verb opens its clause (a negation or a
/// question word in front of it stops it), at most three plain words stand between the verb and the
/// name, and the name is followed by the thing being opened — so a plain mention, a quote, a link
/// and the read of [`agents_query`] stay where they are.
fn run_target() -> String {
    format!(
        r"(?:^|[.!?,;:])\s*(?:{RUN_LEAD}){{0,3}}{RUN_VERB}\s+(?:[A-Za-z0-9'_-]+\s+){{0,3}}lucina[\s-]*agents?\s+{RUN_THING}\b"
    )
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
    if mentions(&t, "mistral") {
        return Some(Target::Mistral);
    }
    if report_for_the_agent(&t) {
        return Some(Target::Report);
    }
    None
}

/// "post the report to the agent", "manda la segnalazione all'agente", "send rapporten til agenten".
fn report_for_the_agent(t: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    test(R.get_or_init(|| compile(
        r"(?:^|[\s,.;:!?(])(?:post|send|give|pass|forward|manda|invia|passa|posta|inoltra|giv|skicka|videresend)(?:\s|$)[^.?!\n]{0,80}\b(?:report|bug report|segnalazione|rapporto|rapporten|fejlrapport(?:en)?)\b[^.?!\n]{0,80}\b(?:agent|agente|agenten)\b",
    )), t)
}

/// Is this a request to message Claude, Grok or Lucina Agents?
pub fn is_request(text: &str) -> bool {
    target(text).is_some()
}

/// He asks what his Lucina Agents are doing, or tells Lucina to look: "what is running in Lucina
/// agents right now?", "check Lucina agents", "any agent still busy in Lucina agents?", "chi è
/// occupato su Lucina agents?", "hvad kører i Lucina agents?".
///
/// This is a *read*, not a passing-on: [`target`] stays what it is, and a dispatch wins (this returns
/// `false` when the words already ask for one). Narrow on purpose: his name for it must be there, the
/// asking word in the same clause, so a message that only happens to contain the words is not one.
pub fn agents_query(text: &str) -> bool {
    let t = without_links(text);
    if t.is_empty() || !test(compile_once("agents"), &t) || mentions(&t, "agents") {
        return false;
    }
    // Words that ask another recipient's work are not a question to the Agents, even when they
    // mention them on the way. (A passing-on aimed at the Agents returned false just above.)
    if target(&t).is_some() {
        return false;
    }
    static BEFORE: &str = r"(?:what|whats|which|who|how\s+many|any|list|show|check|see|read|look|tell\s+me|chi|cosa|quali|quante|quanti|elenco|mostra\w*|controlla|vedi|guarda|dimmi|hvad|hvem|hvilke|vis|tjek)";
    static AFTER: &str = r"(?:running|busy|active|alive|stuck|waiting|queued|done|finished|progress|status|going|still|in\s+esecuzione|occupat|k[øo]rer)";
    static Q: OnceLock<[Regex; 2]> = OnceLock::new();
    let [before, after] = Q.get_or_init(|| {
        [
            compile(&format!(
                r"\b{BEFORE}\b[^.?!\n]{{0,60}}\blucina[\s-]*agents?\b"
            )),
            compile(&format!(
                r"\blucina[\s-]*agents?\b[^.?!\n]{{0,60}}\b(?:{AFTER}|{BEFORE})\b"
            )),
        ]
    });
    test(before, &t) || test(after, &t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mistral_and_reports_for_the_agent() {
        for (text, want) in [
            ("ask Mistral what this error means", Target::Mistral),
            ("chiedi a Mistral cosa significa", Target::Mistral),
            ("spørg Mistral om fejlen", Target::Mistral),
            ("Post the report to the agent in room …", Target::Report),
            ("post the report to the agent", Target::Report),
            ("manda la segnalazione all'agente", Target::Report),
            ("send rapporten til agenten", Target::Report),
            // A name wins over the default.
            ("post the report to the agent: tell Claude", Target::Claude),
        ] {
            assert_eq!(target(text), Some(want), "{text}");
        }
        for text in [
            "is Mistral better than Claude?",
            "tell me about Mistral",
            "the report is about an agent",
            "read me the report",
        ] {
            assert_eq!(target(text), None, "{text}");
        }
    }

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

    /// An order naming a Lucina Agents run as its target. Only the explicit imperative: the same
    /// words in a question, a quote, a link, a plan or aimed at another assistant stay as they were.
    #[test]
    fn run_target_imperatives() {
        for text in [
            "Now start one new Lucina Agents run using the configured lucinacode provider and the failing tests",
            "start a new Lucina Agents run",
            "Start one new Lucina Agents run",
            "launch a Lucina Agents task",
            "create a Lucina Agents run",
            "create a Lucina Agents run for the relay build",
            "Run a Lucina Agents job against the flaky checkout test",
            "kick off a Lucina Agents session on the login build",
            "Please go ahead and start a new Lucina Agents run",
            "Fix the relay. Then start a new Lucina Agents run with the rust branch",
            "ok, launch a Lucina Agents task",
            "avvia una nuova Lucina Agents run",
            "lancia un Lucina Agents task",
            "kør en ny Lucina Agents run",
            "start a Lucina agent run",
        ] {
            assert_eq!(target(text), Some(Target::Agents), "{text}");
            assert!(!agents_query(text), "{text} is a dispatch, not a read");
        }
    }

    /// What must not become a run: a plain mention, a question about the runs, a quote, a link, a
    /// negation, and a request whose recipient is another assistant.
    #[test]
    fn run_target_is_narrow() {
        for text in [
            "Lucina agents run the nightly builds by themselves",
            "the Lucina agents run finished on its own",
            "I will enable Lucina agents soon don't worry",
            "don't start a new Lucina Agents run yet",
            "don't create a Lucina Agents run",
            "should I start a new Lucina Agents run?",
            "what does starting a Lucina Agents run cost?",
            "he said \"start a new Lucina Agents run\" yesterday",
            "see https://lucina.tech/docs/start-a-new-lucina-agents-run for the flags",
            "read the notes about the last Lucina Agents run",
        ] {
            assert_eq!(target(text), None, "{text}");
        }
        // The read of his Agents stays a read.
        for text in [
            "list the Lucina agents runs that finished",
            "how many Lucina agents runs are queued?",
            "check Lucina agents",
        ] {
            assert!(!test(&compile(&run_target()), text), "{text}");
            assert!(agents_query(text), "{text}");
            assert_eq!(target(text), None, "{text}");
        }
        // Another assistant stays the recipient.
        for (text, want) in [
            (
                "ask Claude to start a new Lucina Agents run",
                Target::Claude,
            ),
            (
                "tell Grok that a Lucina Agents run should start later",
                Target::Grok,
            ),
            (
                "write to Grok about the new Lucina Agents task",
                Target::Grok,
            ),
        ] {
            assert_eq!(target(text), Some(want), "{text}");
        }
    }

    /// A question about his Agents, as he actually types it. Only a read: the passing-on cases stay
    /// with `target`, and nothing else becomes one.
    #[test]
    fn questions_about_his_agents() {
        for text in [
            "what is running in Lucina agents right now?",
            "What's busy on Lucina agents?",
            "check Lucina agents",
            "list the agents on Lucina agents",
            "any Lucina agents still waiting?",
            "how many Lucina agents are active?",
            "Lucina agents status?",
            "chi è occupato su Lucina agents?",
            "cosa c'è in esecuzione su lucina agents",
            "mostrami gli agenti su Lucina agents",
            "hvad kører i Lucina agents lige nu?",
            "hvem er beskæftiget på Lucina agents?",
        ] {
            assert!(agents_query(text), "{text}");
            assert_eq!(target(text), None, "{text} is a dispatch");
        }
        for text in [
            // A passing-on is `target`'s, never a query.
            "post it in Lucina agents: fix the login build",
            "ask Lucina agents what they would change here",
            "ask Claude in Lucina agents to check the logs",
            // A request naming another recipient, even with his Agents words in it.
            "tell Claude that the Lucina agents status is fine",
            "ask Grok what is running, he has the Lucina agents docs",
            // Nothing to do with his Agents.
            "what is running on nezopt right now?",
            "what is the weather",
            "read me the notes",
            "I will enable Lucina agents soon don't worry",
            "Claude is great",
            "",
        ] {
            assert!(!agents_query(text), "{text}");
        }
    }
}
