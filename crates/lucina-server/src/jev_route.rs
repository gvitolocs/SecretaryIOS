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
//!   `LUCINA_JEV_FIRST_INTENT_MIN`) Jev routes the turn before the tier-0 rules. A deterministic
//!   route ([`deterministic_intent`]) keeps the turn against an answer Jev is not sure of, has no
//!   option for, or that would start something ([`acts`]); a sure question of Jev's own intents wins
//!   over it from [`JEV_OVER_RULE_MIN`] up (`LUCINA_JEV_OVER_RULE`, `LUCINA_JEV_OVER_RULE_MIN`).
//!   Each decision is one `event="jev_first"` log line, which says where its text came from
//!   (`source`: `"turn"` of the owner's, `"replay"` from the replay endpoint below) — the drift report
//!   counts live traffic from a turn alone.
//!   `POST /api/secretary/jev/route` replays a
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

/// A threshold as an environment value: a number in 0..=1, `default` for anything else (a bad
/// value never routes).
fn threshold(v: Option<&str>, default: f64) -> f64 {
    v.and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| (0.0..=1.0).contains(v))
        .unwrap_or(default)
}

/// An `LUCINA_*` switch value: off only for `0`, `false`, `off` or `no` (case-insensitive);
/// anything else, unset included, is on.
pub(crate) fn switch_off(v: Option<&str>) -> bool {
    v.is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        )
    })
}

