//! The JSON-action planner (Node `routeLucinaPlan` without the tier-0 regex
//! classifier): tier-1 intent JSON (`classifyLucinaIntentWithModel`), then a
//! direct plan, a chat reply (`generateReply`) or the full `planResponse`
//! JSON plan. Prompts are verbatim (see `prompts.rs`).

use std::sync::Arc;

use chrono::Utc;
use lucina_llm::{ChatModel, ChatRequest, LlmError, Message};
use lucina_store::recent::clip_memory_for_reply;
use serde_json::{json, Value};

use crate::prompts::{looks_like_complex_request, looks_like_reminder_request, looks_like_travel_info, looks_like_vulnerable_support, Prompts};

pub const INTENT_CONFIDENCE_MIN: f64 = 0.72;

const INTENTS: &[&str] = &[
    "chat", "social_greeting", "agent_status_check", "explicit_memory_recall", "temporal_query", "continue_current_topic", "new_topic",
    "list_reminders", "schedule_reminder", "generate_image", "graph_task", "spreadsheet_update", "web_crawl", "oracle_monitor", "model_status",
    "revive_qwen", "poko_news_scout_retry", "social_post", "cursor_task", "discord_admin", "grocery.purchase", "grocery.consume",
    "grocery.finished", "grocery.shopping_list.add", "grocery.shopping_list.remove", "grocery.shopping_list.query", "grocery.offer.search",
    "grocery.expense.query", "grocery.pantries.query", "grocery.price.query", "grocery.analytics.query", "unknown",
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
        "list_reminders" | "model_status" | "revive_qwen" | "oracle_monitor" | "poko_news_scout_retry" | "spreadsheet_update" | "social_post" => "direct",
        "chat" | "social_greeting" | "agent_status_check" | "temporal_query" | "continue_current_topic" | "new_topic" | "explicit_memory_recall" => "chat",
        _ => "full_plan",
    }
}

