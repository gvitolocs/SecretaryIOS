//! The JSON-action planner (Node `routeLucinaPlan` without the tier-0 regex
//! classifier): tier-1 intent JSON (`classifyLucinaIntentWithModel`), then a
//! direct plan, a chat reply (`generateReply`) or the full `planResponse`
//! JSON plan. Prompts are verbatim (see `prompts.rs`).

use std::sync::Arc;

use chrono::Utc;
use lucina_llm::{ChatModel, ChatRequest, LlmError, Message};
use lucina_store::recent::clip_memory_for_reply;
use serde_json::{json, Value};

use crate::prompts::{
    looks_like_complex_request, looks_like_reminder_request, looks_like_travel_info,
    looks_like_vulnerable_support, Prompts,
};

pub const INTENT_CONFIDENCE_MIN: f64 = 0.72;

const INTENTS: &[&str] = &[
    "chat",
    "social_greeting",
    "agent_status_check",
    "explicit_memory_recall",
    "temporal_query",
    "continue_current_topic",
    "new_topic",
    "list_reminders",
    "schedule_reminder",
    "calendar_event",
    "generate_image",
    "graph_task",
    "spreadsheet_update",
    "web_crawl",
    "oracle_monitor",
    "model_status",
    "revive_qwen",
    "poko_news_scout_retry",
    "social_post",
    "cursor_task",
    "discord_admin",
    "grocery.purchase",
    "grocery.consume",
    "grocery.finished",
    "grocery.shopping_list.add",
    "grocery.shopping_list.remove",
    "grocery.shopping_list.query",
    "grocery.offer.search",
    "grocery.expense.query",
    "grocery.pantries.query",
    "grocery.price.query",
    "grocery.analytics.query",
    "unknown",
];

#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    pub intent: String,
    pub confidence: f64,
    pub needs_tools: bool,
    pub reply_mode: String,
    pub scope: String,
}

fn default_reply_mode(intent: &str) -> &'static str {
    match intent {
        "list_reminders"
        | "model_status"
        | "revive_qwen"
        | "oracle_monitor"
        | "poko_news_scout_retry"
        | "spreadsheet_update"
        | "social_post" => "direct",
        "chat"
        | "social_greeting"
        | "agent_status_check"
        | "temporal_query"
        | "continue_current_topic"
        | "new_topic"
        | "explicit_memory_recall" => "chat",
        _ => "full_plan",
    }
}

/// `extractJsonObject`: fenced or bare, first `{` to last `}`.
pub fn extract_json_object(content: &str) -> Option<Value> {
    let text = content.trim();
    let candidate = match text.find("```") {
        Some(start) => {
            let rest = &text[start + 3..];
            let rest = rest
                .strip_prefix("json")
                .or_else(|| rest.strip_prefix("JSON"))
                .unwrap_or(rest);
            rest.find("```")
                .map(|end| rest[..end].trim())
                .unwrap_or(text)
        }
        None => text,
    };
    let (s, e) = (candidate.find('{')?, candidate.rfind('}')?);
    if e <= s {
        return None;
    }
    serde_json::from_str(&candidate[s..=e]).ok()
}

/// `parsePlanModelOutput`: JSON, or salvage the `"text"` of broken JSON.
pub fn parse_plan_output(content: &str) -> Option<Value> {
    if let Some(v) = extract_json_object(content) {
        return Some(v);
    }
    let re = regex::Regex::new(r#""text"\s*:\s*"((?:[^"\\]|\\.)*)"#).ok()?;
    let cap = re
        .captures(content)?
        .get(1)?
        .as_str()
        .trim_end_matches('\\');
    let decoded: String =
        serde_json::from_str(&format!("\"{cap}\"")).unwrap_or_else(|_| cap.to_owned());
    (!decoded.trim().is_empty()).then(|| json!({"action": "reply", "text": decoded.trim()}))
}

tokio::task_local! {
    /// Fixed system text for every model call of the current chat turn (her live game: state, identity and
    /// priority rule). Lives in the system part, so no context trimming (casual shortcut, budgets) can drop it.
    pub static TURN_SYSTEM: String;
    /// Swipe to reply: the earlier message the owner is answering
    /// (`chat::reply::block`). Every user part of the turn's model calls
    /// carries it right before the message (`Prompts::user_block`,
    /// `Prompts::plan_user`), so the casual memory clip never drops it.
    pub static TURN_REPLY: String;
    /// The reply-language line for the turn (`resolve_reply_language`): the language of the owner's
    /// latest message, never of earlier turns or the device. Read by `Prompts::language_rule`.
    pub static TURN_LANG: String;
}

/// The turn's reply-language line ("" outside a turn).
pub fn turn_lang() -> String {
    TURN_LANG
        .try_with(|l| l.trim().to_owned())
        .unwrap_or_default()
}

/// The ack of a page read or web search that runs on her computer, in the turn's language.
pub fn web_read_ack() -> &'static str {
    let line = turn_lang();
    if line.starts_with("Reply language: Italian") {
        "Ok, lo apro nel browser del mio computer e ti dico."
    } else if line.starts_with("Reply language: Danish") {
        "Okay, jeg åbner det i browseren på min computer og vender tilbage."
    } else {
        "Ok, I'm opening it in my computer's browser and will report back."
    }
}

