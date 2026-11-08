//! `calendar`: the owner's calendars (his own calendar in Secretary, plus
//! Google Calendar, the iPhone calendar and ICS subscriptions, merged by
//! secretary-server) through `/v1/calendar/*` with the service token.
//!
//! - `list` (today, tomorrow, week, or a from/to range) and `free` (the next
//!   free slots of a given length) only read.
//! - `create` in the owner's own turn ([`Ctx::owner_turn`]) writes the event
//!   to his own calendar in Secretary at once (no Google needed); the reply
//!   offers an Undo for 10 minutes (a `delete` of that id in the same chat,
//!   which then needs no yes). When the write fails, and in every other turn
//!   (an MCP client, an agent, Lucina's own initiative), it files a proposal
//!   the owner confirms in Secretary.
//! - `move` and `delete` go through the confirmation flow: the call becomes a
//!   pending action and runs only after the owner's explicit yes. The server
//!   also demands `confirm: true` for edits and deletes, which this tool sends
//!   only when the confirmed action (or the undo) runs.
//!
//! Configuration: `LUCINA_SECRETARY_SERVER_URL` and
//! `LUCINA_SECRETARY_SERVICE_TOKEN_FILE` (shared with `avatar_outfit`).

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, Effect, SideEffect, Tool, ToolEnv, ToolError, ToolResult};

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
enum Action {
    List,
    Free,
    Create,
    Move,
    Delete,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Range {
    Today,
    Tomorrow,
    Week,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Args {
    action: Action,
    /// list/free: today, tomorrow or week (the next 7 days). Ignored when `from` is given.
    range: Option<Range>,
    /// list/free: first day, YYYY-MM-DD in the owner's time zone (or RFC 3339).
    from: Option<String>,
    /// list/free: last day, inclusive, YYYY-MM-DD (or RFC 3339). Default: same as `from`.
    to: Option<String>,
    /// free: slot length in minutes (default 60).
    minutes: Option<u32>,
    /// create: the title.
    title: Option<String>,
    /// create/move: new start, RFC 3339 with offset ("2026-10-23T09:30:00+02:00") or YYYY-MM-DD for all day.
    start: Option<String>,
    /// create/move: new end (optional; create defaults to one hour or one day; move keeps the duration).
    end: Option<String>,
    /// create: where.
    location: Option<String>,
    /// create: notes.
    description: Option<String>,
    /// move/delete: the event id from `list` (e.g. "google:abc123").
    id: Option<String>,
}

pub struct CalendarTool;

/// How long the Undo of a direct create stays available.
const UNDO_FOR: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Events created directly in the owner's turns: id → (session, when), for the Undo.
static CREATED: LazyLock<Mutex<HashMap<String, (String, Instant)>>> =
    LazyLock::new(Default::default);

fn created() -> std::sync::MutexGuard<'static, HashMap<String, (String, Instant)>> {
    let mut m = CREATED.lock().unwrap_or_else(|e| e.into_inner());
    m.retain(|_, (_, at)| at.elapsed() < UNDO_FOR);
    m
}

/// The session that created event `id` directly, within the undo window.
fn undo_session(id: &str) -> Option<String> {
    created().get(id).map(|(s, _)| s.clone())
}

/// secretary-server's base URL and the service token, when configured.
pub fn server_of(env: &ToolEnv) -> Option<(String, String)> {
    let url = env
        .secretary_url
        .as_deref()
        .map(|u| u.trim_end_matches('/').to_owned())
        .filter(|u| !u.is_empty())?;
    let token = env
        .secretary_service_token_file
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| t.trim().to_owned())
        .filter(|t| t.starts_with("svc_"))?;
    Some((url, token))
}

fn server(ctx: &Ctx) -> Result<(String, String), ToolError> {
    server_of(&ctx.env).ok_or_else(|| {
        ToolError::new("the calendar is not configured (LUCINA_SECRETARY_SERVER_URL and LUCINA_SECRETARY_SERVICE_TOKEN_FILE)")
    })
}

