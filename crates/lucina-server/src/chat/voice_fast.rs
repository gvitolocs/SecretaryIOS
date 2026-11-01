//! The Jev fast path of a spoken turn (`"voice": true`, docs/lucinavoice-protocol.md section 6).
//!
//! A voice turn waits for the planner LLM for seconds, although Jev classifies the same sentence in a
//! quarter of a second. So, for the owner's own spoken turns on the JSON planner:
//!
//! 1. [`start`] (the first thing `run_turn_scoped` does) spawns one Jev intent call. Its state is facts only,
//!    one fixed template: the channel (`voice`), the owner's local date and time, the transcript. The
//!    questions are the intent questions of everyday turns (`jev_route::intent_questions`), so verdicts stay
//!    comparable. It runs in a task of its own, capped at `Config::voice_jev_cap` (400 ms).
//! 2. The turn's context building ([`crate::context::prepare_keyed`]) runs at the same time
//!    ([`Started::verdict`] polls both). A slower, failed or unavailable Jev leaves the turn exactly on
//!    today's path: tier 0, then the planner.
//! 3. [`route`] decides: a verdict of at least `Config::voice_fast_min` (0.85) on one of the fast intents,
//!    with no contradicting tier-0 rule and the deterministic parsers satisfied, is answered here with a
//!    short templated spoken reply and no model call, through the code the normal path writes with. Anything
//!    else is a slow route (and, for the slow intents, an `ack`). The `route` event and the one
//!    `event="voice_route"` log line say which.
//!
//! Fast intents: the current time or date; the shopping list's contents; the reminders (today's or all);
//! adding to or removing from the shopping list and scheduling a reminder or a timer, when the existing
//! parsers resolve them without a model. A fast turn writes the timeline like any other (the caller does).

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use chrono::{DateTime, Datelike, Timelike, Utc};
use chrono_tz::Tz;
use lucina_intent::{grocery as gi, notes as nt, tier0};
use lucina_jev::Jev;
use lucina_store::parser::{parse_reminder_message, ParseOptions};
use lucina_store::reminders::{filter_for_scope, Reminder};
use lucina_store::timeline::Meta;
use serde_json::{json, Value};

use super::{create_turn_reminder, readable_domains, resolve_reminder, Finish, Turn};
use crate::api::owner_reminders;
use crate::voice::{self, RouteInfo};
use crate::AppState;

/// Intents a spoken turn can answer without the planner.
const FAST_INTENTS: &[&str] = &[
    "temporal_query",
    "list_reminders",
    "grocery.shopping_list.query",
    "grocery.shopping_list.add",
    "grocery.shopping_list.remove",
    "schedule_reminder",
];

/// Intents whose turn is known to take a while: the caller hears "One moment…" right after the route.
const ACK_INTENTS: &[&str] = &[
    "web_crawl",
    "computer_task",
    "explicit_memory_recall",
    "calendar_event",
    "agent_message",
    "generate_image",
    "graph_task",
    "spreadsheet_update",
    "cursor_task",
];

/// What Jev said, or why it said nothing.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Outcome {
    Verdict {
        intent: String,
        confidence: f64,
        /// Jev's `scope` answer (`today` / `all`).
        scope: String,
        ms: u64,
    },
    Failed {
        /// `unavailable` (not configured, or the breaker is open), `timeout` or `error`.
        reason: &'static str,
        ms: u64,
    },
}

/// The turn's Jev call, running.
pub(super) struct Started {
    task: tokio::task::JoinHandle<Outcome>,
    /// Jev is configured: the route and the ack are events of the stream. Without a Jev client the turn
    /// is today's path to the last event (and the log line still says so).
    announce: bool,
}

/// The facts Jev sees, in one fixed template: no recent turns, no guessed context.
fn state_of(message: &str, tz: Tz, now: DateTime<Utc>) -> Value {
    let local = now.with_timezone(&tz);
    json!({
        "channel": "voice",
        "local_time": local.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        "weekday": local.format("%A").to_string(),
        "time_zone": tz.name(),
        "message": message,
    })
}

/// Starts Jev's intent call. Call it before anything else of the turn runs.
pub(super) fn start(st: &AppState, message: &str) -> Started {
    let own = st.voice_jev.clone();
    let cap = st.cfg.voice_jev_cap;
    let state = state_of(message, st.cfg.time_zone, Utc::now());
    let announce = own
        .as_ref()
        .map_or_else(|| lucina_jev::shared().configured(), |j| j.configured());
    let task = tokio::spawn(async move {
        let jev: &Jev = match &own {
            Some(j) => j,
            None => lucina_jev::shared(),
        };
        ask(jev, state, cap).await
    });
    Started { task, announce }
}

