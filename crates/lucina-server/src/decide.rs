//! Decisions: an agent asks Lucina for a go-ahead (`docs/design/decisions-api.md`).
//!
//! Owner rule (2026-10-28): agents do not ask Giuseppe "may I?"; they ask Lucina with a structured
//! request (question, options, recommendation, impact, reversible), never free text through the chat's
//! intent routing. She decides from his own words (timeline, Honcho, never an agent's text), her
//! earlier decisions on the same subject and two independent checks (DeepSeek and Jev), or asks him on
//! his phone with one button per option and waits for his answer.
//!
//! - [`create`]: validate, gather evidence, decide or ask, record, report on the timeline.
//! - [`owner_answer`]: his reply ("Decision dc-…: 2") in his own chat resolves a pending decision.
//! - Records: `<LUCINA_DATA_DIR>/decisions/decisions.jsonl`, the full object after every change
//!   (the last line of an id wins), and every settled decision in the ops record (`kind=decision`,
//!   crate `lucina_ops`), whose earlier decisions are evidence for the next ones.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use lucina_jev::Question;
use lucina_llm::{ChatRequest, Message};
use lucina_store::timeline::{Meta, NewItem, TimelineItem};
use lucina_store::timeutil::{iso_ms, now_iso};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::ApiError;
use crate::AppState;

pub const MAX_QUESTION: usize = 2000;
pub const MAX_OPTION: usize = 200;
pub const MAX_OPTIONS: usize = 6;
pub const MAX_CONTEXT: usize = 8000;
/// A pending decision the owner has not answered within this becomes `expired`.
pub const PENDING_TTL: chrono::Duration = chrono::Duration::hours(24);
/// Lucina decides herself from this confidence up (low and medium impact).
pub const MIN_CONFIDENCE_LOW: f64 = 0.6;
pub const MIN_CONFIDENCE_MEDIUM: f64 = 0.85;
/// Jev's "the evidence goes against the recommendation" from this probability up is a conflict.
pub const CONFLICT_MIN: f64 = 0.6;
/// Evidence lines returned to the agent, and their length.
const EVIDENCE_OUT: usize = 6;
const EVIDENCE_CHARS: usize = 300;
/// How far back his own messages count as evidence.
const OWNER_MESSAGE_DAYS: i64 = 14;
const MODEL_TIMEOUT: Duration = Duration::from_secs(25);
const MEMORY_TIMEOUT: Duration = Duration::from_secs(6);

// ---------------------------------------------------------------- the request

/// A validated `POST /api/secretary/decisions` body.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub question: String,
    pub options: Vec<String>,
    /// 0-based index into `options`.
    pub recommendation: Option<usize>,
    pub context: String,
    pub reversible: bool,
    pub impact: Impact,
    pub urgency: String,
    pub topic: Option<String>,
    pub asking_session: Option<String>,
    pub asking_client: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Impact {
    Low,
    Medium,
    High,
}

impl Impact {
    pub fn as_str(self) -> &'static str {
        match self {
            Impact::Low => "low",
            Impact::Medium => "medium",
            Impact::High => "high",
        }
    }
}

fn text_field(v: &Value, key: &str, max: usize, required: bool) -> Result<Option<String>, String> {
    match &v[key] {
        Value::Null if !required => Ok(None),
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                return if required {
                    Err(format!("{key} must not be empty"))
                } else {
                    Ok(None)
                };
            }
            if t.chars().count() > max {
                return Err(format!("{key} is longer than {max} characters"));
            }
            if t.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
                return Err(format!("{key} contains control characters"));
            }
            Ok(Some(t.to_owned()))
        }
        Value::Null => Err(format!("{key} is required")),
        _ => Err(format!("{key} must be a string")),
    }
}

/// The body as the contract describes it; the message names the bad field.
pub fn parse_request(v: &Value) -> Result<Request, String> {
    if !v.is_object() {
        return Err("the body must be a JSON object".into());
    }
    let question = text_field(v, "question", MAX_QUESTION, true)?.unwrap_or_default();
    let raw = v["options"]
        .as_array()
        .ok_or("options must be an array of 2-6 strings")?;
    if raw.len() < 2 || raw.len() > MAX_OPTIONS {
        return Err(format!("options must have 2-{MAX_OPTIONS} entries"));
    }
    let mut options = Vec::with_capacity(raw.len());
    for (i, o) in raw.iter().enumerate() {
        let wrapped = json!({ "o": o });
        let s = text_field(&wrapped, "o", MAX_OPTION, true)
            .map_err(|e| e.replacen('o', &format!("options[{i}]"), 1))?
            .unwrap_or_default();
        if options.iter().any(|x: &String| x.eq_ignore_ascii_case(&s)) {
            return Err(format!("options[{i}] repeats an earlier option"));
        }
        options.push(s);
    }
    let recommendation = match text_field(v, "recommendation", MAX_OPTION, false)? {
        None => None,
        Some(r) => Some(option_index(&options, &r).ok_or(
            "recommendation must be one of the options (its text or its number)",
        )?),
    };
    let context = text_field(v, "context", MAX_CONTEXT, false)?.unwrap_or_default();
    let reversible = v["reversible"]
        .as_bool()
        .ok_or("reversible is required (true or false)")?;
    let impact = match v["impact"].as_str() {
        Some("low") => Impact::Low,
        Some("medium") => Impact::Medium,
        Some("high") => Impact::High,
        _ => return Err("impact must be low, medium or high".into()),
    };
    let urgency = match v["urgency"].as_str() {
        None => "normal",
        Some(u @ ("low" | "normal" | "high")) => u,
        Some(_) => return Err("urgency must be low, normal or high".into()),
    }
    .to_owned();
    let topic = text_field(v, "topic", 64, false)?;
    let asking_session = text_field(v, "asking_session", 120, false)?;
    let asking_client = text_field(v, "asking_client", 32, false)?
        .map(|c| lucina_intent::mcp::note_source(&c))
        .unwrap_or_else(|| "unknown".into());
    Ok(Request {
        question,
        options,
        recommendation,
        context,
        reversible,
        impact,
        urgency,
        topic,
        asking_session,
        asking_client,
    })
}

/// `"Deploy now"` or `"2"` → the option's 0-based index.
pub fn option_index(options: &[String], answer: &str) -> Option<usize> {
    let a = answer.trim().trim_end_matches(['.', ')']).trim();
    if let Ok(n) = a.parse::<usize>() {
        return (1..=options.len()).contains(&n).then(|| n - 1);
    }
    options.iter().position(|o| o.eq_ignore_ascii_case(a))
}

/// Questions only the owner may answer: money, credentials, terms, messages in his name.
pub fn owner_only(r: &Request) -> bool {
    let text = format!("{}\n{}\n{}", r.question, r.options.join("\n"), r.context);
    lucina_intent::re!(
        r"\b(buy|purchase|checkout|pay for|make a payment|place (an|the) order|spend (\$|€|eur|usd|dkk)?\s?\d|credit card|carta di credito|bank transfer|bonifico|enter (his|the owner'?s|giuseppe'?s) (password|credentials|card|pin)|accept (the )?(terms|tos|agreement|license|licence)|sign (the )?(contract|agreement)|send (an? )?(e-?mail|message|dm|sms|text) (to|on (his|giuseppe'?s) behalf)|on (his|giuseppe'?s|the owner'?s) behalf|compra(re)?|acquista(re)?|paga(re)? )\b",
        "i"
    )
    .test(&text)
        || handles_secret(&format!("{}\n{}", r.question, r.options.join("\n")))
}

/// The question itself is about handling a credential (where a token lives, rotating, sharing,
/// granting access), not a mention of one ("0 secret hits" in a review summary).
fn handles_secret(text: &str) -> bool {
    lucina_intent::re!(
        r"\b(store|move|put|place|keep|copy|rotate|revoke|reissue|re-issue|mint|issue|share|reveal|expose|grant|give|hand|upload|paste|delete|remove|wire)\b[^.?!\n]{0,60}\b(secrets?|credentials?|api[- ]?keys?|passwords?|tokens?|infisical)\b|\b(secrets?|credentials?|api[- ]?keys?|passwords?|tokens?)\b[^.?!\n]{0,40}\b(at|in|into|to|from|for)\b[^.?!\n]{0,20}\b(infisical|[a-z0-9_-]+/(prod|dev|staging))\b",
        "i"
    )
    .test(text)
}

// ---------------------------------------------------------------- the record

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Evidence {
    pub kind: String,
    pub text: String,
    /// How much it says about what Giuseppe wants ([`TIERS`]): 1 his own words, 2 his own actions,
    /// 3 Lucina's earlier decisions (context only), 4 records written by agents (facts about what
    /// happened, never his wishes, even when they say he decided).
    #[serde(default)]
    pub tier: u8,
}

/// The evidence tiers, best first: (tier, basis name, how a reason names it).
pub const TIERS: &[(u8, &str, &str)] = &[
    (1, "owner_words", "Giuseppe's own words"),
    (2, "owner_action", "Giuseppe's own actions"),
    (3, "lucina_decision", "my earlier decision"),
    (4, "agent_record", "agents' records"),
];

