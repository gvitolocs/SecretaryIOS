//! The Secretary chat bridges to secretary-server, as the Node Lucina runs
//! them in front of the planner (`src/notes-bridge.js`,
//! `src/calendar-bridge.js`, `src/tasks-bridge.js`, `src/avatar-outfit.js`,
//! `src/app-notify.js`):
//! deterministic parsing (crate `lucina-intent`), one HTTP call with the
//! service token, fixed it/en/da replies.
//!
//! secretary-server: `LUCINA_SECRETARY_SERVER_URL` + the token in
//! `LUCINA_SECRETARY_SERVICE_TOKEN_FILE` (read per call). Missing either is
//! `not_configured`, as in Node.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use lucina_intent::calendar::{self as cal, Create};
use lucina_intent::notes::{self as nt, NotesIntent};
use lucina_intent::outfit::{self, LookIntent};
use lucina_intent::tasks as tk;
use reqwest::Method;
use serde_json::{json, Value};

pub const PART_TTL: Duration = Duration::from_secs(30 * 60);
pub const CALENDAR_PENDING_TTL: Duration = Duration::from_secs(10 * 60);
const LIST_CACHE: Duration = Duration::from_secs(60);
const CALL_TIMEOUT: Duration = Duration::from_secs(8);

/// One secretary-server answer (`{ok, status, code, data}` as in Node).
#[derive(Debug, Clone)]
pub struct Answer {
    pub ok: bool,
    pub status: u16,
    pub code: String,
    pub data: Value,
}

impl Answer {
    fn err(code: &str) -> Self {
        Self {
            ok: false,
            status: 0,
            code: code.into(),
            data: Value::Null,
        }
    }
}

/// secretary-server with Lucina's service token.
#[derive(Clone)]
pub struct Secretary {
    pub http: reqwest::Client,
    pub url: Option<String>,
    pub token_file: Option<PathBuf>,
}

impl Secretary {
    fn token(&self) -> Option<String> {
        let p = self.token_file.as_ref()?;
        std::fs::read_to_string(p)
            .ok()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
    }

    pub async fn call(&self, method: Method, path: &str, body: Option<&Value>) -> Answer {
        let (Some(url), Some(token)) =
            (self.url.as_deref().filter(|u| !u.is_empty()), self.token())
        else {
            return Answer::err("not_configured");
        };
        let mut req = self
            .http
            .request(method, format!("{}{path}", url.trim_end_matches('/')))
            .bearer_auth(token)
            .header("accept", "application/json")
            .timeout(CALL_TIMEOUT);
        if let Some(b) = body {
            req = req.json(b);
        }
        match req.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let data: Value = resp.json().await.unwrap_or(Value::Null);
                if (200..300).contains(&status) {
                    Answer {
                        ok: true,
                        status,
                        code: String::new(),
                        data,
                    }
                } else {
                    let code = data["error"]["code"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("http_{status}"));
                    Answer {
                        ok: false,
                        status,
                        code,
                        data,
                    }
                }
            }
            Err(e) if e.is_timeout() => Answer::err("timeout"),
            Err(_) => Answer::err("unreachable"),
        }
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn seg(key: &str) -> String {
    enc(&key.trim().to_lowercase())
}

/// What a bridge turn produced.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub reply: String,
    pub action: String,
    pub action_result: String,
    pub meta: Option<lucina_store::timeline::Meta>,
    /// Notes: a note to answer from (the caller generates the reply).
    pub read_note: Option<Value>,
    /// Notes: a saved note for Honcho.
    pub memory_note: Option<Value>,
    /// Calendar: the events the answer was made from (place blocks).
    pub events: Vec<Value>,
}

fn meta_note(slug: &str) -> lucina_store::timeline::Meta {
    lucina_store::timeline::Meta {
        slug: Some(slug.to_owned()),
        ..lucina_store::timeline::Meta::kind("note")
    }
}

/// In-memory bridge state: multi-part saves, calendar confirmations, the
/// notes list cache.
#[derive(Default)]
pub struct BridgeState {
    parts: Mutex<HashMap<String, (String, Instant)>>,
    calendar: Mutex<HashMap<String, (Create, Instant)>>,
    notes_cache: Mutex<Option<(Instant, Vec<Value>)>>,
}

