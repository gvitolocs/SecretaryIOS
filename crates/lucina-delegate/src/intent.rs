//! Deterministic parsing (it/en/da) for the JSON planner path, which never
//! offers tools: "chiedi a Grok le ultime notizie su X", "ask Claude to
//! review …", "fai una ricerca su …", the owner's switches ("non usare
//! Grok", "stop delegating", "puoi usare di nuovo Grok") and undo
//! ("annulla delega 1a2b3c4d").

use std::sync::OnceLock;

use regex::Regex;

use crate::types::{guess_lang, AgentId, Kind};

/// An explicit request to delegate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationIntent {
    pub to: Option<AgentId>,
    pub kind: Kind,
    pub task: String,
    pub workspace: Option<String>,
    pub lang: &'static str,
}

/// The owner's switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    /// `None`: all delegation.
    Off(Option<AgentId>),
    On(Option<AgentId>),
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchIntent {
    pub switch: Switch,
    pub lang: &'static str,
}

const AGENT: &str = r"(claude(?:\s+code)?|grok(?:\s+bot)?|chat\s*gpt|chatgpt|gpt|openai|codex)";

fn re(src: &str) -> Regex {
    Regex::new(src).expect("intent regex")
}

struct Patterns {
    ask: Regex,
    delegate: Regex,
    research: Regex,
    off_agent: Regex,
    off_all: Regex,
    on_agent: Regex,
    on_all: Regex,
    status: Regex,
    undo: Regex,
    code: Regex,
    research_words: Regex,
    analysis: Regex,
    workspace: Regex,
}

fn p() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    P.get_or_init(|| Patterns {
        // "chiedi a Grok …", "domanda a Claude se …", "fai fare a Claude …",
        // "senti ChatGPT su …", "ask Grok about …", "have Claude …", "get Claude to …",
        // "spørg Grok om …".
        ask: re(&format!(
            r"(?is)^\s*(?:lucina[,:]?\s+)?(?:(?:per\s+favore|please)[,]?\s+)?(?:chiedi|domanda|fai\s+fare|fai\s+cercare|fai\s+analizzare|senti|interpella|ask|have|get|let|spørg|bed)\s+(?:a\s+|ad\s+|to\s+)?{AGENT}\b[\s,:]*(?:(?:di|se|su|sul|sulla|sulle|sugli|riguardo(?:\s+a)?|per|to|about|whether|if|for|on|om|at|:)\b\s*)?(.+)$"
        )),
        // "delega a Claude: …", "delega …", "delegate to Grok …"
        delegate: re(&format!(
            r"(?is)^\s*(?:lucina[,:]?\s+)?(?:delega|delegate|deleger)\b[\s,:]*(?:(?:a|ad|to|til)\s+{AGENT}\b)?[\s,:]*(?:(?:il\s+compito|the\s+task|di|to)\b\s*)?(.+)$"
        )),
        // "fai una ricerca (online) su …", "cerca online …", "research …",
        // "latest news on …", "ultime notizie su …"
        research: re(
            r"(?is)^\s*(?:lucina[,:]?\s+)?(?:(?:fai|fammi)\s+una\s+ricerca(?:\s+online|\s+sul\s+web)?\s+(?:su|sul|sulla|sulle|sugli|riguardo(?:\s+a)?|di)\b|cerca\s+(?:online|sul\s+web|in\s+rete)\s+(?:su\s+|di\s+)?|(?:do\s+some\s+|do\s+a\s+)?research\s+(?:on\s+|about\s+)?|search\s+(?:the\s+web|online)\s+for\s+|(?:what\s+are\s+the\s+|give\s+me\s+the\s+|dammi\s+le\s+|quali\s+sono\s+le\s+)?(?:latest|ultime)\s+(?:news|notizie)\s+(?:on|about|su|sul|sulla|sulle|sugli|di|riguardo)\b)\s*(.+)$",
        ),
        off_agent: re(&format!(
            r"(?i)^\s*(?:lucina[,:]?\s+)?(?:non\s+usare(?:\s+più)?|smetti\s+di\s+usare|basta\s+(?:con\s+)?|don'?t\s+use|do\s+not\s+use|stop\s+using|no\s+more|brug\s+ikke|stop\s+med\s+at\s+bruge)\s+{AGENT}(?:\s+(?:per\s+ora|for\s+now|più|anymore|please|per\s+favore))*\s*[.!]*\s*$"
        )),
        off_all: re(
            r"(?i)^\s*(?:lucina[,:]?\s+)?(?:stop\s+delegating|don'?t\s+delegate(?:\s+anymore)?|no\s+more\s+delegation|smetti\s+di\s+delegare|non\s+delegare\s+più|basta\s+deleghe|stop\s+(?:alle\s+)?deleghe|stop\s+med\s+at\s+delegere)(?:\s+(?:per\s+ora|for\s+now|please|per\s+favore))*\s*[.!]*\s*$",
        ),
        on_agent: re(&format!(
            r"(?i)^\s*(?:lucina[,:]?\s+)?(?:puoi\s+(?:di\s+nuovo\s+|ancora\s+|tornare\s+a\s+)?usare|usa\s+(?:di\s+nuovo|pure)|riattiva|torna\s+a\s+usare|you\s+can\s+use|use|start\s+using|du\s+må\s+gerne\s+bruge)(?:\s+(?:di\s+nuovo|ancora|again|igen))?\s+{AGENT}(?:\s+(?:di\s+nuovo|again|igen|ancora|please|per\s+favore))*\s*[.!]*\s*$"
        )),
        on_all: re(
            r"(?i)^\s*(?:lucina[,:]?\s+)?(?:riprendi\s+a\s+delegare|puoi\s+(?:di\s+nuovo\s+|tornare\s+a\s+)?delegare(?:\s+di\s+nuovo)?|resume\s+delegating|start\s+delegating\s+again|you\s+can\s+delegate\s+again|genoptag\s+delegering)\s*[.!]*\s*$",
        ),
        status: re(
            r"(?i)^\s*(?:lucina[,:]?\s+)?(?:stato\s+(?:delle\s+)?deleghe|deleghe\s+di\s+oggi|delegation\s+status|delegations\s+today|status\s+for\s+delegering)\s*[?.!]*\s*$",
        ),
        undo: re(
            r"(?i)^\s*(?:lucina[,:]?\s+)?(?:annulla\s+(?:la\s+)?delega|undo\s+(?:the\s+)?delegation|fortryd\s+delegering)\s+([0-9a-f]{6,36})\s*[.!]*\s*$",
        ),
        code: re(
            r"(?i)\b(code|coding|codice|bug|fix|refactor|implement(?:a|are)?|repo|repository|script|crate|cargo|compila|test(?:s)?\b|function|funzione|pull\s+request|commit|programma|scrivi\s+(?:un|uno)\s+(?:programma|script))\b",
        ),
        research_words: re(
            r"(?i)\b(news|notizie|nyheder|latest|ultim[ei]|cerca|search|ricerca|research|online|web|twitter|tweets?|x\.com|trend|trending|oggi|today|this\s+week|questa\s+settimana)\b",
        ),
        analysis: re(
            r"(?i)\b(analizza|analisi|analy[sz]e|analysis|confronta|compare|comparison|valuta|evaluate|review|rivedi|pro\s+e\s+contro|pros\s+and\s+cons|trade-?offs?|riassumi\s+e\s+valuta)\b",
        ),
        workspace: re(r"(/home/nez/Projects/[A-Za-z0-9._-]+)"),
    })
}