async fn send(env: &ToolEnv, req: reqwest::RequestBuilder) -> Result<Value, ToolError> {
    let _ = env;
    let resp = req.send().await.map_err(|e| {
        ToolError::new(format!("secretary-server unreachable: {}", e.without_url()))
    })?;
    let status = resp.status();
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(body);
    }
    let code = body["error"]["code"].as_str().unwrap_or_default();
    let message = body["error"]["message"].as_str().unwrap_or_default();
    Err(ToolError::new(match code {
        "google_reconnect_needed" => "Google Calendar needs reconnecting: ask the owner to open Secretary → Links and tap Reconnect.".to_owned(),
        "google_not_connected" => "Google Calendar is not connected: the owner can connect it in Secretary → Links.".to_owned(),
        "" => format!("secretary-server answered {}", status.as_u16()),
        _ => format!("{code}: {message}"),
    }))
}

fn local_day(now: DateTime<Utc>, tz: Tz, offset: i64) -> NaiveDate {
    now.with_timezone(&tz).date_naive() + Duration::days(offset)
}

fn bounds(a: &Args, now: DateTime<Utc>, tz: Tz) -> (String, String) {
    if let Some(f) = a.from.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let to =
            a.to.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(f);
        return (f.to_owned(), to.to_owned());
    }
    let (a0, a1) = match a.range.unwrap_or(if a.action == Action::Free {
        Range::Week
    } else {
        Range::Today
    }) {
        Range::Today => (0, 0),
        Range::Tomorrow => (1, 1),
        Range::Week => (0, 6),
    };
    (
        local_day(now, tz, a0).format("%Y-%m-%d").to_string(),
        local_day(now, tz, a1).format("%Y-%m-%d").to_string(),
    )
}

/// `HH:MM` (or the date for all-day events) in `tz`.
fn clock(v: &Value, tz: Tz) -> String {
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&tz).format("%a %d %b %H:%M").to_string())
        .unwrap_or_else(|| v.as_str().unwrap_or_default().to_owned())
}

/// Compact events for the model.
fn compact(events: &[Value], tz: Tz) -> Vec<Value> {
    events
        .iter()
        .take(100)
        .map(|e| {
            let all_day = e["allDay"].as_bool().unwrap_or(false);
            json!({
                "id": e["id"], "title": e["title"], "calendar": e["calendar"],
                "start": if all_day { e["start"].clone() } else { json!(clock(&e["start"], tz)) },
                "end": if all_day { e["end"].clone() } else { json!(clock(&e["end"], tz)) },
                "allDay": all_day, "location": e["location"], "editable": e["editable"],
            })
        })
        .collect()
}

fn need(field: &str, v: Option<&str>) -> Result<String, ToolError> {
    v.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ToolError::new(format!("{field} is required")))
}

fn event_id(v: Option<&str>) -> Result<String, ToolError> {
    let id = need("id", v)?;
    if id.len() > 1100
        || id
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '?' || c == '#')
    {
        return Err(ToolError::new("id must be an event id from list"));
    }
    Ok(id)
}