/// Which basis a decision may name, given the tiers its evidence has: only a tier that is there,
/// and never agents' records (they are facts, not his wishes). Anything else is my own judgement.
pub fn valid_basis(claimed: &str, tiers: &[u8]) -> &'static str {
    TIERS
        .iter()
        .find(|(t, name, _)| *name == claimed && *t <= 3 && tiers.contains(t))
        .map(|(_, name, _)| *name)
        .unwrap_or("own_judgement")
}

/// A reason that speaks for him ("Giuseppe decided …", "he told me …").
fn cites_him(reason: &str) -> bool {
    lucina_intent::re!(
        r"\b(giuseppe|the owner|he|his)('s)?\b[^.]{0,30}\b(decided|decision|said|told|wants|wanted|approved|asked|prefers|rule|chose)\b",
        "i"
    )
    .test(reason)
}

fn basis_words(basis: &str) -> &'static str {
    TIERS
        .iter()
        .find(|(_, name, _)| *name == basis)
        .map(|(_, _, words)| *words)
        .unwrap_or("my own judgement")
}

/// Where an ops record stands: only what Lucina wrote from his own turn or tap (`source: lucina`,
/// `decided_by: owner`) is his word; her own records are her decisions; everything else (backfill,
/// `claude:*`, a session's name, `mcp:*`) is an agent's record, whatever it says about who decided.
pub fn ops_tier(source: &str, decided_by: Option<lucina_ops::DecidedBy>) -> (u8, &'static str) {
    match (source, decided_by) {
        ("lucina", Some(lucina_ops::DecidedBy::Owner)) => (1, "owner_decision"),
        ("lucina", _) => (3, "lucina_decision"),
        _ => (4, "agent_record"),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Decision {
    pub decision_id: String,
    pub record_id: String,
    pub status: String,
    pub decision: Option<String>,
    pub chosen_option: Option<String>,
    pub chosen_index: Option<usize>,
    pub reason: String,
    pub decided_by: Option<String>,
    pub confidence: Option<f64>,
    pub asked_owner: bool,
    pub owner_only: bool,
    pub question: String,
    pub options: Vec<String>,
    pub recommendation: Option<String>,
    #[serde(default)]
    pub context: String,
    pub impact: String,
    pub reversible: bool,
    pub urgency: String,
    pub topic: Option<String>,
    pub asking_client: String,
    pub asking_session: Option<String>,
    pub created_at: String,
    pub decided_at: Option<String>,
    pub expires_at: Option<String>,
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    /// What the decision rests on: `owner_words`, `owner_action`, `lucina_decision` or `own_judgement`.
    #[serde(default)]
    pub basis: Option<String>,
    /// The owner's timeline item with the buttons (pending decisions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_item_id: Option<String>,
}

impl Decision {
    /// The API answer (no context, no internal ids).
    pub fn public(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.remove("context");
            o.remove("owner_item_id");
            if let Some(Value::Array(e)) = o.get_mut("evidence") {
                e.truncate(EVIDENCE_OUT);
            }
        }
        v
    }

    fn who(&self) -> String {
        match &self.asking_session {
            Some(s) => format!("{} ({s})", self.asking_client),
            None => self.asking_client.clone(),
        }
    }

    fn choose(&mut self, index: usize, by: &str, confidence: f64, reason: String, basis: &str) {
        self.basis = Some(basis.into());
        self.status = "decided".into();
        self.decision = Some(
            if self.recommendation.as_deref() == Some(self.options[index].as_str()) {
                "go"
            } else {
                "change"
            }
            .into(),
        );
        self.chosen_index = Some(index + 1);
        self.chosen_option = Some(self.options[index].clone());
        self.decided_by = Some(by.into());
        self.confidence = Some(confidence);
        self.reason = reason;
        self.decided_at = Some(now_iso());
        self.expires_at = None;
    }
}

/// The append-only decision log of one data directory.
pub struct Store {
    path: PathBuf,
    items: Mutex<Vec<Decision>>,
}

impl Store {
    pub fn open(dir: &Path) -> Self {
        let path = dir.join("decisions").join("decisions.jsonl");
        let mut items: Vec<Decision> = Vec::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                match serde_json::from_str::<Decision>(line) {
                    Ok(d) => match items.iter_mut().find(|x| x.decision_id == d.decision_id) {
                        Some(x) => *x = d,
                        None => items.push(d),
                    },
                    Err(e) => tracing::warn!(error = %e, "decisions log: unreadable line skipped"),
                }
            }
        }
        Self {
            path,
            items: Mutex::new(items),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Decision>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Append the decision as it is now and keep it in memory.
    pub fn put(&self, d: &Decision) -> std::io::Result<()> {
        let mut items = self.lock();
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut line = serde_json::to_string(d).map_err(std::io::Error::other)?;
        line.push('\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        f.write_all(line.as_bytes())?;
        f.sync_data()?;
        match items.iter_mut().find(|x| x.decision_id == d.decision_id) {
            Some(x) => *x = d.clone(),
            None => items.push(d.clone()),
        }
        Ok(())
    }

    /// The decision, with an unanswered one past its time marked `expired` (and recorded).
    pub fn get(&self, id: &str, now: DateTime<Utc>) -> Option<Decision> {
        let mut d = self.lock().iter().find(|d| d.decision_id == id).cloned()?;
        if expire(&mut d, now) {
            if let Err(e) = self.put(&d) {
                tracing::warn!(error = %e, "decisions log write failed");
            }
        }
        Some(d)
    }

    /// Newest first.
    pub fn list(&self, status: Option<&str>, limit: usize, now: DateTime<Utc>) -> Vec<Decision> {
        let ids: Vec<String> = self
            .lock()
            .iter()
            .rev()
            .map(|d| d.decision_id.clone())
            .collect();
        ids.iter()
            .filter_map(|id| self.get(id, now))
            .filter(|d| status.is_none_or(|s| d.status == s))
            .take(limit)
            .collect()
    }

    /// Earlier settled decisions on the same subject (same topic, or shared words), newest first.
    pub fn similar(&self, r: &Request, limit: usize) -> Vec<Decision> {
        let words = keywords(&format!("{} {}", r.question, r.options.join(" ")));
        let mut scored: Vec<(usize, Decision)> = self
            .lock()
            .iter()
            .rev()
            .filter(|d| d.status == "decided")
            .filter_map(|d| {
                let same_topic = r.topic.is_some() && d.topic == r.topic;
                let shared = overlap(&words, &format!("{} {}", d.question, d.options.join(" ")));
                (same_topic || shared >= 3).then(|| (shared + if same_topic { 10 } else { 0 }, d.clone()))
            })
            .collect();
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        scored.into_iter().take(limit).map(|(_, d)| d).collect()
    }
}

fn expire(d: &mut Decision, now: DateTime<Utc>) -> bool {
    let due = d
        .expires_at
        .as_deref()
        .and_then(|e| DateTime::parse_from_rfc3339(e).ok())
        .is_some_and(|e| e.with_timezone(&Utc) <= now);
    if d.status == "pending" && due {
        d.status = "expired".into();
        d.reason = format!("{} Giuseppe did not answer within 24 hours.", d.reason)
            .trim()
            .to_owned();
        return true;
    }
    false
}

/// The store of `dir` (one per data directory, opened once).
pub fn store(dir: &Path) -> Arc<Store> {
    static STORES: OnceLock<Mutex<HashMap<PathBuf, Arc<Store>>>> = OnceLock::new();
    let mut m = STORES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    m.entry(dir.to_path_buf())
        .or_insert_with(|| Arc::new(Store::open(dir)))
        .clone()
}

// ---------------------------------------------------------------- evidence

const STOPWORDS: &[&str] = &[
    "this", "that", "with", "from", "have", "should", "would", "could", "there", "their", "about",
    "which", "what", "when", "where", "will", "into", "your", "they", "them", "then", "than", "also",
    "only", "just", "more", "some", "other", "need", "needs", "want", "please", "question", "option",
    "options", "recommend", "recommendation", "owner", "giuseppe", "lucina", "agent", "session",
    "della", "delle", "dello", "sono", "come", "questo", "quello", "anche", "ancora", "dopo", "prima",
    "fare", "deve", "posso", "vuoi", "perché", "quando",
];

fn keywords(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .filter(|w| w.chars().count() >= 4 && !STOPWORDS.contains(w))
        .map(str::to_owned)
        .collect();
    out.sort();
    out.dedup();
    out
}

fn overlap(words: &[String], text: &str) -> usize {
    let lower = text.to_lowercase();
    words.iter().filter(|w| lower.contains(w.as_str())).count()
}

fn clip(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let head: String = flat.chars().take(max - 1).collect();
    format!("{}…", head.trim_end())
}

fn day(ts: &str) -> String {
    ts.get(..10).unwrap_or(ts).to_owned()
}

/// Text that talks about the owner in the third person was written by someone else (an agent that
/// posted through the app's chat without the MCP marker), never by him.
fn third_person(text: &str) -> bool {
    lucina_intent::re!(r"\b(the owner|giuseppe|he said|he wants|his phone)\b", "i").test(text)
}

/// His own recent messages that talk about the same thing (never an MCP caller's).
fn owner_messages(st: &AppState, words: &[String], now: DateTime<Utc>) -> Vec<Evidence> {
    let need = if words.len() >= 4 { 2 } else { 1 };
    let since = now - chrono::Duration::days(OWNER_MESSAGE_DAYS);
    let mut found: Vec<(usize, TimelineItem)> = Vec::new();
    let mut before: Option<String> = None;
    for _ in 0..10 {
        let Ok((items, next)) = st.timeline.list(before.as_deref(), 200) else {
            break;
        };
        let mut older = false;
        for raw in &items {
            let Ok(item) = serde_json::from_str::<TimelineItem>(&raw.raw) else {
                continue;
            };
            if DateTime::parse_from_rfc3339(&item.ts).is_ok_and(|t| t.with_timezone(&Utc) < since) {
                older = true;
                continue;
            }
            let agent = item.meta.as_ref().is_some_and(|m| m.via.as_deref() == Some("mcp"));
            if item.role != "user" || agent || item.text.trim().is_empty() || third_person(&item.text) {
                continue;
            }
            let score = overlap(words, &item.text);
            if score >= need {
                found.push((score, item));
            }
        }
        match next {
            Some(n) if !older => before = Some(n),
            _ => break,
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.ts.cmp(&a.1.ts)));
    found
        .into_iter()
        .take(4)
        .map(|(_, i)| Evidence {
            tier: 1,
            kind: "owner_message".into(),
            text: clip(&format!("[{} {}] Giuseppe: {}", day(&i.ts), i.channel, i.text), EVIDENCE_CHARS),
        })
        .collect()
}

/// His long-term memory (Honcho), his own messages only.
async fn owner_memory(st: &AppState, r: &Request, query: &str) -> Vec<Evidence> {
    let mcp = lucina_intent::mcp::McpTurn {
        caller: r.asking_client.clone(),
        text: String::new(),
        allow: Vec::new(),
        conversation: None,
    };
    let surface = crate::surface::chat_surface(
        &st.cfg,
        Some(crate::surface::BearerVerified::checked()),
        "web",
        Some(&mcp),
    );
    let owner = st.cfg.owner_peer_id.clone();
    match tokio::time::timeout(MEMORY_TIMEOUT, st.memory.search(&surface, query, 8)).await {
        Ok(Ok(msgs)) => msgs
            .into_iter()
            .filter(|m| m.peer_id.as_deref().is_none_or(|p| p == owner))
            .filter(|m| {
                !m.metadata.as_ref().is_some_and(|md| {
                    md["via"] == "mcp" || md["speaker"] == "agent"
                }) && !m.content.contains("via Lucina MCP")
                    && !third_person(&m.content)
            })
            .take(4)
            .map(|m| Evidence {
                tier: 1,
                kind: "owner_memory".into(),
                text: clip(
                    &format!(
                        "[{}] Giuseppe: {}",
                        m.created_at.as_deref().map(day).unwrap_or_default(),
                        m.content
                    ),
                    EVIDENCE_CHARS,
                ),
            })
            .collect(),
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "decision: memory search unavailable");
            Vec::new()
        }
        Err(_) => {
            tracing::warn!("decision: memory search timed out");
            Vec::new()
        }
    }
}

