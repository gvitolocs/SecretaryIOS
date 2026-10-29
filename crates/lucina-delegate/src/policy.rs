//! The hard limits (security, not autonomy), outbound prompt sanitising,
//! the agent router and the prompts the agents get.
//!
//! The self-review ([`crate::judge`]) decides whether Lucina acts; nothing
//! here asks the owner. These checks run before and after it and it cannot
//! override them.

use std::sync::OnceLock;

use regex::Regex;

use crate::types::{AgentId, Kind, Origin, Request};

/// Every delegated prompt starts with this marker; a turn that carries it
/// came from a delegated agent and never delegates again.
pub const MARKER: &str = "[Lucina delegation";

/// Why a delegation does not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// `LUCINA_DELEGATE` is off.
    Disabled,
    Empty,
    /// A Lucina MCP turn (or another outside caller) asked: never.
    McpOrigin,
    /// Depth limit: the request comes from a delegated turn.
    Depth,
    /// Back to the agent that is calling Lucina.
    Loop(AgentId),
    /// A purchase or money movement.
    Money,
    /// The proactive engine may only run read-only research.
    ProactiveReadOnly,
    /// The owner said "stop delegating".
    Stopped,
    /// The owner switched this agent off.
    AgentOff(AgentId),
    /// A daily cap was hit: delegation (or this agent) is paused for today.
    Paused(String),
    /// One job per agent at a time.
    Busy(AgentId),
    /// The agent cannot run right now (reason).
    Unavailable(AgentId, String),
    /// The agent does not do this kind of work.
    Unsupported(AgentId, Kind),
    /// No agent can take it now.
    NoAgent(String),
    /// A workspace outside the sandbox rules.
    Workspace(String),
}

