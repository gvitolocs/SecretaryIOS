//! The local Universal Parcel Scraper (`POST /v1/detect`, `POST /v1/track`).

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::policy::{Event, Stage, Status, MAX_EVENTS};

/// A carrier lookup can take up to a minute.
const HTTP_TIMEOUT: Duration = Duration::from_secs(100);
const BUDGET_MS: u64 = 60_000;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrackError {
    #[error("the carrier has no record of it")]
    NotFound,
    /// The carrier needs one more detail (the scraper's `field`).
    #[error("the carrier needs the {0}")]
    InputRequired(String),
    #[error("the carrier does not accept this number")]
    Invalid,
    #[error("{0}")]
    Unavailable(String),
}

/// What `/v1/detect` found in a text.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct Detection {
    #[serde(rename = "trackingNumber", default)]
    pub number: Option<String>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub carrier: Option<String>,
    #[serde(default)]
    pub confidence: String,
    #[serde(default)]
    pub candidates: Vec<String>,
}

impl Detection {
    /// The tracking number, when the text held one.
    pub fn found(&self) -> Option<&str> {
        self.number
            .as_deref()
            .filter(|n| !n.trim().is_empty() && self.source != "none")
    }

    /// The carrier, only when the scraper is sure of it.
    pub fn sure_carrier(&self) -> Option<&str> {
        self.carrier
            .as_deref()
            .filter(|c| self.confidence == "high" && !c.is_empty())
    }
}

#[async_trait]
pub trait Track: Send + Sync {
    async fn track(&self, number: &str, carrier: Option<&str>) -> Result<Status, TrackError>;
}

#[derive(Clone)]
pub struct Scraper {
    base: String,
    http: reqwest::Client,
}

impl Scraper {
    pub fn new(base_url: &str) -> Scraper {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(HTTP_TIMEOUT)
            .build()
            .expect("http client");
        Scraper {
            base: base_url.trim().trim_end_matches('/').to_owned(),
            http,
        }
    }

    async fn post(&self, path: &str, body: Value) -> Result<(u16, Value), TrackError> {
        let r = self
            .http
            .post(format!("{}{path}", self.base))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                TrackError::Unavailable(format!("the tracking service is not reachable ({e})"))
            })?;
        let status = r.status().as_u16();
        let body = r.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, body))
    }

    pub async fn detect(&self, text: &str) -> Result<Detection, TrackError> {
        let (status, body) = self.post("/v1/detect", json!({ "text": text })).await?;
        if status != 200 {
            return Err(parse_error(status, &body));
        }
        serde_json::from_value(body)
            .map_err(|e| TrackError::Unavailable(format!("unreadable detect answer ({e})")))
    }
}

#[async_trait]
impl Track for Scraper {
    async fn track(&self, number: &str, carrier: Option<&str>) -> Result<Status, TrackError> {
        let mut body = json!({ "number": number, "budgetMs": BUDGET_MS });
        if let Some(c) = carrier.filter(|c| !c.is_empty()) {
            body["carrier"] = json!(c);
        }
        let (status, body) = self.post("/v1/track", body).await?;
        if status == 200 {
            parse_status(&body)
        } else {
            Err(parse_error(status, &body))
        }
    }
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// A `/v1/track` 200 body as a [`Status`].
pub fn parse_status(body: &Value) -> Result<Status, TrackError> {
    let result = body
        .get("result")
        .filter(|r| r.is_object())
        .ok_or_else(|| {
            TrackError::Unavailable("the tracking service answered without a result".into())
        })?;
    let events = result
        .get("events")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let newest = events
        .iter()
        .filter_map(|e| {
            Some((
                chrono::DateTime::parse_from_rfc3339(e.get("instant")?.as_str()?).ok()?,
                e,
            ))
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, e)| e)
        .or_else(|| events.first());
    Ok(Status {
        carrier: text(body, "carrier").unwrap_or_default(),
        stage: Stage::parse(
            result
                .get("current_stage")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ),
        text: text(result, "last_status_text"),
        last_update: text(result, "last_update"),
        expected_delivery: text(result, "expected_delivery"),
        location: newest.and_then(|e| text(e, "location")),
        destination_country: text(result, "destination_country"),
        events: parse_events(events),
    })
}

/// The scans, newest first (by `instant`; scans without one keep their order, after the dated
/// ones), at most [`MAX_EVENTS`].
fn parse_events(events: &[Value]) -> Vec<Event> {
    let mut dated: Vec<(Option<chrono::DateTime<chrono::FixedOffset>>, Event)> = events
        .iter()
        .filter_map(|e| {
            let description = text(e, "description")?;
            let instant = e
                .get("instant")
                .and_then(Value::as_str)
                .and_then(|i| chrono::DateTime::parse_from_rfc3339(i).ok());
            Some((
                instant,
                Event {
                    time: text(e, "time"),
                    description,
                    location: text(e, "location"),
                    stage: Stage::parse(e.get("stage").and_then(Value::as_str).unwrap_or("")),
                },
            ))
        })
        .collect();
    dated.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    dated.into_iter().take(MAX_EVENTS).map(|(_, e)| e).collect()
}

/// A scraper error body (HTTP status and JSON) as a [`TrackError`].
pub fn parse_error(http_status: u16, body: &Value) -> TrackError {
    let kind = body
        .pointer("/hint/kind")
        .and_then(Value::as_str)
        .unwrap_or("");
    match kind {
        "not_found" => TrackError::NotFound,
        "input_required" => TrackError::InputRequired(
            text(body, "field").unwrap_or_else(|| "missing detail".into()),
        ),
        "invalid_input" => TrackError::Invalid,
        _ => {
            let why =
                text(body, "error").or_else(|| (!kind.is_empty()).then(|| kind.replace('_', " ")));
            TrackError::Unavailable(why.unwrap_or_else(|| format!("HTTP {http_status}")))
        }
    }
}