/// What she says when the plan failed twice: no tool-less guess, a short honest line in the turn's
/// language (English outside a turn).
pub fn plan_failed_text() -> &'static str {
    let line = turn_lang();
    if line.starts_with("Reply language: Italian") {
        "Scusa, mi sono incartata nel capire come gestire questo messaggio. Me lo rimandi?"
    } else if line.starts_with("Reply language: Danish") {
        "Undskyld, jeg fik viklet mig ind i, hvordan jeg skulle håndtere den besked. Kan du sende den igen?"
    } else {
        "Sorry, I got tangled up working out how to handle that message. Could you send it again?"
    }
}

/// Which language the reply uses: the owner's own text first; with no text of its own (a bare link, a
/// photo, a file) the language of his recent messages, newest first; then English. Never the device
/// locale or his location. `recent_owner` is newest first.
pub fn resolve_reply_language<S: AsRef<str>>(
    owner_text: &str,
    recent_owner: &[S],
) -> lucina_intent::lang::Lang {
    use lucina_intent::lang::{detect, Lang};
    detect(owner_text)
        .or_else(|| recent_owner.iter().find_map(|t| detect(t.as_ref())))
        .unwrap_or(Lang::En)
}

/// The owner's recent plain messages in `chat_id`, newest first (share wrappers are skipped: they hold
/// English instructions and extracted content, not his words).
pub fn recent_owner_texts(recent: &lucina_store::recent::RecentChat, chat_id: &str) -> Vec<String> {
    recent
        .turns(chat_id)
        .into_iter()
        .rev()
        .filter(|t| t.role == "user" && !t.text.contains("<Shared content>"))
        .take(6)
        .map(|t| t.text)
        .collect()
}

/// `Reply language: X ...` for the turn, resolved from the owner's text and recent messages.
pub fn reply_language_line(
    recent: &lucina_store::recent::RecentChat,
    chat_id: &str,
    owner_text: &str,
) -> String {
    let lang = resolve_reply_language(owner_text, &recent_owner_texts(recent, chat_id));
    Prompts::language_line(lang)
}

/// Last thing the planner reads, after the game block: the answer is one JSON object.
pub const PLAN_JSON_REMINDER: &str = "Output format: reply with exactly one JSON object for the owner's latest message (the schema above) and no text outside it. Put prose only inside the \"text\" field.";

/// Re-ask text when a reply came back as tool-call markup.
pub const NO_TOOL_MARKUP_RULE: &str = "You have no tools in this reply. Answer in plain text only; never write tool calls, function-call tags or other markup.";

/// What the owner sees when the model keeps answering in tool-call markup.
pub const TOOL_MARKUP_FAILURE: &str =
    "I couldn't put that into words just now. Please send it once more.";

/// Sent after a plan answer that was not JSON.
pub const PLAN_REPAIR_PROMPT: &str = "Your previous output was not a valid JSON object. Return only the JSON object for this turn, using the schema above, for the owner's latest message only. No text outside the JSON.";

/// The current turn's reply block ("" outside a turn or with no `replyTo`).
pub fn turn_reply() -> String {
    TURN_REPLY
        .try_with(|r| r.trim().to_owned())
        .unwrap_or_default()
}

