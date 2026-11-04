//! Device bridge (`docs/device-bridge.md`): the owner's phone and Mac report
//! events and state to Lucina, and ack the commands Lucina sends them.
//!
//! - `POST /v1/device/events {events}`: validated by `secretary-core`
//!   (whitelisted `type`/`source`, 1..=500 events, `data` ≤ 8 KiB). Event ids
//!   already forwarded for this device (in-memory LRU of [`SEEN_IDS`]) and
//!   repeats inside the batch are dropped; the rest go to Lucina's
//!   `POST /api/secretary/device/events` in chunks of at most
//!   [`LUCINA_CHUNK_BYTES`] (Lucina caps request bodies at 64 KiB), one
//!   after the other. Ids are remembered per chunk Lucina accepted, so after
//!   a failure the client retries the same batch and only the rest is
//!   sent. Own rate
//!   limit bucket ([`crate::auth::require_device_events`]) and a body limit
//!   of [`EVENTS_BODY_LIMIT`].
//! - `POST /v1/device/commands/{id}/result {ok, error?, deviceObjectId?}`
//!   and `POST /v1/device/state {alarms, timers, reminders, events, focus}`
//!   are validated and forwarded.
//! - `GET /v1/device/juno/next?wait=<s>` long-polls Lucina for Juno's next
//!   call (`wait` clamped to 0..=25) and `POST /v1/device/juno/calls/{id}/result`
//!   posts its answer (body limit [`JUNO_RESULT_BODY_LIMIT`]).
//! - `GET /v1/juno/screenshots/{id}.png`: a screenshot Juno sent, as the image
//!   block of Lucina's reply shows it (`image/png`, Lucina keeps it an hour).
//! - `POST /v1/device/juno/events {events: [≤ 64], lost}` (body limit
//!   [`JUNO_EVENTS_BODY_LIMIT`]): the laptop's semantic events, each checked
//!   by [`juno_event`] (a bad one is dropped and counted in `lost`), forwarded
//!   to Lucina with the device stamped from the token.
//! - The `juno` routes but the screenshots take only a device whose registered
//!   name starts with "Juno" (`Juno (Galaxy Book Go)`): others get 403 `not_juno`.
//!
//! `POST /v1/device/state` also hands the snapshot's timed events to the
//! notifier for calendar notifications.
//!
//! Every forwarded body gets `device: {id, name}` (the authenticated
//! device); a client-sent `device` is ignored.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use secretary_core::models::{CommandResult, DeviceEvent, DeviceEvents, DeviceRef, DeviceState};
use secretary_core::Call;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::auth::Identity;
use crate::error::{ApiError, JsonBody};
use crate::AppState;

/// Event ids remembered for dedup (across all devices).
pub const SEEN_IDS: usize = 8192;
/// Body limit of `POST /v1/device/events`: 500 events with 8 KiB `data`
/// each, plus their envelopes.
pub const EVENTS_BODY_LIMIT: usize = 4608 * 1024;

/// Max JSON size of the events in one forward to Lucina.
pub const LUCINA_CHUNK_BYTES: usize = 48 * 1024;

/// Body limit of `POST /v1/device/juno/calls/{id}/result`: a PNG screenshot in base64.
pub const JUNO_RESULT_BODY_LIMIT: usize = 12 * 1024 * 1024;

/// Body limit of `POST /v1/device/juno/events`.
pub const JUNO_EVENTS_BODY_LIMIT: usize = 256 * 1024;

/// Most events in one `POST /v1/device/juno/events`.
pub const JUNO_EVENTS_MAX: usize = 64;

/// Largest `fields` of one Juno event, as JSON.
const JUNO_EVENT_FIELDS_MAX: usize = 4096;

/// Longest `wait` of `GET /v1/device/juno/next`.
const JUNO_WAIT_MAX: u64 = 25;

/// Split `events` into runs whose JSON stays under `max` bytes (an event
/// larger than `max` gets a chunk of its own).
fn chunks(events: Vec<DeviceEvent>, max: usize) -> Vec<Vec<DeviceEvent>> {
    let mut out: Vec<Vec<DeviceEvent>> = Vec::new();
    let mut size = 0;
    for ev in events {
        let len = serde_json::to_vec(&ev).map_or(max, |b| b.len()) + 1;
        match out.last_mut() {
            Some(cur) if size + len <= max => {
                size += len;
                cur.push(ev);
            }
            _ => {
                size = len;
                out.push(vec![ev]);
            }
        }
    }
    out
}

/// A small LRU set of keys.
pub struct SeenIds {
    cap: usize,
    inner: Mutex<Lru>,
}

#[derive(Default)]
struct Lru {
    stamp_of: HashMap<String, u64>,
    by_stamp: BTreeMap<u64, String>,
    next: u64,
}

