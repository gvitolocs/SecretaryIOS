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
use lucina_intent::calendar_event as ev;
use lucina_intent::name_match::PersonAliases;
use lucina_intent::notes::{self as nt, NotesIntent};
use lucina_intent::outfit::{self, LookIntent};
use lucina_intent::tasks as tk;
use reqwest::Method;
use serde_json::{json, Value};

pub const PART_TTL: Duration = Duration::from_secs(30 * 60);
/// How long the Undo of a calendar event created from a chat stays available.
pub const CALENDAR_UNDO_TTL: Duration = Duration::from_secs(cal::UNDO_MINUTES as u64 * 60);
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

pub(crate) fn enc(s: &str) -> String {
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
    /// Calendar: the answer is an agenda (one sentence, then a card per event).
    pub agenda: Option<crate::agenda::Agenda>,
    /// The reply's own buttons (the Undo of a calendar event just created).
    pub routes: Vec<Value>,
}

fn meta_note(slug: &str) -> lucina_store::timeline::Meta {
    lucina_store::timeline::Meta {
        slug: Some(slug.to_owned()),
        ..lucina_store::timeline::Meta::kind("note")
    }
}

/// A calendar event created from a chat, kept for its Undo.
#[derive(Debug, Clone)]
struct Created {
    id: String,
    title: String,
    lang: &'static str,
}

/// In-memory bridge state: multi-part saves, the calendar events created in
/// the last 10 minutes (per session, for the Undo), the notes list cache.
#[derive(Default)]
pub struct BridgeState {
    parts: Mutex<HashMap<String, (String, Instant)>>,
    calendar: Mutex<HashMap<String, (Created, Instant)>>,
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
            let r = s
                .call(Method::GET, "/v1/notes?limit=30&archived=exclude", None)
                .await;
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
                agenda: None,
                routes: Vec::new(),
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
                agenda: None,
                routes: Vec::new(),
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
            let block = nt::render_note_context(&r.data, nt::MAX_CONTEXT_CHARS);
            let block = lucina_secrets::labelled::mask(&block).into_owned();
            // A slug made from a first line that holds a token.
            blocks.push(if lucina_secrets::labelled::slug_holds_token(slug) {
                block.replace(slug, r.data["id"].as_str().unwrap_or("note"))
            } else {
                block
            });
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

pub(crate) async fn events(s: &Secretary, from: &str, to: &str) -> Answer {
    s.call(
        Method::GET,
        &format!("/v1/calendar/events?from={}&to={}", enc(from), enc(to)),
        None,
    )
    .await
}

/// Who asked for a calendar event in a chat turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalendarCaller {
    /// The owner's own message (his app, his Telegram/Discord DMs, voice), on `channel`:
    /// the event is created at once, with an Undo.
    Owner { channel: String },
    /// One of the owner's assistant apps through trusted Lucina MCP (`mcp:<caller>`): a
    /// client, not the owner himself, so the event is a proposal he confirms in Links.
    Client { from: String },
    /// A guarded turn (an agent's MCP turn, someone else in a channel): refused, the
    /// agent files a proposal with the `calendar_create` MCP tool instead.
    Guarded,
}

/// The Undo button of a direct create (the clients send its text back as the owner's message).
pub fn undo_action(lang: &str) -> Value {
    let (label, text) = cal::undo_action(lang);
    json!({"id": "calendar-undo", "label": label, "emoji": "↩️", "kind": "reply", "payload": {"text": text}})
}

/// Files `created` as a proposal in Links (`reason`: why it was not created directly).
async fn propose(s: &Secretary, created: &Create, from: &str, reason: Option<&str>) -> Answer {
    let mut body = created.event.clone();
    body["from"] = json!(from.chars().take(64).collect::<String>());
    if let Some(r) = reason {
        body["reason"] = json!(r);
    }
    s.call(Method::POST, "/v1/calendar/proposals", Some(&body))
        .await
}

