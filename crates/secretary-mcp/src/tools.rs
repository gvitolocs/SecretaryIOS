//! The MCP tools: definitions (JSON Schemas, annotations, scopes) and their
//! implementations against secretary-server.
//!
//! Safety: no tool buys anything, messages third parties or deletes data.
//! `reminder_cancel` only moves a pending reminder to `cancelled` (Lucina
//! keeps the record). Everything else that could act in the world goes
//! through dedicated tools, i.e. through Lucina's own pipeline and its own
//! confirmation flow. Notes are the owner's own local data: `note_save` and
//! `note_append` write without a confirmation step, and every write is
//! logged with the caller.

use std::sync::Arc;

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::backend::{seg, BackendError};
use crate::format;
use crate::keep::{self, ItemRef};
use crate::scope::{Caller, Scope};
use crate::App;

/// Bounded library listing/search arguments.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LibraryListArgs {
    /// Content kinds to list; default book and document (dictionaries excluded).
    pub kind: Option<LibraryKind>,
    pub q: Option<String>,
    pub status: Option<LibraryStatus>,
    pub on_kindle: Option<bool>,
    pub limit: Option<u32>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum LibraryKind {
    Book,
    Dictionary,
    Document,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LibraryStatus {
    Unknown,
    NotStarted,
    Reading,
    Completed,
    Abandoned,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LibraryBookArgs {
    pub id: String,
    pub q: Option<String>,
    pub limit: Option<u32>,
}
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LibraryRecentArgs {
    pub days: Option<u32>,
}

// -- arguments ------------------------------------------------------------------

/// Arguments of `ask_lucina`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskArgs {
    /// What to say to Lucina, in any language (she answers in the language you write in). Up to 12000 characters. To hand over a long document or briefing, save it with note_save instead and tell Lucina its slug.
    pub message: String,
    /// Optional conversation name (letters, digits, '-' and '_', up to 32) to keep separate threads, e.g. "travel". Default: one conversation per client.
    #[serde(default)]
    pub conversation: Option<String>,
    /// Permit this one message to act on these request kinds; duplicates are removed and kinds sorted.
    pub allow: Option<Vec<AllowArg>>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AllowArg {
    Reminders,
    Notes,
    Tasks,
    Shopping,
}
impl AllowArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Reminders => "reminders",
            Self::Notes => "notes",
            Self::Tasks => "tasks",
            Self::Shopping => "shopping",
        }
    }
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ImpactArg {
    Low,
    Medium,
    High,
}
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum UrgencyArg {
    Low,
    Normal,
    High,
}

/// A structured go-ahead request. The authenticated client name is supplied by the server.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecideArgs {
    /// The decision to make, 1-2000 characters.
    pub question: String,
    /// 2-6 choices, each 1-200 characters; unique ignoring case and surrounding whitespace.
    pub options: Vec<String>,
    /// Recommended option text (case-insensitive, trimmed) or its 1-based number, e.g. "2".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommendation: Option<String>,
    /// Supporting context, up to 8000 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Whether the proposed action can be undone. False always requires the owner.
    pub reversible: bool,
    /// Consequence level. High always requires the owner.
    pub impact: ImpactArg,
    /// How urgent the decision is; default normal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub urgency: Option<UrgencyArg>,
    /// Subject grouping, up to 64 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// Asking session name or id, up to 120 characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asking_session: Option<String>,
    /// Poll a pending answer every 3 seconds for 0-100 seconds; default 0.
    #[serde(skip_serializing)]
    pub wait_seconds: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionStatusArgs {
    /// Decision id: dc- followed by 12 lowercase hexadecimal digits.
    pub decision_id: String,
    /// Poll a pending answer every 3 seconds for 0-100 seconds; default 0.
    pub wait_seconds: Option<u32>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionWithdrawArgs {
    /// Decision id from lucina_decide: dc- followed by 12 lowercase hexadecimal digits.
    pub decision_id: String,
    /// Why it no longer needs an answer, shown to the owner; at most 500 characters.
    pub reason: Option<String>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JobStatusArgs {
    /// Background job id returned by ask_lucina.
    pub job_id: String,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChannelArg {
    Telegram,
    Discord,
    Web,
    Ios,
    Alexa,
    Other,
}

impl ChannelArg {
    fn as_str(self) -> &'static str {
        match self {
            ChannelArg::Telegram => "telegram",
            ChannelArg::Discord => "discord",
            ChannelArg::Web => "web",
            ChannelArg::Ios => "ios",
            ChannelArg::Alexa => "alexa",
            ChannelArg::Other => "other",
        }
    }
}

/// Arguments of `lucina_timeline`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct TimelineArgs {
    /// How many messages to return, newest first (1-100, default 20).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Pagination cursor: the `next_before` value printed at the end of the previous page.
    #[serde(default)]
    pub before: Option<String>,
    /// Only messages from this channel.
    #[serde(default)]
    pub channel: Option<ChannelArg>,
}

/// Arguments of `shopping_list`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ShoppingListArgs {
    /// List id or name (case-insensitive). Default: the default list ("default").
    #[serde(default)]
    pub list: Option<String>,
}

/// One item to add: a plain name, or a name with details.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AddItem {
    /// Item name as you would say it, e.g. "latte" or "pane integrale".
    Name(String),
    Detailed {
        /// Item name, 1-200 characters.
        name: String,
        /// Free-text detail, e.g. "6, mature" or "senza lattosio" (up to 200 characters).
        #[serde(default)]
        spec: Option<String>,
        /// Amount (> 0).
        #[serde(default)]
        quantity: Option<f64>,
        /// Unit for the amount, e.g. "kg", "l", "pz".
        #[serde(default)]
        unit: Option<String>,
    },
}

/// Arguments of `shopping_add`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShoppingAddArgs {
    /// 1-50 items to put on the list. Adding an item that was bought recently puts it back on the list.
    pub items: Vec<AddItem>,
    /// List id or name. Default: the default list.
    #[serde(default)]
    pub list: Option<String>,
}

/// Arguments of `shopping_mark_bought`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MarkBoughtArgs {
    /// The item's id (from shopping_list) or its name.
    pub item: String,
    /// List id or name. Default: the default list.
    #[serde(default)]
    pub list: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReminderScopeArg {
    /// Every pending reminder (overdue undelivered ones included).
    Upcoming,
    /// Pending reminders due today.
    Today,
    /// Same as upcoming; with include_sent also sent and cancelled ones.
    All,
}

/// Arguments of `reminders_list`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RemindersListArgs {
    /// Which reminders (default upcoming).
    #[serde(default)]
    pub scope: Option<ReminderScopeArg>,
    /// Also list delivered and cancelled reminders (default false).
    #[serde(default)]
    pub include_sent: Option<bool>,
}

/// Arguments of `reminder_create`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReminderCreateArgs {
    /// What to remind the owner of, 1-500 characters, e.g. "chiamare il dentista".
    pub text: String,
    /// When. Best: an ISO 8601 / RFC 3339 timestamp with offset in the future, e.g. "2026-10-22T09:30:00+02:00" (the owner's time zone is shown by lucina_status). Natural phrases ("domani alle 9:30", "in 2 hours") are passed to Lucina's parser and may be refused.
    pub when: String,
}

/// Arguments of `reminder_cancel`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReminderCancelArgs {
    /// The reminder id from reminders_list.
    pub id: String,
}

/// Arguments of `calendar_list`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct CalendarListArgs {
    /// First day, YYYY-MM-DD in the owner's time zone (default: today), or an RFC 3339 time.
    #[serde(default)]
    pub from: Option<String>,
    /// Last day (inclusive), YYYY-MM-DD, or an RFC 3339 time. Default: `from` + `days`.
    #[serde(default)]
    pub to: Option<String>,
    /// How many days from `from` when `to` is not given (1-62, default 7).
    #[serde(default)]
    pub days: Option<u32>,
}

/// Arguments of `calendar_create`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CalendarCreateArgs {
    /// Event title, 1-300 characters.
    pub title: String,
    /// Start: an RFC 3339 time with offset ("2026-10-23T09:30:00+02:00"), or YYYY-MM-DD for an all-day event.
    pub start: String,
    /// End (optional): same format as start; all-day ends are exclusive dates. Default: one hour / one day.
    #[serde(default)]
    pub end: Option<String>,
    /// Where (optional).
    #[serde(default)]
    pub location: Option<String>,
    /// Notes (optional).
    #[serde(default)]
    pub description: Option<String>,
}

/// Arguments of `gym_summary`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GymArgs {
    /// How many recent workouts to include (1-20, default 5).
    #[serde(default)]
    pub workouts: Option<u32>,
}

/// No arguments.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NoArgs {}

/// One checklist item: plain text, or text with its state.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ItemArg {
    Text(String),
    Item {
        text: String,
        #[serde(default)]
        checked: Option<bool>,
    },
}

/// A checklist item of an existing note: its number as note_get shows it (from 1), or its exact text.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ItemRefArg {
    Number(u32),
    Text(String),
}

/// Arguments of `note_save`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NoteSaveArgs {
    /// Title shown in the app, 1-200 characters, e.g. "CRAFT-IT4SD — meeting with Marianne & Jonas".
    pub title: String,
    /// The note in Markdown (headings, lists, links). Up to 256 KB in total; for very long documents send the first part here and the rest with note_append. Optional when `items` is given.
    #[serde(default)]
    pub content: Option<String>,
    /// Make it a checklist (a Keep list note): strings, or {text, checked}. The note's content becomes the `- [ ]` lines.
    #[serde(default)]
    pub items: Option<Vec<ItemArg>>,
    /// Short name to find it again, e.g. "craftit-meeting": lower-case letters, digits and '-', up to 64. Default: made from the title. Lucina and note_get find the note by this name.
    #[serde(default)]
    pub slug: Option<String>,
    /// Up to 50 tags (the same thing as labels), e.g. ["work", "aarhus"].
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Keep labels by display name, e.g. ["GodLevel", "Fisica 2"] (created when new). Same field as `tags`.
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    /// Keep colour: default, red, orange, yellow, green, teal, blue, cerulean, purple, pink, brown or gray.
    #[serde(default)]
    pub color: Option<String>,
    /// Pin it to the top of the owner's notes (default false).
    #[serde(default)]
    pub pinned: Option<bool>,
    /// Put it in the archive (default false).
    #[serde(default)]
    pub archived: Option<bool>,
    /// When a note with this slug exists: true replaces its title, content (and tags, items, colour, pin, archive if given); false (default) refuses so nothing is overwritten by accident.
    #[serde(default)]
    pub replace: Option<bool>,
}

/// Repeat of a note reminder.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum RepeatArg {
    Daily,
    Weekly,
    Monthly,
    Yearly,
    /// Not repeating.
    None,
}

/// Convert a note between checklist and text.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ConvertArg {
    /// Each non-empty line becomes an unchecked item.
    List,
    /// The items become plain lines (checked state is lost).
    Text,
}

fn nullable<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// Arguments of `note_update`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct NoteUpdateArgs {
    /// The note's slug, e.g. "shopping" (or give `id`).
    #[serde(default)]
    pub slug: Option<String>,
    /// The note's id ("n_…"), instead of `slug`.
    #[serde(default)]
    pub id: Option<String>,
    /// New title.
    #[serde(default)]
    pub title: Option<String>,
    /// Replace the whole content (for a checklist note the lines become the items; prefer the item operations below).
    #[serde(default)]
    pub content: Option<String>,
    /// Keep colour: default, red, orange, yellow, green, teal, blue, cerulean, purple, pink, brown or gray.
    #[serde(default)]
    pub color: Option<String>,
    /// Pin or unpin.
    #[serde(default)]
    pub pinned: Option<bool>,
    /// Archive (true) or bring back from the archive (false).
    #[serde(default)]
    pub archived: Option<bool>,
    /// Labels to add, by display name (created when new).
    #[serde(default)]
    pub add_labels: Option<Vec<String>>,
    /// Labels to remove, by name.
    #[serde(default)]
    pub remove_labels: Option<Vec<String>>,
    /// Set the note's reminder (RFC 3339 with offset, e.g. 2026-11-03T09:00:00+01:00; Lucina delivers it), or null to clear it.
    #[serde(default, deserialize_with = "nullable")]
    #[schemars(with = "Option<String>")]
    pub reminder_at: Option<Option<String>>,
    /// Repeat of the reminder: daily, weekly, monthly, yearly or none. Default: keep the current one.
    #[serde(default)]
    pub reminder_repeat: Option<RepeatArg>,
    /// Check off these checklist items: numbers as note_get shows them, or exact item texts.
    #[serde(default)]
    pub check: Option<Vec<ItemRefArg>>,
    /// Uncheck these checklist items (numbers or exact texts).
    #[serde(default)]
    pub uncheck: Option<Vec<ItemRefArg>>,
    /// Add these items at the end of the checklist: strings, or {text, checked}.
    #[serde(default)]
    pub add_items: Option<Vec<ItemArg>>,
    /// Remove these checklist items (numbers or exact texts).
    #[serde(default)]
    pub remove_items: Option<Vec<ItemRefArg>>,
    /// Turn the note into a checklist ("list") or back into plain text ("text").
    #[serde(default)]
    pub convert: Option<ConvertArg>,
}

/// Arguments of `note_append`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NoteAppendArgs {
    /// The note's slug, e.g. "craftit-meeting" (or give `id`).
    #[serde(default)]
    pub slug: Option<String>,
    /// The note's id ("n_…"), instead of `slug`.
    #[serde(default)]
    pub id: Option<String>,
    /// Markdown to add at the end.
    pub content: String,
    /// true: glue the text on exactly (continuing a chunk that was cut mid-sentence); false (default): start it as a new paragraph.
    #[serde(default)]
    pub continuation: Option<bool>,
}

/// Arguments of `note_get`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NoteGetArgs {
    /// The note's slug, e.g. "craftit-meeting" (or give `id`).
    #[serde(default)]
    pub slug: Option<String>,
    /// The note's id ("n_…"), instead of `slug`.
    #[serde(default)]
    pub id: Option<String>,
    /// Start at this character (for long notes; default 0).
    #[serde(default)]
    pub offset: Option<usize>,
    /// Return at most this many characters (1000-100000, default 40000).
    #[serde(default)]
    pub max_chars: Option<usize>,
}

