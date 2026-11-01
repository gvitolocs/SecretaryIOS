//! The owner's yes/no questions, answered by Jev (TypeSafe's decision model, crate `lucina_jev`).
//!
//! - **Screen:** [`looks_like_question`] is a cheap word check (a `?`, or a yes/no lead word for
//!   voice and Alexa turns, never an open "what / cosa / hvad" question), so plain messages cost no call.
//! - **Gate:** one Jev call ([`gate`]): is it a closed yes/no question ([`GATE_MIN`], 0.75), which
//!   [`Kind`] (his own data or plans / general knowledge / opinion or advice / a live lookup / about
//!   Lucina herself), and is it
//!   medical, legal or financial.
//! - **State:** what Lucina knows that may matter ([`gather`]): the calendar for the next week, the
//!   shopping list, pending reminders, the gym week and body weight, as numbered facts; her memory
//!   context and the recent turns as background. Live sources (calendar, list) go first.
//! - **Answer:** a second Jev call ([`answer`]): P(yes), whether the facts settle it (`grounded`) and
//!   which fact is the reason (`evidence`). From [`ANSWER_MIN`] (0.75) up, or down to 1 - 0.75, the reply
//!   is "Sì / Ja / Yes" or "No / Nej" with the percentage, a confidence word and one sentence of reason.
//!   The reason is the evidence fact put into one sentence by Lucina's text model (thinking off) from
//!   the facts only, else the fact itself; Jev writes no text.
//! - **Otherwise:** in between she says she is not sure and her normal path answers ([`Outcome::Unsure`]);
//!   a medical, legal or financial question gets a hedge line and the normal path ([`Outcome::Hedge`]);
//!   a question about Lucina herself or the conversation ("sei sicura?", "are you online?"), a live
//!   question the facts do not settle, Jev down or over quota, or the feature switched off go on to the
//!   normal path unchanged ([`Outcome::Pass`]).
//! - **Safety:** this only answers. A "yes" here never starts an action: the turn's action is
//!   [`ACTION`], with no job, tool or decision behind it; go-aheads still go through `crate::decide`.
//! - Each turn logs one `event="jev_yesno"` line (`crate::jev_drift` counts them).
//!   `POST /api/secretary/jev/yesno` replays a text through the same path without a turn.
//!
//! Flags: `LUCINA_JEV_YESNO=off`, `LUCINA_JEV_YESNO_GATE_MIN`, `LUCINA_JEV_YESNO_MIN`,
//! `LUCINA_JEV_YESNO_GROUNDED_MIN`, `LUCINA_JEV_YESNO_SENSITIVE_MIN`.

use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use lucina_jev::{Jev, Question};
use lucina_llm::{ChatRequest, Message};
use reqwest::Method;
use serde_json::{json, Value};

use crate::AppState;

/// The turn's action: an answer, nothing run.
pub const ACTION: &str = "yesno";
/// A yes/no question from this P up (`LUCINA_JEV_YESNO_GATE_MIN`).
pub const GATE_MIN: f64 = 0.75;
/// "Yes" from this P(yes) up, "no" from 1 - this down (`LUCINA_JEV_YESNO_MIN`).
pub const ANSWER_MIN: f64 = 0.75;
/// The facts settle a personal or live question from this P up (`LUCINA_JEV_YESNO_GROUNDED_MIN`).
pub const GROUNDED_MIN: f64 = 0.6;
/// Medical, legal or financial from this P up (`LUCINA_JEV_YESNO_SENSITIVE_MIN`; 0.5 hedged "is oracle
/// alright?", his server, as financial in the replay).
pub const SENSITIVE_MIN: f64 = 0.75;
/// Numbered facts in the state (Jev takes up to 255 options; the reason question offers each).
pub const MAX_FACTS: usize = 80;
// The facts plus "none" fit one Jev choice question.
const _: () = assert!(MAX_FACTS < lucina_jev::MAX_OPTIONS);
const SOURCE_TIMEOUT: Duration = Duration::from_millis(2500);
const REASON_TIMEOUT: Duration = Duration::from_secs(5);
const BACKGROUND_CHARS: usize = 3500;
const RECENT_CHARS: usize = 1200;

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| (0.0..=1.0).contains(v))
        .unwrap_or(default)
}

pub fn enabled() -> bool {
    !std::env::var("LUCINA_JEV_YESNO").is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        )
    })
}

pub fn gate_min() -> f64 {
    env_f64("LUCINA_JEV_YESNO_GATE_MIN", GATE_MIN)
}
pub fn answer_min() -> f64 {
    env_f64("LUCINA_JEV_YESNO_MIN", ANSWER_MIN)
}
pub fn grounded_min() -> f64 {
    env_f64("LUCINA_JEV_YESNO_GROUNDED_MIN", GROUNDED_MIN)
}
pub fn sensitive_min() -> f64 {
    env_f64("LUCINA_JEV_YESNO_SENSITIVE_MIN", SENSITIVE_MIN)
}

// ---------------------------------------------------------------- screen

/// Open-question words: such a message is never a yes/no question, so no Jev call.
const OPEN_WORDS: &[&str] = &[
    "what", "when", "where", "who", "whom", "whose", "why", "how", "which", "cosa", "che", "quando",
    "dove", "chi", "perché", "perche", "come", "quanto", "quanti", "quanta", "quante", "quale",
    "quali", "hvad", "hvornår", "hvor", "hvem", "hvorfor", "hvordan", "hvilken", "hvilke", "hvilket",
    // the stems of "cos'è", "com'è", "dov'è", "quant'è"
    "cos", "com", "dov", "quant",
];

