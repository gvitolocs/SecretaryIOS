//! The calendar module: sync of Google Calendar (incremental, sync tokens)
//! and ICS subscriptions into `$SECRETARY_DATA_DIR/calendar.json`, the
//! iPhone's EventKit snapshot (from `POST /v1/device/state`), and the
//! `/v1/calendar/*` routes. Expansion and merging are
//! `secretary-calendar`'s.
//!
//! Lucina's own calendar (`calendar-events.json`, ids `lucina:…`) is where
//! new events go: the Calendar tab, Lucina's "add it to my calendar" and
//! accepted proposals all write there, with no Google grant needed. Google
//! events can still be written (`target: "google"` on create; `google:` ids
//! on edit/delete); the iPhone calendar is written by the app's EventKit
//! bridge and ICS feeds are read-only. Edits and deletes need
//! `confirm: true` (`?confirm=true` for DELETE): the app asks the owner
//! first. Creates from the owner's own turns with Lucina go straight to
//! `POST /v1/calendar/events`; clients that may not write (the Lucina MCP,
//! agents) file a proposal instead, which the owner accepts in the app, and
//! so does Lucina when her direct create failed (with a `reason`).

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use futures_util::StreamExt;
use reqwest::Method;
use secretary_calendar::model::{day_start, iso};
use secretary_calendar::view::SourceItems;
use secretary_calendar::{google as gcal, ics, Event, Item, Moment, Source, Window};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::auth::{Identity, SERVICE_DEVICE_ID};
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::google::{seg, GError};
use crate::links::{bad, now_iso, random_id, text_field, ConnStatus, GoogleCache, Hub, Proposal};
use crate::AppState;

/// Largest ICS body read.
pub const ICS_MAX_BYTES: usize = 10 * 1024 * 1024;
/// Google full sync starts this far back.
const FULL_SYNC_DAYS: i64 = 365;
const MAX_PAGES: usize = 200;
const MAX_PROPOSALS: usize = 50;
const PROPOSAL_TTL_MS: i64 = 7 * 24 * 3600 * 1000;
const PRIMARY: &str = "/calendar/v3/calendars/primary/events";
/// The label of Lucina's own calendar.
pub const OWN_LABEL: &str = "Lucina";
const MAX_OWN_EVENTS: usize = 20_000;
/// Longest notes kept of an iPhone event (the phone reads the full text itself).
const MAX_DEVICE_NOTES: usize = 2000;
/// How far ahead Lucina's own events feed "Coming up" notifications.
const OWN_REMINDER_DAYS: i64 = 35;

pub fn gerror(e: GError) -> ApiError {
    match e {
        GError::NotConnected => ApiError::new(
            StatusCode::CONFLICT,
            "google_not_connected",
            "Connect Google Calendar in Links first",
        ),
        GError::Revoked => ApiError::new(
            StatusCode::CONFLICT,
            "google_reconnect_needed",
            "Google access expired or was revoked: reconnect it in Links",
        ),
        GError::NotFound => {
            ApiError::new(StatusCode::NOT_FOUND, "event_not_found", "no such event")
        }
        GError::Http {
            status: 400,
            message,
        } => ApiError::new(StatusCode::BAD_REQUEST, "google_rejected", message),
        GError::Network(_) => ApiError::new(
            StatusCode::BAD_GATEWAY,
            "google_unreachable",
            "Google is not reachable",
        ),
        other => ApiError::new(StatusCode::BAD_GATEWAY, "google_error", other.to_string()),
    }
}

// -- ICS -------------------------------------------------------------------------

pub enum IcsFetch {
    NotModified,
    Fresh {
        items: Vec<Item>,
        name: Option<String>,
        etag: Option<String>,
        last_modified: Option<String>,
    },
}

/// GET a feed (conditional), at most [`ICS_MAX_BYTES`], and parse it.
/// Errors are short, URL-free strings.
pub async fn fetch_ics(
    hub: &Hub,
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<IcsFetch, String> {
    let mut req = hub
        .http
        .get(url)
        .header("Accept", "text/calendar, */*;q=0.5");
    if let Some(e) = etag {
        req = req.header("If-None-Match", e);
    }
    if let Some(l) = last_modified {
        req = req.header("If-Modified-Since", l);
    }
    let resp = req.send().await.map_err(|e| {
        if e.is_timeout() {
            "timed out".to_owned()
        } else {
            "unreachable".to_owned()
        }
    })?;
    if resp.status() == StatusCode::NOT_MODIFIED {
        return Ok(IcsFetch::NotModified);
    }
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let header = |n: &str| {
        resp.headers()
            .get(n)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let (etag, last_modified) = (header("etag"), header("last-modified"));
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "read failed".to_owned())?;
        if body.len() + chunk.len() > ICS_MAX_BYTES {
            return Err("feed larger than 10 MB".into());
        }
        body.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&body);
    if !text[..text.len().min(65_536)]
        .to_ascii_uppercase()
        .contains("BEGIN:VCALENDAR")
    {
        return Err("not an iCalendar feed".into());
    }
    let feed = ics::parse(&text, hub.tz.name());
    Ok(IcsFetch::Fresh {
        items: feed.items,
        name: feed.name,
        etag,
        last_modified,
    })
}

async fn sync_ics(hub: &Hub) -> Vec<Value> {
    let subs = hub.data.lock().await.links.ics.clone();
    let mut out = Vec::new();
    for s in subs {
        let url = match hub.sealer.open(&format!("ics-url:{}", s.id), &s.url) {
            Ok(u) => u,
            Err(_) => {
                out.push(json!({"id": s.id, "ok": false, "error": "stored address unreadable"}));
                continue;
            }
        };
        let r = fetch_ics(hub, &url, s.etag.as_deref(), s.last_modified.as_deref()).await;
        let mut d = hub.data.lock().await;
        let Some(sub) = d.links.ics.iter_mut().find(|x| x.id == s.id) else {
            continue; // removed meanwhile
        };
        match r {
            Ok(IcsFetch::NotModified) => {
                sub.last_sync_at = Some(now_iso());
                sub.last_error = None;
                out.push(json!({"id": s.id, "ok": true, "changed": false}));
            }
            Ok(IcsFetch::Fresh {
                items,
                name,
                etag,
                last_modified,
            }) => {
                sub.last_sync_at = Some(now_iso());
                sub.last_error = None;
                sub.event_count = items.len();
                sub.feed_name = name;
                sub.etag = etag;
                sub.last_modified = last_modified;
                d.cache.ics.insert(s.id.clone(), items);
                hub.save_cache(&d);
                out.push(json!({"id": s.id, "ok": true, "changed": true}));
            }
            Err(e) => {
                sub.last_error = Some(e.clone());
                out.push(json!({"id": s.id, "ok": false, "error": e}));
            }
        }
        let _ = hub.save_links(&d);
    }
    out
}

// -- Google sync -------------------------------------------------------------------