/// His own actions: secrets he saved through the Add-secret sheet, root steps he approved or denied
/// on his iPhone (the step's description is the requester's; his act is the approval).
fn owner_actions(st: &AppState, words: &[String], now: DateTime<Utc>) -> Vec<Evidence> {
    let since = now - chrono::Duration::days(30);
    let mut out = Vec::new();
    if let Ok((items, _)) = st.timeline.list(None, 200) {
        for raw in &items {
            let Ok(item) = serde_json::from_str::<TimelineItem>(&raw.raw) else {
                continue;
            };
            let receipt = item.meta.as_ref().is_some_and(|m| m.kind.as_deref() == Some("secret_receipt"));
            if receipt && overlap(words, &item.text) >= 1 {
                out.push(Evidence {
                    tier: 2,
                    kind: "owner_action".into(),
                    text: clip(&format!("[{}] Giuseppe saved a secret himself: {}", day(&item.ts), item.text), EVIDENCE_CHARS),
                });
            }
        }
    }
    let audit = std::env::var("LUCINA_ROOT_AUDIT_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| st.cfg.data_dir.join("root-exec-audit.jsonl"));
    if let Ok(text) = std::fs::read_to_string(audit) {
        for v in text.lines().rev().filter_map(|l| serde_json::from_str::<Value>(l).ok()).take(400) {
            let event = v["event"].as_str().unwrap_or("");
            let ts = v["ts"].as_str().unwrap_or("");
            let recent = DateTime::parse_from_rfc3339(ts).is_ok_and(|t| t.with_timezone(&Utc) >= since);
            let reason = v["reason"].as_str().unwrap_or("");
            if matches!(event, "approved" | "denied") && recent && overlap(words, reason) >= 2 {
                out.push(Evidence {
                    tier: 2,
                    kind: "owner_action".into(),
                    text: clip(&format!("[{}] Giuseppe {event} root steps on his iPhone: {reason}", day(ts)), EVIDENCE_CHARS),
                });
            }
        }
    }
    out.truncate(4);
    out
}

/// Earlier decisions in the ops record (sessions', Lucina's and his own).
async fn ops_decisions(st: &AppState, r: &Request, words: &[String]) -> Vec<Evidence> {
    let Some(ops) = st.ops.clone() else {
        return Vec::new();
    };
    let q = match &r.topic {
        Some(t) => format!("{t} {}", words.iter().take(6).cloned().collect::<Vec<_>>().join(" ")),
        None => words.iter().take(8).cloned().collect::<Vec<_>>().join(" "),
    };
    let query = lucina_ops::SearchQuery {
        q: clip(&q, 400),
        kind: Some(lucina_ops::Kind::Decision),
        limit: Some(4),
        ..Default::default()
    };
    match tokio::time::timeout(MEMORY_TIMEOUT, ops.search(query)).await {
        Ok(Ok(found)) => found
            .items
            .into_iter()
            .map(|i| {
                let (tier, kind) = ops_tier(&i.source, i.decided_by);
                Evidence {
                    tier,
                    kind: kind.into(),
                    text: clip(
                        &format!("[{} {}, recorded by {}] {}: {}", day(&i.at), i.project, i.source, i.title, i.body),
                        EVIDENCE_CHARS,
                    ),
                }
            })
            .collect(),
        Ok(Err(e)) => {
            tracing::debug!(error = %e, "decision: ops search failed");
            Vec::new()
        }
        Err(_) => Vec::new(),
    }
}

/// A settled decision goes to the ops record; its id becomes the decision's `record_id`.
async fn record_ops(st: &AppState, d: &mut Decision) {
    let Some(ops) = st.ops.clone() else {
        return;
    };
    if d.status != "decided" {
        return;
    }
    let project = d
        .topic
        .as_deref()
        .map(lucina_intent::mcp::note_source)
        .filter(|p| p != "lucina" && p.len() <= 48)
        .unwrap_or_else(|| "lucina".into());
    let by = if d.decided_by.as_deref() == Some("owner") {
        lucina_ops::DecidedBy::Owner
    } else {
        lucina_ops::DecidedBy::Lucina
    };
    let answer = d.chosen_option.clone().unwrap_or_else(|| d.reason.clone());
    let mut new = lucina_ops::NewRecord::new(
        lucina_ops::Kind::Decision,
        &project,
        &clip(&format!("{} → {answer}", d.question), 200),
    );
    new.body = format!(
        "Asked by {} ({}). Options: {}. Recommended: {}. Impact {}, {}.\n\nAnswer: {answer}. {}",
        d.who(),
        d.decision_id,
        d.options.join(" / "),
        d.recommendation.as_deref().unwrap_or("none"),
        d.impact,
        if d.reversible { "reversible" } else { "irreversible" },
        d.reason
    );
    new.decided_by = Some(by);
    new.question = Some(clip(&d.question, 2000));
    new.asked_by = Some(d.who());
    new.dedup_key = Some(format!("{}:{}", d.decision_id, d.decided_by.as_deref().unwrap_or("lucina")));
    match ops.record(new, "lucina").await {
        Ok(r) => d.record_id = r.id,
        Err(e) => tracing::warn!(error = %e, decision = %d.decision_id, "decision: ops record failed (kept in the local log)"),
    }
}

fn earlier(d: &Decision) -> Evidence {
    let who = d.decided_by.as_deref().unwrap_or("?");
    let who = if who == "owner" { "Giuseppe" } else { "Lucina" };
    Evidence {
        tier: if who == "Giuseppe" { 1 } else { 3 },
        kind: if who == "Giuseppe" {
            "owner_decision"
        } else {
            "lucina_decision"
        }
        .into(),
        text: clip(
            &format!(
                "[{}] {who} decided \"{}\" → \"{}\" ({})",
                day(d.decided_at.as_deref().unwrap_or(&d.created_at)),
                d.question,
                d.chosen_option.as_deref().unwrap_or("own words"),
                d.reason
            ),
            EVIDENCE_CHARS,
        ),
    }
}

// ---------------------------------------------------------------- deciding

/// DeepSeek's reading.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelView {
    /// 0-based option, `None` = Giuseppe must decide.
    pub choice: Option<usize>,
    pub confidence: f64,
    pub conflict: bool,
    /// The tier it says it relied on (checked by [`valid_basis`]).
    pub basis: String,
    pub reason: String,
}

