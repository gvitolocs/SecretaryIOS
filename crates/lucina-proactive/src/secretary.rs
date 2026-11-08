//! secretary-server, with Lucina's service token (`svc_…`, read from its
//! file on every call so a rotation lands without a restart):
//!
//! - `GET /v1/calendar/events?from=&to=` (Google, ICS and iPhone merged);
//! - `GET /v1/notes?q=`, `GET /v1/notes/{key}`, `POST /v1/notes` (prep drafts);
//! - `POST /v1/notify` (push; quiet hours, DND and "app in front" are the
//!   server's call: its answer is recorded as is);
//! - `GET /v1/notify/settings` (quiet hours, DND, language);
//! - `GET /v1/proactive/feedback?after=` (taps on the feedback buttons).
//!
//! The last two are new (branch `proactive-server`); a 404 means "not
//! deployed yet" and the engine falls back (config quiet hours, no feedback).

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("secretary-server not configured (LUCINA_SECRETARY_SERVER_URL / service token)")]
    NotConfigured,
    #[error("secretary-server HTTP {status} {code}")]
    Http { status: u16, code: String },
    #[error("secretary-server transport: {0}")]
    Transport(String),
    #[error("secretary-server response: {0}")]
    Decode(String),
}

impl ApiError {
    pub fn is_missing_route(&self) -> bool {
        matches!(
            self,
            ApiError::Http {
                status: 404 | 405,
                ..
            }
        )
    }
}

/// One calendar event (`secretary-calendar`'s `Event`, the fields we use).
#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalEvent {
    pub id: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub calendar: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub start: String,
    #[serde(default)]
    pub end: String,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub start_ms: i64,
    #[serde(default)]
    pub end_ms: i64,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