impl BridgeState {
    /// The note a "parte 2" in this session continues, or "".
    pub fn pending_part(&self, session: &str) -> String {
        let map = self.parts.lock().unwrap_or_else(|e| e.into_inner());
        map.get(session)
            .filter(|(_, at)| at.elapsed() <= PART_TTL)
            .map(|(s, _)| s.clone())
            .unwrap_or_default()
    }

    fn remember_part(&self, session: &str, slug: &str) {
        let mut map = self.parts.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, (_, at)| at.elapsed() <= PART_TTL);
        map.insert(session.to_owned(), (slug.to_owned(), Instant::now()));
    }

    fn clear_part(&self, session: &str) {
        self.parts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(session);
    }
}

// ---------------------------------------------------------------- notes

/// `handleNotesTurn`. Never fails: errors become replies.
/// `findNote`: a note by slug, else the best search hit for a title or words.
pub async fn find_note(s: &Secretary, name: &str) -> Answer {
    let key = Some(nt::slugify(name))
        .filter(|k| !k.is_empty())
        .unwrap_or_else(|| name.to_owned());
    let r = s
        .call(Method::GET, &format!("/v1/notes/{}", seg(&key)), None)
        .await;
    if r.ok || r.status != 404 {
        return r;
    }
    let found = s
        .call(
            Method::GET,
            &format!("/v1/notes?limit=1&q={}", enc(name)),
            None,
        )
        .await;
    match found
        .ok
        .then(|| found.data["notes"][0]["slug"].as_str().map(str::to_owned))
        .flatten()
    {
        Some(slug) => {
            s.call(Method::GET, &format!("/v1/notes/{}", seg(&slug)), None)
                .await
        }
        None => r,
    }
}