async fn ask(jev: &Jev, state: Value, cap: Duration) -> Outcome {
    if !jev.available() {
        return Outcome::Failed {
            reason: "unavailable",
            ms: 0,
        };
    }
    let questions = crate::jev_route::intent_questions();
    let started = Instant::now();
    let answer = tokio::time::timeout(cap, jev.decide("voice", state, &questions)).await;
    let ms = started.elapsed().as_millis() as u64;
    match answer {
        Err(_) => Outcome::Failed {
            reason: "timeout",
            ms,
        },
        Ok(Err(_)) => Outcome::Failed {
            reason: "error",
            ms,
        },
        Ok(Ok(d)) => match d.choice("intent") {
            Some((intent, confidence)) => Outcome::Verdict {
                intent: intent.to_owned(),
                confidence,
                scope: d
                    .choice("scope")
                    .map(|c| c.0.to_owned())
                    .unwrap_or_default(),
                ms,
            },
            None => Outcome::Failed {
                reason: "error",
                ms,
            },
        },
    }
}

impl Started {
    /// Jev's outcome, while `prepare` (the turn's context building) makes progress at the same time. When
    /// `prepare` finishes first its result is handed back; otherwise it stays pending in the caller's pin
    /// (and is simply dropped if a fast route answers the turn).
    pub(super) async fn verdict<P: Future>(
        self,
        mut prepare: Pin<&mut P>,
    ) -> (Outcome, bool, Option<P::Output>) {
        let announce = self.announce;
        let mut task = self.task;
        let lost = |r: Result<Outcome, tokio::task::JoinError>| {
            r.unwrap_or(Outcome::Failed {
                reason: "error",
                ms: 0,
            })
        };
        tokio::select! {
            r = &mut task => (lost(r), announce, None),
            p = prepare.as_mut() => {
                let r = (&mut task).await;
                (lost(r), announce, Some(p))
            }
        }
    }
}

