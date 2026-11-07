//! The vocabulary: agents, task kinds, origins, requests, judgements, jobs.

use serde::{Deserialize, Serialize};

/// A delegation target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentId {
    Claude,
    Grok,
    Chatgpt,
    /// Lucina's own Mistral API model: never picked first, the fallback for non-code work.
    Mistral,
}

impl AgentId {
    pub const ALL: [AgentId; 4] = [
        AgentId::Claude,
        AgentId::Grok,
        AgentId::Chatgpt,
        AgentId::Mistral,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AgentId::Claude => "claude",
            AgentId::Grok => "grok",
            AgentId::Chatgpt => "chatgpt",
            AgentId::Mistral => "mistral",
        }
    }

    /// The name the owner sees.
    pub fn label(self) -> &'static str {
        match self {
            AgentId::Claude => "Claude",
            AgentId::Grok => "Grok",
            AgentId::Chatgpt => "ChatGPT",
            AgentId::Mistral => "Mistral",
        }
    }

    /// `claude`, `claude code`, `grok`, `xai`, `chatgpt`, `gpt`, `openai`, `codex`, `mistral`.
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim().to_lowercase();
        let t = t.trim_matches(|c: char| !c.is_alphanumeric() && c != ' ');
        match t {
            "claude" | "claude code" | "claude-code" | "claudecode" | "anthropic" => {
                Some(AgentId::Claude)
            }
            "grok" | "grok bot" | "grok-bot" | "xai" | "x.ai" => Some(AgentId::Grok),
            "chatgpt" | "chat gpt" | "gpt" | "openai" | "codex" | "chat-gpt" => {
                Some(AgentId::Chatgpt)
            }
            "mistral" | "mistral ai" | "le chat" => Some(AgentId::Mistral),
            _ => None,
        }
    }

    /// What this agent is best at (router and prompts).
    pub fn strengths(self) -> &'static str {
        match self {
            AgentId::Claude => "code and long, careful analysis",
            AgentId::Grok => "live web and X (Twitter) research and news",
            AgentId::Chatgpt => "general questions and second opinions",
            AgentId::Mistral => {
                "careful questions, research and analysis without live web (the fallback)"
            }
        }
    }
}

/// What kind of work is delegated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Question,
    Research,
    Analysis,
    Code,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Question => "question",
            Kind::Research => "research",
            Kind::Analysis => "analysis",
            Kind::Code => "code",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "question" | "domanda" | "q" => Some(Kind::Question),
            "research" | "ricerca" | "news" => Some(Kind::Research),
            "analysis" | "analisi" | "review" => Some(Kind::Analysis),
            "code" | "codice" | "coding" => Some(Kind::Code),
            _ => None,
        }
    }

    /// Nothing is written anywhere (the agent only reads and answers).
    pub fn read_only(self) -> bool {
        self != Kind::Code
    }
}

/// Who asked Lucina for this delegation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// The owner's own turn (app, web, Telegram).
    Owner,
    /// A Lucina MCP turn (ChatGPT, Grok, Claude Code calling Lucina): never delegates.
    Mcp,
    /// The proactive engine: read-only research only.
    Proactive,
    /// A turn that came from a delegated agent: never delegates again.
    Delegated,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Owner => "owner",
            Origin::Mcp => "mcp",
            Origin::Proactive => "proactive",
            Origin::Delegated => "delegated",
        }
    }
}

/// One delegation request.
#[derive(Debug, Clone)]
pub struct Request {
    /// `None`: Lucina picks the agent.
    pub to: Option<AgentId>,
    pub kind: Kind,
    pub task: String,
    /// Code only: a repo under the allowed roots (worked on as a sandbox clone).
    pub workspace: Option<String>,
    pub origin: Origin,
    /// Delegation depth of the turn that asks (0 for the owner's turns).
    pub depth: u8,
    /// Who is calling Lucina (an MCP caller, an agent), if anyone.
    pub caller: Option<String>,
    /// The chat session (cards go to its channel).
    pub session_id: String,
    /// `web` | `ios` | `telegram` | `proactive`.
    pub channel: String,
    /// `it` | `en` | `da`; `None`: from the task.
    pub lang: Option<String>,
    /// A previous job to continue (Claude `--resume`).
    pub resume_job: Option<String>,
}

impl Request {
    pub fn owner(kind: Kind, task: impl Into<String>) -> Self {
        Self {
            to: None,
            kind,
            task: task.into(),
            workspace: None,
            origin: Origin::Owner,
            depth: 0,
            caller: None,
            session_id: String::new(),
            channel: "web".into(),
            lang: None,
            resume_job: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    Low,
    Med,
    High,
}

impl Risk {
    pub fn as_str(self) -> &'static str {
        match self {
            Risk::Low => "low",
            Risk::Med => "med",
            Risk::High => "high",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Act,
    Skip,
    Revise,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Act => "act",
            Decision::Skip => "skip",
            Decision::Revise => "revise",
        }
    }
}

/// Lucina's self-review before a delegated call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Judgement {
    pub goal: String,
    pub risk: Risk,
    pub reversible: bool,
    pub why: String,
    pub decision: Decision,
    /// With `revise`: the task she sends instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revised_task: Option<String>,
    /// `model` or `fallback` (the judge model was unavailable).
    #[serde(default)]
    pub source: String,
}

impl Judgement {
    /// High risk and not reversible: she may act, but says so on the card.
    pub fn bold(&self) -> bool {
        self.risk == Risk::High && !self.reversible
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Running,
    Done,
    Error,
    Skipped,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Error => "error",
            JobStatus::Skipped => "skipped",
        }
    }
}