/// Incremental sync with the stored sync token; a full sync (last
/// [`FULL_SYNC_DAYS`] days onwards) without one or after `410 Gone`.
/// Returns the number of changed items.
pub async fn sync_google(hub: &Hub) -> Result<usize, GError> {
    {
        let d = hub.data.lock().await;
        match &d.links.google {
            None => return Err(GError::NotConnected),
            Some(g) if g.status == ConnStatus::Reconnect => return Err(GError::Revoked),
            Some(g) if !crate::google::services_of(&g.scopes).contains(&"calendar") => {
                return Err(GError::NotConnected)
            }
            _ => {}
        }
    }
    let mut token = hub.data.lock().await.cache.google.sync_token.clone();
    for _attempt in 0..2 {
        let full = token.is_none();
        let mut events: Vec<Value> = Vec::new();
        let mut page: Option<String> = None;
        let mut next_sync: Option<String> = None;
        let mut cal_tz: Option<String> = None;
        let mut defaults: Option<Vec<i64>> = None;
        let mut gone = false;
        for _ in 0..MAX_PAGES {
            let mut q: Vec<(&str, String)> = vec![
                ("maxResults", "250".into()),
                ("singleEvents", "false".into()),
            ];
            match &token {
                Some(t) => q.push(("syncToken", t.clone())),
                None => {
                    q.push(("showDeleted", "false".into()));
                    q.push((
                        "timeMin",
                        iso(Utc::now() - chrono::Duration::days(FULL_SYNC_DAYS)),
                    ));
                }
            }
            if let Some(p) = &page {
                q.push(("pageToken", p.clone()));
            }
            match hub.google_api(Method::GET, PRIMARY, &q, None).await {
                Ok(v) => {
                    events.extend(
                        v.get("items")
                            .and_then(Value::as_array)
                            .cloned()
                            .unwrap_or_default(),
                    );
                    defaults = gcal::default_reminders(&v).or(defaults);
                    cal_tz = v
                        .get("timeZone")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .or(cal_tz);
                    page = v
                        .get("nextPageToken")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    if page.is_none() {
                        next_sync = v
                            .get("nextSyncToken")
                            .and_then(Value::as_str)
                            .map(str::to_owned);
                        break;
                    }
                }
                Err(GError::Gone) if !full => {
                    gone = true;
                    break;
                }
                Err(e) => {
                    let mut d = hub.data.lock().await;
                    if let Some(g) = d.links.google.as_mut() {
                        g.last_error = Some(e.to_string());
                    }
                    let _ = hub.save_links(&d);
                    return Err(e);
                }
            }
        }
        if gone {
            tracing::info!("google sync token expired (410); full resync");
            token = None;
            continue;
        }
        let mut d = hub.data.lock().await;
        if d.links.google.is_none() {
            // Disconnected while this sync ran: keep nothing.
            return Err(GError::NotConnected);
        }
        let tz = cal_tz
            .clone()
            .or_else(|| d.cache.google.calendar_tz.clone())
            .unwrap_or_else(|| hub.tz.name().to_owned());
        let cache = &mut d.cache.google;
        if full {
            *cache = GoogleCache {
                calendar_tz: Some(tz.clone()),
                ..GoogleCache::default()
            };
        }
        let changed = gcal::apply_page(&mut cache.items, &events, &tz);
        if defaults.is_some() {
            cache.default_reminders = defaults;
        }
        cache.sync_token = next_sync;
        cache.calendar_tz = Some(tz);
        if let Some(g) = d.links.google.as_mut() {
            g.last_sync_at = Some(now_iso());
            g.last_error = None;
        }
        let _ = hub.save_links(&d);
        hub.save_cache(&d);
        return Ok(changed);
    }
    Err(GError::Gone)
}

/// Sync every source once (serialized).
pub async fn sync_all(st: &AppState) -> Value {
    let hub = &st.links;
    let _guard = hub.sync_lock.lock().await;
    let google = match sync_google(hub).await {
        Ok(n) => json!({"ok": true, "changed": n}),
        Err(GError::NotConnected) => json!({"ok": false, "error": "not_connected"}),
        Err(GError::Revoked) => json!({"ok": false, "error": "reconnect_needed"}),
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    };
    let ics = sync_ics(hub).await;
    json!({"google": google, "ics": ics})
}

/// Sync every `every` (the first run after 10 s).
pub fn spawn_sync_loop(st: AppState, every: Duration) {
    let mut stop = st.shutdown.subscribe();
    tokio::spawn(async move {
        let mut wait = Duration::from_secs(10);
        loop {
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                _ = stop.changed() => return,
            }
            let r = sync_all(&st).await;
            tracing::debug!(result = %r, "calendar sync");
            // Time moves on: the next weeks of Lucina's own events.
            refresh_own_reminders(&st).await;
            wait = every;
        }
    });
}

// -- the iPhone snapshot ---------------------------------------------------------------

fn moment_of(v: &Value, all_day: bool, tz: Tz) -> Option<Moment> {
    let t = match v {
        Value::Number(n) => DateTime::<Utc>::from_timestamp_millis(n.as_i64()?)?,
        Value::String(s) => {
            if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                return Some(Moment::Date(d));
            }
            DateTime::parse_from_rfc3339(s).ok()?.with_timezone(&Utc)
        }
        _ => return None,
    };
    Some(if all_day {
        Moment::Date(t.with_timezone(&tz).date_naive())
    } else {
        Moment::Utc(t)
    })
}

/// Keep the events of a `POST /v1/device/state` snapshot (`{id, title,
/// start, end, allDay?, location?, calendar?, notes?, url?, alarms?, free?}`,
/// opaque otherwise; `alarms`: minutes before the start).
pub async fn ingest_device_events(st: &AppState, device: &str, events: &[Value]) {
    let hub = &st.links;
    let tz = hub.tz;
    let items: Vec<Item> = events
        .iter()
        .take(500)
        .filter_map(|e| {
            let all_day = e.get("allDay").and_then(Value::as_bool).unwrap_or(false);
            let start = moment_of(e.get("start")?, all_day, tz)?;
            let end = e.get("end").and_then(|x| moment_of(x, all_day, tz));
            let title = e
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(300)
                .collect::<String>();
            // EventKit gives every occurrence of a recurring event the same
            // identifier: the start makes each one unique.
            let start_ms = start.instant(tz).timestamp_millis();
            let id = match e
                .get("id")
                .and_then(Value::as_str)
                .filter(|x| !x.is_empty())
            {
                Some(x) => format!("{x}@{start_ms}"),
                None => secretary_calendar::model::short_hash(&format!("{title}|{start_ms}")),
            };
            let s = |k: &str| {
                e.get(k)
                    .and_then(Value::as_str)
                    .map(|x| x.chars().take(500).collect::<String>())
                    .filter(|x| !x.is_empty())
            };
            Some(Item {
                attendees: e
                    .get("attendees")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .take(30)
                    .map(str::to_owned)
                    .collect(),
                uid: id.chars().take(200).collect(),
                title,
                start: Some(start),
                end,
                location: s("location"),
                description: e
                    .get("notes")
                    .and_then(Value::as_str)
                    .map(|x| x.chars().take(MAX_DEVICE_NOTES).collect::<String>())
                    .filter(|x| !x.is_empty()),
                url: s("url").filter(|u| u.starts_with("https://") || u.starts_with("http://")),
                calendar: s("calendar"),
                alarms: secretary_calendar::model::normalize_alarms(
                    e.get("alarms")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_i64)
                        .take(20)
                        .collect(),
                ),
                free: e.get("free").and_then(Value::as_bool).unwrap_or(false),
                ..Item::default()
            })
        })
        .collect();
    let mut d = hub.data.lock().await;
    d.cache.iphone.device = Some(device.to_owned());
    d.cache.iphone.reported_at = Some(now_iso());
    d.cache.iphone.items = items;
    hub.save_cache(&d);
}