/// Words an English or Danish yes/no question starts with when it has no `?` (voice and Alexa transcripts).
const LEAD_WORDS: &[&str] = &[
    // en
    "is", "isn't", "are", "aren't", "am", "was", "wasn't", "were", "do", "does", "doesn't", "did",
    "didn't", "don't", "have", "has", "had", "haven't", "hasn't", "can", "can't", "could", "will",
    "won't", "would", "should", "shall", "may", "might", "must",
    // da (Italian asks with intonation only: "ho palestra oggi" is also a statement, so it needs a `?`)
    "har", "er", "skal", "kan", "må", "vil", "bør", "gør", "gjorde", "var", "havde", "findes",
    "regner",
];

fn first_word(text: &str) -> String {
    let t = text.trim_start_matches(|c: char| !c.is_alphanumeric());
    let end = t
        .char_indices()
        .find(|(_, c)| !(c.is_alphanumeric() || *c == '\'' || *c == '’'))
        .map_or(t.len(), |(i, _)| i);
    t[..end].replace('’', "'").to_lowercase()
}

/// Worth asking Jev about: question-shaped, short, no link or command.
pub fn looks_like_question(text: &str) -> bool {
    let t = text.trim();
    let n = t.chars().count();
    if !(3..=280).contains(&n) || t.starts_with('/') || t.contains("://") {
        return false;
    }
    let first = first_word(t);
    // "what's", "cos'è": the part before the apostrophe too.
    let stem = first.split('\'').next().unwrap_or("");
    if OPEN_WORDS.contains(&first.as_str()) || OPEN_WORDS.contains(&stem) {
        return false;
    }
    t.contains('?') || LEAD_WORDS.contains(&first.as_str())
}

// ---------------------------------------------------------------- gate

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Personal,
    General,
    Opinion,
    Live,
    /// About Lucina or the conversation ("are you sure?", "are you online?", "did you do it?").
    Lucina,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Personal => "personal",
            Kind::General => "general",
            Kind::Opinion => "opinion",
            Kind::Live => "live",
            Kind::Lucina => "about_lucina",
        }
    }
    fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "personal" => Kind::Personal,
            "general" => Kind::General,
            "opinion" => Kind::Opinion,
            "live" => Kind::Live,
            "about_lucina" => Kind::Lucina,
            _ => return None,
        })
    }
}

/// The kinds, his own data first (Jev leans towards the first option).
pub const KIND_OPTIONS: [(&str, &str); 5] = [
    ("personal", "About the owner's own data, plans or life: his calendar, lectures, appointments, gym, shopping list, reminders, notes, tasks, body weight, or what he said or did."),
    ("general", "General knowledge: a fact about the world that does not change from day to day."),
    ("opinion", "Asks for an opinion, advice or a recommendation (should I…?, is it a good idea…?, dovrei…?, bør jeg…?)."),
    ("live", "Needs a live fact from outside Lucina's own data: weather, opening hours, prices, news, traffic, departures, whether a site or service is up right now."),
    ("about_lucina", "About Lucina herself or the conversation: whether she is sure, understood him, can hear him, is online, did what he asked, can do or access something, or how she works."),
];

pub const SENSITIVE_OPTIONS: [(&str, &str); 4] = [
    ("none", "Not a medical, legal or financial question."),
    ("medical", "About health, symptoms, medicines, doses or treatment."),
    ("legal", "About the law, rights, contracts, permits, taxes owed or legal risk."),
    ("financial", "About investments, loans, buying or selling assets, or other money decisions. A company or server name alone (Oracle, p1, nezopt) is not financial."),
];

#[derive(Debug, Clone, PartialEq)]
pub struct Gate {
    /// P(a closed yes/no question).
    pub p: f64,
    pub kind: Kind,
    pub kind_p: f64,
    /// "medical" / "legal" / "financial" from [`SENSITIVE_MIN`] up.
    pub sensitive: Option<&'static str>,
    pub ms: u64,
    pub input_tokens: u64,
}

impl Gate {
    pub fn is_yes_no(&self) -> bool {
        self.p >= gate_min()
    }
}

fn gate_questions() -> Vec<(&'static str, Question)> {
    vec![
        (
            "yes_no",
            Question::noul_with(
                "Is the owner's latest message (`message`) a closed question that asks whether something is true, so that yes or no answers it? He writes Italian, Danish or English. Use `recent_turns` only to understand a follow-up.",
                "A closed yes/no question, e.g. 'Ho palestra oggi?', 'Is milk on my shopping list?', 'Har jeg en forelæsning i morgen?', 'Dovrei andare a correre?', 'Is Copenhagen the capital of Denmark?'.",
                "Not one: an open question (what, when, where, which, how much), a request or command phrased as a question ('can you add milk?', 'puoi ricordarmi…?', 'kan du åbne…?'), a greeting, small talk, or a statement.",
            ),
        ),
        (
            "kind",
            Question::choice(
                "If the message is a yes/no question, what does answering it need?",
                KIND_OPTIONS,
            ),
        ),
        (
            "sensitive",
            Question::choice(
                "Is the message a medical, legal or financial question?",
                SENSITIVE_OPTIONS,
            ),
        ),
    ]
}

