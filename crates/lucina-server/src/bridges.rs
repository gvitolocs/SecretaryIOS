//! The Secretary chat bridges to secretary-server, as the Node Lucina runs
//! them in front of the planner (`src/notes-bridge.js`,
//! `src/calendar-bridge.js`, `src/avatar-outfit.js`, `src/app-notify.js`):
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
                        return Some(Ok(out(
                            cal::created_text(&created, tz),
                            "calendar_create",
                            format!("[calendar event created: {title}]"),
                        )));
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
    Some(Ok(out(
        reply,
        "calendar",
        format!("[calendar {}..{}: {} events]", q.from, q.to, evs.len()),
    )))
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
            return r;
        }
        last = r;
        if attempt == 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    tracing::warn!(status = last.status, code = %last.code, "app notification failed");
    last
}
