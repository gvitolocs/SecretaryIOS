//! Who writes `reminders.json`.
//!
//! Phase 1 of the port plan: the Node Lucina still owns the file (it writes
//! it non-atomically under an in-process lock), so Rust only **reads** it and
//! forwards create/cancel to Node's own `POST /api/secretary/reminders` and
//! `DELETE /api/secretary/reminders/:id` ([`NodeForwarder`], the default).
//! [`LocalWriter`] writes the file directly (phase 2, or a data copy).

use std::sync::Arc;
use std::time::Duration;

use lucina_store::reminders::{CancelOutcome, CreateReminder, Reminder, ReminderStore};
use serde_json::{json, Value};

/// A reminder write that failed with an API error code.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl WriteError {
    pub fn new(status: u16, code: &str, message: impl Into<String>) -> Self {
        Self { status, code: code.into(), message: message.into() }
    }
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.code, self.message)
    }
}

/// Result of forwarding a raw API body: `(status, data)`.
pub type Forwarded = Result<(u16, Value), WriteError>;

#[async_trait::async_trait]
pub trait ReminderWriter: Send + Sync {
    fn kind(&self) -> &'static str;
    /// Create `{text, dueAt}` for the owner: `(created, reminder)`.
    async fn create(&self, input: CreateReminder) -> Result<(bool, Reminder), WriteError>;
    async fn cancel(&self, id: &str, owner_chat_id: &str) -> Result<CancelOutcome, WriteError>;
    /// `POST /reminders` body as received (Node parses `naturalText` itself).
    async fn forward_create(&self, body: &Value) -> Option<Forwarded> {
        let _ = body;
        None
    }
}

/// Writes the file (single writer only).
pub struct LocalWriter(pub Arc<ReminderStore>);

#[async_trait::async_trait]
impl ReminderWriter for LocalWriter {
    fn kind(&self) -> &'static str {
        "local"
    }

    async fn create(&self, input: CreateReminder) -> Result<(bool, Reminder), WriteError> {
        let store = self.0.clone();
        tokio::task::spawn_blocking(move || store.create(input))
            .await
            .map_err(|e| WriteError::new(500, "internal_error", e.to_string()))?
            .map_err(|e| WriteError::new(500, "internal_error", e.to_string()))
    }

    async fn cancel(&self, id: &str, owner_chat_id: &str) -> Result<CancelOutcome, WriteError> {
        let (store, id, chat) = (self.0.clone(), id.to_owned(), owner_chat_id.to_owned());
        tokio::task::spawn_blocking(move || store.cancel(&id, Some(&chat)))
            .await
            .map_err(|e| WriteError::new(500, "internal_error", e.to_string()))?
            .map_err(|e| WriteError::new(500, "internal_error", e.to_string()))
    }
}

/// Forwards writes to the Node Lucina (`LUCINA_NODE_URL`, token from
/// `LUCINA_NODE_TOKEN_FILE`, which is Node's `SECRETARY_API_TOKEN`).
pub struct NodeForwarder {
    base: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl std::fmt::Debug for NodeForwarder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeForwarder").field("base", &self.base).field("token", &self.token.as_ref().map(|_| "<redacted>")).finish()
    }
}

impl NodeForwarder {
    pub fn new(base: &str, token: Option<String>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            token: token.filter(|t| !t.is_empty()),
            http: reqwest::Client::builder().timeout(Duration::from_secs(15)).build().expect("http client"),
        }
    }

    fn unconfigured() -> WriteError {
        WriteError::new(503, "reminders_disabled", "Reminder writes go to the Node Lucina and LUCINA_NODE_TOKEN_FILE is not set")
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> Forwarded {
        let token = self.token.as_ref().ok_or_else(Self::unconfigured)?;
        let resp = rb
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| WriteError::new(503, "reminders_disabled", format!("Node Lucina unreachable: {}", e.without_url())))?;
        let status = resp.status().as_u16();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if body["ok"] == json!(true) {
            return Ok((status, body["data"].clone()));
        }
        let code = body["error"].as_str().unwrap_or("internal_error").to_owned();
        let message = body["message"].as_str().unwrap_or("").to_owned();
        // Node's 401/404-for-disabled mean the forward itself is misconfigured.
        if status == 401 || (status == 404 && code == "not_found") {
            return Err(WriteError::new(503, "reminders_disabled", format!("Node Lucina refused the forward ({status} {code})")));
        }
        Err(WriteError { status, code, message })
    }
}

fn reminder_from_public(v: &Value) -> Reminder {
    let s = |k: &str| v[k].as_str().unwrap_or("").to_owned();
    let o = |k: &str| v[k].as_str().map(str::to_owned);
    Reminder {
        id: s("id"),
        dedupe_key: String::new(),
        text: s("text"),
        due_at: s("dueAt"),
        time_zone: s("timeZone"),
        chat_id: String::new(),
        user_peer_id: String::new(),
        user_label: String::new(),
        persona: String::new(),
        source: v["source"].clone(),
        status: s("status"),
        created_at: v["createdAt"].clone(),
        sent_at: o("sentAt"),
        last_error: None,
        cancelled_at: o("cancelledAt"),
        extra: Default::default(),
    }
}

#[async_trait::async_trait]
impl ReminderWriter for NodeForwarder {
    fn kind(&self) -> &'static str {
        "node"
    }

    async fn create(&self, input: CreateReminder) -> Result<(bool, Reminder), WriteError> {
        let (_, data) = self
            .send(self.http.post(format!("{}/api/secretary/reminders", self.base)).json(&json!({"text": input.text, "dueAt": input.due_at})))
            .await?;
        Ok((data["created"] == json!(true), reminder_from_public(&data["reminder"])))
    }

    async fn cancel(&self, id: &str, _owner_chat_id: &str) -> Result<CancelOutcome, WriteError> {
        match self.send(self.http.delete(format!("{}/api/secretary/reminders/{id}", self.base))).await {
            Ok((_, data)) => {
                let r = reminder_from_public(&data["reminder"]);
                Ok(if data["cancelled"] == json!(true) { CancelOutcome::Cancelled(r) } else { CancelOutcome::AlreadyCancelled(r) })
            }
            Err(e) if e.code == "reminder_not_found" => Ok(CancelOutcome::NotFound),
            Err(e) if e.code == "already_sent" => Ok(CancelOutcome::AlreadySent(Reminder { id: id.to_owned(), ..reminder_from_public(&json!({"status": "sent"})) })),
            Err(e) => Err(e),
        }
    }

    async fn forward_create(&self, body: &Value) -> Option<Forwarded> {
        Some(self.send(self.http.post(format!("{}/api/secretary/reminders", self.base)).json(body)).await)
    }
}