impl Refusal {
    pub fn code(&self) -> &'static str {
        match self {
            Refusal::Disabled => "disabled",
            Refusal::Empty => "empty_task",
            Refusal::McpOrigin => "mcp_origin",
            Refusal::Depth => "depth_limit",
            Refusal::Loop(_) => "loop",
            Refusal::Money => "money",
            Refusal::ProactiveReadOnly => "proactive_read_only",
            Refusal::Stopped => "stopped",
            Refusal::AgentOff(_) => "agent_off",
            Refusal::Paused(_) => "paused",
            Refusal::Busy(_) => "busy",
            Refusal::Unavailable(..) => "unavailable",
            Refusal::Unsupported(..) => "unsupported",
            Refusal::NoAgent(_) => "no_agent",
            Refusal::Workspace(_) => "workspace",
        }
    }

    /// One sentence for the owner, in `lang`.
    pub fn message(&self, lang: &str) -> String {
        let it = lang == "it";
        let da = lang == "da";
        match self {
            Refusal::Disabled => pick(lang, "La delega ad altri agenti è spenta su questo host.", "Delegating to other agents is off on this host.", "Delegering til andre agenter er slået fra her."),
            Refusal::Empty => pick(lang, "Non ho capito cosa chiedere.", "I did not get what to ask.", "Jeg forstod ikke, hvad jeg skulle spørge om."),
            Refusal::McpOrigin => pick(lang, "Le richieste che arrivano da Lucina MCP non possono farmi delegare ad altri agenti.", "Requests that come through Lucina MCP cannot make me delegate to other agents.", "Forespørgsler via Lucina MCP kan ikke få mig til at delegere."),
            Refusal::Depth => pick(lang, "Questa richiesta arriva già da una delega: non delego di nuovo.", "This request already comes from a delegation: I do not delegate again.", "Denne forespørgsel kommer allerede fra en delegering."),
            Refusal::Loop(a) => if it { format!("Non rimando la richiesta a {} che me l'ha fatta.", a.label()) } else if da { format!("Jeg sender ikke opgaven tilbage til {}.", a.label()) } else { format!("I do not send the request back to {}, who asked it.", a.label()) },
            Refusal::Money => pick(lang, "Non delego acquisti né movimenti di denaro: quelli li fai tu.", "I never delegate purchases or money movements: those stay with you.", "Jeg delegerer aldrig køb eller pengeoverførsler."),
            Refusal::ProactiveReadOnly => pick(lang, "Dal motore proattivo delego solo ricerche in sola lettura.", "From the proactive engine I only delegate read-only research.", "Fra den proaktive motor delegerer jeg kun læse-research."),
            Refusal::Stopped => pick(lang, "Mi hai chiesto di non delegare più; dimmi \"riprendi a delegare\" per riattivarlo.", "You asked me to stop delegating; say \"resume delegating\" to turn it back on.", "Du bad mig stoppe med at delegere; sig \"genoptag delegering\"."),
            Refusal::AgentOff(a) => if it { format!("Mi hai detto di non usare {}; dimmi \"puoi usare {}\" per riattivarlo.", a.label(), a.label()) } else if da { format!("Du bad mig ikke bruge {}.", a.label()) } else { format!("You told me not to use {}; say \"you can use {} again\" to turn it back on.", a.label(), a.label()) },
            Refusal::Paused(why) => if it { format!("Ho messo in pausa le deleghe per oggi: {why}.") } else if da { format!("Delegering er sat på pause i dag: {why}.") } else { format!("Delegation is paused for today: {why}.") },
            Refusal::Busy(a) => if it { format!("{} sta già lavorando a un'altra mia richiesta; riprova quando finisce.", a.label()) } else if da { format!("{} arbejder allerede på en anden opgave.", a.label()) } else { format!("{} is already working on another request of mine; try again when it finishes.", a.label()) },
            Refusal::Unavailable(a, why) => if it { format!("{} non è raggiungibile adesso ({why}).", a.label()) } else if da { format!("{} er ikke tilgængelig lige nu ({why}).", a.label()) } else { format!("{} is not reachable right now ({why}).", a.label()) },
            Refusal::Unsupported(a, k) => if it { format!("{} non fa lavori di tipo {}.", a.label(), k.as_str()) } else if da { format!("{} laver ikke {}-opgaver.", a.label(), k.as_str()) } else { format!("{} does not do {} work.", a.label(), k.as_str()) },
            Refusal::NoAgent(why) => if it { format!("Nessun agente può prenderla adesso ({why}).") } else if da { format!("Ingen agent kan tage den nu ({why}).") } else { format!("No agent can take it right now ({why}).") },
            Refusal::Workspace(why) => if it { format!("Non lavoro in quella cartella: {why}.") } else if da { format!("Jeg arbejder ikke i den mappe: {why}.") } else { format!("I do not work in that folder: {why}.") },
        }
    }
}

fn pick(lang: &str, it: &str, en: &str, da: &str) -> String {
    match lang {
        "it" => it,
        "da" => da,
        _ => en,
    }
    .to_owned()
}

fn money_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?ix)
            ^\s*(?:please\s+|per\ favore\s+)?(?:buy|purchase|compra|comprami|acquista|køb|betal|overfør)\b
            | \b(?:buy|purchase)\s+(?:it|this|that|them|one|me|for\ me)\b
            | \border\s+(?:me|for\ me|one\ for)\b
            | \b(?:compra(?:mi|lo|la|li|le)|acquista(?:lo|la|li|le)|ordina(?:mi|lo|la|li|le))\b
            | \b(?:paga(?:re)?|pagami)\s+(?:il|la|lo|le|i|gli|per|questo|questa|quello|quella)\b
            | \bpay\s+(?:for|the|my|it|this|that|him|her|them)\b
            | \b(?:send|transfer|wire|move)\s+(?:money|funds|cash|crypto|bitcoin|€|\$|\d)
            | \b(?:manda|invia|trasferisci|sposta)\s+(?:soldi|denaro|fondi|€|\d)
            | \b(?:bonifico|wire\ transfer|money\ transfer|payment\ to|pagamento\ a)\b
            | \b(?:buy|sell|trade)\s+(?:my\s+|the\s+|some\s+|all\s+)?(?:stocks?|shares|crypto|bitcoin|btc|eth|options)\b
            | \b(?:compra|vendi)\s+(?:le\s+|i\s+|il\s+|mie\s+|miei\s+)?(?:azioni|crypto|bitcoin|btc|eth)\b
            | \b(?:add\ to\ cart\ and\ pay|place\ (?:an|the)\ order|effettua\ (?:un|l')\s*ordine|fai\ un\ ordine)\b
            | \bkøb\s+(?:det|den|dem|til\ mig)\b
            | \boverfør\s+(?:penge|\d)",
        )
        .expect("money regex")
    })
}