/// Another agent answered for the one asked (it was out of credits, signed out, unreachable or busy).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fallback {
    /// The agent asked for.
    pub from: AgentId,
    /// Its backend (`codex-cli`, `gbot`, ...); empty when it had none.
    pub from_backend: String,
    /// What it was, as the card says it: "out of credits", "busy with another job", ...
    pub reason: String,
}

/// A delegation job (persisted, redacted; no prompt contents beyond a short
/// redacted task line).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRecord {
    pub id: String,
    /// The agent asked for (named, or Lucina's first choice).
    pub agent: AgentId,
    /// The agent that answered when it is not `agent` (read with [`JobRecord::answered_by`]); older
    /// records have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<Fallback>,
    /// `claude-cli`, `gbot`, `xai-api`, `codex-cli`, `openai-api`, `mock`.
    pub backend: String,
    pub kind: Kind,
    pub origin: Origin,
    /// Who asked when it was not the owner (an MCP caller, an agent, her proactive engine). Such a
    /// job's result goes back to them and stays in the chat: it never alerts the owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    pub status: JobStatus,
    /// The task, redacted and clipped to 160 characters.
    pub task: String,
    pub lang: String,
    pub session_id: String,
    pub channel: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judgement: Option<Judgement>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub duration_ms: u64,
    /// The answer (redacted, at most 8000 characters; the full one is in the note).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// What the chat shows for a long answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note_slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Claude session (follow-ups with `--resume`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default)]
    pub turns: u32,
    /// The sandbox directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    /// Code on a repo: the branch Lucina left in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// How to undo what the job left behind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
}

impl JobRecord {
    pub fn short_id(&self) -> &str {
        &self.id[..self.id.len().min(8)]
    }

    /// The agent that answered: `agent` unless another one stood in.
    pub fn answered_by(&self) -> AgentId {
        self.answered_by.unwrap_or(self.agent)
    }
}

/// `en` or `da` from a text (function words): the language the agent is asked to answer in. Owner rule
/// (2026-11-02): never Italian, even for an Italian task or when nothing can be read.
pub fn guess_lang(text: &str) -> &'static str {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric() && c != 'à' && c != 'è' && c != 'ì')
        .filter(|w| !w.is_empty())
        .collect();
    let count = |list: &[&str]| words.iter().filter(|w| list.contains(w)).count();
    let it = count(&[
        "il", "lo", "la", "che", "di", "per", "non", "una", "un", "sono", "cosa", "come", "chiedi",
        "delle", "della", "sulle", "sul", "su", "ultime", "notizie", "fai", "puoi", "dimmi", "è",
        "gli", "anche", "quale", "quali", "perché", "con", "nel", "nella",
    ]) + lower.chars().filter(|c| "àèéìòù".contains(*c)).count();
    let da = count(&[
        "og", "ikke", "jeg", "det", "er", "på", "hvad", "spørg", "om", "til", "kan", "nyheder",
    ]) + lower.chars().filter(|c| "æøå".contains(*c)).count();
    let en = count(&[
        "the", "and", "what", "is", "are", "of", "to", "ask", "about", "latest", "news", "how",
        "please", "can", "you", "on", "in", "with", "why", "which",
    ]);
    if da > en && da > it {
        "da"
    } else {
        "en"
    }
}

/// The language an answer is written in for a language tag: Danish for `da`, English for everything else
/// (including `it`). Owner rule (2026-11-02): never Italian.
pub fn answer_lang(lang: &str) -> &'static str {
    if lang == "da" {
        "da"
    } else {
        "en"
    }
}

/// At most `max` characters, cut after the last full sentence when one
/// ends in the second half (a preview, not a chopped word).
pub fn clip_sentences(text: &str, max: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max {
        return t.to_owned();
    }
    let head: String = t.chars().take(max).collect();
    let cut = [". ", "! ", "? ", ".\n", "\n"]
        .iter()
        .filter_map(|p| head.rfind(p).map(|i| i + 1))
        .max()
        .filter(|&i| head[..i].chars().count() >= max / 2);
    match cut {
        Some(i) => format!("{} …", head[..i].trim_end()),
        None => clip(t, max),
    }
}

/// At most `max` characters, with an ellipsis.
pub fn clip(text: &str, max: usize) -> String {
    let t = text.trim();
    if t.chars().count() <= max {
        return t.to_owned();
    }
    let head: String = t.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", head.trim_end())
}

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_lang() {
        assert_eq!(AgentId::parse("Claude Code"), Some(AgentId::Claude));
        assert_eq!(AgentId::parse("xAI"), Some(AgentId::Grok));
        assert_eq!(AgentId::parse("Codex"), Some(AgentId::Chatgpt));
        assert_eq!(AgentId::parse("gemini"), None);
        assert_eq!(AgentId::parse("Mistral"), Some(AgentId::Mistral));
        assert_eq!(Kind::parse("Ricerca"), Some(Kind::Research));
        assert!(!Kind::Code.read_only());
        // Italian tasks are understood, and the answer comes back in English.
        assert_eq!(guess_lang("chiedi a Grok le ultime notizie su Rust"), "en");
        assert_eq!(guess_lang(""), "en");
        assert_eq!(guess_lang("ask Grok about the latest news on Rust"), "en");
        assert_eq!(guess_lang("spørg Grok om nyheder på dansk"), "da");
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(
            clip_sentences("Prima frase intera. Seconda frase che non entra tutta", 30),
            "Prima frase intera. …"
        );
        assert_eq!(clip_sentences("corta", 30), "corta");
    }
}
