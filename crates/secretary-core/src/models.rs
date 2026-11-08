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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_message_id: Option<String>,
    /// The client surface: `web` or `ios`, or `voice` (set by secretary-server for LucinaVoice turns only; see [`crate::validate::CHAT_CHANNELS`]).
    /// secretary-server always sets it before forwarding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Authenticated device; secretary-server overwrites client values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    /// Swipe to reply: the timeline id of the earlier message this one
    /// answers (`[A-Za-z0-9_-]{1,64}`, see [`crate::validate::timeline_ref`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// LucinaVoice turns only (secretary-server sets it): what the turn tells Lucina besides
    /// the words, `{turnId, ms: {stt}, ambiguous?: [{heard, candidates}]}`. `turnId` and `ms`
    /// reach the thread item's `meta.voice`; `ambiguous` feeds Lucina's `vocab` layer. Never
    /// audio. A Lucina that does not know the field ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_meta: Option<Value>,
    /// The client that sent the turn (`ios-chat`, `fortuna-chat`, `juno-chat`, …; `[A-Za-z0-9_-]{1,64}`).
    /// Lucina writes it on the turn's items (`meta.source`); `probe…`/`test…` make it a self-test turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// `selftest` (or `diagnostic`): the turn runs in Lucina's self-test conversation, out of the
    /// owner's chat, and never notifies. secretary-server sets it for test devices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
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

// -- shopping lists (v2, docs/shopping-v2.md) -------------------------------

/// A shopping list (summary in `GET /shopping/lists`, header of a list state).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShoppingList {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// `home`, `grocery`, `work`, `holiday`, `garden`, `hardware`, `baby`, `other`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Count of `needed` items (summaries only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needed: Option<u64>,
    /// Count of `recent` items (summaries only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `GET /shopping/lists` data: a bare array or `{lists: [...]}` (both
/// accepted; the app always gets the array).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ShoppingListsData {
    Wrapped { lists: Vec<ShoppingList> },
    Bare(Vec<ShoppingList>),
}

impl ShoppingListsData {
    pub fn into_lists(self) -> Vec<ShoppingList> {
        match self {
            ShoppingListsData::Wrapped { lists } | ShoppingListsData::Bare(lists) => lists,
        }
    }
}

/// A created/updated list: `{list: {...}}` or the bare list (both accepted;
/// the app always gets the bare list).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ShoppingListData {
    Wrapped { list: ShoppingList },
    Bare(ShoppingList),
}

impl ShoppingListData {
    pub fn into_list(self) -> ShoppingList {
        match self {
            ShoppingListData::Wrapped { list } | ShoppingListData::Bare(list) => list,
        }
    }
}

/// An item object (`docs/shopping-v2.md`, "Item objects"): a catalog item
/// (`kind: "catalog"`, id = catalog id) or a custom object of the owner
/// (`kind: "custom"`, id `u_<ULID>`), with names in `it`, `en` and `da`.
/// Custom objects also carry `aliases`, `needsTranslation`, `createdBy`,
/// `createdAt`, `updatedAt` (kept in `extra`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemObject {
    /// Null only for a row Lucina could not give an object yet.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub names: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `GET /shopping/items` data: `{items}` or the bare array.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ItemObjectsData {
    Wrapped { items: Vec<ItemObject> },
    Bare(Vec<ItemObject>),
}

impl ItemObjectsData {
    pub fn into_items(self) -> Vec<ItemObject> {
        match self {
            ItemObjectsData::Wrapped { items } | ItemObjectsData::Bare(items) => items,
        }
    }
}

/// An item of a v2 list. Unknown statuses survive as [`ListItemStatus::Unknown`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListItem {
    pub id: String,
    /// The item object's id (`item.id`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemObject>,
    /// The object's name in the requested language.
    #[serde(default)]
    pub name: String,
    /// The text as typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typed_name: Option<String>,
    #[serde(default)]
    pub spec: Option<String>,
    /// A number or `null`; opaque here.
    #[serde(default)]
    pub quantity: Value,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub status: ListItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    /// `telegram|discord|web|ios|alexa|lucina`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ListItemStatus {
    /// On the list (tiles).
    Needed,
    /// Bought recently (quick re-add tiles).
    Recent,
    /// Deleted; never shown.
    Removed,
    Unknown(String),
}

impl From<String> for ListItemStatus {
    fn from(s: String) -> Self {
        match s.as_str() {
            "needed" => ListItemStatus::Needed,
            "recent" => ListItemStatus::Recent,
            "removed" => ListItemStatus::Removed,
            _ => ListItemStatus::Unknown(s),
        }
    }
}

impl From<ListItemStatus> for String {
    fn from(s: ListItemStatus) -> Self {
        match s {
            ListItemStatus::Needed => "needed".into(),
            ListItemStatus::Recent => "recent".into(),
            ListItemStatus::Removed => "removed".into(),
            ListItemStatus::Unknown(s) => s,
        }
    }
}