/// Jev's reading.
#[derive(Debug, Clone, PartialEq)]
pub struct JevView {
    pub choice: Option<usize>,
    pub probability: f64,
    pub conflict: f64,
}

/// What Lucina does with a request.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Decide {
        index: usize,
        confidence: f64,
        reason: String,
        basis: &'static str,
    },
    Ask {
        why: Vec<String>,
        lean: Option<usize>,
        confidence: Option<f64>,
    },
}

/// The policy, without I/O: hard rules first, then the two readings.
/// `tiers`: the tiers the evidence has (a reason may only rest on one of them, never on tier 4).
pub fn settle(
    r: &Request,
    owner_only: bool,
    model: Option<&ModelView>,
    jev: Option<&JevView>,
    tiers: &[u8],
) -> Verdict {
    let mut why: Vec<String> = Vec::new();
    if owner_only {
        why.push("only Giuseppe can decide this (money, credentials, terms or messages in his name)".into());
    }
    if r.impact == Impact::High {
        why.push("the impact is high".into());
    }
    if !r.reversible {
        why.push("it can't be undone".into());
    }
    let (choice, confidence) = match (model, jev) {
        (Some(m), Some(j)) => match (m.choice, j.choice) {
            // Two independent checks agreeing reinforce each other.
            (Some(a), Some(b)) if a == b => (Some(a), 1.0 - (1.0 - m.confidence) * (1.0 - j.probability)),
            (Some(_), Some(_)) => {
                why.push("my two checks disagree on the option".into());
                (None, 0.0)
            }
            _ => {
                why.push("the evidence doesn't settle it".into());
                (m.choice.or(j.choice), 0.0)
            }
        },
        (Some(m), None) => (m.choice, m.confidence * 0.9),
        (None, Some(j)) => (j.choice, j.probability * 0.9),
        (None, None) if owner_only => (None, 0.0),
        (None, None) => {
            why.push("I couldn't weigh it right now".into());
            (None, 0.0)
        }
    };
    if !owner_only
        && choice.is_none()
        && !why.iter().any(|w| w.contains("checks disagree") || w.contains("settle") || w.contains("weigh"))
    {
        why.push("the evidence doesn't settle it".into());
    }
    if model.is_some_and(|m| m.conflict) || jev.is_some_and(|j| j.conflict >= CONFLICT_MIN) {
        why.push("something Giuseppe said or decided earlier goes against the recommendation".into());
    }
    let min = if r.impact == Impact::Low {
        MIN_CONFIDENCE_LOW
    } else {
        MIN_CONFIDENCE_MEDIUM
    };
    if why.is_empty() && confidence < min {
        why.push(format!("I'm not sure enough ({confidence:.2})"));
    }
    match choice {
        Some(index) if why.is_empty() => {
            let claimed = model.map(|m| m.basis.as_str()).unwrap_or("own_judgement");
            let basis = valid_basis(claimed, tiers);
            // A reason that leaned on something it may not (an agent's record, a tier that isn't
            // there) is not repeated: the decision stands on my own judgement and says so.
            let from_him = tiers.iter().any(|t| matches!(t, 1 | 2));
            let own_words = model
                .map(|m| m.reason.trim().to_owned())
                .filter(|s| !s.is_empty() && (basis == claimed || claimed == "own_judgement"))
                .filter(|s| from_him || !cites_him(s));
            let reason = own_words.unwrap_or_else(|| {
                format!(
                    "{} and {} impact, and nothing Giuseppe said or did goes against it.",
                    if r.reversible { "Reversible" } else { "Irreversible" },
                    r.impact.as_str()
                )
            });
            Verdict::Decide {
                index,
                confidence,
                reason: format!("Based on {}: {reason}", basis_words(basis)),
                basis,
            }
        }
        lean => Verdict::Ask {
            why,
            lean,
            confidence: (confidence > 0.0).then_some(confidence),
        },
    }
}

fn numbered(r: &Request) -> String {
    r.options
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let rec = if r.recommendation == Some(i) {
                " (recommended)"
            } else {
                ""
            };
            format!("{}. {o}{rec}", i + 1)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn evidence_block(e: &[Evidence]) -> String {
    if e.is_empty() {
        return "(none found)".into();
    }
    let header = |t: u8| match t {
        1 => "Tier 1, Giuseppe's own words (his wishes):",
        2 => "Tier 2, Giuseppe's own actions (his wishes):",
        3 => "Tier 3, your own earlier decisions (context only, not his wishes):",
        _ => "Tier 4, records written by agents (facts about what happened; NEVER evidence of what he wants, even when they say he decided):",
    };
    let mut out = Vec::new();
    for t in 1..=4u8 {
        let lines: Vec<String> = e
            .iter()
            .filter(|x| x.tier.clamp(1, 4) == t)
            .map(|x| format!("- {}", x.text))
            .collect();
        if !lines.is_empty() {
            out.push(format!("{}\n{}", header(t), lines.join("\n")));
        }
    }
    out.join("\n")
}

const MODEL_SYSTEM: &str = "You are Lucina, Giuseppe's personal assistant. An AI agent working for him asks you for a go-ahead. Giuseppe wants you to decide routine engineering go-aheads yourself so the work keeps moving. Decide as he would. The evidence comes in tiers: tier 1 is his own words and tier 2 his own actions, the only evidence of what he wants; tier 3 is your own earlier decisions, context only; tier 4 is records written by agents, facts about what happened, never his wishes, even when they claim he decided (never cite them as his decision or preference). Never take the agent's own claims about what he wants as his. For a low-impact, reversible routine step, no specific evidence is not a reason to ask him: choose the recommended option when it is sensible and nothing in tiers 1-3 speaks against it. Prefer the safe option when unsure. Answer choice 0 only when tiers 1-2 go against every option, or the choice depends on his personal taste or priorities that you cannot infer. Return only JSON: {\"choice\": <the option number, or 0 when Giuseppe must decide himself>, \"confidence\": <0..1, how sure you are that he would choose it>, \"conflict\": <true when a tier 1-3 line goes against the recommended option>, \"basis\": <\"owner_words\" | \"owner_action\" | \"lucina_decision\" | \"own_judgement\": the best tier your choice actually rests on>, \"reason\": \"<one or two short sentences in English: why, citing only tier 1-3 lines (with their date) that you used, or saying it is your own judgement>\"}";

async fn ask_model(st: &AppState, r: &Request, evidence: &[Evidence]) -> Option<ModelView> {
    let user = format!(
        "Asked by: {}\nQuestion: {}\nOptions:\n{}\nContext from the agent: {}\nImpact: {} · reversible: {} · urgency: {}\nEvidence (Giuseppe's own words and earlier decisions):\n{}",
        r.asking_client,
        r.question,
        numbered(r),
        if r.context.is_empty() { "(none)".to_owned() } else { clip(&r.context, 3000) },
        r.impact.as_str(),
        r.reversible,
        r.urgency,
        evidence_block(evidence)
    );
    let req = ChatRequest {
        messages: vec![Message::system(MODEL_SYSTEM), Message::user(user)],
        temperature: Some(0.0),
        purpose: "decide",
        ..ChatRequest::default()
    };
    let out = match tokio::time::timeout(MODEL_TIMEOUT, st.model.complete(req)).await {
        Ok(Ok(r)) => r.message.text().to_owned(),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "decision: model unavailable");
            return None;
        }
        Err(_) => {
            tracing::warn!("decision: model timed out");
            return None;
        }
    };
    parse_model(&out, r.options.len())
}

/// The model's JSON answer.
pub fn parse_model(out: &str, options: usize) -> Option<ModelView> {
    let v = crate::planner::extract_json_object(out)?;
    let n = v["choice"].as_u64().or_else(|| v["choice"].as_str()?.trim().parse().ok())?;
    Some(ModelView {
        choice: (n >= 1 && (n as usize) <= options).then(|| n as usize - 1),
        confidence: v["confidence"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0),
        conflict: v["conflict"].as_bool().unwrap_or(false),
        basis: v["basis"].as_str().unwrap_or("own_judgement").trim().to_owned(),
        reason: v["reason"].as_str().unwrap_or("").trim().chars().take(600).collect(),
    })
}