/// The task asks someone to buy, pay or move money (not: research about
/// buying, "which laptop should I buy?").
pub fn money_intent(task: &str) -> bool {
    money_re().is_match(task)
}

fn secret_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)secret://[^\s"'<>)\]]+"#).expect("secret ref regex"))
}

/// What leaves Lucina for a delegated model: secret refs removed, every
/// known secret value and token shape masked. `(text, changed)`.
pub fn sanitize_outbound(text: &str) -> (String, bool) {
    let without_refs = secret_ref_re().replace_all(text, "[secret ref removed]");
    let mut changed = matches!(without_refs, std::borrow::Cow::Owned(_));
    let mut out = without_refs.into_owned();
    if lucina_secrets::redactor().redact_string(&mut out) {
        changed = true;
    }
    (out, changed)
}

/// The checks that never move. `target` is the chosen agent when known.
pub fn hard_limits(req: &Request, target: Option<AgentId>, max_depth: u8) -> Result<(), Refusal> {
    if req.task.trim().is_empty() {
        return Err(Refusal::Empty);
    }
    if req.origin == Origin::Mcp {
        return Err(Refusal::McpOrigin);
    }
    if req.origin == Origin::Delegated || req.depth >= max_depth.min(1) || req.task.contains(MARKER)
    {
        return Err(Refusal::Depth);
    }
    if let (Some(t), Some(caller)) = (target, req.caller.as_deref()) {
        if AgentId::parse(caller) == Some(t) || caller.to_lowercase().contains(t.as_str()) {
            return Err(Refusal::Loop(t));
        }
    }
    if money_intent(&req.task) {
        return Err(Refusal::Money);
    }
    if req.origin == Origin::Proactive && (!req.kind.read_only() || req.workspace.is_some()) {
        return Err(Refusal::ProactiveReadOnly);
    }
    Ok(())
}

fn news_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(news|notizie|nyheder|latest|ultim[ei]|oggi su|today|twitter|tweets?|x\.com|trending|breaking|stock price|prezzo attuale|current price|elezioni|election|this week|questa settimana)\b")
            .expect("news regex")
    })
}

/// The agent order for a kind: Grok for live web and X/news research, Claude
/// for code and long analysis, ChatGPT for general second opinions.
pub fn preference(kind: Kind, task: &str) -> Vec<AgentId> {
    use AgentId::*;
    match kind {
        Kind::Code => vec![Claude],
        Kind::Research => vec![Grok, Chatgpt, Claude],
        Kind::Analysis => vec![Claude, Chatgpt, Grok],
        Kind::Question if news_re().is_match(task) => vec![Grok, Chatgpt, Claude],
        Kind::Question => vec![Chatgpt, Claude, Grok],
    }
}