fn env_f64(key: &str, default: f64) -> f64 {
    threshold(std::env::var(key).ok().as_deref(), default)
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
    ("flaw_report", "The owner reports a small flaw he noticed in one of his own apps (Lucina's web app, Mac app, Android or iPhone app, or Juno): something that looks or behaves wrong, such as emoji shown as boxes, lost line breaks, a card or button still active after it was used, or a status that differs between clients. Not a question about how to use something, and not a request to pass a message on."),
    ("agent_task", "The owner asks Lucina to carry out an action on one of his own devices or machines that she has no tool of her own for: start, open, switch on or copy something on his Nintendo DSi, a PC or server of his (nespc, nezopt), his laptop or another device, or get a game, file or program onto one of them. Not a job for her own sandbox computer or Android phone (computer_task), not a coding task on a project (cursor_task), not passing a message on (agent_message), and not a question about how to do it."),
    ("parcel_track", "The owner gives a parcel or shipment tracking number to follow (track this RR123456789SE, traccia questo pacco, spor denne pakke), asks where a parcel or delivery of his is, or asks which parcels are being tracked. Not a food order, a flight or a train."),
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

/// Jev's state for the intent questions: facts only. `device` and `device_connected` join it when the
/// owner's turn comes from a device we know (`crate::device`), and are absent otherwise.
pub(crate) fn intent_state(t: &TurnInput<'_>, recent: &str) -> Value {
    crate::device::with_device(
        json!({
            "platform": if t.platform.is_empty() { "unknown" } else { t.platform },
            "user": t.user_label,
            "recent_turns": recent,
            "message": t.user_text,
        }),
        t.device,
    )
}

/// The intent as the DeepSeek classifier's JSON plus Jev's confidence in it,
/// or `None` when Jev is off or failed (the caller asks DeepSeek).
pub async fn classify_intent(jev: &Jev, t: &TurnInput<'_>) -> Option<(Value, f64)> {
    if !jev.available() {
        return None;
    }
    let recent: String = t.memory.chars().take(1500).collect();
    let state = intent_state(t, &recent);
    let d = jev
        .decide("intent", state, &intent_questions())
        .await
        .ok()?;
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
        "model": d.model,
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
            "computer_task"
                | "agent_message"
                | "flaw_report"
                | "lucina_log_inspect"
                | "study_focus_reminders"
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
pub fn doubts(
    tier0: &lucina_intent::tier0::Classification,
    jev_intent: &str,
    confidence: f64,
) -> bool {
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

/// Tier-0 intents that start work in the world (jobs, posts, reminders, writes). Not the whole
/// answer to "does this intent act?": the grocery writes join it in [`acts`].
pub const SIDE_EFFECT_INTENTS: &[&str] = &[
    "cursor_task",
    "agent_message",
    "flaw_report",
    "parcel_track",
    "computer_task",
    "agent_task",
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

/// Does running `intent` change something in the world, rather than answer a question? A sure Jev
/// answer that acts never takes the turn from a deterministic rule ([`jev_beats_rule_from`]): some
/// tier-0 routes run their handler straight off the intent string, before any planner or guard
/// could weigh a wrong answer.
pub fn acts(intent: &str) -> bool {
    SIDE_EFFECT_INTENTS.contains(&intent)
        || matches!(
            intent,
            "grocery.purchase"
                | "grocery.consume"
                | "grocery.finished"
                | "grocery.shopping_list.add"
                | "grocery.shopping_list.remove"
        )
}

/// On an MCP turn (an agent's prose, not the owner's words), a side-effect tier-0 rule that Jev
/// reads as anything else, at any confidence, gives way to plain chat: the cost of a wrong reply is a
/// sentence, the cost of a wrong job is a Harness run or a reminder nobody asked for.
pub fn mcp_falls_back_to_chat(
    tier0: &lucina_intent::tier0::Classification,
    jev_intent: &str,
) -> bool {
    SIDE_EFFECT_INTENTS.contains(&tier0.intent.as_str()) && jev_intent != tier0.intent
}

/// An MCP turn's delegation regex hit runs only from this P(yes) up ([`mcp_delegation`]).
pub const MCP_DELEGATION_MIN: f64 = 0.75;

/// Jev's state for [`mcp_delegation`]: who sent it and how the message opens. Facts only, short:
/// the opening says whether it asks for work or reports something.
pub fn mcp_delegation_state(caller: &str, message: &str) -> Value {
    json!({
        "sender": caller,
        "message_start": message.trim().chars().take(400).collect::<String>(),
    })
}

/// On an MCP turn, the owner's delegation regexes ("research …", "ask Grok …") also match an agent's
/// relayed report (2026-11-01: ChatGPT's "Research follow-up requested by Giuseppe on …, please
/// retain this" became a research job that failed). The hit runs only when Jev reads the message as a
/// request to get work done by another AI agent; unsure, down or over quota → plain chat.
pub async fn mcp_delegation(jev: &Jev, caller: &str, message: &str) -> (bool, Option<f64>) {
    if !jev.available() {
        return (false, None);
    }
    let q = Question::noul_with(
        "Does the message ask Lucina to get a task done now by another AI agent (Grok, Claude, ChatGPT, Cursor) or to run a web research job, and wait for its answer?",
        "A request for work, e.g. 'Ask Grok what is trending on X about Rust', 'Do some research on flights to Rome next week', 'Have Claude review this plan'.",
        "Not one: a report, an update, an idea or findings shared with her, or a request to remember, acknowledge or answer it herself.",
    );
    let p = match jev
        .decide(
            "mcp_delegation",
            mcp_delegation_state(caller, message),
            &[("delegate", q)],
        )
        .await
    {
        Ok(d) => d.yes("delegate"),
        Err(_) => None,
    };
    (p.is_some_and(|p| p >= MCP_DELEGATION_MIN), p)
}

/// `LUCINA_JEV_FIRST` (off until calibrated): Jev routes every turn it is sure of, before the
/// tier-0 rules (owner's rule, 2026-10-28: "for confidence over 75%, we trust Jeff").
pub fn jev_first_enabled() -> bool {
    std::env::var("LUCINA_JEV_FIRST").is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        )
    })
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
    let valid = |v: &str| {
        v.trim()
            .parse::<f64>()
            .ok()
            .filter(|v| (0.0..=1.0).contains(v))
    };
    per_intent
        .unwrap_or("")
        .split(',')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| k.trim() == intent)
        .and_then(|(_, v)| valid(v))
        .or_else(|| global.and_then(valid))
        .unwrap_or(JEV_FIRST_MIN)
}

/// Tier-0 routes that parse their own arguments (her computer, groceries, the log inspector, study
/// focus). They keep the turn against an answer Jev is unsure of, has no option for, or that acts;
/// a sure question of one of Jev's own intents wins over them ([`jev_beats_rule_from`]).
pub fn deterministic_rule(c: &lucina_intent::tier0::Classification) -> bool {
    deterministic_intent(&c.intent)
}

/// A turn Jev was not sure of, or could not answer, with JEV-first on: only a deterministic route
/// still runs. No other regex rule and no model call picks an intent, so with none the turn is
/// either the JSON planner or plain chat, whichever way [`crate::context::unsure_plan`] falls out.
pub fn rule_when_jev_unsure(
    rule: Option<lucina_intent::tier0::Classification>,
) -> Option<lucina_intent::tier0::Classification> {
    rule.filter(deterministic_rule)
}