pub async fn notes_turn(
    s: &Secretary,
    st: &BridgeState,
    intent: &NotesIntent,
    session: &str,
    source: &str,
) -> Outcome {
    let lang = intent.lang();
    let out = |reply: String, action_result: String| Outcome {
        reply,
        action: "notes".into(),
        action_result,
        ..Outcome::default()
    };
    match intent {
        NotesIntent::List { .. } => {
            let r = s.call(Method::GET, "/v1/notes?limit=30", None).await;
            if !r.ok {
                return out(
                    nt::reply_read_failed(lang, &r.code),
                    format!("[notes list failed: {}]", r.code),
                );
            }
            let notes = r.data["notes"].as_array().cloned().unwrap_or_default();
            if notes.is_empty() {
                return out(nt::reply_none(lang).into(), "[notes: none]".into());
            }
            let lines: Vec<String> = notes
                .iter()
                .map(|n| {
                    format!(
                        "• {}{} ({})",
                        if n["pinned"].as_bool().unwrap_or(false) {
                            "📌 "
                        } else {
                            ""
                        },
                        n["title"].as_str().unwrap_or(""),
                        n["slug"].as_str().unwrap_or("")
                    )
                })
                .collect();
            Outcome {
                meta: Some(lucina_store::timeline::Meta::kind("notes")),
                ..out(
                    nt::reply_list(lang, notes.len(), &lines.join("\n")),
                    format!("[notes listed: {}]", notes.len()),
                )
            }
        }
        NotesIntent::Read { name, .. } => {
            let r = find_note(s, name).await;
            if !r.ok {
                if r.status == 404 {
                    let all = s.call(Method::GET, "/v1/notes?limit=10", None).await;
                    let names = if all.ok {
                        all.data["notes"]
                            .as_array()
                            .map(|l| {
                                l.iter()
                                    .filter_map(|n| n["slug"].as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            })
                            .unwrap_or_default()
                    } else {
                        String::new()
                    };
                    return out(
                        nt::reply_not_found(lang, name, &names),
                        "[note not found]".into(),
                    );
                }
                return out(
                    nt::reply_read_failed(lang, &r.code),
                    format!("[note read failed: {}]", r.code),
                );
            }
            let slug = r.data["slug"].as_str().unwrap_or("").to_owned();
            Outcome {
                action: "notes".into(),
                action_result: format!("[note read: {slug}]"),
                meta: Some(meta_note(&slug)),
                read_note: Some(r.data),
                ..Outcome::default()
            }
        }
        NotesIntent::Save {
            name,
            slug,
            title,
            content,
            more,
            ..
        } => {
            let content = content.trim();
            if content.is_empty() {
                return out(
                    nt::reply_ask_content(lang, name),
                    "[note: no content]".into(),
                );
            }
            if content.len() > nt::MAX_NOTE_BYTES {
                return out(nt::reply_too_long(lang).into(), "[note too long]".into());
            }
            let content = content.replace("\r\n", "\n").replace('\r', "\n");
            let mut body = json!({"title": if title.is_empty() { "Nota" } else { title.as_str() }, "content": content, "source": source});
            if !slug.is_empty() {
                body["slug"] = json!(slug);
            }
            let mut r = s.call(Method::POST, "/v1/notes", Some(&body)).await;
            let mut existed = false;
            if !r.ok && r.code == "slug_taken" {
                existed = true;
                r = s
                    .call(
                        Method::POST,
                        &format!("/v1/notes/{}/append", seg(slug)),
                        Some(&json!({"content": content, "separator": "\n\n", "source": source})),
                    )
                    .await;
            }
            if !r.ok {
                let reply = if r.code == "note_too_large" {
                    nt::reply_too_long(lang).to_owned()
                } else {
                    nt::reply_failed(lang, &r.code)
                };
                return out(reply, format!("[note save failed: {}]", r.code));
            }
            let saved_slug = r.data["slug"].as_str().unwrap_or("").to_owned();
            if *more {
                st.remember_part(session, &saved_slug);
            }
            tracing::info!(slug = %saved_slug, chars = content.chars().count(), %source, existed, "lucina note saved");
            let v = nt::view(&r.data);
            Outcome {
                reply: if existed {
                    nt::reply_existed(lang, &v)
                } else {
                    nt::reply_saved(lang, &v, *more)
                },
                action: "notes".into(),
                action_result: format!(
                    "[note {}: {saved_slug}]",
                    if existed { "appended" } else { "saved" }
                ),
                meta: Some(meta_note(&saved_slug)),
                memory_note: Some(r.data),
                read_note: None,
                events: Vec::new(),
            }
        }
        NotesIntent::Part { content, more, .. } => {
            let content = content.trim();
            if content.is_empty() {
                return out(nt::reply_ask_content(lang, ""), "[note: no content]".into());
            }
            if content.len() > nt::MAX_NOTE_BYTES {
                return out(nt::reply_too_long(lang).into(), "[note too long]".into());
            }
            let content = content.replace("\r\n", "\n").replace('\r', "\n");
            let slug = st.pending_part(session);
            let r = s
                .call(
                    Method::POST,
                    &format!("/v1/notes/{}/append", seg(&slug)),
                    Some(&json!({"content": content, "separator": "\n\n", "source": source})),
                )
                .await;
            if !r.ok {
                let reply = if r.code == "note_too_large" {
                    nt::reply_too_long(lang).to_owned()
                } else {
                    nt::reply_failed(lang, &r.code)
                };
                return out(reply, format!("[note append failed: {}]", r.code));
            }
            if *more {
                st.remember_part(session, &slug);
            } else {
                st.clear_part(session);
            }
            Outcome {
                reply: nt::reply_appended(lang, &nt::view(&r.data), *more),
                action: "notes".into(),
                action_result: format!("[note appended: {slug}]"),
                meta: Some(meta_note(&slug)),
                memory_note: Some(r.data),
                read_note: None,
                events: Vec::new(),
            }
        }
    }
}

/// `buildNotesContext`: the notes the prompt names, or "".
pub async fn notes_context(s: &Secretary, st: &BridgeState, prompt: &str) -> String {
    let cached = {
        let c = st.notes_cache.lock().unwrap_or_else(|e| e.into_inner());
        c.as_ref()
            .filter(|(at, _)| at.elapsed() < LIST_CACHE)
            .map(|(_, l)| l.clone())
    };
    let list = match cached {
        Some(l) => l,
        None => {
            let r = s.call(Method::GET, "/v1/notes?limit=200", None).await;
            if !r.ok {
                return String::new();
            }
            let l = r.data["notes"].as_array().cloned().unwrap_or_default();
            *st.notes_cache.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((Instant::now(), l.clone()));
            l
        }
    };
    let hits = nt::mentioned_notes(prompt, &list);
    let mut blocks = Vec::new();
    for h in hits {
        let slug = h["slug"].as_str().unwrap_or("");
        let r = s
            .call(Method::GET, &format!("/v1/notes/{}", seg(slug)), None)
            .await;
        if r.ok {
            blocks.push(nt::render_note_context(&r.data, nt::MAX_CONTEXT_CHARS));
        }
    }
    if blocks.is_empty() {
        String::new()
    } else {
        format!(
            "Owner notes named in this message (Secretary app, Notes):\n{}",
            blocks.join("\n\n")
        )
    }
}

// ---------------------------------------------------------------- calendar

async fn events(s: &Secretary, from: &str, to: &str) -> Answer {
    s.call(
        Method::GET,
        &format!("/v1/calendar/events?from={}&to={}", enc(from), enc(to)),
        None,
    )
    .await
}

/// A calendar part of a chat turn: `Err(blocked)` for an MCP create.
pub async fn calendar_turn(
    s: &Secretary,
    st: &BridgeState,
    message: &str,
    session: &str,
    mcp: bool,
    now: DateTime<Utc>,
    tz: Tz,
) -> Option<Result<Outcome, &'static str>> {
    let key = if session.is_empty() {
        "default"
    } else {
        session
    }
    .to_owned();
    let out = |reply: String, action: &str, action_result: String| Outcome {
        reply,
        action: action.into(),
        action_result,
        meta: Some(lucina_store::timeline::Meta::kind("calendar")),
        ..Outcome::default()
    };
    if !mcp {
        let waiting = {
            let mut map = st.calendar.lock().unwrap_or_else(|e| e.into_inner());
            map.retain(|_, (_, at)| at.elapsed() < CALENDAR_PENDING_TTL);
            map.remove(&key)
        };
        if let Some((created, _)) = waiting {
            match cal::classify_yes_no(message) {
                "yes" => {
                    let r = s
                        .call(Method::POST, "/v1/calendar/events", Some(&created.event))
                        .await;
                    let title = created.event["title"].as_str().unwrap_or("");
                    if r.ok {
                        return Some(Ok(Outcome {
                            events: r
                                .data
                                .get("event")
                                .filter(|e| e.is_object())
                                .cloned()
                                .into_iter()
                                .collect(),
                            ..out(
                                cal::created_text(&created, tz),
                                "calendar_create",
                                format!("[calendar event created: {title}]"),
                            )
                        }));
                    }
                    let reply = if r.code == "google_reconnect_needed"
                        || r.code == "google_not_connected"
                    {
                        cal::text(created.lang, "reconnect").to_owned()
                    } else {
                        cal::create_failed_text(created.lang, &r.code)
                    };
                    return Some(Ok(out(
                        reply,
                        "calendar_create",
                        format!("[calendar create failed: {}]", r.code),
                    )));
                }
                "no" => {
                    return Some(Ok(out(
                        cal::text(created.lang, "declined").into(),
                        "calendar_create",
                        "[calendar create declined]".into(),
                    )));
                }
                _ => {}
            }
        }
    }
    if let Some(parsed) = cal::parse_create(message, now, tz) {
        if mcp {
            return Some(Err("calendar.create"));
        }
        return Some(Ok(match parsed {
            Err((error, lang)) => out(
                cal::text(lang, "unclear").into(),
                "calendar_confirm",
                format!("[calendar create unclear: {error}]"),
            ),
            Ok(created) => {
                let q = cal::confirm_question(&created, tz);
                st.calendar
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(key, (created, Instant::now()));
                out(
                    q,
                    "calendar_confirm",
                    "[calendar event awaiting confirmation]".into(),
                )
            }
        }));
    }
    let q = cal::parse_question(message, now, tz)?;
    let r = events(s, &q.from, &q.to).await;
    if !r.ok {
        return Some(Ok(out(
            cal::text(q.lang, "unavailable").into(),
            "calendar",
            format!("[calendar unavailable: {}]", r.code),
        )));
    }
    let evs = r.data["events"].as_array().cloned().unwrap_or_default();
    let mut reply = cal::format_answer(&q, &evs, now, tz);
    if r.data["sources"]["google"]["status"] == "reconnect" {
        reply.push_str(cal::text(q.lang, "reconnect_note"));
    }
    let action_result = format!("[calendar {}..{}: {} events]", q.from, q.to, evs.len());
    Some(Ok(Outcome {
        events: evs,
        ..out(reply, "calendar", action_result)
    }))
}