/// The prompt an agent gets: who asks and why, the rules, the task. The
/// task must already be sanitised.
pub fn build_prompt(
    job_short: &str,
    agent: AgentId,
    kind: Kind,
    task: &str,
    lang: &str,
    sandbox: Option<&str>,
    branch: Option<&str>,
) -> String {
    let it = lang == "it";
    let header = format!("{MARKER} · job {job_short} · depth 1/1]");
    let who = if it {
        "Questa richiesta arriva da Lucina, l'assistente personale di Giuseppe, che te la manda per suo conto. Giuseppe non sta scrivendo direttamente."
    } else {
        "This request comes from Lucina, Giuseppe's personal assistant, who sends it on his behalf. Giuseppe is not typing it himself."
    };
    let rules = if it {
        "Regole: rispondi direttamente e in modo autonomo; non contattare Lucina (né Lucina MCP) e non delegare ad altri agenti; non comprare nulla, non pagare e non spostare denaro; non chiedere né usare password, token o segreti; non pubblicare né inviare messaggi a nome di Giuseppe a meno che il compito lo chieda esplicitamente."
    } else {
        "Rules: answer directly and on your own; do not contact Lucina (or Lucina MCP) and do not delegate to other agents; never buy, pay or move money; never ask for or use passwords, tokens or secrets; do not publish or send messages on Giuseppe's behalf unless the task explicitly asks for it."
    };
    let how = match (kind, it) {
        (Kind::Research, true) => "Usa fonti web aggiornate (e X quando serve), cita i link e indica le date. Chiudi con un riassunto di 2-3 frasi.".to_owned(),
        (Kind::Research, false) => "Use current web sources (and X when useful), cite links and give dates. End with a 2-3 sentence summary.".to_owned(),
        (Kind::Analysis, true) => "Sii accurato e strutturato; chiudi con un riassunto breve e una raccomandazione.".to_owned(),
        (Kind::Analysis, false) => "Be careful and structured; end with a short summary and a recommendation.".to_owned(),
        (Kind::Question, true) => "Rispondi in modo conciso.".to_owned(),
        (Kind::Question, false) => "Answer concisely.".to_owned(),
        (Kind::Code, _) => {
            let dir = sandbox.unwrap_or(".");
            let br = branch
                .map(|b| format!(" Commit your work on branch {b} (already checked out)."))
                .unwrap_or_default();
            format!("Work only inside {dir}.{br} Never push, never touch main or master, never deploy, never use SSH, systemctl or docker. Run the tests you can. End with a short report: what changed, how it was verified, what is left.")
        }
    };
    let task_label = if it { "Compito" } else { "Task" };
    format!(
        "{header}\n{who}\n{rules}\n{how}\n\n{task_label} ({kind}, for {agent}):\n{task}",
        kind = kind.as_str(),
        agent = agent.label()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(origin: Origin, kind: Kind, task: &str) -> Request {
        Request {
            origin,
            ..Request::owner(kind, task)
        }
    }

    #[test]
    fn policy_matrix() {
        use Kind::*;
        use Origin::*;
        // origin × kind: who may delegate what (money aside)
        let cases = [
            (Owner, Question, Ok(())),
            (Owner, Research, Ok(())),
            (Owner, Analysis, Ok(())),
            (Owner, Code, Ok(())),
            (Mcp, Question, Err(Refusal::McpOrigin)),
            (Mcp, Code, Err(Refusal::McpOrigin)),
            (Delegated, Research, Err(Refusal::Depth)),
            (Proactive, Research, Ok(())),
            (Proactive, Question, Ok(())),
            (Proactive, Analysis, Ok(())),
            (Proactive, Code, Err(Refusal::ProactiveReadOnly)),
        ];
        for (o, k, want) in cases {
            assert_eq!(
                hard_limits(&req(o, k, "what is new in Rust 1.95?"), None, 1),
                want,
                "{o:?} {k:?}"
            );
        }
        let mut r = req(Owner, Research, "x");
        r.depth = 1;
        assert_eq!(hard_limits(&r, None, 1), Err(Refusal::Depth));
        assert_eq!(
            hard_limits(&r, None, 9),
            Err(Refusal::Depth),
            "depth stays 1"
        );
        let r = req(Owner, Question, &format!("{MARKER} · job x] hi"));
        assert_eq!(hard_limits(&r, None, 1), Err(Refusal::Depth));
        let mut r = req(Owner, Question, "hi");
        r.caller = Some("grok".into());
        assert_eq!(
            hard_limits(&r, Some(AgentId::Grok), 1),
            Err(Refusal::Loop(AgentId::Grok))
        );
        assert_eq!(hard_limits(&r, Some(AgentId::Claude), 1), Ok(()));
        assert_eq!(
            hard_limits(&req(Owner, Question, "  "), None, 1),
            Err(Refusal::Empty)
        );
        let mut r = req(Proactive, Research, "x");
        r.workspace = Some("/home/nez/Projects/a".into());
        assert_eq!(hard_limits(&r, None, 1), Err(Refusal::ProactiveReadOnly));
    }

    #[test]
    fn money_is_never_delegated() {
        for t in [
            "buy the cheapest flight to Rome",
            "compra le cuffie Sony su Amazon",
            "comprami un biglietto",
            "paga la bolletta della luce",
            "pay the invoice from Hetzner",
            "send money to Marco",
            "trasferisci 50€ a Luca",
            "fai un bonifico a Marco",
            "sell my bitcoin",
            "place an order for 3 pizzas",
            "køb det til mig",
            "Please purchase the domain",
        ] {
            assert!(money_intent(t), "{t}");
            assert_eq!(
                hard_limits(&Request::owner(Kind::Question, t), None, 1),
                Err(Refusal::Money),
                "{t}"
            );
        }
        for t in [
            "which laptop should I buy for coding?",
            "research the best time to buy flights to Rome",
            "confronta i prezzi delle cuffie Sony",
            "in order to compare, list the Rust web frameworks",
            "ordina per data le ultime notizie su Rust",
            "latest news on X about the payment industry",
        ] {
            assert!(!money_intent(t), "{t}");
        }
    }

    #[test]
    fn outbound_prompts_are_redacted() {
        let value = "Zq9vT2pLx7Wm4Rk8Yb3Nc6Hd1Sf5Gj0";
        lucina_secrets::redactor().register("DELEGATE_TEST_TOKEN", value);
        let task = format!(
            "debug this: curl -H 'Authorization: Bearer {value}' and the ref secret://lucina/prod/lucina#OPENAI_API_KEY"
        );
        let (out, changed) = sanitize_outbound(&task);
        assert!(changed);
        assert!(!out.contains(value), "{out}");
        assert!(!out.contains("secret://"), "{out}");
        assert!(out.contains("[secret ref removed]"));
        let (same, changed) = sanitize_outbound("what is new in Rust?");
        assert!(!changed);
        assert_eq!(same, "what is new in Rust?");
        // token shapes, unknown to the redactor
        let (out, _) = sanitize_outbound("key sk-proj-AbCdEf0123456789AbCdEf0123456789 please");
        assert!(!out.contains("sk-proj-AbCdEf0123456789"), "{out}");
    }

    #[test]
    fn routing() {
        use AgentId::*;
        assert_eq!(preference(Kind::Code, "fix it"), vec![Claude]);
        assert_eq!(preference(Kind::Research, "x")[0], Grok);
        assert_eq!(preference(Kind::Analysis, "x")[0], Claude);
        assert_eq!(preference(Kind::Question, "capital of Peru?")[0], Chatgpt);
        assert_eq!(preference(Kind::Question, "latest news on Rust")[0], Grok);
    }

    #[test]
    fn prompts_say_who_asks() {
        let p = build_prompt(
            "abcd1234",
            AgentId::Grok,
            Kind::Research,
            "ultime notizie su Rust",
            "it",
            None,
            None,
        );
        assert!(p.starts_with(MARKER));
        assert!(p.contains("da Lucina"));
        assert!(p.contains("non delegare"));
        assert!(p.contains("ultime notizie su Rust"));
        let p = build_prompt(
            "abcd1234",
            AgentId::Claude,
            Kind::Code,
            "fix",
            "en",
            Some("/s/x"),
            Some("lucina/abcd1234"),
        );
        assert!(p.contains("on his behalf"));
        assert!(p.contains("Never push"));
        assert!(p.contains("lucina/abcd1234"));
    }
}