/// `system` plus the turn's fixed system text (none for the intent classifier, or outside a chat turn).
pub fn with_turn_system(system: String, purpose: &str) -> String {
    if purpose == "intent" {
        return system;
    }
    match TURN_SYSTEM.try_with(|g| g.trim().to_owned()) {
        Ok(g) if !g.is_empty() => format!("{system}\n\n{g}"),
        _ => system,
    }
}

pub struct Planner {
    pub model: Arc<dyn ChatModel>,
    pub prompts: Prompts,
}

pub struct TurnInput<'a> {
    pub user_text: &'a str,
    pub memory: &'a str,
    pub user_label: &'a str,
    pub platform: &'a str,
}

impl Planner {
    async fn complete(
        &self,
        system: String,
        user: String,
        temperature: f32,
        purpose: &'static str,
    ) -> Result<String, LlmError> {
        let r = self
            .model
            .complete(ChatRequest {
                messages: vec![
                    Message::system(with_turn_system(system, purpose)),
                    Message::user(user),
                ],
                temperature: Some(temperature),
                purpose,
                ..ChatRequest::default()
            })
            .await?;
        Ok(r.message.text().to_owned())
    }

    /// `generateReply`.
    pub async fn generate_reply(
        &self,
        t: &TurnInput<'_>,
        extra_system: &str,
        knowledge: &str,
        purpose: &'static str,
    ) -> Result<String, LlmError> {
        let system = self.prompts.reply_system(
            t.platform,
            t.user_text,
            t.memory,
            extra_system,
            knowledge,
            Utc::now(),
        );
        let user = Prompts::user_block(t.user_label, t.memory, t.user_text);
        let mut text = self
            .complete(system.clone(), user.clone(), 0.6, purpose)
            .await?;
        if lucina_llm::has_tool_markup(&text) {
            // The model wrote tool-call markup as its answer (this path has no tools): ask once more for plain
            // text, then admit the failure rather than show the markup to the owner.
            tracing::warn!(purpose, "reply contained tool-call markup; re-asking once");
            text = self
                .complete(
                    format!("{system} {NO_TOOL_MARKUP_RULE}"),
                    user,
                    0.6,
                    purpose,
                )
                .await?;
            if lucina_llm::has_tool_markup(&text) {
                return Ok(TOOL_MARKUP_FAILURE.into());
            }
        }
        Ok(if text.trim().is_empty() {
            "I could not generate a reply.".into()
        } else {
            text
        })
    }

    /// `generateSkillReply`.
    pub async fn generate_skill_reply(
        &self,
        t: &TurnInput<'_>,
        skill_name: &str,
        skill_result: &str,
        instruction: &str,
    ) -> Result<String, LlmError> {
        let (user, extra, knowledge) =
            self.prompts
                .skill_reply_parts(t.user_text, skill_name, skill_result, instruction);
        let inner = TurnInput {
            user_text: &user,
            ..*t
        };
        self.generate_reply(&inner, &extra, &knowledge, "heavy-reply")
            .await
    }

    /// Tier 1 intent classifier: the model's JSON (`None` when it was not
    /// JSON). `t.memory` is the recent-turn context (Node passes the
    /// classifier only that).
    pub async fn classify_json(&self, t: &TurnInput<'_>) -> Result<Option<Value>, LlmError> {
        let content = self
            .complete(
                self.prompts.intent_system(),
                Prompts::intent_user(t.platform, t.user_label, t.memory, t.user_text),
                0.0,
                "intent",
            )
            .await?;
        let text = content.trim();
        // Node `extractJsonObject`: first `{` to last `}`.
        let parsed = match (text.find('{'), text.rfind('}')) {
            (Some(a), Some(b)) if b > a => serde_json::from_str::<Value>(&text[a..=b]).ok(),
            _ => None,
        };
        if parsed.is_none() {
            tracing::warn!("intent classifier output was not JSON");
        }
        Ok(parsed)
    }

