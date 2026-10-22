//! `src/lucina-intent.js`: tier 0 (deterministic, no LLM), the direct
//! plans, `shouldUseFullPlan` and the misrouted-tool-intent guard.

use serde_json::{json, Value};

use crate::grocery::parse_grocery_intent;
use crate::heuristics::*;
use crate::re;

pub const INTENTS: &[&str] = &[
    "chat",
    "social_greeting",
    "agent_status_check",
    "explicit_memory_recall",
    "temporal_query",
    "continue_current_topic",
    "new_topic",
    "list_reminders",
    "schedule_reminder",
    "study_focus_reminders",
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
    "lucina_log_inspect",
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
    "discord_admin",
    "unknown",
];

/// Where a classification came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Tier0,
    Tier1,
    Fallback,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Classification {
    pub intent: String,
    pub confidence: f64,
    pub source: Source,
    pub needs_memory: bool,
    pub needs_tools: bool,
    pub reply_mode: String,
    pub scope: String,
}

impl Classification {
    fn t0(
        intent: &str,
        confidence: f64,
        needs_memory: bool,
        needs_tools: bool,
        reply_mode: &str,
    ) -> Self {
        Self {
            intent: intent.into(),
            confidence,
            source: Source::Tier0,
            needs_memory,
            needs_tools,
            reply_mode: reply_mode.into(),
            scope: "all".into(),
        }
    }

    pub fn tier0(&self) -> bool {
        self.source == Source::Tier0
    }

    /// `safeFallbackDecision` of the intent router (tier 1 failed).
    pub fn fallback() -> Self {
        Self {
            intent: "unknown".into(),
            confidence: 0.0,
            source: Source::Fallback,
            needs_memory: false,
            needs_tools: false,
            reply_mode: "chat".into(),
            scope: "all".into(),
        }
    }

    /// A tier-1 classifier answer that was not JSON.
    pub fn unparsed() -> Self {
        Self {
            intent: "unknown".into(),
            confidence: 0.0,
            source: Source::Tier1,
            needs_memory: true,
            needs_tools: false,
            reply_mode: "full_plan".into(),
            scope: "all".into(),
        }
    }
}

pub fn default_reply_mode(intent: &str) -> &'static str {
    match intent {
        "social_greeting"
        | "agent_status_check"
        | "explicit_memory_recall"
        | "temporal_query"
        | "continue_current_topic"
        | "new_topic"
        | "chat" => "chat",
        "list_reminders"
        | "oracle_monitor"
        | "model_status"
        | "revive_qwen"
        | "poko_news_scout_retry"
        | "lucina_log_inspect"
        | "study_focus_reminders"
        | "social_post"
        | "spreadsheet_update" => "direct",
        _ => "full_plan",
    }
}

/// `normalizeConfidence`.
pub fn normalize_confidence(v: &Value) -> f64 {
    if let Some(n) = v.as_f64() {
        return n.clamp(0.0, 1.0);
    }
    let text = match v {
        Value::String(s) => s.to_lowercase(),
        Value::Null => String::new(),
        other => other.to_string().to_lowercase(),
    };
    match text.as_str() {
        "high" => 0.92,
        "medium" => 0.78,
        "low" => 0.45,
        t => match t.trim().parse::<f64>() {
            Ok(n) if n.is_finite() => {
                if n > 1.0 {
                    (n / 100.0).min(1.0)
                } else {
                    n.clamp(0.0, 1.0)
                }
            }
            _ => 0.5,
        },
    }
}