/// Jev's gate, `None` when Jev is off, cooling down or failed (the turn goes on unchanged).
pub async fn gate(jev: &Jev, text: &str, recent: &str) -> Option<Gate> {
    if !jev.available() {
        return None;
    }
    let recent: String = tail_chars(recent, 800);
    let state = json!({"message": text, "recent_turns": recent});
    let d = jev.decide("yesno_gate", state, &gate_questions()).await.ok()?;
    let p = d.yes("yes_no")?;
    let (kind, kind_p) = d.choice("kind")?;
    let kind = Kind::from_name(kind)?;
    let sensitive = SENSITIVE_OPTIONS[1..]
        .iter()
        .map(|(k, _)| (*k, d.probability("sensitive", k).unwrap_or(0.0)))
        .filter(|(_, p)| *p >= sensitive_min())
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(k, _)| k);
    Some(Gate {
        p,
        kind,
        kind_p,
        sensitive,
        ms: d.elapsed_ms,
        input_tokens: d.input_tokens,
    })
}

// ---------------------------------------------------------------- state

#[derive(Debug, Clone, PartialEq)]
pub struct Fact {
    /// calendar, shopping, reminders, gym, body.
    pub source: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    /// "Thursday 29 October 2026, 12:47 (Europe/Copenhagen) …".
    pub now: String,
    pub facts: Vec<Fact>,
    /// Memory context and recent turns: older or partial, never offered as the reason.
    pub background: String,
}

fn tail_chars(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_owned();
    }
    format!("…{}", s.chars().skip(n - max).collect::<String>())
}

fn clip(s: &str, max: usize) -> String {
    lucina_jev::clip(s.trim(), max)
}

/// The "now" line: weekday, date, time and the names of today and tomorrow.
pub fn now_line(now: DateTime<Utc>, tz: Tz) -> String {
    let local = now.with_timezone(&tz);
    let tomorrow = local.date_naive().succ_opt().unwrap_or(local.date_naive());
    format!(
        "Now: {} ({}). Today is {} {}, tomorrow is {} {}.",
        local.format("%A %-d %B %Y, %H:%M"),
        tz.name(),
        local.format("%a"),
        local.format("%Y-%m-%d"),
        tomorrow.format("%a"),
        tomorrow.format("%Y-%m-%d"),
    )
}

/// The local day of a calendar event's `start` (`YYYY-MM-DD` or RFC 3339).
fn event_day(e: &Value, tz: Tz) -> Option<NaiveDate> {
    let start = e["start"].as_str()?;
    if start.len() == 10 {
        return NaiveDate::parse_from_str(start, "%Y-%m-%d").ok();
    }
    DateTime::parse_from_rfc3339(start)
        .ok()
        .map(|d| d.with_timezone(&tz).date_naive())
}

/// One fact per event: "Thu 2026-10-29 (today) 08:00–12:00 Research Methodology … @ 5124-038".
pub fn calendar_facts(events: &[Value], now: DateTime<Utc>, tz: Tz) -> Vec<Fact> {
    let today = now.with_timezone(&tz).date_naive();
    events
        .iter()
        .filter_map(|e| {
            let day = event_day(e, tz)?;
            let line = lucina_intent::calendar::event_lines(std::slice::from_ref(e), tz, "en")
                .pop()?;
            let rel = match (day - today).num_days() {
                0 => " (today)",
                1 => " (tomorrow)",
                _ => "",
            };
            Some(Fact {
                source: "calendar",
                text: clip(
                    &format!("{} {}{rel} {line}", day.format("%a"), day.format("%Y-%m-%d")),
                    240,
                ),
            })
        })
        .collect()
}

/// Which days the calendar facts cover: anything else is unknown, not empty.
pub fn calendar_coverage(ranges: &[(String, String)]) -> String {
    let r: Vec<String> = ranges
        .iter()
        .map(|(f, t)| if f == t { f.clone() } else { format!("{f}..{t}") })
        .collect();
    format!(
        "Calendar shown for {} only; other days are not shown here (unknown, not empty).",
        r.join(" and ")
    )
}

/// The shopping list: one fact per item to buy, one for the recently bought ones, or "nothing to buy".
pub fn shopping_facts(items: &[Value]) -> Vec<Fact> {
    let name = |i: &Value| {
        let product = i["product"].as_str().unwrap_or("").trim().to_owned();
        match i["quantity"].as_str().or(i["qty"].as_str()).map(str::trim) {
            Some(q) if !q.is_empty() && !product.is_empty() => format!("{product} ({q})"),
            _ => product,
        }
    };
    let purchased = |i: &&Value| i["status"].as_str() == Some("purchased");
    let mut out: Vec<Fact> = items
        .iter()
        .filter(|i| !purchased(i))
        .map(name)
        .filter(|n| !n.is_empty())
        .take(40)
        .map(|n| Fact {
            source: "shopping",
            text: format!("Shopping list, still to buy: {n}"),
        })
        .collect();
    if out.is_empty() {
        out.push(Fact {
            source: "shopping",
            text: "Shopping list: nothing left to buy.".into(),
        });
    }
    let bought: Vec<String> = items.iter().filter(purchased).map(name).take(15).collect();
    if !bought.is_empty() {
        out.push(Fact {
            source: "shopping",
            text: clip(
                &format!("Shopping list, recently bought (not to buy): {}", bought.join(", ")),
                400,
            ),
        });
    }
    out
}