    /// `routeLucinaPlan` with the classification the turn already made
    /// (`precomputedClassification`): full plan, chat reply (with capability
    /// hints on non-casual replies), a direct plan, or the full plan.
    pub async fn route_classified(
        &self,
        t: &TurnInput<'_>,
        c: &lucina_intent::tier0::Classification,
        s: &lucina_intent::heuristics::Settings,
        capability_hints: bool,
    ) -> Result<Value, LlmError> {
        use lucina_intent::heuristics::{looks_like_complex, looks_like_vulnerable_support};
        use lucina_intent::tier0::{direct_plan, is_misrouted, should_use_full_plan};
        if !s.intent_router || should_use_full_plan(c, t.user_text, s) {
            return self.plan_response(t).await;
        }
        if c.reply_mode == "chat" || c.intent == "chat" {
            let casual =
                !looks_like_complex(t.user_text, s) && !looks_like_vulnerable_support(t.user_text);
            let knowledge = if casual {
                String::new()
            } else {
                self.prompts.operator_brief.clone()
            };
            let memory = clip_memory_for_reply(t.memory, if casual { 1600 } else { 4000 });
            let extra = if casual {
                "Reply like a normal Telegram chat. One or two short sentences. No capability menus."
            } else {
                ""
            };
            let inner = TurnInput {
                memory: &memory,
                ..*t
            };
            let raw = self
                .generate_reply(&inner, extra, &knowledge, "reply")
                .await?;
            let text = if casual {
                raw.trim().to_owned()
            } else {
                lucina_intent::hints::append_capability_hints(&raw, t.user_text, capability_hints)
            };
            return Ok(json!({"action": "reply", "text": text}));
        }
        if let Some(p) = direct_plan(c, t.user_text, t.memory, s) {
            if !is_misrouted(c, t.user_text, t.memory) {
                return Ok(p);
            }
            tracing::warn!(intent = %c.intent, "lucina tier0 tool intent ignored for chat message");
        }
        self.plan_response(t).await
    }