/// What a fast turn answers.
#[derive(Debug, Clone, PartialEq)]
enum Plan {
    Clock(Clock, &'static str),
    ListReminders {
        today: bool,
        lang: &'static str,
    },
    ShoppingQuery,
    /// Add or remove: the grocery path of the normal turn does it.
    ShoppingWrite,
    Reminder {
        text: String,
        due: String,
        relative: bool,
        lang: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clock {
    Time,
    Date,
}

/// Decides the turn's route; announces it (`route` event, log line); answers a fast turn. `None`: the
/// turn goes on the normal path (after an `ack` for the slow intents).
pub(super) async fn route(t: &Turn<'_>, outcome: Outcome, announce: bool) -> Option<Finish> {
    let (info, plan) = decide(t, &outcome);
    let (jev, reason) = match &outcome {
        Outcome::Verdict { .. } => ("ok", ""),
        Outcome::Failed { reason, .. } => ("failed", *reason),
    };
    tracing::info!(target: "lucina_voice::route", event = "voice_route", intent = %info.intent,
        confidence = info.confidence, jev_ms = info.jev_ms, fast = info.fast, source = info.source,
        jev, reason, "voice route");
    if announce {
        voice::route(info.clone());
    }
    if let Some(plan) = plan {
        return Some(answer(t, plan).await);
    }
    if announce && ACK_INTENTS.contains(&info.intent.as_str()) {
        voice::ack(voice::ack_text());
    }
    None
}

fn decide(t: &Turn<'_>, outcome: &Outcome) -> (RouteInfo, Option<Plan>) {
    match outcome {
        Outcome::Verdict {
            intent,
            confidence,
            scope,
            ms,
        } => {
            let plan = plan(t, intent, *confidence, scope);
            let info = RouteInfo {
                intent: intent.clone(),
                confidence: *confidence,
                jev_ms: *ms,
                fast: plan.is_some(),
                source: "jev",
            };
            (info, plan)
        }
        Outcome::Failed { ms, .. } => {
            let info = match tier0::classify_tier0(&t.message, "", &t.st.cfg.intent) {
                Some(c) => RouteInfo {
                    intent: c.intent,
                    confidence: c.confidence,
                    jev_ms: *ms,
                    fast: false,
                    source: "tier0",
                },
                None => RouteInfo {
                    intent: "unknown".into(),
                    confidence: 0.0,
                    jev_ms: *ms,
                    fast: false,
                    source: "fallback",
                },
            };
            (info, None)
        }
    }
}

/// The turn's reply language when a template exists for it (`planner::TURN_LANG`, the same detection as
/// every other reply).
fn template_lang() -> Option<&'static str> {
    match crate::jev_yesno::turn_lang_code() {
        "it" => Some("it"),
        "en" => Some("en"),
        _ => None,
    }
}

/// Is `intent`, at `confidence`, a fast turn? Every condition is a reason to take the normal path.
fn plan(t: &Turn<'_>, intent: &str, confidence: f64, scope: &str) -> Option<Plan> {
    let st = t.st;
    if confidence < st.cfg.voice_fast_min || !FAST_INTENTS.contains(&intent) {
        return None;
    }
    // Only the owner's own spoken turns: the same guards as the normal path (no MCP, guest, share).
    if t.guarded || t.consult || t.mcp.is_some() || t.origin.is_some() || t.share {
        return None;
    }
    let msg = t.message.as_str();
    // A tier-0 rule that names another intent wins (a calendar event, a recall, a web search…). Plain
    // chat is the catch-all rule, so it does not contradict a read.
    let rule = tier0::classify_tier0(msg, "", &st.cfg.intent);
    let agrees = |chat_ok: bool| match &rule {
        None => true,
        Some(c) => c.intent == intent || (chat_ok && c.intent == "chat"),
    };
    // The shortcuts the normal path runs before the planner and that a spoken sentence can still trigger.
    if (st.cfg.notes_enabled && nt::parse_notes_intent(msg, false).is_some())
        || readable_domains(st, &t.session, msg).len() >= 2
    {
        return None;
    }
    match intent {
        "temporal_query" => {
            let lang = template_lang()?;
            agrees(true).then_some(())?;
            Some(Plan::Clock(clock_question(msg)?, lang))
        }
        "list_reminders" => {
            let lang = template_lang()?;
            if !st.cfg.reminders_enabled || !agrees(false) {
                return None;
            }
            let today =
                scope == "today" || lucina_intent::heuristics::reminder_list_scope(msg) == "today";
            Some(Plan::ListReminders { today, lang })
        }
        "grocery.shopping_list.query" => {
            (st.grocery.is_some() && agrees(true)).then_some(Plan::ShoppingQuery)
        }
        "grocery.shopping_list.add" | "grocery.shopping_list.remove" => {
            st.grocery.as_ref()?;
            let g = gi::parse_grocery_intent(msg)?;
            (g.intent == intent && g.product.is_some() && agrees(false))
                .then_some(Plan::ShoppingWrite)
        }
        "schedule_reminder" => {
            let lang = template_lang()?;
            if !st.cfg.reminders_enabled
                || st.cfg.owner_chat_id.as_deref().is_none_or(str::is_empty)
                || !agrees(false)
            {
                return None;
            }
            let o = ParseOptions {
                tz: st.cfg.time_zone,
                default_hour: st.cfg.reminder_default_hour,
                default_minute: st.cfg.reminder_default_minute,
                now: Utc::now(),
            };
            // The parsers alone: no planner pass fills in what they miss.
            let parsed = parse_reminder_message(msg, &o).ok()?;
            let (text, due) = resolve_reminder(msg, &json!({"text": "", "dueDate": ""}), &o)?;
            Some(Plan::Reminder {
                text,
                due,
                relative: parsed.relative,
                lang,
            })
        }
        _ => None,
    }
}

async fn answer(t: &Turn<'_>, plan: Plan) -> Finish {
    let st = t.st;
    let tz = st.cfg.time_zone;
    match plan {
        Plan::Clock(kind, lang) => {
            let now = Utc::now().with_timezone(&tz);
            Finish::reply(clock_reply(kind, lang, now), "reply").with("[voice fast: clock]", None)
        }
        Plan::ListReminders { today, lang } => {
            let now = Utc::now();
            let pending = owner_reminders(st).unwrap_or_default();
            let mut pending = filter_for_scope(&pending, today, tz, now);
            pending.sort_by_key(|r| r.due());
            Finish::reply(
                reminders_reply(&pending, today, lang, tz, now),
                "list_reminders",
            )
            .with("[voice fast: reminders]", None)
        }
        Plan::ShoppingQuery => shopping_query(t).await,
        Plan::ShoppingWrite => {
            match crate::grocery_turn::process(
                st.grocery.as_deref(),
                &st.cfg.grocery_user_id,
                &t.message,
                "",
                &t.channel,
            )
            .await
            {
                Ok(Some(h)) => {
                    let meta = h.list_id.map(|id| Meta {
                        list_id: Some(id),
                        ..Meta::kind("shopping")
                    });
                    Finish::reply(h.reply, "grocery").with(h.intent, meta)
                }
                // The parser said yes in `plan`; it cannot say no a moment later.
                Ok(None) => Finish::reply("I could not do that just now.", "grocery"),
                Err(e) => {
                    tracing::error!(error = %e, "secretary grocery turn failed");
                    Finish::reply(
                        "I could not update grocery accounting right now. Your purchase was not recorded.",
                        "grocery",
                    )
                }
            }
        }
        Plan::Reminder {
            text,
            due,
            relative,
            lang,
        } => {
            let chat = st.cfg.owner_chat_id.clone().unwrap_or_default();
            let action = "schedule_reminder";
            match create_turn_reminder(t, chat, text, due).await {
                Ok((created, reminder)) => {
                    let mut f = Finish::reply(
                        reminder_reply(&reminder, created, relative, lang, tz, Utc::now()),
                        action,
                    );
                    f.extra.insert("reminder".into(), reminder.public());
                    f.action_result = format!(
                        "[reminder {}]",
                        if created { "created" } else { "existing" }
                    );
                    f
                }
                Err(e) => Finish::reply(
                    format!("I could not schedule that reminder ({}).", e.code),
                    action,
                ),
            }
        }
    }
}

async fn shopping_query(t: &Turn<'_>) -> Finish {
    let st = t.st;
    let lang = lucina_grocery::message_language(&t.message, None);
    let Some(repo) = st.grocery.as_deref() else {
        return Finish::reply("Grocery accounting is not configured yet.", "grocery");
    };
    match repo
        .legacy_list_in(&st.cfg.grocery_user_id, Some(lang))
        .await
    {
        Ok(items) => {
            let names: Vec<String> = items
                .iter()
                .filter(|i| i["status"] == json!("needed"))
                .filter_map(|i| i["name"].as_str().map(str::to_owned))
                .collect();
            Finish::reply(shopping_list_reply(&names, lang), "grocery")
                .with("grocery.shopping_list.query", None)
        }
        Err(e) => {
            tracing::error!(error = %e, "secretary grocery turn failed");
            Finish::reply(
                "I could not update grocery accounting right now. Your purchase was not recorded.",
                "grocery",
            )
        }
    }
}

// ------------------------------------------------------------------------------ the time and the date

fn fold(c: char) -> char {
    match c {
        'à' | 'á' | 'â' => 'a',
        'è' | 'é' | 'ê' => 'e',
        'ì' | 'í' => 'i',
        'ò' | 'ó' => 'o',
        'ù' | 'ú' => 'u',
        c => c,
    }
}

const LEAD: &[&str] = &[
    "ciao", "hey", "hi", "hello", "ehi", "lucina", "scusa", "scusami", "senti", "dimmi", "ok",
    "okay", "allora", "please", "excuse", "me", "tell", "can", "you", "could", "would", "mi",
    "dici", "puoi", "dirmi", "sapresti", "sai", "pls",
];
const TRAIL: &[&str] = &[
    "lucina",
    "per",
    "favore",
    "piacere",
    "please",
    "adesso",
    "ora",
    "now",
    "oggi",
    "today",
    "esattamente",
    "exactly",
    "grazie",
    "thanks",
    "thank",
    "you",
    "pls",
];
const TIME_PHRASES: &[&str] = &[
    "che ore sono",
    "che ora e",
    "che ora abbiamo",
    "what time is it",
    "what s the time",
    "what is the time",
    "what time do we have",
    "do you have the time",
];
const DATE_PHRASES: &[&str] = &[
    "che giorno e",
    "che giorno abbiamo",
    "che data e",
    "che giorno della settimana e",
    "che giorno e della settimana",
    "quanti ne abbiamo",
    "what day is it",
    "what s the date",
    "what is the date",
    "what s the date today",
    "what s today s date",
    "what is today s date",
    "what date is it",
    "what day of the week is it",
];

/// Is the sentence a question about the current time or date, and nothing more ("che ore sono?",
/// "scusa, che giorno è oggi?")? A deadline, an appointment or a time elsewhere has more words and takes
/// the normal path.
fn clock_question(msg: &str) -> Option<Clock> {
    let mut words: Vec<String> = msg
        .chars()
        .flat_map(char::to_lowercase)
        .map(fold)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    while words.first().is_some_and(|w| LEAD.contains(&w.as_str())) {
        words.remove(0);
    }
    while words.last().is_some_and(|w| TRAIL.contains(&w.as_str())) {
        words.pop();
    }
    let core = words.join(" ");
    if TIME_PHRASES.contains(&core.as_str()) {
        Some(Clock::Time)
    } else if DATE_PHRASES.contains(&core.as_str()) {
        Some(Clock::Date)
    } else {
        None
    }
}

const UNITS: [&str; 20] = [
    "zero",
    "uno",
    "due",
    "tre",
    "quattro",
    "cinque",
    "sei",
    "sette",
    "otto",
    "nove",
    "dieci",
    "undici",
    "dodici",
    "tredici",
    "quattordici",
    "quindici",
    "sedici",
    "diciassette",
    "diciotto",
    "diciannove",
];

/// 0..=59 (and more, for counts) in Italian words: "ventuno", "trentatré".
fn it_num(n: u32) -> String {
    if n < 20 {
        return UNITS[n as usize].to_owned();
    }
    if n >= 100 {
        return n.to_string();
    }
    let tens = [
        "",
        "",
        "venti",
        "trenta",
        "quaranta",
        "cinquanta",
        "sessanta",
        "settanta",
        "ottanta",
        "novanta",
    ][(n / 10) as usize];
    match n % 10 {
        0 => tens.to_owned(),
        3 => format!("{tens}tré"),
        u @ (1 | 8) => format!("{}{}", &tens[..tens.len() - 1], UNITS[u as usize]),
        u => format!("{tens}{}", UNITS[u as usize]),
    }
}

/// "le quattordici e cinque", "l'una", "mezzogiorno": the clock the way Italians say it.
fn it_clock(h: u32, m: u32) -> (&'static str, String) {
    // (verb, phrase)
    let (verb, base) = match h {
        0 => ("È", "mezzanotte".to_owned()),
        12 => ("È", "mezzogiorno".to_owned()),
        1 => ("È", "l'una".to_owned()),
        h => ("Sono", format!("le {}", it_num(h))),
    };
    let phrase = match (m, h) {
        (0, 0 | 12) => base,
        (0, _) => format!("{base} in punto"),
        (m, _) => format!("{base} e {}", it_num(m)),
    };
    (verb, phrase)
}

/// "alle quattordici e cinque", "all'una", "a mezzogiorno".
fn it_at(h: u32, m: u32) -> String {
    let minutes = if m == 0 {
        String::new()
    } else {
        format!(" e {}", it_num(m))
    };
    match h {
        0 => format!("a mezzanotte{minutes}"),
        12 => format!("a mezzogiorno{minutes}"),
        1 => format!("all'una{minutes}"),
        h => format!("alle {}{minutes}", it_num(h)),
    }
}

fn en_clock(h: u32, m: u32) -> String {
    if m == 0 && h == 0 {
        return "midnight".into();
    }
    if m == 0 && h == 12 {
        return "noon".into();
    }
    let (h12, ampm) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        h => (h - 12, "PM"),
    };
    if m == 0 {
        format!("{h12} {ampm}")
    } else {
        format!("{h12}:{m:02} {ampm}")
    }
}

const IT_DAYS: [&str; 7] = [
    "lunedì",
    "martedì",
    "mercoledì",
    "giovedì",
    "venerdì",
    "sabato",
    "domenica",
];
const IT_MONTHS: [&str; 12] = [
    "gennaio",
    "febbraio",
    "marzo",
    "aprile",
    "maggio",
    "giugno",
    "luglio",
    "agosto",
    "settembre",
    "ottobre",
    "novembre",
    "dicembre",
];
const EN_DAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];
const EN_MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn en_ordinal(d: u32) -> String {
    let suffix = match (d % 100, d % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{d}{suffix}")
}

fn clock_reply(kind: Clock, lang: &str, now: DateTime<Tz>) -> String {
    let (h, m) = (now.hour(), now.minute());
    let wd = now.weekday().num_days_from_monday() as usize;
    match (kind, lang) {
        (Clock::Time, "it") => {
            let (verb, phrase) = it_clock(h, m);
            format!("{verb} {phrase}.")
        }
        (Clock::Time, _) => format!("It's {}.", en_clock(h, m)),
        (Clock::Date, "it") => format!(
            "Oggi è {} {} {} {}.",
            IT_DAYS[wd],
            now.day(),
            IT_MONTHS[now.month0() as usize],
            now.year()
        ),
        (Clock::Date, _) => format!(
            "Today is {}, {} {}, {}.",
            EN_DAYS[wd],
            EN_MONTHS[now.month0() as usize],
            en_ordinal(now.day()),
            now.year()
        ),
    }
}

// ------------------------------------------------------------------------------------ the reminders

/// "oggi alle quindici", "domani alle nove", "il 3 novembre alle nove" / "today at 3 PM", "on November 3 at 9 AM".
fn when_phrase(
    due: DateTime<Utc>,
    now: DateTime<Utc>,
    tz: Tz,
    lang: &str,
    with_day: bool,
) -> String {
    let local = due.with_timezone(&tz);
    let today = now.with_timezone(&tz).date_naive();
    let days = (local.date_naive() - today).num_days();
    let (h, m) = (local.hour(), local.minute());
    if lang == "it" {
        let at = it_at(h, m);
        if !with_day {
            return at;
        }
        match days {
            0 => format!("oggi {at}"),
            1 => format!("domani {at}"),
            _ => format!(
                "il {} {} {at}",
                local.day(),
                IT_MONTHS[local.month0() as usize]
            ),
        }
    } else {
        let at = format!("at {}", en_clock(h, m));
        if !with_day {
            return at;
        }
        match days {
            0 => format!("today {at}"),
            1 => format!("tomorrow {at}"),
            _ => format!(
                "on {} {} {at}",
                EN_MONTHS[local.month0() as usize],
                en_ordinal(local.day())
            ),
        }
    }
}

fn spoken_text(s: &str) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    one.chars().take(80).collect()
}

