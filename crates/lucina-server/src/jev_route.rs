//! Jev (TypeSafe's decision model, crate `lucina_jev`) in everyday turns.
//!
//! - **Intent:** tier-0 regex rules are double-checked by Jev, which wins
//!   only when sure ([`OVERRIDE_MIN`]) and never into a side-effect intent.
//! - **Intent (tier 1):** when tier 0 has no rule for a message, Jev picks the
//!   intent, scope and the memory/tool flags in one call (the reply mode
//!   follows from the intent), in the
//!   same JSON shape the DeepSeek classifier returns, so the turn goes through
//!   the same guards ([`lucina_intent::tier0::from_model_json`], misroute
//!   checks, `should_use_full_plan`). A low-confidence answer asks DeepSeek
//!   too; a Jev failure (quota, timeout, breaker) falls back to DeepSeek alone.
//! - **Tools:** before the agent loop's first model call, Jev guesses the first
//!   tool the request needs (or none), and a confident guess becomes a short
//!   hint for the model. It never removes tools: a wrong guess costs nothing.
//! - **JEV-first** (`LUCINA_JEV_FIRST`): from [`JEV_FIRST_MIN`] up (per intent,
//!   `LUCINA_JEV_FIRST_INTENT_MIN`) Jev routes the turn before the tier-0 rules,
//!   except a deterministic route ([`deterministic_intent`]). Each decision is
//!   one `event="jev_first"` log line. `POST /api/secretary/jev/route` replays a
//!   text through the same path. The weekly check is `lucina-rs jev drift-report`
//!   (`docs/lucina-rust/JEV-ROUTING.md`); it only suggests new thresholds.

use std::sync::Arc;

use serde_json::{json, Value};

use lucina_jev::{Jev, Question};

use crate::planner::TurnInput;

/// Below this the intent answer is checked with DeepSeek (`LUCINA_JEV_INTENT_MIN`).
pub const INTENT_MIN: f64 = 0.6;
/// A first-tool guess at least this sure becomes a hint (`LUCINA_JEV_TOOL_MIN`).
pub const TOOL_HINT_MIN: f64 = 0.7;

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| (0.0..=1.0).contains(v))
        .unwrap_or(default)
}

/// The intents Jev may pick, with what each means. Tier-0-only intents (log
/// inspection, study focus) stay with their deterministic rules.
pub const INTENT_OPTIONS: &[(&str, &str)] = &[
    ("chat", "General conversation, questions, opinions, advice or explanations that Lucina answers herself; no tool needed. Travel, flights and trains are chat."),
    ("social_greeting", "Only a pure greeting (ciao, buongiorno, come va) with no request or question beyond it."),
    ("agent_status_check", "A ping asking whether Lucina is alive or online (ci sei?, sei attivo?, ping)."),
    ("explicit_memory_recall", "The user explicitly asks Lucina to recall a past decision, statement or conversation (ti ricordi, cosa avevamo deciso, l'altra volta)."),
    ("temporal_query", "A question about dates, times, deadlines or appointments that needs the current clock (a che ora, quanto manca, che giorno è)."),
    ("continue_current_topic", "A short follow-up that continues the immediately previous topic (e per la seconda parte?)."),
    ("new_topic", "The user explicitly changes subject."),
    ("list_reminders", "The user asks which reminders exist (today or all). Not travel, flights or trains."),
    ("calendar_event", "A NEW event for the owner's calendar, or a change to one: an appointment, meeting, session, lesson, exam, interview, visit or meal with someone, with a day and/or time (Mentor session 6 november at 12:30, lezione giovedì alle 10), or the day and time answering Lucina's question about such an event. Not when the user says remind me / ricordami."),
    ("schedule_reminder", "The user explicitly wants a NEW reminder (remind me, ricordami, avvisami, promemoria) at a time or date, or a timer with a relative duration (timer 20 minutes, tra mezz'ora). An appointment or meeting with a date is calendar_event. A question about time is not this."),
    ("generate_image", "The user asks for a picture, drawing or other visual deliverable."),
    ("graph_task", "The user asks for a graph or chart."),
    ("spreadsheet_update", "Log weight, a receipt or patrimony to the Google Sheet or Drive."),
    ("web_crawl", "A supplied URL or web page must be read to answer or act: bare links, this page, summarise or compare a page, or look something up on the web."),
    ("computer_task", "The user wants Lucina to do something on her own computer (Linux desktop with Chromium) or her Android phone: open a site or app, check the owner's Outlook or Gmail, click through pages, use an Android app, find flights or trains on a booking site, or order food on Wolt or Just Eat (she stops at the checkout)."),
    ("oracle_monitor", "CPU, RAM or status of the Oracle peer server (SSH)."),
    ("model_status", "The user asks which model or LLM Lucina is using."),
    ("revive_qwen", "The user asks to load or wake Qwen back onto the GPU."),
    ("poko_news_scout_retry", "Fix or retry the Poko news channel post or the Bot Hospital scout."),
    ("social_post", "Publish something to the Pokoin Telegram news channel."),
    ("cursor_task", "Give a coding task to a coding agent (Cursor, Codex, Claude Code) on a project."),
    ("agent_message", "The owner asks Lucina to pass a message, question or issue report on to another assistant: Claude (the Claude Code coordinator of his projects), Grok Bot, or Lucina Agents (tell Claude that…, report this to Claude, ask Grok…, post it in Lucina agents, use Lucina agents)."),
    ("discord_admin", "Administer the Discord server: channels, roles, members."),
    ("grocery.purchase", "The user reports groceries they bought."),
    ("grocery.consume", "The user reports eating or using up part of a grocery item."),
    ("grocery.finished", "The user says a grocery item is finished or empty."),
    ("grocery.shopping_list.add", "Add items to the shopping list."),
    ("grocery.shopping_list.remove", "Remove items from the shopping list."),
    ("grocery.shopping_list.query", "Ask what is on the shopping list."),
    ("grocery.offer.search", "Look for supermarket offers or deals."),
    ("grocery.expense.query", "Ask about grocery spending."),
    ("grocery.pantries.query", "Ask what is in the pantry or fridge."),
    ("grocery.price.query", "Ask the price of a grocery item."),
    ("grocery.analytics.query", "Ask for grocery statistics or trends."),
    ("unknown", "None of the above fits, or the message cannot be understood."),
];