/// Pending reminders: "Reminder Thu 2026-10-30 09:00: call the dentist".
pub fn reminder_facts(reminders: &[lucina_store::reminders::Reminder], tz: Tz) -> Vec<Fact> {
    let mut out: Vec<Fact> = reminders
        .iter()
        .filter(|r| r.status == "pending")
        .take(15)
        .map(|r| {
            let when = DateTime::parse_from_rfc3339(&r.due_at)
                .map(|d| d.with_timezone(&tz).format("%a %Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|_| r.due_at.clone());
            Fact {
                source: "reminders",
                text: clip(&format!("Reminder {when}: {}", r.text), 240),
            }
        })
        .collect();
    if out.is_empty() {
        out.push(Fact {
            source: "reminders",
            text: "Reminders: none pending.".into(),
        });
    }
    out
}

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "?".into(),
        other => other.to_string(),
    }
}

/// `GET /v1/gym/state`: this week's workouts against the goal, and a workout in progress.
pub fn gym_facts(state: &Value) -> Vec<Fact> {
    let view = &state["view"];
    let sum = &view["summary"];
    if !sum.is_object() {
        return Vec::new();
    }
    let mut out = vec![Fact {
        source: "gym",
        text: format!(
            "Gym this week: {} of {} workouts done (weekly goal). Gym days are not in a fixed schedule unless the calendar shows them.",
            scalar(&sum["weekly_workouts"]),
            scalar(&sum["weekly_goal"])
        ),
    }];
    if view["state"]["active"].is_object() {
        out.push(Fact {
            source: "gym",
            text: format!(
                "A gym workout is in progress right now ({} sets so far).",
                scalar(&sum["active_completed_sets"])
            ),
        });
    }
    out
}

/// `GET /v1/body/state`: current weight, 7-day average, goal.
pub fn body_facts(state: &Value) -> Vec<Fact> {
    let view = &state["view"];
    let labels = &view["stat_labels"];
    if view["stats"]["count"].as_u64().unwrap_or(0) == 0 {
        return Vec::new();
    }
    let l = |k: &str| labels[k].as_str().unwrap_or("n/a").to_owned();
    let mut text = format!(
        "Body weight: current {}, 7-day average {}, weekly change {}, 30-day trend {}",
        l("current"),
        l("avg7"),
        l("weekly_change"),
        l("trend30")
    );
    if labels.get("goal").is_some() {
        let reached = view["stats"]["goal_reached"].as_bool() == Some(true);
        text.push_str(&format!(
            "; goal {} ({} to go{})",
            l("goal"),
            l("goal_distance"),
            if reached { ", reached" } else { "" }
        ));
    }
    text.push('.');
    vec![Fact {
        source: "body",
        text,
    }]
}

async fn within<T>(fut: impl std::future::Future<Output = Vec<T>>) -> Vec<T> {
    tokio::time::timeout(SOURCE_TIMEOUT, fut)
        .await
        .unwrap_or_default()
}

fn unavailable(source: &'static str) -> Vec<Fact> {
    vec![Fact {
        source,
        text: format!("{source}: unreadable right now (do not guess)."),
    }]
}

/// What Lucina knows that may matter, read in parallel (each source at most 2.5 s; a failed source says
/// so instead of looking empty).
pub async fn gather(
    st: &AppState,
    question: &str,
    memory: &str,
    recent: &str,
    now: DateTime<Utc>,
) -> Facts {
    let tz = st.cfg.time_zone;
    let local = now.with_timezone(&tz).date_naive();
    let from = local.format("%Y-%m-%d").to_string();
    let to = (local + chrono::Days::new(7)).format("%Y-%m-%d").to_string();
    // A day he names outside the next week ("per il 28 agosto…") is read too.
    let asked = lucina_intent::calendar::parse_question(question, now, tz)
        .map(|q| (q.from, q.to))
        .filter(|(f, t)| *f < from || *t > to);
    let calendar = within(async {
        if !st.cfg.calendar_enabled {
            return Vec::new();
        }
        let mut ranges = vec![(from.clone(), to.clone())];
        ranges.extend(asked.clone());
        let mut facts = vec![Fact {
            source: "calendar",
            text: calendar_coverage(&ranges),
        }];
        for (f, t) in &ranges {
            let r = crate::bridges::events(&st.secretary, f, t).await;
            if !r.ok {
                return unavailable("calendar");
            }
            let evs = r.data["events"].as_array().cloned().unwrap_or_default();
            let found = calendar_facts(&evs, now, tz);
            if found.is_empty() {
                facts.push(Fact {
                    source: "calendar",
                    text: format!("Calendar {f}..{t}: nothing scheduled."),
                });
            }
            for e in found {
                if !facts.contains(&e) {
                    facts.push(e);
                }
            }
        }
        facts
    });
    let shopping = within(async {
        let Some(repo) = st.grocery.as_deref() else {
            return Vec::new();
        };
        match repo.legacy_list(&st.cfg.grocery_user_id).await {
            Ok(items) => shopping_facts(&items),
            Err(e) => {
                tracing::warn!(error = %e, "yes/no state: shopping list failed");
                unavailable("shopping list")
            }
        }
    });
    let reminders = within(async {
        if !st.cfg.reminders_enabled {
            return Vec::new();
        }
        match crate::api::owner_reminders(st) {
            Ok(r) => reminder_facts(&r, tz),
            Err(_) => unavailable("reminders"),
        }
    });
    let tz_q = crate::bridges::enc(tz.name());
    let gym = within(async {
        let r = st
            .secretary
            .call(Method::GET, &format!("/v1/gym/state?timezone={tz_q}"), None)
            .await;
        if r.ok { gym_facts(&r.data) } else { Vec::new() }
    });
    let body = within(async {
        let r = st
            .secretary
            .call(Method::GET, &format!("/v1/body/state?timezone={tz_q}"), None)
            .await;
        if r.ok { body_facts(&r.data) } else { Vec::new() }
    });
    let (calendar, shopping, reminders, gym, body) =
        tokio::join!(calendar, shopping, reminders, gym, body);
    let mut facts: Vec<Fact> = [calendar, shopping, reminders, gym, body]
        .into_iter()
        .flatten()
        .collect();
    facts.truncate(MAX_FACTS);
    Facts {
        now: now_line(now, tz),
        facts,
        background: background(memory, recent),
    }
}