/// `buildCalendarContext`: the events a calendar question asks about.
pub async fn calendar_context(s: &Secretary, prompt: &str, now: DateTime<Utc>, tz: Tz) -> String {
    let Some(q) = cal::parse_question(prompt, now, tz) else {
        return String::new();
    };
    let r = events(s, &q.from, &q.to).await;
    if !r.ok {
        return format!(
            "Calendar ({}..{}): unavailable right now; do not guess appointments.",
            q.from, q.to
        );
    }
    let evs = r.data["events"].as_array().cloned().unwrap_or_default();
    let lines = cal::event_lines(&evs, tz, "en");
    let range = if q.from == q.to {
        q.from.clone()
    } else {
        format!("{}..{}", q.from, q.to)
    };
    format!(
        "Giuseppe's calendar {range} ({}, Google + iPhone + subscriptions): {}. Answer from this list only.",
        tz.name(),
        if lines.is_empty() { "nothing scheduled".to_owned() } else { lines.join("; ") }
    )
}

/// `eventsForBriefing`: the day's events (empty when unavailable).
pub async fn events_for_briefing(s: &Secretary, date: &str) -> Vec<Value> {
    let r = events(s, date, date).await;
    if r.ok {
        r.data["events"].as_array().cloned().unwrap_or_default()
    } else {
        Vec::new()
    }
}