/// The kind a task reads as.
pub fn infer_kind(task: &str, to: Option<AgentId>) -> Kind {
    let p = p();
    if p.code.is_match(task) && to != Some(AgentId::Grok) {
        Kind::Code
    } else if p.research_words.is_match(task) || to == Some(AgentId::Grok) {
        Kind::Research
    } else if p.analysis.is_match(task) {
        Kind::Analysis
    } else {
        Kind::Question
    }
}

fn clean_task(t: &str) -> String {
    t.trim()
        .trim_start_matches([':', ',', '-'])
        .trim()
        .to_owned()
}

/// An explicit delegation request, or `None`.
pub fn parse_delegation(text: &str) -> Option<DelegationIntent> {
    let p = p();
    let lang = guess_lang(text);
    let workspace = |task: &str| {
        p.workspace
            .captures(task)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim_end_matches(['.', ',']).to_owned())
    };
    if let Some(c) = p.ask.captures(text) {
        let to = c.get(1).and_then(|m| AgentId::parse(m.as_str()));
        let task = clean_task(c.get(2).map_or("", |m| m.as_str()));
        if task.chars().count() >= 3 {
            let kind = infer_kind(&task, to);
            return Some(DelegationIntent {
                to,
                kind,
                workspace: if kind == Kind::Code {
                    workspace(&task)
                } else {
                    None
                },
                task,
                lang,
            });
        }
    }
    if let Some(c) = p.delegate.captures(text) {
        let to = c.get(1).and_then(|m| AgentId::parse(m.as_str()));
        let task = clean_task(c.get(2).map_or("", |m| m.as_str()));
        if task.chars().count() >= 3 {
            let kind = infer_kind(&task, to);
            return Some(DelegationIntent {
                to,
                kind,
                workspace: if kind == Kind::Code {
                    workspace(&task)
                } else {
                    None
                },
                task,
                lang,
            });
        }
    }
    if let Some(c) = p.research.captures(text) {
        let topic = clean_task(c.get(1).map_or("", |m| m.as_str()));
        if topic.chars().count() >= 2 {
            return Some(DelegationIntent {
                to: None,
                kind: Kind::Research,
                task: text.trim().to_owned(),
                workspace: None,
                lang,
            });
        }
    }
    None
}