/// Memory and recent turns, clipped (recent turns keep their newest end).
pub fn background(memory: &str, recent: &str) -> String {
    [
        clip(memory, BACKGROUND_CHARS),
        tail_chars(recent.trim(), RECENT_CHARS),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n")
}

/// Jev's state: the question, the numbered facts (live sources first), then the background.
pub fn state_text(question: &str, kind: Kind, facts: &Facts) -> String {
    let mut out = format!(
        "{}\nThe owner, Giuseppe, asks Lucina (his assistant): \"{}\"\nKind of question: {}.\n",
        facts.now,
        question.trim(),
        kind.name()
    );
    if kind == Kind::General {
        out.push_str("Answer from general knowledge.\n");
        return out;
    }
    out.push_str("Facts Lucina knows, one per line (for his own data these are all there is: what a list or calendar below does not contain is not there):\n");
    for (i, f) in facts.facts.iter().enumerate() {
        out.push_str(&format!("f{} [{}] {}\n", i + 1, f.source, f.text));
    }
    if facts.facts.is_empty() {
        out.push_str("(none)\n");
    }
    if !facts.background.is_empty() {
        out.push_str(&format!(
            "\nBackground (Lucina's memory and the recent conversation; may be old or partial):\n{}\n",
            facts.background
        ));
    }
    out
}

// ---------------------------------------------------------------- answer

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub p_yes: f64,
    /// The facts settle it (not asked for general knowledge).
    pub grounded: Option<f64>,
    /// 0-based index into the facts, and Jev's confidence in it.
    pub evidence: Option<(usize, f64)>,
    pub ms: u64,
    pub input_tokens: u64,
}

fn answer_questions(kind: Kind, facts: &Facts) -> Vec<(&'static str, Question)> {
    let answer = match kind {
        Kind::Opinion => Question::noul_with(
            "Considering the facts and common sense, is the sensible answer to the owner's question yes?",
            "Yes is the sensible answer.",
            "No is the sensible answer.",
        ),
        Kind::General => Question::noul_with(
            "Is the true answer to the owner's question yes?",
            "The answer is yes.",
            "The answer is no.",
        ),
        _ => Question::noul_with(
            "From the numbered facts, is the true answer to the owner's question yes? For his own data only the facts count: something his calendar, list or reminders do not contain is not there.",
            "The facts say yes.",
            "The facts say no.",
        ),
    };
    let mut qs = vec![("answer", answer)];
    if kind == Kind::General || facts.facts.is_empty() {
        return qs;
    }
    qs.push((
        "grounded",
        Question::noul_with(
            "Do the numbered facts settle the answer for certain, rather than leaving it to a guess?",
            "Settled: a fact says it, or a list or calendar that is shown simply does not contain the thing (then the answer is no).",
            "Not settled: the facts are about something else, a source is unreadable, or it needs information Lucina does not have.",
        ),
    ));
    let mut options: Vec<(String, String)> = facts
        .facts
        .iter()
        .enumerate()
        .map(|(i, f)| (format!("f{}", i + 1), clip(&f.text, 200)))
        .collect();
    options.push(("none".into(), "No single fact is the reason.".into()));
    qs.push((
        "evidence",
        Question::choice(
            "Which numbered fact is the main reason for the answer?",
            options,
        ),
    ));
    qs
}