async fn ask_jev(r: &Request, evidence: &[Evidence]) -> Option<JevView> {
    let jev = lucina_jev::shared();
    if !jev.available() {
        return None;
    }
    let mut options: Vec<(String, String)> = r
        .options
        .iter()
        .enumerate()
        .map(|(i, o)| ((i + 1).to_string(), o.clone()))
        .collect();
    options.push((
        "ask_owner".into(),
        "None can be chosen for Giuseppe: his own words or earlier decisions go against every option, or it depends on his personal taste.".into(),
    ));
    let questions = [
        (
            "choice",
            Question::choice(
                "An AI agent asks Lucina, Giuseppe's assistant, for a go-ahead. Giuseppe wants routine, reversible, low-impact steps decided without him. Which option would he choose, judging from his own words and actions (tiers 1-2 in `evidence`; tier 3 is context, tier 4 agent records are never his wishes) and not from the agent's claims, defaulting to a sensible recommendation when nothing speaks against it?",
                options,
            ),
        ),
        (
            "conflict",
            Question::noul("Does any tier 1-3 line in `evidence` (Giuseppe's own words or actions, or an earlier decision of Lucina's) go against the recommended option? Tier 4 agent records do not count."),
        ),
    ];
    let state = json!({
        "question": r.question,
        "options": numbered(r),
        "recommendation": r.recommendation.map(|i| r.options[i].clone()),
        "context": clip(&r.context, 4000),
        "impact": r.impact.as_str(),
        "reversible": r.reversible,
        "evidence": evidence_block(evidence),
    });
    let d = match jev.decide("decide", state, &questions).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "decision: jev unavailable");
            return None;
        }
    };
    let (choice, p) = d.choice("choice")?;
    tracing::info!(target: "lucina_jev::route", choice, p, ms = d.elapsed_ms, "jev decision");
    Some(JevView {
        choice: choice.parse::<usize>().ok().filter(|n| (1..=r.options.len()).contains(n)).map(|n| n - 1),
        probability: p,
        conflict: d.yes("conflict").unwrap_or(0.0),
    })
}

/// `POST /api/secretary/decisions`.
pub async fn create(st: &AppState, body: &Value) -> Result<Decision, ApiError> {
    let r = parse_request(body).map_err(ApiError::bad_request)?;
    let now = Utc::now();
    let store = store(&st.cfg.data_dir);
    let only = owner_only(&r);
    // Evidence: earlier decisions, his own recent messages, his long-term memory.
    let words = keywords(&format!(
        "{} {} {} {}",
        r.question,
        r.options.join(" "),
        r.topic.as_deref().unwrap_or(""),
        clip(&r.context, 500)
    ));
    let mut evidence: Vec<Evidence> = store.similar(&r, 3).iter().map(earlier).collect();
    evidence.extend(ops_decisions(st, &r, &words).await);
    evidence.extend(owner_messages(st, &words, now));
    let query = clip(&format!("{} {}", r.question, r.options.join(" / ")), 400);
    evidence.extend(owner_memory(st, &r, &query).await);
    evidence.extend(owner_actions(st, &words, now));
    // Best tier first: his words and actions lead, agents' records come last.
    evidence.sort_by_key(|e| e.tier);
    let tiers: Vec<u8> = evidence.iter().map(|e| e.tier).collect();
    let (model, jev) = if only {
        (None, None)
    } else {
        tokio::join!(ask_model(st, &r, &evidence), ask_jev(&r, &evidence))
    };
    let verdict = settle(&r, only, model.as_ref(), jev.as_ref(), &tiers);
    tracing::info!(client = %r.asking_client, impact = r.impact.as_str(), reversible = r.reversible,
        owner_only = only, model = ?model.as_ref().map(|m| (m.choice, m.confidence, m.conflict)),
        jev = ?jev.as_ref().map(|j| (j.choice, j.probability, j.conflict)), verdict = ?verdict, "decision");
    let id = format!("dc-{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    let mut d = Decision {
        decision_id: id.clone(),
        record_id: id,
        status: "pending".into(),
        decision: None,
        chosen_option: None,
        chosen_index: None,
        reason: String::new(),
        decided_by: None,
        confidence: None,
        asked_owner: false,
        owner_only: only,
        question: r.question.clone(),
        options: r.options.clone(),
        recommendation: r.recommendation.map(|i| r.options[i].clone()),
        context: r.context.clone(),
        impact: r.impact.as_str().into(),
        reversible: r.reversible,
        urgency: r.urgency.clone(),
        topic: r.topic.clone(),
        asking_client: r.asking_client.clone(),
        asking_session: r.asking_session.clone(),
        created_at: iso_ms(now),
        decided_at: None,
        expires_at: None,
        evidence,
        basis: None,
        owner_item_id: None,
    };
    match verdict {
        Verdict::Decide {
            index,
            confidence,
            reason,
            basis,
        } => d.choose(index, "lucina", (confidence * 100.0).round() / 100.0, reason, basis),
        Verdict::Ask {
            why,
            lean,
            confidence,
        } => {
            d.asked_owner = true;
            d.confidence = confidence.map(|c| (c * 100.0).round() / 100.0);
            d.expires_at = Some(iso_ms(now + PENDING_TTL));
            let lean = lean
                .map(|i| format!(" I lean to \"{}\".", d.options[i]))
                .unwrap_or_default();
            d.reason = format!("I asked Giuseppe: {}.{lean}", why.join("; "));
            d.owner_item_id = ask_owner(st, &d).await;
        }
    }
    record_ops(st, &mut d).await;
    store
        .put(&d)
        .map_err(|e| ApiError::new(500, "internal_error", format!("decisions log: {e}")))?;
    report_to_asker(st, &d, true);
    Ok(d)
}

// ---------------------------------------------------------------- the timeline

fn agent_meta(d: &Decision) -> Meta {
    Meta {
        via: Some("mcp".into()),
        caller: Some(d.asking_client.clone()),
        kind: Some("decision".into()),
        decision_id: Some(d.decision_id.clone()),
        ..Meta::default()
    }
}

/// What the agent's MCP conversation shows: the request (first time) and where it stands.
fn report_to_asker(st: &AppState, d: &Decision, with_request: bool) {
    let request = with_request.then(|| {
        let rec = d
            .recommendation
            .as_deref()
            .map(|r| format!(" Recommended: \"{r}\"."))
            .unwrap_or_default();
        st.timeline.record(NewItem {
            channel: "web".into(),
            role: "user".into(),
            text: clip(
                &format!(
                    "Decision request {}: {}\nOptions: {}.{rec} Impact {}, {}.",
                    d.decision_id,
                    d.question,
                    d.options.join(" / "),
                    d.impact,
                    if d.reversible { "reversible" } else { "irreversible" }
                ),
                2000,
            ),
            meta: Some(agent_meta(d)),
            ..Default::default()
        })
    });
    st.timeline.record(NewItem {
        channel: "web".into(),
        role: "lucina".into(),
        text: summary(d),
        reply_to: request.flatten().map(|i| i.id),
        meta: Some(agent_meta(d)),
        ..Default::default()
    });
}

/// One line for the agent's conversation.
pub fn summary(d: &Decision) -> String {
    let by = if d.decided_by.as_deref() == Some("owner") {
        "Giuseppe decided"
    } else {
        "I decided"
    };
    match (d.status.as_str(), d.decision.as_deref()) {
        ("decided", Some("owner_reply")) => format!("{}: Giuseppe answered: \"{}\"", d.decision_id, d.reason),
        ("decided", Some(kind)) => format!(
            "{}: {by}: {} \"{}\". {}",
            d.decision_id,
            if kind == "go" { "GO with" } else { "CHANGE to" },
            d.chosen_option.as_deref().unwrap_or(""),
            d.reason
        ),
        ("expired", _) => format!("{}: expired, Giuseppe did not answer.", d.decision_id),
        ("withdrawn", _) => format!("{}: withdrawn, no answer needed. {}", d.decision_id, d.reason),
        _ => format!("{}: pending. {}", d.decision_id, d.reason),
    }
}

