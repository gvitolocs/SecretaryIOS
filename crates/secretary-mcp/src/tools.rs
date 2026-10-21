//! The MCP tools: definitions (JSON Schemas, annotations, scopes) and their
//! implementations against secretary-server.
//!
//! Safety: no tool buys anything, messages third parties or deletes data.
//! `reminder_cancel` only moves a pending reminder to `cancelled` (Lucina
//! keeps the record). Everything else that could act in the world goes
//! through `ask_lucina`, i.e. through Lucina's own pipeline and its own
//! confirmation flow.

use std::sync::Arc;

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::backend::{seg, BackendError};
use crate::format;
use crate::scope::{Caller, Scope};
use crate::App;

// -- arguments ------------------------------------------------------------------

/// Arguments of `ask_lucina`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskArgs {
    /// What to say to Lucina, in any language (she answers in the language you write in). Up to 3800 characters.
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
            "Send a message to Lucina, the owner's personal AI assistant, and get her reply. Lucina knows the owner (long-term memory), their reminders, shopping list and recent conversations on every channel, and can act through her own tools (reminders, shopping, research jobs, images). Use it for anything that needs Lucina's judgement or memory. She is told which client is asking. Actions with consequences (purchases, posts, messages to other people) go through Lucina's own confirmation flow. A turn can take up to two minutes; long jobs finish in the background and show up in lucina_timeline."),
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
        spec!("gym_summary", "Gym summary", Scope::Read, ro = true, destructive = false, idempotent = true, GymArgs,
            "The owner's training: workouts this week vs. the weekly goal, recent workouts with volume, exercises and personal records, and weekly volume."),
        spec!("body_summary", "Body weight summary", Scope::Read, ro = true, destructive = false, idempotent = true, NoArgs,
            "The owner's body weight: current weight, 7-day average, weekly change, 30/90-day trend, goal distance, and the latest entries."),
        spec!("lucina_status", "Lucina status", Scope::Read, ro = true, destructive = false, idempotent = true, NoArgs,
            "Health of the Secretary gateway and of Lucina (reachable, latency), the owner's time zone and current local time, and which client and scopes this connection has."),
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

const MAX_ASK_CHARS: usize = 3800;

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
            // Ignored by secretary-server today (see the TODO in backend::chat).
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
