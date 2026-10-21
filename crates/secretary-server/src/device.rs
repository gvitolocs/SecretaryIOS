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
//!
//! Every forwarded body gets `device: {id, name}` (the authenticated
//! device); a client-sent `device` is ignored.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Extension, Json, Router};
use secretary_core::models::{CommandResult, DeviceEvent, DeviceEvents, DeviceRef, DeviceState};
use secretary_core::Call;
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

async fn state(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    JsonBody(mut snapshot): JsonBody<DeviceState>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    snapshot.device = Some(device_ref(&ident));
    forward(&st, &Call::device_state(&snapshot)?).await
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