/// `seg` for a path segment.
fn seg(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b':') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[async_trait::async_trait]
impl Tool for CalendarTool {
    fn name(&self) -> &'static str {
        "calendar"
    }
    fn description(&self) -> &'static str {
        "Giuseppe's calendar: his own calendar in Secretary (the Calendar tab), merged with his iPhone calendar, \
         Google Calendar and subscribed ICS calendars. \
         list: events for today, tomorrow, the week, or from/to dates (with event ids). \
         free: the next free slots of `minutes` (default 60) in working hours. \
         create: a new event in his calendar (title, start, end?, location?). When Giuseppe asks for it himself \
         it is added at once (no yes needed; the reply has an Undo for 10 minutes, which is `delete` of that id); \
         if the write fails, or the request is not his, it becomes a proposal he confirms in Secretary. \
         move: change an event's start/end by id. delete: remove an event by id. \
         move and delete wait for Giuseppe's explicit yes; only his own (lucina:…) and Google events can be moved \
         or deleted (iPhone events change on the phone)."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn needs_confirmation(&self, args: &Value) -> bool {
        match args.get("action").and_then(Value::as_str) {
            // Direct for the owner, a proposal for everyone else: never a pending action.
            Some("list" | "free" | "create") => false,
            // The Undo of an event just created in a chat (`call` checks the session).
            Some("delete") => args
                .get("id")
                .and_then(Value::as_str)
                .and_then(undo_session)
                .is_none(),
            _ => true,
        }
    }
    fn summarize(&self, args: &Value) -> String {
        let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("");
        match s("action") {
            "create" => format!("calendar: add \"{}\" at {}", s("title"), s("start")),
            "move" => format!("calendar: move {} to {}", s("id"), s("start")),
            "delete" => format!("calendar: delete {}", s("id")),
            other => format!("calendar {other}"),
        }
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let (base, token) = server(ctx)?;
        let env = &ctx.env;
        let tz = env.time_zone;
        let http = &env.http;
        match a.action {
            Action::List => {
                let (from, to) = bounds(&a, ctx.now, tz);
                let v = send(
                    env,
                    http.get(format!("{base}/v1/calendar/events"))
                        .bearer_auth(&token)
                        .query(&[("from", from.as_str()), ("to", to.as_str())]),
                )
                .await?;
                let events = v["events"].as_array().cloned().unwrap_or_default();
                ctx.push_effect(Effect::CalendarEvents {
                    events: events.clone(),
                });
                let mut out = json!({
                    "from": from, "to": to, "timeZone": tz.name(),
                    "count": events.len(), "events": compact(&events, tz),
                });
                if v.pointer("/sources/google/status").and_then(Value::as_str) == Some("reconnect")
                {
                    out["note"] = json!("Google Calendar needs reconnecting in Secretary → Links; Google events may be stale.");
                }
                Ok(out)
            }
            Action::Free => {
                let (from, to) = bounds(&a, ctx.now, tz);
                let minutes = a.minutes.unwrap_or(60).clamp(5, 1440).to_string();
                let v = send(
                    env,
                    http.get(format!("{base}/v1/calendar/free"))
                        .bearer_auth(&token)
                        .query(&[
                            ("from", from.as_str()),
                            ("to", to.as_str()),
                            ("minutes", minutes.as_str()),
                        ]),
                )
                .await?;
                let slots: Vec<Value> = v["slots"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| json!({"start": clock(&s["start"], tz), "end": clock(&s["end"], tz)}))
                    .collect();
                Ok(
                    json!({"minutes": minutes, "timeZone": tz.name(), "next": slots.first(), "slots": slots}),
                )
            }
            Action::Create => {
                let mut body = json!({
                    "title": need("title", a.title.as_deref())?,
                    "start": need("start", a.start.as_deref())?,
                });
                for (k, v) in [
                    ("end", &a.end),
                    ("location", &a.location),
                    ("description", &a.description),
                ] {
                    if let Some(x) = v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                        body[k] = json!(x);
                    }
                }
                if !(ctx.owner_turn || ctx.confirmed) {
                    return propose(ctx, &base, &token, body, None).await;
                }
                let v = match send(
                    env,
                    http.post(format!("{base}/v1/calendar/events"))
                        .bearer_auth(&token)
                        .json(&body),
                )
                .await
                {
                    Ok(v) if v["event"].is_object() => v,
                    Ok(_) => {
                        return propose(ctx, &base, &token, body, Some("secretary-server returned no event".into())).await
                    }
                    Err(e) => return propose(ctx, &base, &token, body, Some(e.0)).await,
                };
                let event = v["event"].clone();
                ctx.push_effect(Effect::CalendarEvents {
                    events: vec![event.clone()],
                });
                ctx.push_effect(Effect::CalendarCreated {
                    event: event.clone(),
                });
                let id = event["id"].as_str().unwrap_or_default().to_owned();
                if !id.is_empty() {
                    created().insert(id.clone(), (ctx.session_id.clone(), Instant::now()));
                }
                let c = compact(std::slice::from_ref(&event), tz);
                // "Fri 30 Oct 23:00", or the date of an all-day event.
                let when = c[0]["start"].as_str().unwrap_or_default().to_owned();
                let title = event["title"].as_str().unwrap_or_default();
                Ok(json!({
                    "created": true,
                    "event": c.first(),
                    "link": event["url"],
                    "undo": {"action": "delete", "id": id, "minutes": UNDO_FOR.as_secs() / 60},
                    "instruction": format!("Done, it is in his calendar (the Calendar tab in Secretary). Reply in English (Danish when he writes Danish; never Italian) like: Added to your calendar: {title}, {when} (with the link if there is one). The reply has an Undo button for 10 minutes; if he says undo, call delete with this id."),
                }))
            }
            Action::Move => {
                let id = event_id(a.id.as_deref())?;
                let mut body =
                    json!({"confirm": true, "start": need("start", a.start.as_deref())?});
                if let Some(e) = a.end.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    body["end"] = json!(e);
                }
                let v = send(
                    env,
                    http.patch(format!("{base}/v1/calendar/events/{}", seg(&id)))
                        .bearer_auth(&token)
                        .json(&body),
                )
                .await?;
                if v["event"].is_object() {
                    ctx.push_effect(Effect::CalendarEvents {
                        events: vec![v["event"].clone()],
                    });
                }
                Ok(json!({"moved": true, "event": compact(&[v["event"].clone()], tz).first()}))
            }
            Action::Delete => {
                let id = event_id(a.id.as_deref())?;
                let undo = !ctx.confirmed;
                if undo && !(ctx.owner_turn && undo_session(&id).as_deref() == Some(ctx.session_id.as_str())) {
                    return Err(ToolError::new("delete needs the owner's explicit yes (only an event created in this chat in the last 10 minutes can be undone without it)"));
                }
                send(
                    env,
                    http.delete(format!("{base}/v1/calendar/events/{}", seg(&id)))
                        .bearer_auth(&token)
                        .query(&[("confirm", "true")]),
                )
                .await?;
                created().remove(&id);
                Ok(json!({"deleted": true, "id": id, "undone": undo}))
            }
        }
    }
}

