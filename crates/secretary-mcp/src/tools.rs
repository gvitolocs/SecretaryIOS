//! The MCP tools: definitions (JSON Schemas, annotations, scopes) and their
//! implementations against secretary-server.
//!
//! Safety: no tool buys anything, messages third parties or deletes data.
//! `reminder_cancel` only moves a pending reminder to `cancelled` (Lucina
//! keeps the record). Everything else that could act in the world goes
//! through `ask_lucina`, i.e. through Lucina's own pipeline and its own
//! confirmation flow. Notes are the owner's own local data: `note_save` and
//! `note_append` write without a confirmation step, and every write is
//! logged with the caller.

use std::sync::Arc;

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::backend::{seg, BackendError};
use crate::format;
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

/// Arguments of `note_save`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NoteSaveArgs {
    /// Title shown in the app, 1-200 characters, e.g. "CRAFT-IT4SD — meeting with Marianne & Jonas".
    pub title: String,
    /// The note in Markdown (headings, lists, links). Up to 256 KB in total; for very long documents send the first part here and the rest with note_append.
    pub content: String,
    /// Short name to find it again, e.g. "craftit-meeting": lower-case letters, digits and '-', up to 64. Default: made from the title. Lucina and note_get find the note by this name.
    #[serde(default)]
    pub slug: Option<String>,
    /// Up to 16 tags, e.g. ["work", "aarhus"].
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    /// Pin it to the top of the owner's notes (default false).
    #[serde(default)]
    pub pinned: Option<bool>,
    /// When a note with this slug exists: true replaces its title, content (and tags/pin if given); false (default) refuses so nothing is overwritten by accident.
    #[serde(default)]
    pub replace: Option<bool>,
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

/// Arguments of `notes_search`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NotesSearchArgs {
    /// Words to look for in titles, slugs, tags and content. Empty: list the latest notes.
    #[serde(default)]
    pub q: Option<String>,
    /// Only notes with this tag.
    #[serde(default)]
    pub tag: Option<String>,
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