/// Ask Giuseppe in his own chat (buttons) and push it to his phone. Returns the item id.
async fn ask_owner(st: &AppState, d: &Decision) -> Option<String> {
    let mut flags = vec![format!("impact {}", d.impact)];
    if !d.reversible {
        flags.push("can't be undone".into());
    }
    if d.urgency == "high" {
        flags.push("urgent".into());
    }
    let context = if d.context.is_empty() {
        String::new()
    } else {
        format!("\n{}", clip(&d.context, 400))
    };
    let options = d
        .options
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let rec = if d.recommendation.as_deref() == Some(o.as_str()) {
                " (recommended)"
            } else {
                ""
            };
            format!("{}. {o}{rec}", i + 1)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = format!(
        "{} needs a decision: {}{context}\n\n{options}\n\n{} · {}\nTap an option, or answer \"Decision {}: <your words>\".",
        d.who(),
        d.question,
        flags.join(" · "),
        d.reason,
        d.decision_id
    );
    let max_buttons = if d.options.len() > 4 { 3 } else { 4 };
    let mut actions: Vec<Value> = d
        .options
        .iter()
        .take(max_buttons)
        .enumerate()
        .map(|(i, o)| {
            json!({
                "id": format!("{}-{}", d.decision_id, i + 1),
                "label": clip(&format!("{}. {o}", i + 1), 40),
                "kind": "reply",
                "payload": {"text": format!("Decision {}: {}. {}", d.decision_id, i + 1, o)},
            })
        })
        .collect();
    if d.options.len() > max_buttons {
        actions.push(json!({
            "id": format!("{}-other", d.decision_id),
            "label": "Other answer",
            "kind": "reply",
            "payload": {"text": format!("Decision {}: ", d.decision_id), "prefill": true},
        }));
    }
    let rich = lucina_store::rich::RichMessage {
        format: lucina_store::rich::FORMAT.into(),
        bubbles: vec![text.clone()],
        lang: Some("en".into()),
        blocks: Vec::new(),
        actions,
    };
    let meta = Meta {
        kind: Some("decision".into()),
        decision_id: Some(d.decision_id.clone()),
        ..Meta::default()
    }
    .merge(Meta::rich(&rich));
    let item = st.timeline.record(NewItem {
        channel: "web".into(),
        role: "lucina".into(),
        text: text.clone(),
        meta: Some(meta),
        ..Default::default()
    });
    let id = item.map(|i| i.id);
    let mut push = json!({
        "category": "lucina_message",
        "title": format!("Decision needed ({})", d.asking_client),
        "body": clip(&d.question, 300),
        "route": "/",
        "channel": "web",
    });
    if let Some(id) = &id {
        // The timeline ingest pushes the same item: one dedup key for both.
        push["timelineId"] = json!(id);
        push["dedupKey"] = json!(format!("tl:{id}"));
    }
    let r = crate::bridges::notify(&st.secretary, &push).await;
    if !r.ok {
        tracing::warn!(status = r.status, code = %r.code, "decision push failed (the timeline item still notifies)");
    }
    id
}

// ---------------------------------------------------------------- his answer

/// Lucina's reply to the owner's answer.
pub struct OwnerReply {
    pub reply: String,
    pub action_result: String,
}

/// `Decision dc-…: 2. Deploy now` / `decisione dc-…: aspetta domani`.
pub fn parse_answer(text: &str) -> Option<(String, String)> {
    let c = lucina_intent::re!(
        r"^\s*(?:decision[ei]?|decisione|beslutning)\s+(dc-[0-9a-f]{12})\b\s*[:.\-–—→>]*\s*([\s\S]*)$",
        "i"
    )
    .exec(text)?;
    Some((
        lucina_intent::jsre::group(&c, 1).to_lowercase(),
        lucina_intent::jsre::group(&c, 2).trim().to_owned(),
    ))
}

/// Which option an answer names: `2`, `2. Deploy now`, `Deploy now`.
pub fn answer_option(options: &[String], answer: &str) -> Option<usize> {
    let a = answer.trim();
    let lead: String = a.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !lead.is_empty() {
        let rest = a[lead.len()..].trim_start_matches(['.', ')', ':']).trim();
        let n: usize = lead.parse().ok()?;
        if (1..=options.len()).contains(&n) && (rest.is_empty() || rest.eq_ignore_ascii_case(&options[n - 1])) {
            return Some(n - 1);
        }
        return None;
    }
    option_index(options, a)
}

/// The owner's own turn: resolves a pending decision. `None`: not a decision answer.
pub async fn owner_answer(st: &AppState, text: &str) -> Option<OwnerReply> {
    let (id, answer) = parse_answer(text)?;
    let store = store(&st.cfg.data_dir);
    let Some(mut d) = store.get(&id, Utc::now()) else {
        return Some(OwnerReply {
            reply: format!("I don't have a decision {id}."),
            action_result: format!("[decision {id}: unknown]"),
        });
    };
    if d.status != "pending" {
        return Some(OwnerReply {
            reply: format!("{id} is already {}: {}", d.status, summary(&d)),
            action_result: format!("[decision {id}: already {}]", d.status),
        });
    }
    if answer.is_empty() {
        return Some(OwnerReply {
            reply: format!("Which option for {id}? Answer with its number, or in your own words."),
            action_result: format!("[decision {id}: no answer yet]"),
        });
    }
    match answer_option(&d.options, &answer) {
        Some(i) => d.choose(i, "owner", 1.0, "Giuseppe chose it himself.".into(), "owner_words"),
        None => {
            d.status = "decided".into();
            d.decision = Some("owner_reply".into());
            d.basis = Some("owner_words".into());
            d.decided_by = Some("owner".into());
            d.confidence = Some(1.0);
            d.reason = clip(&answer, 1000);
            d.decided_at = Some(now_iso());
            d.expires_at = None;
        }
    }
    record_ops(st, &mut d).await;
    if let Err(e) = store.put(&d) {
        tracing::error!(error = %e, "decisions log write failed");
        return Some(OwnerReply {
            reply: format!("I couldn't record your answer to {id} ({e}). Try again in a moment."),
            action_result: format!("[decision {id}: not recorded]"),
        });
    }
    report_to_asker(st, &d, false);
    let what = match &d.chosen_option {
        Some(o) => format!("\"{o}\""),
        None => format!("your answer (\"{}\")", clip(&d.reason, 120)),
    };
    Some(OwnerReply {
        reply: format!("Ok, {} gets {what}.", d.who()),
        action_result: format!(
            "[decision {id}] {} asked: {} → Giuseppe: {}",
            d.who(),
            d.question,
            d.chosen_option.as_deref().unwrap_or(&d.reason)
        ),
    })
}

/// `DELETE /api/secretary/decisions/{id}?reason=`: the asker takes back a question still waiting for
/// the owner (asked by mistake, settled elsewhere). His buttons then answer that it was withdrawn.
pub fn withdraw(st: &AppState, id: &str, reason: &str) -> Result<Value, ApiError> {
    let store = store(&st.cfg.data_dir);
    let mut d = valid_id(id)
        .then(|| store.get(id, Utc::now()))
        .flatten()
        .ok_or_else(|| ApiError::new(404, "decision_not_found", "No such decision"))?;
    if d.status != "pending" {
        return Err(ApiError::new(409, "not_pending", format!("{id} is {}, not pending", d.status)));
    }
    let reason = clip(reason.trim(), 500);
    d.status = "withdrawn".into();
    d.reason = if reason.is_empty() {
        "Withdrawn by the asker.".into()
    } else {
        format!("Withdrawn by the asker: {reason}")
    };
    d.expires_at = None;
    d.decided_at = Some(now_iso());
    store
        .put(&d)
        .map_err(|e| ApiError::new(500, "internal_error", format!("decisions log: {e}")))?;
    report_to_asker(st, &d, false);
    Ok(d.public())
}

/// "What's the state of dc-…?": the decision's status from the log, on any turn (read only).
pub fn status_answer(st: &AppState, text: &str) -> Option<String> {
    let c = lucina_intent::re!(r"\b(dc-[0-9a-f]{12})\b", "i").exec(text)?;
    let id = lucina_intent::jsre::group(&c, 1).to_lowercase();
    Some(match store(&st.cfg.data_dir).get(&id, Utc::now()) {
        Some(d) => summary(&d),
        None => format!("I don't have a decision {id}."),
    })
}

// ---------------------------------------------------------------- API

pub fn valid_id(id: &str) -> bool {
    id.len() == 15
        && id.starts_with("dc-")
        && id[3..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `GET /api/secretary/decisions/{id}`.
pub fn get(st: &AppState, id: &str) -> Result<Value, ApiError> {
    if !valid_id(id) {
        return Err(ApiError::new(404, "decision_not_found", "No such decision"));
    }
    store(&st.cfg.data_dir)
        .get(id, Utc::now())
        .map(|d| d.public())
        .ok_or_else(|| ApiError::new(404, "decision_not_found", "No such decision"))
}

/// `GET /api/secretary/decisions?status=&limit=`.
pub fn list(st: &AppState, status: Option<&str>, limit: Option<&str>) -> Result<Value, ApiError> {
    let status = match status {
        None | Some("") => None,
        Some(s @ ("pending" | "decided" | "expired" | "withdrawn")) => Some(s),
        Some(_) => return Err(ApiError::bad_request("status must be pending, decided, expired or withdrawn")),
    };
    let limit = match limit {
        None | Some("") => 20,
        Some(l) => l
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=100).contains(n))
            .ok_or_else(|| ApiError::bad_request("limit must be 1-100"))?,
    };
    let items: Vec<Value> = store(&st.cfg.data_dir)
        .list(status, limit, Utc::now())
        .iter()
        .map(Decision::public)
        .collect();
    Ok(json!({ "items": items }))
}

/// `lucina-rs decide …` (main.rs): a client of the running server.
pub mod cli {
    use std::time::Duration;

    use serde_json::Value;

    /// The unit's env files, in the order systemd reads them (the last value wins).
    const ENV_FILES: &[&str] = &[".config/lucina-rs/lucina.env", ".config/lucina-rs/mode.env"];