fn reminder_reply(
    r: &Reminder,
    created: bool,
    relative: bool,
    lang: &str,
    tz: Tz,
    now: DateTime<Utc>,
) -> String {
    let Some(due) = r.due() else {
        return if lang == "it" {
            "Fatto.".into()
        } else {
            "Done.".into()
        };
    };
    let text = spoken_text(&r.text);
    let minutes = ((due - now).num_seconds() + 30) / 60;
    let it = lang == "it";
    let when = if relative && (1..120).contains(&minutes) {
        if it {
            match minutes {
                1 => "tra un minuto".to_owned(),
                m => format!("tra {} minuti", it_num(m as u32)),
            }
        } else {
            match minutes {
                1 => "in one minute".to_owned(),
                m => format!("in {m} minutes"),
            }
        }
    } else {
        when_phrase(due, now, tz, lang, true)
    };
    match (it, created) {
        (true, true) => format!("Fatto, ti ricordo {when}: {text}."),
        (true, false) => format!("C'era già un promemoria {when}: {text}."),
        (false, true) => format!("Okay, I'll remind you {when}: {text}."),
        (false, false) => format!("That reminder was already set {when}: {text}."),
    }
}

/// At most this many reminders are read out.
const SPOKEN_ITEMS: usize = 3;

fn count_it(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("un {one}")
    } else {
        format!("{} {many}", it_num(n as u32))
    }
}