impl ToolSpec {
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
            "Send a message to Lucina, the owner's personal AI assistant, and get her reply. Lucina knows the owner (long-term memory), their reminders, shopping list, notes and recent conversations on every channel, and can act through her own tools (reminders, shopping, research jobs, images). Use it for anything that needs Lucina's judgement or memory. She is told which client is asking. Actions with consequences (purchases, posts, messages to other people) go through Lucina's own confirmation flow. Messages are limited to 12000 characters: do NOT paste long documents, briefings or context here; save them with note_save (and note_append) and then tell Lucina the note's slug. A turn can take up to two minutes; long jobs finish in the background and show up in lucina_timeline."),
        spec!("lucina_timeline", "Lucina timeline", Scope::Read, ro = true, destructive = false, idempotent = true, TimelineArgs,
            "Recent messages between the owner and Lucina on all channels (Telegram, Discord, web app, iOS app, Alexa, MCP), newest first, with time, channel and who spoke. Paginate with `before`."),
        spec!("shopping_list", "Shopping list", Scope::Read, ro = true, destructive = false, idempotent = true, ShoppingListArgs,
            "Show a shopping list: what is still needed (with item ids) and what was bought recently. Also lists the other lists by name."),
        spec!("shopping_add", "Add to shopping list", Scope::Write, ro = false, destructive = false, idempotent = false, ShoppingAddArgs,
            "Put items on the owner's shopping list (default list unless `list` is given). Items can be plain names or {name, spec, quantity, unit}."),
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
            "Propose a new event for the owner's Google Calendar. It is NOT created directly: it waits for the owner's confirmation in the Secretary app (Links), and the owner can accept or dismiss it. Tell the owner to confirm it there."),
        spec!("gym_summary", "Gym summary", Scope::Read, ro = true, destructive = false, idempotent = true, GymArgs,
            "The owner's training: workouts this week vs. the weekly goal, recent workouts with volume, exercises and personal records, and weekly volume."),
        spec!("body_summary", "Body weight summary", Scope::Read, ro = true, destructive = false, idempotent = true, NoArgs,
            "The owner's body weight: current weight, 7-day average, weekly change, 30/90-day trend, goal distance, and the latest entries."),
        spec!("note_save", "Save note", Scope::Write, ro = false, destructive = false, idempotent = false, NoteSaveArgs,
            "Save a note, briefing or document for the owner: it appears in the Secretary app (Notes, with your name as the source) and Lucina can recall it by its slug, e.g. \"craftit-meeting\". Use this, not ask_lucina, for anything long or worth keeping. Markdown content, up to 256 KB per note; for a document too long for one call, save the first part here and send the rest in order with note_append. An existing slug is only overwritten with replace=true. Answers the slug to use later."),
        spec!("note_append", "Append to note", Scope::Write, ro = false, destructive = false, idempotent = false, NoteAppendArgs,
            "Add text at the end of an existing note (by slug or id): the next part of a long document, an update to a briefing, meeting minutes. With continuation=true the text is glued on exactly (for a chunk cut mid-sentence); otherwise it starts a new paragraph."),
        spec!("note_get", "Read note", Scope::Read, ro = true, destructive = false, idempotent = true, NoteGetArgs,
            "Read one of the owner's notes by slug (e.g. \"craftit-meeting\") or id: title, tags, who wrote it, when, and the Markdown content. Long notes come in pages: call again with the offset it gives you."),
        spec!("notes_search", "Search notes", Scope::Read, ro = true, destructive = false, idempotent = true, NotesSearchArgs,
            "Find the owner's notes, briefings and documents by words (title, slug, tags, content) or tag; pinned first, newest first. Each hit shows its slug, title, source and an excerpt. Without `q` it lists the latest notes."),
        spec!("tasks_list", "Tasks", Scope::Read, ro = true, destructive = false, idempotent = true, TasksListArgs,
            "The owner's tasks (synced with Todoist): by default Today with the overdue ones first, like Todoist's Today view; or upcoming, inbox, a project, a search or Todoist filter, or what was completed. Each task shows its id, project, due date (↻ = recurring), priority and labels, plus the daily goal."),
        spec!("task_add", "Add task", Scope::Write, ro = false, destructive = false, idempotent = false, TaskAddArgs,
            "Add a task to the owner's Todoist through Secretary, written like Todoist's quick add: dates and recurrence in Italian, English or Danish (\"domani alle 9\", \"every day\", \"hver mandag\"), #Project, @label, p1-p4. It shows up at once and syncs to Todoist within a minute."),
        spec!("task_complete", "Complete task", Scope::Write, ro = false, destructive = false, idempotent = false, TaskCompleteArgs,
            "Mark one of the owner's tasks done (by id from tasks_list, or by words of its title). A recurring task moves to its next date in Todoist. Deleting tasks is not possible here; ask the owner."),
        spec!("lucina_status", "Lucina status", Scope::Read, ro = true, destructive = false, idempotent = true, NoArgs,
            "Health of the Secretary gateway and of Lucina (reachable, latency), the owner's time zone and current local time, and which client and scopes this connection has."),
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
            "Give Lucina's own Linux desktop (an isolated sandbox with Chromium, a terminal and files; no access to the owner's machines) a whole job in words; her computer-use engine does it and reports what it observed. Tasks run up to 200 s: this waits up to wait_seconds (default 45) and otherwise returns a job id for computer_task_status. One task at a time. Lucina's computer is an isolated Linux desktop sandbox (container `lucina-workspace` on nezopt; not nezopt's host, not the owner's Mac). Its only browser is Chromium. Use the Lucina MCP tools: `computer_screenshot` to see it, `computer_call` with the driver's `browser_*` tools (`start_session` {session}, `browser_prepare` {session, allow_launch: true, profile: {mode: isolated_named, name: lucina}} to open Chromium, `get_browser_state` {session, pid, window_id} on that window for its target_id and tab_id, then `browser_navigate` and the other browser_* tools with session, target_id and tab_id) or click/type_text/press_key, `computer_bash` for shell work, or `computer_task` to have Lucina do the whole job. Never install or launch Firefox."),
        spec!("computer_task_status", "Computer task status", Scope::Computer, ro = true, destructive = false, idempotent = true, crate::computer::JobArgs,
            "Wait for a computer_task job (up to wait_seconds, default 45) and return its result: status, what Lucina's engine observed, and her answer."),
        spec!("computer_task_cancel", "Cancel computer task", Scope::Computer, ro = false, destructive = false, idempotent = true, crate::computer::JobArgs,
            "Stop a running computer_task job. Steps already done in the sandbox are not undone."),
        spec!("computer_bash", "Computer shell", Scope::Computer, ro = false, destructive = false, idempotent = false, crate::computer::BashArgs,
            "Run one shell command as user `cua` in Lucina's Linux desktop sandbox (isolated container, no host mounts) and return its output. Chromium only; never install or launch Firefox."),
        spec!("computer_screenshot", "Computer screenshot", Scope::Computer, ro = true, destructive = false, idempotent = true, crate::computer::NoArgs,
            "The current screen of Lucina's Linux desktop (1280x800 PNG) and its visible windows with the pid and window_id that computer_call tools take."),
        spec!("computer_tools", "Computer driver tools", Scope::Computer, ro = true, destructive = false, idempotent = true, crate::computer::ToolsArgs,
            "List the desktop driver tools (click, type_text, press_key, scroll, launch_app, get_window_state, browser_*, …); pass name for the full input schemas of the matching tools. Chromium only; browser_* tools need start_session, browser_prepare and get_browser_state first (see computer_call)."),
        spec!("computer_call", "Computer driver call", Scope::Computer, ro = false, destructive = false, idempotent = false, crate::computer::CallArgs,
            "Call one desktop driver tool on Lucina's Linux desktop, e.g. click {pid, window_id, x, y} or {scope: \"desktop\", x, y}, type_text {text}, press_key {key}, launch_app. Chromium only; never install or launch Firefox. Prefer browser_* tools for web pages: start_session {session}, browser_prepare {session, allow_launch: true, profile: {mode: isolated_named, name: lucina}} to open Chromium, get_browser_state {session, pid, window_id} on that window for its target_id and tab_id, then browser_navigate and the other browser_* tools with session, target_id and tab_id. Refused while the owner drives the desktop from the app."),
        spec!("android_bash", "Android shell", Scope::Computer, ro = false, destructive = false, idempotent = false, crate::computer::BashArgs,
            "Run one shell command in Lucina's Android sandbox container and return its output."),
        spec!("android_screenshot", "Android screenshot", Scope::Computer, ro = true, destructive = false, idempotent = true, crate::computer::NoArgs,
            "The current screen of Lucina's Android sandbox and its visible windows."),
        spec!("android_tools", "Android driver tools", Scope::Computer, ro = true, destructive = false, idempotent = true, crate::computer::ToolsArgs,
            "List the Android sandbox's driver tools; pass name for full input schemas."),
        spec!("android_call", "Android driver call", Scope::Computer, ro = false, destructive = false, idempotent = false, crate::computer::CallArgs,
            "Call one driver tool on Lucina's Android sandbox (click, type_text, press_key, scroll, launch_app, …). Refused while the owner drives it from the app."),

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
    if tags.len() > 16 {
        return Err(bad("`tags`: at most 16"));
    }
    Ok(Some(
        tags.iter()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .collect(),
    ))
}

