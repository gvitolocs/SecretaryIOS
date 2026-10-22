//! Stored state, the command envelope and the views.
//!
//! The stored model follows Todoist's API v1 objects (ids are Todoist's
//! strings, `priority` is the API value where 4 = p1) so a sync round-trip
//! keeps every field the owner set, recurrence strings included. Fields only
//! this engine uses are marked "local".

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A due date as Todoist stores it.
///
/// `date` is `YYYY-MM-DD` (all day), `YYYY-MM-DDTHH:MM:SS[.ffffff]`
/// (floating, the user's zone) or `…Z` (fixed, UTC, with `timezone`).
/// `string` is the human text ("every day", "ogni lunedì alle 9") and is
/// what makes a recurring task recur: it is kept verbatim.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Due {
    pub date: String,
    #[serde(default)]
    pub string: String,
    #[serde(default)]
    pub lang: String,
    #[serde(default)]
    pub is_recurring: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskDuration {
    pub amount: u32,
    /// `minute` or `day`.
    pub unit: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    #[serde(default)]
    pub section_id: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub content: String,
    #[serde(default)]
    pub description: String,
    /// Todoist API priority: 1 (normal, p4) … 4 (urgent, p1).
    #[serde(default = "one")]
    pub priority: u8,
    #[serde(default)]
    pub due: Option<Due>,
    /// `YYYY-MM-DD`.
    #[serde(default)]
    pub deadline: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub child_order: i64,
    #[serde(default)]
    pub day_order: i64,
    #[serde(default)]
    pub checked: bool,
    #[serde(default)]
    pub is_collapsed: bool,
    #[serde(default)]
    pub duration: Option<TaskDuration>,
    #[serde(default)]
    pub added_at: Option<String>,
    /// Todoist's `updated_at`: the conflict base of local edits.
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    /// Local: soft-deleted at (undo window, then `item_delete`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_ms: Option<u64>,
    /// Local: the due date is this engine's guess after completing a
    /// recurring task; Todoist's answer replaces it at the next sync.
    #[serde(default, skip_serializing_if = "is_false")]
    pub provisional: bool,
}

fn one() -> u8 {
    1
}
fn is_false(b: &bool) -> bool {
    !*b
}

impl Task {
    pub fn is_local(&self) -> bool {
        self.id.starts_with(TEMP_PREFIX)
    }
    pub fn recurring(&self) -> bool {
        self.due.as_ref().is_some_and(|d| d.is_recurring)
    }
    /// The user-facing priority: 1 (urgent) … 4 (none).
    pub fn p(&self) -> u8 {
        5 - self.priority.clamp(1, 4)
    }
}

/// Ids of tasks created here and not yet acknowledged by Todoist. Todoist
/// treats `tmp-` ids as client placeholders (and resolves them as `temp_id`s
/// inside one sync request).
pub const TEMP_PREFIX: &str = "tmp-";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Project {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub child_order: i64,
    #[serde(default)]
    pub inbox_project: bool,
    #[serde(default)]
    pub is_favorite: bool,
    #[serde(default)]
    pub view_style: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Section {
    pub id: String,
    pub project_id: String,
    pub name: String,
    #[serde(default)]
    pub section_order: i64,
    #[serde(default)]
    pub is_collapsed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Label {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub item_order: i64,
    #[serde(default)]
    pub is_favorite: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Filter {
    pub id: String,
    pub name: String,
    pub query: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub item_order: i64,
    #[serde(default)]
    pub is_favorite: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Settings {
    /// Tasks per day (Todoist karma goal).
    pub daily_goal: u32,
    #[serde(default)]
    pub weekly_goal: u32,
    /// Todoist's language for parsing due strings.
    #[serde(default = "en")]
    pub lang: String,
    /// 1 = Monday … 7 = Sunday.
    #[serde(default = "monday")]
    pub start_day: u8,
    /// The weekday "next week" means (1 = Monday).
    #[serde(default = "monday")]
    pub next_week: u8,
    /// Todoist account time zone, when known.
    #[serde(default)]
    pub timezone: Option<String>,
}

fn en() -> String {
    "en".into()
}
fn monday() -> u8 {
    1
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            daily_goal: 5,
            weekly_goal: 25,
            lang: en(),
            start_day: 1,
            next_week: 1,
            timezone: None,
        }
    }
}

/// One completion (here or in any Todoist client).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Completed {
    pub task_id: String,
    pub content: String,
    pub project_id: String,
    pub completed_ms: u64,
    #[serde(default)]
    pub recurring: bool,
    /// `local` until Todoist reports it, then `todoist`.
    pub source: String,
    /// Local completions keep the task so `uncomplete` can restore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Task>,
    /// Sub-tasks completed with it (restored by `uncomplete`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Task>,
    /// For recurring completions: the due date before completing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_due: Option<Due>,
}

/// A queued Todoist command (`/api/v1/sync` `commands`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Op {
    /// Todoist's command `uuid` (idempotent on their side too).
    pub uuid: String,
    /// `item_add`, `item_update`, `item_close`, …
    #[serde(rename = "type")]
    pub kind: String,
    pub args: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_id: Option<String>,
    /// The task it touches (the conflict key).
    #[serde(default)]
    pub task_id: Option<String>,
    /// Todoist's `updated_at` of that task when the edit was made.
    #[serde(default)]
    pub base_updated_at: Option<String>,
    pub created_ms: u64,
    /// Not pushed before this time (deletes wait out the undo window).
    #[serde(default)]
    pub not_before_ms: u64,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub last_error: Option<String>,
    /// The task should still recur after this edit.
    #[serde(default, skip_serializing_if = "is_false")]
    pub expect_recurring: bool,
}