impl CalEvent {
    pub fn starts(&self) -> Option<DateTime<Utc>> {
        DateTime::from_timestamp_millis(self.start_ms)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub pinned: bool,
    /// Keep: in the archive (not a note to bring up unprompted).
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub excerpt: String,
    #[serde(default)]
    pub updated_at: Value,
}

#[async_trait::async_trait]
pub trait SecretaryApi: Send + Sync {
    async fn calendar_events(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CalEvent>, ApiError>;
    async fn notes(&self, query: Option<&str>) -> Result<Vec<NoteSummary>, ApiError>;
    /// The full note (`content` included), `None` when it does not exist.
    async fn note(&self, key: &str) -> Result<Option<Value>, ApiError>;
    /// Create a note; returns its slug.
    async fn create_note(
        &self,
        title: &str,
        content: &str,
        slug: &str,
        tags: &[&str],
    ) -> Result<String, ApiError>;
    /// `POST /v1/notify`; the server's JSON answer.
    async fn notify(&self, body: &Value) -> Result<Value, ApiError>;
    /// Notification settings; `None` when the route is not deployed.
    async fn notify_settings(&self) -> Result<Option<Value>, ApiError>;
    /// Feedback taps after the cursor; `None` when the route is not deployed.
    async fn feedback_since(&self, after: Option<&str>) -> Result<Option<Value>, ApiError>;
}

pub struct HttpSecretary {
    base: String,
    token_file: PathBuf,
    http: reqwest::Client,
}

impl std::fmt::Debug for HttpSecretary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpSecretary")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

fn iso(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Percent-encode a query value (RFC 3986 unreserved kept).
pub fn qs(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl HttpSecretary {
    pub fn new(base: &str, token_file: PathBuf) -> Result<Self, ApiError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("lucina-proactive/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| ApiError::Transport(e.without_url().to_string()))?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            token_file,
            http,
        })
    }

    fn token(&self) -> Result<String, ApiError> {
        std::fs::read_to_string(&self.token_file)
            .map(|t| t.trim().to_owned())
            .ok()
            .filter(|t| !t.is_empty())
            .ok_or(ApiError::NotConfigured)
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value, ApiError> {
        let resp = req
            .bearer_auth(self.token()?)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| ApiError::Transport(e.without_url().to_string()))?;
        let status = resp.status().as_u16();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            let code = body
                .pointer("/error/code")
                .or_else(|| body.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            return Err(ApiError::Http { status, code });
        }
        Ok(body)
    }

    async fn get(&self, path: &str) -> Result<Value, ApiError> {
        self.send(self.http.get(format!("{}{path}", self.base)))
            .await
    }
}

#[async_trait::async_trait]
impl SecretaryApi for HttpSecretary {
    async fn calendar_events(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<CalEvent>, ApiError> {
        let v = self
            .get(&format!(
                "/v1/calendar/events?from={}&to={}",
                qs(&iso(from)),
                qs(&iso(to))
            ))
            .await?;
        serde_json::from_value(v.get("events").cloned().unwrap_or(json!([])))
            .map_err(|e| ApiError::Decode(e.to_string()))
    }

    async fn notes(&self, query: Option<&str>) -> Result<Vec<NoteSummary>, ApiError> {
        let path = match query {
            Some(q) => format!("/v1/notes?limit=50&q={}", qs(q)),
            None => "/v1/notes?limit=200&archived=exclude".into(),
        };
        let v = self.get(&path).await?;
        serde_json::from_value(v.get("notes").cloned().unwrap_or(json!([])))
            .map_err(|e| ApiError::Decode(e.to_string()))
    }

    async fn note(&self, key: &str) -> Result<Option<Value>, ApiError> {
        match self.get(&format!("/v1/notes/{}", qs(key))).await {
            Ok(v) => Ok(Some(v)),
            Err(ApiError::Http { status: 404, .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn create_note(
        &self,
        title: &str,
        content: &str,
        slug: &str,
        tags: &[&str],
    ) -> Result<String, ApiError> {
        let body = json!({"title": title, "content": content, "slug": slug, "tags": tags, "source": "lucina"});
        let v = self
            .send(
                self.http
                    .post(format!("{}/v1/notes", self.base))
                    .json(&body),
            )
            .await?;
        Ok(v.get("slug")
            .and_then(Value::as_str)
            .unwrap_or(slug)
            .to_owned())
    }

    async fn notify(&self, body: &Value) -> Result<Value, ApiError> {
        self.send(
            self.http
                .post(format!("{}/v1/notify", self.base))
                .json(body),
        )
        .await
    }

    async fn notify_settings(&self) -> Result<Option<Value>, ApiError> {
        match self.get("/v1/notify/settings").await {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.is_missing_route() => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn feedback_since(&self, after: Option<&str>) -> Result<Option<Value>, ApiError> {
        let path = match after {
            Some(a) => format!("/v1/proactive/feedback?after={}", qs(a)),
            None => "/v1/proactive/feedback".into(),
        };
        match self.get(&path).await {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.is_missing_route() => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// No secretary-server (not configured): every read is empty, every write fails.
pub struct NoSecretary;

#[async_trait::async_trait]
impl SecretaryApi for NoSecretary {
    async fn calendar_events(
        &self,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) -> Result<Vec<CalEvent>, ApiError> {
        Err(ApiError::NotConfigured)
    }
    async fn notes(&self, _: Option<&str>) -> Result<Vec<NoteSummary>, ApiError> {
        Err(ApiError::NotConfigured)
    }
    async fn note(&self, _: &str) -> Result<Option<Value>, ApiError> {
        Err(ApiError::NotConfigured)
    }
    async fn create_note(&self, _: &str, _: &str, _: &str, _: &[&str]) -> Result<String, ApiError> {
        Err(ApiError::NotConfigured)
    }
    async fn notify(&self, _: &Value) -> Result<Value, ApiError> {
        Err(ApiError::NotConfigured)
    }
    async fn notify_settings(&self) -> Result<Option<Value>, ApiError> {
        Ok(None)
    }
    async fn feedback_since(&self, _: Option<&str>) -> Result<Option<Value>, ApiError> {
        Ok(None)
    }
}