pub async fn answer(jev: &Jev, question: &str, kind: Kind, facts: &Facts) -> Option<Verdict> {
    if !jev.available() {
        return None;
    }
    let qs = answer_questions(kind, facts);
    let state = Value::String(state_text(question, kind, facts));
    let d = jev.decide("yesno", state, &qs).await.ok()?;
    let evidence = d.choice("evidence").and_then(|(c, p)| {
        let i: usize = c.strip_prefix('f')?.parse().ok()?;
        (1..=facts.facts.len()).contains(&i).then_some((i - 1, p))
    });
    Some(Verdict {
        p_yes: d.yes("answer")?,
        grounded: d.yes("grounded"),
        evidence,
        ms: d.elapsed_ms,
        input_tokens: d.input_tokens,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Yes or no, with the certainty of that answer (P(yes) or 1 - P(yes)).
    Answer { yes: bool, certainty: f64 },
    /// Between the bars.
    Unsure,
    /// His own data or a live fact, and the facts do not settle it (not his calendar or list, say).
    NotSettled,
}

/// The thresholds: a sure P either way, and for his own data or a live fact, facts that settle it.
pub fn decide(kind: Kind, v: &Verdict) -> Decision {
    decide_with(kind, v, answer_min(), grounded_min())
}

pub fn decide_with(kind: Kind, v: &Verdict, min: f64, grounded: f64) -> Decision {
    let settled = match kind {
        Kind::Personal | Kind::Live | Kind::Lucina => v.grounded.is_some_and(|g| g >= grounded),
        Kind::General | Kind::Opinion => true,
    };
    if !settled {
        return Decision::NotSettled;
    }
    if v.p_yes >= min {
        Decision::Answer {
            yes: true,
            certainty: v.p_yes,
        }
    } else if v.p_yes <= 1.0 - min {
        Decision::Answer {
            yes: false,
            certainty: 1.0 - v.p_yes,
        }
    } else {
        Decision::Unsure
    }
}

// ---------------------------------------------------------------- words

/// The turn's language: it, da or en (`planner::TURN_LANG`).
pub fn turn_lang_code() -> &'static str {
    lang_code(&crate::planner::turn_lang())
}

pub fn lang_code(line: &str) -> &'static str {
    if line.starts_with("Reply language: Italian") {
        "it"
    } else if line.starts_with("Reply language: Danish") {
        "da"
    } else {
        "en"
    }
}

fn language_name(lang: &str) -> &'static str {
    match lang {
        "it" => "Italian",
        "da" => "Danish",
        _ => "English",
    }
}

pub fn answer_word(lang: &str, yes: bool) -> &'static str {
    match (lang, yes) {
        ("it", true) => "Sì",
        ("da", true) => "Ja",
        (_, true) => "Yes",
        ("da", false) => "Nej",
        (_, false) => "No",
    }
}

pub fn confidence_word(lang: &str, certainty: f64) -> &'static str {
    let level = if certainty >= 0.95 {
        0
    } else if certainty >= 0.85 {
        1
    } else {
        2
    };
    match (lang, level) {
        ("it", 0) => "sicuro",
        ("it", 1) => "quasi sicuro",
        ("it", _) => "probabile",
        ("da", 0) => "sikkert",
        ("da", 1) => "næsten sikkert",
        ("da", _) => "sandsynligt",
        (_, 0) => "certain",
        (_, 1) => "almost certain",
        _ => "likely",
    }
}

/// "Sì (93%, quasi sicuro). Oggi alle 17:00 hai Palestra."
pub fn reply_text(lang: &str, yes: bool, certainty: f64, reason: &str) -> String {
    let pct = (certainty * 100.0).round().clamp(0.0, 100.0) as u32;
    let head = format!(
        "{} ({pct}%, {})",
        answer_word(lang, yes),
        confidence_word(lang, certainty)
    );
    let reason = reason.trim();
    if reason.is_empty() {
        format!("{head}.")
    } else {
        format!("{head}. {reason}")
    }
}

/// Said before the normal answer when Jev is not sure.
pub fn unsure_line(lang: &str) -> &'static str {
    match lang {
        "it" => "Non ne sono sicura.",
        "da" => "Det er jeg ikke sikker på.",
        _ => "I'm not sure.",
    }
}

/// Said before the normal answer to a medical, legal or financial yes/no question.
pub fn hedge_line(lang: &str, topic: &str) -> String {
    let (it, da, en) = match topic {
        "medical" => ("medica", "medicinsk", "medical"),
        "legal" => ("legale", "juridisk", "legal"),
        _ => ("finanziaria", "økonomisk", "financial"),
    };
    match lang {
        "it" => format!("Su una questione {it} non ti do un sì o un no secco: prendilo come orientamento, non come un parere professionale."),
        "da" => format!("Til et {da} spørgsmål giver jeg ikke et rent ja eller nej: tag det som en vejledning, ikke som professionel rådgivning."),
        _ => format!("On a {en} question I won't give a bare yes or no: take this as orientation, not professional advice."),
    }
}

/// The facts the reason may use: the evidence first, then others of its source (or every fact, clipped).
pub fn reason_facts(facts: &Facts, evidence: Option<usize>) -> Vec<String> {
    match evidence.and_then(|i| facts.facts.get(i).map(|f| (i, f))) {
        Some((i, f)) => std::iter::once(f.text.clone())
            .chain(
                facts
                    .facts
                    .iter()
                    .enumerate()
                    .filter(|(j, g)| *j != i && g.source == f.source)
                    .map(|(_, g)| g.text.clone())
                    .take(8),
            )
            .collect(),
        None => facts.facts.iter().take(25).map(|f| f.text.clone()).collect(),
    }
}