/// [`deterministic_rule`] by the rule's intent name (the drift report's replay).
pub fn deterministic_intent(intent: &str) -> bool {
    intent.starts_with("grocery.")
        || matches!(
            intent,
            "computer_task" | "agent_message" | "lucina_log_inspect" | "study_focus_reminders"
        )
}

/// A Jev answer that names one of its own intents routes the turn over a deterministic keyword
/// rule from this confidence up (`LUCINA_JEV_OVER_RULE_MIN`), whatever JEV-first's own threshold
/// for it is.
pub const JEV_OVER_RULE_MIN: f64 = 0.90;

/// Does Jev's answer `intent` at `confidence` route the turn over the deterministic rule
/// `rule_intent`? It must be an intent Jev itself offers ([`INTENT_OPTIONS`]), neither `chat` nor
/// the rule's own, one that only answers ([`acts`]), and sure past both [`jev_first_min`] for it
/// and [`JEV_OVER_RULE_MIN`] — `LUCINA_JEV_OVER_RULE` off restores the rule's precedence.
/// Pure: the environment values are the caller's, like [`jev_first_min_from`].
pub fn jev_beats_rule_from(
    intent: &str,
    confidence: f64,
    rule_intent: &str,
    over_rule: Option<&str>,
    over_rule_min: Option<&str>,
    first_min: Option<&str>,
    first_intent_min: Option<&str>,
) -> bool {
    !switch_off(over_rule)
        && !intent.is_empty()
        && intent != "unknown"
        && intent != "chat"
        && intent != rule_intent
        && !acts(intent)
        && INTENT_OPTIONS.iter().any(|(n, _)| *n == intent)
        && confidence
            >= jev_first_min_from(intent, first_min, first_intent_min)
                .max(threshold(over_rule_min, JEV_OVER_RULE_MIN))
}

/// Does JEV-first route this turn? `Err` names why not (for the drift log).
pub fn jev_first(
    jev: Option<(&str, f64)>,
    rule: Option<&lucina_intent::tier0::Classification>,
) -> Result<(), &'static str> {
    jev_first_from(
        jev,
        rule,
        std::env::var("LUCINA_JEV_FIRST_MIN").ok().as_deref(),
        std::env::var("LUCINA_JEV_FIRST_INTENT_MIN").ok().as_deref(),
        std::env::var("LUCINA_JEV_OVER_RULE").ok().as_deref(),
        std::env::var("LUCINA_JEV_OVER_RULE_MIN").ok().as_deref(),
    )
}

/// [`jev_first`] with the environment values as arguments, like [`jev_first_min_from`].
pub fn jev_first_from(
    jev: Option<(&str, f64)>,
    rule: Option<&lucina_intent::tier0::Classification>,
    first_min: Option<&str>,
    first_intent_min: Option<&str>,
    over_rule: Option<&str>,
    over_rule_min: Option<&str>,
) -> Result<(), &'static str> {
    jev_first_rule_from(
        jev,
        rule.map(|r| r.intent.as_str()),
        first_min,
        first_intent_min,
        over_rule,
        over_rule_min,
    )
}

/// [`jev_first_from`] with the tier-0 rule as its intent name, the way [`deterministic_intent`] is
/// the rule's own test by name. The decision is the same one - the rule enters it only through
/// whether it is deterministic and what it is called - so the drift report's replay takes it from the
/// response's `tier0` string without rebuilding a classification it has no text for.
pub fn jev_first_rule_from(
    jev: Option<(&str, f64)>,
    rule: Option<&str>,
    first_min: Option<&str>,
    first_intent_min: Option<&str>,
    over_rule: Option<&str>,
    over_rule_min: Option<&str>,
) -> Result<(), &'static str> {
    let Some((intent, confidence)) = jev else {
        return Err("jev_unavailable");
    };
    if let Some(r) = rule.filter(|r| deterministic_intent(r)) {
        if !jev_beats_rule_from(
            intent,
            confidence,
            r,
            over_rule,
            over_rule_min,
            first_min,
            first_intent_min,
        ) {
            return Err("deterministic_rule");
        }
    }
    if intent.is_empty() || intent == "unknown" {
        return Err("jev_unknown");
    }
    if confidence < jev_first_min_from(intent, first_min, first_intent_min) {
        return Err("below_threshold");
    }
    Ok(())
}