/// Files `body` as a proposal the owner confirms in Secretary → Links: the
/// turn is not the owner's own (`reason: None`), or the direct create failed.
async fn propose(
    ctx: &Ctx,
    base: &str,
    token: &str,
    mut body: Value,
    reason: Option<String>,
) -> ToolResult {
    let channel = if ctx.channel.is_empty() { "chat" } else { ctx.channel.as_str() };
    body["from"] = json!(match ctx.origin {
        crate::Origin::Mcp => "lucina-mcp".to_owned(),
        crate::Origin::Owner => format!("lucina:{channel}"),
    });
    if let Some(r) = &reason {
        body["reason"] = json!(r.chars().take(300).collect::<String>());
    }
    let p = send(
        &ctx.env,
        ctx.env
            .http
            .post(format!("{base}/v1/calendar/proposals"))
            .bearer_auth(token)
            .json(&body),
    )
    .await
    .map_err(|e| match &reason {
        Some(r) => ToolError::new(format!("not created ({r}) and the proposal failed too: {}", e.0)),
        None => e,
    })?;
    let instruction = match &reason {
        Some(r) => format!("Not created: {r}. It is now a proposal waiting in Secretary → Links; tell the owner why in one short sentence and that he can confirm it there."),
        None => "Not created: this turn is not the owner's own request, so the event is a proposal he confirms in Secretary → Links. Say so.".to_owned(),
    };
    Ok(json!({
        "created": false,
        "proposed": true,
        "proposalId": p["id"],
        "reason": reason,
        "instruction": instruction,
    }))
}