fn reminders_reply(
    pending: &[Reminder],
    today: bool,
    lang: &str,
    tz: Tz,
    now: DateTime<Utc>,
) -> String {
    let it = lang == "it";
    if pending.is_empty() {
        return match (it, today) {
            (true, true) => "Non hai promemoria per oggi.".into(),
            (true, false) => "Non hai promemoria in programma.".into(),
            (false, true) => "You have no reminders for today.".into(),
            (false, false) => "You have no upcoming reminders.".into(),
        };
    }
    let items: Vec<String> = pending
        .iter()
        .take(SPOKEN_ITEMS)
        .filter_map(|r| {
            let due = r.due()?;
            Some(format!(
                "{}, {}",
                when_phrase(due, now, tz, lang, !today),
                spoken_text(&r.text)
            ))
        })
        .collect();
    let more = pending.len().saturating_sub(SPOKEN_ITEMS);
    let n = pending.len();
    let head = match (it, today) {
        (true, true) => format!("Hai {} oggi", count_it(n, "promemoria", "promemoria")),
        (true, false) => format!(
            "Hai {} in programma",
            count_it(n, "promemoria", "promemoria")
        ),
        (false, true) => format!(
            "You have {} today",
            if n == 1 {
                "one reminder".to_owned()
            } else {
                format!("{n} reminders")
            }
        ),
        (false, false) => format!(
            "You have {} coming up",
            if n == 1 {
                "one reminder".to_owned()
            } else {
                format!("{n} reminders")
            }
        ),
    };
    let tail = match (more, it) {
        (0, _) => String::new(),
        (k, true) => format!("; e altri {}", it_num(k as u32)),
        (k, false) => format!("; and {k} more"),
    };
    format!("{head}: {}{tail}.", items.join("; "))
}