// ---------------------------------------------------------------- outfit

/// `changeOutfit`: save the look, answer in the request's language.
pub async fn change_look(s: &Secretary, i: &LookIntent) -> Outcome {
    let r = s
        .call(Method::PUT, "/v1/avatar", Some(&outfit::patch_for(i)))
        .await;
    let result: Result<(), &str> = if r.ok { Ok(()) } else { Err(r.code.as_str()) };
    if let Err(code) = result {
        if !code.starts_with("unknown_") {
            tracing::warn!(status = r.status, %code, "avatar look change failed");
        }
    }
    let (reply, saved) = outfit::reply_for(i, result);
    Outcome {
        reply,
        action: "avatar_outfit".into(),
        action_result: outfit::action_result(i, result),
        meta: saved.then(|| lucina_store::timeline::Meta {
            outfit: if i.hair.is_some() {
                None
            } else {
                i.outfit.clone()
            },
            hair: i.hair.clone(),
            ..lucina_store::timeline::Meta::kind("avatar_outfit")
        }),
        ..Outcome::default()
    }
}

// ---------------------------------------------------------------- notify

/// `notifyApp` (`POST /v1/notify`), best effort with one retry on a network
/// error or 5xx. Used by the reminder scheduler when it moves to Rust.
pub async fn notify(s: &Secretary, body: &Value) -> Answer {
    let mut last = Answer::err("unreachable");
    for attempt in 0..2 {
        let r = s.call(Method::POST, "/v1/notify", Some(body)).await;
        if r.ok || (r.status > 0 && r.status < 500) {
            // A 4xx is an answer (bad body, quiet hours…): the path works.
            crate::delivery_monitor::record_notify(true);
            return r;
        }
        last = r;
        if attempt == 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    tracing::warn!(status = last.status, code = %last.code, "app notification failed");
    crate::delivery_monitor::record_notify(false);
    last
}

// ---------------------------------------------------------------- tasks

/// `/v1/tasks/state?view=…` (`fetchTasks`).
async fn fetch_tasks(s: &Secretary, view: &str, query: Option<&str>, tz: Tz) -> Answer {
    let mut path = format!(
        "/v1/tasks/state?view={}&timezone={}",
        enc(view),
        enc(tz.name())
    );
    if let Some(q) = query {
        path.push_str(&format!("&query={}", enc(q)));
    }
    s.call(Method::GET, &path, None).await
}

/// `POST /v1/tasks/command` (`taskCommand`), with a fresh op id.
async fn task_command(s: &Secretary, command: Value, tz: Tz) -> Answer {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let mut ms = Utc::now().timestamp_millis().max(0) as u64;
    let mut b36 = Vec::new();
    loop {
        b36.push(char::from_digit((ms % 36) as u32, 36).unwrap_or('0'));
        ms /= 36;
        if ms == 0 {
            break;
        }
    }
    let ts: String = b36.into_iter().rev().collect();
    let op_id = format!("lucina-rs-{ts}-{}-{n}", std::process::id());
    let body = json!({"op_id": op_id, "timezone": tz.name(), "command": command});
    s.call(Method::POST, "/v1/tasks/command", Some(&body)).await
}

fn tasks_outcome(reply: String, action: &str, action_result: String) -> Outcome {
    Outcome {
        reply,
        action: action.into(),
        action_result,
        meta: Some(lucina_store::timeline::Meta::kind("tasks")),
        ..Outcome::default()
    }
}

fn game_base() -> Option<String> {
    if std::env::var("LUCINA_GAME")
        .map(|v| v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
    {
        return None;
    }
    let base = std::env::var("LUCINA_GAME_URL").unwrap_or_else(|_| "http://127.0.0.1:18767".into());
    Some(base.trim_end_matches('/').to_owned())
}

/// What the chat brain is told about her own playing self: identity, the actions, and the live game state.
pub const GAME_IDENTITY: &str = "This Pokémon Red run is YOU: your own playing self (the same Lucina) playing on nezopt through the game tools while Giuseppe watches. Never deny being the player and never say you cannot see the game: the state below is live. From chat you do not press buttons yourself; your playing self acts in its own turn loop, and you can pass it advice. When Giuseppe gives in-game advice or an instruction for the run (even without saying Pokémon, e.g. \"that was a tree to cut, go back there\"), choose {\"action\":\"game_hint\",\"text\":\"<his advice as one clear instruction to your playing self, in English>\",\"ack\":\"<short reply in his language saying you passed it to your playing self>\"}. To start or resume the run choose {\"action\":\"game_start\"}; to pause it {\"action\":\"game_pause\"} (only when he asks). Questions about the game (where are you, how is it going, why did you do that) are a normal reply from this state, in your own words. If he asks you to look up how to progress (next badge, an item, a blocked path), use your own Pokémon Red knowledge and pass the concrete next step to your playing self as game_hint.";

/// Live summary of Lucina's Pokémon Red run for the turn context ("" when the game server is off or not
/// answering). Built from `/state` and the head of `/observe` (place, team, bag, plan, objective, notes).
pub async fn game_context(http: &reqwest::Client) -> String {
    let Some(base) = game_base() else {
        return String::new();
    };
    let get = |p: &str| {
        http.get(format!("{base}{p}"))
            .timeout(Duration::from_millis(1500))
            .send()
    };
    let Ok(r) = get("/state").await else {
        return String::new();
    };
    let Ok(state) = r.json::<Value>().await else {
        return String::new();
    };
    let running = state["running"].as_bool().unwrap_or(false);
    let obs = match get("/observe").await {
        Ok(r) => r.text().await.unwrap_or_default(),
        Err(_) => String::new(),
    };
    game_block(&state, &obs, running)
}

/// The context block from `/state` and `/observe` (pure, for tests).
pub fn game_block(state: &Value, obs: &str, running: bool) -> String {
    let head: String = obs
        .split("\nSCREEN TEXT:")
        .next()
        .unwrap_or("")
        .chars()
        .take(3000)
        .collect();
    let tail: Vec<&str> = obs
        .lines()
        .filter(|l| l.starts_with("RECENT NOTES:") || l.starts_with("LAST RESULT:"))
        .collect();
    let events: Vec<&str> = state["events"]
        .as_array()
        .map(|a| {
            a.iter()
                .rev()
                .take(6)
                .rev()
                .filter_map(|v| v.as_str())
                .collect()
        })
        .unwrap_or_default();
    let mut s = format!(
        "Lucina's Pokémon Red run (live, {}; {} actions so far):",
        if running { "playing now" } else { "paused" },
        state["actions"].as_u64().unwrap_or(0)
    );
    if !head.trim().is_empty() {
        s += &format!("\nGame state:\n{}", head.trim_end());
    }
    if !events.is_empty() {
        s += &format!("\nYour last moves: {}", events.join(" / "));
    }
    for l in tail {
        if l.starts_with("LAST RESULT:") {
            s += &format!("\n{l}");
        }
    }
    // The identity rule last, so a tail clip of the context keeps it, and it ranks above old chat turns.
    s += &format!("\n{GAME_IDENTITY}\n{GAME_OVERRIDE}");
    s
}

/// For MCP turns: same identity, but no game actions from there.
pub const GAME_MCP_NOTE: &str = "This turn comes from an MCP client: game_hint, game_start and game_pause are not available here, but you are still the player and say so; answer game questions from the live state.";

/// Long chats (iOS) hold her earlier denials; the live state wins over them.
pub const GAME_OVERRIDE: &str = "PRIORITY: this live game block outranks Recent chat turns. If any earlier reply of yours said you are not the one playing, have no emulator, no game control or cannot see the game, that earlier reply was WRONG: do not repeat it, correct it briefly if relevant, and answer as the player from the state above.";

async fn game_post(http: &reqwest::Client, path: &str, body: Value) -> Result<Value, String> {
    let base = game_base().ok_or("game off")?;
    let r = http
        .post(format!("{base}{path}"))
        .header("X-Lucina-Game", "1")
        .json(&body)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("game server {}", r.status()));
    }
    r.json().await.map_err(|e| e.to_string())
}