// -- queries -----------------------------------------------------------------------

/// Every event overlapping `[from, to)`, all sources merged.
pub async fn events_between(
    hub: &Hub,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    only: Option<&[Source]>,
) -> Vec<Event> {
    let d = hub.data.lock().await;
    let google_items: Vec<Item> = d.cache.google.items.values().cloned().collect();
    let own_items: Vec<Item> = d
        .own
        .events
        .values()
        .filter_map(|v| gcal::to_item(v, hub.tz.name()))
        .collect();
    let want = |s: Source| only.is_none_or(|o| o.contains(&s));
    let mut sources: Vec<SourceItems<'_>> = Vec::new();
    if want(Source::Lucina) {
        sources.push(SourceItems {
            source: Source::Lucina,
            key: "",
            label: Some(OWN_LABEL),
            items: &own_items,
        });
    }
    if want(Source::Google) && d.links.google.is_some() {
        sources.push(SourceItems {
            source: Source::Google,
            key: "",
            label: Some("Google"),
            items: &google_items,
        });
    }
    if want(Source::Ics) {
        for s in &d.links.ics {
            if let Some(items) = d.cache.ics.get(&s.id) {
                sources.push(SourceItems {
                    source: Source::Ics,
                    key: &s.id,
                    label: Some(&s.label),
                    items,
                });
            }
        }
    }
    if want(Source::Iphone) {
        sources.push(SourceItems {
            source: Source::Iphone,
            key: "",
            label: Some("iPhone"),
            items: &d.cache.iphone.items,
        });
    }
    let mut events = secretary_calendar::window(&sources, Window { from, to }, hub.tz);
    secretary_calendar::view::drop_mirrored(&mut events);
    if let Some(defaults) = &d.cache.google.default_reminders {
        for e in events
            .iter_mut()
            .filter(|e| e.source == Source::Google && e.alarms_default && e.alarms.is_empty())
        {
            e.alarms.clone_from(defaults);
        }
    }
    events
}

/// The events of one local date (for `/v1/agenda`).
pub async fn events_on(hub: &Hub, date: NaiveDate) -> Vec<Event> {
    let from = day_start(hub.tz, date);
    let to = day_start(hub.tz, date + chrono::Duration::days(1));
    events_between(hub, from, to, None).await
}

fn parse_bound(field: &str, v: &str, tz: Tz, end: bool) -> Result<DateTime<Utc>, ApiError> {
    if let Ok(d) = NaiveDate::parse_from_str(v, "%Y-%m-%d") {
        // A date `to` is inclusive: the end of that day.
        return Ok(day_start(
            tz,
            if end {
                d + chrono::Duration::days(1)
            } else {
                d
            },
        ));
    }
    DateTime::parse_from_rfc3339(v)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| {
            bad(format!(
                "{field}: YYYY-MM-DD or an RFC 3339 time with offset"
            ))
        })
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RangeQuery {
    from: Option<String>,
    to: Option<String>,
    days: Option<i64>,
    sources: Option<String>,
    minutes: Option<i64>,
    day_start: Option<u32>,
    day_end: Option<u32>,
    limit: Option<usize>,
}

fn range(q: &RangeQuery, tz: Tz) -> Result<(DateTime<Utc>, DateTime<Utc>), ApiError> {
    let from = match q.from.as_deref() {
        Some(f) => parse_bound("from", f, tz, false)?,
        None => day_start(tz, Utc::now().with_timezone(&tz).date_naive()),
    };
    let to = match (q.to.as_deref(), q.days) {
        (Some(t), _) => parse_bound("to", t, tz, true)?,
        (None, Some(n)) if (1..=secretary_calendar::MAX_WINDOW_DAYS).contains(&n) => {
            from + chrono::Duration::days(n)
        }
        (None, Some(_)) => {
            return Err(bad(format!(
                "days: 1-{}",
                secretary_calendar::MAX_WINDOW_DAYS
            )))
        }
        (None, None) => from + chrono::Duration::days(7),
    };
    if to <= from {
        return Err(bad("to: must be after from"));
    }
    if to - from > chrono::Duration::days(secretary_calendar::MAX_WINDOW_DAYS) {
        return Err(bad(format!(
            "the range is at most {} days",
            secretary_calendar::MAX_WINDOW_DAYS
        )));
    }
    Ok((from, to))
}

async fn sources_status(hub: &Hub) -> Value {
    let d = hub.data.lock().await;
    json!({
        "google": d.links.google.as_ref().map(|g| json!({
            "status": if g.status == ConnStatus::Reconnect { "reconnect" } else { "connected" },
            "account": g.account, "lastSyncAt": g.last_sync_at, "error": g.last_error,
        })),
        "ics": d.links.ics.iter().map(|s| json!({"id": s.id, "label": s.label, "lastSyncAt": s.last_sync_at, "error": s.last_error})).collect::<Vec<_>>(),
        "iphone": {"device": d.cache.iphone.device, "reportedAt": d.cache.iphone.reported_at},
        "lucina": {"count": d.own.events.len()},
    })
}

async fn list_events(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<RangeQuery>,
) -> Result<Json<Value>, ApiError> {
    let hub = &st.links;
    let (from, to) = range(&q, hub.tz)?;
    let only: Option<Vec<Source>> = match q.sources.as_deref() {
        None | Some("") => None,
        Some(s) => Some(
            s.split(',')
                .map(|x| match x.trim() {
                    "google" => Ok(Source::Google),
                    "ics" => Ok(Source::Ics),
                    "iphone" => Ok(Source::Iphone),
                    "lucina" => Ok(Source::Lucina),
                    _ => Err(bad("sources: lucina, google, ics, iphone")),
                })
                .collect::<Result<_, _>>()?,
        ),
    };
    let events = events_between(hub, from, to, only.as_deref()).await;
    Ok(Json(json!({
        "from": iso(from), "to": iso(to), "timeZone": hub.tz.name(),
        "events": events, "sources": sources_status(hub).await,
    })))
}