pub(crate) fn intent_questions() -> Vec<(&'static str, Question)> {
    vec![
        (
            "intent",
            Question::choice(
                "Classify the user's latest message (the `message` field) for Lucina, a personal assistant. Use the recent turns only to understand follow-ups.",
                INTENT_OPTIONS.iter().copied(),
            ),
        ),
        (
            "scope",
            Question::choice(
                "If the user asks about reminders, do they mean only today's or all of them?",
                [("today", "Only today."), ("all", "All of them, or not about reminders.")],
            ),
        ),
        (
            "needs_memory",
            Question::noul("Does answering the latest message need Lucina's memory of the owner or of earlier conversations?"),
        ),
        (
            "needs_tools",
            Question::noul("Does answering the latest message need a tool or an action (reminders, shopping list, web, computer, images, coding agent), rather than words alone?"),
        ),
    ]
}

/// The intent as the DeepSeek classifier's JSON plus Jev's confidence in it,
/// or `None` when Jev is off or failed (the caller asks DeepSeek).
pub async fn classify_intent(jev: &Jev, t: &TurnInput<'_>) -> Option<(Value, f64)> {
    if !jev.available() {
        return None;
    }
    let recent: String = t.memory.chars().take(1500).collect();
    let state = json!({
        "platform": if t.platform.is_empty() { "unknown" } else { t.platform },
        "user": t.user_label,
        "recent_turns": recent,
        "message": t.user_text,
    });
    let d = jev.decide("intent", state, &intent_questions()).await.ok()?;
    intent_json(&d)
}

/// The intent decision as the DeepSeek classifier's JSON plus Jev's confidence in it (the reply mode follows
/// from the intent: a tool intent answered as "chat" would skip its tool; only a conversational intent may
/// still need one). A spoken turn's own Jev call (`chat::voice_fast`) answers the same questions and is
/// shared with the classifier through this.
pub fn intent_json(d: &lucina_jev::Decision) -> Option<(Value, f64)> {
    let (intent, confidence) = d.choice("intent")?;
    let mode = lucina_intent::tier0::default_reply_mode(intent);
    let v = json!({
        "intent": intent,
        "confidence": confidence,
        "reply_mode": mode,
        "scope": d.choice("scope").map(|c| c.0),
        "needs_memory": d.yes("needs_memory").is_some_and(|p| p >= 0.5),
        "needs_tools": mode != "chat" || d.yes("needs_tools").is_some_and(|p| p >= 0.5),
    });
    tracing::info!(target: "lucina_jev::route", intent, confidence, reply_mode = ?v["reply_mode"],
        ms = d.elapsed_ms, "jev intent");
    Some((v, confidence))
}