/// `GET /shopping/lists/{id}` data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListState {
    pub list: ShoppingList,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default)]
    pub items: Vec<ListItem>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `POST /shopping/lists/{id}/ops` data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    /// opIds applied by this batch.
    #[serde(default)]
    pub applied: Vec<String>,
    /// opIds that had been applied before (no change).
    #[serde(default)]
    pub duplicates: Vec<String>,
    /// The full list state after the batch.
    #[serde(default)]
    pub items: Vec<ListItem>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `POST /shopping/lists {name, theme?}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateList {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
}

/// `PATCH /shopping/lists/{id} {name?, theme?}` (at least one).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateList {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
}

/// `POST /shopping/lists/{id}/ops` body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsBatch {
    /// The revision the client's view is based on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    /// Language of item names in the answer and of `rename` (`it|en|da`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lang: Option<String>,
    /// Who sent the batch; `mcp` = an MCP client: Lucina checks its
    /// free-text adds with Jev. Absent = the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    pub ops: Vec<ShoppingOp>,
}

/// One idempotent list operation; `opId` is a UUID. Only the listed fields
/// are forwarded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShoppingOp {
    /// Create, or move a `recent` item back to `needed`. Needs an object id
    /// (`itemId`, legacy `catalogId`) or typed text (`text`, legacy `name`).
    #[serde(rename_all = "camelCase")]
    Add {
        op_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        catalog_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spec: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        quantity: Option<serde_json::Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        section: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Need { op_id: String, item_id: String },
    #[serde(rename_all = "camelCase")]
    Purchase { op_id: String, item_id: String },
    #[serde(rename_all = "camelCase")]
    Remove { op_id: String, item_id: String },
    /// `spec: null` or `""` clears it.
    #[serde(rename_all = "camelCase")]
    SetSpec {
        op_id: String,
        item_id: String,
        #[serde(default)]
        spec: Option<String>,
    },
    /// `quantity: null` clears it.
    #[serde(rename_all = "camelCase")]
    SetQuantity {
        op_id: String,
        item_id: String,
        #[serde(default)]
        quantity: Option<serde_json::Number>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    SetSection {
        op_id: String,
        item_id: String,
        section: String,
    },
    #[serde(rename_all = "camelCase")]
    Rename {
        op_id: String,
        item_id: String,
        name: String,
    },
    #[serde(rename_all = "camelCase")]
    Move {
        op_id: String,
        item_id: String,
        to_list_id: String,
    },
    #[serde(rename_all = "camelCase")]
    ClearRecent { op_id: String },
    /// Edit a custom item object everywhere: names (any of `it`, `en`,
    /// `da`), section, icon (`""` clears it). `itemId` is the object id (a
    /// row id of the list works too).
    #[serde(rename_all = "camelCase")]
    EditItem {
        op_id: String,
        item_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        names: Option<std::collections::BTreeMap<String, String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        section: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Reorder {
        op_id: String,
        item_id: String,
        position: u32,
    },
}

impl ShoppingOp {
    pub fn op_id(&self) -> &str {
        match self {
            ShoppingOp::Add { op_id, .. }
            | ShoppingOp::Need { op_id, .. }
            | ShoppingOp::Purchase { op_id, .. }
            | ShoppingOp::Remove { op_id, .. }
            | ShoppingOp::SetSpec { op_id, .. }
            | ShoppingOp::SetQuantity { op_id, .. }
            | ShoppingOp::SetSection { op_id, .. }
            | ShoppingOp::Rename { op_id, .. }
            | ShoppingOp::Move { op_id, .. }
            | ShoppingOp::ClearRecent { op_id }
            | ShoppingOp::EditItem { op_id, .. }
            | ShoppingOp::Reorder { op_id, .. } => op_id,
        }
    }
}

// -- device bridge (docs/device-bridge.md) --------------------------------

/// One event from the owner's devices (`POST /device/events`). Only these
/// fields are forwarded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceEvent {
    /// Client-chosen id (dedup key), `[A-Za-z0-9_.:-]{1,128}`.
    pub id: String,
    /// Epoch milliseconds or an RFC 3339 timestamp.
    pub ts: Scalar,
    /// One of [`crate::validate::DEVICE_EVENT_TYPES`].
    #[serde(rename = "type")]
    pub kind: String,
    /// One of [`crate::validate::DEVICE_SOURCES`].
    pub source: String,
    /// Type-specific payload, a JSON object of at most 8 KiB.
    #[serde(default = "empty_object")]
    pub data: Value,
}

fn empty_object() -> Value {
    Value::Object(Map::new())
}

/// The device that sent a request, added by secretary-server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceRef {
    pub id: String,
    pub name: String,
}

/// `POST /device/events` body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceEvents {
    pub events: Vec<DeviceEvent>,
    /// Set by the server (never trusted from the client).
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceRef>,
}

/// `POST /device/commands/{id}/result` body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The id the device gave the object it created (alarm, reminder, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_object_id: Option<String>,
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceRef>,
}

/// `POST /device/state` body: what is on the device right now. Items are
/// objects passed through opaque; `focus` is the Focus mode (object, string
/// or `null`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceState {
    #[serde(default)]
    pub alarms: Vec<Value>,
    #[serde(default)]
    pub timers: Vec<Value>,
    #[serde(default)]
    pub reminders: Vec<Value>,
    #[serde(default)]
    pub events: Vec<Value>,
    #[serde(default)]
    pub focus: Value,
    /// When the snapshot was taken (epoch ms or RFC 3339).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts: Option<Scalar>,
    #[serde(default, skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceRef>,
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
    /// The LucinaVoice speaker's shared thread ("Lucina Voice").
    Voice,
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
            "voice" => Channel::Voice,
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
            Channel::Voice => "voice".into(),
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

/// `GET /timeline` data: `nextBefore` pages older history; `cursor` tracks
/// arrivals for incremental requests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelinePage {
    #[serde(default)]
    pub items: Vec<TimelineItem>,
    #[serde(default)]
    pub next_before: Option<Scalar>,
    /// Arrival-order id to use as the next `after` cursor.
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_more: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `GET /timeline/conversations` data, with forward-compatible fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineConversations {
    pub items: Vec<TimelineConversation>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineConversation {
    pub key: String,
    pub kind: String,
    pub title: String,
    pub channel: Channel,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
    pub count: u64,
    pub last_id: Scalar,
    pub last_ts: Scalar,
    pub last_role: Role,
    pub last_text: String,
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
    /// Arrival-order cursor: fetch items received after this id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
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
    fn shopping_v2_shapes() {
        let state = json!({
            "list": {"id": "default", "name": "Casa", "theme": "home"}, "revision": 42,
            "items": [{"id": "u1", "catalogId": "banana", "name": "Banane", "spec": "6, mature",
                       "quantity": null, "unit": null, "section": "produce", "icon": "🍌",
                       "status": "needed", "position": 3, "updatedAt": "t", "updatedBy": "telegram",
                       "price": {"eur": 1.2}}],
            "etagHint": "x"
        });
        let s: ListState = serde_json::from_value(state.clone()).unwrap();
        assert_eq!(s.items[0].status, ListItemStatus::Needed);
        assert_eq!(serde_json::to_value(&s).unwrap(), state);
        let s: ListItem = serde_json::from_value(json!({"id": "u", "status": "archived"})).unwrap();
        assert_eq!(s.status, ListItemStatus::Unknown("archived".into()));

        let one =
            json!({"id": "default", "name": "Casa", "theme": "home", "needed": 3, "recent": 9});
        let bare: ShoppingListsData = serde_json::from_value(json!([one])).unwrap();
        let wrapped: ShoppingListsData =
            serde_json::from_value(json!({"lists": [one], "total": 1})).unwrap();
        assert_eq!(bare.into_lists(), wrapped.into_lists());
        let a: ShoppingListData = serde_json::from_value(json!({"list": one})).unwrap();
        let b: ShoppingListData = serde_json::from_value(one.clone()).unwrap();
        assert_eq!(a.into_list(), b.into_list());
    }

    #[test]
    fn chat_request_reply_to_roundtrip() {
        let raw = json!({"message": "e questo?", "sessionId": "iphone", "clientMessageId": "c-9",
                         "channel": "ios", "replyTo": "01K83ZK4Q6X1V9W2D3E4F5G6H7"});
        let req: ChatRequest = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(req.reply_to.as_deref(), Some("01K83ZK4Q6X1V9W2D3E4F5G6H7"));
        assert_eq!(serde_json::to_value(&req).unwrap(), raw);
        // Absent: not serialized at all, so the forwarded body is unchanged.
        let raw = json!({"message": "ciao"});
        let req: ChatRequest = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(req.reply_to, None);
        assert_eq!(serde_json::to_value(&req).unwrap(), raw);
    }

    #[test]
    fn device_chat_and_incremental_page_shapes() {
        let req: ChatRequest = serde_json::from_value(json!({"message": "hi"})).unwrap();
        assert!(req.device.is_none());
        assert!(req.device_name.is_none());
        let body = serde_json::to_value(req).unwrap();
        assert!(body.get("device").is_none());
        assert!(body.get("deviceName").is_none());
        let raw = json!({"items": [], "nextBefore": null, "cursor": "arrival_1",
            "hasMore": true, "futurePage": 42});
        let page: TimelinePage = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(page.cursor.as_deref(), Some("arrival_1"));
        assert_eq!(page.has_more, Some(true));
        assert!(!page.extra.contains_key("cursor"));
        assert!(!page.extra.contains_key("hasMore"));
        assert_eq!(serde_json::to_value(page).unwrap(), raw);
        let old: TimelinePage = serde_json::from_value(json!({"items": []})).unwrap();
        assert_eq!(old.cursor, None);
        assert_eq!(old.has_more, None);
        assert!(serde_json::from_value::<TimelinePage>(json!({"cursor": 1})).is_err());
        assert!(serde_json::from_value::<TimelinePage>(json!({"hasMore": "yes"})).is_err());
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