async fn free(
    State(st): State<AppState>,
    QueryArgs(q): QueryArgs<RangeQuery>,
) -> Result<Json<Value>, ApiError> {
    let hub = &st.links;
    let (mut from, to) = range(&q, hub.tz)?;
    from = from.max(Utc::now());
    let minutes = q.minutes.unwrap_or(60);
    if !(5..=24 * 60).contains(&minutes) {
        return Err(bad("minutes: 5-1440"));
    }
    let (h0, h1) = (q.day_start.unwrap_or(9), q.day_end.unwrap_or(19));
    if h0 > 23 || h1 > 24 || h1 <= h0 {
        return Err(bad("dayStart/dayEnd: hours with dayStart < dayEnd"));
    }
    let events = events_between(hub, from, to, None).await;
    let sq = secretary_calendar::SlotQuery {
        from,
        to,
        minutes,
        day_start_hour: h0,
        day_end_hour: h1,
        max: q.limit.unwrap_or(5).clamp(1, 50),
    };
    let slots = secretary_calendar::free_slots(&events, &sq, hub.tz);
    Ok(Json(json!({
        "timeZone": hub.tz.name(), "minutes": minutes,
        "slots": slots.iter().map(|(a, b)| json!({"start": iso(*a), "end": iso(*b)})).collect::<Vec<_>>(),
    })))
}

async fn sync_now(State(st): State<AppState>) -> Json<Value> {
    Json(sync_all(&st).await)
}

// -- writes ------------------------------------------------------------------------

/// A validated new event: the Google body.
fn google_body(
    v: &Map<String, Value>,
    tz: Tz,
    partial: bool,
) -> Result<Map<String, Value>, ApiError> {
    let mut out = Map::new();
    let s = |k: &str| v.get(k).and_then(Value::as_str);
    if let Some(t) = s("title") {
        out.insert("summary".into(), json!(text_field("title", t, 300)?));
    } else if !partial {
        return Err(bad("title: required"));
    }
    for (k, g, max) in [
        ("location", "location", 500),
        ("description", "description", 4000),
    ] {
        if let Some(x) = v.get(k) {
            match x {
                Value::Null => {
                    out.insert(g.into(), json!(""));
                }
                Value::String(t) if t.trim().is_empty() => {
                    out.insert(g.into(), json!(""));
                }
                Value::String(t) if t.chars().count() <= max => {
                    out.insert(g.into(), json!(t.trim()));
                }
                _ => return Err(bad(format!("{k}: at most {max} characters"))),
            }
        }
    }
    let zone = match s("timeZone") {
        Some(z) => secretary_calendar::parse_tz(z)
            .ok_or_else(|| bad("timeZone: unknown IANA time zone"))?,
        None => tz,
    };
    let all_day = v.get("allDay").and_then(Value::as_bool);
    let start = s("start");
    let end = s("end");
    if start.is_none() && end.is_some() && !partial {
        return Err(bad("start: required"));
    }
    if let Some(st) = start {
        let date = NaiveDate::parse_from_str(st, "%Y-%m-%d").ok();
        let is_all_day = all_day.unwrap_or(date.is_some());
        if is_all_day {
            let d = date.ok_or_else(|| bad("start: YYYY-MM-DD for an all-day event"))?;
            let e = match end {
                Some(e) => NaiveDate::parse_from_str(e, "%Y-%m-%d")
                    .map_err(|_| bad("end: YYYY-MM-DD (exclusive)"))?,
                None => d + chrono::Duration::days(1),
            };
            if e <= d || (e - d).num_days() > 366 {
                return Err(bad("end: after start, within a year"));
            }
            out.insert(
                "start".into(),
                json!({"date": d.format("%Y-%m-%d").to_string()}),
            );
            out.insert(
                "end".into(),
                json!({"date": e.format("%Y-%m-%d").to_string()}),
            );
        } else {
            let a = DateTime::parse_from_rfc3339(st)
                .map_err(|_| bad("start: RFC 3339 with offset, e.g. 2026-10-23T09:30:00+02:00"))?;
            let b = match end {
                Some(e) => {
                    DateTime::parse_from_rfc3339(e).map_err(|_| bad("end: RFC 3339 with offset"))?
                }
                None => a + chrono::Duration::hours(1),
            };
            if b <= a || (b - a).num_days() > 31 {
                return Err(bad("end: after start, within 31 days"));
            }
            let fmt = |t: DateTime<chrono::FixedOffset>| {
                t.with_timezone(&zone)
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
            };
            out.insert(
                "start".into(),
                json!({"dateTime": fmt(a), "timeZone": zone.name()}),
            );
            out.insert(
                "end".into(),
                json!({"dateTime": fmt(b), "timeZone": zone.name()}),
            );
        }
    } else if end.is_some() || all_day.is_some() {
        return Err(bad("start: required when changing end or allDay"));
    }
    settings_fields(v, &mut out)?;
    if partial && out.is_empty() {
        return Err(bad("nothing to change"));
    }
    Ok(out)
}

/// The event settings of a request: `alarms` (minutes before the start; null:
/// the calendar's defaults), `free` (show as free), `travelMin` (null or 0:
/// none) and `leaveAlert`. Travel time and the leave alert go to private
/// extended properties, `""` clearing one (see [`merge_private`]).
fn settings_fields(v: &Map<String, Value>, out: &mut Map<String, Value>) -> Result<(), ApiError> {
    use secretary_calendar::model::{MAX_ALARMS, MAX_ALARM_MIN, MAX_TRAVEL_MIN};
    match v.get("alarms") {
        None => {}
        Some(Value::Null) => {
            out.insert("reminders".into(), json!({"useDefault": true}));
        }
        Some(Value::Array(a)) => {
            let mins: Option<Vec<i64>> = a
                .iter()
                .map(|m| m.as_i64().filter(|m| (0..=MAX_ALARM_MIN).contains(m)))
                .collect();
            let mins = mins.ok_or_else(|| {
                bad(format!(
                    "alarms: minutes before the start, 0-{MAX_ALARM_MIN}"
                ))
            })?;
            let mins = secretary_calendar::model::normalize_alarms(mins);
            if a.len() > MAX_ALARMS {
                return Err(bad(format!("alarms: at most {MAX_ALARMS}")));
            }
            let overrides: Vec<Value> = mins
                .iter()
                .map(|m| json!({"method": "popup", "minutes": m}))
                .collect();
            out.insert(
                "reminders".into(),
                json!({"useDefault": false, "overrides": overrides}),
            );
        }
        Some(_) => return Err(bad("alarms: a list of minutes, or null for the defaults")),
    }
    match v.get("free") {
        None => {}
        Some(Value::Bool(f)) => {
            out.insert(
                "transparency".into(),
                json!(if *f { "transparent" } else { "opaque" }),
            );
        }
        Some(_) => return Err(bad("free: true or false")),
    }
    let mut private = Map::new();
    match v.get("travelMin") {
        None => {}
        Some(Value::Null) => {
            private.insert("travelMin".into(), json!(""));
        }
        Some(m) => match m.as_u64().filter(|m| *m <= u64::from(MAX_TRAVEL_MIN)) {
            Some(0) => {
                private.insert("travelMin".into(), json!(""));
            }
            Some(m) => {
                private.insert("travelMin".into(), json!(m.to_string()));
            }
            None => return Err(bad(format!("travelMin: 0-{MAX_TRAVEL_MIN} minutes"))),
        },
    }
    match v.get("leaveAlert") {
        None => {}
        Some(Value::Bool(b)) => {
            private.insert("leaveAlert".into(), json!(if *b { "1" } else { "" }));
        }
        Some(_) => return Err(bad("leaveAlert: true or false")),
    }
    if !private.is_empty() {
        out.insert("extendedProperties".into(), json!({"private": private}));
    }
    Ok(())
}