impl Lru {
    fn touch(&mut self, key: &str) {
        let stamp = self.next;
        self.next += 1;
        if let Some(old) = self.stamp_of.insert(key.to_owned(), stamp) {
            self.by_stamp.remove(&old);
        }
        self.by_stamp.insert(stamp, key.to_owned());
    }
}

impl SeenIds {
    pub fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            inner: Mutex::default(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Lru> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Whether `key` was seen (and marks it most recently used if so).
    pub fn contains(&self, key: &str) -> bool {
        let mut l = self.lock();
        let hit = l.stamp_of.contains_key(key);
        if hit {
            l.touch(key);
        }
        hit
    }

    /// Remember `key`, evicting the least recently used beyond the capacity.
    pub fn insert(&self, key: &str) {
        let mut l = self.lock();
        l.touch(key);
        while l.stamp_of.len() > self.cap {
            let Some((_, oldest)) = l.by_stamp.pop_first() else {
                break;
            };
            l.stamp_of.remove(&oldest);
        }
    }

    pub fn len(&self) -> usize {
        self.lock().stamp_of.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn seen_key(device: &str, event: &str) -> String {
    format!("{device}\u{1f}{event}")
}

fn device_ref(ident: &Identity) -> DeviceRef {
    DeviceRef {
        id: ident.device_id.clone(),
        name: ident.device_name.clone(),
    }
}

async fn forward(st: &AppState, call: &Call) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (status, data) = st.lucina()?.call_with_status::<Value>(call).await?;
    Ok((
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(data),
    ))
}

/// `200 {accepted, duplicates, results}`: how many events went to Lucina,
/// the ids dropped as already sent, and Lucina's `data` for each chunk
/// (empty when nothing was forwarded). A Lucina failure on any chunk is the
/// route's error; the chunks before it stay accepted.
async fn events(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    JsonBody(mut batch): JsonBody<DeviceEvents>,
) -> Result<Json<Value>, ApiError> {
    // The whole batch must be valid, duplicates included.
    Call::device_events(&batch)?;
    let mut duplicates = Vec::new();
    let mut in_batch = HashSet::new();
    batch.events.retain(|e| {
        let fresh = in_batch.insert(e.id.clone())
            && !st.event_ids.contains(&seen_key(&ident.device_id, &e.id));
        if !fresh {
            duplicates.push(e.id.clone());
        }
        fresh
    });
    let mut accepted = 0;
    let mut results = Vec::new();
    for chunk in chunks(std::mem::take(&mut batch.events), LUCINA_CHUNK_BYTES) {
        let part = DeviceEvents {
            events: chunk,
            device: Some(device_ref(&ident)),
        };
        let call = Call::device_events(&part)?;
        results.push(st.lucina()?.call::<Value>(&call).await?);
        for e in &part.events {
            st.event_ids.insert(&seen_key(&ident.device_id, &e.id));
        }
        accepted += part.events.len();
    }
    Ok(Json(json!({
        "accepted": accepted,
        "duplicates": duplicates,
        "results": results,
    })))
}

async fn command_result(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    Path(id): Path<String>,
    JsonBody(mut res): JsonBody<CommandResult>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    res.device = Some(device_ref(&ident));
    forward(&st, &Call::command_result(&id, &res)?).await
}

/// What Lucina gets of a snapshot: the calendar keeps an event's notes and link for the Calendar tab; they don't go on
/// to her.
fn for_lucina(snapshot: &DeviceState) -> DeviceState {
    let mut copy = snapshot.clone();
    for e in &mut copy.events {
        if let Some(o) = e.as_object_mut() {
            o.remove("notes");
            o.remove("url");
        }
    }
    copy
}

async fn state(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    JsonBody(mut snapshot): JsonBody<DeviceState>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    snapshot.device = Some(device_ref(&ident));
    let call = Call::device_state(&for_lucina(&snapshot))?;
    // Upcoming events feed calendar notifications, whatever Lucina answers.
    let (notify, device, snap) = (st.notify.clone(), ident.device_id.clone(), snapshot.clone());
    match tokio::task::spawn_blocking(move || notify.device_state(&device, &snap)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "could not keep calendar events"),
        Err(_) => tracing::warn!("calendar events task failed"),
    }
    // The calendar keeps the phone's events (Links, /v1/calendar/events),
    // whether or not Lucina is reachable.
    crate::calendar::ingest_device_events(&st, &ident.device_name, &snapshot.events).await;
    forward(&st, &call).await
}

/// `200 {"call": null | {id, tool, arguments, deadlineMs}}`: Lucina's data
/// for Juno's next call, passed through unchanged.
async fn juno_next(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    only_juno(&ident)?;
    let wait = match q.get("wait") {
        None => JUNO_WAIT_MAX,
        Some(w) => w
            .parse::<u64>()
            .map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "wait must be a number of seconds 0..=25",
                )
            })?
            .min(JUNO_WAIT_MAX),
    };
    let call = Call::juno_next(&device_ref(&ident), wait)?;
    let data = st
        .lucina()?
        .call_with_timeout::<Value>(&call, Duration::from_secs(wait + 10))
        .await?;
    Ok((StatusCode::OK, Json(data)))
}