/// A visible note: Todoist won a conflict or refused an edit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Conflict {
    pub id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    pub content: String,
    /// `changed_remotely`, `deleted_remotely`, `rejected`, `recurrence_lost`.
    pub kind: String,
    pub message: String,
    pub at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SyncStatus {
    /// No Todoist account linked: tasks stay local.
    #[default]
    Disconnected,
    Ok,
    Syncing,
    /// Network or Todoist error; retried with backoff.
    Error,
    /// Todoist answered 429; retried after `retry_after_ms`.
    RateLimited,
    /// The token was revoked: reconnect in Links.
    Reconnect,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SyncMeta {
    #[serde(default)]
    pub status: SyncStatus,
    #[serde(default)]
    pub sync_token: Option<String>,
    #[serde(default)]
    pub last_sync_ms: Option<u64>,
    #[serde(default)]
    pub last_full_sync_ms: Option<u64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub retry_after_ms: Option<u64>,
    /// Todoist's completed count for a date (`stats.days_items`).
    #[serde(default)]
    pub remote_completed: Option<(String, u32)>,
    /// The Todoist account (email or name), for display.
    #[serde(default)]
    pub account: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    pub schema_version: u32,
    pub revision: u64,
    #[serde(default)]
    pub tasks: BTreeMap<String, Task>,
    #[serde(default)]
    pub projects: BTreeMap<String, Project>,
    #[serde(default)]
    pub sections: BTreeMap<String, Section>,
    #[serde(default)]
    pub labels: BTreeMap<String, Label>,
    #[serde(default)]
    pub filters: BTreeMap<String, Filter>,
    #[serde(default)]
    pub settings: Settings,
    /// Newest last; trimmed to [`crate::MAX_COMPLETED`].
    #[serde(default)]
    pub completed: Vec<Completed>,
    /// Deleted tasks already removed on Todoist (restorable as new tasks).
    #[serde(default)]
    pub trash: Vec<Task>,
    #[serde(default)]
    pub outbox: Vec<Op>,
    #[serde(default)]
    pub conflicts: Vec<Conflict>,
    #[serde(default)]
    pub sync: SyncMeta,
    #[serde(default)]
    pub receipts: BTreeMap<String, String>,
}

impl State {
    pub fn initial() -> Self {
        Self {
            schema_version: 1,
            revision: 0,
            tasks: BTreeMap::new(),
            projects: BTreeMap::new(),
            sections: BTreeMap::new(),
            labels: BTreeMap::new(),
            filters: BTreeMap::new(),
            settings: Settings::default(),
            completed: vec![],
            trash: vec![],
            outbox: vec![],
            conflicts: vec![],
            sync: SyncMeta::default(),
            receipts: BTreeMap::new(),
        }
    }

    pub fn inbox_id(&self) -> Option<&str> {
        self.projects
            .values()
            .find(|p| p.inbox_project)
            .map(|p| p.id.as_str())
    }
}

// -- protocol ----------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Request {
    #[serde(default)]
    pub op_id: Option<String>,
    /// Optional for tasks: when given it must match (409 `conflict`). Task
    /// commands each target one task, so callers such as Lucina may omit it.
    #[serde(default)]
    pub expected_revision: Option<u64>,
    #[serde(default = "utc")]
    pub timezone: String,
    pub command: Command,
}
fn utc() -> String {
    "UTC".into()
}

/// What a snapshot shows besides the sidebar.
#[derive(Clone, Debug, Deserialize, Serialize, Default, PartialEq, Eq)]
pub struct ViewQuery {
    /// `inbox` | `today` | `upcoming` | `project` | `label` | `filter` |
    /// `search` | `completed` (default `today`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Project id, label name or filter id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// A Todoist filter query (`kind=filter` without `id`) or the search text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    /// Upcoming / completed range in days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days: Option<u32>,
}

