//! `reminders`: create, list and cancel the owner's reminders.
//!
//! Reads come from `reminders.json`; writes go through the configured
//! [`crate::reminder_writer::ReminderWriter`] (the Node Lucina in phase 1).
//! Cancelling needs the owner's confirmation.

use chrono::Utc;
use lucina_store::parser::{parse_reminder_message, ParseOptions};
use lucina_store::reminders::{filter_for_scope, format_confirmation, format_list, CancelOutcome, CreateReminder, Reminder};
use lucina_store::timeutil::{iso_ms, parse_js_date};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, Effect, SideEffect, Tool, ToolError, ToolResult};

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Action {
    Create,
    List,
    Cancel,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Scope {
    Upcoming,
    Today,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
struct Args {
    action: Action,
    /// What to remind (create).
    text: Option<String>,
    /// When, ISO 8601 with an offset, in the future (create), e.g. "2026-10-22T09:30:00+02:00".
    due_at: Option<String>,
    /// Instead of dueAt: the owner's own words ("alle 18:30 di chiamare la mamma", "in 2 hours to stretch").
    natural_text: Option<String>,
    /// list: upcoming (default) or today.
    scope: Option<Scope>,
    /// Reminder id (cancel).
    id: Option<String>,
}

fn owner_reminders(ctx: &Ctx) -> Result<Vec<Reminder>, ToolError> {
    let store = ctx.env.reminders.as_ref().ok_or_else(|| ToolError::new("reminders are not configured"))?;
    let chat = ctx.env.owner_chat_id.clone().unwrap_or_default();
    if chat.is_empty() {
        return Ok(Vec::new());
    }
    Ok(store
        .list(true)
        .map_err(|e| ToolError::new(format!("reminders unreadable: {e}")))?
        .into_iter()
        .filter(|r| r.chat_id == chat)
        .collect())
}

pub struct RemindersTool;

#[async_trait::async_trait]
impl Tool for RemindersTool {
    fn name(&self) -> &'static str {
        "reminders"
    }
    fn description(&self) -> &'static str {
        "The owner's timed reminders (delivered on Telegram): create one (text + dueAt, or naturalText), list upcoming/today, or cancel one by id."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::Local
    }
    fn needs_confirmation(&self, args: &Value) -> bool {
        args["action"] == "cancel"
    }
    fn summarize(&self, args: &Value) -> String {
        match args["action"].as_str() {
            Some("cancel") => format!("cancel reminder {}", args["id"].as_str().unwrap_or("?")),
            Some("create") => format!("reminder at {}", args["dueAt"].as_str().or(args["naturalText"].as_str()).unwrap_or("?")),
            _ => "list reminders".into(),
        }
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let env = &ctx.env;
        let tz = env.time_zone;
        match a.action {
            Action::List => {
                let all = owner_reminders(ctx)?;
                let today = a.scope == Some(Scope::Today);
                let mut pending = filter_for_scope(&all, today, tz, ctx.now);
                pending.sort_by_key(|r| r.due());
                ctx.push_effect(Effect::RemindersListed);
                Ok(json!({
                    "timeZone": tz.name(),
                    "reminders": pending.iter().map(Reminder::public).collect::<Vec<_>>(),
                    "formatted": format_list(&all, tz, &env.persona, today, ctx.now),
                }))
            }
            Action::Create => {
                if !env.reminders_enabled {
                    return Err(ToolError::new("reminders are disabled on this host"));
                }
                let chat = env.owner_chat_id.clone().filter(|c| !c.is_empty()).ok_or_else(|| ToolError::new("no owner Telegram chat is configured"))?;
                let writer = env.reminder_writer.as_ref().ok_or_else(|| ToolError::new("reminder writes are not configured"))?;
                let (text, due) = match (a.due_at.as_deref(), a.natural_text.as_deref()) {
                    (Some(d), _) => {
                        let text = a.text.clone().map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()).ok_or_else(|| ToolError::new("text is required with dueAt"))?;
                        let has_offset = d.ends_with('Z') || d.len() > 19 && matches!(d.as_bytes()[d.len() - 6], b'+' | b'-');
                        if !has_offset {
                            return Err(ToolError::new("dueAt needs a UTC offset, e.g. 2026-10-22T09:30:00+02:00"));
                        }
                        let due = parse_js_date(d).ok_or_else(|| ToolError::new("dueAt is not a valid ISO 8601 time"))?;
                        (text, due)
                    }
                    (None, Some(nat)) => {
                        let p = parse_reminder_message(nat, &ParseOptions { tz, default_hour: env.reminder_default_hour, default_minute: env.reminder_default_minute, now: ctx.now })
                            .map_err(|e| ToolError::new(format!("could not parse the reminder ({e}); pass text and dueAt")))?;
                        (a.text.clone().filter(|t| !t.trim().is_empty()).unwrap_or(p.text), p.due_at)
                    }
                    (None, None) => return Err(ToolError::new("dueAt (or naturalText) is required")),
                };
                if due <= Utc::now() {
                    return Err(ToolError::new("due_in_past: the reminder time is not in the future"));
                }
                let (created, reminder) = writer
                    .create(CreateReminder {
                        text,
                        due_at: iso_ms(due),
                        chat_id: chat,
                        user_peer_id: env.owner_peer_id.clone(),
                        user_label: env.user_label.clone(),
                        persona: env.persona.clone(),
                        time_zone: tz.name().to_owned(),
                        dedupe_key: None,
                        source: "secretary".into(),
                    })
                    .await
                    .map_err(|e| ToolError::new(e.to_string()))?;
                let confirmation = format_confirmation(&reminder, tz);
                ctx.push_effect(Effect::ReminderCreated { reminder: reminder.clone(), created });
                Ok(json!({"created": created, "reminder": reminder.public(), "confirmation": confirmation}))
            }
            Action::Cancel => {
                let id = a.id.as_deref().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| ToolError::new("id is required"))?;
                let writer = env.reminder_writer.as_ref().ok_or_else(|| ToolError::new("reminder writes are not configured"))?;
                let chat = env.owner_chat_id.clone().unwrap_or_default();
                match writer.cancel(id, &chat).await.map_err(|e| ToolError::new(e.to_string()))? {
                    CancelOutcome::Cancelled(r) => {
                        ctx.push_effect(Effect::ReminderCancelled { reminder: r.clone() });
                        Ok(json!({"cancelled": true, "reminder": r.public()}))
                    }
                    CancelOutcome::AlreadyCancelled(r) => Ok(json!({"cancelled": false, "reminder": r.public()})),
                    CancelOutcome::AlreadySent(_) => Err(ToolError::new("already_sent: that reminder was already delivered")),
                    CancelOutcome::NotFound => Err(ToolError::new("reminder_not_found")),
                }
            }
        }
    }
}