/// Which notes of the archive to include.
#[derive(Debug, Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ArchivedArg {
    Include,
    Exclude,
    Only,
}

/// What kind of notes.
#[derive(Debug, Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum NoteKindArg {
    List,
    Text,
    Image,
    Audio,
    Link,
    Reminder,
}

/// Arguments of `notes_search`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NotesSearchArgs {
    /// Words to look for in titles, slugs, labels, content and checklist items. Empty: list the latest notes.
    #[serde(default)]
    pub q: Option<String>,
    /// Only notes with this tag (same as `label`).
    #[serde(default)]
    pub tag: Option<String>,
    /// Only notes with this label, e.g. "GodLevel" (see note_labels).
    #[serde(default)]
    pub label: Option<String>,
    /// Only notes of this Keep colour.
    #[serde(default)]
    pub color: Option<String>,
    /// Only this kind: list (checklist), text, image, audio, link or reminder.
    #[serde(default, rename = "type")]
    pub kind: Option<NoteKindArg>,
    /// Archived notes: include (default), exclude or only.
    #[serde(default)]
    pub archived: Option<ArchivedArg>,
    /// How many to return (1-50, default 20).
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Arguments of `tasks_list`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct TasksListArgs {
    /// Which list: "today" (default: overdue + today, like Todoist's Today), "upcoming" (next 7 days), "inbox", "completed" (last 7 days), or "search" (with `query`).
    #[serde(default)]
    pub view: Option<String>,
    /// Only this Todoist project, by name (e.g. "Lingue").
    #[serde(default)]
    pub project: Option<String>,
    /// Words to search for (view "search"), or a Todoist filter such as "p1 & overdue" or "@lingue".
    #[serde(default)]
    pub query: Option<String>,
}

/// Arguments of `task_add`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskAddArgs {
    /// The task in Todoist quick add style, any of Italian, English or Danish: "15 min shadowing every day", "chiamare Marco domani alle 9 #Lavoro p1", "ring til tandlægen i morgen @home". Dates, recurrence, #Project, @label and p1-p4 are understood.
    pub text: String,
    /// Due date when the text has none, e.g. "today" (default: no date).
    #[serde(default)]
    pub due: Option<String>,
}

/// Arguments of `task_complete`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct TaskCompleteArgs {
    /// The task id from tasks_list.
    #[serde(default)]
    pub id: Option<String>,
    /// Or words of the title, e.g. "shadowing": must match exactly one open task due today or overdue (or any open task when nothing due matches).
    #[serde(default)]
    pub title: Option<String>,
}

// -- definitions ------------------------------------------------------------------

pub struct ToolSpec {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub scope: Scope,
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    schema: fn() -> Tool,
}

/// A second scope that also allows a tool: every coding agent (scope `agents`)
/// reports to and reads Lucina's ops record, and looks things up in AU Find
/// and the caller screen.
pub fn also_allowed(name: &str) -> Option<Scope> {
    (name.starts_with("ops_") || name.starts_with("au_") || name == "caller_screen")
        .then_some(Scope::Agents)
}

impl ToolSpec {
    /// Whether a caller with these scopes may list and call the tool.
    pub fn allowed(&self, has: impl Fn(Scope) -> bool) -> bool {
        has(self.scope) || also_allowed(self.name).is_some_and(&has)
    }

    pub fn tool(&self) -> Tool {
        (self.schema)().with_title(self.title).with_annotations(
            ToolAnnotations::with_title(self.title)
                .read_only(self.read_only)
                .destructive(self.destructive)
                .idempotent(self.idempotent)
                .open_world(false),
        )
    }
}

macro_rules! spec {
    ($name:literal, $title:literal, $scope:expr, ro = $ro:expr, destructive = $d:expr, idempotent = $i:expr, $args:ty, $desc:expr) => {
        ToolSpec {
            name: $name,
            title: $title,
            description: $desc,
            scope: $scope,
            read_only: $ro,
            destructive: $d,
            idempotent: $i,
            schema: || {
                Tool::new($name, $desc, Arc::new(JsonObject::new())).with_input_schema::<$args>()
            },
        }
    };
}