/// A calendar part of a chat turn: `Err(blocked)` for a guarded create.
pub async fn calendar_turn(
    s: &Secretary,
    st: &BridgeState,
    message: &str,
    session: &str,
    caller: &CalendarCaller,
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
    // "annulla" / "undo" right after a direct create: the event comes off the calendar.
    if matches!(caller, CalendarCaller::Owner { .. }) && cal::is_undo(message) {
        let undo = {
            let mut map = st.calendar.lock().unwrap_or_else(|e| e.into_inner());
            map.retain(|_, (_, at)| at.elapsed() < CALENDAR_UNDO_TTL);
            map.remove(&key)
        };
        if let Some((c, _)) = undo {
            let r = s
                .call(
                    Method::DELETE,
                    &format!("/v1/calendar/events/{}?confirm=true", enc(&c.id)),
                    None,
                )
                .await;
            return Some(Ok(if r.ok {
                out(
                    cal::undone_text(c.lang, &c.title),
                    "calendar_undo",
                    format!("[calendar event removed: {}]", c.title),
                )
            } else {
                out(
                    cal::undo_failed_text(c.lang, &r.code),
                    "calendar_undo",
                    format!("[calendar undo failed: {}]", r.code),
                )
            }));
        }
    }
    // "Aggiungi al calendario …", or an event said plainly ("Mentor session 6 november at
    // 12:30"); one the event parser cannot complete goes on to the planner.
    let parsed = cal::parse_create(message, now, tz)
        .or_else(|| cal::parse_event(message, now, tz).filter(Result::is_ok));
    // A create whose name is the owner's whole sentence (a correction of one hour for another)
    // names no event: the rule cannot tell the event from the words around it, so the planner,
    // which sees the recent turns, is asked for the name. A guarded caller is refused here, as
    // below, and never costs a planner call.
    let parsed = match parsed {
        Some(Ok(c))
            if *caller != CalendarCaller::Guarded
                && ev::title_is_sentence(c.event["title"].as_str().unwrap_or(""), message) =>
        {
            tracing::info!(
                target: "lucina_chat::calendar",
                event = "title_replan",
                words = message.split_whitespace().count()
            );
            None
        }
        p => p,
    };
    if let Some(parsed) = parsed {
        if *caller == CalendarCaller::Guarded {
            return Some(Err("calendar.create"));
        }
        let created = match parsed {
            Err((error, lang)) => {
                return Some(Ok(out(
                    cal::text(lang, "unclear").into(),
                    "calendar_create",
                    format!("[calendar create unclear: {error}]"),
                )))
            }
            Ok(c) => c,
        };
        return Some(Ok(create_event(s, st, created, key, caller, tz).await));
    }
    calendar_read(s, message, now, tz).await.map(Ok)
}

/// The link a reply gives to a created event: the Calendar tab for Lucina's own calendar,
/// the Google Calendar page for a Google event.
fn event_link(s: &Secretary, event: &Value) -> Option<String> {
    if let Some(u) = event["url"].as_str().filter(|u| u.starts_with("https://")) {
        return Some(u.to_owned());
    }
    let id = event["id"].as_str().filter(|i| i.starts_with("lucina:"))?;
    let base = s.url.as_deref()?.trim_end_matches('/');
    base.starts_with("https://")
        .then(|| format!("{base}/calendar?event={}", enc(id)))
}