/// Optional command fields are skipped when absent, so a command's
/// serialization (its receipt fingerprint) depends only on what was sent.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    Snapshot {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ViewQuery>,
    },
    /// Preview of quick add parsing (chips while typing).
    ParseQuickAdd {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lang: Option<String>,
    },
    Add {
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        section_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
        /// Natural language ("tomorrow 9am", "every day", "ogni lunedì").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_string: Option<String>,
        /// `YYYY-MM-DD` or `YYYY-MM-DDTHH:MM`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_date: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lang: Option<String>,
        /// User-facing 1 (urgent) … 4.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        priority: Option<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        labels: Option<Vec<String>>,
    },
    /// One line, Todoist quick add syntax: dates, `#project`, `/section`,
    /// `@label`, `p1`…`p4`, `// description`.
    QuickAdd {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lang: Option<String>,
        /// Default project when the text names none (the open project view).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        section_id: Option<String>,
        /// Default due when the text has none (`today` from the Today view).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default_due: Option<String>,
    },
    Complete {
        task_id: String,
    },
    Uncomplete {
        task_id: String,
    },
    Update {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        priority: Option<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        labels: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_string: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_date: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lang: Option<String>,
        #[serde(default, skip_serializing_if = "is_false")]
        clear_due: bool,
    },
    /// Move the due date, keeping the time and the recurrence.
    Reschedule {
        task_id: String,
        /// `YYYY-MM-DD`, or a phrase (`tomorrow`, `next week`, `domani`).
        date: String,
    },
    /// Todoist's "Reschedule" on the Overdue group (default: today).
    RescheduleOverdue {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        date: Option<String>,
    },
    /// Soft delete: hidden at once, sent to Todoist after the undo window.
    Delete {
        task_id: String,
    },
    UndoDelete {
        task_id: String,
    },
    /// Exactly one of the three destinations.
    Move {
        task_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        section_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_id: Option<String>,
    },
    SetDailyGoal {
        goal: u32,
    },
    DismissConflict {
        conflict_id: String,
    },
}

impl Command {
    pub fn is_query(&self) -> bool {
        matches!(self, Self::Snapshot { .. } | Self::ParseQuickAdd { .. })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DueView {
    pub date: String,
    /// `HH:MM` in the request zone, for timed dues.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<String>,
    pub string: String,
    pub is_recurring: bool,
    /// Days from today (negative = overdue).
    pub rel_days: i64,
    pub overdue: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub provisional: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskView {
    pub id: String,
    pub content: String,
    pub description: String,
    pub project_id: String,
    pub project_name: String,
    pub project_color: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_content: Option<String>,
    /// User-facing: 1 (urgent, red) … 4 (none).
    pub priority: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<DueView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline: Option<String>,
    pub labels: Vec<String>,
    pub checked: bool,
    pub subtasks: u32,
    pub subtasks_done: u32,
    /// Edits not yet confirmed by Todoist.
    pub pending: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_minutes: Option<u32>,
    /// Nested sub-tasks (project, inbox, label and filter views).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<TaskView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Group {
    /// `overdue`, a date `YYYY-MM-DD`, a section id, or `none`.
    pub key: String,
    /// `overdue` | `date` | `section` | `none` | `completed`.
    pub kind: String,
    /// Section name; dates and "Overdue" are localized by the shells.
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rel_days: Option<i64>,
    pub tasks: Vec<TaskView>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListView {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Project / label / filter name, or the search text.
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    pub groups: Vec<Group>,
    pub count: u32,
    /// The filter query used words this engine does not evaluate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectView {
    pub id: String,
    pub name: String,
    pub color: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub inbox: bool,
    pub favorite: bool,
    pub count: u32,
    pub sections: Vec<SectionView>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SectionView {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NamedCount {
    pub id: String,
    pub name: String,
    pub color: String,
    pub favorite: bool,
    pub count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Counts {
    pub inbox: u32,
    pub today: u32,
    pub overdue: u32,
    pub upcoming: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Goal {
    pub daily_goal: u32,
    pub completed_today: u32,
    pub reached: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SyncView {
    pub status: SyncStatus,
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_sync_ms: Option<u64>,
    pub pending: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct View {
    pub revision: u64,
    /// Today in the request zone.
    pub today: String,
    pub timezone: String,
    pub counts: Counts,
    pub goal: Goal,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inbox_id: Option<String>,
    pub projects: Vec<ProjectView>,
    pub labels: Vec<NamedCount>,
    pub filters: Vec<NamedCount>,
    pub sync: SyncView,
    pub conflicts: Vec<Conflict>,
    /// Soft-deleted tasks still inside the undo window.
    pub undo: Vec<String>,
    pub list: ListView,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    /// `date` | `project` | `section` | `label` | `priority` | `description`.
    pub kind: String,
}

/// What quick add understood.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct Parsed {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The date phrase exactly as typed (sent to Todoist as `due.string`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_string: Option<String>,
    /// This engine's reading of it (Todoist's parser has the last word).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    pub is_recurring: bool,
    /// User-facing 1…4.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    pub lang: String,
    /// Byte ranges in the input (for highlighting).
    pub spans: Vec<Span>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub replayed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<View>,
    /// `parse_quick_add`, `quick_add`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed: Option<Parsed>,
    /// The id of a task created by `add` / `quick_add` (a `tmp-` id until
    /// Todoist confirms it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// The command queued work for Todoist (the server syncs soon).
    #[serde(skip_serializing_if = "is_false", default)]
    pub queued: bool,
}