pub fn specs() -> Vec<ToolSpec> {
    vec![
        spec!("ask_lucina", "Ask Lucina", Scope::Chat, ro = false, destructive = false, idempotent = false, AskArgs,
            "Send a message to Lucina, the owner's personal AI assistant, and get her reply. Lucina knows the owner (long-term memory), their reminders, shopping list, notes and recent conversations on every channel, and recent context. For agent clients this is consult-only: answers, no jobs, computer tasks, reminders, posts or list writes. For a go-ahead use lucina_decide. For actions use reminder_create, shopping_add, note_save, task_add, computer_task or agent_run. The optional allow array lets this one message act on that kind of request. She is told which client is asking. Actions with consequences (purchases, posts, messages to other people) go through Lucina's own confirmation flow. Messages are limited to 12000 characters: do NOT paste long documents, briefings or context here; save them with note_save (and note_append) and then tell Lucina the note's slug. A turn can take up to two minutes; long jobs finish in the background and show up in lucina_timeline."),
        spec!("lucina_decide", "Ask Lucina for a decision", Scope::Chat, ro = false, destructive = false, idempotent = false, DecideArgs,
            "THE way for agents to get a go-ahead: use this instead of asking the owner or using ask_lucina. Lucina decides from the owner's memory, messages and earlier decisions with JEV. She asks the owner on his phone for high impact, irreversible or owner-only requests, conflicting evidence or low confidence. A pending answer means do not act yet; follow it with lucina_decision_status. Optionally wait and poll for up to 100 seconds."),
        spec!("lucina_decision_status", "Lucina decision status", Scope::Chat, ro = true, destructive = false, idempotent = true, DecisionStatusArgs,
            "Check a structured go-ahead from lucina_decide, optionally polling for up to 100 seconds. Pending means do not act yet; decided and expired answers include the full decision record."),
        spec!("lucina_decision_withdraw", "Withdraw a Lucina decision", Scope::Chat, ro = false, destructive = false, idempotent = true, DecisionWithdrawArgs,
            "Take back a decision you asked with lucina_decide that is still pending (asked by mistake, settled elsewhere). The owner's buttons then answer that it was withdrawn."),
        spec!("lucina_job_status", "Lucina job status", Scope::Chat, ro = true, destructive = false, idempotent = true, JobStatusArgs,
            "Follow a background job started by Lucina: status, action, result or error, and created, started and finished times. For go-aheads use lucina_decide; ask_lucina is answers-only for agents."),
        spec!("lucina_timeline", "Lucina timeline", Scope::Read, ro = true, destructive = false, idempotent = true, TimelineArgs,
            "Recent messages between the owner and Lucina on all channels (Telegram, Discord, web app, iOS app, Alexa, MCP), newest first, with time, channel and who spoke. Paginate with `before`."),
        spec!("shopping_list", "Shopping list", Scope::Read, ro = true, destructive = false, idempotent = true, ShoppingListArgs,
            "Show a shopping list: what is still needed (with item ids) and what was bought recently. Also lists the other lists by name."),
        spec!("shopping_add", "Add to shopping list", Scope::Write, ro = false, destructive = false, idempotent = false, ShoppingAddArgs,
            "Put items on the owner's shopping list (default list unless `list` is given). Items can be plain names or {name, spec, quantity, unit}. Lucina checks each item; one that doesn't look like a shopping item is not added and her question is returned for the owner."),
        spec!("shopping_mark_bought", "Mark item bought", Scope::Write, ro = false, destructive = false, idempotent = true, MarkBoughtArgs,
            "Tick off one shopping list item as bought (it moves to 'recently bought', nothing is deleted). Identify it by id or by name."),
        spec!("reminders_list", "List reminders", Scope::Read, ro = true, destructive = false, idempotent = true, RemindersListArgs,
            "The owner's reminders (delivered by Lucina on Telegram) with ids, due times in the owner's time zone and status."),
        spec!("reminder_create", "Create reminder", Scope::Write, ro = false, destructive = false, idempotent = true, ReminderCreateArgs,
            "Create a reminder that Lucina delivers to the owner on Telegram at the given time. An identical pending reminder is not duplicated."),
        spec!("reminder_cancel", "Cancel reminder", Scope::Write, ro = false, destructive = true, idempotent = true, ReminderCancelArgs,
            "Cancel a pending reminder so it is not delivered. The reminder is kept (status 'cancelled'), not deleted. Confirm with the owner before cancelling."),
        spec!("calendar_list", "Calendar", Scope::Read, ro = true, destructive = false, idempotent = true, CalendarListArgs,
            "The owner's calendar: events from Google Calendar, the iPhone calendar and subscribed ICS calendars, merged, with times in the owner's time zone and event ids. Default: the next 7 days from today."),
        spec!("calendar_create", "Propose calendar event", Scope::Write, ro = false, destructive = false, idempotent = false, CalendarCreateArgs,
            "Propose a new event for the owner's Google Calendar. From an agent or MCP client it is NOT created directly: it waits for the owner's confirmation in the Secretary app (Links), and the owner can accept or dismiss it. Tell the owner to confirm it there. (When the owner asks Lucina himself in his own chat, she adds the event directly; that is not this tool.)"),
        spec!("gym_summary", "Gym summary", Scope::Read, ro = true, destructive = false, idempotent = true, GymArgs,
            "The owner's training: workouts this week vs. the weekly goal, recent workouts with volume, exercises and personal records, and weekly volume."),
        spec!("body_summary", "Body weight summary", Scope::Read, ro = true, destructive = false, idempotent = true, NoArgs,
            "The owner's body weight: current weight, 7-day average, weekly change, 30/90-day trend, goal distance, and the latest entries."),
        spec!("note_save", "Save note", Scope::Write, ro = false, destructive = false, idempotent = false, NoteSaveArgs,
            "Save a note, briefing, document or checklist for the owner: it appears in the Secretary app (Notes, with your name as the source) and Lucina can recall it by its slug, e.g. \"craftit-meeting\". Use this, not ask_lucina, for anything long or worth keeping. Markdown content, up to 256 KB per note; for a document too long for one call, save the first part here and send the rest in order with note_append. Keep features: `items` makes it a checklist (strings or {text, checked}), `labels` (display names), `color` (default, red, orange, yellow, green, teal, blue, cerulean, purple, pink, brown, gray), `pinned`, `archived`. An existing slug is only overwritten with replace=true. Answers the slug to use later."),
        spec!("note_append", "Append to note", Scope::Write, ro = false, destructive = false, idempotent = false, NoteAppendArgs,
            "Add text at the end of an existing note (by slug or id): the next part of a long document, an update to a briefing, meeting minutes. With continuation=true the text is glued on exactly (for a chunk cut mid-sentence); otherwise it starts a new paragraph. On a checklist note each line becomes a new item (use note_update addItems for that)."),
        spec!("note_update", "Update note", Scope::Write, ro = false, destructive = false, idempotent = false, NoteUpdateArgs,
            "Change an existing note (by slug or id) without rewriting it: title, content, colour, pinned, archived, labels (addLabels / removeLabels by name), reminder (reminderAt as RFC 3339 with offset, or null to clear; reminderRepeat), and checklist edits: check / uncheck (item numbers exactly as note_get shows them, or the exact item text), addItems, removeItems, and convert (list or text). Only what you pass changes. Nothing is deleted: the note stays, and removed items or labels are only taken off it. Read the note with note_get first to see the item numbers."),
        spec!("note_get", "Read note", Scope::Read, ro = true, destructive = false, idempotent = true, NoteGetArgs,
            "Read one of the owner's notes by slug (e.g. \"craftit-meeting\") or id: title, labels, colour, pinned/archived/trashed, who wrote it, when, the reminder, attachments (name, type, size), links and collaborators, and the Markdown content. A checklist note shows its items with numbers and checked state instead (use the numbers with note_update). Long notes come in pages: call again with the offset it gives you."),
        spec!("notes_search", "Search notes", Scope::Read, ro = true, destructive = false, idempotent = true, NotesSearchArgs,
            "Find the owner's notes, briefings, documents and checklists by words (title, slug, labels, content, checklist items), label, colour or kind (list, text, image, audio, link, reminder); archived notes are included unless archived=exclude (or archived=only). Pinned first, newest first. Each hit shows its slug, title, source, labels, colour, checklist progress (3/7 done), reminder and an excerpt. Without `q` it lists the latest notes."),
        spec!("note_labels", "Note labels", Scope::Read, ro = true, destructive = false, idempotent = true, NoArgs,
            "The owner's note labels (Keep labels) by display name, with how many notes each has. Use a name with notes_search label=."),
        // Lucina's ops record (crate::ops): every session and agent reports here; `agents` tokens too.
        spec!("ops_record", "Report to Lucina's record", Scope::Write, ro = false, destructive = false, idempotent = true, crate::ops::RecordArgs,
            "Report to Lucina's record of the owner's projects. Call it whenever you deploy something, hit or fix an incident, make or learn of a decision, finish a piece of work (status), or find something worth keeping (finding). Lucina writes it to the Obsidian log (Lucina/Log/<date>.md), the project page (Lucina/Projects/<project>.md), an incident page for incidents, and her memory, so the owner and every agent can later ask what happened. One record per event; repeats are de-duplicated; secrets in the text are redacted. project is a short slug (lucina, lucina-agents, pokoin, orchestra, nezopt, p1, ...). For incidents give severity, timeline, cause, fix and follow_ups when known. For decisions say who decided (decided_by owner, lucina or session), the question and who asked (asked_by): when Lucina decides something you asked her through ask_lucina, record it with decided_by lucina. at = when it happened (RFC 3339) when that was not just now."),
        spec!("ops_search", "Search Lucina's record", Scope::Read, ro = true, destructive = false, idempotent = true, crate::ops::SearchArgs,
            "Search Lucina's record of the owner's projects (deploys, incidents, decisions, status, findings) by words, project, kind and dates: answers 'what happened with X', 'why did we decide Y', 'when did Z go live'. Newest first when q is empty."),
        spec!("ops_get", "Read a record", Scope::Read, ro = true, destructive = false, idempotent = true, crate::ops::GetArgs,
            "One record of Lucina's ops record in full (timeline, cause, fix, follow-ups, links, pages), by the id ops_record or ops_search returned."),
        // AU Find (crate::au): Aarhus University people, buildings, campus maps; `agents` tokens too.
        spec!("au_people_search", "AU people search", Scope::Read, ro = true, destructive = false, idempotent = true, crate::au::PeopleArgs,
            "Aarhus University people lookup through AU Find. Retries (surname, then prefix) and disambiguation (top 3, never pick one) are built in. Quote fields as returned and cite the source; never invent emails, phones, rooms, coordinates or URLs. If status is not_found, say 'not found in AU Find'; if unavailable, say AU Find is unreachable and fall back to au.dk pages (www.au.dk, mit.au.dk, brightspace.au.dk, pure.au.dk only), quoting text and URL, stopping at any login. Answers a numbered list (name, position, units, AU room, email, phone, profile URL), then the raw JSON."),
        spec!("au_building", "AU building", Scope::Read, ro = true, destructive = false, idempotent = true, crate::au::BuildingArgs,
            "Aarhus University building lookup through AU Find, by building number (e.g. 1532), name or address: id, names, address and coordinates, then the raw JSON. Quote fields as returned and cite the source; never invent addresses, coordinates or URLs. If status is not_found, say 'not found in AU Find'. Draw a building with au_campus_map."),
        spec!("au_campus_map", "AU campus map", Scope::Read, ro = true, destructive = false, idempotent = true, crate::au::MapArgs,
            "An 800x600 PNG map of an Aarhus University building (building id from au_building; zoom 17 by default) or of a point (latitude, longitude), zoom 14-18; with nothing given, the AU park in Aarhus at zoom 15. Cite the caption's source (AU Find map, OpenStreetMap tiles)."),
        // Caller screening (crate::callers): Jev's verdict on numbers that called the owner; `agents` tokens too.
        spec!("caller_screen", "Screen callers", Scope::Read, ro = true, destructive = false, idempotent = false, crate::callers::ScreenArgs,
            "Who called the owner from these phone numbers? For 1-20 numbers Lucina first reads the public caller-ID page (180.dk), then asks Jev for P(spam), who is likely calling (person, business, telemarketing, scam, unknown) and P(pick up), from facts about each number only (country, line type, his contacts and his calls with it, the public reports). Returns her table with the quoted sources (show it as is), then the raw JSON with labels and reasons. Never invent owners of numbers. Read-only: it never answers, rejects or places calls."),
        spec!("tasks_list", "Tasks", Scope::Read, ro = true, destructive = false, idempotent = true, TasksListArgs,
            "The owner's tasks (synced with Todoist): by default Today with the overdue ones first, like Todoist's Today view; or upcoming, inbox, a project, a search or Todoist filter, or what was completed. Each task shows its id, project, due date (↻ = recurring), priority and labels, plus the daily goal."),
        spec!("task_add", "Add task", Scope::Write, ro = false, destructive = false, idempotent = false, TaskAddArgs,
            "Add a task to the owner's Todoist through Secretary, written like Todoist's quick add: dates and recurrence in Italian, English or Danish (\"domani alle 9\", \"every day\", \"hver mandag\"), #Project, @label, p1-p4. It shows up at once and syncs to Todoist within a minute."),
        spec!("task_complete", "Complete task", Scope::Write, ro = false, destructive = false, idempotent = false, TaskCompleteArgs,
            "Mark one of the owner's tasks done (by id from tasks_list, or by words of its title). A recurring task moves to its next date in Todoist. Deleting tasks is not possible here; ask the owner."),
        spec!("lucina_status", "Lucina status", Scope::Read, ro = true, destructive = false, idempotent = true, NoArgs,
            "Health of the Secretary gateway and of Lucina (reachable, latency), the owner's time zone and current local time, and which client and scopes this connection has. Use lucina_decide for go-aheads; ask_lucina is answers-only for agents."),
        spec!("library_list_books", "Library list books", Scope::Read, ro = true, destructive = false, idempotent = true, LibraryListArgs, "List books. Ownership and Kindle presence do not imply reading or completion."),
        spec!("library_search", "Library search", Scope::Read, ro = true, destructive = false, idempotent = true, LibraryListArgs, "Search titles, authors, tags and clippings, with source references."),
        spec!("library_get_book", "Library get book", Scope::Read, ro = true, destructive = false, idempotent = true, LibraryBookArgs, "Read book metadata and highlights with references."),
        spec!("library_get_progress", "Library get progress", Scope::Read, ro = true, destructive = false, idempotent = true, LibraryBookArgs, "Show explicitly recorded progress; unknown stays unknown."),
        spec!("library_get_highlights", "Library get highlights", Scope::Read, ro = true, destructive = false, idempotent = true, LibraryBookArgs, "Read highlights, notes and bookmarks with page/location references."),
        spec!("library_get_recent_reading", "Library get recent reading", Scope::Read, ro = true, destructive = false, idempotent = true, LibraryRecentArgs, "Recent highlights and owner reading changes. Stock Kindle sessions are unavailable."),
        spec!("library_get_brief", "Library get brief", Scope::Read, ro = true, destructive = false, idempotent = true, LibraryBookArgs, "Read generated briefs and their highlight references; never attribute them to the owner."),
        spec!("library_find_related_notes", "Library find related notes", Scope::Read, ro = true, destructive = false, idempotent = true, LibraryBookArgs, "Read Obsidian notes indexed as referring to a book, cited as [obsidian:<vault>/<path>#L<line>]. Mentions are relations, not reading evidence."),
        spec!("library_sync_status", "Library sync status", Scope::Read, ro = true, destructive = false, idempotent = true, NoArgs, "Show the last imports and library counts."),
        spec!("library_generate_brief", "Library generate brief", Scope::Write, ro = false, destructive = false, idempotent = true, LibraryBookArgs, "Generate an extractive brief from highlights, with citations. Model briefs are not implemented."),
        spec!("computer_task", "Computer task", Scope::Computer, ro = false, destructive = false, idempotent = false, crate::computer::TaskArgs,
            "Give Lucina's own Linux desktop (an isolated sandbox with Chromium, a terminal and files; no access to the owner's machines) a whole job in words; her computer-use engine does it and reports what it observed. Tasks run up to 200 s: this waits up to wait_seconds (default 45) and otherwise returns a job id for computer_task_status. One task at a time. Lucina's computer is an isolated Linux desktop sandbox (container `lucina-workspace` on nezopt; not nezopt's host, not the owner's Mac). Its only browser is Chromium: one window, already open and signed in to the owner's accounts (Outlook, Google). Use the Lucina MCP tools: `computer_screenshot` to see it (it lists each window's pid and window_id), `computer_call` with `get_window_state` {pid, window_id} to read that Chromium window and `click`, `type_text`, `press_key` to act in it (reuse its tabs; ctrl+t opens a new tab there), `computer_bash` for shell work, or `computer_task` to have Lucina do the whole job. The driver's browser_* tools cannot attach to that window; never open a second browser window or an isolated profile, and never install or launch Firefox. Those signed-in sessions are the owner's: read and navigate freely, but never send email or messages, buy anything, accept terms or change account settings without the owner's explicit OK."),
        spec!("computer_task_status", "Computer task status", Scope::Computer, ro = true, destructive = false, idempotent = true, crate::computer::JobArgs,
            "Wait for a computer_task job (up to wait_seconds, default 45) and return its result: status, what Lucina's engine observed, and her answer."),
        spec!("computer_task_cancel", "Cancel computer task", Scope::Computer, ro = false, destructive = false, idempotent = true, crate::computer::JobArgs,
            "Stop a running computer_task job. Steps already done in the sandbox are not undone."),
        spec!("computer_bash", "Computer shell", Scope::Computer, ro = false, destructive = false, idempotent = false, crate::computer::BashArgs,
            "Run one shell command as user `cua` in Lucina's Linux desktop sandbox (isolated container, no host mounts) and return its output. Chromium only; never install or launch Firefox."),
        spec!("computer_screenshot", "Computer screenshot", Scope::Computer, ro = true, destructive = false, idempotent = true, crate::computer::NoArgs,
            "The current screen of Lucina's Linux desktop (1280x800 PNG) and its visible windows with the pid and window_id that computer_call tools take."),
        spec!("computer_tools", "Computer driver tools", Scope::Computer, ro = true, destructive = false, idempotent = true, crate::computer::ToolsArgs,
            "List the desktop driver tools (click, type_text, press_key, scroll, launch_app, get_window_state, browser_*, …); pass name for the full input schemas of the matching tools. Chromium only: web pages go through the open Chromium window with get_window_state, click and type_text (see computer_call)."),
        spec!("computer_call", "Computer driver call", Scope::Computer, ro = false, destructive = false, idempotent = false, crate::computer::CallArgs,
            "Call one desktop driver tool on Lucina's Linux desktop, e.g. click {pid, window_id, x, y} or {scope: \"desktop\", x, y}, type_text {text}, press_key {key}, launch_app. Chromium only; never install or launch Firefox. For web pages: get_window_state {pid, window_id} on the open Chromium window to read it, then click, type_text and press_key in it; the browser_* tools cannot attach to it, and never open a second browser. Never send email or messages, buy anything, accept terms or change account settings in the owner's signed-in sessions without the owner's explicit OK. Refused while the owner drives the desktop from the app."),
        spec!("android_bash", "Android shell", Scope::Computer, ro = false, destructive = false, idempotent = false, crate::computer::BashArgs,
            "Run one shell command in Lucina's Android sandbox container and return its output."),
        spec!("android_screenshot", "Android screenshot", Scope::Computer, ro = true, destructive = false, idempotent = true, crate::computer::NoArgs,
            "The current screen of Lucina's Android sandbox and its visible windows."),
        spec!("android_tools", "Android driver tools", Scope::Computer, ro = true, destructive = false, idempotent = true, crate::computer::ToolsArgs,
            "List the Android sandbox's driver tools; pass name for full input schemas."),
        spec!("android_call", "Android driver call", Scope::Computer, ro = false, destructive = false, idempotent = false, crate::computer::CallArgs,
            "Call one driver tool on Lucina's Android sandbox (click, type_text, press_key, scroll, launch_app, …). Refused while the owner drives it from the app."),
        // Lucina Agents (crate::agents): scope `agents`.
        spec!("agents_list", "List coding agents", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::ListArgs,
            "List the owner's coding agents (Lucina Agents: Claude Code, Codex, OpenCode, Cursor, Copilot … running on nezopt), newest first: short id, title, provider/model, status (running, idle, error), whether it needs attention, directory, who started it, and the full id the other agent_* tools take. Filter by status, mine=true for the ones this connection started, archived=true for archived ones."),
        spec!("agent_run", "Start a coding agent", Scope::Agents, ro = false, destructive = false, idempotent = false, crate::agents::RunArgs,
            "Start a coding agent on nezopt with a task, and wait up to wait_seconds (default 30) for its first answer; long tasks keep running, then use agent_wait and agent_logs. Run it in a worktree: workspace_create first and pass workspace_id (main checkouts are refused). It asks the owner before risky steps; those permission requests are answered only by the owner in the Secretary app, never through this MCP, and modes that skip them are refused. Agents started here can't start further agents (delegation depth 1), and each connection has a budget of running agents and runs per hour. Pick the provider (claude, codex, opencode, cursor, copilot) and optionally model and mode (e.g. plan for a read-only plan)."),
        spec!("agent_send", "Message a coding agent", Scope::Agents, ro = false, destructive = false, idempotent = false, crate::agents::SendArgs,
            "Send a follow-up to a coding agent. If it is in the middle of a turn, steer=true adds your message to that turn; otherwise (default) the running turn is interrupted and a new one starts. Optionally wait up to wait_seconds for its answer."),
        spec!("agent_logs", "Coding agent log", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::LogsArgs,
            "What a coding agent did, as a plain-text activity log: the prompts, its messages and tool calls (commands, edits), last `tail` entries (default 40). Same text as `lagent logs` on nezopt."),
        spec!("agent_wait", "Wait for a coding agent", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::WaitArgs,
            "Wait up to wait_seconds (default 45, max 50) for a coding agent to finish its turn, then return how it ended (idle, error, or waiting for a permission only the owner can answer), the finished-run gate's verdict when there is one, and its latest message. Call again if it is still working."),
        spec!("agent_stop", "Stop a coding agent", Scope::Agents, ro = false, destructive = true, idempotent = true, crate::agents::AgentArgs,
            "Stop a coding agent's running turn. Its work so far stays in its workspace; agent_send starts it again."),
        spec!("agent_archive", "Archive a coding agent", Scope::Agents, ro = false, destructive = true, idempotent = true, crate::agents::AgentArgs,
            "Archive a coding agent: it leaves the lists (agents_list archived=true still shows it). Nothing is deleted; its workspace, branch and history stay."),
        spec!("agent_inspect", "Inspect a coding agent", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::AgentArgs,
            "Details of one coding agent: id, title, provider/model, status, mode, directory and workspace, timestamps, last error, last turn's token use, the finished-run gate, labels (who started it, delegation depth) and pending permission requests (read only)."),
        spec!("agents_search", "Search coding agents", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::SearchArgs,
            "Find coding agents (current and archived) by their title, workspace, branch or project (typos allowed), or by words they or the owner said."),
        spec!("workspaces_list", "Agent workspaces", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::WorkspacesArgs,
            "The workspaces coding agents work in: git worktrees on nezopt with their id, title, project, branch and base, directory and setup status. Filter with q."),
        spec!("workspace_create", "Create agent workspace", Scope::Agents, ro = false, destructive = false, idempotent = false, crate::agents::WorkspaceCreateArgs,
            "Create a git worktree for coding agents from a repository's main checkout on nezopt (repo_path, e.g. /home/nez/Projects/Lucina/SecretaryIOS), on a new branch (from `base`, default the repository's base branch). Returns the workspace_id to pass to agent_run. The owner merges, pushes and opens PRs; this MCP can't."),
        spec!("workspace_diff", "Workspace diff", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::DiffArgs,
            "The changes in an agent workspace as a unified diff: uncommitted (working tree), base (commits since its base branch) or all (default). Optionally one file."),
        spec!("files_list", "Workspace files", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::FilesArgs,
            "List a directory of an agent workspace (path relative to it; default its root): files with sizes, and subdirectories."),
        spec!("file_read", "Read workspace file", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::FileReadArgs,
            "Read a text file in an agent workspace (path relative to it), about 12000 characters at a time; call again with offset for the rest."),
        spec!("terminal_list", "Agent terminals", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::WorkspaceArgs,
            "The terminals you may use (of agents you started and scripts you ran): id, name, status, directory, and the ports their programs listen on. workspace_id narrows it to one workspace."),
        spec!("terminal_read", "Read agent terminal", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::TerminalArgs,
            "The last lines (default 200) of a terminal from terminal_list, as plain text."),
        spec!("terminal_send", "Type in agent terminal", Scope::Agents, ro = false, destructive = false, idempotent = false, crate::agents::TerminalSendArgs,
            "Type text or one key token (Enter, C-c, Up, …) into a terminal from terminal_list, e.g. to answer a prompt. It can't open or kill terminals."),
        spec!("scripts_list", "Workspace scripts", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::WorkspaceArgs,
            "The scripts and services a workspace defines (paseo.json, .lucina/agents.toml), whether they run, their ports, health and terminals."),
        spec!("script_start", "Start workspace script", Scope::Agents, ro = false, destructive = false, idempotent = false, crate::agents::ScriptArgs,
            "Start one of a workspace's scripts (scripts_list) in its own terminal; a service gets a port. Its output is in terminal_read."),
        spec!("script_stop", "Stop workspace script", Scope::Agents, ro = false, destructive = false, idempotent = true, crate::agents::ScriptArgs,
            "Stop a running workspace script (scripts_list) and the programs it started."),
        spec!("schedules_list", "Agent schedules", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::NoArgs,
            "Scheduled coding-agent runs and heartbeats (cron), with their next run, marking the ones this connection created."),
        spec!("schedule_add", "Add agent schedule", Scope::Agents, ro = false, destructive = false, idempotent = false, crate::agents::ScheduleAddArgs,
            "Schedule a recurring coding-agent run with a cron expression in the owner's time zone; each run starts a new agent in a mode that keeps permission prompts. Agents started from the MCP can't add schedules."),
        spec!("schedule_remove", "Remove agent schedule", Scope::Agents, ro = false, destructive = true, idempotent = true, crate::agents::ScheduleArgs,
            "Remove a schedule this connection created; others belong to the owner."),
        spec!("permissions_list", "Agent permission requests", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::PermissionsArgs,
            "Permission requests from coding agents (and from the secrets broker) waiting for the owner: what is asked, by which agent, until when. Read only: only the owner answers them, in the Secretary app."),
        spec!("agents_usage", "Coding agent usage", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::NoArgs,
            "Coding agents by provider (how many, how many running, tokens of their last turns, cost when the provider reports it), the providers this server has, and the schedules."),
        // The Homelab service catalog (agentd /catalog, nezopt ~/Projects/Homelab/catalog): ask before acting.
        spec!("catalog_get", "Catalog component", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::CatalogGetArgs,
            "One component of the owner's systems from the Homelab catalog (hosts nezopt, p1, pi-home, oracle-peer1…; services, timers, containers, k3s workloads, databases, tunnels, sites, repos): where it runs, its units, containers and ports, what it needs and what needs it, its health right now (checked every 5 minutes), the last deploy, its logs, restart and rollback commands, and its runbook. Ask before restarting, deploying or changing anything."),
        spec!("catalog_impact", "Catalog impact", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::CatalogImpactArgs,
            "What breaks and what degrades if these components are down (stopped, restarted, unreachable), following the dependency graph, with the public hostnames that would fail. Use it before stopping, restarting or redeploying anything, or to understand an outage."),
        spec!("catalog_health", "Catalog health", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::CatalogHealthArgs,
            "Health from the catalog: without id, everything that is down, broken, degraded or warning right now (worst first, with the reason); with id, that component's checks. live=true rescans its host first (slower)."),
        spec!("catalog_drift", "Catalog drift", Scope::Agents, ro = true, destructive = false, idempotent = true, crate::agents::NoArgs,
            "Drift between the catalog and reality: units, containers, k8s workloads, tunnels, Caddy sites and repos that run but are not catalogued, and catalogued ones that are missing."),
        // Lucina Notebooks (crate::notebooks): source-grounded notebooks; scope `notebooks` only.
        spec!("notebook_list", "List notebooks", Scope::Notebooks, ro = true, destructive = false, idempotent = true, crate::notebooks::ListArgs,
            "List the owner's Lucina Notebooks (NotebookLM-style workspaces that group sources, linked notes and chats) with their ids and how many sources, notes, chats and podcasts each has. Archived ones only with archived=include or only."),
        spec!("notebook_get", "Read a notebook", Scope::Notebooks, ro = true, destructive = false, idempotent = true, crate::notebooks::GetArgs,
            "One notebook in compact text: its description, sources (id, kind, title, whether it is ready, pages and passages, whether it counts in full, as a summary or not at all), linked notes (slugs), chats (ids, to continue with notebook_ask) and podcasts (ids and status). Sources are still read in the background right after notebook_add_source: wait for `ready`."),
        spec!("notebook_create", "Create a notebook", Scope::Notebooks, ro = false, destructive = false, idempotent = false, crate::notebooks::CreateArgs,
            "Create an empty notebook (a name and an optional description) and answer its id. Then add sources with notebook_add_source. A second call with the same name makes a second notebook."),
        spec!("notebook_add_source", "Add a source to a notebook", Scope::Notebooks, ro = false, destructive = false, idempotent = false, crate::notebooks::AddSourceArgs,
            "Add one source to a notebook: a `url` (web page, YouTube video, PDF or audio file), pasted `text` (with a `title`), a file on Lucina's desktop sandbox (`sandbox_path`), or a Keep note by its slug (`note_slug`, linked, not copied). Lucina reads it in the background (extract, split into passages, embed) and answers its id with status queued; check notebook_get until it is `ready`. Large files that live on the owner's devices are uploaded in the Secretary app, not here."),
        spec!("notebook_ask", "Ask a notebook", Scope::Notebooks, ro = false, destructive = false, idempotent = false, crate::notebooks::AskArgs,
            "Ask a question that the notebook answers only from its sources and linked notes. Starts a new chat unless chat_id continues one, posts the question and returns the answer with a numbered list of citations (title, page or time, excerpt). The answer text cites passages inline as [source:<source id>-<n>] (e.g. [source:s0a9be3c112-14]), notes as [note:<slug>]: keep those markers when you quote it, and never present a claim without one as coming from the sources. model is deepseek (default) or qwen (local, slower). Takes up to two minutes; the chat id is returned for follow-ups."),
        spec!("notebook_search", "Search notebooks", Scope::Notebooks, ro = true, destructive = false, idempotent = true, crate::notebooks::SearchArgs,
            "Search the passages of a notebook's sources and its linked notes (or of every notebook without notebook_id) by words or meaning. Returns ranked hits with their citation ref ([source:<source id>-<n>] or [note:<slug>]), title and excerpt, without asking a model. Use notebook_ask for an answer."),
        spec!("notebook_transform", "Transform a notebook source", Scope::Notebooks, ro = false, destructive = false, idempotent = false, crate::notebooks::TransformArgs,
            "Run a transformation (a saved prompt such as t-dense-summary) on one source; it answers the insight's id and status (queued, then running, ready) and the text is read later by calling this again with the same source_id. Without transformation_id it starts nothing and lists the available transformations and the source's existing insights. Insights are cited as [insight:<id>] in answers."),
        spec!("notebook_podcast", "Notebook podcast", Scope::Notebooks, ro = false, destructive = false, idempotent = false, crate::notebooks::PodcastArgs,
            "Make a podcast episode (two voices discussing the sources) from a notebook: pass notebook_id (and optionally briefing, minutes, episode_profile, speaker_profile) to start one, which answers its id and takes a few minutes; pass podcast_id to read its status (queued, outline, script, voicing, mixing, ready or failed), outline and whether the audio is ready. The owner plays the audio in the Secretary app. One of notebook_id or podcast_id, not both."),
        spec!("diagnostics_recent", "Recent crash reports", Scope::Read, ro = true, destructive = false, idempotent = true, crate::diagnostics::RecentArgs,
            "Crash reports, errors, hangs and user problem reports from the owner's Lucina clients (Secretary Code on the Mac, the iOS and Android apps, the web app, the servers), newest first. Each shows its id, client, kind, when (owner's time zone), how many times it repeated, version, OS, the message and the top stack frame. Filter by app, kind and since (\"24h\", \"7d\", a date). Use it when the owner asks why something crashed or misbehaved."),
        spec!("diagnostics_get", "Crash report", Scope::Read, ro = true, destructive = false, idempotent = true, crate::diagnostics::GetArgs,
            "One crash report by id: message, stack, context (window, route, engine version, ...), how often and where it repeated, its attachments, and the end of its main log, so you can explain to the owner what happened and why (e.g. \"Secretary Code crashed at 19:44: the background service timed out\"). Secrets were scrubbed before storage."),
        spec!("diagnostics_file", "Crash report attachment", Scope::Read, ro = true, destructive = false, idempotent = true, crate::diagnostics::FileArgs,
            "Read a text attachment of a crash report (a log, a JSON dump), by default its end, up to 60000 characters per call. Binary attachments such as minidumps are not readable here."),

    ]
}