    /// Tier 1 intent classifier. A parse failure is `unknown`/`full_plan`.
    pub async fn classify(&self, t: &TurnInput<'_>) -> Result<Classification, LlmError> {
        let content = self
            .complete(
                self.prompts.intent_system(),
                Prompts::intent_user(t.platform, t.user_label, t.memory, t.user_text),
                0.0,
                "intent",
            )
            .await?;
        let Some(v) = extract_json_object(&content) else {
            tracing::warn!("intent classifier output was not JSON");
            return Ok(Classification {
                intent: "unknown".into(),
                confidence: 0.0,
                needs_tools: false,
                reply_mode: "full_plan".into(),
                scope: "all".into(),
            });
        };
        let mut intent = v["intent"]
            .as_str()
            .filter(|i| INTENTS.contains(i))
            .unwrap_or("unknown")
            .to_owned();
        if intent == "oracle_monitor" && !self.prompts.oracle_monitor_enabled {
            intent = "chat".into();
        }
        if intent == "list_reminders" && looks_like_travel_info(t.user_text) {
            intent = "chat".into();
        }
        let reply_mode = v["reply_mode"]
            .as_str()
            .filter(|m| ["direct", "chat", "after_tool", "full_plan"].contains(m))
            .map(str::to_owned)
            .unwrap_or_else(|| default_reply_mode(&intent).into());
        let confidence = v["confidence"]
            .as_f64()
            .or_else(|| v["confidence"].as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        Ok(Classification {
            intent,
            confidence,
            needs_tools: v["needs_tools"].as_bool().unwrap_or(false),
            reply_mode,
            scope: if v["scope"] == "today" {
                "today".into()
            } else {
                "all".into()
            },
        })
    }

    pub fn should_use_full_plan(c: &Classification, user_text: &str) -> bool {
        c.confidence < INTENT_CONFIDENCE_MIN
            || c.intent == "unknown"
            || c.reply_mode == "full_plan"
            || c.reply_mode == "after_tool"
            || (c.needs_tools && c.reply_mode != "direct")
            || ((c.intent == "chat" || c.reply_mode == "chat")
                && looks_like_complex_request(user_text, "")
                && !looks_like_vulnerable_support(user_text)
                && actionable(user_text))
    }

    /// One plan call (JSON mode): the text and the finish reason.
    async fn plan_call(
        &self,
        messages: Vec<Message>,
    ) -> Result<(String, Option<String>), LlmError> {
        let r = self
            .model
            .complete(ChatRequest {
                messages,
                temperature: Some(0.2),
                purpose: "plan",
                json_object: true,
                ..ChatRequest::default()
            })
            .await?;
        Ok((r.message.text().to_owned(), r.finish_reason))
    }

    /// `planResponse` (Lucina): the JSON plan plus Node's post-processing. The request asks for JSON
    /// mode; a reply that still is not JSON gets one repair retry before the caller falls back.
    pub async fn plan_response(&self, t: &TurnInput<'_>) -> Result<Value, LlmError> {
        let system = format!(
            "{}\n\n{PLAN_JSON_REMINDER}",
            with_turn_system(
                self.prompts.plan_system_for(t.user_text, Utc::now()),
                "plan"
            )
        );
        let user = Prompts::plan_user(t.platform, t.user_label, t.memory, t.user_text);
        let mut messages = vec![Message::system(system), Message::user(user)];
        let (mut content, finish) = self.plan_call(messages.clone()).await?;
        let mut parsed = parse_plan_output(&content);
        if parsed.is_none() {
            tracing::warn!(
                chars = content.chars().count(),
                finish = ?finish,
                head = %content.chars().take(80).collect::<String>().replace('\n', " "),
                "plan output was not JSON; retrying once"
            );
            if !content.trim().is_empty() {
                messages.push(Message::assistant(content.clone()));
            }
            messages.push(Message::user(PLAN_REPAIR_PROMPT));
            let (again, _) = self.plan_call(messages).await?;
            parsed = parse_plan_output(&again);
            if parsed.is_none() {
                // Both answers, raw (capped), so a failing turn can be debugged from the journal.
                tracing::warn!(
                    first = %content.chars().take(2000).collect::<String>(),
                    retry = %again.chars().take(2000).collect::<String>(),
                    "plan output was not JSON twice"
                );
            }
            content = again;
        }
        let parsed = parsed
            .ok_or_else(|| LlmError::Decode("No JSON object found in model response".into()))?;
        let action = parsed["action"].as_str().unwrap_or("reply").to_owned();
        let s = |k: &str| parsed[k].as_str().unwrap_or("").trim().to_owned();
        if looks_like_reminder_request(t.user_text)
            && (action == "reply" || action == "cursor_task")
        {
            let due = if s("dueDate").is_empty() {
                t.user_text.trim().to_owned()
            } else {
                s("dueDate")
            };
            return Ok(
                json!({"action": "schedule_reminder", "text": s("text"), "dueDate": due, "ack": "I will schedule that reminder now."}),
            );
        }
        Ok(match action.as_str() {
            "schedule_reminder" => {
                json!({"action": "schedule_reminder", "text": s("text"), "dueDate": s("dueDate"), "ack": if s("ack").is_empty() { "I will schedule that reminder now.".into() } else { s("ack") }})
            }
            "calendar_event" => {
                json!({"action": "calendar_event", "title": s("title"), "start": s("start"), "end": s("end"), "location": s("location")})
            }
            "social_post_task" => json!({"action": "poko_news_scout_retry", "ack": s("ack")}),
            "oracle_monitor_task" if !self.prompts.oracle_monitor_enabled => json!({
                "action": "reply",
                "text": format!("Su {} il monitor SSH Oracle è disabilitato. Per Poko/Bot Hospital uso peer1.", self.prompts.host_label),
            }),
            "generate_image"
            | "web_crawl_task"
            | "cursor_task"
            | "graph_task"
            | "spreadsheet_update_task"
            | "poko_news_scout_retry"
            | "oracle_monitor_task"
            | "model_status_task"
            | "revive_qwen_task"
            | "discord_admin_task"
            | "delegate_task"
            | "computer_task"
            | "game_hint"
            | "game_start"
            | "game_pause" => parsed.clone(),
            _ => {
                let text = if s("text").is_empty() {
                    content.trim().to_owned()
                } else {
                    s("text")
                };
                if lucina_llm::has_tool_markup(&text) {
                    // The caller falls back to a plain reply, which guards against the markup too.
                    return Err(LlmError::Decode("tool-call markup in the plan text".into()));
                }
                json!({"action": "reply", "text": text})
            }
        })
    }

    /// `buildDirectPlan` for the intents the classifier can settle alone.
    fn direct_plan(c: &Classification) -> Option<Value> {
        Some(match c.intent.as_str() {
            "list_reminders" => json!({"action": "list_reminders", "scope": c.scope}),
            "schedule_reminder" => {
                json!({"action": "schedule_reminder", "text": "", "dueDate": "", "ack": "Programmo il promemoria adesso."})
            }
            "calendar_event" => json!({"action": "calendar_event"}),
            "model_status" => {
                json!({"action": "model_status_task", "ack": "Controllo quale modello è caricato adesso."})
            }
            "revive_qwen" => {
                json!({"action": "revive_qwen_task", "ack": "Riporto Qwen 3.8 sulla GPU adesso."})
            }
            "poko_news_scout_retry" | "social_post" => {
                json!({"action": "poko_news_scout_retry", "ack": "Avvio il Bot Hospital su Poko (peer1) e riprovo lo scout news adesso."})
            }
            "spreadsheet_update" => {
                json!({"action": "spreadsheet_update_task", "ack": "Controllo il Google Sheet e provo ad aggiornarlo adesso."})
            }
            "cursor_task" => {
                json!({"action": "cursor_task", "ack": "Avvio Harness con Qwen locale su nezopt per questo task adesso."})
            }
            _ => return None,
        })
    }

    /// Route a turn: `(plan, classification)`.
    pub async fn route(
        &self,
        t: &TurnInput<'_>,
        extra_system: &str,
    ) -> Result<(Value, Classification), LlmError> {
        let c = self.classify(t).await?;
        tracing::info!(intent = %c.intent, confidence = c.confidence, reply_mode = %c.reply_mode, "lucina intent classified");
        if c.intent.starts_with("grocery.") {
            // Grocery turns run the shopping tool (Node: the deterministic grocery pipeline).
            return Ok((json!({"action": "grocery"}), c));
        }
        if Self::should_use_full_plan(&c, t.user_text) {
            return Ok((self.plan_response(t).await?, c));
        }
        if c.reply_mode == "chat" || c.intent == "chat" {
            let casual = !looks_like_complex_request(t.user_text, t.memory)
                && !looks_like_vulnerable_support(t.user_text);
            let knowledge = if casual {
                String::new()
            } else {
                self.prompts.operator_brief.clone()
            };
            let memory = clip_memory_for_reply(t.memory, if casual { 1600 } else { 4000 });
            let extra = if casual {
                format!("Reply like a normal Telegram chat. One or two short sentences. No capability menus. {extra_system}")
            } else {
                extra_system.to_owned()
            };
            let inner = TurnInput {
                memory: &memory,
                ..*t
            };
            let text = self
                .generate_reply(&inner, extra.trim(), &knowledge, "reply")
                .await?;
            return Ok((json!({"action": "reply", "text": text.trim()}), c));
        }
        if let Some(p) = Self::direct_plan(&c) {
            return Ok((p, c));
        }
        Ok((self.plan_response(t).await?, c))
    }
}

/// Simplified `looksLikeActionableLucinaRequest`.
fn actionable(text: &str) -> bool {
    regex::Regex::new(r"(?i)^(please |per favore )?(crea|create|genera|generate|fai|make|aggiorna|update|scrivi|write|controlla|check|apri|open|esegui|run|installa|install|pubblica|publish|manda|send|imposta|set|metti|aggiungi|add)\b")
        .map(|r| r.is_match(text.trim()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_extraction() {
        assert_eq!(
            extract_json_object("```json\n{\"action\":\"reply\",\"text\":\"ciao\"}\n```").unwrap()
                ["text"],
            "ciao"
        );
        assert_eq!(
            extract_json_object("Sure: {\"a\": {\"b\": 1}} ok").unwrap()["a"]["b"],
            1
        );
        assert!(extract_json_object("no json").is_none());
        assert_eq!(
            parse_plan_output("{\"action\":\"reply\",\"text\":\"Ciao, come st").unwrap()["text"],
            "Ciao, come st"
        );
    }

    // ---- the JSON plan: JSON mode, one repair retry, language and the game block

    use lucina_llm::ScriptedModel;

    fn planner(model: &Arc<ScriptedModel>) -> Planner {
        Planner {
            model: model.clone(),
            prompts: Prompts::minimal("Lucina"),
        }
    }

    fn turn<'a>(text: &'a str, memory: &'a str) -> TurnInput<'a> {
        TurnInput {
            user_text: text,
            memory,
            user_label: "Giuseppe",
            platform: "secretary",
        }
    }

    /// The 10:59 and 11:00 turns of 2026-10-28, with her old Italian turns in the context and the game block
    /// that every owner turn carried in the system part.
    const MEMORY: &str = "Recent chat turns (most recent last; stamps show when each turn happened):\n[28/10 10:58 Europe/Copenhagen] user: Ho fatto la lavanderia, poi vado alla posta per il pacco\n[28/10 10:58 Europe/Copenhagen] assistant: Va bene, ti preparo il giro delle commissioni.";

    #[tokio::test]
    async fn the_plan_request_is_json_mode_and_ends_with_the_format_reminder() {
        let model = Arc::new(ScriptedModel::new());
        model.push_text(r#"{"action":"reply","text":"At 12:00."}"#);
        let game = crate::bridges::game_block(
            &json!({"running": false, "actions": 17274, "events": ["a", "b"]}),
            "MODE: Dialog\nLOCATION: CHAMPIONS_ROOM",
            false,
        );
        let plan = TURN_SYSTEM
            .scope(game, async {
                planner(&model)
                    .plan_response(&turn(
                        "You know what time is the meeting therese è teresa",
                        MEMORY,
                    ))
                    .await
            })
            .await
            .unwrap();
        assert_eq!(plan["text"], "At 12:00.");
        let reqs = model.requests.lock().unwrap().clone();
        assert_eq!(reqs.len(), 1);
        assert!(reqs[0].json_object, "JSON mode");
        assert_eq!(reqs[0].purpose, "plan");
        let system = reqs[0].messages[0].text();
        assert!(
            system.ends_with(PLAN_JSON_REMINDER),
            "the last words are the format"
        );
        assert!(system.contains("Reply language: English"));
        assert!(
            system.contains("CHAMPIONS_ROOM"),
            "the paused game stays as information"
        );
        assert!(!system.contains("This Pokémon Red run is YOU: your own playing self"));
        assert!(system.contains("Never flatly deny what you said earlier"));
        // Size of what a failing turn sent: the verbatim system prompt is most of it.
        let user = reqs[0].messages[1].text();
        eprintln!(
            "plan prompt chars: system={} user={}",
            system.chars().count(),
            user.chars().count()
        );
        assert!(user.contains("Message:\nYou know what time is the meeting therese è teresa"));
    }

    #[tokio::test]
    async fn a_prose_answer_gets_one_repair_retry() {
        let model = Arc::new(ScriptedModel::new());
        model.push_text("Alle 12:00 hai Therese zoom, non Teresa.");
        model.push_text(r#"{"action":"reply","text":"Therese zoom is at 12:00."}"#);
        let plan = planner(&model)
            .plan_response(&turn(
                "You know what time is the meeting therese è teresa",
                MEMORY,
            ))
            .await
            .unwrap();
        assert_eq!(plan["action"], "reply");
        assert_eq!(plan["text"], "Therese zoom is at 12:00.");
        let reqs = model.requests.lock().unwrap().clone();
        assert_eq!(reqs.len(), 2);
        assert!(reqs.iter().all(|r| r.json_object));
        let retry = &reqs[1].messages;
        assert_eq!(
            retry.len(),
            4,
            "system, user, her previous output, the repair request"
        );
        assert_eq!(retry[2].text(), "Alle 12:00 hai Therese zoom, non Teresa.");
        assert!(retry[3]
            .text()
            .contains("Return only the JSON object for this turn"));
    }

    #[tokio::test]
    async fn two_non_json_answers_fail_so_the_caller_can_fall_back() {
        let model = Arc::new(ScriptedModel::new());
        model.push_text("prose");
        model.push_text("more prose");
        let err = planner(&model)
            .plan_response(&turn("Cancella", ""))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("No JSON object found"));
        assert_eq!(model.request_count(), 2, "exactly one retry");
    }

    #[tokio::test]
    async fn the_fallback_reply_answers_the_latest_message_in_its_language() {
        let model = Arc::new(ScriptedModel::new());
        model.push_text("It is at 12:00.");
        let t = turn("You know what time is the meeting therese è teresa", MEMORY);
        let text = planner(&model)
            .generate_reply(&t, crate::prompts::PLAN_FALLBACK_RULE, "", "reply")
            .await
            .unwrap();
        assert_eq!(text, "It is at 12:00.");
        let reqs = model.requests.lock().unwrap().clone();
        let system = reqs[0].messages[0].text();
        assert!(system.contains("Answer ONLY the owner's latest message"));
        assert!(system.contains("never answer an older topic"));
        assert!(system.contains("Reply language: English"));
        assert!(!reqs[0].json_object);
    }

    #[test]
    fn the_reply_language_is_the_owner_s_own_text_then_his_recent_messages_then_english() {
        use lucina_intent::lang::Lang;
        let none: [&str; 0] = [];
        // His own text wins, whatever came before.
        assert_eq!(
            resolve_reply_language(
                "You know what time is the meeting therese è teresa",
                &["Di cosa stai parlando?"]
            ),
            Lang::En
        );
        assert_eq!(
            resolve_reply_language("Explode outlook in your sandbox", &["Cancella"]),
            Lang::En
        );
        assert_eq!(
            resolve_reply_language("Di cosa stai parlando?", &["hello there my friend"]),
            Lang::It
        );
        assert_eq!(
            resolve_reply_language("hvad har jeg i kalenderen i morgen", &none),
            Lang::Da
        );
        // No text of its own (a bare link, a photo, a file): his recent messages, newest first, then English.
        let recent = [
            "ok",
            "Ho fatto la lavanderia, poi vado alla posta per il pacco",
            "hello there my friend",
        ];
        assert_eq!(resolve_reply_language("", &recent), Lang::It);
        assert_eq!(
            resolve_reply_language("📎 The Little Mermaid (statue)", &recent[..1]),
            Lang::En
        );
        assert_eq!(resolve_reply_language("", &none), Lang::En);
        // The exact texts that came back in Danish on 2026-10-26/27.
        for t in [
            "[selftest] Comprare: latte, uova, pane integrale, 2 limoni",
            "[selftest] promemoria: chiamare il dentista domani alle 10",
            "[selftest] tre foto: cosa hanno in comune?",
        ] {
            assert_eq!(resolve_reply_language(t, &none), Lang::It, "{t}");
        }
        assert_eq!(
            resolve_reply_language(
                "https://en.wikipedia.org/wiki/The_Little_Mermaid_(statue)",
                &none
            ),
            Lang::En
        );
    }

    #[test]
    fn recent_owner_texts_skip_share_wrappers_and_assistant_turns() {
        let recent = lucina_store::recent::RecentChat::new(None);
        let now = Utc::now();
        recent.append("s", "user", "Ho fatto la lavanderia", now);
        recent.append("s", "assistant", "Hello there, how are you my friend", now);
        recent.append(
            "s",
            "user",
            "Owner's question or note: \"\"\n\n<Shared content>the and of</Shared content>",
            now,
        );
        let texts = recent_owner_texts(&recent, "s");
        assert_eq!(texts, vec!["Ho fatto la lavanderia".to_owned()]);
        assert!(reply_language_line(&recent, "s", "").contains("Reply language: Italian"));
    }

    #[tokio::test]
    async fn tool_markup_is_never_returned_as_a_reply() {
        let markup = "<\u{ff5c}DSML\u{ff5c}function_calls><\u{ff5c}DSML\u{ff5c}invoke name=\"drive\"></\u{ff5c}DSML\u{ff5c}invoke></\u{ff5c}DSML\u{ff5c}function_calls>";
        // Once, then plain text: the retry answers.
        let model = Arc::new(ScriptedModel::new());
        model.push_text(markup);
        model.push_text("Alle 12:00.");
        let t = turn("A che ora è la riunione?", "");
        let text = planner(&model)
            .generate_reply(&t, "", "", "reply")
            .await
            .unwrap();
        assert_eq!(text, "Alle 12:00.");
        let reqs = model.requests.lock().unwrap().clone();
        assert!(reqs[1].messages[0].text().contains(NO_TOOL_MARKUP_RULE));
        // Twice: the honest one-line failure.
        let model = Arc::new(ScriptedModel::new());
        model.push_text(markup);
        model.push_text(markup);
        let text = planner(&model)
            .generate_reply(&t, "", "", "reply")
            .await
            .unwrap();
        assert_eq!(text, TOOL_MARKUP_FAILURE);
        // A plan whose text is markup is an error, so the caller falls back.
        let model = Arc::new(ScriptedModel::new());
        model.push_text(&format!(
            "{{\"action\":\"reply\",\"text\":{}}}",
            serde_json::to_string(markup).unwrap()
        ));
        assert!(planner(&model).plan_response(&t).await.is_err());
    }
}