/// The owner's on/off/status switches (whole message only).
pub fn parse_switch(text: &str) -> Option<SwitchIntent> {
    let p = p();
    let lang = guess_lang(text);
    let agent = |r: &Regex| {
        r.captures(text)
            .and_then(|c| c.get(1))
            .and_then(|m| AgentId::parse(m.as_str()))
    };
    let switch = if let Some(a) = agent(&p.off_agent) {
        Switch::Off(Some(a))
    } else if p.off_all.is_match(text) {
        Switch::Off(None)
    } else if let Some(a) = agent(&p.on_agent) {
        Switch::On(Some(a))
    } else if p.on_all.is_match(text) {
        Switch::On(None)
    } else if p.status.is_match(text) {
        Switch::Status
    } else {
        return None;
    };
    Some(SwitchIntent { switch, lang })
}

/// "annulla delega <id>": the job id prefix.
pub fn parse_undo(text: &str) -> Option<(String, &'static str)> {
    p().undo
        .captures(text)
        .and_then(|c| c.get(1))
        .map(|m| (m.as_str().to_lowercase(), guess_lang(text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(t: &str) -> DelegationIntent {
        parse_delegation(t).unwrap_or_else(|| panic!("no intent: {t}"))
    }

    #[test]
    fn explicit_asks() {
        let i = d("Chiedi a Grok le ultime notizie su Rust 1.95");
        assert_eq!(
            (i.to, i.kind, i.lang),
            (Some(AgentId::Grok), Kind::Research, "it")
        );
        assert_eq!(i.task, "le ultime notizie su Rust 1.95");
        let i = d("chiedi a Claude di analizzare i pro e contro di SQLite rispetto a Postgres");
        assert_eq!((i.to, i.kind), (Some(AgentId::Claude), Kind::Analysis));
        assert!(i.task.starts_with("analizzare"));
        let i = d("ask ChatGPT whether a tomato is a fruit");
        assert_eq!(
            (i.to, i.kind, i.lang),
            (Some(AgentId::Chatgpt), Kind::Question, "en")
        );
        assert_eq!(i.task, "a tomato is a fruit");
        let i = d("ask Claude Code to fix the failing test in /home/nez/Projects/demo-app.");
        assert_eq!((i.to, i.kind), (Some(AgentId::Claude), Kind::Code));
        assert_eq!(i.workspace.as_deref(), Some("/home/nez/Projects/demo-app"));
        let i = d("delega a Claude: scrivi uno script che conta le righe dei file .rs");
        assert_eq!((i.to, i.kind), (Some(AgentId::Claude), Kind::Code));
        let i = d("delegate: compare Hetzner and OVH for a small VPS");
        assert_eq!((i.to, i.kind), (None, Kind::Analysis));
        let i = d("fai una ricerca su cosa dicono su X del nuovo iPhone");
        assert_eq!((i.to, i.kind), (None, Kind::Research));
        let i = d("latest news on the Rust Foundation");
        assert_eq!((i.to, i.kind), (None, Kind::Research));
        let i = d("spørg Grok om nyheder om Novo Nordisk");
        assert_eq!((i.to, i.lang), (Some(AgentId::Grok), "da"));
    }

    #[test]
    fn not_delegations() {
        for t in [
            "che tempo fa domani?",
            "aggiungi il latte alla lista",
            "chiedi a mamma se viene a cena",
            "ask me later",
            "ricordami di chiedere a Grok domani",
            "Grok è meglio di ChatGPT?",
        ] {
            assert_eq!(parse_delegation(t), None, "{t}");
        }
    }

    #[test]
    fn switches_and_undo() {
        let s = |t: &str| parse_switch(t).map(|s| s.switch);
        assert_eq!(s("non usare Grok"), Some(Switch::Off(Some(AgentId::Grok))));
        assert_eq!(
            s("Non usare più ChatGPT per ora."),
            Some(Switch::Off(Some(AgentId::Chatgpt)))
        );
        assert_eq!(
            s("don't use Claude"),
            Some(Switch::Off(Some(AgentId::Claude)))
        );
        assert_eq!(s("stop delegating"), Some(Switch::Off(None)));
        assert_eq!(s("smetti di delegare"), Some(Switch::Off(None)));
        assert_eq!(
            s("puoi usare di nuovo Grok"),
            Some(Switch::On(Some(AgentId::Grok)))
        );
        assert_eq!(
            s("you can use grok again"),
            Some(Switch::On(Some(AgentId::Grok)))
        );
        assert_eq!(s("riprendi a delegare"), Some(Switch::On(None)));
        assert_eq!(s("stato deleghe?"), Some(Switch::Status));
        assert_eq!(
            s("non usare Grok per cercare le notizie, chiedi a Claude"),
            None
        );
        assert_eq!(s("usa il latte"), None);
        assert_eq!(
            parse_undo("annulla delega 1a2b3c4d"),
            Some(("1a2b3c4d".into(), "it"))
        );
        assert_eq!(parse_undo("annulla il promemoria"), None);
    }
}