/// Tier-1 JSON answer to a classification (`classifyLucinaIntentWithModel`
/// after parsing). `travel_text` is the message plus the memory given to
/// the classifier (Node checks both for travel words).
pub fn from_model_json(v: &Value, user_text: &str, memory: &str, s: &Settings) -> Classification {
    let mut intent = v["intent"]
        .as_str()
        .filter(|i| INTENTS.contains(i))
        .unwrap_or("unknown")
        .to_owned();
    if !s.oracle_monitor && intent == "oracle_monitor" {
        intent = "chat".into();
    }
    if intent == "list_reminders"
        && looks_like_travel_info(
            &[user_text, memory]
                .iter()
                .filter(|x| !x.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
        )
    {
        intent = "chat".into();
    }
    let reply_mode = v["reply_mode"]
        .as_str()
        .filter(|m| ["direct", "chat", "after_tool", "full_plan"].contains(m))
        .map(str::to_owned)
        .unwrap_or_else(|| default_reply_mode(&intent).into());
    Classification {
        confidence: normalize_confidence(&v["confidence"]),
        source: Source::Tier1,
        needs_memory: truthy(&v["needs_memory"]),
        needs_tools: truthy(&v["needs_tools"]),
        reply_mode,
        scope: if v["scope"] == "today" {
            "today"
        } else {
            "all"
        }
        .into(),
        intent,
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

const PURE_GREETING: &str = r"^(?:ciao|buongiorno|buonasera|buonanotte|hey+|hi+|hello+|hej+|ehi|salve|yo|halla|godmorgen|godaften)[\s,!.?🌀-🫿☀-➿]*(?:lucina|flar?e|hermes|pokoin)?[\s,!.?🌀-🫿☀-➿]*$";
const HOW_ARE_YOU: &str = r"^(?:(?:ciao|salve|buongiorno|buonasera|hey+|hi+|hello+|ehi)[\s,!.!]*)?(?:come va|come stai|come butta|come te la passi|tutto bene|tutto ok|tutto OK|how are you|how'?s it going|what'?s up|wass?up)[\s,!.?]*$";
const STATUS_PING: &str = r"^(?:(?:ciao|buongiorno|salve|hey+|hi+|hej)[\s,!.!]*)?(?:ci sei|sei attiv[oa]|sei vivo|sei sveglio|sei online|er du der|er du vågen|ping|pong|test|funzioni|funziona|stai funzionando|rispondi se ci sei|are you (?:there|alive|awake|up)|you (?:alive|up)|status check)[\s?.!]*$";
const TEMPORAL_QUERY: &str = r"\b(?:a che ora|che ora (?:era|erano|e'|è|e)|che giorno (?:e'|è|e|era)|quanto manca|quanti giorni|che data|quando (?:scade|parte|arriva)|scadenza|mi sembra di avere|il mio appuntamento|l'appuntamento|still (?:upcoming|ahead|relevant)|already (?:done|happened|past)|is it past)";
const CONTINUATION: &str = r"^(?:(?:ciao|hey|ok)[\s,!.!]*)?(?:e (?:poi|per|adesso|ora|quindi|invece|allora|quello)|quindi|proseguiamo|continuiamo|continua (?:da|dove|con|a )|vamos|andiamo avanti|ok e |e allora|e adesso|e per la|e per il|s[iì][\s,!.!]*però)";

/// `looksLikeDirectLogInspectRequest` (`src/lucina-log-inspect.js`).
pub fn looks_like_direct_log_inspect(text: &str) -> bool {
    if crate::notes::looks_like_store_note(text) || !looks_like_harness_or_log_task(text) {
        return false;
    }
    if re!(r"\b(fix|patch|repair|refactor|implement|commit|deploy|bug|codebase|\brepo\b|codex|cursor)\b", "i").test(text) {
        return false;
    }
    if re!(r"(harness|deepseek|dsh|mimir)", "i").test(text)
        && re!(r"(use|try|run|with|dig|prov)", "i").test(text)
    {
        return false;
    }
    re!(
        r"\b(?:logs?|logging|logfiles?|journalctl|docker logs|lucina log|hermes-lucina)\b",
        "i"
    )
    .test(text)
        || (re!(r"(stack trace|error message)", "i").test(text)
            && re!(r"(lucina|hermes|ollama|docker)", "i").test(text))
}

/// `classifyLucinaIntentTier0(userText, memoryContext)`.
pub fn classify_tier0(text: &str, memory: &str, s: &Settings) -> Option<Classification> {
    use Classification as C;
    let len = lucina_store::timeutil::utf16_len(text);
    if !looks_like_reminder_request(text) && explicit_recall(text) {
        return Some(C::t0("explicit_memory_recall", 0.9, true, false, "chat"));
    }
    if let Some(g) = parse_grocery_intent(text) {
        return Some(C::t0(g.intent, 0.97, true, true, "direct"));
    }
    if looks_like_generate_image(text) {
        return Some(C::t0("generate_image", 0.95, false, true, "full_plan"));
    }
    if looks_like_direct_log_inspect(text) {
        return Some(C::t0("lucina_log_inspect", 1.0, true, true, "direct"));
    }
    if looks_like_personal_state_query(text) {
        return Some(C::t0("explicit_memory_recall", 0.9, true, false, "chat"));
    }
    if looks_like_travel_info(text) {
        if looks_like_harness_or_log_task(text) || looks_like_code_project(text) {
            return Some(C::t0("cursor_task", 0.92, true, true, "direct"));
        }
        return Some(C::t0("chat", 1.0, true, false, "chat"));
    }
    if looks_like_reminder_list(text) {
        let mut c = C::t0("list_reminders", 1.0, false, false, "direct");
        c.scope = reminder_list_scope(text).into();
        return Some(c);
    }
    if looks_like_poko_status(text)
        || looks_like_poko_news_scout_retry(text)
        || looks_like_poko_news_scout_retry_followup(text, memory)
    {
        return Some(C::t0("poko_news_scout_retry", 1.0, false, true, "direct"));
    }
    if looks_like_explicit_web_search(text) {
        return Some(C::t0("web_crawl", 0.9, true, true, "full_plan"));
    }
    if looks_like_web_crawl(text) {
        return Some(C::t0("web_crawl", 0.92, true, true, "full_plan"));
    }
    if looks_like_site_browse_or_opinion(text) {
        return Some(C::t0("web_crawl", 0.9, true, true, "full_plan"));
    }
    if looks_like_study_focus_reminder(text) {
        return Some(C::t0("study_focus_reminders", 1.0, false, true, "direct"));
    }
    let trimmed = text.trim();
    if re!(STATUS_PING, "i").test(trimmed) {
        return Some(C::t0("agent_status_check", 0.95, false, false, "chat"));
    }
    if re!(PURE_GREETING, "iu").test(trimmed) || re!(HOW_ARE_YOU, "i").test(trimmed) {
        return Some(C::t0("social_greeting", 0.95, false, false, "chat"));
    }
    if len < 200 && re!(TEMPORAL_QUERY, "i").test(text) {
        return Some(C::t0("temporal_query", 0.8, true, false, "chat"));
    }
    if len < 120 && re!(CONTINUATION, "i").test(text) {
        return Some(C::t0("continue_current_topic", 0.8, true, false, "chat"));
    }
    if looks_like_reminder_request(text) {
        return Some(C::t0("schedule_reminder", 1.0, false, true, "direct"));
    }
    if looks_like_conversational_advice(text) || looks_like_education_or_study(text) {
        return Some(C::t0("chat", 1.0, true, false, "chat"));
    }
    if looks_like_revive_qwen(text) {
        return Some(C::t0("revive_qwen", 1.0, false, true, "direct"));
    }
    if looks_like_model_status(text) {
        return Some(C::t0("model_status", 1.0, false, true, "direct"));
    }
    if s.oracle_monitor && looks_like_oracle_monitor(text) {
        return Some(C::t0("oracle_monitor", 1.0, false, true, "direct"));
    }
    if looks_like_spreadsheet_write(text) {
        return Some(C::t0("spreadsheet_update", 1.0, true, true, "direct"));
    }
    if looks_like_graph(text) {
        return Some(C::t0("graph_task", 0.92, true, true, "full_plan"));
    }
    if re!(
        r"controlla.{0,50}(deploy|running|servizio|server|tunnel|stato)",
        "i"
    )
    .test(text)
        || looks_like_code_project(text)
        || looks_like_harness_or_log_task(text)
    {
        return Some(C::t0("cursor_task", 0.9, true, true, "direct"));
    }
    if looks_like_social_post(text) {
        return Some(C::t0("social_post", 1.0, false, true, "direct"));
    }
    if looks_like_vulnerable_support(text) {
        return Some(C::t0("chat", 1.0, true, false, "chat"));
    }
    if looks_like_actionable(text) {
        if re!(r"(cursor|codex|commit|deploy|refactor|codebase|\brepo\b|implementa|fix|harness|\blog\b|logs|diagnos|debug)", "i").test(text) {
            return Some(C::t0("cursor_task", 0.88, true, true, "full_plan"));
        }
        if re!(r"(graph|grafico|chart|plot)", "i").test(text) {
            return Some(C::t0("graph_task", 0.88, true, true, "full_plan"));
        }
        if re!(r"(generate|create|draw|immagine|foto|image|comfyui)", "i").test(text) {
            return Some(C::t0("generate_image", 0.88, false, true, "full_plan"));
        }
    }
    if s.fast_chat && !looks_like_complex(text, s) {
        return Some(C::t0("chat", 0.95, true, false, "chat"));
    }
    None
}

/// `isMisroutedTier0ToolIntent`.
pub fn is_misrouted(c: &Classification, text: &str, memory: &str) -> bool {
    match c.intent.as_str() {
        "oracle_monitor" => !looks_like_oracle_monitor(text),
        "model_status" => !looks_like_model_status(text),
        "revive_qwen" => !looks_like_revive_qwen(text),
        "spreadsheet_update" => !looks_like_spreadsheet_write(text),
        "social_post" => !looks_like_social_post(text),
        "poko_news_scout_retry" => {
            !(looks_like_poko_news_scout_retry(text)
                || looks_like_poko_news_scout_retry_followup(text, memory))
        }
        _ => false,
    }
}

/// `shouldUseFullPlan`.
pub fn should_use_full_plan(c: &Classification, text: &str, s: &Settings) -> bool {
    if c.intent == "lucina_log_inspect" && c.tier0() {
        return false;
    }
    if c.intent == "cursor_task" && c.tier0() && c.confidence >= 0.85 {
        return false;
    }
    if c.confidence < s.confidence_min
        || c.intent == "unknown"
        || c.reply_mode == "full_plan"
        || c.reply_mode == "after_tool"
        || (c.needs_tools && c.reply_mode != "direct")
    {
        return true;
    }
    s.aggressive_tool_routing
        && !text.is_empty()
        && looks_like_actionable(text)
        && (c.intent == "chat" || c.reply_mode == "chat")
}

/// `buildDirectPlan`.
pub fn direct_plan(c: &Classification, text: &str, memory: &str, s: &Settings) -> Option<Value> {
    Some(match c.intent.as_str() {
        "list_reminders" => {
            let scope = if c.scope.is_empty() {
                reminder_list_scope(text).to_owned()
            } else {
                c.scope.clone()
            };
            json!({"action": "list_reminders", "scope": scope})
        }
        "poko_news_scout_retry" => {
            json!({"action": "poko_news_scout_retry", "ack": "Avvio il Bot Hospital su Poko (peer1) e riprovo lo scout news adesso."})
        }
        "model_status" => {
            json!({"action": "model_status_task", "ack": "Controllo quale modello è caricato adesso."})
        }
        "revive_qwen" => {
            json!({"action": "revive_qwen_task", "ack": "Riporto Qwen 3.8 sulla GPU adesso."})
        }
        "oracle_monitor" => {
            if !s.oracle_monitor {
                return None;
            }
            json!({"action": "oracle_monitor_task", "ack": "Controllo subito CPU e memoria sulle istanze Oracle configurate."})
        }
        "spreadsheet_update" => {
            let prompt = [
                "Update the appropriate Google Drive spreadsheet according to this user request and any relevant memory rules.",
                "Before writing, inspect Drive spreadsheet candidates and choose the workbook/sheet that best matches the request domain, filenames, sheet names, and headers.",
                "Actually write the data, upload the workbook back to Drive, and verify the result before reporting success.",
                &format!("Request/context:\n{}", text.trim()),
            ]
            .join("\n\n");
            let both = [text, memory]
                .iter()
                .filter(|x| !x.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            json!({
                "action": "spreadsheet_update_task",
                "prompt": prompt,
                "spreadsheetName": default_spreadsheet_name(&both),
                "ack": "Controllo il Google Sheet e provo ad aggiornarlo adesso.",
            })
        }
        "social_post" => {
            json!({"action": "poko_news_scout_retry", "ack": "Chiedo a Poko sul server Oracle di pubblicare sul canale Pokoin News adesso."})
        }
        "schedule_reminder" => {
            json!({"action": "schedule_reminder", "text": "", "dueDate": "", "ack": "Programmo il promemoria adesso."})
        }
        "lucina_log_inspect" => {
            json!({"action": "lucina_log_inspect", "prompt": text.trim(), "ack": "Leggo i log di Lucina (docker + journalctl) adesso."})
        }
        "cursor_task" => {
            let prompt = [
                "DeepSeek Harness task for Hermes/Lucina on nezopt using local Qwen 3.8 27B.",
                "Do NOT use Codevira MCP. Do NOT ask what to do next — execute immediately.",
                "For log/diagnostic requests: run ONLY shell inspection:",
                "  docker logs hermes-lucina-nez --tail 200",
                "  journalctl -u docker -n 100 --no-pager",
                "Then summarize concrete findings (errors, lines, root cause). No filler.",
                &format!("User request:\n{}", text.trim()),
            ]
            .join("\n\n");
            json!({"action": "cursor_task", "prompt": prompt, "ack": "Avvio Harness con Qwen locale su nezopt per questo task adesso."})
        }
        "study_focus_reminders" => {
            json!({"action": "study_focus_reminders", "ack": "Programmo i promemoria studio (focus/dopamina) per la sessione di oggi."})
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> Settings {
        Settings {
            fast_chat: true,
            ..Settings::default()
        }
    }

    fn intent(t: &str) -> Option<String> {
        classify_tier0(t, "", &s()).map(|c| c.intent)
    }

    #[test]
    fn tier0_like_node() {
        assert_eq!(intent("ciao").as_deref(), Some("social_greeting"));
        assert_eq!(intent("Ciao Lucina!").as_deref(), Some("social_greeting"));
        assert_eq!(intent("come stai?").as_deref(), Some("social_greeting"));
        assert_eq!(intent("ci sei?").as_deref(), Some("agent_status_check"));
        assert_eq!(
            intent("ricordami alle 18:30 di chiamare la mamma").as_deref(),
            Some("schedule_reminder")
        );
        assert_eq!(
            intent("Timer 20 minutes").as_deref(),
            Some("schedule_reminder")
        );
        assert_eq!(
            intent("quali promemoria ho oggi?").as_deref(),
            Some("list_reminders")
        );
        assert_eq!(
            intent("aggiungi il latte alla lista della spesa").as_deref(),
            Some("grocery.shopping_list.add")
        );
        assert_eq!(
            intent("cosa c'è nella lista della spesa?").as_deref(),
            Some("grocery.shopping_list.query")
        );
        assert_eq!(
            intent("ti ricordi cosa avevamo deciso?").as_deref(),
            Some("explicit_memory_recall")
        );
        assert_eq!(intent("che bella giornata").as_deref(), Some("chat"));
        assert_eq!(
            intent("pubblica la news sul canale telegram").as_deref(),
            Some("social_post")
        );
        assert_eq!(
            intent("genera un'immagine di un drago").as_deref(),
            Some("generate_image")
        );
        assert_eq!(
            intent("controlla i log di lucina").as_deref(),
            Some("lucina_log_inspect")
        );
        assert_eq!(
            classify_tier0("che bella giornata", "", &Settings::default()),
            None,
            "without fast chat the model decides"
        );
    }

    #[test]
    fn plans_and_full_plan_rules() {
        let c = classify_tier0("quali promemoria ho oggi?", "", &s()).unwrap();
        assert_eq!(
            direct_plan(&c, "quali promemoria ho oggi?", "", &s()).unwrap(),
            json!({"action": "list_reminders", "scope": "today"})
        );
        assert!(!should_use_full_plan(&c, "quali promemoria ho oggi?", &s()));
        let r = classify_tier0("ricordami domani alle 9 di pagare", "", &s()).unwrap();
        assert!(!should_use_full_plan(
            &r,
            "ricordami domani alle 9 di pagare",
            &s()
        ));
        let img = classify_tier0("genera un'immagine di un drago", "", &s()).unwrap();
        assert!(should_use_full_plan(
            &img,
            "genera un'immagine di un drago",
            &s()
        ));
        assert_eq!(normalize_confidence(&json!("high")), 0.92);
        assert_eq!(normalize_confidence(&json!("85")), 0.85);
        assert_eq!(normalize_confidence(&json!(85)), 1.0);
    }
}