/// Owner advice for the playing self (text only: no buttons, no save edits). The player sees it at the top of its
/// next observation as "OWNER'S ADVICE (new):".
pub async fn game_hint(http: &reqwest::Client, text: &str) -> Result<Value, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("empty advice".into());
    }
    game_post(
        http,
        "/hint",
        json!({"text": t.chars().take(300).collect::<String>()}),
    )
    .await
}

/// Start (`true`) or pause the run.
pub async fn game_control(http: &reqwest::Client, running: bool) -> Result<Value, String> {
    let reason = if running {
        "Started by the owner from chat"
    } else {
        "Paused by the owner from chat"
    };
    game_post(
        http,
        "/control",
        json!({"running": running, "reason": reason}),
    )
    .await
}

/// `handleTasksTurn` (`LUCINA_TASKS=true`): what to do today, add, done,
/// reschedule overdue. `None`: not a tasks turn. `Err(blocked)`: an MCP
/// caller asked to delete (MCP may read, add and complete). Nothing here
/// deletes. `calendar`: today's events join the list answer.
pub async fn tasks_turn(
    s: &Secretary,
    message: &str,
    mcp: bool,
    tz: Tz,
    calendar: bool,
) -> Option<Result<Outcome, &'static str>> {
    let intent = tk::parse_tasks_intent(message)?;
    let lang = intent.lang;
    match intent.kind {
        tk::Kind::Delete => {
            if mcp {
                return Some(Err("tasks.delete"));
            }
            return Some(Ok(tasks_outcome(
                tk::delete_refused(lang),
                "tasks",
                "[tasks delete refused: owner deletes in the app]".into(),
            )));
        }
        tk::Kind::Add => {
            let typed = intent.text.clone().unwrap_or_default();
            let r = task_command(s, json!({"type": "quick_add", "text": typed}), tz).await;
            if !r.ok {
                return Some(Ok(tasks_outcome(
                    tk::unavailable(lang),
                    "tasks",
                    format!("[tasks add failed: {}]", r.code),
                )));
            }
            let parsed = &r.data["parsed"];
            let content = parsed["content"]
                .as_str()
                .filter(|c| !c.is_empty())
                .unwrap_or(&typed)
                .to_owned();
            return Some(Ok(tasks_outcome(
                tk::added(lang, parsed, &typed),
                "tasks_add",
                format!("[task added: {content}]"),
            )));
        }
        tk::Kind::Overdue => {
            let date = tk::overdue_date(intent.date.as_deref());
            let r = task_command(s, json!({"type": "reschedule_overdue", "date": date}), tz).await;
            if !r.ok {
                return Some(Ok(tasks_outcome(
                    tk::unavailable(lang),
                    "tasks",
                    format!("[tasks reschedule failed: {}]", r.code),
                )));
            }
            return Some(Ok(tasks_outcome(
                tk::overdue_moved(lang),
                "tasks",
                format!("[overdue tasks rescheduled to {date}]"),
            )));
        }
        tk::Kind::List | tk::Kind::Done => {}
    }
    let r = fetch_tasks(s, "today", None, tz).await;
    if !r.ok {
        // "ho fatto la spesa": not ours to answer.
        if intent.kind == tk::Kind::Done {
            return None;
        }
        return Some(Ok(tasks_outcome(
            tk::unavailable(lang),
            "tasks",
            format!("[tasks unavailable: {}]", r.code),
        )));
    }
    let view = &r.data["view"];
    if intent.kind == tk::Kind::List {
        let events = if calendar {
            let today = Utc::now().with_timezone(&tz).format("%Y-%m-%d").to_string();
            Some(cal::event_lines(
                &events_for_briefing(s, &today).await,
                tz,
                "en",
            ))
        } else {
            None
        };
        return Some(Ok(tasks_outcome(
            tk::today_answer(view, lang, events.as_deref()),
            "tasks",
            format!("[tasks today: {}]", tk::task_lines(view, lang).len()),
        )));
    }
    // done
    let what = intent.what.clone().unwrap_or_default();
    let mut hits: Vec<Value> = tk::match_tasks(view, &what).into_iter().cloned().collect();
    if hits.is_empty() {
        let found = fetch_tasks(s, "search", Some(&what), tz).await;
        if found.ok {
            hits = tk::match_tasks(&found.data["view"], &what)
                .into_iter()
                .cloned()
                .collect();
        }
    }
    match hits.len() {
        // Not a task: the normal turn handles it.
        0 => None,
        1 => {
            let task = &hits[0];
            let c = task_command(s, json!({"type": "complete", "task_id": task["id"]}), tz).await;
            if !c.ok {
                return Some(Ok(tasks_outcome(
                    tk::unavailable(lang),
                    "tasks",
                    format!("[task complete failed: {}]", c.code),
                )));
            }
            Some(Ok(tasks_outcome(
                tk::completed(lang, task, &c.data["view"]["goal"]),
                "tasks_complete",
                format!("[task completed: {}]", tk::js(&task["content"])),
            )))
        }
        n => {
            let refs: Vec<&Value> = hits.iter().collect();
            Some(Ok(tasks_outcome(
                tk::which_one(lang, &refs),
                "tasks",
                format!("[task complete ambiguous: {n}]"),
            )))
        }
    }
}

