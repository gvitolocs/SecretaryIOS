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
//! Around that route sit the other Jev layers of a spoken turn (docs/lucinavoice-protocol.md section 9), each with
//! its own fixed facts-only state, the same 400 ms cap, a place in the turn's [`voice::Trace`] and a fail-open
//! rule (no answer in time: the turn does what it did before the layer existed). A turn makes at most three Jev
//! calls in all ([`voice::MAX_JEV_CALLS`]):
//!
//! - `intent`: the call above. Its verdict is also the context builder's intent verdict, so a spoken turn
//!   asks Jev its intent questions once ([`crate::voice::shared_intent`]);
//! - `slots`: only when the deterministic parser of a quick command is unsure (a reminder whose time was not
//!   said, or whose text is long; a shopping list with several products), one question. It can hand a reminder
//!   to the planner or split a shopping list;
//! - `confirm`: a risky action (a message to another assistant or a post, a purchase, a delete) is held for a
//!   spoken "sei sicuro?" when Jev says it must be; the next turn's yes runs the held request;
//! - `vocab`: the "which of these words did they say" choice of Voice Learn (`crate::voice_learn`).
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
use lucina_jev::{Decision, Jev, Question};
use lucina_store::parser::{parse_reminder_message, ParseOptions};
use lucina_store::reminders::{filter_for_scope, Reminder};
use lucina_store::timeline::Meta;
use lucina_tools::pending::{classify_reply, Reply};
use serde_json::{json, Value};

use super::{create_turn_reminder, readable_domains, resolve_reminder, Finish, Turn};
use crate::api::owner_reminders;
use crate::voice::{self, Layer, RouteInfo, SharedIntent};
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
    /// The call's verdict in the classifier's shape, for the context building (one intent call per turn).
    intent: tokio::sync::watch::Receiver<Option<SharedIntent>>,
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

/// The Jev client of spoken turns: the config's own (tests), else the process-wide one.
fn jev_of(st: &AppState) -> &Jev {
    match &st.voice_jev {
        Some(j) => j,
        None => lucina_jev::shared(),
    }
}

/// Starts Jev's intent call. Call it before anything else of the turn runs.
pub(super) fn start(st: &AppState, message: &str) -> Started {
    let own = st.voice_jev.clone();
    let cap = st.cfg.voice_jev_cap;
    let state = state_of(message, st.cfg.time_zone, Utc::now());
    let announce = jev_of(st).configured();
    // The intent call is the first of the turn's three (a Jev that is not available makes none).
    if jev_of(st).available() {
        if let Some(trace) = voice::trace() {
            trace.take_call();
        }
    }
    let (tx, intent) = voice::intent_channel();
    let task = tokio::spawn(async move {
        let jev: &Jev = match &own {
            Some(j) => j,
            None => lucina_jev::shared(),
        };
        let (outcome, shared) = ask(jev, state, cap).await;
        let _ = tx.send(Some(shared));
        outcome
    });
    Started {
        task,
        announce,
        intent,
    }
}

async fn ask(jev: &Jev, state: Value, cap: Duration) -> (Outcome, SharedIntent) {
    let failed = |reason, ms| (Outcome::Failed { reason, ms }, None);
    if !jev.available() {
        return failed("unavailable", 0);
    }
    let questions = crate::jev_route::intent_questions();
    let started = Instant::now();
    let answer = tokio::time::timeout(cap, jev.decide("voice", state, &questions)).await;
    let ms = started.elapsed().as_millis() as u64;
    match answer {
        Err(_) => failed("timeout", ms),
        Ok(Err(_)) => failed("error", ms),
        Ok(Ok(d)) => match (d.choice("intent"), crate::jev_route::intent_json(&d)) {
            (Some((intent, confidence)), shared) => (
                Outcome::Verdict {
                    intent: intent.to_owned(),
                    confidence,
                    scope: d
                        .choice("scope")
                        .map(|c| c.0.to_owned())
                        .unwrap_or_default(),
                    ms,
                },
                shared,
            ),
            (None, _) => failed("error", ms),
        },
    }
}

impl Started {
    /// The receiving end of the intent verdict, for the context building.
    pub(super) fn intent_rx(&self) -> tokio::sync::watch::Receiver<Option<SharedIntent>> {
        self.intent.clone()
    }