/// One sentence of the model's, or `None` (refused, empty, several lines, too long, a yes/no word
/// first, or the model disagreeing with the answer).
pub fn clean_reason(raw: &str) -> Option<String> {
    let line = raw.trim().lines().next()?.trim();
    let line = line.trim_matches(|c| c == '"' || c == '“' || c == '”' || c == '«' || c == '»').trim();
    let up = line.to_uppercase();
    if line.chars().count() < 3
        || line.chars().count() > 240
        || up.starts_with("NONE")
        || up.starts_with("DISAGREE")
        || raw.trim().lines().filter(|l| !l.trim().is_empty()).count() > 1
    {
        return None;
    }
    let first = first_word(line);
    if ["yes", "no", "sì", "si", "ja", "nej"].contains(&first.as_str()) {
        return None;
    }
    Some(line.to_owned())
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reason {
    Text(String),
    /// The model thinks the answer is wrong (general knowledge only): treat as unsure.
    Disagree,
    None,
}

/// The reason in one sentence, by Lucina's text model (thinking off) from `facts` only. With no facts
/// (general knowledge) the model may say DISAGREE, which makes the answer unsure.
pub async fn phrase_reason(
    st: &AppState,
    lang: &str,
    question: &str,
    yes: bool,
    facts: &[String],
) -> Reason {
    let general = facts.is_empty();
    let system = format!(
        "You write ONE short sentence in {} for Lucina, the owner's assistant. The owner asked a yes/no question and the answer is {}. Give the reason in one sentence{}. Do not start with yes or no, add nothing that is not in the facts, no greeting, no emoji, no markdown. {}",
        language_name(lang),
        if yes { "YES" } else { "NO" },
        if general {
            ", from well-known general knowledge only"
        } else {
            ", using ONLY the facts listed"
        },
        if general {
            "If you are not sure the answer is right, reply with exactly: DISAGREE."
        } else {
            "If the facts do not explain the answer, reply with exactly: NONE."
        }
    );
    let user = format!(
        "Question: {}\nAnswer: {}\nFacts:\n{}",
        question.trim(),
        if yes { "yes" } else { "no" },
        if general {
            "(general knowledge)".to_owned()
        } else {
            facts.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n")
        }
    );
    let req = ChatRequest {
        messages: vec![Message::system(system), Message::user(user)],
        temperature: Some(0.2),
        max_tokens: Some(90),
        thinking: Some(false),
        purpose: "yesno-reason",
        ..ChatRequest::default()
    };
    let out = match tokio::time::timeout(REASON_TIMEOUT, st.planner.model.complete(req)).await {
        Ok(Ok(r)) => r.message.text().to_owned(),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "yes/no reason: model failed");
            return Reason::None;
        }
        Err(_) => {
            tracing::warn!("yes/no reason: model timed out");
            return Reason::None;
        }
    };
    if general && out.trim().to_uppercase().starts_with("DISAGREE") {
        return Reason::Disagree;
    }
    clean_reason(&out).map_or(Reason::None, Reason::Text)
}

// ---------------------------------------------------------------- the whole path

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Her whole reply.
    Answer { reply: String },
    /// Say this, then the normal answer.
    Unsure { line: String },
    Hedge { line: String },
    /// The normal path, unchanged; why (for the log).
    Pass { why: &'static str },
}

#[derive(Debug, Clone)]
pub struct Run {
    pub outcome: Outcome,
    pub gate: Option<Gate>,
    pub verdict: Option<Verdict>,
    pub kind: Option<Kind>,
    /// Jev milliseconds (gate + answer).
    pub jev_ms: u64,
    pub total_ms: u64,
    pub facts: usize,
}

impl Run {
    fn pass(why: &'static str, started: Instant) -> Self {
        Run {
            outcome: Outcome::Pass { why },
            gate: None,
            verdict: None,
            kind: None,
            jev_ms: 0,
            total_ms: started.elapsed().as_millis() as u64,
            facts: 0,
        }
    }

    pub fn outcome_name(&self) -> &'static str {
        match &self.outcome {
            Outcome::Answer { .. } => "answer",
            Outcome::Unsure { .. } => "unsure",
            Outcome::Hedge { .. } => "hedge",
            Outcome::Pass { .. } => "pass",
        }
    }

    /// The bubble's Jev badge (none when Jev did not shape the reply).
    pub fn badge(&self) -> Option<Value> {
        let answer = match &self.outcome {
            Outcome::Answer { .. } => match self.verdict.as_ref()?.p_yes >= 0.5 {
                true => "yes",
                false => "no",
            },
            Outcome::Unsure { .. } => "unsure",
            Outcome::Hedge { .. } => "hedge",
            Outcome::Pass { .. } => return None,
        };
        let p = match (&self.outcome, &self.verdict) {
            (Outcome::Answer { .. }, Some(v)) => v.p_yes.max(1.0 - v.p_yes),
            (_, Some(v)) => v.p_yes,
            (_, None) => self.gate.as_ref().map_or(0.0, |g| g.p),
        };
        Some(json!({
            "feature": "yesno",
            "answer": answer,
            "p": (p * 1000.0).round() / 1000.0,
            "ms": self.jev_ms,
            "totalMs": self.total_ms,
            "model": lucina_jev::shared().model(),
        }))
    }

    /// One `event="jev_yesno"` line for the drift report.
    pub fn log(&self, source: &'static str) {
        let why = match &self.outcome {
            Outcome::Pass { why } => *why,
            _ => "",
        };
        tracing::info!(target: "lucina_jev::route", event = "jev_yesno", source,
            outcome = self.outcome_name(), why,
            gate = ?self.gate.as_ref().map(|g| (g.p * 1000.0).round() / 1000.0),
            kind = ?self.kind.map(Kind::name),
            sensitive = ?self.gate.as_ref().and_then(|g| g.sensitive),
            p_yes = ?self.verdict.as_ref().map(|v| (v.p_yes * 1000.0).round() / 1000.0),
            grounded = ?self.verdict.as_ref().and_then(|v| v.grounded).map(|g| (g * 1000.0).round() / 1000.0),
            facts = self.facts, jev_ms = self.jev_ms, total_ms = self.total_ms, "jev_yesno");
    }
}

