//! The unified conversation timeline: every message Lucina sees or sends,
//! from any channel.
//!
//! - `GET /v1/timeline` → history or incremental arrivals, optionally filtered
//!   by conversation or authenticated device.
//!   Typed through `secretary-core`, unknown fields kept.
//! - `GET /v1/timeline/conversations` → conversation summaries.
//! - `GET /v1/timeline/stream` → Server-Sent Events, relayed byte for byte
//!   from Lucina's `/api/secretary/timeline/stream` without buffering.
//!   `Last-Event-ID` (header, or `?lastEventId=` for a fresh EventSource) is
//!   forwarded so Lucina replays the gap.
//!
//! Stream rules:
//! - at most [`PER_DEVICE`] open streams per device and [`TOTAL`] overall,
//!   else `429 too_many_streams`; a slot is freed when the response body is
//!   dropped (client gone, upstream ended, shutdown);
//! - the device rate limit counts the open request once (it runs in
//!   `require_device`, which never sees the events);
//! - an upstream that ends, fails or idles past `Timeouts::stream_idle`
//!   ends the response cleanly (terminating chunk), so EventSource
//!   reconnects with `Last-Event-ID`;
//! - on SIGTERM every stream ends, so graceful shutdown does not wait on
//!   them.
//!
//! No layer in [`crate::app`] imposes a total request deadline; if one is
//! ever added, exclude `/v1/timeline/stream`. `DefaultBodyLimit` limits
//! request bodies only.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path as UrlPath, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use futures_util::StreamExt;
use secretary_core::models::{TimelineConversations, TimelinePage, TimelineQuery};
use secretary_core::Call;
use serde::Deserialize;

use crate::auth::Identity;
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::AppState;

/// Open streams allowed per device.
pub const PER_DEVICE: usize = 4;
/// Open streams allowed in total.
pub const TOTAL: usize = 20;
/// `Retry-After` on `429 too_many_streams`.
const RETRY_AFTER_SECS: u64 = 5;

#[derive(Default)]
struct Counts {
    total: usize,
    per_device: HashMap<String, usize>,
}

/// Concurrency accounting for open event streams.
pub struct StreamSlots {
    per_device: usize,
    total: usize,
    counts: Arc<Mutex<Counts>>,
}

/// One open stream; frees its slot on drop.
pub struct StreamSlot {
    device: String,
    counts: Arc<Mutex<Counts>>,
}

impl StreamSlots {
    pub fn new(per_device: usize, total: usize) -> Self {
        Self {
            per_device,
            total,
            counts: Arc::default(),
        }
    }

    /// A slot for `device`, or `None` when a limit is reached.
    pub fn try_acquire(&self, device: &str) -> Option<StreamSlot> {
        let mut c = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        let mine = c.per_device.get(device).copied().unwrap_or(0);
        if c.total >= self.total || mine >= self.per_device {
            return None;
        }
        c.total += 1;
        c.per_device.insert(device.to_owned(), mine + 1);
        Some(StreamSlot {
            device: device.to_owned(),
            counts: self.counts.clone(),
        })
    }

    /// `(total, for device)` currently open.
    pub fn open(&self, device: &str) -> (usize, usize) {
        let c = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        (c.total, c.per_device.get(device).copied().unwrap_or(0))
    }
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut c = self.counts.lock().unwrap_or_else(|p| p.into_inner());
        c.total = c.total.saturating_sub(1);
        if let Some(n) = c.per_device.get_mut(&self.device) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                c.per_device.remove(&self.device);
            }
        }
    }
}

fn too_many_streams() -> ApiError {
    let mut e = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "too_many_streams",
        "too many open timeline streams; close one first",
    );
    e.retry_after = Some(RETRY_AFTER_SECS);
    e
}

async fn page(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    headers: HeaderMap,
    QueryArgs(q): QueryArgs<TimelineQuery>,
) -> Result<Json<TimelinePage>, ApiError> {
    let call = if q.scope.as_deref() == Some("mine") {
        let claimed = crate::routes::chat_channel(&headers, None)?;
        let channel = crate::routes::app_channel(&headers, &claimed, None, None);
        Call::timeline_for_device(&q, &ident.device_id, &channel)?
    } else {
        Call::timeline(&q)?
    };
    Ok(Json(st.lucina()?.call::<TimelinePage>(&call).await?))
}