pub fn spec(name: &str) -> Option<ToolSpec> {
    specs().into_iter().find(|s| s.name == name)
}

// -- implementation ----------------------------------------------------------------

/// A tool failure the model should read (returned as `isError: true`).
#[derive(Debug)]
pub struct ToolError(pub String);

impl From<BackendError> for ToolError {
    fn from(e: BackendError) -> Self {
        ToolError(match e.code.as_str() {
            "backend_unreachable" | "backend_timeout" => {
                format!("Secretary is not reachable right now ({e}). Try again later.")
            }
            "lucina_unreachable" | "lucina_timeout" | "lucina_error" | "lucina_not_configured" => {
                format!("Lucina is not available right now ({e}). Try again later.")
            }
            "rate_limited" => format!(
                "Too many requests; retry in {} s.",
                e.retry_after.unwrap_or(30)
            ),
            _ => format!("Secretary refused the request: {e}"),
        })
    }
}

pub type ToolResult = Result<String, ToolError>;

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError(msg.into())
}

fn parse<T: serde::de::DeserializeOwned>(args: &JsonObject) -> Result<T, ToolError> {
    serde_json::from_value(Value::Object(args.clone()))
        .map_err(|e| bad(format!("Invalid arguments: {e}")))
}

fn check_text(field: &str, s: &str, max: usize) -> Result<String, ToolError> {
    let t = s.trim();
    if t.is_empty() {
        return Err(bad(format!("`{field}` must not be empty")));
    }
    if t.chars().count() > max {
        return Err(bad(format!("`{field}` is longer than {max} characters")));
    }
    if t.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(bad(format!("`{field}` contains control characters")));
    }
    Ok(t.to_owned())
}

fn id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

/// Lucina accepts 12000 characters after the MCP prefix.
pub const MAX_ASK_CHARS: usize = 12_000;
/// What one `note_save` / `note_append` call may carry (= the note limit).
pub const MAX_NOTE_BYTES: usize = 256 * 1024;

/// A slug (`[a-z0-9-]`, at most 64) or a note id (`n_` + 20 hex).
fn note_key(slug: Option<&str>, id: Option<&str>) -> Result<String, ToolError> {
    fn pick(v: Option<&str>) -> Option<&str> {
        v.map(str::trim).filter(|v| !v.is_empty())
    }
    let key = match (pick(slug), pick(id)) {
        (_, Some(id)) => id.to_owned(),
        (Some(slug), None) => slug.to_lowercase(),
        (None, None) => {
            return Err(bad(
                "Give the note's `slug` (e.g. \"craftit-meeting\") or its `id`.",
            ))
        }
    };
    let ok = key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if !ok {
        return Err(bad(
            "`slug` must be lower-case letters, digits and '-' (or pass the note `id`)",
        ));
    }
    Ok(key)
}

fn note_content(field: &str, s: &str) -> Result<String, ToolError> {
    if s.trim().is_empty() {
        return Err(bad(format!("`{field}` must not be empty")));
    }
    if s.len() > MAX_NOTE_BYTES {
        return Err(bad(format!(
            "`{field}` is over 256 KB; send it in parts: note_save with the first part, then note_append for the rest"
        )));
    }
    if s.chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(bad(format!("`{field}` contains control characters")));
    }
    Ok(s.to_owned())
}

fn note_tags(tags: Option<&[String]>) -> Result<Option<Vec<String>>, ToolError> {
    let Some(tags) = tags else { return Ok(None) };
    if tags.len() > 50 {
        return Err(bad("`tags`: at most 50"));
    }
    Ok(Some(
        tags.iter()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect(),
    ))
}

/// Label display names, checked like the server will.
fn label_names(field: &str, labels: &[String]) -> Result<Vec<String>, ToolError> {
    if labels.len() > 50 {
        return Err(bad(format!("`{field}`: at most 50")));
    }
    labels
        .iter()
        .map(|l| keep::label_name(l).map_err(|e| bad(format!("`{field}`: {e}"))))
        .collect()
}

fn keep_color(c: &str) -> Result<String, ToolError> {
    keep::color(c).map_err(bad)
}

/// Checklist items from the arguments: `{text, checked, indent: 0}`.
fn items_json(field: &str, items: &[ItemArg]) -> Result<Vec<Value>, ToolError> {
    if items.len() > 1000 {
        return Err(bad(format!("`{field}`: at most 1000 items")));
    }
    items
        .iter()
        .map(|it| {
            let (text, checked) = match it {
                ItemArg::Text(t) => (t.as_str(), false),
                ItemArg::Item { text, checked } => (text.as_str(), checked.unwrap_or(false)),
            };
            let text = check_text(&format!("{field} text"), text, 2000)?;
            Ok(keep::new_item(&text, checked))
        })
        .collect()
}

fn item_refs(refs: Option<&[ItemRefArg]>) -> Vec<ItemRef> {
    refs.unwrap_or_default()
        .iter()
        .map(|r| match r {
            ItemRefArg::Number(n) => ItemRef::Number(*n as usize),
            ItemRefArg::Text(t) => ItemRef::Text(t.clone()),
        })
        .collect()
}

/// The items of a note (a list note), when it is one.
fn note_items(n: &Value) -> Option<Vec<Value>> {
    n.get("items").and_then(Value::as_array).cloned()
}