/// Screen, gate, state, answer, reason. Read-only: nothing here writes or starts anything. The chat
/// turn checks [`enabled`] first; the replay endpoint runs it either way.
pub async fn run(
    st: &AppState,
    question: &str,
    memory: &str,
    recent: &str,
    lang: &str,
    now: DateTime<Utc>,
) -> Run {
    let started = Instant::now();
    if !looks_like_question(question) {
        return Run::pass("not_question_shaped", started);
    }
    let jev = lucina_jev::shared();
    let Some(g) = gate(jev, question, recent).await else {
        return Run::pass("jev_unavailable", started);
    };
    let mut run = Run {
        outcome: Outcome::Pass { why: "not_yes_no" },
        gate: Some(g.clone()),
        verdict: None,
        kind: Some(g.kind),
        jev_ms: g.ms,
        total_ms: 0,
        facts: 0,
    };
    let finish = |mut run: Run, outcome: Outcome| {
        run.outcome = outcome;
        run.total_ms = started.elapsed().as_millis() as u64;
        run
    };
    if !g.is_yes_no() {
        return finish(run, Outcome::Pass { why: "not_yes_no" });
    }
    // About her or the conversation ("sei sicura?", "are you online?"): her own words answer best.
    if g.kind == Kind::Lucina {
        return finish(run, Outcome::Pass { why: "about_lucina" });
    }
    if let Some(topic) = g.sensitive {
        return finish(run, Outcome::Hedge { line: hedge_line(lang, topic) });
    }
    let facts = if g.kind == Kind::General {
        Facts {
            now: now_line(now, st.cfg.time_zone),
            ..Facts::default()
        }
    } else {
        gather(st, question, memory, recent, now).await
    };
    run.facts = facts.facts.len();
    let Some(v) = answer(jev, question, g.kind, &facts).await else {
        return finish(run, Outcome::Pass { why: "jev_unavailable" });
    };
    run.jev_ms += v.ms;
    run.verdict = Some(v.clone());
    let (yes, certainty) = match decide(g.kind, &v) {
        Decision::Answer { yes, certainty } => (yes, certainty),
        // Her facts do not cover it (his email, a live fact): the normal path looks it up, no "not sure".
        Decision::NotSettled if g.kind == Kind::Live => {
            return finish(run, Outcome::Pass { why: "live_lookup" })
        }
        Decision::NotSettled => return finish(run, Outcome::Pass { why: "not_in_facts" }),
        Decision::Unsure => {
            return finish(run, Outcome::Unsure { line: unsure_line(lang).into() })
        }
    };
    let evidence = v.evidence.filter(|(_, p)| *p >= 0.3).map(|(i, _)| i);
    let reason_from = if g.kind == Kind::General {
        Vec::new()
    } else {
        reason_facts(&facts, evidence)
    };
    let reason = match phrase_reason(st, lang, question, yes, &reason_from).await {
        Reason::Text(t) => t,
        Reason::Disagree => {
            return finish(run, Outcome::Unsure { line: unsure_line(lang).into() })
        }
        // The model failed: the evidence fact as it is (never a sentence of our own).
        Reason::None => evidence
            .and_then(|i| facts.facts.get(i))
            .map(|f| clip(&f.text, 160))
            .unwrap_or_default(),
    };
    // No reason to give: a bare "No (94%)" is not an answer he can check.
    if reason.trim().is_empty() {
        return finish(run, Outcome::Unsure { line: unsure_line(lang).into() });
    }
    finish(
        run,
        Outcome::Answer {
            reply: reply_text(lang, yes, certainty, &reason),
        },
    )
}

/// `POST /api/secretary/jev/yesno`: the same path on a text, no turn (calibration and the replay).
/// `gate_only`: the screen and Jev's gate, no state or answer (old questions about "today").
pub async fn preview(st: &AppState, text: &str, recent: &str, lang: &str, gate_only: bool) -> Value {
    if gate_only {
        let screened = looks_like_question(text);
        let g = match screened {
            true => gate(lucina_jev::shared(), text, recent).await,
            false => None,
        };
        return json!({
            "screen": screened,
            "gate": g.as_ref().map(|g| json!({"p": g.p, "yesNo": g.is_yes_no(), "kind": g.kind.name(),
                "kindP": g.kind_p, "sensitive": g.sensitive, "ms": g.ms})),
        });
    }
    let r = run(st, text, "", recent, lang, Utc::now()).await;
    r.log("preview");
    json!({
        "outcome": r.outcome_name(),
        "why": match &r.outcome { Outcome::Pass { why } => *why, _ => "" },
        "reply": match &r.outcome {
            Outcome::Answer { reply } => reply.as_str(),
            Outcome::Unsure { line } | Outcome::Hedge { line } => line.as_str(),
            Outcome::Pass { .. } => "",
        },
        "gate": r.gate.as_ref().map(|g| json!({"p": g.p, "kind": g.kind.name(), "kindP": g.kind_p,
            "sensitive": g.sensitive, "ms": g.ms})),
        "verdict": r.verdict.as_ref().map(|v| json!({"pYes": v.p_yes, "grounded": v.grounded,
            "evidence": v.evidence.map(|(i, p)| json!([i + 1, p])), "ms": v.ms})),
        "facts": r.facts,
        "jevMs": r.jev_ms,
        "totalMs": r.total_ms,
        "badge": r.badge(),
    })
}

#[cfg(test)]
mod tests;
