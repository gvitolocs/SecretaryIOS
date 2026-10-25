//! The Node Lucina's grocery deals engine (`src/grocery/deals/` on Node).
//! Node stays canonical for deals until the port (PORT_PLAN phase 6), so the
//! Rust Lucina asks it, with the same bearer token as the reminder forward
//! (`LUCINA_NODE_TOKEN_FILE`, Node's SECRETARY_API_TOKEN):
//!
//! - `GET /api/secretary/grocery/deals/map` is forwarded as it is (query
//!   string included); Node's status and JSON body pass through unchanged.
//!   Node unreachable, a timeout, a non-JSON body or a refused token answer
//!   `503 deals_unavailable`.
//! - Chat turns that classify as one of [`DEALS_INTENTS`] get their reply
//!   from `POST /api/secretary/grocery/deals/answer`. Any failure there
//!   returns `None` and the turn falls back to the Rust grocery path, so a
//!   Rust deploy before the Node route lands is safe.

use std::time::Duration;

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

use crate::api::ApiError;

/// The grocery intents Node answers (its `DEALS_ANSWER_INTENTS`).
pub const DEALS_INTENTS: &[&str] = &[
    "grocery.offer.search",
    "grocery.deals.cheapest",
    "grocery.deals.lunch",
];

/// The deal engine reads flyers and food-waste stock from several sources.
pub const TIMEOUT: Duration = Duration::from_secs(25);

pub struct NodeDeals {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl std::fmt::Debug for NodeDeals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeDeals")
            .field("base", &self.base)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

fn unavailable(message: impl Into<String>) -> ApiError {
    ApiError::new(503, "deals_unavailable", message)
}

impl NodeDeals {
    pub fn new(base: &str, token: Option<String>, timeout: Duration) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            token: token.filter(|t| !t.is_empty()),
            http: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .expect("http client"),
        }
    }

    /// `GET /grocery/deals/map[?query]` on Node, passed through.
    pub async fn map(&self, query: Option<&str>) -> Result<Response, ApiError> {
        let token = self.token.as_ref().ok_or_else(|| {
            unavailable("Deals come from the Node Lucina and LUCINA_NODE_TOKEN_FILE is not set")
        })?;
        let mut url = format!("{}/api/secretary/grocery/deals/map", self.base);
        if let Some(q) = query.filter(|q| !q.is_empty()) {
            url.push('?');
            url.push_str(q);
        }
        let resp = self
            .http
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(error = %e.without_url(), "deals map: Node Lucina unreachable");
                unavailable("The Node Lucina did not answer")
            })?;
        let status = resp.status().as_u16();
        if status == 401 {
            tracing::warn!("deals map: Node Lucina refused the forward token");
            return Err(unavailable("The Node Lucina refused the forward"));
        }
        let body = resp.bytes().await.map_err(|e| {
            tracing::warn!(error = %e.without_url(), "deals map: Node Lucina body failed");
            unavailable("The Node Lucina did not answer")
        })?;
        if serde_json::from_slice::<Value>(&body).is_err() {
            tracing::warn!(status, "deals map: Node Lucina answered no JSON");
            return Err(unavailable("The Node Lucina gave no JSON answer"));
        }
        let mut out = (
            StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            body,
        )
            .into_response();
        let h = out.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        Ok(out)
    }

    /// Node's reply to a deals question: `(reply, intent)`, or `None` when
    /// Node cannot answer (no token, unreachable, timeout, 4xx/5xx).
    pub async fn answer(&self, text: &str, channel: &str) -> Option<(String, String)> {
        let token = self.token.as_ref()?;
        let resp = self
            .http
            .post(format!("{}/api/secretary/grocery/deals/answer", self.base))
            .bearer_auth(token)
            .json(&json!({"text": text, "channel": channel}))
            .send()
            .await
            .map_err(|e| tracing::warn!(error = %e.without_url(), "deals answer: Node Lucina unreachable"))
            .ok()?;
        let status = resp.status().as_u16();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        let data = &body["data"];
        match (status, body["ok"].as_bool(), data["reply"].as_str()) {
            (200, Some(true), Some(reply)) if !reply.trim().is_empty() => Some((
                reply.to_owned(),
                data["intent"].as_str().unwrap_or("").to_owned(),
            )),
            _ => {
                tracing::warn!(
                    status,
                    error = body["error"].as_str().unwrap_or(""),
                    "deals answer: Node Lucina gave no reply; Rust grocery fallback"
                );
                None
            }
        }
    }
}