/// Copies every field of `from` (an object) into `into`.
fn merge_into(into: &mut Value, from: &Value) {
    if let (Some(into), Some(from)) = (into.as_object_mut(), from.as_object()) {
        for (k, v) in from {
            into.insert(k.clone(), v.clone());
        }
    }
}

/// The prefix Lucina sees on MCP messages, naming the client.
pub fn ask_prefix(caller: &str) -> String {
    format!("[via Lucina MCP, from {caller}] ")
}

/// Prefix for a single message allowed to act on selected request kinds.
pub fn ask_prefix_allow(caller: &str, allow: &[AllowArg]) -> String {
    let mut kinds: Vec<_> = allow.iter().map(|a| a.as_str()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    format!(
        "[via Lucina MCP, from {caller}; allow={}] ",
        kinds.join(",")
    )
}

fn wait_seconds(wait: Option<u32>) -> Result<u32, ToolError> {
    let wait = wait.unwrap_or(0);
    if wait > 100 {
        return Err(bad("`wait_seconds` must be 0-100"));
    }
    Ok(wait)
}

impl DecideArgs {
    fn validate(&self) -> Result<(), ToolError> {
        fn length(field: &str, text: &str, min: usize, max: usize) -> Result<(), ToolError> {
            if !(min..=max).contains(&text.chars().count()) {
                return Err(bad(format!("`{field}` must be {min}-{max} characters")));
            }
            Ok(())
        }
        length("question", &self.question, 1, 2000)?;
        if !(2..=6).contains(&self.options.len()) {
            return Err(bad("`options` must have 2-6 entries"));
        }
        let mut seen = std::collections::HashSet::new();
        for option in &self.options {
            length("options", option, 1, 200)?;
            if !seen.insert(option.trim().to_lowercase()) {
                return Err(bad("`options` must be unique ignoring case and whitespace"));
            }
        }
        if let Some(r) = &self.recommendation {
            let r = r.trim().to_lowercase();
            if !seen.contains(&r) && !(1..=self.options.len()).any(|i| r == i.to_string()) {
                return Err(bad(
                    "`recommendation` must be an option or its 1-based number",
                ));
            }
        }
        for (field, text, max) in [
            ("context", &self.context, 8000),
            ("topic", &self.topic, 64),
            ("asking_session", &self.asking_session, 120),
        ] {
            if let Some(text) = text {
                length(field, text, 0, max)?;
            }
        }
        wait_seconds(self.wait_seconds)?;
        Ok(())
    }
}

pub struct Tools<'a> {
    pub app: &'a App,
    pub caller: &'a Caller,
}