    /// Jev's outcome, while `prepare` (the turn's context building) makes progress at the same time. When
    /// `prepare` finishes first its result is handed back; otherwise it stays pending in the caller's pin
    /// (and is simply dropped if a fast route answers the turn).
    pub(super) async fn verdict<P: Future>(
        self,
        mut prepare: Pin<&mut P>,
    ) -> (Outcome, bool, Option<P::Output>) {
        let announce = self.announce;
        let mut task = self.task;
        drop(self.intent);
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
    /// Add or remove: the grocery path of the normal turn does it. With several products (the `slots`
    /// layer split the sentence): one `(message, product)` per product, each run through the same path.
    ShoppingWrite(Vec<(String, String)>),
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

/// A spoken turn's pending "sei sicuro?" (`st.pending`, tool name).
const CONFIRM_TOOL: &str = "voice_confirm";
/// The action of the confirmation question and of the cancel answer.
pub(super) const CONFIRM_ACTION: &str = "voice_confirm";

/// Decides the turn's route; announces it (`route` event, log line); answers a fast turn. `None`: the
/// turn goes on the normal path (after an `ack` for the slow intents). `confirmed`: this turn runs a request
/// the owner just confirmed, so the `confirm` layer does not ask again.
pub(super) async fn route(
    t: &Turn<'_>,
    outcome: Outcome,
    announce: bool,
    confirmed: bool,
) -> Option<Finish> {
    let trace = voice::trace();
    let (mut info, mut plan) = decide(t, &outcome);
    if let Some(layer) = intent_layer(&outcome, &info) {
        push(layer);
    }
    let mut held: Option<Finish> = None;
    if t.st.cfg.voice_layers {
        if let Some(p) = plan.take() {
            // A quick command the parser was unsure about gets one more look.
            plan = Box::pin(slots_layer(t, p)).await;
            info.fast = plan.is_some();
        } else if !confirmed {
            // A risky action may need a spoken yes first: the question is the whole answer of this turn.
            held = Box::pin(confirm_layer(t, &info)).await;
            info.fast = held.is_some();
        }
    }
    info.layers = trace.as_ref().map(|t| t.layers()).unwrap_or_default();
    if let Some(trace) = trace.as_ref().filter(|_| announce) {
        trace.set_route(info.clone());
    }
    let (jev, reason) = match &outcome {
        Outcome::Verdict { .. } => ("ok", ""),
        Outcome::Failed { reason, .. } => ("failed", *reason),
    };
    let layers = info
        .layers
        .iter()
        .map(|l| {
            format!(
                "{}:{}:{:.2}:{}ms{}",
                l.layer,
                l.verdict,
                l.confidence,
                l.ms,
                if l.used { ":used" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    tracing::info!(target: "lucina_voice::route", event = "voice_route", intent = %info.intent,
        confidence = info.confidence, jev_ms = info.jev_ms, fast = info.fast, source = info.source,
        jev, reason, jev_calls = trace.as_ref().map_or(0, |t| t.calls()), layers = %layers, "voice route");
    if announce {
        voice::route(info.clone());
    }
    if let Some(finish) = held {
        return Some(finish);
    }
    if let Some(plan) = plan {
        return Some(Box::pin(answer(t, plan)).await);
    }
    if announce && ACK_INTENTS.contains(&info.intent.as_str()) {
        voice::ack(voice::ack_text());
    }
    None
}

fn push(layer: Layer) {
    if let Some(trace) = voice::trace() {
        trace.push(layer);
    }
}

/// The intent layer's record: Jev's verdict (or why it gave none), and whether the route took it.
fn intent_layer(outcome: &Outcome, info: &RouteInfo) -> Option<Layer> {
    match outcome {
        Outcome::Verdict {
            intent,
            confidence,
            ms,
            ..
        } => Some(Layer {
            layer: "intent",
            verdict: intent.clone(),
            confidence: *confidence,
            ms: *ms,
            used: info.fast || *confidence >= crate::jev_route::intent_min(),
        }),
        // Not configured, or the breaker is open: no call was made, nothing to report.
        Outcome::Failed {
            reason: "unavailable",
            ..
        } => None,
        Outcome::Failed { reason, ms } => Some(Layer {
            layer: "intent",
            verdict: (*reason).to_owned(),
            confidence: 0.0,
            ms: *ms,
            used: false,
        }),
    }
}

/// One capped Jev call of a layer. `Err(reason)`: no answer (`unavailable`, `budget` when the turn's three
/// calls are used, `timeout`, `error`), with the milliseconds spent.
async fn layer_call(
    t: &Turn<'_>,
    purpose: &'static str,
    state: Value,
    questions: &[(&str, Question)],
) -> Result<(Decision, u64), (&'static str, u64)> {
    let jev = jev_of(t.st);
    if !jev.available() {
        return Err(("unavailable", 0));
    }
    if let Some(trace) = voice::trace() {
        if !trace.take_call() {
            return Err(("budget", 0));
        }
    }
    let started = Instant::now();
    let answer = tokio::time::timeout(
        t.st.cfg.voice_jev_cap,
        jev.decide(purpose, state, questions),
    )
    .await;
    let ms = started.elapsed().as_millis() as u64;
    match answer {
        Err(_) => Err(("timeout", ms)),
        Ok(Err(_)) => Err(("error", ms)),
        Ok(Ok(d)) => Ok((d, ms)),
    }
}

/// Records a layer that got no answer (a Jev that is not available made no call and leaves no record).
fn push_failed(layer: &'static str, reason: &'static str, ms: u64) {
    if reason != "unavailable" {
        push(Layer {
            layer,
            verdict: reason.to_owned(),
            confidence: 0.0,
            ms,
            used: false,
        });
    }
}

/// `p` of a yes/no question as a verdict word and the confidence in it.
fn yes_no(p: f64, yes: &str, no: &str) -> (String, f64) {
    if p >= 0.5 {
        (yes.to_owned(), p)
    } else {
        (no.to_owned(), 1.0 - p)
    }
}

// ------------------------------------------------------------------------------------------ slots layer

/// The pieces of a shopping sentence's product phrase ("latte, uova e pane"), if it names several.
fn split_products(raw: &str) -> Vec<String> {
    static SPLIT: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = SPLIT.get_or_init(|| {
        regex::Regex::new(r"(?i)\s*(?:,|;|\+|&|\s+e\s+|\s+and\s+|\s+og\s+)\s*").expect("valid")
    });
    let pieces: Vec<String> = re
        .split(raw)
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect();
    if (2..=6).contains(&pieces.len()) && pieces.iter().all(|p| p.chars().count() <= 40) {
        pieces
    } else {
        Vec::new()
    }
}

/// Is a parsed reminder one the deterministic parser may have got wrong? The time was not said (a part of the
/// day or a bare day gives a default hour), the text is long (a second clause rode along), or the parser
/// removed nothing from the sentence.
fn reminder_unsure(message: &str, text: &str, relative: bool) -> bool {
    (!relative && !message.bytes().any(|b| b.is_ascii_digit()))
        || text.split_whitespace().count() > 10
        || text.trim().eq_ignore_ascii_case(message.trim())
}

/// The `slots` layer: a quick command whose parse was unsure gets one question; `None` hands the turn to
/// the planner (fail open keeps the plan).
async fn slots_layer(t: &Turn<'_>, plan: Plan) -> Option<Plan> {
    let st = t.st;
    let tz = st.cfg.time_zone;
    let min = st.cfg.voice_slots_min;
    match plan {
        Plan::Reminder {
            ref text,
            ref due,
            relative,
            ..
        } => {
            if !reminder_unsure(&t.message, text, relative) {
                return Some(plan);
            }
            let due_local = lucina_store::timeutil::parse_js_date(due)
                .map(|d| {
                    d.with_timezone(&tz)
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
                })
                .unwrap_or_default();
            let state = json!({
                "channel": "voice",
                "message": t.message,
                "time_zone": tz.name(),
                "parsed": {"kind": "reminder", "text": text, "due": due_local, "relative": relative},
            });
            let q = [(
                "slots_ok",
                Question::noul("Do the parsed fields (the reminder's text and the day and time it is due) say exactly what the message asks for? Answer no if the message names a different time, or more than one thing to be reminded of."),
            )];
            match layer_call(t, "voice_slots", state, &q).await {
                Ok((d, ms)) => {
                    let p = d.yes("slots_ok").unwrap_or(1.0);
                    let (verdict, confidence) = yes_no(p, "ok", "no");
                    let demote = verdict == "no" && confidence >= min;
                    push(Layer {
                        layer: "slots",
                        verdict,
                        confidence,
                        ms,
                        used: demote,
                    });
                    (!demote).then_some(plan)
                }
                Err((reason, ms)) => {
                    push_failed("slots", reason, ms);
                    Some(plan)
                }
            }
        }
        Plan::ShoppingWrite(_) => {
            let Some(g) = gi::parse_grocery_intent(&t.message) else {
                return Some(plan);
            };
            let names = split_products(&g.raw_product);
            if names.len() < 2 {
                return Some(plan);
            }
            // Each piece must parse alone to the same intent with a product, or the split is not safe.
            let each: Vec<(String, String)> = names
                .iter()
                .filter_map(|n| {
                    let m = t.message.replacen(&g.raw_product, n, 1);
                    let p = gi::parse_grocery_intent(&m)?;
                    (p.intent == g.intent && p.product.is_some()).then(|| (m, n.clone()))
                })
                .collect();
            if each.len() != names.len() {
                return Some(plan);
            }
            let state = json!({
                "channel": "voice",
                "message": t.message,
                "parsed": {"kind": "shopping_list", "phrase": g.raw_product, "pieces": names},
            });
            let q = [(
                "products",
                Question::choice(
                    "Does the product phrase name one product or several separate products?",
                    [
                        ("one", "One product (its name may hold an 'and' or a comma, like salt and vinegar crisps)."),
                        ("several", "Two or more separate products, one per piece."),
                    ],
                ),
            )];
            match layer_call(t, "voice_slots", state, &q).await {
                Ok((d, ms)) => {
                    let (verdict, confidence) = d
                        .choice("products")
                        .map_or(("one".to_owned(), 0.0), |(c, p)| (c.to_owned(), p));
                    let split = verdict == "several" && confidence >= min;
                    push(Layer {
                        layer: "slots",
                        verdict,
                        confidence,
                        ms,
                        used: split,
                    });
                    Some(if split {
                        Plan::ShoppingWrite(each)
                    } else {
                        plan
                    })
                }
                Err((reason, ms)) => {
                    push_failed("slots", reason, ms);
                    Some(plan)
                }
            }
        }
        other => Some(other),
    }
}

// ----------------------------------------------------------------------------------------- confirm layer

/// What kind of risky action the turn is, if it is one: a message to another assistant (`message`), a post
/// to the Pokoin channel (`post`), a purchase or an order (`purchase`), a delete (`delete`). Deterministic and
/// cheap: it only decides whether the Jev question is worth asking.
pub(super) fn risk_kind(intent: &str, message: &str) -> Option<&'static str> {
    static BUY: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static DEL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static OBJ: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let buy = BUY.get_or_init(|| {
        regex::Regex::new(r"(?i)\b(compra\w*|acquist\w*|ordin(?:a|ami|are)|pag(?:a|ami|are)|checkout|buy|purchase|order|pay|køb\w*|bestil\w*|betal\w*)\b").expect("valid")
    });
    let del = DEL.get_or_init(|| {
        regex::Regex::new(r"(?i)\b(cancell\w*|elimin\w*|rimuov\w*|rimuovi|togli\w*|delete|remove|erase|slet\w*|fjern\w*)\b").expect("valid")
    });
    let obj = OBJ.get_or_init(|| {
        regex::Regex::new(r"(?i)\b(tutt\w*|all|everything|promemoria|reminders?|event\w*|appuntament\w*|not[ae]|notes?|file|folder|account|lista|list)\b").expect("valid")
    });
    match intent {
        "agent_message" => return Some("message"),
        "social_post" => return Some("post"),
        _ => {}
    }
    if matches!(intent, "computer_task" | "web_crawl" | "agent_task") && buy.is_match(message) {
        return Some("purchase");
    }
    const READ_ONLY: &[&str] = &[
        "social_greeting",
        "agent_status_check",
        "temporal_query",
        "explicit_memory_recall",
        "list_reminders",
        "model_status",
        "grocery.shopping_list.query",
        "grocery.shopping_list.remove",
        "grocery.consume",
        "grocery.finished",
        "grocery.pantries.query",
        "grocery.price.query",
        "grocery.expense.query",
        "grocery.analytics.query",
        "grocery.offer.search",
    ];
    if READ_ONLY.contains(&intent) || !del.is_match(message) {
        return None;
    }
    // Plain conversation only counts when it names something that can be deleted.
    let talk = matches!(
        intent,
        "chat" | "unknown" | "continue_current_topic" | "new_topic"
    );
    (!talk || obj.is_match(message)).then_some("delete")
}

fn confirm_question(kind: &str, lang: &str) -> String {
    let what = match (kind, lang) {
        ("message", "da") => "sende en besked videre til en anden assistent",
        ("post", "da") => "udgive et opslag",
        ("purchase", "da") => "foretage et køb eller en bestilling",
        (_, "da") => "slette noget",
        ("message", _) => "pass a message on to another assistant",
        ("post", _) => "publish a post",
        ("purchase", _) => "make a purchase or an order",
        _ => "delete something",
    };
    match lang {
        "da" => format!("Er du sikker? Jeg er ved at {what}. Sig ja for at bekræfte."),
        _ => format!("Are you sure? I'm about to {what}. Say yes to confirm."),
    }
}

fn cancel_text(lang: &str) -> &'static str {
    match lang {
        "da" => "Okay, det gør jeg ikke.",
        _ => "OK, I won't do it.",
    }
}

/// The `confirm` layer: for a risky action, one question says whether to ask first. If so, the request is held
/// (`st.pending`, ten minutes) and this turn answers with the question.
async fn confirm_layer(t: &Turn<'_>, info: &RouteInfo) -> Option<Finish> {
    let kind = risk_kind(&info.intent, &t.message)?;
    let state = json!({
        "channel": "voice",
        "message": t.message,
        "intent": info.intent,
        "risk": kind,
    });
    let q = [(
        "confirm",
        Question::noul("Must Lucina ask the owner \"are you sure?\" by voice, and wait for a yes, before doing this? Answer yes when the action is hard to undo, costs money, or sends words to someone else; answer no when it is harmless, easy to undo, or the owner's words already make exactly this action unmistakable."),
    )];
    match layer_call(t, "voice_confirm", state, &q).await {
        Ok((d, ms)) => {
            let p = d.yes("confirm").unwrap_or(0.0);
            let (verdict, confidence) = yes_no(p, "ask", "act");
            let ask = verdict == "ask" && confidence >= t.st.cfg.voice_confirm_min;
            push(Layer {
                layer: "confirm",
                verdict,
                confidence,
                ms,
                used: ask,
            });
            if !ask {
                return None;
            }
            t.st.pending.create(
                &t.session,
                CONFIRM_TOOL,
                json!({"message": t.message, "intent": info.intent, "risk": kind}),
                &format!("spoken {kind}"),
                Utc::now(),
            );
            let lang = crate::jev_yesno::turn_lang_code();
            let mut f = Finish::reply(confirm_question(kind, lang), CONFIRM_ACTION);
            f.action_result = format!("[voice confirm asked: {kind}]");
            Some(f)
        }
        Err((reason, ms)) => {
            push_failed("confirm", reason, ms);
            None
        }
    }
}

/// What a spoken turn does with its session's pending "sei sicuro?".
pub(super) enum Held {
    /// None pending (or the turn is something else: the pending question is dropped).
    None,
    /// A yes: run this held request.
    Run(String),
    /// A no: answer with this text.
    Cancel(String),
}

/// Settles the session's pending spoken confirmation with this turn's words: only an explicit yes runs the
/// held request; a no cancels it; anything else drops it (the turn is read as itself).
pub(super) fn settle_confirmation(st: &AppState, session: &str, message: &str) -> Held {
    let Some(p) = st
        .pending
        .current(session, Utc::now())
        .filter(|p| p.tool == CONFIRM_TOOL)
    else {
        return Held::None;
    };
    let Some(action) = st.pending.take(&p.id) else {
        return Held::None;
    };
    let word = |verdict: &str, used: bool| {
        push(Layer {
            layer: "confirm",
            verdict: verdict.to_owned(),
            confidence: 1.0,
            ms: 0,
            used,
        })
    };
    match classify_reply(message) {
        Reply::Yes => match action.args["message"].as_str().filter(|m| !m.is_empty()) {
            Some(m) => {
                word("yes", true);
                Held::Run(m.to_owned())
            }
            None => Held::None,
        },
        Reply::No => {
            word("no", true);
            Held::Cancel(cancel_text(crate::jev_yesno::turn_lang_code()).to_owned())
        }
        Reply::Other => {
            word("dropped", false);
            Held::None
        }
    }
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
                layers: Vec::new(),
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
                    layers: Vec::new(),
                },
                None => RouteInfo {
                    intent: "unknown".into(),
                    confidence: 0.0,
                    jev_ms: *ms,
                    fast: false,
                    source: "fallback",
                    layers: Vec::new(),
                },
            };
            (info, None)
        }
    }
}

