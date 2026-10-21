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
    /// Contract version (`1`).
    #[serde(default)]
    pub api_version: Option<u32>,
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
    /// The client surface: `web` or `ios` (see [`crate::validate::CHAT_CHANNELS`]).
    /// secretary-server always sets it before forwarding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReply {
    pub reply: String,
    /// What Lucina did: `reply`, `grocery`, `list_reminders`,
    /// `schedule_reminder`, or an async action (`graph_task`,
    /// `generate_image`, …). Kept opaque so new actions need no release.
    #[serde(default)]
    pub action: Value,
    /// Set for a started async action; poll `GET /jobs/{id}`.
    #[serde(default)]
    pub job: Option<JobRef>,
    /// Full Honcho session id used (`secretary_<sessionId>`).
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub client_message_id: Option<String>,
    /// Only for `action: schedule_reminder` when a reminder exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reminder: Option<Reminder>,
    /// `true` on a `clientMessageId` replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duplicate: Option<bool>,
    /// Timeline id of the stored user message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_item_id: Option<String>,
    /// Timeline id of Lucina's stored reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lucina_item_id: Option<String>,
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
    /// Reply text once `done`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Failure text when `status` is `error` (`"interrupted"` after a restart).
    #[serde(default)]
    pub error: Option<String>,
    /// Only for `generate_image` with an http(s) result.
    #[serde(default)]
    pub image_url: Option<String>,
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
    #[serde(default)]
    pub cancelled_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `GET /reminders` data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderList {
    pub scope: ReminderScope,
    pub include_sent: bool,
    #[serde(default)]
    pub time_zone: Option<String>,
    pub reminders: Vec<Reminder>,
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
    /// `false` when it was already cancelled.
    #[serde(default)]
    pub cancelled: bool,
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

// -- timeline ---------------------------------------------------------------

/// A JSON string or number kept exactly as Lucina sent it (timeline ids and
/// timestamps; the web client accepts both forms).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Scalar {
    Text(String),
    Number(serde_json::Number),
}

impl Scalar {
    /// The value as text (`"42"` for the number 42).
    pub fn to_text(&self) -> String {
        match self {
            Scalar::Text(s) => s.clone(),
            Scalar::Number(n) => n.to_string(),
        }
    }
}

impl From<&str> for Scalar {
    fn from(s: &str) -> Self {
        Scalar::Text(s.to_owned())
    }
}

/// Where a timeline message was said. Unknown values survive a round trip
/// in [`Channel::Unknown`] (clients show them as `other`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Channel {
    Telegram,
    Discord,
    Web,
    Ios,
    Alexa,
    #[default]
    Other,
    Unknown(String),
}

impl From<String> for Channel {
    fn from(s: String) -> Self {
        match s.as_str() {
            "telegram" => Channel::Telegram,
            "discord" => Channel::Discord,
            "web" => Channel::Web,
            "ios" => Channel::Ios,
            "alexa" => Channel::Alexa,
            "other" => Channel::Other,
            _ => Channel::Unknown(s),
        }
    }
}

impl From<Channel> for String {
    fn from(c: Channel) -> Self {
        match c {
            Channel::Telegram => "telegram".into(),
            Channel::Discord => "discord".into(),
            Channel::Web => "web".into(),
            Channel::Ios => "ios".into(),
            Channel::Alexa => "alexa".into(),
            Channel::Other => "other".into(),
            Channel::Unknown(s) => s,
        }
    }
}

/// Who said it. Unknown values survive a round trip in [`Role::Unknown`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Role {
    User,
    Lucina,
    Unknown(String),
}

impl From<String> for Role {
    fn from(s: String) -> Self {
        match s.as_str() {
            "user" => Role::User,
            "lucina" => Role::Lucina,
            _ => Role::Unknown(s),
        }
    }
}

impl From<Role> for String {
    fn from(r: Role) -> Self {
        match r {
            Role::User => "user".into(),
            Role::Lucina => "lucina".into(),
            Role::Unknown(s) => s,
        }
    }
}

/// One message of the unified conversation timeline (any channel).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineItem {
    pub id: Scalar,
    /// Epoch milliseconds (an ISO 8601 string is tolerated and kept).
    pub ts: Scalar,
    #[serde(default)]
    pub channel: Channel,
    pub role: Role,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<Scalar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<Scalar>,
    /// `[{kind: image|voice|file, name?}]`; opaque here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Value>,
    /// `{chatTitle?, guildName?, channelName?, isDm?, kind?}`; opaque here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backfilled: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `GET /timeline` data: newest first; `nextBefore` is the cursor for the
/// next (older) page, `null` at the start of history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelinePage {
    #[serde(default)]
    pub items: Vec<TimelineItem>,
    #[serde(default)]
    pub next_before: Option<Scalar>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// -- query strings ----------------------------------------------------------

/// Query of `GET /timeline`, as the app sends it (validated by
/// [`crate::Call::timeline`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineQuery {
    /// Cursor: an item id; only older items are returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// 1..=200; Lucina defaults to 50.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<String>,
}

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
            "createdAt": "2026-10-21T10:00:00Z", "sentAt": null, "cancelledAt": null, "priority": 2
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
        assert!(r.user_item_id.is_none());
        let r: ChatReply = serde_json::from_value(
            json!({"reply": "hi", "userItemId": "u1", "lucinaItemId": "l1"}),
        )
        .unwrap();
        assert_eq!(
            (r.user_item_id.as_deref(), r.lucina_item_id.as_deref()),
            (Some("u1"), Some("l1"))
        );
    }

    #[test]
    fn timeline_item_roundtrip_keeps_shape_and_unknown_fields() {
        let raw = json!({
            "id": "tl_2", "ts": 1_761_048_000_000_i64, "channel": "telegram", "role": "lucina",
            "text": "Fatto.", "clientMessageId": "c1", "replyTo": "tl_1",
            "attachments": [{"kind": "image", "name": "x.png"}],
            "meta": {"chatTitle": "Casa", "isDm": false}, "backfilled": true, "edited": 1
        });
        let it: TimelineItem = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(it.channel, Channel::Telegram);
        assert_eq!(it.role, Role::Lucina);
        assert_eq!(it.extra["edited"], 1);
        assert_eq!(serde_json::to_value(&it).unwrap(), raw);

        // numeric ids, ISO ts, unknown channel and a missing text all survive
        let raw = json!({"id": 7, "ts": "2026-10-21T12:00:00.000Z", "channel": "whatsapp",
                         "role": "user"});
        let it: TimelineItem = serde_json::from_value(raw).unwrap();
        assert_eq!(it.id.to_text(), "7");
        assert_eq!(it.channel, Channel::Unknown("whatsapp".into()));
        assert_eq!(it.text, "");
        let out = serde_json::to_value(&it).unwrap();
        assert_eq!(out["id"], 7);
        assert_eq!(out["channel"], "whatsapp");

        let page: TimelinePage =
            serde_json::from_value(json!({"items": [], "nextBefore": null})).unwrap();
        assert!(page.next_before.is_none());
        assert!(
            serde_json::from_value::<TimelineItem>(json!({"id": "x", "role": "user"})).is_err()
        );
    }
}