/// Jev overrides a broad tier-0 rule from this confidence up (`LUCINA_JEV_OVERRIDE_MIN`).
pub const OVERRIDE_MIN: f64 = 0.9;

pub fn override_min() -> f64 {
    env_f64("LUCINA_JEV_OVERRIDE_MIN", OVERRIDE_MIN)
}

/// Tier-0 answers Jev double-checks: every regex rule, except the
/// deterministic routes that parse their own arguments (the computer,
/// groceries, the log inspector, study focus).
pub fn checks_tier0(c: &lucina_intent::tier0::Classification) -> bool {
    !c.intent.starts_with("grocery.")
        && !matches!(
            c.intent.as_str(),
            "computer_task" | "agent_message" | "lucina_log_inspect" | "study_focus_reminders"
        )
        && lucina_jev::shared().available()
}

/// Intents with side effects that a Jev override never routes into: only
/// their own tier-0 rules (or the planner) reach them.
const NO_OVERRIDE_INTO: &[&str] = &[
    "social_post",
    "revive_qwen",
    "spreadsheet_update",
    "poko_news_scout_retry",
    "cursor_task",
    "discord_admin",
    "oracle_monitor",
];

/// A side-effect tier-0 rule that Jev doubts from this confidence up goes to
/// the full planner instead of running directly (`LUCINA_JEV_DOUBT_MIN`).
pub const DOUBT_MIN: f64 = 0.6;

/// Jev doubts a tier-0 rule enough that the planner should decide instead:
/// a side-effect rule (a post, reviving Qwen, a spreadsheet write, a coding
/// task…) Jev reads as something else, or a plain-chat rule (no tools) Jev
/// reads as a request for an action.
pub fn doubts(tier0: &lucina_intent::tier0::Classification, jev_intent: &str, confidence: f64) -> bool {
    if jev_intent == tier0.intent || confidence < env_f64("LUCINA_JEV_DOUBT_MIN", DOUBT_MIN) {
        return false;
    }
    let side_effect = NO_OVERRIDE_INTO.contains(&tier0.intent.as_str());
    let chat_but_action = tier0.reply_mode == "chat"
        && !tier0.needs_tools
        && lucina_intent::tier0::default_reply_mode(jev_intent) != "chat"
        && jev_intent != "unknown";
    side_effect || chat_but_action
}

/// Tier-0 intents that start work in the world (jobs, posts, reminders, writes).
pub const SIDE_EFFECT_INTENTS: &[&str] = &[
    "cursor_task",
    "agent_message",
    "computer_task",
    "schedule_reminder",
    "calendar_event",
    "study_focus_reminders",
    "social_post",
    "revive_qwen",
    "spreadsheet_update",
    "poko_news_scout_retry",
    "discord_admin",
    "oracle_monitor",
    "generate_image",
    "graph_task",
    "web_crawl",
];

/// On an MCP turn (an agent's prose, not the owner's words), a side-effect tier-0 rule that Jev
/// reads as anything else, at any confidence, gives way to plain chat: the cost of a wrong reply is a
/// sentence, the cost of a wrong job is a Harness run or a reminder nobody asked for.
pub fn mcp_falls_back_to_chat(tier0: &lucina_intent::tier0::Classification, jev_intent: &str) -> bool {
    SIDE_EFFECT_INTENTS.contains(&tier0.intent.as_str()) && jev_intent != tier0.intent
}

/// `LUCINA_JEV_FIRST` (off until calibrated): Jev routes every turn it is sure of, before the
/// tier-0 rules (owner's rule, 2026-10-28: "for confidence over 75%, we trust Jeff").
pub fn jev_first_enabled() -> bool {
    std::env::var("LUCINA_JEV_FIRST")
        .is_ok_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"))
}

/// JEV-first trusts Jev from this confidence up (`LUCINA_JEV_FIRST_MIN`).
pub const JEV_FIRST_MIN: f64 = 0.75;

/// The JEV-first threshold for `intent`: `LUCINA_JEV_FIRST_INTENT_MIN` (`"cursor_task=0.95,chat=0.7"`)
/// overrides `LUCINA_JEV_FIRST_MIN` per intent.
pub fn jev_first_min(intent: &str) -> f64 {
    jev_first_min_from(
        intent,
        std::env::var("LUCINA_JEV_FIRST_MIN").ok().as_deref(),
        std::env::var("LUCINA_JEV_FIRST_INTENT_MIN").ok().as_deref(),
    )
}