impl Tools<'_> {
    async fn poll_decision(&self, mut response: Value, wait: u32) -> Result<Value, ToolError> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait.into());
        while response["status"] == "pending" && wait > 0 {
            let next = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
            if next > deadline {
                break;
            }
            let id = response["decision_id"]
                .as_str()
                .ok_or_else(|| bad("Pending response missing decision_id"))?
                .to_owned();
            secretary_core::Call::decision(&id).map_err(|e| bad(e.to_string()))?;
            tokio::time::sleep_until(next).await;
            match tokio::time::timeout_at(
                deadline,
                self.app.backend.get(&format!("/v1/decisions/{id}"), &[]),
            )
            .await
            {
                Ok(result) => response = result?,
                Err(_) => break,
            }
        }
        Ok(response)
    }

    pub async fn decide(&self, args: &JsonObject) -> ToolResult {
        let a: DecideArgs = parse(args)?;
        a.validate()?;
        let wait = wait_seconds(a.wait_seconds)?;
        let mut body = serde_json::to_value(&a).map_err(|e| bad(e.to_string()))?;
        body["asking_client"] = json!(self.caller.name);
        let response = self.app.backend.create_decision(&body).await?;
        Ok(format::decision(&self.poll_decision(response, wait).await?))
    }

    pub async fn decision_status(&self, args: &JsonObject) -> ToolResult {
        let a: DecisionStatusArgs = parse(args)?;
        let wait = wait_seconds(a.wait_seconds)?;
        secretary_core::Call::decision(&a.decision_id).map_err(|e| bad(e.to_string()))?;
        let response = self
            .app
            .backend
            .get(&format!("/v1/decisions/{}", a.decision_id), &[])
            .await?;
        Ok(format::decision(&self.poll_decision(response, wait).await?))
    }

    pub async fn decision_withdraw(&self, args: &JsonObject) -> ToolResult {
        let a: DecisionWithdrawArgs = parse(args)?;
        let reason = a.reason.as_deref().map(str::trim).filter(|r| !r.is_empty());
        secretary_core::Call::withdraw_decision(&a.decision_id, reason)
            .map_err(|e| bad(e.to_string()))?;
        let query: Vec<(&str, String)> = reason
            .map(|r| ("reason", r.to_owned()))
            .into_iter()
            .collect();
        let response = self
            .app
            .backend
            .send_device_query(
                reqwest::Method::DELETE,
                &format!("/v1/decisions/{}", a.decision_id),
                &query,
                None,
                reqwest::header::HeaderMap::new(),
            )
            .await?;
        Ok(format::decision(&response))
    }

    pub async fn job_status(&self, args: &JsonObject) -> ToolResult {
        let a: JobStatusArgs = parse(args)?;
        secretary_core::Call::job(&a.job_id).map_err(|e| bad(e.to_string()))?;
        let response = self
            .app
            .backend
            .get(&format!("/v1/jobs/{}", seg(&a.job_id)), &[])
            .await?;
        Ok(format::job(&response))
    }

    pub async fn ask(&self, args: &JsonObject) -> ToolResult {
        let a: AskArgs = parse(args)?;
        let msg = check_text("message", &a.message, MAX_ASK_CHARS)?;
        let mut session = format!("mcp-{}", self.caller.name);
        if let Some(c) = a
            .conversation
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        {
            if c.len() > 32
                || !c
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err(bad(
                    "`conversation` must be 1-32 letters, digits, '-' or '_'",
                ));
            }
            session = format!("{session}-{c}");
        }
        session.truncate(64);
        let body = json!({
            "message": format!("{}{msg}", a.allow.as_deref().filter(|a| !a.is_empty()).map(|a| ask_prefix_allow(&self.caller.name, a)).unwrap_or_else(|| ask_prefix(&self.caller.name))),
            "sessionId": session,
            "clientMessageId": uuid::Uuid::new_v4().to_string(),
            "channel": "web",
            // Lucina derives these timeline tags from the session id and message prefix.
            "meta": { "via": "mcp", "caller": self.caller.name },
        });
        let r = self.app.backend.chat(&body).await?;
        Ok(format::chat_reply(&r))
    }

    pub async fn timeline(&self, args: &JsonObject) -> ToolResult {
        let a: TimelineArgs = parse(args)?;
        let limit = a.limit.unwrap_or(20).clamp(1, 100);
        let mut q = vec![(
            "limit",
            if a.channel.is_some() {
                "200".to_owned()
            } else {
                limit.to_string()
            },
        )];
        if let Some(b) = a.before.as_deref() {
            if !id_ok(b) {
                return Err(bad("`before` must be a timeline id from a previous page"));
            }
            q.push(("before", b.to_owned()));
        }
        let page = self.app.backend.get("/v1/timeline", &q).await?;
        Ok(format::timeline(
            &page,
            a.channel.map(ChannelArg::as_str),
            limit as usize,
            self.app.cfg.timezone,
        ))
    }

    /// Resolve `list` (id or name) to a list id; `Ok(None)` = v2 unavailable.
    async fn list_id(&self, list: Option<&str>) -> Result<Result<String, BackendError>, ToolError> {
        let Some(want) = list.map(str::trim).filter(|l| !l.is_empty()) else {
            return Ok(Ok("default".into()));
        };
        let lists = match self.app.backend.get("/v1/shopping/lists", &[]).await {
            Ok(v) => v,
            Err(e) if e.is_v2_unavailable() => return Ok(Err(e)),
            Err(e) => return Err(e.into()),
        };
        let arr = lists.as_array().cloned().unwrap_or_default();
        let found = arr
            .iter()
            .find(|l| l.get("id").and_then(Value::as_str) == Some(want))
            .or_else(|| {
                arr.iter().find(|l| {
                    l.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|n| format::fold(n) == format::fold(want))
                })
            });
        match found.and_then(|l| l.get("id")).and_then(Value::as_str) {
            Some(id) => Ok(Ok(id.to_owned())),
            None => Err(bad(format!(
                "No shopping list called {want:?}. Lists: {}",
                format::list_names(&arr)
            ))),
        }
    }

    async fn legacy_ok(&self, list: Option<&str>, e: BackendError) -> Result<(), ToolError> {
        match list.map(str::trim).filter(|l| !l.is_empty()) {
            None | Some("default") => Ok(()),
            Some(_) => Err(ToolError(format!(
                "Named lists need the v2 shopping API, which is not available here ({e})."
            ))),
        }
    }

    pub async fn shopping_list(&self, args: &JsonObject) -> ToolResult {
        let a: ShoppingListArgs = parse(args)?;
        let v2 = match self.list_id(a.list.as_deref()).await? {
            Ok(id) => {
                self.app
                    .backend
                    .get(&format!("/v1/shopping/lists/{}", seg(&id)), &[])
                    .await
            }
            Err(e) => Err(e),
        };
        match v2 {
            Ok(state) => {
                let others = self.app.backend.get("/v1/shopping/lists", &[]).await.ok();
                Ok(format::list_state(&state, others.as_ref()))
            }
            Err(e) if e.is_v2_unavailable() => {
                self.legacy_ok(a.list.as_deref(), e).await?;
                let items = self.app.backend.get("/v1/shopping", &[]).await?;
                Ok(format::legacy_list(&items))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub async fn shopping_add(&self, args: &JsonObject) -> ToolResult {
        let a: ShoppingAddArgs = parse(args)?;
        if a.items.is_empty() || a.items.len() > 50 {
            return Err(bad("`items` needs 1-50 entries"));
        }
        let mut ops = Vec::new();
        let mut names = Vec::new();
        for (i, it) in a.items.iter().enumerate() {
            let (name, spec, quantity, unit) = match it {
                AddItem::Name(n) => (n.as_str(), None, None, None),
                AddItem::Detailed {
                    name,
                    spec,
                    quantity,
                    unit,
                } => (name.as_str(), spec.as_deref(), *quantity, unit.as_deref()),
            };
            let name = check_text(&format!("items[{i}].name"), name, 200)?;
            let op_id = uuid::Uuid::new_v4().to_string();
            let mut op = json!({"opId": op_id, "type": "add", "name": name});
            if let Some(s) = spec.map(str::trim).filter(|s| !s.is_empty()) {
                op["spec"] = json!(check_text(&format!("items[{i}].spec"), s, 200)?);
            }
            if let Some(q) = quantity {
                if !(q > 0.0 && q <= 100_000.0) {
                    return Err(bad(format!(
                        "items[{i}].quantity must be > 0 and at most 100000"
                    )));
                }
                op["quantity"] = json!(q);
            }
            if let Some(u) = unit.map(str::trim).filter(|u| !u.is_empty()) {
                op["unit"] = json!(check_text(&format!("items[{i}].unit"), u, 32)?);
            }
            names.push((
                op_id,
                match spec.map(str::trim).filter(|s| !s.is_empty()) {
                    Some(s) => format!("{name} ({s})"),
                    None => name,
                },
            ));
            ops.push(op);
        }
        let v2 = match self.list_id(a.list.as_deref()).await? {
            Ok(id) => self
                .app
                .backend
                .post(
                    &format!("/v1/shopping/lists/{}/ops", seg(&id)),
                    &json!({"via": "mcp", "ops": ops}),
                )
                .await
                .map(|r| (id, r)),
            Err(e) => Err(e),
        };
        match v2 {
            // Lucina's questions are a normal answer to relay, not an error
            Ok((id, r)) => Ok(format::add_result(&names, &id, &r)),
            Err(e) if e.is_v2_unavailable() => {
                self.legacy_ok(a.list.as_deref(), e).await?;
                let mut added = Vec::new();
                for (_, n) in &names {
                    self.app
                        .backend
                        .post("/v1/shopping", &json!({"rawName": n}))
                        .await?;
                    added.push(n.clone());
                }
                Ok(format!("Added to the shopping list: {}.", added.join(", ")))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub async fn mark_bought(&self, args: &JsonObject) -> ToolResult {
        let a: MarkBoughtArgs = parse(args)?;
        let want = check_text("item", &a.item, 200)?;
        let v2 = match self.list_id(a.list.as_deref()).await? {
            Ok(id) => self
                .app
                .backend
                .get(&format!("/v1/shopping/lists/{}", seg(&id)), &[])
                .await
                .map(|s| (id, s)),
            Err(e) => Err(e),
        };
        match v2 {
            Ok((id, state)) => {
                let items = state
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let item = format::pick_item(&items, &want, |i| {
                    i.get("status").and_then(Value::as_str) == Some("needed")
                })
                .map_err(|m| {
                    let recent = format::pick_item(&items, &want, |i| {
                        i.get("status").and_then(Value::as_str) == Some("recent")
                    });
                    match recent {
                        Ok(r) => bad(format!(
                            "{} is already marked as bought.",
                            r.get("name").and_then(Value::as_str).unwrap_or(&want)
                        )),
                        Err(_) => bad(m),
                    }
                })?;
                let item_id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&want)
                    .to_owned();
                let op = json!({"opId": uuid::Uuid::new_v4().to_string(), "type": "purchase", "itemId": item_id});
                let r = self
                    .app
                    .backend
                    .post(
                        &format!("/v1/shopping/lists/{}/ops", seg(&id)),
                        &json!({"ops": [op]}),
                    )
                    .await?;
                Ok(format::ops_result("Marked as bought", &[name], &id, &r))
            }
            Err(e) if e.is_v2_unavailable() => {
                self.legacy_ok(a.list.as_deref(), e).await?;
                let items = self.app.backend.get("/v1/shopping", &[]).await?;
                let arr = items.as_array().cloned().unwrap_or_default();
                let item = format::pick_legacy(&arr, &want).map_err(bad)?;
                let item_id = format::scalar(item.get("id"));
                if !id_ok(&item_id) {
                    return Err(bad("that item has an unusable id"));
                }
                self.app
                    .backend
                    .patch(
                        &format!("/v1/shopping/{}", seg(&item_id)),
                        &json!({"status": "purchased"}),
                    )
                    .await?;
                Ok(format!("Marked as bought: {}.", format::legacy_name(&item)))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub async fn reminders(&self, args: &JsonObject) -> ToolResult {
        let a: RemindersListArgs = parse(args)?;
        let mut q = Vec::new();
        if let Some(s) = a.scope {
            q.push((
                "scope",
                match s {
                    ReminderScopeArg::Upcoming => "upcoming",
                    ReminderScopeArg::Today => "today",
                    ReminderScopeArg::All => "all",
                }
                .to_owned(),
            ));
        }
        if let Some(b) = a.include_sent {
            q.push(("includeSent", if b { "1" } else { "0" }.to_owned()));
        }
        let r = self.app.backend.get("/v1/reminders", &q).await?;
        Ok(format::reminders(&r, self.app.cfg.timezone))
    }

    pub async fn reminder_create(&self, args: &JsonObject) -> ToolResult {
        let a: ReminderCreateArgs = parse(args)?;
        let text = check_text("text", &a.text, 480)?;
        let when = check_text("when", &a.when, 200)?;
        let body = match chrono::DateTime::parse_from_rfc3339(&when) {
            Ok(t) => {
                if t.timestamp() <= chrono::Utc::now().timestamp() {
                    return Err(bad(format!(
                        "`when` is in the past. {}",
                        format::now_line(self.app.cfg.timezone)
                    )));
                }
                json!({"text": text, "dueAt": when})
            }
            Err(_) => json!({"naturalText": format!("{when} — {text}")}),
        };
        match self.app.backend.post("/v1/reminders", &body).await {
            Ok(r) => Ok(format::created_reminder(&r, self.app.cfg.timezone)),
            Err(e) if matches!(e.code.as_str(), "unparseable_reminder" | "due_in_past" | "bad_request") => {
                Err(bad(format!(
                    "Lucina could not use that time ({}: {}). Send `when` as an ISO 8601 timestamp with offset in the future. {}",
                    e.code,
                    e.message,
                    format::now_line(self.app.cfg.timezone)
                )))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub async fn reminder_cancel(&self, args: &JsonObject) -> ToolResult {
        let a: ReminderCancelArgs = parse(args)?;
        let id = a.id.trim();
        if !id_ok(id) {
            return Err(bad("`id` must be a reminder id from reminders_list"));
        }
        let r = self
            .app
            .backend
            .delete(&format!("/v1/reminders/{}", seg(id)))
            .await?;
        let cancelled = r.get("cancelled").and_then(Value::as_bool).unwrap_or(false);
        let text = r
            .pointer("/reminder/text")
            .and_then(Value::as_str)
            .unwrap_or("");
        Ok(if cancelled {
            format!("Cancelled the reminder \"{text}\" (id {id}); it will not be delivered.")
        } else {
            format!("The reminder \"{text}\" (id {id}) was already cancelled.")
        })
    }

    pub async fn calendar_list(&self, args: &JsonObject) -> ToolResult {
        let a: CalendarListArgs = parse(args)?;
        let mut q: Vec<(&str, String)> = Vec::new();
        for (k, v) in [("from", a.from.as_deref()), ("to", a.to.as_deref())] {
            if let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) {
                if v.len() > 40
                    || !v.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'-' | b':' | b'+' | b'.')
                    })
                {
                    return Err(bad(format!("`{k}` must be YYYY-MM-DD or an RFC 3339 time")));
                }
                q.push((k, v.to_owned()));
            }
        }
        if a.to.is_none() {
            let days = a.days.unwrap_or(7);
            if !(1..=62).contains(&days) {
                return Err(bad("`days` must be 1-62"));
            }
            q.push(("days", days.to_string()));
        }
        let r = self.app.backend.get("/v1/calendar/events", &q).await?;
        Ok(format::calendar(&r, self.app.cfg.timezone))
    }

    /// Never writes: files a proposal the owner accepts in the app.
    pub async fn calendar_create(&self, args: &JsonObject) -> ToolResult {
        let a: CalendarCreateArgs = parse(args)?;
        let mut body = json!({
            "title": check_text("title", &a.title, 300)?,
            "start": check_text("start", &a.start, 40)?,
            "from": format!("mcp:{}", self.caller.name),
        });
        if let Some(e) = a.end.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
            body["end"] = json!(check_text("end", e, 40)?);
        }
        if let Some(l) = a
            .location
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            body["location"] = json!(check_text("location", l, 500)?);
        }
        if let Some(d) = a
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            body["description"] = json!(check_text("description", d, 4000)?);
        }
        match self.app.backend.post("/v1/calendar/proposals", &body).await {
            Ok(p) => Ok(format!(
                "Proposed \"{}\" ({}). It needs owner confirmation in the app: the owner accepts or dismisses it in Secretary → Links before anything is added to the calendar. Proposal id {}.",
                body["title"].as_str().unwrap_or(""),
                format::local_time(body.get("start"), self.app.cfg.timezone),
                p.get("id").and_then(Value::as_str).unwrap_or("?")
            )),
            Err(e) if e.code == "bad_request" => Err(bad(format!(
                "Secretary refused the event ({}). Use RFC 3339 times with offset or YYYY-MM-DD dates. {}",
                e.message,
                format::now_line(self.app.cfg.timezone)
            ))),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn tasks_list(&self, args: &JsonObject) -> ToolResult {
        let a: TasksListArgs = parse(args)?;
        let view = a
            .view
            .as_deref()
            .map(str::trim)
            .unwrap_or("today")
            .to_lowercase();
        let mut q: Vec<(&str, String)> = Vec::new();
        let query = match a.query.as_deref().map(str::trim).filter(|x| !x.is_empty()) {
            Some(x) => Some(check_text("query", x, 300)?),
            None => None,
        };
        match (
            view.as_str(),
            a.project
                .as_deref()
                .map(str::trim)
                .filter(|x| !x.is_empty()),
        ) {
            (_, Some(p)) => {
                let p = check_text("project", p, 120)?;
                q.push(("view", "filter".into()));
                q.push(("query", format!("#{p}")));
            }
            ("search", _) => {
                q.push(("view", "search".into()));
                q.push((
                    "query",
                    query
                        .clone()
                        .ok_or_else(|| bad("view \"search\" needs `query`"))?,
                ));
            }
            ("today" | "upcoming" | "inbox" | "completed", _) if query.is_none() => {
                q.push(("view", view.clone()));
            }
            (_, _) if query.is_some() => {
                q.push(("view", "filter".into()));
                q.push(("query", query.clone().unwrap_or_default()));
            }
            _ => {
                return Err(bad(
                    "`view` must be today, upcoming, inbox, completed or search",
                ))
            }
        }
        let r = self.app.backend.get("/v1/tasks/state", &q).await?;
        Ok(format::tasks(&r))
    }

    async fn task_command(&self, command: Value) -> Result<Value, ToolError> {
        let body = json!({"op_id": format!("mcp-{}", uuid::Uuid::new_v4()), "command": command});
        match self.app.backend.post("/v1/tasks/command", &body).await {
            Ok(v) => Ok(v),
            Err(e) if e.code == "invalid_command" => {
                Err(bad(format!("Secretary refused it: {}", e.message)))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub async fn task_add(&self, args: &JsonObject) -> ToolResult {
        let a: TaskAddArgs = parse(args)?;
        let text = check_text("text", &a.text, 800)?;
        let mut cmd = json!({"type": "quick_add", "text": text});
        if let Some(d) = a.due.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            cmd["default_due"] = json!(check_text("due", d, 100)?);
        }
        let r = self.task_command(cmd).await?;
        tracing::info!(target: "secretary_mcp::audit", event = "task_add", caller = %self.caller.name);
        Ok(format::task_added(&r))
    }

    pub async fn task_complete(&self, args: &JsonObject) -> ToolResult {
        let a: TaskCompleteArgs = parse(args)?;
        let id = match (
            a.id.as_deref().map(str::trim).filter(|x| !x.is_empty()),
            a.title.as_deref().map(str::trim).filter(|x| !x.is_empty()),
        ) {
            (Some(id), _) => {
                if !id_ok(id) {
                    return Err(bad("`id` is not a task id"));
                }
                id.to_owned()
            }
            (None, Some(words)) => {
                let words = check_text("title", words, 200)?;
                let today = self
                    .app
                    .backend
                    .get("/v1/tasks/state", &[("view", "today".into())])
                    .await?;
                let mut hits = format::task_matches(&today, &words);
                if hits.is_empty() {
                    let all = self
                        .app
                        .backend
                        .get(
                            "/v1/tasks/state",
                            &[("view", "search".into()), ("query", words.clone())],
                        )
                        .await?;
                    hits = format::task_matches(&all, &words);
                }
                match hits.len() {
                    0 => {
                        return Err(bad(format!(
                            "No open task matches \"{words}\". tasks_list shows them with ids."
                        )))
                    }
                    1 => hits.remove(0).0,
                    _ => {
                        let list: Vec<String> = hits
                            .iter()
                            .take(8)
                            .map(|(id, c)| format!("\"{c}\" [id {id}]"))
                            .collect();
                        return Err(bad(format!(
                            "Several tasks match \"{words}\": {}. Call again with the id.",
                            list.join(", ")
                        )));
                    }
                }
            }
            (None, None) => {
                return Err(bad(
                    "Give the task `id` (from tasks_list) or words of its `title`.",
                ))
            }
        };
        let r = self
            .task_command(json!({"type": "complete", "task_id": id}))
            .await?;
        tracing::info!(target: "secretary_mcp::audit", event = "task_complete", id = %id, caller = %self.caller.name);
        Ok(format::task_completed(&r, &id))
    }

    pub async fn gym(&self, args: &JsonObject) -> ToolResult {
        let a: GymArgs = parse(args)?;
        let n = a.workouts.unwrap_or(5).clamp(1, 20);
        let tz = self.app.cfg.timezone.name();
        if self.app.backend.has_service_token() {
            match self.app.backend.insights(tz, n).await {
                Ok(v) => return Ok(format::gym_insights(&v)),
                Err(e) => {
                    tracing::warn!(code = %e.code, status = e.status, "insights failed; using gym state")
                }
            }
        }
        let st = self
            .app
            .backend
            .get("/v1/gym/state", &[("timezone", tz.to_owned())])
            .await?;
        Ok(format::gym_state(&st))
    }

    pub async fn body(&self) -> ToolResult {
        let tz = self.app.cfg.timezone.name();
        let st = self
            .app
            .backend
            .get("/v1/body/state", &[("timezone", tz.to_owned())])
            .await?;
        Ok(format::body_state(&st, self.app.cfg.timezone))
    }

    fn note_saved(&self, verb: &str, n: &Value) -> String {
        let slug = n.get("slug").and_then(Value::as_str).unwrap_or("?");
        let title = n.get("title").and_then(Value::as_str).unwrap_or("");
        let chars = n
            .get("content")
            .and_then(Value::as_str)
            .map_or(0, |c| c.chars().count());
        let id = n.get("id").and_then(Value::as_str).unwrap_or("");
        tracing::info!(
            target: "secretary_mcp::audit",
            event = "note_write",
            verb,
            slug,
            id,
            chars,
            caller = %self.caller.name,
        );
        let list = note_items(n)
            .map(|items| format!(" (checklist, {})", format::progress(&items)))
            .unwrap_or_default();
        format!(
            "{verb} note \"{slug}\" ({title}){list}, {chars} characters in total. The owner sees it in the Secretary app under Notes; Lucina and note_get find it by the slug \"{slug}\"."
        )
    }

    pub async fn note_save(&self, args: &JsonObject) -> ToolResult {
        let a: NoteSaveArgs = parse(args)?;
        let title = check_text("title", &a.title, 200)?;
        let items = a
            .items
            .as_deref()
            .map(|i| items_json("items", i))
            .transpose()?;
        let content = match (a.content.as_deref(), &items) {
            (Some(c), Some(_)) if c.trim().is_empty() => None,
            (Some(c), _) => Some(note_content("content", c)?),
            (None, Some(_)) => None,
            (None, None) => {
                return Err(bad(
                    "`content` must not be empty (or give `items` for a checklist)",
                ))
            }
        };
        let tags = note_tags(a.tags.as_deref())?;
        let labels = a
            .labels
            .as_deref()
            .map(|l| label_names("labels", l))
            .transpose()?;
        let color = a.color.as_deref().map(keep_color).transpose()?;
        let slug = match a.slug.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => Some(note_key(Some(s), None)?),
            None => None,
        };
        // The fields both a new note and a replacement carry.
        let mut keep = json!({});
        if let Some(t) = &tags {
            keep["tags"] = json!(t);
        }
        if let Some(l) = &labels {
            keep["labels"] = json!(l);
        }
        if let Some(c) = &color {
            keep["color"] = json!(c);
        }
        if let Some(p) = a.pinned {
            keep["pinned"] = json!(p);
        }
        if let Some(r) = a.archived {
            keep["archived"] = json!(r);
        }
        if let Some(i) = &items {
            keep["items"] = json!(i);
        }
        let mut body = json!({"title": title, "source": self.caller.name});
        if let Some(c) = &content {
            body["content"] = json!(c);
        }
        if let Some(s) = &slug {
            body["slug"] = json!(s);
        }
        merge_into(&mut body, &keep);
        match self.app.backend.post("/v1/notes", &body).await {
            Ok(n) => Ok(self.note_saved("Saved", &n)),
            Err(e) if e.code == "slug_taken" => {
                let slug = slug.unwrap_or_default();
                if !a.replace.unwrap_or(false) {
                    let existing = self
                        .app
                        .backend
                        .get(&format!("/v1/notes/{}", seg(&slug)), &[])
                        .await
                        .ok();
                    let what = existing
                        .as_ref()
                        .and_then(|n| n.get("title").and_then(Value::as_str))
                        .map(|t| format!(" (\"{t}\")"))
                        .unwrap_or_default();
                    return Err(bad(format!(
                        "A note named \"{slug}\"{what} already exists. Call note_save again with replace=true to overwrite it, use note_append (or note_update) to add to it, or pick another slug."
                    )));
                }
                let mut patch = json!({"title": body["title"], "source": self.caller.name});
                if let Some(c) = &content {
                    patch["content"] = json!(c);
                }
                merge_into(&mut patch, &keep);
                let n = self
                    .app
                    .backend
                    .patch(&format!("/v1/notes/{}", seg(&slug)), &patch)
                    .await?;
                Ok(self.note_saved("Replaced", &n))
            }
            Err(e) => Err(e.into()),
        }
    }

    pub async fn note_append(&self, args: &JsonObject) -> ToolResult {
        let a: NoteAppendArgs = parse(args)?;
        let key = note_key(a.slug.as_deref(), a.id.as_deref())?;
        let content = note_content("content", &a.content)?;
        let mut body = json!({"content": content, "source": self.caller.name});
        if !a.continuation.unwrap_or(false) {
            body["separator"] = json!("\n\n");
        }
        match self
            .app
            .backend
            .post(&format!("/v1/notes/{}/append", seg(&key)), &body)
            .await
        {
            Ok(n) => Ok(self.note_saved("Appended to", &n)),
            Err(e) if e.code == "note_not_found" => Err(bad(format!(
                "No note \"{key}\". Create it with note_save first (notes_search lists the existing ones)."
            ))),
            Err(e) if e.code == "note_too_large" => Err(bad(
                "That would make the note larger than 256 KB; start a second note (e.g. slug \"<name>-2\") for the rest.",
            )),
            Err(e) => Err(e.into()),
        }
    }

    /// `GET /v1/notes/{key}`, with "no such note" as a message the model can act on.
    async fn fetch_note(&self, key: &str) -> Result<Value, ToolError> {
        match self
            .app
            .backend
            .get(&format!("/v1/notes/{}", seg(key)), &[])
            .await
        {
            Ok(n) => Ok(n),
            Err(e) if e.code == "note_not_found" => Err(bad(format!(
                "No note \"{key}\". notes_search finds notes by words or lists the latest ones."
            ))),
            Err(e) => Err(e.into()),
        }
    }

    /// A full note carries its labels as stored tags; add the display names
    /// (`labels`) from the owner's label registry, when it can be read.
    async fn with_label_names(&self, n: &mut Value) {
        if n.get("labels").is_some_and(Value::is_array) {
            return;
        }
        let tags: Vec<String> = n
            .get("tags")
            .and_then(Value::as_array)
            .map(|t| {
                t.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if tags.is_empty() {
            return;
        }
        let Ok(reg) = self.app.backend.get("/v1/notes/labels", &[]).await else {
            return;
        };
        let known = reg
            .get("labels")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let names: Vec<String> = tags
            .iter()
            .map(|t| {
                known
                    .iter()
                    .find(|l| l.get("tag").and_then(Value::as_str) == Some(t))
                    .and_then(|l| l.get("name").and_then(Value::as_str))
                    .unwrap_or(t)
                    .to_owned()
            })
            .collect();
        n["labels"] = json!(names);
    }

    pub async fn note_get(&self, args: &JsonObject) -> ToolResult {
        let a: NoteGetArgs = parse(args)?;
        let key = note_key(a.slug.as_deref(), a.id.as_deref())?;
        let max = a.max_chars.unwrap_or(40_000).clamp(1000, 100_000);
        let mut n = self.fetch_note(&key).await?;
        self.with_label_names(&mut n).await;
        Ok(format::note(
            &n,
            a.offset.unwrap_or(0),
            max,
            self.app.cfg.timezone,
        ))
    }

    /// PATCH a note, editing from the version we hold; on `409 conflict`
    /// (somebody changed it since) fetch the new version and edit that, once.
    async fn patch_note(
        &self,
        key: &str,
        mut note: Value,
        build: impl Fn(&Value) -> Result<Value, ToolError>,
    ) -> Result<Value, ToolError> {
        let path = format!("/v1/notes/{}", seg(key));
        let mut retried = false;
        loop {
            let mut body = build(&note)?;
            if let Some(u) = note.get("updatedAt") {
                body["baseUpdatedAt"] = u.clone();
            }
            match self.app.backend.patch(&path, &body).await {
                Ok(n) => return Ok(n),
                Err(e) if e.code == "conflict" && !retried => {
                    retried = true;
                    note = self.fetch_note(key).await?;
                }
                Err(e) if e.code == "conflict" => {
                    return Err(bad(
                        "The note was changed by someone else while this edit was being made. Read it again with note_get and repeat the change.",
                    ))
                }
                Err(e) if e.status == 502 && body.get("reminder").is_some() => {
                    return Err(bad(
                        "Lucina could not be reached to schedule the reminder, so the note is unchanged. Try again later.",
                    ))
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    pub async fn note_update(&self, args: &JsonObject) -> ToolResult {
        let a: NoteUpdateArgs = parse(args)?;
        let key = note_key(a.slug.as_deref(), a.id.as_deref())?;
        // Everything that can be checked without the note.
        let mut fields = json!({});
        let mut changes: Vec<String> = Vec::new();
        if let Some(t) = &a.title {
            fields["title"] = json!(check_text("title", t, 200)?);
            changes.push("title".into());
        }
        if let Some(c) = &a.content {
            fields["content"] = json!(note_content("content", c)?);
            changes.push("content".into());
        }
        if let Some(c) = &a.color {
            let c = keep_color(c)?;
            changes.push(format!("colour {c}"));
            fields["color"] = json!(c);
        }
        if let Some(p) = a.pinned {
            fields["pinned"] = json!(p);
            changes.push(if p { "pinned" } else { "unpinned" }.into());
        }
        if let Some(r) = a.archived {
            fields["archived"] = json!(r);
            changes.push(if r { "archived" } else { "unarchived" }.into());
        }
        let add_labels = label_names("addLabels", a.add_labels.as_deref().unwrap_or_default())?;
        let remove_labels = label_names(
            "removeLabels",
            a.remove_labels.as_deref().unwrap_or_default(),
        )?;
        if !add_labels.is_empty() {
            changes.push(format!("label +{}", add_labels.join(", +")));
        }
        if !remove_labels.is_empty() {
            changes.push(format!("label -{}", remove_labels.join(", -")));
        }
        let add_items = items_json("addItems", a.add_items.as_deref().unwrap_or_default())?;
        let (check, uncheck, remove) = (
            item_refs(a.check.as_deref()),
            item_refs(a.uncheck.as_deref()),
            item_refs(a.remove_items.as_deref()),
        );
        let has_ops =
            !(check.is_empty() && uncheck.is_empty() && remove.is_empty() && add_items.is_empty());
        if has_ops && a.content.is_some() {
            return Err(bad(
                "Give either `content` or the checklist operations (check, uncheck, addItems, removeItems), not both.",
            ));
        }
        if let Some(Some(at)) = &a.reminder_at {
            if chrono::DateTime::parse_from_rfc3339(at).is_err() {
                return Err(bad(
                    "`reminderAt` must be RFC 3339 with a UTC offset, e.g. 2026-11-03T09:00:00+01:00 (or null to clear the reminder)",
                ));
            }
        }
        let reminder_change = a.reminder_at.is_some() || a.reminder_repeat.is_some();
        if fields.as_object().is_some_and(|f| f.is_empty())
            && !reminder_change
            && !has_ops
            && add_labels.is_empty()
            && remove_labels.is_empty()
            && a.convert.is_none()
        {
            return Err(bad(
                "Nothing to change: pass at least one of title, content, color, pinned, archived, addLabels, removeLabels, reminderAt, check, uncheck, addItems, removeItems, convert.",
            ));
        }

        let mut note = self.fetch_note(&key).await?;
        // Convert first: the item operations then work on the converted note.
        if let Some(c) = &a.convert {
            let is_list = note.get("items").is_some_and(Value::is_array);
            let want_list = matches!(c, ConvertArg::List);
            if want_list != is_list {
                let how = if want_list { "list" } else { "text" };
                note = self
                    .patch_note(&key, note, |_| Ok(json!({"convert": how})))
                    .await?;
                changes.insert(0, format!("converted to {how}"));
            }
        }
        // The checklist edits, resolved once against the items as note_get showed them.
        let plan = if has_ops {
            let Some(items) = note_items(&note) else {
                return Err(bad(
                    "This is a text note, not a checklist. Pass convert=\"list\" to make it one, or use note_append to add text.",
                ));
            };
            let plan =
                keep::Plan::new(&items, &check, &uncheck, &remove, add_items).map_err(bad)?;
            changes.push(plan.summary());
            plan
        } else {
            keep::Plan::default()
        };
        let needs_patch = fields.as_object().is_some_and(|f| !f.is_empty())
            || reminder_change
            || !plan.is_empty()
            || !add_labels.is_empty()
            || !remove_labels.is_empty();
        if needs_patch {
            let reminder_text = match &a.reminder_at {
                Some(Some(at)) => Some(format!(
                    "reminder {}",
                    format::local_time(Some(&json!(at)), self.app.cfg.timezone)
                )),
                Some(None) => Some("reminder cleared".to_owned()),
                None if a.reminder_repeat.is_some() => Some("reminder repeat".to_owned()),
                None => None,
            };
            note = self
                .patch_note(&key, note, |cur| {
                    let mut body = fields.clone();
                    if !add_labels.is_empty() || !remove_labels.is_empty() {
                        let tags: Vec<String> = cur
                            .get("tags")
                            .and_then(Value::as_array)
                            .map(|t| {
                                t.iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default();
                        body["labels"] =
                            json!(keep::merge_labels(&tags, &add_labels, &remove_labels));
                    }
                    if !plan.is_empty() {
                        let items = note_items(cur).ok_or_else(|| {
                            bad("The note is no longer a checklist; read it again with note_get.")
                        })?;
                        body["items"] = json!(plan.apply(&items));
                    }
                    if reminder_change {
                        body["reminder"] = self.reminder_value(&a, cur)?;
                    }
                    Ok(body)
                })
                .await?;
            changes.extend(reminder_text);
        }
        let slug = note.get("slug").and_then(Value::as_str).unwrap_or("?");
        let title = note.get("title").and_then(Value::as_str).unwrap_or("");
        let id = note.get("id").and_then(Value::as_str).unwrap_or("");
        tracing::info!(
            target: "secretary_mcp::audit",
            event = "note_update",
            slug,
            id,
            changes = %changes.join("; "),
            caller = %self.caller.name,
        );
        let mut out = format!("Updated note \"{slug}\" ({title}): {}.", changes.join(", "));
        if let Some(items) = note_items(&note) {
            out.push_str(&format!(
                "\nChecklist, {}:\n{}",
                format::progress(&items),
                format::checklist(&items)
            ));
        }
        Ok(out)
    }

    /// The `reminder` object to send: the new time (or the current one), and
    /// the repeat asked for (or the current one).
    fn reminder_value(&self, a: &NoteUpdateArgs, cur: &Value) -> Result<Value, ToolError> {
        if let Some(None) = a.reminder_at {
            return Ok(Value::Null);
        }
        let cur_r = cur.get("reminder").filter(|r| r.is_object());
        let at = match &a.reminder_at {
            Some(Some(at)) => at.clone(),
            _ => cur_r
                .and_then(|r| r.get("at").and_then(Value::as_str))
                .map(str::to_owned)
                .ok_or_else(|| bad("This note has no reminder yet: give `reminderAt`."))?,
        };
        let repeat = match &a.reminder_repeat {
            Some(RepeatArg::None) => None,
            Some(RepeatArg::Daily) => Some("daily".to_owned()),
            Some(RepeatArg::Weekly) => Some("weekly".to_owned()),
            Some(RepeatArg::Monthly) => Some("monthly".to_owned()),
            Some(RepeatArg::Yearly) => Some("yearly".to_owned()),
            None => cur_r
                .and_then(|r| r.get("recurrence").and_then(Value::as_str))
                .map(str::to_owned),
        };
        let mut r = json!({"at": at});
        if let Some(rec) = repeat {
            r["recurrence"] = json!(rec);
        }
        Ok(r)
    }

    pub async fn note_labels(&self) -> ToolResult {
        let v = self.app.backend.get("/v1/notes/labels", &[]).await?;
        Ok(format::note_labels(&v))
    }

    /// Library tools use typed arguments and the same authenticated backend as notes.
    pub async fn library(&self, name: &str, args: &JsonObject) -> ToolResult {
        let v = match name {
            "library_list_books" | "library_search" => {
                let a: LibraryListArgs = parse(args)?;
                let mut q = vec![("limit", library_limit(a.limit)?.to_string())];
                if let Some(words) = a.q {
                    q.push(("q", check_text("q", &words, 200)?));
                }
                if let Some(kind) = a.kind {
                    q.push((
                        "kind",
                        match kind {
                            LibraryKind::Book => "book",
                            LibraryKind::Dictionary => "dictionary",
                            LibraryKind::Document => "document",
                        }
                        .into(),
                    ));
                }
                if let Some(status) = a.status {
                    q.push((
                        "status",
                        match status {
                            LibraryStatus::Unknown => "unknown",
                            LibraryStatus::NotStarted => "not_started",
                            LibraryStatus::Reading => "reading",
                            LibraryStatus::Completed => "completed",
                            LibraryStatus::Abandoned => "abandoned",
                        }
                        .to_owned(),
                    ));
                }
                if let Some(on) = a.on_kindle {
                    q.push(("onKindle", on.to_string()));
                }
                self.app
                    .backend
                    .library_get(
                        if name == "library_search" {
                            "/v1/library/search"
                        } else {
                            "/v1/library/books"
                        },
                        &q,
                    )
                    .await?
            }
            "library_get_recent_reading" => {
                let a: LibraryRecentArgs = parse(args)?;
                let days = a.days.unwrap_or(7);
                if !(1..=3650).contains(&days) {
                    return Err(bad("days must be 1-3650"));
                }
                self.app
                    .backend
                    .library_get("/v1/library/recent", &[("days", days.to_string())])
                    .await?
            }
            "library_sync_status" => {
                let _: NoArgs = parse(args)?;
                self.app
                    .backend
                    .library_get("/v1/library/sync", &[])
                    .await?
            }
            _ => {
                let a: LibraryBookArgs = parse(args)?;
                library_id(&a.id)?;
                let mut q = vec![("limit", library_limit(a.limit)?.to_string())];
                if let Some(words) = a.q {
                    q.push(("q", check_text("q", &words, 200)?));
                }
                let path = format!("/v1/library/books/{}", seg(&a.id));
                if name == "library_generate_brief" {
                    let generated = self.app.backend.library_brief(&a.id).await?;
                    let book = self.app.backend.library_get(&path, &[]).await?;
                    return Ok(format_library_brief(&a.id, &generated, &book));
                }
                if name == "library_find_related_notes" {
                    let v = self
                        .app
                        .backend
                        .library_get(&format!("{path}/notes"), &[("limit", q[0].1.clone())])
                        .await?;
                    return Ok(format_related_notes(&a.id, &v));
                }
                if name == "library_get_highlights" {
                    let mut value = self
                        .app
                        .backend
                        .library_get(&format!("{path}/highlights"), &q)
                        .await?;
                    value["bookId"] = json!(a.id);
                    value
                } else {
                    self.app.backend.library_get(&path, &[]).await?
                }
            }
        };
        Ok(format_library(name, &v))
    }

    pub async fn notes_search(&self, args: &JsonObject) -> ToolResult {
        let a: NotesSearchArgs = parse(args)?;
        let limit = a.limit.unwrap_or(20).clamp(1, 50);
        let mut q = vec![("limit", limit.to_string())];
        let mut what = String::new();
        let words = a.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if let Some(w) = words {
            q.push(("q", check_text("q", w, 200)?));
            what.push_str(&format!(" matching {w:?}"));
        }
        let label = a
            .label
            .as_deref()
            .or(a.tag.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(l) = label {
            let l = check_text("label", l.trim_start_matches('#'), 50)?;
            q.push(("tag", keep::label_tag(&l)));
            what.push_str(&format!(" with label {l:?}"));
        }
        if let Some(c) = a.color.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let c = keep_color(c)?;
            what.push_str(&format!(" in colour {c}"));
            q.push(("color", c));
        }
        if let Some(k) = a.kind {
            let k = match k {
                NoteKindArg::List => "list",
                NoteKindArg::Text => "text",
                NoteKindArg::Image => "image",
                NoteKindArg::Audio => "audio",
                NoteKindArg::Link => "link",
                NoteKindArg::Reminder => "reminder",
            };
            q.push(("type", k.to_owned()));
            what.push_str(&format!(" of type {k}"));
        }
        match a.archived {
            Some(ArchivedArg::Exclude) => {
                q.push(("archived", "exclude".into()));
                what.push_str(" (archive excluded)");
            }
            Some(ArchivedArg::Only) => {
                q.push(("archived", "only".into()));
                what.push_str(" (archived only)");
            }
            Some(ArchivedArg::Include) | None => {}
        }
        let v = self.app.backend.get("/v1/notes", &q).await?;
        Ok(format::notes_list(&v, &what, self.app.cfg.timezone))
    }

    /// `diagnostics_recent` / `diagnostics_get` / `diagnostics_file`.
    pub async fn diagnostics(&self, name: &str, args: &JsonObject) -> ToolResult {
        let d = self.app.diagnostics.as_ref().ok_or_else(|| {
            bad("Diagnostics are not configured on this server (SECRETARY_MCP_DIAGNOSTICS_URL).")
        })?;
        let tz = self.app.cfg.timezone;
        match name {
            "diagnostics_recent" => d.recent(parse(args)?, tz).await,
            "diagnostics_get" => d.get(parse(args)?, tz).await,
            "diagnostics_file" => d.file(parse(args)?).await,
            _ => Err(bad(format!("unknown tool {name}"))),
        }
    }

    pub async fn status(&self) -> ToolResult {
        let health = self.app.backend.health().await;
        let token = if self.app.backend.has_device_token() {
            Some(self.app.backend.token_accepted().await)
        } else {
            None
        };
        Ok(format::status(&health, token, self.caller, self.app))
    }
}

/// Library ids are a single path segment with a fixed digest length.
fn library_id(id: &str) -> Result<(), ToolError> {
    if id.len() != 19
        || !id.starts_with("bk_")
        || !id.as_bytes()[3..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
    {
        return Err(bad("id must be bk_ followed by 16 lowercase hex digits"));
    }
    Ok(())
}
fn library_limit(n: Option<u32>) -> Result<u32, ToolError> {
    let n = n.unwrap_or(20);
    if !(1..=50).contains(&n) {
        return Err(bad("limit must be 1-50"));
    }
    Ok(n)
}
fn library_str<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("unknown")
}
fn library_cite(id: &str, h: &Value) -> String {
    let mut c = id.to_owned();
    if let Some(p) = h.get("page").and_then(Value::as_str) {
        c.push_str(&format!(" p.{p}"));
    }
    if let Some(l) = h.get("location").filter(|l| !l.is_null()) {
        c.push_str(&format!(" loc {}-{}", l["start"], l["end"]));
    }
    format!(
        "[{c}, {}]",
        h.get("source").and_then(Value::as_str).unwrap_or("library")
    )
}
fn library_quote(v: &Value) -> String {
    library_str(v, "text").chars().take(600).collect()
}
fn format_library_brief(id: &str, brief: &Value, book: &Value) -> String {
    let mut out = format!(
        "Generated {} brief (not the owner's words):\n",
        library_str(brief, "kind")
    );
    if let Some(ids) = brief["basedOn"].as_array() {
        for hid in ids {
            if let Some(h) = book["highlights"]
                .as_array()
                .and_then(|hs| hs.iter().find(|h| h["id"] == *hid))
            {
                out.push_str(&format!("{} {}\n", library_quote(h), library_cite(id, h)));
            }
        }
    }
    if out.lines().count() == 1 {
        out.push_str(&format!("No highlights available. [{id}, library]"));
    }
    out
}
/// Indexed Obsidian references with one citation each; says plainly when none exist.
fn format_related_notes(id: &str, v: &Value) -> String {
    let notes = v["notes"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let vaults = v["indexedVaults"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if notes.is_empty() {
        if vaults.is_empty() {
            return format!("No related notes: no Obsidian vault has been indexed, so relationships are unknown. [{id}, library]");
        }
        let names: Vec<String> = vaults
            .iter()
            .map(|x| {
                format!(
                    "{} at {}",
                    library_str(x, "vault"),
                    library_str(x, "generatedAt")
                )
            })
            .collect();
        return format!(
            "No indexed Obsidian note refers to this book (indexed: {}). [{id}, library]",
            names.join(", ")
        );
    }
    let mut out = String::from(
        "Obsidian notes that refer to this book (relations from the owner's notes; a mention is not evidence of reading):\n",
    );
    for n in notes.iter().take(20) {
        out.push_str(&format!(
            "- {} ({}): \"{}\" [obsidian:{}/{}#L{}]\n",
            library_str(n, "noteTitle"),
            library_str(n, "how"),
            n["excerpt"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(240)
                .collect::<String>(),
            library_str(n, "vault"),
            library_str(n, "notePath"),
            n["line"].as_u64().unwrap_or(0)
        ));
    }
    let total = v["total"].as_u64().unwrap_or(notes.len() as u64);
    if total > notes.len().min(20) as u64 {
        out.push_str(&format!(
            "Showing {} of {total} references.\n",
            notes.len().min(20)
        ));
    }
    out
}
fn format_library(name: &str, v: &Value) -> String {
    let mut out = String::new();
    if name == "library_sync_status" {
        return format!("Library sync: {v} [library, imports log]");
    }
    if let Some(b) = v.get("book") {
        let id = library_str(b, "id");
        match name {
            "library_get_progress" => {
                let progress = if b["progress"].is_null() {
                    "unknown".into()
                } else {
                    b["progress"].to_string()
                };
                return format!("{}: progress {progress}; status {}. Reading sessions unavailable on stock Kindle. [{id}, library]",library_str(b,"title"),library_str(b,"status"));
            }
            "library_get_brief" => {
                if let Some(briefs) = b["briefs"].as_array() {
                    for brief in briefs.iter().rev().take(3) {
                        out.push_str(&format_library_brief(id, brief, v));
                    }
                }
                if out.is_empty() {
                    return format!("No generated brief. [{id}, library]");
                }
                return out;
            }
            _ => {
                let progress = if b["progress"].is_null() {
                    "unknown".into()
                } else {
                    b["progress"].to_string()
                };
                out.push_str(&format!(
                    "{} — {}; status {}; progress {progress}. [{id}, library]\n",
                    library_str(b, "title"),
                    b["authors"],
                    library_str(b, "status")
                ));
            }
        }
    }

    for key in ["books", "results", "highlights", "changes"] {
        if let Some(rows) = v[key].as_array() {
            for r in rows.iter().take(20) {
                let id = r
                    .get("bookId")
                    .or_else(|| r.get("id"))
                    .and_then(Value::as_str)
                    .or_else(|| v["book"]["id"].as_str())
                    .or_else(|| v["bookId"].as_str())
                    .unwrap_or("unknown");
                let text = if key == "books" {
                    format!(
                        "{} — {}; status {}",
                        library_str(r, "title"),
                        r["authors"],
                        library_str(r, "status")
                    )
                } else if key == "results" {
                    library_str(r, "snippet").to_owned()
                } else if key == "changes" {
                    format!(
                        "Status {}; progress {}",
                        library_str(r, "status"),
                        r["progress"]
                    )
                } else {
                    format!(
                        "{}{}",
                        library_quote(r),
                        r["note"]
                            .as_str()
                            .map(|n| format!(
                                " (note: {})",
                                n.chars().take(300).collect::<String>()
                            ))
                            .unwrap_or_default()
                    )
                };
                out.push_str(&format!("{text} {}\n", library_cite(id, r)));
            }
            if rows.len() > 20 {
                out.push_str("Showing first 20 results.\n");
            }
        }
    }
    if name == "library_get_recent_reading" {
        out.push_str("Reading sessions unavailable on stock Kindle; reading time is unknown. [library, stock-kindle]\n");
    }
    if out.is_empty() {
        out = "No library records found. [library]".into();
    }
    out
}

#[cfg(test)]
mod library_tests {
    use super::*;
    #[test]
    fn library_specs_and_validation() {
        let tools: Vec<_> = specs()
            .into_iter()
            .filter(|s| s.name.starts_with("library_"))
            .collect();
        assert_eq!(tools.len(), 10);
        for s in tools {
            assert_eq!(
                s.scope,
                if s.name == "library_generate_brief" {
                    Scope::Write
                } else {
                    Scope::Read
                }
            );
        }
        for s in ["../bad", "bk_aaa", "bk_0123456789abcdeg"] {
            assert!(library_id(s).is_err());
        }
        assert!(library_id("bk_0123456789abcdef").is_ok());
        assert!(library_limit(Some(0)).is_err());
    }
    #[test]
    fn citations_and_unknowns() {
        assert_eq!(
            library_cite(
                "bk_a",
                &json!({"page":"12","location":{"start":170,"end":172},"source":"kindle-clippings"})
            ),
            "[bk_a p.12 loc 170-172, kindle-clippings]"
        );
        assert!(
            format_related_notes("bk_a", &json!({"notes":[],"indexedVaults":[]}))
                .contains("no Obsidian vault has been indexed")
        );
        assert!(format_related_notes(
            "bk_a",
            &json!({"notes":[],"indexedVaults":[{"vault":"Vault","generatedAt":"t"}]})
        )
        .contains("No indexed Obsidian note refers to this book (indexed: Vault at t)"));
        let text = format_related_notes(
            "bk_a",
            &json!({"total":1,"notes":[{"vault":"Vault","notePath":"Ideas/A note.md","noteTitle":"A note","how":"mention","excerpt":"the book","line":7}],"indexedVaults":[]}),
        );
        assert!(
            text.contains("- A note (mention): \"the book\" [obsidian:Vault/Ideas/A note.md#L7]"),
            "{text}"
        );
        assert!(text.contains("not evidence of reading"));
        assert!(format_library(
            "library_get_progress",
            &json!({"book":{"id":"bk_a","title":"Book","status":"unknown","progress":null}})
        )
        .contains("progress unknown"));
    }
}

#[cfg(test)]
mod decision_tests {
    use super::*;
    #[test]
    fn decision_argument_contract() {
        let base = json!({"question":"Ship?", "options":["Ship", "Wait"], "reversible":true, "impact":"low"});
        for recommendation in [" ship ", "2"] {
            let mut args = base.clone();
            args["recommendation"] = json!(recommendation);
            args["wait_seconds"] = json!(100);
            serde_json::from_value::<DecideArgs>(args)
                .unwrap()
                .validate()
                .unwrap();
        }
        for (field, value) in [
            ("question", json!("")),
            ("question", json!("é".repeat(2001))),
            ("options", json!(["only"])),
            ("options", json!(["a", "b", "c", "d", "e", "f", "g"])),
            ("options", json!(["Ship", " SHIP "])),
            ("options", json!(["x".repeat(201), "Wait"])),
            ("recommendation", json!("3")),
            ("recommendation", json!("other")),
            ("context", json!("x".repeat(8001))),
            ("topic", json!("x".repeat(65))),
            ("asking_session", json!("x".repeat(121))),
            ("wait_seconds", json!(101)),
        ] {
            let mut args = base.clone();
            args[field] = value;
            assert!(
                serde_json::from_value::<DecideArgs>(args)
                    .unwrap()
                    .validate()
                    .is_err(),
                "{field}"
            );
        }
        let mut boundary = base.clone();
        boundary["question"] = json!("é".repeat(2000));
        boundary["context"] = json!("x".repeat(8000));
        boundary["topic"] = json!("x".repeat(64));
        boundary["asking_session"] = json!("x".repeat(120));
        boundary["wait_seconds"] = json!(0);
        serde_json::from_value::<DecideArgs>(boundary)
            .unwrap()
            .validate()
            .unwrap();
        for (field, value) in [
            ("asking_client", json!("spoof")),
            ("extra", json!(1)),
            ("wait_seconds", json!(-1)),
            ("impact", json!("unknown")),
            ("urgency", json!("medium")),
        ] {
            let mut args = base.clone();
            args[field] = value;
            assert!(serde_json::from_value::<DecideArgs>(args).is_err());
        }
    }

    #[test]
    fn prefixes() {
        assert_eq!(
            ask_prefix("claude-code"),
            "[via Lucina MCP, from claude-code] "
        );
        assert_eq!(
            ask_prefix_allow(
                "claude-code",
                &[AllowArg::Tasks, AllowArg::Notes, AllowArg::Notes]
            ),
            "[via Lucina MCP, from claude-code; allow=notes,tasks] "
        );
        assert_eq!(
            ask_prefix_allow("claude-code", &[]),
            "[via Lucina MCP, from claude-code; allow=] "
        );
    }
}