// ------------------------------------------------------------------------------------ the shopping list

/// How many items of the list are read out.
const SPOKEN_ITEMS_LIST: usize = 8;

fn join_names(names: &[String], and: &str) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} {and} {last}", init.join(", ")),
    }
}

fn shopping_list_reply(names: &[String], lang: &str) -> String {
    if names.is_empty() {
        return crate::grocery_turn::shopping_reply(lang, "empty", "");
    }
    let shown = &names[..names.len().min(SPOKEN_ITEMS_LIST)];
    let more = names.len() - shown.len();
    match lang {
        "it" => {
            let list = join_names(shown, "e");
            if more == 0 {
                format!("Nella lista della spesa hai {list}.")
            } else {
                format!(
                    "Nella lista della spesa hai {} e altre {} cose.",
                    shown.join(", "),
                    it_num(more as u32)
                )
            }
        }
        "da" => {
            if more == 0 {
                format!("Du har {} på indkøbslisten.", join_names(shown, "og"))
            } else {
                format!(
                    "Du har {} og {more} ting mere på indkøbslisten.",
                    shown.join(", ")
                )
            }
        }
        _ => {
            if more == 0 {
                format!(
                    "You have {} on your shopping list.",
                    join_names(shown, "and")
                )
            } else {
                format!(
                    "You have {} and {more} more things on your shopping list.",
                    shown.join(", ")
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Tz> {
        chrono_tz::Europe::Rome
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
    }

    #[test]
    fn only_a_plain_time_or_date_question_is_a_clock_question() {
        for t in [
            "che ore sono?",
            "Che ore sono adesso?",
            "scusa, che ora è?",
            "Lucina, che ora è per favore",
            "mi dici che ore sono",
            "what time is it?",
            "Hey, what's the time please?",
            "can you tell me what time is it now",
        ] {
            assert_eq!(clock_question(t), Some(Clock::Time), "{t}");
        }
        for t in [
            "che giorno è oggi?",
            "che giorno è?",
            "Che data è oggi",
            "che giorno della settimana è oggi?",
            "what day is it today?",
            "what's the date?",
            "what is today's date",
        ] {
            assert_eq!(clock_question(t), Some(Clock::Date), "{t}");
        }
        // Anything that needs data, or a place, or a day other than today, is the normal path's.
        for t in [
            "a che ora parte il volo per Londra?",
            "che ore sono a New York?",
            "che giorno è domani?",
            "che giorno è il mio appuntamento?",
            "quanto manca alla riunione?",
            "what time is my meeting?",
            "what time is it in Tokyo",
            "che ora è la scadenza dell'affitto",
            "ciao come stai",
        ] {
            assert_eq!(clock_question(t), None, "{t}");
        }
    }

    #[test]
    fn italian_numbers_and_times_are_said_the_italian_way() {
        assert_eq!(it_num(0), "zero");
        assert_eq!(it_num(21), "ventuno");
        assert_eq!(it_num(28), "ventotto");
        assert_eq!(it_num(33), "trentatré");
        assert_eq!(it_num(45), "quarantacinque");
        assert_eq!(it_num(59), "cinquantanove");
        let say = |h, m| {
            let (v, p) = it_clock(h, m);
            format!("{v} {p}")
        };
        assert_eq!(say(14, 5), "Sono le quattordici e cinque");
        assert_eq!(say(14, 0), "Sono le quattordici in punto");
        assert_eq!(say(1, 20), "È l'una e venti");
        assert_eq!(say(12, 0), "È mezzogiorno");
        assert_eq!(say(0, 0), "È mezzanotte");
        assert_eq!(say(0, 15), "È mezzanotte e quindici");
        assert_eq!(it_at(9, 0), "alle nove");
        assert_eq!(it_at(1, 5), "all'una e cinque");
        assert_eq!(it_at(12, 30), "a mezzogiorno e trenta");
        assert_eq!(en_clock(14, 5), "2:05 PM");
        assert_eq!(en_clock(9, 0), "9 AM");
        assert_eq!(en_clock(0, 0), "midnight");
        assert_eq!(en_clock(0, 7), "12:07 AM");
        assert_eq!(en_clock(12, 0), "noon");
    }

    #[test]
    fn the_clock_replies_name_the_day_and_the_time() {
        let sunday = at(2026, 11, 1, 14, 5);
        assert_eq!(
            clock_reply(Clock::Time, "it", sunday),
            "Sono le quattordici e cinque."
        );
        assert_eq!(clock_reply(Clock::Time, "en", sunday), "It's 2:05 PM.");
        assert_eq!(
            clock_reply(Clock::Date, "it", sunday),
            "Oggi è domenica 1 novembre 2026."
        );
        assert_eq!(
            clock_reply(Clock::Date, "en", sunday),
            "Today is Sunday, November 1st, 2026."
        );
        assert_eq!(en_ordinal(2), "2nd");
        assert_eq!(en_ordinal(11), "11th");
        assert_eq!(en_ordinal(23), "23rd");
    }

    fn reminder(text: &str, due: DateTime<Tz>) -> Reminder {
        serde_json::from_value(json!({
            "id": "r1", "text": text, "dueAt": due.with_timezone(&Utc).to_rfc3339(),
            "status": "pending", "chatId": "42",
        }))
        .unwrap()
    }

    #[test]
    fn reminders_are_read_out_briefly_in_the_turn_language() {
        let tz = chrono_tz::Europe::Rome;
        let now = at(2026, 11, 1, 10, 0).with_timezone(&Utc);
        assert_eq!(
            reminders_reply(&[], true, "it", tz, now),
            "Non hai promemoria per oggi."
        );
        assert_eq!(
            reminders_reply(&[], false, "en", tz, now),
            "You have no upcoming reminders."
        );
        let one = [reminder("chiamare Anna", at(2026, 11, 1, 15, 30))];
        assert_eq!(
            reminders_reply(&one, true, "it", tz, now),
            "Hai un promemoria oggi: alle quindici e trenta, chiamare Anna."
        );
        assert_eq!(
            reminders_reply(&one, true, "en", tz, now),
            "You have one reminder today: at 3:30 PM, chiamare Anna."
        );
        let many: Vec<Reminder> = (0..5)
            .map(|i| reminder(&format!("cosa {i}"), at(2026, 11, 2 + i, 9, 0)))
            .collect();
        let said = reminders_reply(&many, false, "it", tz, now);
        assert_eq!(
            said,
            "Hai cinque promemoria in programma: domani alle nove, cosa 0; il 3 novembre alle nove, cosa 1; il 4 novembre alle nove, cosa 2; e altri due."
        );
    }

    #[test]
    fn a_new_reminder_is_confirmed_with_when_and_what() {
        let tz = chrono_tz::Europe::Rome;
        let now = at(2026, 11, 1, 10, 0).with_timezone(&Utc);
        let r = reminder("spegnere il forno", at(2026, 11, 1, 10, 20));
        assert_eq!(
            reminder_reply(&r, true, true, "it", tz, now),
            "Fatto, ti ricordo tra venti minuti: spegnere il forno."
        );
        assert_eq!(
            reminder_reply(&r, true, true, "en", tz, now),
            "Okay, I'll remind you in 20 minutes: spegnere il forno."
        );
        let r = reminder("pagare l'affitto", at(2026, 11, 2, 9, 0));
        assert_eq!(
            reminder_reply(&r, true, false, "it", tz, now),
            "Fatto, ti ricordo domani alle nove: pagare l'affitto."
        );
        assert_eq!(
            reminder_reply(&r, false, false, "en", tz, now),
            "That reminder was already set tomorrow at 9 AM: pagare l'affitto."
        );
    }

    #[test]
    fn the_shopping_list_is_read_out_with_a_limit() {
        let names: Vec<String> = ["latte", "uova", "pane"].map(String::from).to_vec();
        assert_eq!(
            shopping_list_reply(&names, "it"),
            "Nella lista della spesa hai latte, uova e pane."
        );
        assert_eq!(
            shopping_list_reply(&names, "en"),
            "You have latte, uova and pane on your shopping list."
        );
        assert_eq!(
            shopping_list_reply(&[], "it"),
            "La lista della spesa è vuota."
        );
        let many: Vec<String> = (0..11).map(|i| format!("x{i}")).collect();
        assert_eq!(
            shopping_list_reply(&many, "it"),
            "Nella lista della spesa hai x0, x1, x2, x3, x4, x5, x6, x7 e altre tre cose."
        );
    }

    #[test]
    fn jevs_state_is_facts_only_in_one_template() {
        let now = at(2026, 11, 1, 14, 5).with_timezone(&Utc);
        let s = state_of("che ore sono?", chrono_tz::Europe::Rome, now);
        assert_eq!(
            s,
            json!({"channel": "voice", "local_time": "2026-11-01T14:05:00+01:00",
                "weekday": "Sunday", "time_zone": "Europe/Rome", "message": "che ore sono?"})
        );
    }
}