pub fn jev_first_min_from(intent: &str, global: Option<&str>, per_intent: Option<&str>) -> f64 {
    let valid = |v: &str| v.trim().parse::<f64>().ok().filter(|v| (0.0..=1.0).contains(v));
    per_intent
        .unwrap_or("")
        .split(',')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| k.trim() == intent)
        .and_then(|(_, v)| valid(v))
        .or_else(|| global.and_then(valid))
        .unwrap_or(JEV_FIRST_MIN)
}

/// Tier-0 routes that parse their own arguments and that Jev has no option for (her computer,
/// groceries, the log inspector, study focus): they keep precedence over JEV-first.
pub fn deterministic_rule(c: &lucina_intent::tier0::Classification) -> bool {
    deterministic_intent(&c.intent)
}

/// [`deterministic_rule`] by the rule's intent name (the drift report's replay).
pub fn deterministic_intent(intent: &str) -> bool {
    intent.starts_with("grocery.")
        || matches!(
            intent,
            "computer_task" | "agent_message" | "lucina_log_inspect" | "study_focus_reminders"
        )
}

/// Does JEV-first route this turn? `Err` names why not (for the drift log).
pub fn jev_first(
    jev: Option<(&str, f64)>,
    rule: Option<&lucina_intent::tier0::Classification>,
) -> Result<(), &'static str> {
    let Some((intent, confidence)) = jev else {
        return Err("jev_unavailable");
    };
    if rule.is_some_and(deterministic_rule) {
        return Err("deterministic_rule");
    }
    if intent.is_empty() || intent == "unknown" {
        return Err("jev_unknown");
    }
    if confidence < jev_first_min(intent) {
        return Err("below_threshold");
    }
    Ok(())
}

/// Jev's sure answer replaces the tier-0 rule's.
pub fn overrides(tier0_intent: &str, jev_intent: &str, confidence: f64) -> bool {
    jev_intent != tier0_intent
        && confidence >= override_min()
        && !NO_OVERRIDE_INTO.contains(&jev_intent)
}

/// Jev's intent answer is trusted from this confidence up.
pub fn intent_min() -> f64 {
    env_f64("LUCINA_JEV_INTENT_MIN", INTENT_MIN)
}

/// The agent loop's tool router: Jev's guess at the first tool.
pub struct JevToolRouter {
    pub jev: &'static Jev,
}

#[async_trait::async_trait]
impl lucina_tools::agent::ToolRouter for JevToolRouter {
    async fn first_tool(&self, user_text: &str, tools: &[(String, String)]) -> Option<String> {
        if !self.jev.available() || tools.len() + 1 > lucina_jev::MAX_OPTIONS {
            return None;
        }
        let mut options: Vec<(String, String)> = tools
            .iter()
            .map(|(name, desc)| (name.clone(), lucina_jev::clip(desc, 400)))
            .collect();
        options.push((
            "none".into(),
            "No tool: Lucina can answer with words alone.".into(),
        ));
        let q = [(
            "first_tool",
            Question::choice(
                "Lucina, a personal assistant, must handle the owner's message. Which tool should she call first?",
                options,
            ),
        )];
        let d = self
            .jev
            .decide("tools", json!({"message": user_text}), &q)
            .await
            .ok()?;
        let (tool, confidence) = d.choice("first_tool")?;
        tracing::info!(target: "lucina_jev::route", tool, confidence, ms = d.elapsed_ms, "jev first tool");
        (tool != "none" && confidence >= env_f64("LUCINA_JEV_TOOL_MIN", TOOL_HINT_MIN))
            .then(|| tool.to_owned())
    }
}