/// Juno's answer to one call; extra fields (a client-sent `device`) are ignored.
#[derive(Deserialize)]
struct JunoResultBody {
    ok: bool,
    result: Option<Value>,
    error: Option<String>,
}

async fn juno_result(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<JunoResultBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    only_juno(&ident)?;
    let call = Call::juno_result(&device_ref(&ident), &id, body.ok, body.result, body.error)?;
    forward(&st, &call).await
}

/// A kept Juno screenshot, relayed as is.
async fn juno_screenshot(
    State(st): State<AppState>,
    Path(file): Path<String>,
) -> Result<Response, ApiError> {
    let call = Call::juno_screenshot(&file)?;
    let (content_type, body) = st.lucina()?.bytes(&call).await?;
    if !content_type.starts_with("image/png") {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "lucina_error",
            "Lucina sent a screenshot that is not a PNG",
        ));
    }
    Ok((
        [
            (header::CONTENT_TYPE, "image/png".to_owned()),
            (header::CACHE_CONTROL, "private, max-age=3600".to_owned()),
        ],
        body,
    )
        .into_response())
}

/// 403 `not_juno` unless the token's device is a Juno laptop.
fn only_juno(ident: &Identity) -> Result<(), ApiError> {
    if ident.device_name.starts_with("Juno") {
        return Ok(());
    }
    Err(ApiError::new(
        StatusCode::FORBIDDEN,
        "not_juno",
        "Only a Juno device may use the Juno routes",
    ))
}

/// `POST /v1/device/juno/events {events, lost}`.
async fn juno_events(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    JsonBody(body): JsonBody<Value>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    only_juno(&ident)?;
    let bad = |m: &str| ApiError::new(StatusCode::BAD_REQUEST, "bad_request", m);
    let events = body["events"]
        .as_array()
        .ok_or_else(|| bad("events must be an array"))?;
    if events.len() > JUNO_EVENTS_MAX {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "too_many_events",
            "at most 64 events per batch",
        ));
    }
    let lost = match &body["lost"] {
        Value::Null => 0,
        v => v
            .as_u64()
            .ok_or_else(|| bad("lost must be a whole number"))?,
    };
    let kept: Vec<Value> = events.iter().filter_map(juno_event).collect();
    let dropped = (events.len() - kept.len()) as u64;
    let call = Call::juno_events(&device_ref(&ident), kept, lost.saturating_add(dropped))?;
    forward(&st, &call).await
}

/// One laptop event as Lucina takes it, or `None` (dropped): `{seq, src,
/// src_seq, t_ms, kind, app?, subject?, fields}` with `src` ≤ 64 chars,
/// `t_ms` ≥ 0, `kind` 1-64 of `[a-z0-9._-]`, `app` ≤ 64, `subject` ≤ 200 and
/// `fields` an object of at most 4096 bytes of JSON. Other keys are dropped.
pub fn juno_event(v: &Value) -> Option<Value> {
    let text = |k: &str, max: usize| -> Option<Option<&str>> {
        match &v[k] {
            Value::Null => Some(None),
            Value::String(t) if t.chars().count() <= max => Some(Some(t.as_str())),
            _ => None,
        }
    };
    let seq = v["seq"].as_u64()?;
    let src = text("src", 64)??;
    let src_seq = v["src_seq"].as_u64()?;
    let t_ms = v["t_ms"].as_i64().filter(|t| *t >= 0)?;
    let kind = v["kind"].as_str().filter(|k| {
        (1..=64).contains(&k.len())
            && k.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
            })
    })?;
    let app = text("app", 64)?;
    let subject = text("subject", 200)?;
    let fields = match &v["fields"] {
        Value::Null => json!({}),
        f @ Value::Object(_) if f.to_string().len() <= JUNO_EVENT_FIELDS_MAX => f.clone(),
        _ => return None,
    };
    let mut out = json!({"seq": seq, "src": src, "src_seq": src_seq, "t_ms": t_ms, "kind": kind, "fields": fields});
    if let Some(a) = app {
        out["app"] = json!(a);
    }
    if let Some(s) = subject {
        out["subject"] = json!(s);
    }
    Some(out)
}

/// `POST /v1/device/events`, behind [`crate::auth::require_device_events`].
pub fn events_routes() -> Router<AppState> {
    Router::new().route(
        "/v1/device/events",
        post(events).layer(DefaultBodyLimit::max(EVENTS_BODY_LIMIT)),
    )
}