/// The prefix Lucina sees on MCP messages, naming the client.
pub fn ask_prefix(caller: &str) -> String {
    format!("[via Lucina MCP, from {caller}] ")
}

pub struct Tools<'a> {
    pub app: &'a App,
    pub caller: &'a Caller,
}

impl Tools<'_> {
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
            "message": format!("{}{msg}", ask_prefix(&self.caller.name)),
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
            let mut op =
                json!({"opId": uuid::Uuid::new_v4().to_string(), "type": "add", "name": name});
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
            names.push(match spec.map(str::trim).filter(|s| !s.is_empty()) {
                Some(s) => format!("{name} ({s})"),
                None => name,
            });
            ops.push(op);
        }
        let v2 = match self.list_id(a.list.as_deref()).await? {
            Ok(id) => self
                .app
                .backend
                .post(
                    &format!("/v1/shopping/lists/{}/ops", seg(&id)),
                    &json!({"ops": ops}),
                )
                .await
                .map(|r| (id, r)),
            Err(e) => Err(e),
        };
        match v2 {
            Ok((id, r)) => Ok(format::ops_result("Added", &names, &id, &r)),
            Err(e) if e.is_v2_unavailable() => {
                self.legacy_ok(a.list.as_deref(), e).await?;
                let mut added = Vec::new();
                for n in &names {
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
        format!(
            "{verb} note \"{slug}\" ({title}), {chars} characters in total. The owner sees it in the Secretary app under Notes; Lucina and note_get find it by the slug \"{slug}\"."
        )
    }

    pub async fn note_save(&self, args: &JsonObject) -> ToolResult {
        let a: NoteSaveArgs = parse(args)?;
        let title = check_text("title", &a.title, 200)?;
        let content = note_content("content", &a.content)?;
        let tags = note_tags(a.tags.as_deref())?;
        let slug = match a.slug.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => Some(note_key(Some(s), None)?),
            None => None,
        };
        let mut body = json!({"title": title, "content": content, "source": self.caller.name});
        if let Some(s) = &slug {
            body["slug"] = json!(s);
        }
        if let Some(t) = &tags {
            body["tags"] = json!(t);
        }
        if let Some(p) = a.pinned {
            body["pinned"] = json!(p);
        }
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
                        "A note named \"{slug}\"{what} already exists. Call note_save again with replace=true to overwrite it, use note_append to add to it, or pick another slug."
                    )));
                }
                let mut patch = json!({"title": body["title"], "content": body["content"], "source": self.caller.name});
                if let Some(t) = tags {
                    patch["tags"] = json!(t);
                }
                if let Some(p) = a.pinned {
                    patch["pinned"] = json!(p);
                }
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

    pub async fn note_get(&self, args: &JsonObject) -> ToolResult {
        let a: NoteGetArgs = parse(args)?;
        let key = note_key(a.slug.as_deref(), a.id.as_deref())?;
        let max = a.max_chars.unwrap_or(40_000).clamp(1000, 100_000);
        match self
            .app
            .backend
            .get(&format!("/v1/notes/{}", seg(&key)), &[])
            .await
        {
            Ok(n) => Ok(format::note(
                &n,
                a.offset.unwrap_or(0),
                max,
                self.app.cfg.timezone,
            )),
            Err(e) if e.code == "note_not_found" => Err(bad(format!(
                "No note \"{key}\". notes_search finds notes by words or lists the latest ones."
            ))),
            Err(e) => Err(e.into()),
        }
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
        let words = a.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if let Some(w) = words {
            q.push(("q", check_text("q", w, 200)?));
        }
        let tag = a.tag.as_deref().map(str::trim).filter(|s| !s.is_empty());
        if let Some(t) = tag {
            q.push(("tag", check_text("tag", t.trim_start_matches('#'), 32)?));
        }
        let v = self.app.backend.get("/v1/notes", &q).await?;
        Ok(format::notes_list(&v, words, tag, self.app.cfg.timezone))
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