/// The turn's reply language when a template exists for it (`planner::TURN_LANG`, the same detection as
/// every other reply).
fn template_lang() -> Option<&'static str> {
    // Owner rule (2026-11-02, confirmed for spoken replies 2026-11-08): English, or Danish when he
    // writes Danish; never Italian. Every template below has both.
    match crate::jev_yesno::turn_lang_code() {
        "da" => Some("da"),
        _ => Some("en"),
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
                .then(|| Plan::ShoppingWrite(Vec::new()))
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
        Plan::ShoppingWrite(pieces) if !pieces.is_empty() => shopping_pieces(t, &pieces).await,
        Plan::ShoppingWrite(_) => {
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

/// A shopping sentence the `slots` layer split: every product goes through the grocery path on its own, and
/// one sentence says what was done.
async fn shopping_pieces(t: &Turn<'_>, pieces: &[(String, String)]) -> Finish {
    let st = t.st;
    let mut intent = String::new();
    let mut names = Vec::new();
    let mut list_id = None;
    for (message, name) in pieces {
        match crate::grocery_turn::process(
            st.grocery.as_deref(),
            &st.cfg.grocery_user_id,
            message,
            "",
            &t.channel,
        )
        .await
        {
            Ok(Some(h)) => {
                intent = h.intent;
                list_id = list_id.or(h.list_id);
                names.push(name.clone());
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %e, "secretary grocery turn failed");
                return Finish::reply(
                    "I could not update grocery accounting right now. Your purchase was not recorded.",
                    "grocery",
                );
            }
        }
    }
    if names.is_empty() {
        return Finish::reply("I could not do that just now.", "grocery");
    }
    let lang = lucina_grocery::message_language(&t.message, None);
    // The reply is English or Danish (shopping_reply), so is the word that joins the names.
    let joiner = if lang == "da" { " og " } else { " and " };
    let kind = if intent.ends_with(".remove") {
        "removed"
    } else {
        "added"
    };
    let reply = crate::grocery_turn::shopping_reply(lang, kind, &names.join(joiner));
    let meta = list_id.map(|id| Meta {
        list_id: Some(id),
        ..Meta::kind("shopping")
    });
    Finish::reply(reply, "grocery").with(intent, meta)
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

// Spoken replies are English, or Danish when the turn is Danish; never Italian (owner rule
// 2026-11-02, confirmed for these replies 2026-11-08).

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

/// "14.05", "9.00": the 24-hour clock as Danish writes and says it.
fn da_clock(h: u32, m: u32) -> String {
    format!("{h}.{m:02}")
}

const DA_DAYS: [&str; 7] = [
    "mandag", "tirsdag", "onsdag", "torsdag", "fredag", "lørdag", "søndag",
];
const DA_MONTHS: [&str; 12] = [
    "januar",
    "februar",
    "marts",
    "april",
    "maj",
    "juni",
    "juli",
    "august",
    "september",
    "oktober",
    "november",
    "december",
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
        (Clock::Time, "da") => format!("Klokken er {}.", da_clock(h, m)),
        (Clock::Time, _) => format!("It's {}.", en_clock(h, m)),
        (Clock::Date, "da") => format!(
            "I dag er det {} den {}. {} {}.",
            DA_DAYS[wd],
            now.day(),
            DA_MONTHS[now.month0() as usize],
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

/// "today at 3 PM", "on November 3rd at 9 AM" / "i dag kl. 15.00", "den 3. november kl. 9.00".
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
    if lang == "da" {
        let at = format!("kl. {}", da_clock(h, m));
        if !with_day {
            return at;
        }
        match days {
            0 => format!("i dag {at}"),
            1 => format!("i morgen {at}"),
            _ => format!(
                "den {}. {} {at}",
                local.day(),
                DA_MONTHS[local.month0() as usize]
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
    let da = lang == "da";
    let Some(due) = r.due() else {
        return if da { "Klaret.".into() } else { "Done.".into() };
    };
    let text = spoken_text(&r.text);
    let minutes = ((due - now).num_seconds() + 30) / 60;
    let when = if relative && (1..120).contains(&minutes) {
        match (minutes, da) {
            (1, true) => "om et minut".to_owned(),
            (m, true) => format!("om {m} minutter"),
            (1, false) => "in one minute".to_owned(),
            (m, false) => format!("in {m} minutes"),
        }
    } else {
        when_phrase(due, now, tz, lang, true)
    };
    match (da, created) {
        (true, true) => format!("Okay, jeg minder dig om det {when}: {text}."),
        (true, false) => format!("Den påmindelse findes allerede {when}: {text}."),
        (false, true) => format!("Okay, I'll remind you {when}: {text}."),
        (false, false) => format!("That reminder was already set {when}: {text}."),
    }
}

/// At most this many reminders are read out.
const SPOKEN_ITEMS: usize = 3;

fn reminders_reply(
    pending: &[Reminder],
    today: bool,
    lang: &str,
    tz: Tz,
    now: DateTime<Utc>,
) -> String {
    let da = lang == "da";
    if pending.is_empty() {
        return match (da, today) {
            (true, true) => "Du har ingen påmindelser i dag.".into(),
            (true, false) => "Du har ingen kommende påmindelser.".into(),
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
    let count = match (da, n) {
        (true, 1) => "én påmindelse".to_owned(),
        (true, n) => format!("{n} påmindelser"),
        (false, 1) => "one reminder".to_owned(),
        (false, n) => format!("{n} reminders"),
    };
    let head = match (da, today) {
        (true, true) => format!("Du har {count} i dag"),
        (true, false) => format!("Du har {count} forude"),
        (false, true) => format!("You have {count} today"),
        (false, false) => format!("You have {count} coming up"),
    };
    let tail = match (more, da) {
        (0, _) => String::new(),
        (k, true) => format!("; og {k} mere"),
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
    // English or Danish, never Italian (owner rule 2026-11-02, as `shopping_reply`); the item
    // names stay in the message's language.
    match lang {
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
    #[test]
    fn product_phrases_split_on_the_usual_separators() {
        assert_eq!(split_products("latte e uova"), ["latte", "uova"]);
        assert_eq!(
            split_products("latte, uova e pane"),
            ["latte", "uova", "pane"]
        );
        assert_eq!(split_products("milk and eggs"), ["milk", "eggs"]);
        assert_eq!(split_products("mælk og æg"), ["mælk", "æg"]);
        assert!(split_products("latte").is_empty());
        assert!(
            split_products("a, b, c, d, e, f, g").is_empty(),
            "more than six is not a list"
        );
    }

    #[test]
    fn a_reminder_parse_is_unsure_without_a_said_time_or_with_a_long_text() {
        // A relative timer and a clock time are sure.
        assert!(!reminder_unsure(
            "tra 20 minuti ricordami di spegnere il forno",
            "spegnere il forno",
            true
        ));
        assert!(!reminder_unsure(
            "ricordami domani alle 9 di pagare",
            "pagare",
            false
        ));
        // A day with no hour is a default hour.
        assert!(reminder_unsure(
            "ricordami domani di chiamare Marco",
            "chiamare Marco",
            false
        ));
        // A long text rode a second clause along.
        assert!(reminder_unsure(
            "ricordami domani alle 9 di chiamare Marco e poi di portare fuori il cane e di comprare il latte",
            "chiamare Marco e poi di portare fuori il cane e di comprare il latte",
            false
        ));
        // The parser removed nothing.
        assert!(reminder_unsure("domani", "domani", false));
    }

    #[test]
    fn only_risky_actions_get_the_confirm_question() {
        assert_eq!(
            risk_kind("agent_message", "dì a Claude di controllare"),
            Some("message")
        );
        assert_eq!(
            risk_kind("social_post", "pubblica la notizia"),
            Some("post")
        );
        assert_eq!(
            risk_kind("computer_task", "ordina la pizza su Wolt"),
            Some("purchase")
        );
        assert_eq!(
            risk_kind("computer_task", "apri Outlook e leggi la posta"),
            None
        );
        assert_eq!(
            risk_kind("calendar_event", "cancella l'appuntamento di domani"),
            Some("delete")
        );
        assert_eq!(
            risk_kind("unknown", "elimina tutti i promemoria"),
            Some("delete")
        );
        // Plain talk about deleting is not a delete; reads and list edits are never asked about.
        assert_eq!(
            risk_kind("chat", "come si elimina un messaggio su Telegram?"),
            None
        );
        assert_eq!(
            risk_kind("grocery.shopping_list.remove", "togli il latte dalla lista"),
            None
        );
        assert_eq!(risk_kind("temporal_query", "che ore sono?"), None);
        assert_eq!(
            risk_kind("schedule_reminder", "ricordami domani alle 9 di pagare"),
            None
        );
    }

    #[test]
    fn the_confirmation_is_said_in_the_turns_language() {
        assert!(
            confirm_question("message", "it").starts_with("Are you sure?"),
            "never Italian"
        );
        assert!(confirm_question("purchase", "en").starts_with("Are you sure?"));
        assert!(confirm_question("delete", "da").starts_with("Er du sikker?"));
        assert!(
            !confirm_question("message", "it").contains("Claude"),
            "no transcript in the question"
        );
    }

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
    fn clocks_are_said_in_english_and_danish() {
        assert_eq!(en_clock(14, 5), "2:05 PM");
        assert_eq!(en_clock(9, 0), "9 AM");
        assert_eq!(en_clock(0, 0), "midnight");
        assert_eq!(en_clock(0, 7), "12:07 AM");
        assert_eq!(en_clock(12, 0), "noon");
        assert_eq!(da_clock(14, 5), "14.05");
        assert_eq!(da_clock(9, 0), "9.00");
        assert_eq!(da_clock(0, 0), "0.00");
    }

    #[test]
    fn the_clock_replies_name_the_day_and_the_time() {
        let sunday = at(2026, 11, 1, 14, 5);
        assert_eq!(clock_reply(Clock::Time, "en", sunday), "It's 2:05 PM.");
        assert_eq!(clock_reply(Clock::Time, "da", sunday), "Klokken er 14.05.");
        assert_eq!(
            clock_reply(Clock::Date, "en", sunday),
            "Today is Sunday, November 1st, 2026."
        );
        assert_eq!(
            clock_reply(Clock::Date, "da", sunday),
            "I dag er det søndag den 1. november 2026."
        );
        // An Italian turn is answered in English (owner rule 2026-11-02): no Italian template.
        assert_eq!(clock_reply(Clock::Time, "it", sunday), "It's 2:05 PM.");
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
    fn reminders_are_read_out_briefly_in_english_or_danish() {
        let tz = chrono_tz::Europe::Rome;
        let now = at(2026, 11, 1, 10, 0).with_timezone(&Utc);
        assert_eq!(
            reminders_reply(&[], true, "en", tz, now),
            "You have no reminders for today."
        );
        assert_eq!(
            reminders_reply(&[], false, "en", tz, now),
            "You have no upcoming reminders."
        );
        assert_eq!(
            reminders_reply(&[], true, "da", tz, now),
            "Du har ingen påmindelser i dag."
        );
        assert_eq!(
            reminders_reply(&[], true, "it", tz, now),
            "You have no reminders for today.",
            "an Italian turn gets English"
        );
        let one = [reminder("chiamare Anna", at(2026, 11, 1, 15, 30))];
        assert_eq!(
            reminders_reply(&one, true, "en", tz, now),
            "You have one reminder today: at 3:30 PM, chiamare Anna."
        );
        assert_eq!(
            reminders_reply(&one, true, "da", tz, now),
            "Du har én påmindelse i dag: kl. 15.30, chiamare Anna."
        );
        let many: Vec<Reminder> = (0..5)
            .map(|i| reminder(&format!("ting {i}"), at(2026, 11, 2 + i, 9, 0)))
            .collect();
        assert_eq!(
            reminders_reply(&many, false, "en", tz, now),
            "You have 5 reminders coming up: tomorrow at 9 AM, ting 0; on November 3rd at 9 AM, ting 1; on November 4th at 9 AM, ting 2; and 2 more."
        );
        assert_eq!(
            reminders_reply(&many, false, "da", tz, now),
            "Du har 5 påmindelser forude: i morgen kl. 9.00, ting 0; den 3. november kl. 9.00, ting 1; den 4. november kl. 9.00, ting 2; og 2 mere."
        );
    }

    #[test]
    fn a_new_reminder_is_confirmed_with_when_and_what() {
        let tz = chrono_tz::Europe::Rome;
        let now = at(2026, 11, 1, 10, 0).with_timezone(&Utc);
        let r = reminder("spegnere il forno", at(2026, 11, 1, 10, 20));
        assert_eq!(
            reminder_reply(&r, true, true, "en", tz, now),
            "Okay, I'll remind you in 20 minutes: spegnere il forno."
        );
        assert_eq!(
            reminder_reply(&r, true, true, "da", tz, now),
            "Okay, jeg minder dig om det om 20 minutter: spegnere il forno."
        );
        assert_eq!(
            reminder_reply(&r, true, true, "it", tz, now),
            "Okay, I'll remind you in 20 minutes: spegnere il forno.",
            "an Italian turn gets English"
        );
        let r = reminder("pagare l'affitto", at(2026, 11, 2, 9, 0));
        assert_eq!(
            reminder_reply(&r, false, false, "en", tz, now),
            "That reminder was already set tomorrow at 9 AM: pagare l'affitto."
        );
        assert_eq!(
            reminder_reply(&r, false, false, "da", tz, now),
            "Den påmindelse findes allerede i morgen kl. 9.00: pagare l'affitto."
        );
    }

    #[test]
    fn the_shopping_list_is_read_out_with_a_limit() {
        let names: Vec<String> = ["latte", "uova", "pane"].map(String::from).to_vec();
        assert_eq!(
            shopping_list_reply(&names, "it"),
            "You have latte, uova and pane on your shopping list.",
            "Italian messages get an English reply (owner rule 2026-11-02)"
        );
        assert_eq!(
            shopping_list_reply(&names, "en"),
            "You have latte, uova and pane on your shopping list."
        );
        assert_eq!(
            shopping_list_reply(&[], "it"),
            "Your shopping list is empty."
        );
        let many: Vec<String> = (0..11).map(|i| format!("x{i}")).collect();
        assert_eq!(
            shopping_list_reply(&many, "it"),
            "You have x0, x1, x2, x3, x4, x5, x6, x7 and 3 more things on your shopping list."
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