/// The other device routes, behind `require_device`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/device/commands/{id}/result", post(command_result))
        .route("/v1/device/state", post(state))
        .route("/v1/device/juno/next", get(juno_next))
        .route(
            "/v1/device/juno/calls/{id}/result",
            post(juno_result).layer(DefaultBodyLimit::max(JUNO_RESULT_BODY_LIMIT)),
        )
        .route(
            "/v1/device/juno/events",
            post(juno_events).layer(DefaultBodyLimit::max(JUNO_EVENTS_BODY_LIMIT)),
        )
        .route("/v1/juno/screenshots/{file}", get(juno_screenshot))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lucina_does_not_get_an_events_notes() {
        let snapshot: DeviceState = serde_json::from_value(json!({"events": [
            {"id": "e1", "title": "Kitchen", "start": "2026-11-02T13:00:00Z", "location": "Boiler",
             "notes": "Door code 1234", "url": "https://x.example/y", "alarms": [60], "free": true}
        ]}))
        .unwrap();
        let e = &for_lucina(&snapshot).events[0];
        assert!(e.get("notes").is_none() && e.get("url").is_none());
        assert_eq!(e["title"], "Kitchen");
        assert_eq!(e["location"], "Boiler");
        assert_eq!(e["alarms"], json!([60]));
        // The calendar's own copy keeps them.
        assert_eq!(snapshot.events[0]["notes"], "Door code 1234");
    }

    #[test]
    fn lru_evicts_least_recently_used() {
        let s = SeenIds::new(3);
        for k in ["a", "b", "c"] {
            s.insert(k);
        }
        assert!(s.contains("a")); // a is now the most recent
        s.insert("d"); // evicts b
        assert!(!s.contains("b"));
        assert!(s.contains("a") && s.contains("c") && s.contains("d"));
        assert_eq!(s.len(), 3);
        s.insert("a"); // re-insert does not grow
        assert_eq!(s.len(), 3);
        assert_ne!(seen_key("dev1", "e"), seen_key("dev2", "e"));
    }

    #[test]
    fn chunks_stay_under_the_size() {
        let ev = |i: usize, n: usize| -> DeviceEvent {
            serde_json::from_value(json!({"id": format!("e{i}"), "ts": 1, "type": "app_open",
                                          "source": "ios", "data": {"x": "y".repeat(n)}}))
            .unwrap()
        };
        let parts = chunks((0..100).map(|i| ev(i, 2_000)).collect(), 10_000);
        assert!(parts.len() >= 20);
        assert_eq!(parts.iter().map(Vec::len).sum::<usize>(), 100);
        for p in &parts {
            assert!(serde_json::to_vec(p).unwrap().len() <= 10_000);
        }
        assert_eq!(parts[0][0].id, "e0");
        assert_eq!(parts.last().unwrap().last().unwrap().id, "e99");
        // oversize events still go out, one per chunk
        assert_eq!(chunks(vec![ev(0, 50), ev(1, 50)], 10).len(), 2);
        assert!(chunks(Vec::new(), 10).is_empty());
    }

    #[test]
    fn juno_event_validates_and_drops_unknown_keys() {
        let good = json!({"seq": 7, "src": "devd", "src_seq": 3, "t_ms": 1700, "kind": "app.focused",
                          "app": "video", "subject": "Video", "fields": {"pid": 12}, "extra": "dropped"});
        let v = juno_event(&good).unwrap();
        assert_eq!(
            v,
            json!({"seq": 7, "src": "devd", "src_seq": 3, "t_ms": 1700, "kind": "app.focused",
                   "app": "video", "subject": "Video", "fields": {"pid": 12}})
        );
        let bare =
            juno_event(&json!({"seq": 1, "src": "s", "src_seq": 1, "t_ms": 0, "kind": "net.up"}))
                .unwrap();
        assert_eq!(bare["fields"], json!({}));
        assert!(bare.get("app").is_none());
        let with = |k: &str, val: Value| {
            let mut e = good.clone();
            e[k] = val;
            juno_event(&e)
        };
        for (k, val) in [
            ("seq", json!(-1)),
            ("seq", json!("1")),
            ("src", json!("x".repeat(65))),
            ("src", Value::Null),
            ("src_seq", json!(1.5)),
            ("t_ms", json!(-5)),
            ("kind", json!("BAD KIND")),
            ("kind", json!("")),
            ("kind", json!("k".repeat(65))),
            ("app", json!("a".repeat(65))),
            ("app", json!(3)),
            ("subject", json!("s".repeat(201))),
            ("fields", json!([1])),
            ("fields", json!({"big": "x".repeat(4096)})),
        ] {
            assert!(with(k, val.clone()).is_none(), "{k} {val}");
        }
        assert!(with("subject", json!("s".repeat(200))).is_some());
        assert!(juno_event(&json!("junk")).is_none());
    }
}