/// `patch`'s private extended properties merged into `cur`'s (an empty
/// value removes the key): Google and Lucina's store both keep the rest.
fn merge_private(cur: Option<&Value>, patch: &Value) -> Value {
    let mut private = cur
        .and_then(|c| c.pointer("/extendedProperties/private"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(p) = patch
        .pointer("/extendedProperties/private")
        .and_then(Value::as_object)
    {
        for (k, v) in p {
            if v == &json!("") {
                private.remove(k);
            } else {
                private.insert(k.clone(), v.clone());
            }
        }
    }
    json!({"private": private})
}

fn as_object(v: Value) -> Result<Map<String, Value>, ApiError> {
    match v {
        Value::Object(m) => Ok(m),
        _ => Err(bad("expected a JSON object")),
    }
}

/// A created/updated event (Google's shape, from Google or Lucina's own
/// store) as an [`Event`].
fn event_of(v: &Value, tz: Tz, source: Source) -> Option<Event> {
    let item = gcal::to_item(v, tz.name())?;
    let start = item.start.as_ref()?.instant(tz);
    let w = Window {
        from: start - chrono::Duration::days(1),
        to: start + chrono::Duration::days(400),
    };
    let items = [Item {
        rrule: None,
        ..item
    }];
    secretary_calendar::window(
        &[SourceItems {
            source,
            key: "",
            label: Some(if source == Source::Lucina {
                OWN_LABEL
            } else {
                "Google"
            }),
            items: &items,
        }],
        w,
        tz,
    )
    .into_iter()
    .next()
}

async fn after_write(hub: &Hub) {
    let _g = hub.sync_lock.lock().await;
    if let Err(e) = sync_google(hub).await {
        tracing::warn!(error = %e, "post-write sync failed");
    }
}

/// Who is writing: Lucina (the service token) or the owner's device.
fn added_by(ident: &Identity) -> String {
    if ident.device_id == SERVICE_DEVICE_ID {
        return OWN_LABEL.to_owned();
    }
    let name: String = ident.device_name.trim().chars().take(64).collect();
    if name.is_empty() {
        "Secretary".to_owned()
    } else {
        name
    }
}

fn set_added_by(ev: &mut Map<String, Value>, who: &str) {
    let patch = json!({"extendedProperties": {"private": {"addedBy": who}}});
    let mut cur = Value::Object(std::mem::take(ev));
    let merged = merge_private(Some(&cur), &patch);
    if let Value::Object(m) = &mut cur {
        m.insert("extendedProperties".into(), merged);
        *ev = std::mem::take(m);
    }
}

/// `conferenceUrl` of a request: `Some("")` clears it.
fn conference_field(v: &Map<String, Value>) -> Result<Option<String>, ApiError> {
    match v.get("conferenceUrl") {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(String::new())),
        Some(Value::String(u)) => {
            let u = u.trim();
            if u.is_empty() {
                Ok(Some(String::new()))
            } else if u.starts_with("https://")
                && u.len() <= 500
                && !u.contains(char::is_whitespace)
            {
                Ok(Some(u.to_owned()))
            } else {
                Err(bad("conferenceUrl: an https link, at most 500 characters"))
            }
        }
        _ => Err(bad("conferenceUrl: an https link, at most 500 characters")),
    }
}

fn event_not_found() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "event_not_found", "no such event")
}

/// Lucina's own timed events of the next weeks feed "Coming up"
/// notifications, like the phone's.
pub async fn refresh_own_reminders(st: &AppState) {
    let now = Utc::now();
    let events = events_between(
        &st.links,
        now - chrono::Duration::hours(1),
        now + chrono::Duration::days(OWN_REMINDER_DAYS),
        Some(&[Source::Lucina]),
    )
    .await;
    let upcoming = events
        .into_iter()
        .filter(|e| !e.all_day)
        .take(200)
        .map(|e| crate::notify::store::CalendarEvent {
            alerts: (!e.alarms_default)
                .then(|| e.alarms.iter().map(|m| e.start_ms - m * 60_000).collect()),
            leave_at: e
                .travel_min
                .filter(|_| e.leave_alert)
                .map(|m| e.start_ms - i64::from(m) * 60_000),
            id: e.id,
            title: e.title.chars().take(120).collect(),
            start: e.start_ms,
        })
        .collect();
    if let Err(e) = st.notify.own_events(upcoming) {
        tracing::warn!(error = %e, "could not keep Lucina's calendar events for notifications");
    }
}

/// A new event in Lucina's own calendar.
async fn create_own(
    st: &AppState,
    body: Map<String, Value>,
    who: &str,
) -> Result<Value, ApiError> {
    let hub = &st.links;
    let mut ev = google_body(&body, hub.tz, false)?;
    if let Some(u) = conference_field(&body)?.filter(|u| !u.is_empty()) {
        ev.insert("hangoutLink".into(), json!(u));
    }
    set_added_by(&mut ev, who);
    let id = random_id("ev_");
    let now = now_iso();
    ev.insert("id".into(), json!(id));
    ev.insert("created".into(), json!(now));
    ev.insert("updated".into(), json!(now));
    let v = Value::Object(ev);
    {
        let mut d = hub.data.lock().await;
        if d.own.events.len() >= MAX_OWN_EVENTS {
            return Err(bad(format!("at most {MAX_OWN_EVENTS} events")));
        }
        d.own.events.insert(id.clone(), v.clone());
        if let Err(e) = hub.save_own(&d) {
            d.own.events.remove(&id);
            return Err(e);
        }
    }
    refresh_own_reminders(st).await;
    Ok(json!({"created": true, "event": event_of(&v, hub.tz, Source::Lucina)}))
}

async fn create_on_google(
    hub: &Hub,
    body: Map<String, Value>,
    who: &str,
) -> Result<Value, ApiError> {
    let mut gbody = google_body(&body, hub.tz, false)?;
    set_added_by(&mut gbody, who);
    let created = hub
        .google_api(Method::POST, PRIMARY, &[], Some(&Value::Object(gbody)))
        .await
        .map_err(gerror)?;
    after_write(hub).await;
    Ok(json!({"created": true, "event": event_of(&created, hub.tz, Source::Google)}))
}

async fn create(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    JsonBody(body): JsonBody<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut body = as_object(body)?;
    let who = added_by(&ident);
    let target = body.remove("target");
    let r = match target.as_ref().map(|t| t.as_str()) {
        None | Some(Some("lucina")) => create_own(&st, body, &who).await?,
        Some(Some("google")) => create_on_google(&st.links, body, &who).await?,
        Some(_) => return Err(bad("target: lucina or google")),
    };
    Ok((StatusCode::CREATED, Json(r)))
}