/// Creates `created` for `caller` (not a guarded one): directly for the owner, with an Undo
/// for the chat `key`; a proposal for one of his clients, or when the write fails.
pub async fn create_event(
    s: &Secretary,
    st: &BridgeState,
    created: Create,
    key: String,
    caller: &CalendarCaller,
    tz: Tz,
) -> Outcome {
    let out = |reply: String, action: &str, action_result: String| Outcome {
        reply,
        action: action.into(),
        action_result,
        meta: Some(lucina_store::timeline::Meta::kind("calendar")),
        ..Outcome::default()
    };
    let title = created.event["title"].as_str().unwrap_or("").to_owned();
    let channel = match caller {
        CalendarCaller::Owner { channel } => channel,
        CalendarCaller::Client { from } => {
            let p = propose(s, &created, from, None).await;
            return if p.ok {
                out(
                    cal::proposed_text(&created, tz, None),
                    "calendar_create",
                    format!("[calendar event proposed: {title}]"),
                )
            } else {
                out(
                    cal::create_failed_text(created.lang, &p.code),
                    "calendar_create",
                    format!("[calendar proposal failed: {}]", p.code),
                )
            };
        }
        CalendarCaller::Guarded => {
            return out(
                cal::text(created.lang, "unclear").into(),
                "calendar_create",
                "[calendar create refused: guarded turn]".into(),
            )
        }
    };
    let r = s
        .call(Method::POST, "/v1/calendar/events", Some(&created.event))
        .await;
    let event = r.data.get("event").filter(|e| e.is_object()).cloned();
    if let (true, Some(event)) = (r.ok, event) {
        let id = event["id"].as_str().unwrap_or("").to_owned();
        let mut routes = Vec::new();
        if !id.is_empty() {
            st.calendar
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    key,
                    (
                        Created {
                            id,
                            title: title.clone(),
                            lang: created.lang,
                        },
                        Instant::now(),
                    ),
                );
            routes.push(undo_action(created.lang));
        }
        return Outcome {
            routes,
            events: vec![event.clone()],
            ..out(
                cal::added_text(&created, tz, event_link(s, &event).as_deref()),
                "calendar_create",
                format!("[calendar event created: {title}]"),
            )
        };
    }
    // Not connected, or Google failed: a proposal in Links, and the reply says why.
    let code = if r.ok {
        "no_event".to_owned()
    } else {
        r.code.clone()
    };
    let reason = cal::reason_text(created.lang, &code);
    let channel = if channel.is_empty() {
        "chat"
    } else {
        channel.as_str()
    };
    let p = propose(s, &created, &format!("lucina:{channel}"), Some(&reason)).await;
    if p.ok {
        return out(
            cal::proposed_text(&created, tz, Some(&reason)),
            "calendar_create",
            format!("[calendar create failed: {code}; proposed instead]"),
        );
    }
    let reply = if code == "google_reconnect_needed" || code == "google_not_connected" {
        cal::text(created.lang, "reconnect").to_owned()
    } else {
        cal::create_failed_text(created.lang, &code)
    };
    out(
        reply,
        "calendar_create",
        format!("[calendar create failed: {code}]"),
    )
}

fn calendar_outcome(reply: String, action_result: String) -> Outcome {
    Outcome {
        reply,
        action: "calendar".into(),
        action_result,
        meta: Some(lucina_store::timeline::Meta::kind("calendar")),
        ..Outcome::default()
    }
}