/// `GET /v1/timeline/cards`: which items' buttons are closed, the same for every client.
async fn cards(State(st): State<AppState>) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(st.lucina()?.call::<serde_json::Value>(&Call::timeline_cards()).await?))
}

/// `POST /v1/timeline/cards/{item}` with `{action}`: the owner used this button.
async fn choose_card(
    State(st): State<AppState>,
    UrlPath(item): UrlPath<String>,
    JsonBody(body): JsonBody<serde_json::Value>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(st.lucina()?.call::<serde_json::Value>(&Call::timeline_card_choice(&item, body)?).await?))
}

async fn conversations(State(st): State<AppState>) -> Result<Json<TimelineConversations>, ApiError> {
    Ok(Json(st.lucina()?.timeline_conversations().await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamQuery {
    /// For a new EventSource, which cannot set headers; the header wins.
    last_event_id: Option<String>,
}

const LAST_EVENT_ID: HeaderName = HeaderName::from_static("last-event-id");

async fn stream(
    State(st): State<AppState>,
    Extension(ident): Extension<Identity>,
    headers: HeaderMap,
    QueryArgs(q): QueryArgs<StreamQuery>,
) -> Result<Response, ApiError> {
    let last = match headers.get(LAST_EVENT_ID) {
        Some(v) => Some(
            v.to_str()
                .map_err(|_| {
                    ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "bad_request",
                        "Last-Event-ID: not text",
                    )
                })?
                .trim()
                .to_owned(),
        ),
        None => q.last_event_id,
    }
    .filter(|s| !s.is_empty());
    if let Some(id) = last.as_deref() {
        secretary_core::validate::id("Last-Event-ID", id)?;
    }
    let lucina = st.lucina()?;
    let slot = st
        .streams
        .try_acquire(&ident.device_id)
        .ok_or_else(too_many_streams)?;
    let upstream = lucina.timeline_stream(last.as_deref()).await?;
    // While it is open the device counts as connected for the notifier (a reply it asked for is shown here).
    let link = st.notify.link(&ident.device_id);

    let mut shutdown = st.shutdown.subscribe();
    let body = upstream
        .bytes_stream()
        // An upstream error (reset, idle timeout) ends the response cleanly.
        .take_while(|chunk| {
            let ok = match chunk {
                Ok(_) => true,
                Err(e) => {
                    tracing::info!(
                        timeout = e.is_timeout(),
                        "timeline upstream ended with an error"
                    );
                    false
                }
            };
            std::future::ready(ok)
        })
        .take_until(async move {
            let _ = shutdown.wait_for(|stop| *stop).await;
        })
        // The slot lives as long as the body.
        .map(move |chunk| {
            let _held = (&slot, &link);
            chunk
        });

    let mut resp = Body::from_stream(body).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    Ok(resp)
}

/// Behind `require_device` (Bearer or cookie; GET needs no CSRF).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/timeline", get(page))
        .route("/v1/timeline/stream", get(stream))
        .route("/v1/timeline/conversations", get(conversations))
        .route("/v1/timeline/cards", get(cards))
        .route("/v1/timeline/cards/{item}", post(choose_card))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_limit_per_device_and_total() {
        let s = StreamSlots::new(2, 3);
        let a1 = s.try_acquire("a").unwrap();
        let _a2 = s.try_acquire("a").unwrap();
        assert!(s.try_acquire("a").is_none(), "per-device cap");
        let _b1 = s.try_acquire("b").unwrap();
        assert!(s.try_acquire("c").is_none(), "total cap");
        assert_eq!(s.open("a"), (3, 2));
        drop(a1);
        assert_eq!(s.open("a"), (2, 1));
        let _c1 = s.try_acquire("c").unwrap();
        assert!(s.try_acquire("a").is_none(), "total cap again");
    }

    #[test]
    fn slots_are_freed_on_drop() {
        let s = StreamSlots::new(PER_DEVICE, TOTAL);
        let held: Vec<_> = (0..PER_DEVICE)
            .map(|_| s.try_acquire("d").unwrap())
            .collect();
        assert!(s.try_acquire("d").is_none());
        drop(held);
        assert_eq!(s.open("d"), (0, 0));
        assert!(s.counts.lock().unwrap().per_device.is_empty());
    }
}