/// The router for the owner's tool turns (`None` when Jev is not configured).
pub fn tool_router() -> Option<Arc<dyn lucina_tools::agent::ToolRouter>> {
    let jev = lucina_jev::shared();
    jev.configured()
        .then(|| Arc::new(JevToolRouter { jev }) as Arc<dyn lucina_tools::agent::ToolRouter>)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_intent_option_is_one_lucina_knows() {
        for (name, _) in INTENT_OPTIONS {
            assert!(
                lucina_intent::tier0::INTENTS.contains(name),
                "{name} is not a tier-0 intent"
            );
        }
        for (name, q) in intent_questions() {
            if let Question::Choice { options, .. } = &q {
                assert!(options.len() <= lucina_jev::MAX_OPTIONS, "{name}");
            }
        }
    }

    #[test]
    fn overrides_need_confidence_and_never_reach_side_effect_intents() {
        assert!(overrides("social_post", "schedule_reminder", 0.95));
        assert!(!overrides("social_post", "schedule_reminder", 0.6));
        assert!(!overrides("chat", "social_post", 0.99));
        assert!(!overrides("chat", "cursor_task", 0.99));
        assert!(!overrides("chat", "chat", 0.99));
        let s = lucina_intent::heuristics::Settings::default();
        let post = lucina_intent::tier0::from_model_json(
            &json!({"intent": "social_post", "confidence": 1.0, "needs_tools": true}), "x", "", &s);
        assert!(doubts(&post, "schedule_reminder", 0.87));
        assert!(!doubts(&post, "social_post", 0.99));
        assert!(!doubts(&post, "schedule_reminder", 0.4));
        let chat = lucina_intent::tier0::from_model_json(
            &json!({"intent": "chat", "confidence": 0.95, "reply_mode": "chat"}), "x", "", &s);
        assert!(doubts(&chat, "computer_task", 0.6));
        assert!(!doubts(&chat, "social_greeting", 0.9), "both are conversational");
        assert!(!doubts(&chat, "unknown", 0.9));
    }

    #[test]
    fn jev_first_trusts_sure_answers_and_keeps_deterministic_routes() {
        let s = lucina_intent::heuristics::Settings::default();
        let rule = |intent: &str| lucina_intent::tier0::from_model_json(
            &json!({"intent": intent, "confidence": 1.0}), "x", "", &s);
        assert_eq!(jev_first_min_from("chat", None, None), 0.75);
        assert_eq!(jev_first_min_from("chat", Some("0.8"), None), 0.8);
        assert_eq!(jev_first_min_from("cursor_task", Some("0.8"), Some("cursor_task=0.95, chat=0.7")), 0.95);
        assert_eq!(jev_first_min_from("chat", Some("x"), Some("chat=2")), 0.75, "bad values are ignored");
        assert_eq!(jev_first(None, None), Err("jev_unavailable"));
        assert_eq!(jev_first(Some(("unknown", 0.99)), None), Err("jev_unknown"));
        assert_eq!(jev_first(Some(("chat", 0.99)), Some(&rule("computer_task"))), Err("deterministic_rule"));
        assert_eq!(jev_first(Some(("chat", 0.99)), Some(&rule("grocery.shopping_list.add"))), Err("deterministic_rule"));
        // The 2026-10-28 misroutes: Jev was unsure, so the rules (tightened) and consult mode still matter.
        assert_eq!(jev_first(Some(("unknown", 0.28)), Some(&rule("cursor_task"))), Err("jev_unknown"));
        assert_eq!(jev_first(Some(("computer_task", 0.49)), Some(&rule("schedule_reminder"))), Err("below_threshold"));
        assert_eq!(jev_first(Some(("chat", 0.8)), Some(&rule("schedule_reminder"))), Ok(()));
    }

    #[test]
    fn mcp_turns_fall_back_to_chat_when_jev_disagrees_with_a_side_effect_rule() {
        let s = lucina_intent::heuristics::Settings::default();
        let rule = |intent: &str| lucina_intent::tier0::from_model_json(
            &json!({"intent": intent, "confidence": 1.0, "needs_tools": true}), "x", "", &s);
        // The 2026-10-28 incident: tier 0 said cursor_task 0.9 (Jev: unknown 0.28) and
        // schedule_reminder 1.0 (Jev: computer_task 0.49).
        assert!(mcp_falls_back_to_chat(&rule("cursor_task"), "unknown"));
        assert!(mcp_falls_back_to_chat(&rule("schedule_reminder"), "computer_task"));
        assert!(!mcp_falls_back_to_chat(&rule("schedule_reminder"), "schedule_reminder"));
        assert!(!mcp_falls_back_to_chat(&rule("list_reminders"), "chat"), "reads stay");
        assert!(!mcp_falls_back_to_chat(&rule("chat"), "cursor_task"));
    }

    #[test]
    fn jev_answers_go_through_the_same_guards_as_deepseek() {
        let s = lucina_intent::heuristics::Settings::default();
        // Jev's JSON in the classifier's shape: the travel guard still turns list_reminders into chat.
        let v = json!({"intent": "list_reminders", "confidence": 0.9, "reply_mode": "direct",
            "scope": "all", "needs_memory": false, "needs_tools": true});
        let c = lucina_intent::tier0::from_model_json(&v, "a che ora parte il volo per CPH?", "", &s);
        assert_eq!(c.intent, "chat");
        let c = lucina_intent::tier0::from_model_json(&v, "che promemoria ho?", "", &s);
        assert_eq!(c.intent, "list_reminders");
        assert_eq!(c.reply_mode, "direct");
        assert!((c.confidence - 0.9).abs() < 1e-9);
    }
}