/// One context line with today's events for the chat prompt, or `None`
/// when the calendar is not configured or does not answer within 3 s.
pub async fn today_context(env: &ToolEnv, now: DateTime<Utc>) -> Option<String> {
    let (base, token) = server_of(env)?;
    let tz = env.time_zone;
    let day = local_day(now, tz, 0).format("%Y-%m-%d").to_string();
    let req = env
        .http
        .get(format!("{base}/v1/calendar/events"))
        .bearer_auth(&token)
        .query(&[("from", day.as_str()), ("to", day.as_str())]);
    let v = tokio::time::timeout(std::time::Duration::from_secs(3), send(env, req))
        .await
        .ok()?
        .ok()?;
    let events = v["events"].as_array().cloned().unwrap_or_default();
    if events.is_empty() {
        return Some(format!(
            "Giuseppe's calendar today ({day}, {}): nothing scheduled.",
            tz.name()
        ));
    }
    let hm = |s: &Value| {
        s.as_str()
            .and_then(|x| DateTime::parse_from_rfc3339(x).ok())
            .map(|t| t.with_timezone(&tz).format("%H:%M").to_string())
            .unwrap_or_default()
    };
    let items: Vec<String> = events
        .iter()
        .take(12)
        .map(|e| {
            let title = e["title"].as_str().unwrap_or("(no title)");
            let when = if e["allDay"].as_bool() == Some(true) {
                "all day".to_owned()
            } else {
                format!("{}–{}", hm(&e["start"]), hm(&e["end"]))
            };
            match e["location"].as_str() {
                Some(l) if !l.is_empty() => format!("{when} {title} @ {l}"),
                _ => format!("{when} {title}"),
            }
        })
        .collect();
    let more = events.len().saturating_sub(12);
    Some(format!(
        "Giuseppe's calendar today ({day}, {}): {}{}.",
        tz.name(),
        items.join("; "),
        if more > 0 {
            format!("; and {more} more")
        } else {
            String::new()
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn env(server: &MockServer, dir: &std::path::Path) -> Arc<ToolEnv> {
        let tok = dir.join("svc.token");
        std::fs::write(&tok, "svc_testtoken\n").unwrap();
        let mut e = ToolEnv::new(dir, chrono_tz::Europe::Rome);
        e.secretary_url = Some(server.uri());
        e.secretary_service_token_file = Some(tok);
        Arc::new(e)
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-22T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn ev() -> Value {
        json!({"id": "google:abc", "source": "google", "title": "Dentist", "calendar": "Google",
               "start": "2026-10-23T07:30:00.000Z", "end": "2026-10-23T08:00:00.000Z", "allDay": false,
               "location": "Via Roma 1", "editable": true})
    }

    #[tokio::test]
    async fn list_tomorrow_and_context() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/calendar/events"))
            .and(header("authorization", "Bearer svc_testtoken"))
            .and(query_param("from", "2026-10-23"))
            .and(query_param("to", "2026-10-23"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"events": [ev()], "sources": {"google": {"status": "connected"}}}),
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/calendar/events"))
            .and(query_param("from", "2026-10-22"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"events": [
                {"title": "Gym", "start": "2026-10-22T16:00:00Z", "end": "2026-10-22T17:00:00Z", "allDay": false},
                {"title": "Anna's birthday", "start": "2026-10-22", "end": "2026-10-23", "allDay": true}]})))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let e = env(&server, dir.path()).await;
        let mut ctx = Ctx::new(e.clone(), "web", "s");
        ctx.now = now();
        let out = CalendarTool
            .call(json!({"action": "list", "range": "tomorrow"}), &ctx)
            .await
            .unwrap();
        assert_eq!(out["count"], 1);
        assert_eq!(out["events"][0]["start"], "Fri 23 Oct 09:30");
        assert_eq!(out["events"][0]["id"], "google:abc");
        // The raw events (RFC 3339 times, location) go to the reply as an effect.
        let listed: Vec<Value> = ctx
            .effects()
            .into_iter()
            .filter_map(|e| match e {
                Effect::CalendarEvents { events } => Some(events),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(listed, vec![ev()]);
        let line = today_context(&e, now()).await.unwrap();
        assert_eq!(line, "Giuseppe's calendar today (2026-10-22, Europe/Rome): 18:00–19:00 Gym; all day Anna's birthday.");
    }

    #[tokio::test]
    async fn owner_creates_directly_and_edits_need_confirmation() {
        assert!(!CalendarTool.needs_confirmation(&json!({"action": "list"})));
        assert!(!CalendarTool.needs_confirmation(&json!({"action": "free"})));
        assert!(!CalendarTool.needs_confirmation(&json!({"action": "create"})));
        for a in ["move", "delete"] {
            assert!(
                CalendarTool.needs_confirmation(&json!({"action": a})),
                "{a}"
            );
        }
        assert_eq!(
            CalendarTool.summarize(
                &json!({"action": "create", "title": "Padel", "start": "2026-10-24T10:00:00+02:00"})
            ),
            "calendar: add \"Padel\" at 2026-10-24T10:00:00+02:00"
        );
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/calendar/events"))
            .and(body_json(
                json!({"title": "Padel", "start": "2026-10-24T10:00:00+02:00", "location": "Club"}),
            ))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(json!({"created": true, "event": ev()})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path("/v1/calendar/events/google:abc"))
            .and(body_json(
                json!({"confirm": true, "start": "2026-10-23T11:00:00+02:00"}),
            ))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"updated": true, "event": ev()})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/v1/calendar/events/google:abc"))
            .and(query_param("confirm", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"deleted": true})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/calendar/free"))
            .and(query_param("minutes", "90"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"slots": [{"start": "2026-10-22T09:00:00Z", "end": "2026-10-22T12:00:00Z"}]})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/calendar/proposals"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "prop_x"})))
            .expect(0)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let e = env(&server, dir.path()).await;
        let ctx = Ctx::new(e, "web", "s").owner_turn();
        let c = CalendarTool
            .call(json!({"action": "create", "title": "Padel", "start": "2026-10-24T10:00:00+02:00", "location": "Club"}), &ctx)
            .await
            .unwrap();
        assert_eq!(c["created"], true);
        assert_eq!(c["undo"], json!({"action": "delete", "id": "google:abc", "minutes": 10}));
        assert!(
            c["instruction"].as_str().unwrap().contains("Added to your calendar: Dentist, Fri 23 Oct 09:30"),
            "{c}"
        );
        assert!(ctx
            .effects()
            .iter()
            .any(|e| matches!(e, Effect::CalendarCreated { event } if event["id"] == "google:abc")));
        CalendarTool
            .call(
                json!({"action": "move", "id": "google:abc", "start": "2026-10-23T11:00:00+02:00"}),
                &ctx,
            )
            .await
            .unwrap();
        CalendarTool
            .call(json!({"action": "delete", "id": "google:abc"}), &ctx)
            .await
            .unwrap();
        let f = CalendarTool
            .call(
                json!({"action": "free", "minutes": 90, "from": "2026-10-22"}),
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(f["next"]["start"], "Thu 22 Oct 11:00");
        let returned: Vec<Vec<Value>> = ctx
            .effects()
            .into_iter()
            .filter_map(|effect| match effect {
                Effect::CalendarEvents { events } => Some(events),
                _ => None,
            })
            .collect();
        assert_eq!(returned, vec![vec![ev()], vec![ev()]]);
        // The delete above was the Undo (same chat, within 10 minutes); used up, a delete needs a yes again.
        assert!(CalendarTool.needs_confirmation(&json!({"action": "delete", "id": "google:abc"})));
        assert!(CalendarTool
            .call(json!({"action": "delete", "id": "../x"}), &ctx)
            .await
            .is_err());
        assert!(CalendarTool
            .call(json!({"action": "create", "start": "x"}), &ctx)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn not_configured_and_reconnect() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::new(
            Arc::new(ToolEnv::new(dir.path(), chrono_tz::UTC)),
            "web",
            "s",
        );
        let err = CalendarTool
            .call(json!({"action": "list"}), &ctx)
            .await
            .unwrap_err();
        assert!(err.0.contains("not configured"));
        assert!(today_context(&ctx.env, now()).await.is_none());
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(409).set_body_json(
                json!({"error": {"code": "google_reconnect_needed", "message": "x"}}),
            ))
            .mount(&server)
            .await;
        let ctx = Ctx::new(env(&server, dir.path()).await, "web", "s").owner_turn();
        let err = CalendarTool
            .call(
                json!({"action": "create", "title": "x", "start": "2026-10-24"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.0.contains("Reconnect"), "{}", err.0);
    }

    #[tokio::test]
    async fn agents_and_failures_propose() {
        let server = MockServer::start().await;
        // Every POST to events fails: Google needs reconnecting.
        Mock::given(method("POST"))
            .and(path("/v1/calendar/events"))
            .respond_with(ResponseTemplate::new(409).set_body_json(
                json!({"error": {"code": "google_reconnect_needed", "message": "x"}}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/calendar/proposals"))
            .and(body_json(json!({"title": "Padel", "start": "2026-10-24T10:00:00+02:00", "from": "lucina-mcp"})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "prop_mcp"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/calendar/proposals"))
            .and(body_json(json!({"title": "Padel", "start": "2026-10-24T10:00:00+02:00", "from": "lucina:telegram"})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "prop_bg"})))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/calendar/proposals"))
            .and(body_json(json!({"title": "Dentist", "start": "2026-10-24T10:00:00+02:00", "from": "lucina:ios",
                "reason": "Google Calendar needs reconnecting: ask the owner to open Secretary → Links and tap Reconnect."})))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "prop_fail"})))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let e = env(&server, dir.path()).await;
        let padel = json!({"action": "create", "title": "Padel", "start": "2026-10-24T10:00:00+02:00"});

        // An agent through Lucina MCP: a proposal, not a refusal, and never a Google write.
        let mcp = Ctx::new(e.clone(), "web", "m").with_origin(crate::Origin::Mcp);
        let mut reg = crate::Registry::new();
        reg.register(Arc::new(CalendarTool)).unwrap();
        let gate = crate::agent::call_gate(&reg, "calendar", &padel, crate::Origin::Mcp);
        assert_eq!(gate, crate::agent::CallGate::Execute);
        let p = CalendarTool.call(padel.clone(), &mcp).await.unwrap();
        assert_eq!((p["created"].as_bool(), p["proposed"].as_bool(), p["proposalId"].as_str()), (Some(false), Some(true), Some("prop_mcp")));
        assert!(p["reason"].is_null());
        assert!(mcp.effects().is_empty());

        // Not the owner's own message (Lucina's own initiative): a proposal too.
        let bg = Ctx::new(e.clone(), "telegram", "b");
        let p = CalendarTool.call(padel, &bg).await.unwrap();
        assert_eq!(p["proposalId"], "prop_bg");

        // The owner's turn, but Google fails: a proposal with the reason.
        let owner = Ctx::new(e.clone(), "ios", "o").owner_turn();
        let p = CalendarTool
            .call(json!({"action": "create", "title": "Dentist", "start": "2026-10-24T10:00:00+02:00"}), &owner)
            .await
            .unwrap();
        assert_eq!((p["created"].as_bool(), p["proposalId"].as_str()), (Some(false), Some("prop_fail")));
        assert!(p["reason"].as_str().unwrap().contains("reconnecting"), "{p}");
        assert!(p["instruction"].as_str().unwrap().contains("Secretary → Links"));
        assert!(!owner.effects().iter().any(|e| matches!(e, Effect::CalendarCreated { .. })));
    }

    #[tokio::test]
    async fn undo_only_in_the_chat_that_created_it() {
        let server = MockServer::start().await;
        let made = json!({"id": "google:undo1", "title": "Claude Max 5x si sblocca", "start": "2026-10-30T22:00:00.000Z",
                          "end": "2026-10-30T23:00:00.000Z", "allDay": false, "url": "https://calendar.google.com/event?eid=undo1"});
        Mock::given(method("POST"))
            .and(path("/v1/calendar/events"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"created": true, "event": made})))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/v1/calendar/events/google:undo1"))
            .and(query_param("confirm", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"deleted": true})))
            .expect(1)
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let e = env(&server, dir.path()).await;
        let owner = Ctx::new(e.clone(), "ios", "chat-1").owner_turn();
        let c = CalendarTool
            .call(json!({"action": "create", "title": "Claude Max 5x si sblocca", "start": "2026-10-30T23:00:00+01:00"}), &owner)
            .await
            .unwrap();
        assert_eq!(c["link"], "https://calendar.google.com/event?eid=undo1");
        assert!(c["instruction"].as_str().unwrap().contains("Added to your calendar: Claude Max 5x si sblocca, Fri 30 Oct 23:00"), "{c}");
        let undo = json!({"action": "delete", "id": "google:undo1"});
        assert!(!CalendarTool.needs_confirmation(&undo));
        // Another chat, or a turn that is not the owner's, cannot use the undo.
        let other = Ctx::new(e.clone(), "ios", "chat-2").owner_turn();
        assert!(CalendarTool.call(undo.clone(), &other).await.is_err());
        let not_owner = Ctx::new(e.clone(), "ios", "chat-1");
        assert!(CalendarTool.call(undo.clone(), &not_owner).await.is_err());
        let next = Ctx::new(e, "ios", "chat-1").owner_turn();
        let d = CalendarTool.call(undo.clone(), &next).await.unwrap();
        assert_eq!((d["deleted"].as_bool(), d["undone"].as_bool()), (Some(true), Some(true)));
        assert!(CalendarTool.needs_confirmation(&undo));
    }
}