    /// `KEY=value` from the environment, else from the unit's env files.
    fn setting(key: &str) -> Option<String> {
        if let Ok(v) = std::env::var(key) {
            return Some(v);
        }
        let home = std::env::var("HOME").ok()?;
        let mut found = None;
        for f in ENV_FILES {
            let Ok(text) = std::fs::read_to_string(std::path::Path::new(&home).join(f)) else {
                continue;
            };
            for line in text.lines() {
                if let Some(v) = line.trim().strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
                    found = Some(v.trim().trim_matches('"').to_owned());
                }
            }
        }
        found
    }

    pub struct Client {
        http: reqwest::Client,
        base: String,
        token: String,
    }

    impl Client {
        pub fn from_env() -> anyhow::Result<Self> {
            let token = setting("SECRETARY_API_TOKEN_FILE")
                .and_then(|p| std::fs::read_to_string(p).ok())
                .or_else(|| setting("SECRETARY_API_TOKEN"))
                .map(|t| t.trim().to_owned())
                .filter(|t| !t.is_empty())
                .ok_or_else(|| anyhow::anyhow!("no SECRETARY_API_TOKEN in the environment or ~/.config/lucina-rs/*.env"))?;
            let bind = setting("LUCINA_DECIDE_URL").unwrap_or_else(|| {
                let first = setting("LUCINA_BIND").unwrap_or_default();
                let first = first.split(',').next().unwrap_or("").trim().to_owned();
                format!("http://{}", if first.is_empty() { "127.0.0.1:8790" } else { &first })
            });
            Ok(Self {
                http: reqwest::Client::builder().timeout(Duration::from_secs(90)).build()?,
                base: format!("{}/api/secretary/decisions", bind.trim_end_matches('/')),
                token,
            })
        }

        async fn send(&self, rb: reqwest::RequestBuilder) -> anyhow::Result<Value> {
            let r = rb.bearer_auth(&self.token).send().await?;
            let status = r.status();
            let v: Value = r.json().await.unwrap_or(Value::Null);
            if !status.is_success() {
                anyhow::bail!(
                    "Lucina answered {status}: {}",
                    v["message"].as_str().or(v["error"].as_str()).unwrap_or("")
                );
            }
            Ok(v.get("data").cloned().unwrap_or(v))
        }

        pub async fn create(&self, body: &Value) -> anyhow::Result<Value> {
            self.send(self.http.post(&self.base).json(body)).await
        }

        pub async fn get(&self, id: &str) -> anyhow::Result<Value> {
            if !super::valid_id(id) {
                anyhow::bail!("a decision id is dc- and 12 hex digits");
            }
            self.send(self.http.get(format!("{}/{id}", self.base))).await
        }

        pub async fn withdraw(&self, id: &str, reason: &str) -> anyhow::Result<Value> {
            if !super::valid_id(id) {
                anyhow::bail!("a decision id is dc- and 12 hex digits");
            }
            self.send(self.http.delete(format!("{}/{id}", self.base)).query(&[("reason", reason)]))
                .await
        }

        pub async fn list(&self, status: Option<&str>, limit: u32) -> anyhow::Result<Value> {
            let mut q = vec![("limit", limit.to_string())];
            if let Some(s) = status {
                q.push(("status", s.to_owned()));
            }
            self.send(self.http.get(&self.base).query(&q)).await
        }

        /// Poll every 3 s while it is pending, up to `seconds` (at most 600).
        pub async fn wait(&self, mut d: Value, seconds: u64) -> anyhow::Result<Value> {
            let until = std::time::Instant::now() + Duration::from_secs(seconds.min(600));
            while d["status"] == "pending" && std::time::Instant::now() + Duration::from_secs(3) <= until {
                tokio::time::sleep(Duration::from_secs(3)).await;
                let id = d["decision_id"].as_str().unwrap_or("").to_owned();
                d = self.get(&id).await?;
            }
            Ok(d)
        }
    }

    /// The one line for a human (stderr): the same words as the MCP tool.
    pub fn line(d: &Value) -> String {
        let s = |k: &str| d[k].as_str().unwrap_or("");
        let id = s("decision_id");
        match (s("status"), s("decision")) {
            ("pending", _) => format!("Pending: Lucina asked the owner on his phone (decision {id}). Do not act on this yet; check with: lucina-rs decide status {id} --wait 60"),
            ("expired", _) => format!("Expired: the owner did not answer within 24 h (decision {id})."),
            ("withdrawn", _) => format!("Withdrawn (decision {id}): {}", s("reason")),
            (_, "owner_reply") => format!("The owner answered: \"{}\"  Decision {id}", s("reason")),
            (_, kind) => format!(
                "{} decided: {} \"{}\" (confidence {:.2}). Reason: {}  Decision {id}",
                if s("decided_by") == "owner" { "The owner" } else { "Lucina" },
                if kind == "change" { "CHANGE to" } else { "GO with" },
                s("chosen_option"),
                d["confidence"].as_f64().unwrap_or(0.0),
                s("reason")
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body() -> Value {
        json!({
            "question": "Deploy the gallery fix to p1 now?",
            "options": ["Deploy now", "Wait for tomorrow"],
            "recommendation": "Deploy now",
            "context": "Tests pass; one-line CSS change.",
            "reversible": true,
            "impact": "low",
            "asking_session": "coordinator",
            "asking_client": "claude-code",
        })
    }

    fn req() -> Request {
        parse_request(&body()).unwrap()
    }

    #[test]
    fn requests_are_validated_field_by_field() {
        let r = req();
        assert_eq!(r.recommendation, Some(0));
        assert_eq!(r.urgency, "normal");
        assert_eq!(r.asking_client, "claude-code");
        let bad = |patch: Value| {
            let mut b = body();
            for (k, v) in patch.as_object().unwrap() {
                b[k] = v.clone();
            }
            parse_request(&b).unwrap_err()
        };
        assert!(bad(json!({"options": ["only one"]})).contains("2-6"));
        assert!(bad(json!({"options": ["a", "A"]})).contains("repeats"));
        assert!(bad(json!({"recommendation": "something else"})).contains("one of the options"));
        assert!(bad(json!({"impact": "huge"})).contains("impact"));
        assert!(bad(json!({"reversible": null})).contains("reversible"));
        assert!(bad(json!({"question": ""})).contains("question"));
        assert!(bad(json!({"urgency": "now"})).contains("urgency"));
        let mut b = body();
        b["recommendation"] = json!("2");
        assert_eq!(parse_request(&b).unwrap().recommendation, Some(1));
    }

    #[test]
    fn owner_only_questions() {
        assert!(!owner_only(&req()));
        let mut b = body();
        b["question"] = json!("Should I buy the 2 TB plan for the backups?");
        assert!(owner_only(&parse_request(&b).unwrap()));
        b["question"] = json!("Accept the terms of the new Infisical licence?");
        assert!(owner_only(&parse_request(&b).unwrap()));
        b["question"] = json!("Send an email to Marianne about the CRAFT tool?");
        assert!(owner_only(&parse_request(&b).unwrap()));
        // Secret wiring is his (2026-10-28: the Paseo session's token-placement question).
        b["question"] = json!("Store the Hermes agents MCP token at lucina/prod instead of agents/prod?");
        assert!(owner_only(&parse_request(&b).unwrap()));
        // A review summary that mentions secrets is not a secret question (dc-d5c85cb22592).
        b["question"] = json!("Merge gvitolocs/pokoin PR #513 into main?");
        b["context"] = json!("Reviewed clean: tests pass, diffgate ok, 0 secret hits, mergeable.");
        assert!(!owner_only(&parse_request(&b).unwrap()));
        b["question"] = json!("Rotate the Pokobot DeepSeek API key now?");
        assert!(owner_only(&parse_request(&b).unwrap()));
        b["context"] = json!("Tests pass.");
        b["question"] = json!("Restart the payment-webhook worker on p1?");
        assert!(!owner_only(&parse_request(&b).unwrap()), "technical words are not a payment");
    }

    fn model(choice: Option<usize>, confidence: f64) -> ModelView {
        ModelView { choice, confidence, conflict: false, basis: "owner_words".into(), reason: "He said to ship small fixes.".into() }
    }

    fn jev(choice: Option<usize>, probability: f64) -> JevView {
        JevView { choice, probability, conflict: 0.1 }
    }

    #[test]
    fn low_impact_reversible_agreement_is_a_go() {
        match settle(&req(), false, Some(&model(Some(0), 0.9)), Some(&jev(Some(0), 0.8)), &[1]) {
            Verdict::Decide { index, confidence, reason, basis } => {
                assert_eq!(basis, "owner_words");
                assert!(reason.starts_with("Based on Giuseppe's own words: "), "{reason}");
                assert_eq!(index, 0);
                assert!((confidence - 0.98).abs() < 1e-9, "agreement reinforces: {confidence}");
                assert!(reason.contains("ship small fixes"));
            }
            v => panic!("{v:?}"),
        }
        // Only Jev answered (DeepSeek down): still a decision when it is sure enough.
        assert!(matches!(settle(&req(), false, None, Some(&jev(Some(0), 0.9)), &[]), Verdict::Decide { .. }));
    }

    #[test]
    fn she_asks_him_when_it_matters_or_is_unclear() {
        let ask = |r: &Request, only: bool, m: Option<&ModelView>, j: Option<&JevView>| match settle(r, only, m, j, &[1]) {
            Verdict::Ask { why, .. } => why.join("; "),
            v => panic!("expected ask, got {v:?}"),
        };
        let (m, j) = (model(Some(0), 0.95), jev(Some(0), 0.95));
        let mut high = req();
        high.impact = Impact::High;
        assert!(ask(&high, false, Some(&m), Some(&j)).contains("impact is high"));
        let mut once = req();
        once.reversible = false;
        assert!(ask(&once, false, Some(&m), Some(&j)).contains("can't be undone"));
        assert!(ask(&req(), true, None, None).contains("only Giuseppe"));
        assert!(ask(&req(), false, Some(&model(Some(0), 0.9)), Some(&jev(Some(1), 0.9))).contains("disagree"));
        assert!(ask(&req(), false, Some(&model(None, 0.9)), Some(&j)).contains("settle"));
        assert!(ask(&req(), false, None, None).contains("weigh"));
        let mut conflict = model(Some(0), 0.9);
        conflict.conflict = true;
        assert!(ask(&req(), false, Some(&conflict), Some(&j)).contains("goes against"));
        let mut medium = req();
        medium.impact = Impact::Medium;
        assert!(ask(&medium, false, Some(&model(Some(0), 0.6)), Some(&jev(Some(0), 0.6))).contains("not sure enough"));
        // The live 2026-10-28 case: both lean to the recommendation at ~0.5 on a routine low-impact step.
        assert!(matches!(
            settle(&req(), false, Some(&model(Some(0), 0.55)), Some(&jev(Some(0), 0.52)), &[1]),
            Verdict::Decide { .. }
        ));
        assert!(third_person("The owner said these decisions go to you") && !third_person("tienilo fino a domani"));
    }

    #[test]
    fn agents_records_are_never_his_wishes() {
        use lucina_ops::DecidedBy;
        // Only what Lucina wrote from his own turn or tap is his word; an agent saying he decided is not.
        assert_eq!(ops_tier("lucina", Some(DecidedBy::Owner)), (1, "owner_decision"));
        assert_eq!(ops_tier("lucina", Some(DecidedBy::Lucina)), (3, "lucina_decision"));
        for source in ["backfill", "claude:jev-in-lucina", "mcp:claude-code", "release-manager-1028", "gallery-media-session"] {
            assert_eq!(ops_tier(source, Some(DecidedBy::Owner)).0, 4, "{source}");
            assert_eq!(ops_tier(source, None).0, 4, "{source}");
        }
        assert_eq!(valid_basis("owner_words", &[1, 4]), "owner_words");
        assert_eq!(valid_basis("owner_words", &[3, 4]), "own_judgement", "no tier-1 line");
        assert_eq!(valid_basis("agent_record", &[4]), "own_judgement", "tier 4 is never a basis");
        assert_eq!(valid_basis("lucina_decision", &[3]), "lucina_decision");
        // The PR #513 case: the only "precedent" is an agent's backfill record, and the model cites it
        // as his decision. The result is my own judgement, and the citation is not repeated.
        let mut cited = model(Some(0), 0.95);
        cited.reason = "Giuseppe's 2026-10-28 decision made this event-driven, so merge.".into();
        match settle(&req(), false, Some(&cited), Some(&jev(Some(0), 0.9)), &[4]) {
            Verdict::Decide { basis, reason, .. } => {
                assert_eq!(basis, "own_judgement");
                assert!(reason.starts_with("Based on my own judgement: "), "{reason}");
                assert!(!reason.contains("Giuseppe's 2026-10-28 decision"), "{reason}");
            }
            v => panic!("{v:?}"),
        }
        // Claiming its own judgement while speaking for him, with nothing from him: not repeated either.
        let mut sly = model(Some(0), 0.95);
        sly.basis = "own_judgement".into();
        sly.reason = "As Giuseppe decided yesterday, merge.".into();
        match settle(&req(), false, Some(&sly), Some(&jev(Some(0), 0.9)), &[4]) {
            Verdict::Decide { reason, .. } => assert!(!reason.contains("As Giuseppe decided"), "{reason}"),
            v => panic!("{v:?}"),
        }
        let block = evidence_block(&[
            Evidence { tier: 4, kind: "agent_record".into(), text: "[2026-10-28 pokoin, recorded by backfill] x".into() },
            Evidence { tier: 1, kind: "owner_message".into(), text: "[2026-10-27 ios] Giuseppe: ship it".into() },
        ]);
        assert!(block.starts_with("Tier 1, Giuseppe's own words"), "{block}");
        assert!(block.contains("Tier 4, records written by agents (facts about what happened; NEVER evidence"));
    }

    #[test]
    fn model_answers_and_owner_answers_parse() {
        let m = parse_model("```json\n{\"choice\": 2, \"confidence\": 0.7, \"conflict\": false, \"reason\": \"x\"}\n```", 2).unwrap();
        assert_eq!(m.choice, Some(1));
        assert_eq!(parse_model("{\"choice\": 0, \"confidence\": 0.2}", 2).unwrap().choice, None);
        assert!(parse_model("not json", 2).is_none());
        assert_eq!(
            parse_answer("Decision dc-0123456789ab: 2. Wait for tomorrow"),
            Some(("dc-0123456789ab".into(), "2. Wait for tomorrow".into()))
        );
        assert_eq!(
            parse_answer("decisione dc-0123456789AB aspetta domani sera").map(|a| a.1),
            Some("aspetta domani sera".into())
        );
        assert!(parse_answer("I made a decision dc-0123456789ab yesterday").is_none());
        let opts = vec!["Deploy now".to_owned(), "Wait for tomorrow".to_owned()];
        assert_eq!(answer_option(&opts, "2. Wait for tomorrow"), Some(1));
        assert_eq!(answer_option(&opts, "1"), Some(0));
        assert_eq!(answer_option(&opts, "deploy now"), Some(0));
        assert_eq!(answer_option(&opts, "3"), None);
        assert_eq!(answer_option(&opts, "no, wait until Friday"), None);
        assert!(valid_id("dc-0123456789ab") && !valid_id("dc-0123") && !valid_id("../etc/passwd"));
    }

    #[test]
    fn cli_lines() {
        let d = json!({"decision_id": "dc-0123456789ab", "status": "decided", "decision": "go",
            "chosen_option": "merge", "confidence": 0.9, "reason": "Reviewed clean.", "decided_by": "lucina"});
        assert_eq!(cli::line(&d), "Lucina decided: GO with \"merge\" (confidence 0.90). Reason: Reviewed clean.  Decision dc-0123456789ab");
        let p = json!({"decision_id": "dc-0123456789ab", "status": "pending"});
        assert!(cli::line(&p).contains("lucina-rs decide status dc-0123456789ab"));
    }

    #[test]
    fn the_log_keeps_the_last_state_and_expires_unanswered_ones() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path());
        let now = Utc::now();
        let mut d = Decision {
            decision_id: "dc-0123456789ab".into(),
            record_id: "dc-0123456789ab".into(),
            status: "pending".into(),
            decision: None,
            chosen_option: None,
            chosen_index: None,
            reason: "I asked Giuseppe: the impact is high.".into(),
            decided_by: None,
            confidence: None,
            asked_owner: true,
            owner_only: false,
            question: "Deploy the gallery fix to p1 now?".into(),
            options: vec!["Deploy now".into(), "Wait".into()],
            recommendation: Some("Deploy now".into()),
            context: String::new(),
            impact: "high".into(),
            reversible: true,
            urgency: "normal".into(),
            topic: Some("gallery".into()),
            asking_client: "claude-code".into(),
            asking_session: None,
            created_at: iso_ms(now),
            decided_at: None,
            expires_at: Some(iso_ms(now + PENDING_TTL)),
            evidence: vec![],
            basis: None,
            owner_item_id: Some("x".into()),
        };
        s.put(&d).unwrap();
        d.choose(1, "owner", 1.0, "Giuseppe chose it himself.".into(), "owner_words");
        s.put(&d).unwrap();
        let reopened = Store::open(dir.path());
        let got = reopened.get("dc-0123456789ab", now).unwrap();
        assert_eq!(got.status, "decided");
        assert_eq!(got.decision.as_deref(), Some("change"));
        assert_eq!(got.chosen_index, Some(2));
        assert!(got.public().get("context").is_none() && got.public().get("owner_item_id").is_none());
        // Same topic → an earlier decision the next request sees.
        let mut r = req();
        r.topic = Some("gallery".into());
        assert_eq!(reopened.similar(&r, 3).len(), 1);
        let mut p = d.clone();
        p.decision_id = "dc-ba9876543210".into();
        p.status = "pending".into();
        p.expires_at = Some(iso_ms(now + PENDING_TTL));
        reopened.put(&p).unwrap();
        let later = now + PENDING_TTL + chrono::Duration::minutes(1);
        assert_eq!(reopened.get("dc-ba9876543210", later).unwrap().status, "expired");
        assert_eq!(Store::open(dir.path()).get("dc-ba9876543210", now).unwrap().status, "expired", "recorded");
        assert_eq!(reopened.list(Some("expired"), 10, later).len(), 1);
    }
}