/// `buildTasksContext`: today's tasks as one context line for owner turns
/// about the day ("" when unavailable).
pub async fn tasks_context(s: &Secretary, prompt: &str, tz: Tz) -> String {
    if !tk::is_task_topic(prompt) {
        return String::new();
    }
    let path = format!("/v1/tasks/signals?timezone={}", enc(tz.name()));
    let r = tokio::time::timeout(Duration::from_secs(3), s.call(Method::GET, &path, None)).await;
    match r {
        Ok(a) if a.ok && a.data.is_object() => tk::context_line(&a.data),
        _ => String::new(),
    }
}

/// `tasksForBriefing`: today's open and overdue task lines.
pub async fn tasks_for_briefing(s: &Secretary, tz: Tz) -> Vec<String> {
    let r = fetch_tasks(s, "today", None, tz).await;
    if r.ok {
        tk::task_lines(&r.data["view"], "en")
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod game_tests {
    use super::*;

    #[test]
    fn game_block_is_live_state_with_identity() {
        let state = json!({"running": true, "actions": 2908, "events": ["Healing the team", "Heal tool found no route"]});
        let obs = "OWNER'S ADVICE (new): Giuseppe is watching you play and told you in chat:\n  1. (0 min ago) Use Cut\nMODE: Overworld\nLOCATION: VERMILION_CITY (map 5) x=12 y=22, facing down\nSCREEN TEXT:\n  (no text on screen)\nRECENT NOTES: a / b\nLAST RESULT: No known walking route";
        let b = game_block(&state, obs, true);
        assert!(b.contains("playing now"));
        assert!(b.contains("This Pokémon Red run is YOU"));
        assert!(b.contains("VERMILION_CITY"));
        assert!(b.contains("OWNER'S ADVICE"));
        assert!(b.contains("Heal tool found no route"));
        assert!(b.contains("LAST RESULT: No known walking route"));
        assert!(!b.contains("no text on screen"));
        assert!(game_block(&json!({"running": false}), "", false).contains("paused"));
    }
}