/// Which calendar an event id belongs to.
enum Target<'a> {
    Own(&'a str),
    Google(&'a str),
}

fn target(id: &str) -> Result<Target<'_>, ApiError> {
    match id.split_once(':') {
        Some(("lucina", o)) if !o.is_empty() && o.len() <= 200 => Ok(Target::Own(o)),
        _ => google_id(id).map(Target::Google),
    }
}

/// Moving the start keeps the duration (`cur`: the stored event) unless
/// `end` is given.
fn keep_duration(body: &mut Map<String, Value>, cur: &Value, tz: Tz) {
    if !body.contains_key("start") || body.contains_key("end") {
        return;
    }
    let tz_name = tz.name();
    let (Some(a), Some(b)) = (
        cur.get("start").and_then(|s| gcal::moment(s, tz_name)),
        cur.get("end").and_then(|s| gcal::moment(s, tz_name)),
    ) else {
        return;
    };
    let new_start = body
        .get("start")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if let (Moment::Date(da), Moment::Date(db)) = (&a, &b) {
        if let Ok(ns) = NaiveDate::parse_from_str(&new_start, "%Y-%m-%d") {
            body.insert(
                "end".into(),
                json!((ns + (*db - *da)).format("%Y-%m-%d").to_string()),
            );
        }
    } else if let Ok(ns) = DateTime::parse_from_rfc3339(&new_start) {
        let len = b.instant(tz) - a.instant(tz);
        body.insert("end".into(), json!((ns + len).to_rfc3339()));
    }
}

/// `patch` (a partial [`google_body`]) and the conference link applied to a
/// stored event: an empty location/description removes it.
fn apply_patch(cur: &Value, patch: Map<String, Value>, conference: Option<String>) -> Value {
    let mut ev = cur.as_object().cloned().unwrap_or_default();
    for (k, v) in patch {
        if k == "extendedProperties" {
            let merged = merge_private(Some(cur), &json!({"extendedProperties": v}));
            ev.insert(k, merged);
        } else if v == json!("") {
            ev.remove(&k);
        } else {
            ev.insert(k, v);
        }
    }
    match conference {
        Some(u) if !u.is_empty() => {
            ev.insert("hangoutLink".into(), json!(u));
        }
        Some(_) => {
            ev.remove("hangoutLink");
        }
        None => {}
    }
    ev.insert("updated".into(), json!(now_iso()));
    Value::Object(ev)
}

async fn update_own(
    st: &AppState,
    oid: &str,
    mut body: Map<String, Value>,
) -> Result<Json<Value>, ApiError> {
    let hub = &st.links;
    let conference = conference_field(&body)?;
    body.remove("conferenceUrl");
    let saved = {
        let mut d = hub.data.lock().await;
        let cur = d.own.events.get(oid).cloned().ok_or_else(event_not_found)?;
        keep_duration(&mut body, &cur, hub.tz);
        let patch = if body.is_empty() {
            Map::new()
        } else {
            google_body(&body, hub.tz, true)?
        };
        if patch.is_empty() && conference.is_none() {
            return Err(bad("nothing to change"));
        }
        let next = apply_patch(&cur, patch, conference);
        d.own.events.insert(oid.to_owned(), next.clone());
        if let Err(e) = hub.save_own(&d) {
            d.own.events.insert(oid.to_owned(), cur);
            return Err(e);
        }
        next
    };
    refresh_own_reminders(st).await;
    Ok(Json(
        json!({"updated": true, "event": event_of(&saved, hub.tz, Source::Lucina)}),
    ))
}

fn google_id(id: &str) -> Result<&str, ApiError> {
    match id.split_once(':') {
        Some(("google", g)) if !g.is_empty() && g.len() <= 1024 => Ok(g),
        Some(("ics" | "iphone", _)) => Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "read_only_source",
            "only Lucina's and Google Calendar events can be changed here (ICS feeds are read-only; iPhone events change on the phone)",
        )),
        _ => Err(ApiError::new(StatusCode::NOT_FOUND, "event_not_found", "no such event")),
    }
}

fn need_confirm(ok: bool) -> Result<(), ApiError> {
    if ok {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::PRECONDITION_REQUIRED,
            "confirmation_required",
            "ask the owner first, then send confirm: true",
        ))
    }
}

async fn update(
    State(st): State<AppState>,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<Value>,
) -> Result<Json<Value>, ApiError> {
    let hub = &st.links;
    let t = target(&id)?;
    let mut body = as_object(body)?;
    need_confirm(body.remove("confirm").and_then(|v| v.as_bool()) == Some(true))?;
    let gid = match t {
        Target::Own(oid) => return update_own(&st, oid, body).await,
        Target::Google(gid) => gid,
    };
    let needs_cur = (body.contains_key("start") && !body.contains_key("end"))
        || body.contains_key("travelMin")
        || body.contains_key("leaveAlert");
    let cur = if needs_cur {
        Some(
            hub.google_api(Method::GET, &format!("{PRIMARY}/{}", seg(gid)), &[], None)
                .await
                .map_err(gerror)?,
        )
    } else {
        None
    };
    if let Some(cur) = &cur {
        keep_duration(&mut body, cur, hub.tz);
    }
    let mut gbody = google_body(&body, hub.tz, true)?;
    if gbody.contains_key("extendedProperties") {
        let patch = Value::Object(gbody.clone());
        gbody.insert(
            "extendedProperties".into(),
            merge_private(cur.as_ref(), &patch),
        );
    }
    let updated = hub
        .google_api(
            Method::PATCH,
            &format!("{PRIMARY}/{}", seg(gid)),
            &[],
            Some(&Value::Object(gbody)),
        )
        .await
        .map_err(gerror)?;
    after_write(hub).await;
    Ok(Json(
        json!({"updated": true, "event": event_of(&updated, hub.tz, Source::Google)}),
    ))
}

/// What a moved event keeps: its text, times, repeat rule, settings and
/// private properties; the video link goes into the notes when the target
/// cannot hold it (Google takes no plain link).
fn movable_body(cur: &Value, to_google: bool) -> Map<String, Value> {
    let mut out = Map::new();
    for k in [
        "summary",
        "location",
        "description",
        "start",
        "end",
        "recurrence",
        "reminders",
        "transparency",
        "extendedProperties",
    ] {
        if let Some(v) = cur.get(k).filter(|v| !v.is_null()) {
            out.insert(k.into(), v.clone());
        }
    }
    let link = cur
        .get("hangoutLink")
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("https://"));
    if let Some(u) = link {
        if to_google {
            let notes = out.get("description").and_then(Value::as_str).unwrap_or("");
            if !notes.contains(u) {
                let joined = if notes.trim().is_empty() {
                    u.to_owned()
                } else {
                    format!("{}\n\n{u}", notes.trim_end())
                };
                out.insert("description".into(), json!(joined));
            }
        } else {
            out.insert("hangoutLink".into(), json!(u));
        }
    }
    out
}