/// `extractJsonObject`: fenced or bare, first `{` to last `}`.
pub fn extract_json_object(content: &str) -> Option<Value> {
    let text = content.trim();
    let candidate = match text.find("```") {
        Some(start) => {
            let rest = &text[start + 3..];
            let rest = rest.strip_prefix("json").or_else(|| rest.strip_prefix("JSON")).unwrap_or(rest);
            rest.find("```").map(|end| rest[..end].trim()).unwrap_or(text)
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
    let cap = re.captures(content)?.get(1)?.as_str().trim_end_matches('\\');
    let decoded: String = serde_json::from_str(&format!("\"{cap}\"")).unwrap_or_else(|_| cap.to_owned());
    (!decoded.trim().is_empty()).then(|| json!({"action": "reply", "text": decoded.trim()}))
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
    async fn complete(&self, system: String, user: String, temperature: f32, purpose: &'static str) -> Result<String, LlmError> {
        let r = self
            .model
            .complete(ChatRequest {
                messages: vec![Message::system(system), Message::user(user)],
                temperature: Some(temperature),
                purpose,
                ..ChatRequest::default()
            })
            .await?;
        Ok(r.message.text().to_owned())
    }

    /// `generateReply`.
    pub async fn generate_reply(&self, t: &TurnInput<'_>, extra_system: &str, knowledge: &str, purpose: &'static str) -> Result<String, LlmError> {
        let system = self.prompts.reply_system(t.platform, t.user_text, t.memory, extra_system, knowledge, Utc::now());
        let text = self.complete(system, Prompts::user_block(t.user_label, t.memory, t.user_text), 0.6, purpose).await?;
        Ok(if text.trim().is_empty() { "I could not generate a reply.".into() } else { text })
    }

    /// `generateSkillReply`.
    pub async fn generate_skill_reply(&self, t: &TurnInput<'_>, skill_name: &str, skill_result: &str, instruction: &str) -> Result<String, LlmError> {
        let (user, extra, knowledge) = self.prompts.skill_reply_parts(t.user_text, skill_name, skill_result, instruction);
        let inner = TurnInput { user_text: &user, ..*t };
        self.generate_reply(&inner, &extra, &knowledge, "heavy-reply").await
    }

    /// Tier 1 intent classifier. A parse failure is `unknown`/`full_plan`.
    pub async fn classify(&self, t: &TurnInput<'_>) -> Result<Classification, LlmError> {
        let content = self
            .complete(self.prompts.intent_system(), Prompts::intent_user(t.platform, t.user_label, t.memory, t.user_text), 0.0, "intent")
            .await?;
        let Some(v) = extract_json_object(&content) else {
            tracing::warn!("intent classifier output was not JSON");
            return Ok(Classification { intent: "unknown".into(), confidence: 0.0, needs_tools: false, reply_mode: "full_plan".into(), scope: "all".into() });
        };
        let mut intent = v["intent"].as_str().filter(|i| INTENTS.contains(i)).unwrap_or("unknown").to_owned();
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
        let confidence = v["confidence"].as_f64().or_else(|| v["confidence"].as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0).clamp(0.0, 1.0);
        Ok(Classification {
            intent,
            confidence,
            needs_tools: v["needs_tools"].as_bool().unwrap_or(false),
            reply_mode,
            scope: if v["scope"] == "today" { "today".into() } else { "all".into() },
        })
    }

    pub fn should_use_full_plan(c: &Classification, user_text: &str) -> bool {
        c.confidence < INTENT_CONFIDENCE_MIN
            || c.intent == "unknown"
            || c.reply_mode == "full_plan"
            || c.reply_mode == "after_tool"
            || (c.needs_tools && c.reply_mode != "direct")
            || ((c.intent == "chat" || c.reply_mode == "chat") && looks_like_complex_request(user_text, "") && !looks_like_vulnerable_support(user_text) && actionable(user_text))
    }

    /// `planResponse` (Lucina): the JSON plan plus Node's post-processing.
    pub async fn plan_response(&self, t: &TurnInput<'_>) -> Result<Value, LlmError> {
        let content = self.complete(self.prompts.plan_system(), Prompts::plan_user(t.platform, t.user_label, t.memory, t.user_text), 0.2, "plan").await?;
        let parsed = parse_plan_output(&content).ok_or_else(|| LlmError::Decode("No JSON object found in model response".into()))?;
        let action = parsed["action"].as_str().unwrap_or("reply").to_owned();
        let s = |k: &str| parsed[k].as_str().unwrap_or("").trim().to_owned();
        if looks_like_reminder_request(t.user_text) && (action == "reply" || action == "cursor_task") {
            let due = if s("dueDate").is_empty() { t.user_text.trim().to_owned() } else { s("dueDate") };
            return Ok(json!({"action": "schedule_reminder", "text": s("text"), "dueDate": due, "ack": "I will schedule that reminder now."}));
        }
        Ok(match action.as_str() {
            "schedule_reminder" => json!({"action": "schedule_reminder", "text": s("text"), "dueDate": s("dueDate"), "ack": if s("ack").is_empty() { "I will schedule that reminder now.".into() } else { s("ack") }}),
            "social_post_task" => json!({"action": "poko_news_scout_retry", "ack": s("ack")}),
            "oracle_monitor_task" if !self.prompts.oracle_monitor_enabled => json!({
                "action": "reply",
                "text": format!("Su {} il monitor SSH Oracle è disabilitato. Per Poko/Bot Hospital uso peer1.", self.prompts.host_label),
            }),
            "generate_image" | "web_crawl_task" | "cursor_task" | "graph_task" | "spreadsheet_update_task" | "poko_news_scout_retry" | "oracle_monitor_task" | "model_status_task" | "revive_qwen_task" | "discord_admin_task" => parsed.clone(),
            _ => json!({"action": "reply", "text": if s("text").is_empty() { content.trim().to_owned() } else { s("text") }}),
        })
    }

    /// `buildDirectPlan` for the intents the classifier can settle alone.
    fn direct_plan(c: &Classification) -> Option<Value> {
        Some(match c.intent.as_str() {
            "list_reminders" => json!({"action": "list_reminders", "scope": c.scope}),
            "schedule_reminder" => json!({"action": "schedule_reminder", "text": "", "dueDate": "", "ack": "Programmo il promemoria adesso."}),
            "model_status" => json!({"action": "model_status_task", "ack": "Controllo quale modello è caricato adesso."}),
            "revive_qwen" => json!({"action": "revive_qwen_task", "ack": "Riporto Qwen 3.8 sulla GPU adesso."}),
            "poko_news_scout_retry" | "social_post" => json!({"action": "poko_news_scout_retry", "ack": "Avvio il Bot Hospital su Poko (peer1) e riprovo lo scout news adesso."}),
            "spreadsheet_update" => json!({"action": "spreadsheet_update_task", "ack": "Controllo il Google Sheet e provo ad aggiornarlo adesso."}),
            "cursor_task" => json!({"action": "cursor_task", "ack": "Avvio Harness con Qwen locale su nezopt per questo task adesso."}),
            _ => return None,
        })
    }

    /// Route a turn: `(plan, classification)`.
    pub async fn route(&self, t: &TurnInput<'_>, extra_system: &str) -> Result<(Value, Classification), LlmError> {
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
            let casual = !looks_like_complex_request(t.user_text, t.memory) && !looks_like_vulnerable_support(t.user_text);
            let knowledge = if casual { String::new() } else { self.prompts.operator_brief.clone() };
            let memory = clip_memory_for_reply(t.memory, if casual { 1600 } else { 4000 });
            let extra = if casual {
                format!("Reply like a normal Telegram chat. One or two short sentences. No capability menus. {extra_system}")
            } else {
                extra_system.to_owned()
            };
            let inner = TurnInput { memory: &memory, ..*t };
            let text = self.generate_reply(&inner, extra.trim(), &knowledge, "reply").await?;
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
        assert_eq!(extract_json_object("```json\n{\"action\":\"reply\",\"text\":\"ciao\"}\n```").unwrap()["text"], "ciao");
        assert_eq!(extract_json_object("Sure: {\"a\": {\"b\": 1}} ok").unwrap()["a"]["b"], 1);
        assert!(extract_json_object("no json").is_none());
        assert_eq!(parse_plan_output("{\"action\":\"reply\",\"text\":\"Ciao, come st").unwrap()["text"], "Ciao, come st");
    }
}
