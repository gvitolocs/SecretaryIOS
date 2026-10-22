//! `calendar`: the owner's calendars (Google Calendar, the iPhone calendar
//! and ICS subscriptions, merged by secretary-server) through
//! `/v1/calendar/*` with the service token.
//!
//! - `list` (today, tomorrow, week, or a from/to range) and `free` (the next
//!   free slots of a given length) only read.
//! - `create`, `move` and `delete` write to Google Calendar and always go
//!   through the confirmation flow: the call becomes a pending action and runs
//!   only after the owner's explicit yes. The server also demands
//!   `confirm: true` for edits and deletes, which this tool sends only when
//!   the confirmed action runs.
//!
//! Configuration: `LUCINA_SECRETARY_SERVER_URL` and
//! `LUCINA_SECRETARY_SERVICE_TOKEN_FILE` (shared with `avatar_outfit`).

use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolEnv, ToolError, ToolResult};

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
        "Giuseppe's calendar (Google Calendar, his iPhone calendar and subscribed ICS calendars, merged). \
         list: events for today, tomorrow, the week, or from/to dates (with event ids). \
         free: the next free slots of `minutes` (default 60) in working hours. \
         create: a new Google Calendar event (title, start, end?, location?). \
         move: change an event's start/end by id. delete: remove an event by id. \
         create, move and delete wait for Giuseppe's explicit yes before anything changes; \
         only Google events can be moved or deleted."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn needs_confirmation(&self, args: &Value) -> bool {
        !matches!(
            args.get("action").and_then(Value::as_str),
            Some("list" | "free")
        )
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
                let v = send(
                    env,
                    http.post(format!("{base}/v1/calendar/events"))
                        .bearer_auth(&token)
                        .json(&body),
                )
                .await?;
                Ok(json!({"created": true, "event": compact(&[v["event"].clone()], tz).first()}))
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
                Ok(json!({"moved": true, "event": compact(&[v["event"].clone()], tz).first()}))
            }
            Action::Delete => {
                let id = event_id(a.id.as_deref())?;
                send(
                    env,
                    http.delete(format!("{base}/v1/calendar/events/{}", seg(&id)))
                        .bearer_auth(&token)
                        .query(&[("confirm", "true")]),
                )
                .await?;
                Ok(json!({"deleted": true, "id": id}))
            }
        }
    }
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
        let line = today_context(&e, now()).await.unwrap();
        assert_eq!(line, "Giuseppe's calendar today (2026-10-22, Europe/Rome): 18:00–19:00 Gym; all day Anna's birthday.");
    }

    #[tokio::test]
    async fn writes_need_confirmation_and_send_confirm() {
        assert!(!CalendarTool.needs_confirmation(&json!({"action": "list"})));
        assert!(!CalendarTool.needs_confirmation(&json!({"action": "free"})));
        for a in ["create", "move", "delete"] {
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
        let dir = tempfile::tempdir().unwrap();
        let e = env(&server, dir.path()).await;
        let ctx = Ctx::new(e, "web", "s");
        let c = CalendarTool
            .call(json!({"action": "create", "title": "Padel", "start": "2026-10-24T10:00:00+02:00", "location": "Club"}), &ctx)
            .await
            .unwrap();
        assert_eq!(c["created"], true);
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
        let ctx = Ctx::new(env(&server, dir.path()).await, "web", "s");
        let err = CalendarTool
            .call(
                json!({"action": "create", "title": "x", "start": "2026-10-24"}),
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.0.contains("Reconnect"), "{}", err.0);
    }
}