/// `POST /v1/calendar/events/{id}/move {target: "lucina"|"google", confirm: true}`:
/// the event is created in the other calendar, then removed from its own.
/// Instances of repeating events stay where they are.
async fn move_event(
    State(st): State<AppState>,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<Value>,
) -> Result<Json<Value>, ApiError> {
    let hub = &st.links;
    let body = as_object(body)?;
    need_confirm(body.get("confirm").and_then(Value::as_bool) == Some(true))?;
    let to_google = match body.get("target").and_then(Value::as_str) {
        Some("google") => true,
        Some("lucina") => false,
        _ => return Err(bad("target: lucina or google")),
    };
    let recurring = || {
        ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "recurring_event",
            "a repeating event can't be moved to another calendar here",
        )
    };
    match (target(&id)?, to_google) {
        (Target::Own(_), false) | (Target::Google(_), true) => {
            Err(bad("the event is already in that calendar"))
        }
        (Target::Own(oid), true) => {
            let cur = hub
                .data
                .lock()
                .await
                .own
                .events
                .get(oid)
                .cloned()
                .ok_or_else(event_not_found)?;
            if cur.get("recurrence").is_some() || cur.get("recurringEventId").is_some() {
                return Err(recurring());
            }
            let created = hub
                .google_api(
                    Method::POST,
                    PRIMARY,
                    &[],
                    Some(&Value::Object(movable_body(&cur, true))),
                )
                .await
                .map_err(gerror)?;
            {
                let mut d = hub.data.lock().await;
                if let Some(old) = d.own.events.remove(oid) {
                    if let Err(e) = hub.save_own(&d) {
                        d.own.events.insert(oid.to_owned(), old);
                        return Err(e);
                    }
                }
            }
            refresh_own_reminders(&st).await;
            after_write(hub).await;
            Ok(Json(
                json!({"moved": true, "from": id, "event": event_of(&created, hub.tz, Source::Google)}),
            ))
        }
        (Target::Google(gid), false) => {
            let path = format!("{PRIMARY}/{}", seg(gid));
            let cur = hub
                .google_api(Method::GET, &path, &[], None)
                .await
                .map_err(gerror)?;
            if cur.get("recurrence").is_some() || cur.get("recurringEventId").is_some() {
                return Err(recurring());
            }
            let mut ev = movable_body(&cur, false);
            if let Some(u) = gcal::to_item(&cur, hub.tz.name()).and_then(|i| i.conference_url) {
                ev.entry("hangoutLink").or_insert(json!(u));
            }
            let oid = random_id("ev_");
            let now = now_iso();
            ev.insert("id".into(), json!(oid));
            ev.insert("created".into(), json!(now));
            ev.insert("updated".into(), json!(now));
            let v = Value::Object(ev);
            {
                let mut d = hub.data.lock().await;
                if d.own.events.len() >= MAX_OWN_EVENTS {
                    return Err(bad(format!("at most {MAX_OWN_EVENTS} events")));
                }
                d.own.events.insert(oid.clone(), v.clone());
                if let Err(e) = hub.save_own(&d) {
                    d.own.events.remove(&oid);
                    return Err(e);
                }
            }
            match hub.google_api(Method::DELETE, &path, &[], None).await {
                Ok(_) | Err(GError::Gone) => {}
                Err(e) => {
                    // Keep one copy: drop the new one again.
                    let mut d = hub.data.lock().await;
                    d.own.events.remove(&oid);
                    let _ = hub.save_own(&d);
                    return Err(gerror(e));
                }
            }
            refresh_own_reminders(&st).await;
            after_write(hub).await;
            Ok(Json(
                json!({"moved": true, "from": id, "event": event_of(&v, hub.tz, Source::Lucina)}),
            ))
        }
    }
}

#[derive(Deserialize, Default)]
struct ConfirmQuery {
    confirm: Option<String>,
}

async fn remove(
    State(st): State<AppState>,
    Path(id): Path<String>,
    QueryArgs(q): QueryArgs<ConfirmQuery>,
) -> Result<Json<Value>, ApiError> {
    let hub = &st.links;
    let t = target(&id)?;
    need_confirm(matches!(q.confirm.as_deref(), Some("true" | "1")))?;
    let gid = match t {
        Target::Own(oid) => {
            {
                let mut d = hub.data.lock().await;
                // Already gone counts as deleted, like Google's 410.
                if let Some(old) = d.own.events.remove(oid) {
                    if let Err(e) = hub.save_own(&d) {
                        d.own.events.insert(oid.to_owned(), old);
                        return Err(e);
                    }
                }
            }
            refresh_own_reminders(&st).await;
            return Ok(Json(json!({"deleted": true, "id": id})));
        }
        Target::Google(gid) => gid,
    };
    match hub
        .google_api(
            Method::DELETE,
            &format!("{PRIMARY}/{}", seg(gid)),
            &[],
            None,
        )
        .await
    {
        Ok(_) => {}
        // Already gone (410 for an event deleted before).
        Err(GError::Gone) => {}
        Err(e) => return Err(gerror(e)),
    }
    after_write(hub).await;
    Ok(Json(json!({"deleted": true, "id": id})))
}

// -- proposals -------------------------------------------------------------------

async fn proposals(State(st): State<AppState>) -> Json<Value> {
    let now = Utc::now().timestamp_millis();
    let d = st.links.data.lock().await;
    let list: Vec<&Proposal> = d
        .cache
        .proposals
        .iter()
        .filter(|p| p.expires_at_ms > now)
        .collect();
    Json(json!({"proposals": list}))
}

async fn propose(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    JsonBody(body): JsonBody<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let hub = &st.links;
    let mut body = as_object(body)?;
    let from = match body
        .remove("from")
        .and_then(|v| v.as_str().map(str::to_owned))
    {
        Some(f) => text_field("from", &f, 64)?,
        None => ident.device_name.clone(),
    };
    let reason = match body
        .remove("reason")
        .and_then(|v| v.as_str().map(str::to_owned))
    {
        Some(r) => Some(text_field("reason", &r, 300)?),
        None => None,
    };
    // Validate now so the owner only sees proposals that would work.
    google_body(&body, hub.tz, false)?;
    let keep: Map<String, Value> = body
        .into_iter()
        .filter(|(k, _)| {
            [
                "title",
                "start",
                "end",
                "allDay",
                "location",
                "description",
                "timeZone",
            ]
            .contains(&k.as_str())
        })
        .collect();
    let now = Utc::now().timestamp_millis();
    let p = Proposal {
        id: random_id("prop_"),
        event: Value::Object(keep),
        from,
        reason,
        created_at: now_iso(),
        expires_at_ms: now + PROPOSAL_TTL_MS,
    };
    let mut d = hub.data.lock().await;
    d.cache.proposals.retain(|x| x.expires_at_ms > now);
    if d.cache.proposals.len() >= MAX_PROPOSALS {
        return Err(bad(format!("at most {MAX_PROPOSALS} pending proposals")));
    }
    d.cache.proposals.push(p.clone());
    hub.save_cache(&d);
    Ok((StatusCode::CREATED, Json(json!(p))))
}