/// A calendar read: the events of the days asked ("cosa ho domani?") or the event asked about
/// ("dove si tiene la Hack Night di domani?"). `None` when the text is neither, or when no event
/// matches the one asked about (the planner then answers with [`calendar_context`]).
pub async fn calendar_read(
    s: &Secretary,
    message: &str,
    now: DateTime<Utc>,
    tz: Tz,
) -> Option<Outcome> {
    if let Some(q) = cal::parse_question(message, now, tz) {
        let r = events(s, &q.from, &q.to).await;
        if !r.ok {
            return Some(calendar_outcome(
                cal::text(q.lang, "unavailable").into(),
                format!("[calendar unavailable: {}]", r.code),
            ));
        }
        let evs = r.data["events"].as_array().cloned().unwrap_or_default();
        let mut reply = cal::format_answer(&q, &evs, now, tz);
        let mut sentence = (!evs.is_empty()).then(|| ev::listing_sentence(&q, evs.len(), now, tz));
        if r.data["sources"]["google"]["status"] == "reconnect" {
            reply.push_str(cal::text(q.lang, "reconnect_note"));
            if let Some(s) = sentence.as_mut() {
                s.push_str(cal::text(q.lang, "reconnect_note"));
            }
        }
        let action_result = format!("[calendar {}..{}: {} events]", q.from, q.to, evs.len());
        return Some(Outcome {
            events: evs,
            agenda: sentence.map(|sentence| crate::agenda::Agenda {
                sentence,
                lang: q.lang,
            }),
            ..calendar_outcome(reply, action_result)
        });
    }
    let q = ev::parse_event_question(message, now, tz)?;
    // Unreachable calendar: the planner answers, told so by [`calendar_context`].
    let r = events(s, &q.from, &q.to).await;
    if !r.ok {
        return None;
    }
    let evs = r.data["events"].as_array().cloned().unwrap_or_default();
    let aliases = person_aliases(&q);
    let hits = ev::select_with(&q, &evs, now, tz, &aliases);
    let first = hits.first()?;
    let sentence = ev::answer_sentence(first, now, tz, q.lang);
    let action_result = format!(
        "[calendar event \"{}\" {}..{}: {} of {} events]",
        q.topic,
        q.from,
        q.to,
        hits.len(),
        evs.len()
    );
    Some(Outcome {
        agenda: Some(crate::agenda::Agenda {
            sentence: sentence.clone(),
            lang: q.lang,
        }),
        events: hits,
        ..calendar_outcome(sentence, action_result)
    })
}

