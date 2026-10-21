//! Wire models for Lucina's `/api/secretary/*` contract.
//!
//! Field names are Lucina's (camelCase). Every response struct keeps the
//! fields it does not know in `extra`, so a newer Lucina can add fields
//! without the server silently dropping them on the way to the app.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

/// `{ok:true,data}` | `{ok:false,error,message?}`.
#[derive(Debug, Clone, Deserialize)]
pub struct Envelope<T> {
    pub ok: bool,
    #[serde(default = "Option::default")]
    pub data: Option<T>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

// -- health -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    #[serde(default)]
    pub persona: Option<String>,
    #[serde(default)]
    pub memory_provider: Option<String>,
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub now: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// -- chat / jobs ------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReply {
    pub reply: String,
    /// What Lucina decided to do with the message (e.g. `"reminder.create"`);
    /// kept opaque so new actions need no server release.
    #[serde(default)]
    pub action: Value,
    #[serde(default)]
    pub job: Option<JobRef>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRef {
    pub id: String,
    pub status: JobStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub status: JobStatus,
    #[serde(default)]
    pub action: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// -- reminders --------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReminderStatus {
    Pending,
    Sent,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reminder {
    pub id: String,
    pub text: String,
    pub due_at: String,
    #[serde(default)]
    pub time_zone: Option<String>,
    pub status: ReminderStatus,
    #[serde(default)]
    pub source: Value,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub sent_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReminderScope {
    Upcoming,
    Today,
    All,
}

impl ReminderScope {
    pub fn as_str(self) -> &'static str {
        match self {
            ReminderScope::Upcoming => "upcoming",
            ReminderScope::Today => "today",
            ReminderScope::All => "all",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "upcoming" => Some(ReminderScope::Upcoming),
            "today" => Some(ReminderScope::Today),
            "all" => Some(ReminderScope::All),
            _ => None,
        }
    }
}

/// Body of `POST /reminders`: an explicit `{text, dueAt}` or a
/// `{naturalText}` for Lucina to parse ("tomorrow 9am call mum").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CreateReminder {
    #[serde(rename_all = "camelCase")]
    Explicit { text: String, due_at: String },
    #[serde(rename_all = "camelCase")]
    Natural { natural_text: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreatedReminder {
    pub created: bool,
    pub reminder: Reminder,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CancelledReminder {
    pub reminder: Reminder,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// -- agenda -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agenda {
    pub date: String,
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub reminders: Vec<Reminder>,
    /// Lucina's generated day briefing (text or structured); opaque here.
    #[serde(default)]
    pub briefing: Value,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// -- query strings ----------------------------------------------------------

/// Query of `GET /reminders`, as the app sends it. Raw strings on purpose:
/// [`crate::Call::reminders`] validates them and builds Lucina's query.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// `0|1` (also accepted: `true|false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_sent: Option<String>,
}

/// Query of `GET /agenda`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgendaQuery {
    /// `YYYY-MM-DD`; Lucina defaults to today in its time zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// `0|1` (also accepted: `true|false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<String>,
}

// -- shopping ---------------------------------------------------------------

/// A grocery item. Only the id is interpreted; every other field is passed
/// through untouched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShoppingItem {
    #[serde(deserialize_with = "string_or_number")]
    pub id: String,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

fn string_or_number<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    match Value::deserialize(d)? {
        Value::String(s) => Ok(s),
        Value::Number(n) => Ok(n.to_string()),
        other => Err(serde::de::Error::custom(format!(
            "expected string or number id, got {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reminder_roundtrip_keeps_unknown_fields() {
        let raw = json!({
            "id": "r1", "text": "call mum", "dueAt": "2026-10-22T09:00:00+02:00",
            "timeZone": "Europe/Rome", "status": "pending", "source": "chat",
            "createdAt": "2026-10-21T10:00:00Z", "sentAt": null, "priority": 2
        });
        let r: Reminder = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(r.status, ReminderStatus::Pending);
        assert_eq!(serde_json::to_value(&r).unwrap(), raw);
    }

    #[test]
    fn create_reminder_forms() {
        let a: CreateReminder =
            serde_json::from_value(json!({"text": "x", "dueAt": "2026-10-22T09:00:00Z"})).unwrap();
        assert!(matches!(a, CreateReminder::Explicit { .. }));
        let b: CreateReminder =
            serde_json::from_value(json!({"naturalText": "tomorrow 9 call mum"})).unwrap();
        assert!(matches!(b, CreateReminder::Natural { .. }));
        assert_eq!(
            serde_json::to_value(&b).unwrap(),
            json!({"naturalText": "tomorrow 9 call mum"})
        );
    }

    #[test]
    fn shopping_item_numeric_id() {
        let i: ShoppingItem =
            serde_json::from_value(json!({"id": 7, "name": "milk", "checked": false})).unwrap();
        assert_eq!(i.id, "7");
        assert_eq!(i.fields["name"], "milk");
    }

    #[test]
    fn chat_reply_without_job() {
        let r: ChatReply =
            serde_json::from_value(json!({"reply": "hi", "action": null, "job": null})).unwrap();
        assert!(r.job.is_none());
    }
}