async fn accept(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if ident.device_id == SERVICE_DEVICE_ID {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "owner_only",
            "the owner accepts proposals in the app",
        ));
    }
    need_confirm(body.get("confirm").and_then(Value::as_bool) == Some(true))?;
    let hub = &st.links;
    let p = {
        let d = hub.data.lock().await;
        d.cache.proposals.iter().find(|p| p.id == id).cloned()
    }
    .ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "proposal_not_found",
            "no such proposal",
        )
    })?;
    let r = create_own(&st, as_object(p.event)?, &p.from).await?;
    let mut d = hub.data.lock().await;
    d.cache.proposals.retain(|x| x.id != id);
    hub.save_cache(&d);
    Ok((StatusCode::CREATED, Json(r)))
}

async fn dismiss(
    State(st): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let hub = &st.links;
    let mut d = hub.data.lock().await;
    let before = d.cache.proposals.len();
    d.cache.proposals.retain(|x| x.id != id);
    if d.cache.proposals.len() == before {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "proposal_not_found",
            "no such proposal",
        ));
    }
    hub.save_cache(&d);
    Ok(Json(json!({"dismissed": true})))
}

/// `/v1/calendar/*` (device token, cookie, or Lucina's service token).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/calendar/events", get(list_events).post(create))
        .route(
            "/v1/calendar/events/{id}",
            axum::routing::patch(update).delete(remove),
        )
        .route("/v1/calendar/events/{id}/move", post(move_event))
        .route("/v1/calendar/free", get(free))
        .route("/v1/calendar/sync", post(sync_now))
        .route("/v1/calendar/proposals", get(proposals).post(propose))
        .route("/v1/calendar/proposals/{id}/accept", post(accept))
        .route(
            "/v1/calendar/proposals/{id}",
            axum::routing::delete(dismiss),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn google_bodies() {
        let tz = chrono_tz::Europe::Rome;
        let m = |v: Value| as_object(v).unwrap();
        let b = google_body(
            &m(json!({"title": " Dentist ", "start": "2026-10-23T09:30:00+02:00"})),
            tz,
            false,
        )
        .unwrap();
        assert_eq!(b["summary"], "Dentist");
        assert_eq!(
            b["start"],
            json!({"dateTime": "2026-10-23T09:30:00+02:00", "timeZone": "Europe/Rome"})
        );
        assert_eq!(b["end"]["dateTime"], "2026-10-23T10:30:00+02:00");
        let d = google_body(
            &m(json!({"title": "Trip", "start": "2026-11-01", "end": "2026-11-04"})),
            tz,
            false,
        )
        .unwrap();
        assert_eq!(d["start"], json!({"date": "2026-11-01"}));
        assert!(google_body(&m(json!({"start": "2026-11-01"})), tz, false).is_err());
        assert!(google_body(
            &m(json!({"title": "x", "start": "2026-10-23T09:30:00"})),
            tz,
            false
        )
        .is_err());
        assert!(google_body(&m(json!({"title": "x", "start": "2026-10-23T09:30:00Z", "end": "2026-10-23T08:00:00Z"})), tz, false).is_err());
        let p = google_body(&m(json!({"location": null})), tz, true).unwrap();
        assert_eq!(p["location"], "");
        assert!(google_body(&m(json!({})), tz, true).is_err());
        assert!(google_id("ics:abc:def").is_err());
        assert_eq!(
            google_id("google:abc_20261022T070000Z").unwrap(),
            "abc_20261022T070000Z"
        );
    }
    #[test]
    fn event_settings() {
        let tz = chrono_tz::Europe::Copenhagen;
        let m = |v: Value| as_object(v).unwrap();
        let b = google_body(
            &m(json!({"alarms": [1440, 60, 60], "free": true, "travelMin": 25, "leaveAlert": true})),
            tz,
            true,
        )
        .unwrap();
        assert_eq!(
            b["reminders"],
            json!({"useDefault": false, "overrides": [
                {"method": "popup", "minutes": 60}, {"method": "popup", "minutes": 1440}]})
        );
        assert_eq!(b["transparency"], "transparent");
        assert_eq!(
            b["extendedProperties"],
            json!({"private": {"travelMin": "25", "leaveAlert": "1"}})
        );
        let d = google_body(
            &m(json!({"alarms": null, "travelMin": 0, "leaveAlert": false})),
            tz,
            true,
        )
        .unwrap();
        assert_eq!(d["reminders"], json!({"useDefault": true}));
        for bad_body in [
            json!({"alarms": [-1]}),
            json!({"alarms": [1, 2, 3, 4, 5, 6]}),
            json!({"alarms": "15"}),
            json!({"free": "yes"}),
            json!({"travelMin": 5000}),
        ] {
            assert!(google_body(&m(bad_body), tz, true).is_err());
        }

        // Lucina's store: the patch keeps addedBy and clears what is "".
        let cur = json!({"id": "ev_1", "summary": "x",
            "extendedProperties": {"private": {"addedBy": "Lucina", "travelMin": "25", "leaveAlert": "1"}}});
        let next = apply_patch(&cur, d, None);
        assert_eq!(
            next["extendedProperties"],
            json!({"private": {"addedBy": "Lucina"}})
        );
        assert_eq!(next["reminders"], json!({"useDefault": true}));
        let item = gcal::to_item(&next, "UTC").unwrap();
        assert!(item.alarms_default && item.travel_min.is_none() && !item.leave_alert);

        let mut created = google_body(
            &m(json!({"title": "y", "start": "2026-11-02", "travelMin": 15})),
            tz,
            false,
        )
        .unwrap();
        set_added_by(&mut created, "iPhone");
        assert_eq!(
            created["extendedProperties"],
            json!({"private": {"travelMin": "15", "addedBy": "iPhone"}})
        );
    }

    #[test]
    fn moved_events_keep_their_settings() {
        let cur = json!({"id": "ev_1", "created": "t", "summary": "Kitchen", "description": "Bring ID",
            "start": {"dateTime": "2026-11-02T14:00:00+01:00"}, "end": {"dateTime": "2026-11-02T15:30:00+01:00"},
            "reminders": {"useDefault": false, "overrides": [{"method": "popup", "minutes": 60}]},
            "transparency": "transparent", "hangoutLink": "https://teams.microsoft.com/l/meetup-join/x"});
        let g = movable_body(&cur, true);
        assert!(
            g.get("id").is_none() && g.get("created").is_none() && g.get("hangoutLink").is_none()
        );
        assert_eq!(
            g["description"],
            "Bring ID\n\nhttps://teams.microsoft.com/l/meetup-join/x"
        );
        assert_eq!(g["reminders"], cur["reminders"]);
        let own = movable_body(&cur, false);
        assert_eq!(own["hangoutLink"], cur["hangoutLink"]);
        assert_eq!(own["description"], "Bring ID");
    }
}