/// The one `event="jev_first"` line of a turn JEV-first routed. `source` is the caller's word for
/// what the text was, passed the way the `jev_yesno` line's is (`"turn"`, `"replay"`):
/// [`crate::jev_drift`] counts live traffic from a turn alone.
pub fn log_jev_first_used(
    source: &'static str,
    intent: &str,
    confidence: f64,
    tier0: Option<&str>,
    overrode_rule: &str,
    guard: &str,
) {
    tracing::info!(target: "lucina_jev::route", event = "jev_first", source, used = true,
        intent = %intent, confidence, tier0 = ?tier0, overrode_rule = %overrode_rule, guard,
        "jev_first");
}

/// The one `event="jev_first"` line of a turn JEV-first did not route; `reason` is [`jev_first`]'s.
pub fn log_jev_first_fallback(
    source: &'static str,
    reason: &'static str,
    jev: Option<(&str, f64)>,
    tier0: Option<&str>,
) {
    tracing::info!(target: "lucina_jev::route", event = "jev_first", source, used = false, reason,
        intent = ?jev.map(|p| p.0), confidence = ?jev.map(|p| p.1), tier0 = ?tier0,
        "jev_first fallback");
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

/// Jev's state for a first-tool question: `{"message", ...facts}`. `facts` is an object of facts the
/// caller knows (the device, the previous turn's calls); anything else is ignored.
pub fn tool_state(user_text: &str, facts: Value) -> Value {
    let mut state = serde_json::Map::new();
    state.insert("message".into(), Value::from(user_text));
    if let Value::Object(facts) = facts {
        for (k, v) in facts {
            if k != "message" {
                state.insert(k, v);
            }
        }
    }
    Value::Object(state)
}

/// Jev's top first tool with its confidence, unthresholded (`"none"` included).
#[async_trait::async_trait]
pub trait TopTool: Send + Sync {
    /// `facts` join the message in Jev's state ([`tool_state`]); `None`: Jev is off or failed.
    async fn first_tool_with(
        &self,
        user_text: &str,
        facts: Value,
        tools: &[(String, String)],
    ) -> Option<(String, f64)>;
}

/// The first-tool hint threshold (`LUCINA_JEV_TOOL_MIN`).
pub fn tool_min() -> f64 {
    env_f64("LUCINA_JEV_TOOL_MIN", TOOL_HINT_MIN)
}

#[async_trait::async_trait]
impl TopTool for JevToolRouter {
    async fn first_tool_with(
        &self,
        user_text: &str,
        facts: Value,
        tools: &[(String, String)],
    ) -> Option<(String, f64)> {
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
            .decide("tools", tool_state(user_text, facts), &q)
            .await
            .ok()?;
        let (tool, confidence) = d.choice("first_tool")?;
        tracing::info!(target: "lucina_jev::route", tool, confidence, ms = d.elapsed_ms, "jev first tool");
        Some((tool.to_owned(), confidence))
    }
}

#[async_trait::async_trait]
impl lucina_tools::agent::ToolRouter for JevToolRouter {
    async fn first_tool(&self, user_text: &str, tools: &[(String, String)]) -> Option<String> {
        let (tool, confidence) = self.first_tool_with(user_text, json!({}), tools).await?;
        (tool != "none" && confidence >= tool_min()).then_some(tool)
    }
}

/// Jev's top-tool router for turns that pass facts (`None` when Jev is not configured).
pub fn top_tool_router() -> Option<Arc<dyn TopTool>> {
    let jev = lucina_jev::shared();
    jev.configured()
        .then(|| Arc::new(JevToolRouter { jev }) as Arc<dyn TopTool>)
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
    fn tool_state_is_the_message_and_the_facts() {
        assert_eq!(tool_state("go", json!({})), json!({"message": "go"}));
        assert_eq!(
            tool_state("go", json!({"device": "d", "message": "not this"})),
            json!({"message": "go", "device": "d"})
        );
        assert_eq!(tool_state("go", json!("x")), json!({"message": "go"}));
    }

    #[test]
    fn mcp_delegation_state_is_short_facts_only() {
        let long = format!(
            "Research follow-up requested by Giuseppe on {}",
            "x".repeat(2000)
        );
        let s = mcp_delegation_state("chatgpt", &format!("  {long}"));
        assert_eq!(s.as_object().unwrap().len(), 2);
        assert_eq!(s["sender"], "chatgpt");
        let start = s["message_start"].as_str().unwrap();
        assert_eq!(start.chars().count(), 400);
        assert!(start.starts_with("Research follow-up"));
    }

    #[tokio::test]
    async fn mcp_delegation_without_jev_is_plain_chat() {
        let jev = Jev::new(lucina_jev::Config {
            url: "http://127.0.0.1:9/v1/systemone".into(),
            model: "jev-1.14.0".into(),
            key: None,
            timeout: std::time::Duration::from_secs(1),
            enabled: true,
        });
        assert_eq!(
            mcp_delegation(&jev, "chatgpt", "Ask Grok what is trending").await,
            (false, None)
        );
    }

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
    fn jev_first_routes_agent_task_and_no_tier0_rule_can() {
        assert_eq!(jev_first(Some(("agent_task", 0.8)), None), Ok(()));
        assert_eq!(
            jev_first(Some(("agent_task", 0.5)), None),
            Err("below_threshold")
        );
        assert!(SIDE_EFFECT_INTENTS.contains(&"agent_task"));
        assert_eq!(
            lucina_intent::tier0::default_reply_mode("agent_task"),
            "direct"
        );
        assert!(INTENT_OPTIONS.iter().any(|(n, _)| *n == "agent_task"));
        // No regex rule names it: the intent table and its reply mode are the only places.
        let source = include_str!("../../lucina-intent/src/tier0.rs");
        assert_eq!(
            source.matches("\"agent_task\"").count(),
            2,
            "only the intent list and its reply mode"
        );
        let s = lucina_intent::heuristics::Settings::default();
        for m in [
            "Accendi i Anespici e passami Pokémon Diamante su questo DSi",
            "turn on nespc and put Pokémon Diamond on this DSi",
            "tænd for min pc og læg et spil på min DSi",
            "avvia il gioco sul DSi",
            "copy the ROM from nespc to the DSi",
            "open Safari on my Mac",
            "start the game on my laptop",
        ] {
            let rule = lucina_intent::tier0::classify_tier0(m, "", &s);
            assert!(rule.is_none_or(|c| c.intent != "agent_task"), "{m}");
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
            &json!({"intent": "social_post", "confidence": 1.0, "needs_tools": true}),
            "x",
            "",
            &s,
        );
        assert!(doubts(&post, "schedule_reminder", 0.87));
        assert!(!doubts(&post, "social_post", 0.99));
        assert!(!doubts(&post, "schedule_reminder", 0.4));
        let chat = lucina_intent::tier0::from_model_json(
            &json!({"intent": "chat", "confidence": 0.95, "reply_mode": "chat"}),
            "x",
            "",
            &s,
        );
        assert!(doubts(&chat, "computer_task", 0.6));
        assert!(
            !doubts(&chat, "social_greeting", 0.9),
            "both are conversational"
        );
        assert!(!doubts(&chat, "unknown", 0.9));
    }

    #[test]
    fn jev_first_trusts_sure_answers_and_keeps_deterministic_routes() {
        let s = lucina_intent::heuristics::Settings::default();
        let rule = |intent: &str| {
            lucina_intent::tier0::from_model_json(
                &json!({"intent": intent, "confidence": 1.0}),
                "x",
                "",
                &s,
            )
        };
        assert_eq!(jev_first_min_from("chat", None, None), 0.75);
        assert_eq!(jev_first_min_from("chat", Some("0.8"), None), 0.8);
        assert_eq!(
            jev_first_min_from(
                "cursor_task",
                Some("0.8"),
                Some("cursor_task=0.95, chat=0.7")
            ),
            0.95
        );
        assert_eq!(
            jev_first_min_from("chat", Some("x"), Some("chat=2")),
            0.75,
            "bad values are ignored"
        );
        assert_eq!(jev_first(None, None), Err("jev_unavailable"));
        assert_eq!(jev_first(Some(("unknown", 0.99)), None), Err("jev_unknown"));
        assert_eq!(
            jev_first(Some(("chat", 0.99)), Some(&rule("computer_task"))),
            Err("deterministic_rule")
        );
        assert_eq!(
            jev_first(
                Some(("chat", 0.99)),
                Some(&rule("grocery.shopping_list.add"))
            ),
            Err("deterministic_rule")
        );
        // The 2026-10-28 misroutes: Jev was unsure, so the rules (tightened) and consult mode still matter.
        assert_eq!(
            jev_first(Some(("unknown", 0.28)), Some(&rule("cursor_task"))),
            Err("jev_unknown")
        );
        assert_eq!(
            jev_first(
                Some(("computer_task", 0.49)),
                Some(&rule("schedule_reminder"))
            ),
            Err("below_threshold")
        );
        assert_eq!(
            jev_first(Some(("chat", 0.8)), Some(&rule("schedule_reminder"))),
            Ok(())
        );
    }

    /// JEV-first's decision with every environment value at its default: no test of the precedence
    /// reads the process environment.
    fn first(
        jev: Option<(&str, f64)>,
        rule: Option<&lucina_intent::tier0::Classification>,
    ) -> Result<(), &'static str> {
        jev_first_from(jev, rule, None, None, None, None)
    }

    /// TURN-01: a sure answer of one of Jev's own intents routes the turn over a deterministic
    /// keyword rule that disagrees with it; anything else leaves the rule in charge.
    #[test]
    fn a_sure_jev_answer_beats_a_deterministic_rule() {
        let s = lucina_intent::heuristics::Settings::default();
        let rule = |intent: &str| {
            lucina_intent::tier0::from_model_json(
                &json!({"intent": intent, "confidence": 1.0}),
                "x",
                "",
                &s,
            )
        };
        // The live case: the shopping list asked about, the grocery spending rule matched.
        assert_eq!(
            first(
                Some(("grocery.shopping_list.query", 0.99)),
                Some(&rule("grocery.expense.query"))
            ),
            Ok(())
        );
        // Below JEV_OVER_RULE_MIN the rule keeps the turn.
        assert_eq!(
            first(
                Some(("grocery.shopping_list.query", 0.89)),
                Some(&rule("grocery.expense.query"))
            ),
            Err("deterministic_rule")
        );
        assert_eq!(
            first(
                Some(("unknown", 0.99)),
                Some(&rule("grocery.expense.query"))
            ),
            Err("deterministic_rule")
        );
        // An intent Jev has no option for stays with its own rule.
        assert!(INTENT_OPTIONS
            .iter()
            .all(|(n, _)| *n != "lucina_log_inspect"));
        assert_eq!(
            first(
                Some(("lucina_log_inspect", 0.99)),
                Some(&rule("computer_task"))
            ),
            Err("deterministic_rule")
        );
        // The same intent on both sides: unchanged, the rule keeps the turn and runs its own
        // classification, the one that parses the message's arguments.
        assert_eq!(
            first(
                Some(("grocery.expense.query", 0.99)),
                Some(&rule("grocery.expense.query"))
            ),
            Err("deterministic_rule")
        );
        assert_eq!(
            rule_when_jev_unsure(Some(rule("grocery.expense.query")))
                .map(|c| c.intent)
                .as_deref(),
            Some("grocery.expense.query")
        );
    }

    /// TURN-01: only a question wins over a deterministic rule. An intent that starts something runs
    /// the rule's own parsing of the message, as it did before the override existed: a few tier-0
    /// routes act on the intent string alone, so a wrong-but-sure Jev answer would act.
    #[test]
    fn an_acting_jev_answer_never_beats_a_deterministic_rule() {
        let s = lucina_intent::heuristics::Settings::default();
        let rule = |intent: &str| {
            lucina_intent::tier0::from_model_json(
                &json!({"intent": intent, "confidence": 1.0}),
                "x",
                "",
                &s,
            )
        };
        for name in SIDE_EFFECT_INTENTS {
            assert!(acts(name), "{name} acts and does not say so");
        }
        for intent in [
            "agent_task",
            "agent_message",
            "flaw_report",
            "parcel_track",
            "computer_task",
            "schedule_reminder",
            "calendar_event",
        ] {
            assert!(acts(intent), "{intent} is not known to act");
            // Sure, an option of Jev's own, and not the rule's intent: `acts` alone keeps the rule.
            assert!(
                INTENT_OPTIONS.iter().any(|(n, _)| *n == intent),
                "{intent} is not Jev's own option: the case would pass on the table alone"
            );
            assert_eq!(
                first(
                    Some((intent, 0.99)),
                    Some(&rule("grocery.shopping_list.add"))
                ),
                Err("deterministic_rule"),
                "{intent}"
            );
        }
        // The grocery writes act without being in `SIDE_EFFECT_INTENTS`; the fifth one,
        // `grocery.shopping_list.add`, is the rule of the cases above and keeps its own turn there.
        for intent in [
            "grocery.purchase",
            "grocery.consume",
            "grocery.finished",
            "grocery.shopping_list.remove",
        ] {
            assert!(acts(intent), "{intent} is not known to act");
            assert!(!SIDE_EFFECT_INTENTS.contains(&intent), "{intent}");
        }
        // The list's own write against the rule of a question about spending: the write keeps it,
        // and a question about the list still wins (`a_sure_jev_answer_beats_a_deterministic_rule`).
        assert_eq!(
            first(
                Some(("grocery.shopping_list.add", 0.99)),
                Some(&rule("grocery.expense.query"))
            ),
            Err("deterministic_rule")
        );
        // A question about the same thing wins: `list_reminders` reads, it starts nothing.
        assert!(!acts("list_reminders"));
        assert_eq!(
            first(
                Some(("list_reminders", 0.95)),
                Some(&rule("study_focus_reminders"))
            ),
            Ok(())
        );
    }

    /// The same with the environment's values as arguments: the switch back to the rule's
    /// precedence, and `LUCINA_JEV_OVER_RULE_MIN`.
    #[test]
    fn the_over_rule_precedence_switch_and_threshold_are_the_environment_s() {
        let s = lucina_intent::heuristics::Settings::default();
        let rule = |intent: &str| {
            lucina_intent::tier0::from_model_json(
                &json!({"intent": intent, "confidence": 1.0}),
                "x",
                "",
                &s,
            )
        };
        let spending = rule("grocery.expense.query");
        let jev = Some(("grocery.shopping_list.query", 0.99));
        for switch in ["0", "false", "off", "no", "OFF", " No "] {
            assert_eq!(
                jev_first_from(jev, Some(&spending), None, None, Some(switch), None),
                Err("deterministic_rule"),
                "{switch}"
            );
        }
        for switch in ["1", "true", "on", "yes", "", "x"] {
            assert_eq!(
                jev_first_from(jev, Some(&spending), None, None, Some(switch), None),
                Ok(()),
                "{switch}"
            );
        }
        assert_eq!(
            jev_first_from(jev, Some(&spending), None, None, None, None),
            Ok(()),
            "default: on"
        );
        // The threshold: 0.93 beats the rule only at or above the floor it must clear.
        let unsure = Some(("grocery.shopping_list.query", 0.93));
        assert_eq!(
            jev_first_from(unsure, Some(&spending), None, None, None, Some("0.95")),
            Err("deterministic_rule")
        );
        assert_eq!(
            jev_first_from(unsure, Some(&spending), None, None, None, Some("x")),
            Ok(()),
            "a bad value is ignored: the default 0.90 still lets 0.93 through"
        );
        // JEV-first's own threshold for the intent is a floor too, never a ceiling.
        assert_eq!(
            jev_first_from(unsure, Some(&spending), Some("0.95"), None, None, None),
            Err("deterministic_rule")
        );
    }

    /// TURN-11: deciding by the rule's name is the same decision the turn path makes, on every shape
    /// a replay response can carry.
    #[test]
    fn jev_first_by_the_rules_name_is_the_same_decision() {
        let s = lucina_intent::heuristics::Settings::default();
        let args = [
            (None, None, None, None),
            (Some("0.95"), None, None, None),
            (None, None, Some("off"), None),
            (None, None, None, Some("0.95")),
        ];
        for (first_min, first_intent_min, over_rule, over_rule_min) in args {
            for rule_intent in [
                None,
                Some("grocery.expense.query"),
                Some("computer_task"),
                Some("schedule_reminder"),
                Some("chat"),
            ] {
                let rule = rule_intent.map(|t| {
                    lucina_intent::tier0::from_model_json(
                        &json!({"intent": t, "confidence": 1.0}),
                        "x",
                        "",
                        &s,
                    )
                });
                assert_eq!(
                    rule.as_ref().map(|r| r.intent.as_str()),
                    rule_intent,
                    "the rule reaches the decision by its name only"
                );
                for jev in [
                    None,
                    Some(("", 0.99)),
                    Some(("unknown", 0.99)),
                    Some(("chat", 0.99)),
                    Some(("computer_task", 0.99)),
                    Some(("grocery.shopping_list.query", 0.95)),
                    Some(("grocery.shopping_list.query", 0.8)),
                ] {
                    assert_eq!(
                        jev_first_from(jev, rule.as_ref(), first_min, first_intent_min, over_rule, over_rule_min),
                        jev_first_rule_from(jev, rule_intent, first_min, first_intent_min, over_rule, over_rule_min),
                        "{jev:?} against {rule_intent:?} with {first_min:?} {first_intent_min:?} {over_rule:?} {over_rule_min:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn only_deterministic_rules_survive_an_unsure_jev() {
        let s = lucina_intent::heuristics::Settings::default();
        let rule = |intent: &str| {
            lucina_intent::tier0::from_model_json(
                &json!({"intent": intent, "confidence": 1.0, "needs_tools": true}),
                "x",
                "",
                &s,
            )
        };
        for intent in ["computer_task", "grocery.shopping_list.add"] {
            assert_eq!(
                rule_when_jev_unsure(Some(rule(intent)))
                    .map(|c| c.intent)
                    .as_deref(),
                Some(intent)
            );
        }
        for intent in [
            "social_post",
            "cursor_task",
            "schedule_reminder",
            "list_reminders",
            "chat",
        ] {
            assert!(
                rule_when_jev_unsure(Some(rule(intent))).is_none(),
                "{intent}"
            );
        }
        assert!(rule_when_jev_unsure(None).is_none());
    }

    #[test]
    fn mcp_turns_fall_back_to_chat_when_jev_disagrees_with_a_side_effect_rule() {
        let s = lucina_intent::heuristics::Settings::default();
        let rule = |intent: &str| {
            lucina_intent::tier0::from_model_json(
                &json!({"intent": intent, "confidence": 1.0, "needs_tools": true}),
                "x",
                "",
                &s,
            )
        };
        // The 2026-10-28 incident: tier 0 said cursor_task 0.9 (Jev: unknown 0.28) and
        // schedule_reminder 1.0 (Jev: computer_task 0.49).
        assert!(mcp_falls_back_to_chat(&rule("cursor_task"), "unknown"));
        assert!(mcp_falls_back_to_chat(
            &rule("schedule_reminder"),
            "computer_task"
        ));
        assert!(!mcp_falls_back_to_chat(
            &rule("schedule_reminder"),
            "schedule_reminder"
        ));
        assert!(
            !mcp_falls_back_to_chat(&rule("list_reminders"), "chat"),
            "reads stay"
        );
        assert!(!mcp_falls_back_to_chat(&rule("chat"), "cursor_task"));
    }

    /// The lines logged inside `run`, captured on this thread only so a parallel test cannot fill
    /// the buffer.
    fn logged(run: impl FnOnce()) -> String {
        use std::io::Write as _;
        use std::sync::{Arc, Mutex};

        #[derive(Clone)]
        struct W(Arc<Mutex<Vec<u8>>>);
        impl Write for W {
            fn write(&mut self, d: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(d);
                Ok(d.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buf = Arc::new(Mutex::new(Vec::new()));
        let w = W(buf.clone());
        let sub = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .with_writer(move || w.clone())
            .finish();
        tracing::subscriber::with_default(sub, run);
        String::from_utf8_lossy(&buf.lock().unwrap().clone()).to_string()
    }

    /// TURN-11: a turn and a replay of the same text both log a `jev_first` line, and each says which
    /// it was — the field `jev_drift` reads to keep the labelled replay out of the live count.
    #[test]
    fn the_jev_first_line_says_whether_a_turn_or_a_replay_wrote_it() {
        let text = logged(|| {
            log_jev_first_used("turn", "chat", 0.9, Some("schedule_reminder"), "", "");
            log_jev_first_fallback(
                "replay",
                "below_threshold",
                Some(("chat", 0.6)),
                Some("schedule_reminder"),
            );
        });
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| l.contains(r#"event="jev_first""#))
            .collect();
        assert_eq!(lines.len(), 2, "{text}");
        let turn = crate::jev_drift::parse_log_line(lines[0]).expect("the turn line");
        let replay = crate::jev_drift::parse_log_line(lines[1]).expect("the replay line");
        assert_eq!((turn.source.as_deref(), turn.used), (Some("turn"), true));
        assert_eq!(turn.tier0.as_deref(), Some("schedule_reminder"));
        assert_eq!(
            (replay.source.as_deref(), replay.used),
            (Some("replay"), false)
        );
        assert_eq!(replay.reason.as_deref(), Some("below_threshold"));
        assert_eq!(replay.confidence, Some(0.6));
    }

    #[test]
    fn jev_answers_go_through_the_same_guards_as_deepseek() {
        let s = lucina_intent::heuristics::Settings::default();
        // Jev's JSON in the classifier's shape: the travel guard still turns list_reminders into chat.
        let v = json!({"intent": "list_reminders", "confidence": 0.9, "reply_mode": "direct",
            "scope": "all", "needs_memory": false, "needs_tools": true});
        let c =
            lucina_intent::tier0::from_model_json(&v, "a che ora parte il volo per CPH?", "", &s);
        assert_eq!(c.intent, "chat");
        let c = lucina_intent::tier0::from_model_json(&v, "che promemoria ho?", "", &s);
        assert_eq!(c.intent, "list_reminders");
        assert_eq!(c.reply_mode, "direct");
        assert!((c.confidence - 0.9).abs() < 1e-9);
    }
}