/// `buildCalendarContext`: the events a calendar question asks about.
pub async fn calendar_context(s: &Secretary, prompt: &str, now: DateTime<Utc>, tz: Tz) -> String {
    let Some(q) = cal::parse_question(prompt, now, tz) else {
        return event_context(s, prompt, now, tz).await;
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

/// Where the owner's "X is Y" statements about names are kept: `person-aliases.json` next to
/// the other state of this server (`LUCINA_DATA_DIR`, as in [`crate::config`]).
fn person_aliases_path() -> PathBuf {
    let dir = std::env::var("LUCINA_DATA_DIR").unwrap_or_else(|_| "data".into());
    PathBuf::from(dir)
        .join("lucina-rs")
        .join("person-aliases.json")
}

/// The names the owner said are one person, read on every event question. When the question
/// itself says it ("the meeting Therese è Teresa"), the pair is saved first, so a later question
/// naming only one of them finds the event too.
fn person_aliases(q: &ev::EventQuestion) -> PersonAliases {
    static WRITE: Mutex<()> = Mutex::new(());
    let path = person_aliases_path();
    if q.kind == ev::EventKind::Named {
        if let Some((a, b)) = lucina_intent::name_match::alias_to_remember(&q.topic) {
            let _guard = WRITE.lock().unwrap_or_else(|e| e.into_inner());
            if let Err(e) = PersonAliases::remember(&path, &a, &b) {
                tracing::warn!(error = %e, "person alias not saved");
            }
        }
    }
    PersonAliases::load(&path)
}

/// The calendar's answer to a question about one event, for the planner: the matching events,
/// or that none matches (so the model never answers it from memory or notes).
async fn event_context(s: &Secretary, prompt: &str, now: DateTime<Utc>, tz: Tz) -> String {
    let Some(q) = ev::parse_event_question(prompt, now, tz) else {
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
    let aliases = person_aliases(&q);
    let hits = ev::select_with(&q, &evs, now, tz, &aliases);
    if hits.is_empty() {
        return ev::no_match_context_in(&q, &evs, now, tz);
    }
    format!(
        "Giuseppe's calendar, events matching \"{}\" ({}): {}. Answer from this list only.",
        q.topic,
        tz.name(),
        cal::event_lines(&hits, tz, "en").join("; ")
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
pub const GAME_IDENTITY: &str = "This Pokémon Red run is YOU: your own playing self (the same Lucina) playing on nezopt through the game tools while Giuseppe watches. Never deny being the player and never say you cannot see the game: the state below is live. From chat you do not press buttons yourself; your playing self acts in its own turn loop, and you can pass it advice. When Giuseppe gives in-game advice or an instruction for the run (even without saying Pokémon, e.g. \"that was a tree to cut, go back there\"), choose {\"action\":\"game_hint\",\"text\":\"<his advice as one clear instruction to your playing self, in English>\",\"ack\":\"<short reply in English (Danish when he writes Danish; never Italian) saying you passed it to your playing self>\"}. To start or resume the run choose {\"action\":\"game_start\"}; to pause it {\"action\":\"game_pause\"} (only when he asks). Questions about the game (where are you, how is it going, why did you do that) are a normal reply from this state, in your own words. If he asks you to look up how to progress (next badge, an item, a blocked path), use your own Pokémon Red knowledge and pass the concrete next step to your playing self as game_hint.";

/// Live summary of Lucina's Pokémon Red run for the turn context ("" when the game server is off or not
/// answering). Built from `/state` and the head of `/observe` (place, team, bag, plan, objective, notes).
pub async fn game_context(http: &reqwest::Client) -> GameContext {
    let Some(base) = game_base() else {
        return GameContext::default();
    };
    let get = |p: &str| {
        http.get(format!("{base}{p}"))
            .timeout(Duration::from_millis(1500))
            .send()
    };
    let Ok(r) = get("/state").await else {
        return GameContext::default();
    };
    let Ok(state) = r.json::<Value>().await else {
        return GameContext::default();
    };
    let running = state["running"].as_bool().unwrap_or(false);
    let obs = match get("/observe").await {
        Ok(r) => r.text().await.unwrap_or_default(),
        Err(_) => String::new(),
    };
    GameContext {
        block: game_block(&state, &obs, running),
        running,
    }
}

/// The game block for the turn, and whether the run is playing right now. `/state` has no activity
/// timestamp, so `running` is the only liveness signal: a paused or finished run (the Hall of Fame, 2026-10-27)
/// stays in the context as information but is not a "live game" for routing.
#[derive(Debug, Clone, Default)]
pub struct GameContext {
    pub block: String,
    pub running: bool,
}

/// The context block from `/state` and `/observe` (pure, for tests).
pub fn game_block(state: &Value, obs: &str, running: bool) -> String {
    let head: String = obs
        .split("\nSCREEN TEXT:")
        .next()
        .unwrap_or("")
        .chars()
        .take(if running { 3000 } else { 700 })
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
                .take(if running { 6 } else { 2 })
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
    if running {
        s += &format!("\n{GAME_IDENTITY}\n{GAME_OVERRIDE}");
    } else {
        s += &format!("\n{GAME_PAUSED_NOTE}");
    }
    s
}

/// A paused or finished run: information only. It must not make unrelated messages game turns.
pub const GAME_PAUSED_NOTE: &str = "This Pokémon Red run is YOU (your own playing self, the same Lucina), but it is paused: the block above is background only. Use it only when Giuseppe's message is about the game, and then answer as the player, never denying it. For every other message ignore this block and do not choose game_hint, game_start or game_pause. Only if he asks to resume or start the run choose {\"action\":\"game_start\"}; if he asks for advice on it, {\"action\":\"game_hint\",\"text\":\"<one clear instruction, in English>\",\"ack\":\"<short reply in English (Danish when he writes Danish; never Italian)>\"}. An earlier reply of yours denying that you play was wrong.";

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

    #[test]
    fn a_paused_run_is_information_not_a_routing_rule() {
        let state = json!({"running": false, "actions": 17274, "events": ["a", "b", "c"]});
        let obs = format!(
            "MODE: Dialog\nLOCATION: CHAMPIONS_ROOM\n{}\nSCREEN TEXT:\nx",
            "y".repeat(2000)
        );
        let b = game_block(&state, &obs, false);
        assert!(b.contains("paused"));
        assert!(b.contains("CHAMPIONS_ROOM"));
        assert!(
            !b.contains(GAME_IDENTITY),
            "no rule that pushes game actions"
        );
        assert!(!b.contains(GAME_OVERRIDE));
        assert!(b.contains("do not choose game_hint, game_start or game_pause"));
        assert!(b.chars().count() < 1800, "clipped: {}", b.chars().count());
        assert!(!b.contains("Your last moves: a"), "only the last two moves");
        let live = game_block(&json!({"running": true}), &obs, true);
        assert!(live.contains(GAME_IDENTITY) && live.contains(GAME_OVERRIDE));
    }
}

#[cfg(test)]
mod calendar_create_tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TZ: Tz = chrono_tz::Europe::Copenhagen;

    fn now() -> DateTime<Utc> {
        // Thursday 29 October 2026, 20:00 in Copenhagen.
        DateTime::parse_from_rfc3339("2026-10-29T19:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn secretary(server: &MockServer, dir: &std::path::Path) -> Secretary {
        let tok = dir.join("svc.token");
        std::fs::write(&tok, "svc_test\n").unwrap();
        Secretary {
            http: reqwest::Client::new(),
            url: Some(server.uri()),
            token_file: Some(tok),
        }
    }

    const ASK: &str = "add Claude Max 5x unlocks to my calendar on 30/10 at 23:00";

    fn owner() -> CalendarCaller {
        CalendarCaller::Owner {
            channel: "ios".into(),
        }
    }

    #[tokio::test]
    async fn an_event_said_plainly_goes_in_his_calendar_with_a_link_to_it() {
        let server = MockServer::start().await;
        let event = json!({"id": "lucina:ev_1", "source": "lucina", "title": "Mentor session",
                           "start": "2026-11-06T11:30:00.000Z", "end": "2026-11-06T12:30:00.000Z", "allDay": false});
        Mock::given(method("POST"))
            .and(path("/v1/calendar/events"))
            .and(body_partial_json(json!({"title": "Mentor session", "start": "2026-11-06T12:30:00+01:00", "end": "2026-11-06T13:30:00+01:00"})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"created": true, "event": event})))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let s = secretary(&server, dir.path());
        let st = BridgeState::default();
        let o = calendar_turn(
            &s,
            &st,
            "Mentor session 6 november at 12:30",
            "chat-1",
            &owner(),
            now(),
            TZ,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            o.reply,
            "Added to your calendar: \"Mentor session\", Friday 6 November at 12:30."
        );
        assert_eq!(o.routes, vec![undo_action("en")]);
        // Live (an https server) the reply links to the event in the Calendar tab.
        let live = Secretary {
            url: Some("https://getsecretary.tech/".into()),
            ..secretary(&server, dir.path())
        };
        let link = event_link(&live, &o.events[0]);
        assert_eq!(
            link.as_deref(),
            Some("https://getsecretary.tech/calendar?event=lucina%3Aev_1")
        );
        let created = cal::parse_event("Mentor session 6 november at 12:30", now(), TZ)
            .unwrap()
            .unwrap();
        assert_eq!(
            cal::added_text(&created, TZ, link.as_deref()),
            "Added to your calendar: \"Mentor session\", Friday 6 November at 12:30. [Open in calendar](https://getsecretary.tech/calendar?event=lucina%3Aev_1)"
        );
        // A reminder is not the calendar's.
        assert!(calendar_turn(
            &s,
            &st,
            "ricordami domani alle 9 di pagare",
            "chat-1",
            &owner(),
            now(),
            TZ
        )
        .await
        .is_none());
    }

    #[tokio::test]
    async fn the_owner_gets_the_event_at_once_with_an_undo() {
        let server = MockServer::start().await;
        let event = json!({"id": "google:new1", "title": "Claude Max 5x unlocks", "start": "2026-10-30T22:00:00.000Z",
                           "end": "2026-10-30T23:00:00.000Z", "allDay": false, "url": "https://calendar.google.com/event?eid=new1"});
        Mock::given(method("POST"))
            .and(path("/v1/calendar/events"))
            .and(body_partial_json(
                json!({"title": "Claude Max 5x unlocks", "start": "2026-10-30T23:00:00+01:00"}),
            ))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(json!({"created": true, "event": event})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/calendar/proposals"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "prop_x"})))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/v1/calendar/events/google%3Anew1"))
            .and(query_param("confirm", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"deleted": true})))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let s = secretary(&server, dir.path());
        let st = BridgeState::default();
        let o = calendar_turn(&s, &st, ASK, "chat-1", &owner(), now(), TZ)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            o.reply,
            "Added to your calendar: \"Claude Max 5x unlocks\", Friday 30 October at 23:00. [Open in Google Calendar](https://calendar.google.com/event?eid=new1)"
        );
        assert_eq!(
            o.action_result,
            "[calendar event created: Claude Max 5x unlocks]"
        );
        assert_eq!(o.events.len(), 1);
        assert_eq!(o.routes, vec![undo_action("en")]);
        assert_eq!(o.routes[0]["payload"]["text"], "undo");
        // Undo in another chat does nothing; in the same chat it deletes the event, once.
        assert!(
            calendar_turn(&s, &st, "undo", "chat-2", &owner(), now(), TZ)
                .await
                .is_none()
        );
        let u = calendar_turn(&s, &st, "undo", "chat-1", &owner(), now(), TZ)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            u.reply,
            "Undone: \"Claude Max 5x unlocks\" is off your calendar."
        );
        assert!(
            calendar_turn(&s, &st, "undo", "chat-1", &owner(), now(), TZ)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn clients_propose_and_agents_are_refused() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/calendar/events"))
            .respond_with(ResponseTemplate::new(201))
            .expect(0)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/calendar/proposals"))
            .and(body_partial_json(
                json!({"title": "Claude Max 5x unlocks", "from": "mcp:claude-ai"}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "prop_1"})))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let s = secretary(&server, dir.path());
        let st = BridgeState::default();
        let client = CalendarCaller::Client {
            from: "mcp:claude-ai".into(),
        };
        let o = calendar_turn(&s, &st, ASK, "m", &client, now(), TZ)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            o.reply,
            "I proposed \"Claude Max 5x unlocks\", Friday 30 October at 23:00: confirm it in Secretary → Links."
        );
        assert!(o.routes.is_empty());
        assert_eq!(
            calendar_turn(&s, &st, ASK, "g", &CalendarCaller::Guarded, now(), TZ)
                .await
                .unwrap()
                .unwrap_err(),
            "calendar.create"
        );
        // No undo for a client's turn.
        assert!(calendar_turn(&s, &st, "undo", "m", &client, now(), TZ)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn a_google_failure_becomes_a_proposal_with_the_reason() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/calendar/events"))
            .respond_with(ResponseTemplate::new(409).set_body_json(
                json!({"error": {"code": "google_not_connected", "message": "Connect Google Calendar in Links first"}}),
            ))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/calendar/proposals"))
            .and(body_partial_json(
                json!({"from": "lucina:ios", "reason": "Google Calendar isn't connected"}),
            ))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "prop_2"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/calendar/proposals"))
            .and(body_partial_json(json!({"from": "lucina:telegram"})))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let s = secretary(&server, dir.path());
        let st = BridgeState::default();
        let o = calendar_turn(&s, &st, ASK, "chat-1", &owner(), now(), TZ)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            o.reply,
            "I couldn't add it directly (Google Calendar isn't connected), so I proposed it instead: \"Claude Max 5x unlocks\", Friday 30 October at 23:00. Confirm it in Secretary → Links."
        );
        assert!(o.routes.is_empty());
        // The proposal fails too: say so plainly.
        let tg = CalendarCaller::Owner {
            channel: "telegram".into(),
        };
        let o = calendar_turn(&s, &st, ASK, "chat-2", &tg, now(), TZ)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            o.reply,
            "I couldn't: Google Calendar needs reconnecting in Secretary → Links."
        );
    }
}
