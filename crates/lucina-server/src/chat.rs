//! `POST /api/secretary/chat`: one Lucina text turn.
//!
//! `LUCINA_PLANNER=json` (default) is the Node Lucina's `runChatTurn`, step
//! for step: timeline user item (with `meta: {via: "mcp", caller}` on Lucina
//! MCP turns) → turn context (tier-0 / DeepSeek intent, context policy,
//! gated sources) → outfit → notes → calendar → grocery → the JSON-action
//! planner → MCP guard → reminders / jobs / reply → recent turns, Honcho
//! (`readwrite` only), the Lucina item.
//!
//! `LUCINA_PLANNER=tools` is the native tool loop (Rust tools, confirmation
//! flow); it is not part of the Node parity surface.

use std::time::{Duration, Instant};

use chrono::Utc;
use futures_util::FutureExt;
use lucina_intent::mcp::{self, McpTurn};
use lucina_intent::{domains as dm, grocery as gi, notes as nt, outfit};
use lucina_llm::Message;
use lucina_memory::{secretary_session_id, MemoryMode, TurnSurface};
use lucina_store::jobs::Job;
use lucina_store::parser::{parse_at_time, parse_date_part, parse_reminder_message, ParseOptions};
use lucina_store::reminders::{format_confirmation, format_list, CreateReminder};
use lucina_store::timeline::{Meta, TimelineTurn};
use lucina_store::timeutil::{iso_ms, now_iso, utf16_len};
use lucina_tools::agent::{self, AgentConfig, ToolRecord};
use lucina_tools::pending::{classify_reply, settle, Settle};
use lucina_tools::{Ctx, Effect, Origin};
use serde_json::{json, Value};

use crate::api::{optional_string, owner_reminders, ApiError, PLATFORM_LABEL, USER_LABEL};
use std::sync::Arc;

use crate::planner::TurnInput;
use crate::prompts::Prompts;
use crate::surface::BearerVerified;
use crate::{jobs, AppState, PlannerMode};
use lucina_store::recent::RecentChat;
use lucina_store::timeline::{Attachment, TimelineStore};

pub mod drive_turn;
pub mod media_turn;
pub mod memo_turn;
pub mod meshy_turn;
pub mod reply;
mod share_turn;

/// Counted after the MCP prefix (which may add up to 100 more on the wire).
pub const MAX_MESSAGE_CHARS: usize = 12_000;
const CLIENT_MESSAGE_TTL: Duration = Duration::from_secs(600);
const CLIENT_MESSAGE_KEEP: usize = 200;
pub const PLANNER_PLATFORM: &str = "secretary";
pub const SECRETARY_EXTRA_SYSTEM: &str = "This conversation comes from the Secretary app on Giuseppe's iPhone. Reply in the language of the message. Keep it short: usually 1-3 sentences. Plain text only: no markdown tables or headings.";

/// Validate the body, classify the turn's memory surface (once), dedupe
/// `clientMessageId` retries, run the turn. `bearer`: the request passed the
/// Secretary bearer check.
pub async fn handle_chat(
    st: AppState,
    bearer: BearerVerified,
    body: Value,
) -> Result<Value, ApiError> {
    let message = match &body["message"] {
        Value::String(s) if !s.trim().is_empty() => lucina_store::timeutil::js_trim(s).to_owned(),
        _ => return Err(ApiError::bad_request("message is required")),
    };
    let too_long = || {
        ApiError::bad_request(format!(
            "message must be at most {MAX_MESSAGE_CHARS} characters"
        ))
    };
    if utf16_len(&message) > MAX_MESSAGE_CHARS + mcp::PREFIX_ROOM {
        return Err(too_long());
    }
    let requested = optional_string(&body, "sessionId", 64)?;
    if !requested.is_empty()
        && !requested
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(ApiError::bad_request(
            "sessionId may only contain letters, digits, \"_\" and \"-\"",
        ));
    }
    let client_message_id = optional_string(&body, "clientMessageId", 128)?;
    let channel = optional_string(&body, "channel", 16)?;
    let channel = if channel.is_empty() {
        "web".to_owned()
    } else {
        channel
    };
    if channel != "web" && channel != "ios" {
        return Err(ApiError::bad_request("channel must be web or ios"));
    }
    // Swipe to reply: the earlier timeline item this message answers.
    let reply_to = optional_string(&body, "replyTo", 64)?;
    if !reply_to.is_empty() && !reply::valid_id(&reply_to) {
        return Err(ApiError::bad_request(
            "replyTo may only contain letters, digits, \"_\" and \"-\"",
        ));
    }
    let reply_to = (!reply_to.is_empty()).then_some(reply_to);
    // Self-tests and diagnostics run in their own conversation, out of the owner's chat.
    let selftest = crate::selftest::requested(&body, &requested, &message);
    let session = if selftest {
        crate::selftest::SESSION.to_owned()
    } else {
        secretary_session_id(&requested, &st.cfg.default_session)
    };
    let mcp_turn = mcp::turn_info(&requested, &message);
    let text = mcp_turn
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_else(|| message.clone());
    if utf16_len(&text) > MAX_MESSAGE_CHARS {
        return Err(too_long());
    }
    let surface = crate::surface::chat_surface(&st.cfg, Some(bearer), &channel, mcp_turn.as_ref());
    tracing::info!(session_id = %session, %channel, mcp = mcp_turn.as_ref().map(|m| m.caller.as_str()), surface = surface.class_name(), chars = message.chars().count(), has_client_message_id = !client_message_id.is_empty(), "secretary chat received");
    // Only the owner and his MCP delegates chat here; anything else fails
    // closed before any memory read or timeline write.
    if !surface.is_owner() {
        tracing::warn!(
            surface = surface.class_name(),
            "secretary chat refused by memory surface policy"
        );
        return Err(ApiError::new(403, "forbidden", ""));
    }
    // OTP requests: answered here, never seen by the model, the timeline,
    // memory or the replay cache (Node `handleChat`).
    if let Some(out) = crate::otp::secretary_chat(
        &st.cfg.otp,
        &text,
        mcp_turn.is_some(),
        &session,
        &client_message_id,
    )
    .await
    {
        return Ok(out);
    }
    let cmid = (!client_message_id.is_empty()).then_some(client_message_id);
    let Some(cmid_value) = cmid.clone() else {
        return run_turn(
            st, text, session, None, channel, mcp_turn, surface, reply_to,
        )
        .await;
    };
    let key = format!("{session}:{cmid_value}");
    enum Slot {
        Replay(crate::SharedChat),
        Fresh(crate::SharedChat),
    }
    let slot = {
        let mut map = st.client_messages.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, (at, _)| at.elapsed() <= CLIENT_MESSAGE_TTL);
        match map.get(&key) {
            Some((_, fut)) => Slot::Replay(fut.clone()),
            None => {
                while map.len() >= CLIENT_MESSAGE_KEEP {
                    let oldest = map
                        .iter()
                        .min_by_key(|(_, (at, _))| *at)
                        .map(|(k, _)| k.clone());
                    match oldest {
                        Some(k) => map.remove(&k),
                        None => break,
                    };
                }
                let fut = run_turn(
                    st.clone(),
                    text,
                    session,
                    cmid,
                    channel,
                    mcp_turn,
                    surface,
                    reply_to,
                )
                .boxed()
                .shared();
                map.insert(key.clone(), (Instant::now(), fut.clone()));
                Slot::Fresh(fut)
            }
        }
    };
    let shared = match slot {
        Slot::Replay(fut) => {
            let mut out = fut.await?;
            out["duplicate"] = json!(true);
            return Ok(out);
        }
        Slot::Fresh(fut) => fut,
    };
    let out = shared.await;
    if out.is_err() {
        st.client_messages
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key);
    }
    out
}

/// Sends a later message (a finished job's result) back to the channel.
pub type ReplySink =
    Arc<dyn Fn(String) -> futures_util::future::BoxFuture<'static, ()> + Send + Sync>;

/// What a channel's own planner action produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelActionOutcome {
    /// The skill ran: the reply is Node's `generateSkillReply` over its
    /// result, and Honcho keeps `[<memo>]\n<result>` with it.
    Skill {
        name: String,
        result: String,
        instruction: String,
        memo: String,
    },
    /// A fixed reply (a refusal, a failure).
    Reply(String),
}

/// A channel's own planner actions (Discord: `discord_admin_task`): called
/// with the action and the plan; `None` when the channel does not handle
/// that action. It runs before the turn's guard and does its own
/// authorisation.
pub type ChannelActions = Arc<
    dyn Fn(String, Value) -> futures_util::future::BoxFuture<'static, Option<ChannelActionOutcome>>
        + Send
        + Sync,
>;

/// A turn from a chat channel other than the Secretary app (Telegram): where
/// it comes from and where its effects go. The Secretary path has none and
/// keeps its own values.
#[derive(Clone)]
pub struct ChannelOrigin {
    /// The planner's `Platform:` (Node: `telegram`).
    pub planner_platform: &'static str,
    /// The reply generator's platform (Node: `Telegram`).
    pub platform_label: &'static str,
    pub user_label: String,
    /// Extra system text for replies (Node passes none for Telegram).
    pub extra_system: String,
    /// The Honcho user peer (the owner's chat: the canonical owner peer).
    pub peer_id: String,
    /// The recent-turn buffer and its key (Telegram: the chat id).
    pub recent: Arc<RecentChat>,
    pub recent_key: String,
    /// The owner's timeline, or a disabled store when this turn is not recorded.
    pub timeline: Arc<TimelineStore>,
    /// On every item of the turn (`isDm`, `chatTitle`).
    pub timeline_meta: Option<Meta>,
    /// The owner's item: its text (a voice note's transcript) and attachments.
    pub timeline_text: Option<String>,
    pub user_attachments: Vec<Attachment>,
    /// Reminders created in this turn are delivered here.
    pub reminder_chat_id: String,
    pub reminder_source: &'static str,
    /// Reminder actions (schedule, list, the reminders of a compound
    /// question) are refused with this text (Discord: anyone but the owner).
    pub reminders_refused: Option<String>,
    /// The owner himself: otherwise the turn is held to the MCP allowlist
    /// (no owner tools, no secrets).
    pub owner: bool,
    /// An attachment turn (the share pipeline's focused tool loop).
    pub share: bool,
    /// Where a started job's result is sent when it finishes.
    pub on_job_done: Option<ReplySink>,
    /// Planner actions the channel runs itself.
    pub channel_actions: Option<ChannelActions>,
}

/// A chat channel's text (or attachment) turn through the Secretary turn
/// pipeline (JSON planner; `LUCINA_PLANNER=tools` does not apply here).
pub async fn channel_turn(
    st: AppState,
    message: String,
    session: String,
    channel: String,
    surface: TurnSurface,
    origin: ChannelOrigin,
) -> Result<Value, ApiError> {
    run_turn_inner(
        st,
        message,
        session,
        None,
        channel,
        None,
        surface,
        None,
        Some(Arc::new(origin)),
        None,
    )
    .await
}

/// What the turn answered.
struct Finish {
    reply: String,
    action: String,
    extra: serde_json::Map<String, Value>,
    meta: Option<Meta>,
    action_result: String,
    job: Option<Job>,
    /// The plan of a job to start once the ack item is on the timeline.
    job_plan: Option<Value>,
    /// Buttons the tools asked for (a Gallery page with what Lucina found).
    routes: Vec<Value>,
    /// Calendar events the turn read: the reply gets map cards for those it names.
    calendar_events: Vec<Value>,
    /// A calendar question the calendar answered: the reply is its agenda (crate::agenda).
    agenda: Option<crate::agenda::Agenda>,
}

impl Finish {
    fn reply(reply: impl Into<String>, action: &str) -> Self {
        Self {
            reply: reply.into(),
            action: action.into(),
            extra: serde_json::Map::new(),
            meta: None,
            action_result: String::new(),
            job: None,
            job_plan: None,
            routes: Vec::new(),
            calendar_events: Vec::new(),
            agenda: None,
        }
    }

    fn with(mut self, action_result: impl Into<String>, meta: Option<Meta>) -> Self {
        self.action_result = action_result.into();
        self.meta = meta;
        self
    }

    fn refusal(persona: &str, blocked: &str) -> Self {
        let mut f = Finish::reply(mcp::refusal(persona, blocked), "needs_confirmation");
        f.extra.insert("blockedAction".into(), json!(blocked));
        f
    }
}

/// Node `resolveReminderRequest`: the parser first, then the plan's dueDate.
fn resolve_reminder(message: &str, plan: &Value, o: &ParseOptions) -> Option<(String, String)> {
    let parsed = parse_reminder_message(message, o).ok();
    let mut text = plan["text"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| parsed.as_ref().map(|p| p.text.clone()))
        .unwrap_or_default();
    let mut due = parsed.as_ref().map(|p| p.due_at_iso());
    let due_date = plan["dueDate"].as_str().unwrap_or("").trim();
    if due.is_none() && !due_date.is_empty() {
        let hhmm = due_date.len() <= 5
            && due_date.split_once(':').is_some_and(|(h, m)| {
                !h.is_empty()
                    && h.len() <= 2
                    && m.len() == 2
                    && h.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit())
            });
        if hhmm && !text.is_empty() {
            due = parse_at_time(&format!("alle {due_date} di fare {text}"), o)
                .ok()
                .map(|p| p.due_at_iso());
        }
        if due.is_none() {
            due = parse_date_part(due_date, o)
                .and_then(|r| r.ok())
                .map(|(d, _)| iso_ms(d));
        }
    }
    if text.is_empty() {
        if let Some(p) = &parsed {
            text = p.text.clone();
        }
    }
    match (text.is_empty(), due) {
        (false, Some(d)) => Some((text, d)),
        _ => None,
    }
}

/// Everything a turn needs once it is past validation.
struct Turn<'a> {
    st: &'a AppState,
    message: String,
    session: String,
    channel: String,
    mcp: Option<McpTurn>,
    /// The turn's memory surface (classified once in `handle_chat`).
    surface: TurnSurface,
    /// An MCP turn held to the allowlist (`LUCINA_MCP_TRUSTED=false`), or a
    /// channel turn that is not the owner's.
    guarded: bool,
    user_label: String,
    extra_system: String,
    /// The share pipeline's tool loop.
    share: bool,
    /// `None`: the Secretary app.
    origin: Option<Arc<ChannelOrigin>>,
}

impl Turn<'_> {
    fn persona(&self) -> &str {
        &self.st.cfg.persona
    }

    /// The planner's platform (`secretary`, or the channel's).
    fn planner_platform(&self) -> &'static str {
        self.origin
            .as_ref()
            .map_or(PLANNER_PLATFORM, |o| o.planner_platform)
    }

    /// The reply generator's platform (`Secretary app`, or the channel's).
    fn platform_label(&self) -> &'static str {
        self.origin
            .as_ref()
            .map_or(PLATFORM_LABEL, |o| o.platform_label)
    }

    fn recent(&self) -> &RecentChat {
        self.origin.as_ref().map_or(&self.st.recent, |o| &o.recent)
    }

    /// The channel refuses reminder actions for this sender.
    fn reminders_refused(&self) -> Option<&str> {
        self.origin
            .as_ref()
            .and_then(|o| o.reminders_refused.as_deref())
    }

    fn recent_key(&self) -> &str {
        self.origin
            .as_ref()
            .map_or(self.session.as_str(), |o| o.recent_key.as_str())
    }

    fn peer(&self) -> &str {
        self.origin
            .as_ref()
            .map_or(self.st.cfg.owner_peer_id.as_str(), |o| o.peer_id.as_str())
    }

    fn input<'b>(&'b self, memory: &'b str, platform: &'b str) -> TurnInput<'b> {
        TurnInput {
            user_text: &self.message,
            memory,
            user_label: &self.user_label,
            platform,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_turn(
    st: AppState,
    message: String,
    session: String,
    cmid: Option<String>,
    channel: String,
    mcp_turn: Option<McpTurn>,
    surface: TurnSurface,
    reply_to: Option<String>,
) -> Result<Value, ApiError> {
    run_turn_inner(
        st, message, session, cmid, channel, mcp_turn, surface, None, None, reply_to,
    )
    .await
}

pub(crate) struct ShareTurn {
    pub item: lucina_store::timeline::TimelineItem,
    pub voice_only: bool,
    pub reply_meta: Meta,
    /// What the owner wrote or said (the caption, or a voice note's transcript). The secrets
    /// pre-check reads only this: shared content (OCR of a screenshot, a page) is not the owner asking.
    pub owner_text: String,
}

/// Share ingestion has already written the owner's timeline item (with its
/// `replyTo`, which the turn quotes).
pub(crate) async fn share_turn(
    st: AppState,
    message: String,
    session: String,
    channel: String,
    input: ShareTurn,
) -> Result<Value, ApiError> {
    let surface =
        crate::surface::chat_surface(&st.cfg, Some(BearerVerified::checked()), &channel, None);
    let reply_to = input.item.reply_to.clone();
    run_turn_inner(
        st,
        message,
        session,
        None,
        channel,
        None,
        surface,
        Some(input),
        None,
        reply_to,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_turn_inner(
    st: AppState,
    message: String,
    session: String,
    cmid: Option<String>,
    channel: String,
    mcp_turn: Option<McpTurn>,
    surface: TurnSurface,
    existing: Option<ShareTurn>,
    origin: Option<Arc<ChannelOrigin>>,
    reply_to: Option<String>,
) -> Result<Value, ApiError> {
    let normal_share = existing.as_ref().is_some_and(|s| !s.voice_only);
    let secrets_text = existing.as_ref().map(|s| s.owner_text.clone());
    let voice_only = existing.as_ref().is_some_and(|s| s.voice_only);
    let reply_meta = existing.as_ref().map(|s| s.reply_meta.clone());
    // Swipe to reply: the quoted message rides in every model call's user part.
    let reply_block = match reply_to.as_deref() {
        Some(id) => reply::block(&st, &surface, id).await,
        None => String::new(),
    };
    // An id that names nothing readable is ignored, on the timeline too.
    let reply_to = reply_to.filter(|_| !reply_block.is_empty());
    if st.cfg.planner == PlannerMode::Tools && !normal_share && origin.is_none() {
        let from_mcp = mcp_turn.is_some();
        return crate::planner::TURN_REPLY
            .scope(
                reply_block,
                tools_turn(
                    st, message, session, cmid, channel, surface, from_mcp, existing, reply_to,
                ),
            )
            .await;
    }
    // With rich replies the app turns structure into widgets and buttons.
    let base_system = if normal_share {
        crate::share::SHARE_SYSTEM
    } else if st.cfg.rich_replies {
        crate::rich::SECRETARY_EXTRA_SYSTEM_RICH
    } else {
        SECRETARY_EXTRA_SYSTEM
    };
    let base_system = if voice_only {
        format!("{base_system} {}", crate::share::guidance::VOICE_SYSTEM)
    } else {
        base_system.to_owned()
    };
    let note = |m: &McpTurn| {
        if st.cfg.mcp_trusted {
            mcp::note_trusted(m)
        } else {
            mcp::note(m)
        }
    };
    let (user_label, extra_system) = match (&mcp_turn, &origin) {
        (_, Some(o)) => (o.user_label.clone(), o.extra_system.clone()),
        (Some(m), None) => (
            format!("{USER_LABEL}; {}", note(m)),
            format!("{base_system} {}", note(m)),
        ),
        (None, None) => (USER_LABEL.to_owned(), base_system.to_owned()),
    };
    let share = normal_share || origin.as_ref().is_some_and(|o| o.share);
    let turn_meta = mcp_turn.as_ref().map(|m| Meta {
        via: Some("mcp".into()),
        caller: Some(m.caller.clone()),
        ..Meta::default()
    });
    let (timeline_store, turn_meta) = match &origin {
        Some(o) => (o.timeline.clone(), o.timeline_meta.clone()),
        None => (st.timeline_for(&session), turn_meta),
    };
    let timeline = TimelineTurn::new(timeline_store, &channel, cmid.clone())
        .with_meta(turn_meta)
        .with_reply_to(reply_to);
    let timeline = if let Some(item) = existing {
        timeline.with_user_item(item.item)
    } else {
        timeline
    };
    let user_item = match &origin {
        Some(o) => timeline.record_user(
            o.timeline_text.as_deref().unwrap_or(&message),
            o.user_attachments.clone(),
        ),
        None => timeline.record_user(&message, Vec::new()),
    };
    let guarded =
        (mcp_turn.is_some() && !st.cfg.mcp_trusted) || origin.as_ref().is_some_and(|o| !o.owner);
    let t = Turn {
        st: &st,
        message,
        session,
        channel,
        mcp: mcp_turn,
        surface,
        guarded,
        user_label,
        extra_system,
        share,
        origin,
    };
    // Secrets first, without the model: a fixed refusal for reveal requests
    // (MCP included), status and audit for the owner.
    let not_owner = t.mcp.is_some() || t.origin.as_ref().is_some_and(|o| !o.owner);
    let (finish, prepared) = match secrets_finish(
        &st,
        secrets_text.as_deref().unwrap_or(&t.message),
        not_owner,
    )
    .await
    {
        Some(f) => {
            t.recent()
                .append(t.recent_key(), "user", &t.message, Utc::now());
            (f, None)
        }
        None => {
            let prepared = crate::context::prepare_keyed(
                &st,
                &t.surface,
                &t.session,
                crate::context::Keys {
                    recent: t.recent(),
                    recent_key: t.recent_key(),
                    peer: t.peer(),
                },
                &t.message,
                &t.user_label,
                t.planner_platform(),
                false,
            )
            .await;
            t.recent()
                .append(t.recent_key(), "user", &t.message, Utc::now());
            let game_system = if prepared.game.is_empty() || !t.guarded {
                prepared.game.clone()
            } else {
                format!("{}\n{}", prepared.game, crate::bridges::GAME_MCP_NOTE)
            };
            let game_system = if voice_only {
                format!("{game_system}\n{}", crate::share::guidance::VOICE_SYSTEM)
            } else {
                game_system
            };
            let finish = crate::planner::TURN_REPLY
                .scope(
                    reply_block,
                    crate::planner::TURN_SYSTEM
                        .scope(game_system, json_turn(&t, &prepared, cmid.as_deref())),
                )
                .await;
            (finish, Some(prepared))
        }
    };

    // Finish: recent turns, Honcho, the Lucina item.
    let (rich_msg, reply, remembered) = if t.origin.is_some() {
        // Channels send plain text (no app widgets).
        let reply = finish.reply.trim().to_owned();
        (None, reply.clone(), reply)
    } else {
        rich_reply(
            &st,
            finish.reply.trim(),
            &t.message,
            &finish.action,
            &finish.action_result,
            // Secrets replies are fixed texts: no model rewrite.
            !finish.action.starts_with("secrets_"),
            &finish.calendar_events,
            finish.agenda.as_ref(),
        )
        .await
    };
    let rich_msg = with_routes(rich_msg, &finish.routes);
    t.recent()
        .append(t.recent_key(), "assistant", &remembered, Utc::now());
    remember_turn(&st, &t, &finish.action, &remembered, &finish.action_result);
    let meta = match &finish.job {
        Some(job) => Some(Meta {
            job_id: Some(job.id.clone()),
            ..Meta::default()
        }),
        None => finish.meta.clone(),
    };
    let meta = with_rich_meta(meta, rich_msg.as_ref());
    let meta = reply_meta
        .map(|m| meta.clone().unwrap_or_default().merge(m))
        .or(meta);
    let lucina_item = timeline.record_lucina(&reply, Vec::new(), meta);
    // Node queues the job's result after the ack on the turn's timeline chain.
    if let (Some(job), Some(plan)) = (&finish.job, &finish.job_plan) {
        spawn_job(
            st.clone(),
            job.clone(),
            plan.clone(),
            t.message.clone(),
            prepared
                .as_ref()
                .map(|p| p.memory_context.clone())
                .unwrap_or_default(),
            JobDest {
                session: t.session.clone(),
                user_item: timeline.user_item_id(),
                channel: t.channel.clone(),
                surface: t.surface.clone(),
                origin: t.origin.clone(),
            },
        );
    }
    let mut out = json!({
        "reply": reply,
        "action": finish.action,
        "job": finish.job.as_ref().map(|j| json!({"id": j.id, "status": j.status})),
        "sessionId": t.session,
        "clientMessageId": cmid,
        "userItemId": user_item.map(|i| i.id),
        "lucinaItemId": lucina_item.map(|i| i.id),
    });
    add_rich(&mut out, rich_msg.as_ref());
    if crate::selftest::is_session(&session_of(&out)) {
        out["conversation"] = json!(crate::selftest::CONVERSATION);
    }
    for (k, v) in finish.extra {
        out[k] = v;
    }
    Ok(out)
}

/// A calendar bridge answer as the turn's finish.
fn calendar_finish(o: crate::bridges::Outcome) -> Finish {
    let mut f = Finish::reply(o.reply, &o.action).with(o.action_result, o.meta);
    f.calendar_events = o.events;
    f.agenda = o.agenda;
    f
}

/// `LUCINA_RICH_REPLIES=true`: the reply as Lucina message format v1, plus the
/// plain fallback text (timeline, `reply`, MCP callers, old clients) and the
/// text to remember (without the button hints). Calendar events the reply
/// names get place blocks, and places get coordinates when the map server's
/// geocoder is configured. Off: the reply unchanged.
#[allow(clippy::too_many_arguments)]
async fn rich_reply(
    st: &AppState,
    reply: &str,
    user_text: &str,
    action: &str,
    action_result: &str,
    use_model: bool,
    calendar_events: &[Value],
    agenda: Option<&crate::agenda::Agenda>,
) -> (Option<lucina_store::rich::RichMessage>, String, String) {
    if !st.cfg.rich_replies {
        return (None, reply.to_owned(), reply.to_owned());
    }
    // An agenda: one sentence and the event cards, no model rewrite, no wall of text.
    if let Some(a) = agenda.filter(|_| !calendar_events.is_empty()) {
        let tz = st.cfg.time_zone;
        let mut m = crate::agenda::message(a, calendar_events, tz);
        if let Some(g) = &st.geocoder {
            g.fill(&mut m.blocks).await;
            g.add_travel(&mut m.blocks).await;
        }
        m.blocks = m
            .blocks
            .iter()
            .filter_map(lucina_store::rich::normalize_block)
            .collect();
        crate::agenda::with_navigation(&mut m);
        let text = crate::agenda::plain_text(a, calendar_events, &m.blocks, tz);
        return (Some(m), text.clone(), text);
    }
    let draft = crate::rich::Draft {
        text: reply,
        user_text,
        action,
        action_result,
        use_model,
    };
    match crate::rich::compose(&st.model, draft, st.cfg.rich_compose_timeout).await {
        Some(mut m) => {
            let final_text = m.plain_text(false);
            crate::places::add_calendar_places(&mut m, calendar_events, &final_text);
            if let Some(g) = &st.geocoder {
                g.fill(&mut m.blocks).await;
            }
            let (text, remembered) = (m.plain_text(true), m.plain_text(false));
            (Some(m), text, remembered)
        }
        None => (None, reply.to_owned(), reply.to_owned()),
    }
}

fn with_rich_meta(
    meta: Option<Meta>,
    rich: Option<&lucina_store::rich::RichMessage>,
) -> Option<Meta> {
    match rich {
        Some(m) => Some(meta.unwrap_or_default().merge(Meta::rich(m))),
        None => meta,
    }
}

fn session_of(out: &Value) -> String {
    out["sessionId"].as_str().unwrap_or("").to_owned()
}

/// The v1 fields of the chat response (`reply` is the plain fallback).
fn add_rich(out: &mut Value, rich: Option<&lucina_store::rich::RichMessage>) {
    if let Some(m) = rich {
        out["format"] = json!(m.format);
        out["lang"] = json!(m.lang);
        out["bubbles"] = json!(m.bubbles);
        out["blocks"] = json!(m.blocks);
        out["actions"] = json!(m.actions);
    }
}

/// The secrets turn as a [`Finish`] (see [`crate::secrets::secrets_turn`]).
async fn secrets_finish(st: &AppState, text: &str, mcp: bool) -> Option<Finish> {
    let (reply, action) = crate::secrets::secrets_turn(st.secrets.as_deref(), text, mcp).await?;
    let mut f = Finish::reply(reply, action);
    if action == "secrets_refused" {
        f.extra.insert("blockedAction".into(), json!("secrets"));
    }
    f.action_result = format!("[{action}]");
    Some(f)
}

/// Honcho `remember` (`readwrite` only), Node's `rememberSecretaryTurn`.
fn remember_turn(st: &AppState, t: &Turn<'_>, action: &str, reply: &str, action_result: &str) {
    if st.memory.mode() != MemoryMode::ReadWrite || crate::selftest::is_session(&t.session) {
        return;
    }
    if t.origin.is_some() {
        // Node's Telegram turns: the user's text and the reply as sent
        // (grocery replies carry their intent).
        let assistant = if action == "grocery" && !action_result.is_empty() {
            format!("[{action_result}] {reply}")
        } else if action == "discord_admin_task" && !action_result.is_empty() {
            // Node: `[discord admin]\n<result>\n\n[reply]\n<reply>`.
            format!("{action_result}\n\n[reply]\n{reply}")
        } else {
            reply.to_owned()
        };
        let (mem, peer, sid, surface, user) = (
            st.memory.clone(),
            t.peer().to_owned(),
            t.session.clone(),
            t.surface.clone(),
            t.message.clone(),
        );
        tokio::spawn(async move {
            if let Err(e) = mem.remember(&surface, &peer, &sid, &user, &assistant).await {
                tracing::warn!(error = %e, "channel honcho remember failed");
            }
        });
        return;
    }
    let label = match &t.mcp {
        Some(m) => format!(
            "[{PLATFORM_LABEL}, via Lucina MCP from {}] {}",
            m.caller, t.message
        ),
        None => format!("[{PLATFORM_LABEL}] {}", t.message),
    };
    let assistant = [
        format!(
            "[action:{}]",
            if action.is_empty() { "reply" } else { action }
        ),
        reply.to_owned(),
        if action_result.is_empty() {
            String::new()
        } else {
            format!(
                "[result]\n{}",
                action_result.chars().take(3000).collect::<String>()
            )
        },
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    let (mem, peer, sid, surface) = (
        st.memory.clone(),
        st.cfg.owner_peer_id.clone(),
        t.session.clone(),
        t.surface.clone(),
    );
    tokio::spawn(async move {
        if let Err(e) = mem
            .remember(&surface, &peer, &sid, &label, &assistant)
            .await
        {
            tracing::warn!(error = %e, "secretary honcho remember failed");
        }
    });
}

fn compound_label(domain: &str) -> &'static str {
    match domain {
        dm::SHOPPING => "the shopping list",
        dm::NOTES => "a saved note",
        dm::CALENDAR => "the calendar",
        _ => "the reminders",
    }
}

/// The domains of a compound question this host can read (features on).
fn readable_domains(st: &AppState, session: &str, message: &str) -> Vec<&'static str> {
    let pending = st.cfg.notes_enabled && !st.bridge_state.pending_part(session).is_empty();
    dm::read_domains(message, Utc::now(), st.cfg.time_zone, pending)
        .into_iter()
        .filter(|d| match *d {
            dm::SHOPPING => st.grocery.is_some(),
            dm::NOTES => st.cfg.notes_enabled,
            dm::CALENDAR => st.cfg.calendar_enabled,
            _ => st.cfg.reminders_enabled,
        })
        .collect()
}

/// One context block per domain of a compound question; blocks already in
/// the turn context (calendar, named notes, shopping list) are not repeated.
/// A source that fails says so, so the reply does not guess.
async fn compound_blocks(t: &Turn<'_>, domains: &[&'static str], memory: &str) -> Vec<String> {
    let st = t.st;
    let tz = st.cfg.time_zone;
    let mut blocks: Vec<String> = Vec::new();
    let mut add = |block: String| {
        let b = block.trim().to_owned();
        if !b.is_empty() && !memory.contains(&b) {
            blocks.push(b);
        }
    };
    for domain in domains {
        match *domain {
            dm::SHOPPING => {
                if memory.contains("Shopping list (Secretary app), to buy:") {
                    continue;
                }
                let Some(repo) = st.grocery.as_deref() else {
                    continue;
                };
                match repo.legacy_list(&st.cfg.grocery_user_id).await {
                    Ok(items) => add(dm::shopping_list_block(&items)),
                    Err(e) => {
                        tracing::warn!(domain, error = %e, "secretary compound context failed");
                        add(format!(
                            "{}: unavailable right now; do not guess.",
                            compound_label(domain)
                        ));
                    }
                }
            }
            dm::NOTES => {
                match nt::parse_notes_intent(&t.message, false) {
                    Some(nt::NotesIntent::Read { name, .. }) => {
                        let r = crate::bridges::find_note(&st.secretary, &name).await;
                        if r.ok {
                            add(format!(
                                "Owner notes named in this message (Secretary app, Notes):\n{}",
                                nt::render_note_context(&r.data, nt::MAX_CONTEXT_CHARS)
                            ));
                        } else if r.status == 404 {
                            add(format!("Note \"{name}\": no note with that name; do not guess its content."));
                        } else {
                            add(format!(
                            "Note \"{name}\": unreadable right now ({}); do not guess its content.",
                            r.code
                        ));
                        }
                    }
                    _ => {
                        let r = st
                            .secretary
                            .call(reqwest::Method::GET, "/v1/notes?limit=30", None)
                            .await;
                        if r.ok {
                            let notes = r.data["notes"].as_array().cloned().unwrap_or_default();
                            let list = notes
                                .iter()
                                .map(|n| {
                                    format!(
                                        "{} ({})",
                                        n["title"].as_str().unwrap_or(""),
                                        n["slug"].as_str().unwrap_or("")
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("; ");
                            add(format!(
                                "Owner notes (Secretary app): {}.",
                                if list.is_empty() { "none" } else { &list }
                            ));
                        } else {
                            add("Notes: unreadable right now.".into());
                        }
                    }
                }
            }
            dm::CALENDAR => {
                add(
                    crate::bridges::calendar_context(&st.secretary, &t.message, Utc::now(), tz)
                        .await,
                )
            }
            _ => {
                if let Some(refused) = t.reminders_refused() {
                    add(format!("Reminders: not available here ({refused})."));
                    continue;
                }
                let pending: Vec<_> = owner_reminders(st)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|r| r.status == "pending")
                    .collect();
                let scope = lucina_intent::heuristics::reminder_list_scope(&t.message);
                add(format!(
                    "Pending reminders:\n{}",
                    format_list(&pending, tz, t.persona(), scope == "today", Utc::now())
                ));
            }
        }
    }
    blocks
}

async fn json_turn(
    t: &Turn<'_>,
    prepared: &crate::context::Prepared,
    cmid: Option<&str>,
) -> Finish {
    let st = t.st;
    let persona = t.persona().to_owned();
    let memory = prepared.memory_context.as_str();

    if t.share {
        if t.guarded {
            // Not the owner: no tools, an answer from the extracted content only.
            let reply = st
                .planner
                .generate_reply(
                    &t.input(memory, t.platform_label()),
                    &t.extra_system,
                    "",
                    "reply",
                )
                .await
                .unwrap_or_default();
            return Finish::reply(reply, "reply");
        }
        return share_turn::run(t, memory).await;
    }

    // Meshy 3D (LUCINA_MESHY=true): a pending plan's yes/no, or a 3D request.
    if let Some(f) = meshy_turn::try_turn(t, memory).await {
        return f;
    }

    // Lucina's own Google Drive (LUCINA_DRIVE=true with a grant), read-only.
    if let Some(f) = drive_turn::try_turn(t, memory).await {
        return f;
    }

    // "Mettiti il vestito da cameriera" (LUCINA_AVATAR_OUTFIT=true; never from MCP).
    if !t.guarded && st.cfg.outfit_enabled {
        if let Some(look) = outfit::parse_outfit_intent(&t.message) {
            let o = crate::bridges::change_look(&st.secretary, &look).await;
            return Finish::reply(o.reply, "avatar_outfit").with(o.action_result, o.meta);
        }
    }

    // Delegation (LUCINA_DELEGATE=true): the owner's switches, undo, and
    // explicit asks ("chiedi a Grok …"). From Lucina MCP only when trusted.
    if let Some(d) = st.delegator.clone() {
        if let Some(f) = delegation_hook(t, &d, memory).await {
            return f;
        }
    }

    // A question about two or more of the owner's data ("cosa c'è sulla lista
    // della spesa e cosa ho nella nota X?"): no single bridge may answer it.
    // Gather each part's data and let the model answer every part.
    let domains = readable_domains(st, &t.session, &t.message);
    if domains.len() >= 2 {
        let blocks = compound_blocks(t, &domains, memory).await;
        let context = blocks
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(memory))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        let labels: Vec<&str> = domains.iter().map(|d| compound_label(d)).collect();
        let extra = [
            t.extra_system.clone(),
            format!("The owner asked about several things in one message: {}. The real data for each is in the context above.", labels.join(", ")),
            "Answer every part, in the order asked, each briefly (keep to any length the owner asks for, such as one line). If a part has no data, say so in a few words.".into(),
            "Use only that data: never invent items, events, reminders or note content.".into(),
        ]
        .join(" ");
        let reply = st
            .planner
            .generate_reply(&t.input(&context, t.platform_label()), &extra, "", "reply")
            .await
            .unwrap_or_default();
        let reply = match reply.trim() {
            "" => {
                let raw: String = blocks.join("\n\n").chars().take(1500).collect();
                if raw.is_empty() {
                    "I could not read that right now.".to_owned()
                } else {
                    raw
                }
            }
            r => r.to_owned(),
        };
        return Finish::reply(reply, "reply")
            .with(format!("[compound: {}]", domains.join("+")), None);
    }

    // Notes (LUCINA_NOTES=true): save, append, read, list; MCP callers may too.
    if st.cfg.notes_enabled {
        let pending = !st.bridge_state.pending_part(&t.session).is_empty();
        if let Some(intent) = nt::parse_notes_intent(&t.message, pending) {
            if t.guarded && !mcp::ALLOWED_ACTIONS.contains(&"notes") {
                return Finish::refusal(&persona, "notes");
            }
            let source = t
                .mcp
                .as_ref()
                .map(|m| mcp::note_source(&m.caller))
                .unwrap_or_else(|| "owner".into());
            let o = crate::bridges::notes_turn(
                &st.secretary,
                &st.bridge_state,
                &intent,
                &t.session,
                &source,
            )
            .await;
            if let Some(note) = &o.memory_note {
                remember_note(st, &t.surface, &t.session, note, &source);
            }
            let mut reply = o.reply.clone();
            if let Some(note) = &o.read_note {
                let context = [nt::render_note_context(note, 12_000), memory.to_owned()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let extra = [
                    t.extra_system.clone(),
                    format!("The owner asked about the saved note \"{}\" ({}); its content is in the context above.", note["title"].as_str().unwrap_or(""), note["slug"].as_str().unwrap_or("")),
                    "Answer from it: a short summary with the key points unless a detail is asked for (a few bullet points are fine).".into(),
                    "Say that the whole note is in the Secretary app under Notes. Never invent content that is not in the note.".into(),
                ]
                .join(" ");
                reply = st
                    .planner
                    .generate_reply(&t.input(&context, t.platform_label()), &extra, "", "reply")
                    .await
                    .unwrap_or_default();
                if reply.trim().is_empty() {
                    reply = format!(
                        "{}\n\n{}",
                        note["title"].as_str().unwrap_or(""),
                        note["content"]
                            .as_str()
                            .unwrap_or("")
                            .chars()
                            .take(1500)
                            .collect::<String>()
                    );
                }
            }
            return Finish::reply(reply, "notes").with(o.action_result, o.meta);
        }
    }

    // Tasks (LUCINA_TASKS=true): what to do today, add, done, reschedule
    // overdue. MCP turns may read, add and complete; deleting is refused.
    // Before the calendar: "cosa devo fare oggi?" answers with the tasks and
    // today's events.
    if st.cfg.tasks_enabled {
        match crate::bridges::tasks_turn(
            &st.secretary,
            &t.message,
            t.guarded,
            st.cfg.time_zone,
            st.cfg.calendar_enabled,
        )
        .await
        {
            Some(Err(blocked)) => {
                tracing::warn!(session_id = %t.session, %blocked, "secretary mcp turn refused");
                return Finish::refusal(&persona, blocked);
            }
            Some(Ok(o)) => return Finish::reply(o.reply, &o.action).with(o.action_result, o.meta),
            None => {}
        }
    }

    // Calendar questions and confirmed event creation (LUCINA_CALENDAR=true).
    if st.cfg.calendar_enabled {
        match crate::bridges::calendar_turn(
            &st.secretary,
            &st.bridge_state,
            &t.message,
            &t.session,
            t.guarded,
            Utc::now(),
            st.cfg.time_zone,
        )
        .await
        {
            Some(Err(blocked)) => {
                tracing::warn!(session_id = %t.session, %blocked, "secretary mcp turn refused");
                return Finish::refusal(&persona, blocked);
            }
            Some(Ok(o)) => return calendar_finish(o),
            None => {}
        }
    }

    // Voice memos: "cosa ho detto nel memo di ieri su X" (a short tool loop, before the library's).
    if let Some(f) = memo_turn::try_turn(t, memory).await {
        return f;
    }

    // The photo library: "fammi vedere le foto di Copenaghen" (a short tool loop, like Meshy).
    if let Some(f) = media_turn::try_turn(t, memory).await {
        return f;
    }

    // The deterministic grocery path (same as a Telegram text turn).
    let grocery_intent = gi::parse_grocery_intent(&t.message).map(|g| g.intent);
    if t.guarded {
        if let Some(intent) = grocery_intent {
            if !mcp::ALLOWED_GROCERY_INTENTS.contains(&intent) {
                tracing::warn!(session_id = %t.session, blocked = intent, "secretary mcp turn refused");
                return Finish::refusal(&persona, intent);
            }
        }
    }
    // Deal questions: the deals engine answers (crate::deals); the grocery
    // path below takes what it cannot (as Node's grocery process()).
    if let (Some(engine), Some(_)) = (
        st.deals.as_deref(),
        grocery_intent.filter(|i| crate::deals::DEALS_INTENTS.contains(i)),
    ) {
        if let Some((reply, intent)) =
            crate::deals::answer(engine, &st.cfg.grocery_user_id, &t.message).await
        {
            return Finish::reply(reply, "grocery").with(intent, None);
        }
    }
    match crate::grocery_turn::process(
        st.grocery.as_deref(),
        &st.cfg.grocery_user_id,
        &t.message,
        &prepared.recent_context,
        &t.channel,
    )
    .await
    {
        Ok(Some(h)) => {
            let meta = h.list_id.map(|id| Meta {
                list_id: Some(id),
                ..Meta::kind("shopping")
            });
            return Finish::reply(h.reply, "grocery").with(h.intent, meta);
        }
        Ok(None) => {}
        Err(e) => {
            tracing::error!(error = %e, "secretary grocery turn failed");
            return Finish::reply(
                "I could not update grocery accounting right now. Your purchase was not recorded.",
                "grocery",
            );
        }
    }

    // The JSON-action planner, with the turn's classification. While her game is live, owner turns skip the
    // casual chat shortcut (which clips the context and cannot choose actions) so she reasons over the game
    // state and can pass advice to her playing self (game_hint).
    let game_live = !t.guarded && !prepared.game.is_empty();
    let routed = if game_live {
        st.planner
            .plan_response(&t.input(memory, t.planner_platform()))
            .await
    } else {
        st.planner
            .route_classified(
                &t.input(memory, t.planner_platform()),
                &prepared.classification,
                &st.cfg.intent,
                st.cfg.capability_hints,
            )
            .await
    };
    let plan = match routed {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(surface = "secretary", error = %e, "response planning failed");
            match st
                .planner
                .generate_reply(&t.input(memory, t.platform_label()), "", "", "reply")
                .await
            {
                Ok(text) => json!({"action": "reply", "text": text}),
                Err(_) => {
                    json!({"action": "reply", "text": format!("{persona}: I misread that turn and could not recover it. Please send the message once more.")})
                }
            }
        }
    };
    let action = plan["action"]
        .as_str()
        .filter(|a| !a.is_empty())
        .unwrap_or("reply")
        .to_owned();

    // The channel's own actions (Discord admin): before the guard, which
    // they replace with their own authorisation.
    if let Some(run) = t.origin.as_ref().and_then(|o| o.channel_actions.clone()) {
        if let Some(outcome) = run(action.clone(), plan.clone()).await {
            return match outcome {
                ChannelActionOutcome::Reply(text) => Finish::reply(text, &action),
                ChannelActionOutcome::Skill {
                    name,
                    result,
                    instruction,
                    memo,
                } => {
                    let text = st
                        .planner
                        .generate_skill_reply(
                            &t.input(memory, t.platform_label()),
                            &name,
                            &result,
                            &instruction,
                        )
                        .await
                        .unwrap_or_default();
                    let text = if text.trim().is_empty() {
                        "I could not generate a reply.".to_owned()
                    } else {
                        text
                    };
                    Finish::reply(text, &action).with(format!("[{memo}]\n{result}"), None)
                }
            };
        }
    }

    if t.guarded && !mcp::ALLOWED_ACTIONS.contains(&action.as_str()) {
        tracing::warn!(session_id = %t.session, blocked = %action, "secretary mcp turn refused");
        return Finish::refusal(&persona, &action);
    }

    // Her Pokémon Red run (planner-chosen, never keyword shortcuts; MCP callers are refused above because these
    // actions are not in mcp::ALLOWED_ACTIONS). Advice is text for her playing self only.
    if action == "game_hint" {
        let text = plan["text"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&t.message)
            .to_owned();
        let ack = plan["ack"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        return match crate::bridges::game_hint(&st.secretary.http, &text).await {
            Ok(_) => Finish::reply(
                ack.unwrap_or("Passed it to my playing self: I'll act on it in the game.")
                    .to_owned(),
                "game",
            )
            .with(
                format!("[game hint queued] {text}"),
                Some(Meta::kind("game")),
            ),
            Err(e) => {
                tracing::warn!(error = %e, "game hint failed");
                Finish::reply(
                    "I couldn't reach my game right now, so the advice didn't get through.",
                    "game",
                )
                .with("[game hint failed]", Some(Meta::kind("game")))
            }
        };
    }
    if action == "game_start" || action == "game_pause" {
        let running = action == "game_start";
        let ok = crate::bridges::game_control(&st.secretary.http, running)
            .await
            .is_ok();
        let lang = lucina_intent::tasks::guess_lang(&t.message);
        let text = if ok {
            lucina_intent::game::control_reply(lang, running, true).to_owned()
        } else {
            lucina_intent::game::off(lang).to_owned()
        };
        return Finish::reply(text, "game").with(
            format!(
                "[game {} {}]",
                if running { "start" } else { "pause" },
                if ok { "ok" } else { "failed" }
            ),
            Some(Meta::kind("game")),
        );
    }

    // Lucina decided herself that another agent should do it.
    if action == "delegate_task" {
        if let Some(d) = st.delegator.clone() {
            let task = plan["task"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(&t.message)
                .to_owned();
            let to = plan["to"]
                .as_str()
                .and_then(lucina_delegate::AgentId::parse);
            let kind = plan["kind"]
                .as_str()
                .and_then(lucina_delegate::Kind::parse)
                .unwrap_or_else(|| lucina_delegate::intent::infer_kind(&task, to));
            let req = owner_request(t, to, kind, task, None);
            return delegated_finish(t, &d, req, memory).await;
        }
    }

    let tz = st.cfg.time_zone;
    if action == "list_reminders" || action == "schedule_reminder" {
        if let Some(refused) = t.reminders_refused() {
            return Finish::reply(refused, &action);
        }
    }
    if action == "list_reminders" && st.cfg.reminders_enabled {
        let scope = plan["scope"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| lucina_intent::heuristics::reminder_list_scope(&t.message));
        let pending: Vec<_> = owner_reminders(st)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.status == "pending")
            .collect();
        return Finish::reply(
            format_list(&pending, tz, &persona, scope == "today", Utc::now()),
            "list_reminders",
        );
    }

    if action == "schedule_reminder" {
        if !st.cfg.reminders_enabled {
            return Finish::reply(
                "Timed reminders are disabled on this host.".to_owned(),
                &action,
            );
        }
        let chat = match &t.origin {
            Some(o) => Some(o.reminder_chat_id.clone()),
            None => st.cfg.owner_chat_id.clone(),
        };
        let Some(chat) = chat.filter(|c| !c.is_empty()) else {
            return Finish::reply(
                "No owner Telegram chat is configured, so I cannot schedule reminders.".to_owned(),
                &action,
            );
        };
        let o = ParseOptions {
            tz,
            default_hour: st.cfg.reminder_default_hour,
            default_minute: st.cfg.reminder_default_minute,
            now: Utc::now(),
        };
        let Some((text, due)) = resolve_reminder(&t.message, &plan, &o) else {
            return Finish::reply("I could not work out when to remind you. Try \"domani mattina di comprare il latte\", \"alle 21:30 di fare la doccia\" or \"28 August 2026 — unsubscribe from Google AI Plus\".".to_owned(), &action);
        };
        return match st
            .writer
            .create(CreateReminder {
                text,
                due_at: due,
                chat_id: chat,
                user_peer_id: t.peer().to_owned(),
                user_label: t.user_label.clone(),
                persona: persona.clone(),
                time_zone: tz.name().into(),
                dedupe_key: None,
                source: t
                    .origin
                    .as_ref()
                    .map_or("secretary", |o| o.reminder_source)
                    .into(),
            })
            .await
        {
            Ok((created, reminder)) => {
                let prefix = if created {
                    ""
                } else {
                    "That reminder was already scheduled. "
                };
                let mut f = Finish::reply(
                    format!("{prefix}{}", format_confirmation(&reminder, tz)),
                    &action,
                );
                f.extra.insert("reminder".into(), reminder.public());
                f.action_result = format!(
                    "[reminder {}]",
                    if created { "created" } else { "existing" }
                );
                f
            }
            Err(e) => Finish::reply(
                format!("I could not schedule that reminder ({}).", e.code),
                &action,
            ),
        };
    }

    if jobs::ASYNC_ACTIONS.contains(&action.as_str()) {
        let blocker = if action == "generate_image" && st.cfg.image_jobs {
            None
        } else {
            jobs::blocker(&plan, &st.cfg.dsh)
        };
        if let Some(b) = blocker {
            return Finish::reply(b, &action);
        }
        let now = now_iso();
        let job = Job {
            id: uuid::Uuid::new_v4().to_string(),
            action: action.clone(),
            status: "queued".into(),
            result: None,
            error: None,
            image_url: None,
            session_id: t.session.clone(),
            client_message_id: cmid.map(str::to_owned),
            created_at: now.clone(),
            started_at: None,
            finished_at: None,
            updated_at: now,
            timeline_item_id: None,
            owner: None,
            boot_id: None,
            extra: Default::default(),
        };
        match st.jobs.insert(job) {
            Ok(job) => {
                let mut f = Finish::reply(
                    format!(
                        "On it: I started the {}. The result will appear here when it finishes.",
                        jobs::label(&action)
                    ),
                    &action,
                );
                f.job = Some(job);
                f.job_plan = Some(plan.clone());
                return f;
            }
            Err(e) => {
                tracing::error!(error = %e, "secretary job insert failed");
                return Finish::reply(
                    format!("I could not start the {}.", jobs::label(&action)),
                    &action,
                );
            }
        }
    }

    if action == "reply" {
        let text = match plan["text"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(text) => text.to_owned(),
            None => st
                .planner
                .generate_reply(
                    &t.input(memory, t.platform_label()),
                    &t.extra_system,
                    "",
                    "reply",
                )
                .await
                .unwrap_or_default(),
        };
        let text = if text.trim().is_empty() {
            "I could not generate a reply.".to_owned()
        } else {
            text
        };
        return Finish::reply(text, "reply");
    }

    // A tool the app cannot run (Discord admin, web crawl, log inspection, …).
    let extra = if t.origin.is_some() {
        format!(
            "{} This request needs the \"{action}\" tool, which this Lucina cannot run yet. Say so in one sentence, answer whatever you can without it, and never claim the tool ran.",
            t.extra_system
        )
        .trim()
        .to_owned()
    } else {
        format!(
            "{} This request needs the \"{action}\" tool, which the Secretary app cannot run. Say so in one sentence, answer whatever you can without it, and suggest asking on Telegram for the tool. Never claim the tool ran.",
            t.extra_system
        )
    };
    let text = st
        .planner
        .generate_reply(&t.input(memory, t.platform_label()), &extra, "", "reply")
        .await
        .unwrap_or_default();
    let text = if text.trim().is_empty() {
        if t.origin.is_some() {
            "I cannot do that here yet.".to_owned()
        } else {
            "I cannot do that from the app yet. Ask me on Telegram.".to_owned()
        }
    } else {
        text
    };
    Finish::reply(text, "reply")
}

/// A delegation request from the owner's own turn.
fn owner_request(
    t: &Turn<'_>,
    to: Option<lucina_delegate::AgentId>,
    kind: lucina_delegate::Kind,
    task: String,
    workspace: Option<String>,
) -> lucina_delegate::Request {
    lucina_delegate::Request {
        to,
        kind,
        task,
        workspace,
        origin: lucina_delegate::Origin::Owner,
        depth: 0,
        caller: None,
        session_id: t.session.clone(),
        channel: t.channel.clone(),
        lang: None,
        resume_job: None,
    }
}

/// The owner's switches ("non usare Grok", "stop delegating") and undo
/// ("annulla delega 1a2b3c4d"); `mcp`: refused.
async fn delegation_settings(
    d: &std::sync::Arc<lucina_delegate::Delegator>,
    message: &str,
    mcp: bool,
) -> Option<Finish> {
    use lucina_delegate::intent;
    let (reply, action) = if let Some(s) = intent::parse_switch(message) {
        if mcp {
            return Some(delegation_refused(s.lang));
        }
        (d.switch(&s), "delegation_settings")
    } else if let Some((id, lang)) = intent::parse_undo(message) {
        if mcp {
            return Some(delegation_refused(lang));
        }
        (d.undo(&id, lang).await, "delegation_undo")
    } else {
        return None;
    };
    Some(Finish::reply(reply, action).with(format!("[{action}]"), Some(Meta::kind("delegation"))))
}

/// Hard limit: outside agents calling Lucina never drive delegation.
fn delegation_refused(lang: &str) -> Finish {
    let mut f = Finish::reply(
        lucina_delegate::policy::Refusal::McpOrigin.message(lang),
        "needs_confirmation",
    );
    f.extra.insert("blockedAction".into(), json!("delegate"));
    f.action_result = "[delegation refused: mcp_origin]".into();
    f
}

/// The JSON planner path's delegation hook (before the planner).
async fn delegation_hook(
    t: &Turn<'_>,
    d: &std::sync::Arc<lucina_delegate::Delegator>,
    memory: &str,
) -> Option<Finish> {
    if let Some(f) = delegation_settings(d, &t.message, t.guarded).await {
        return Some(f);
    }
    let i = lucina_delegate::intent::parse_delegation(&t.message)?;
    if t.guarded {
        tracing::warn!(session_id = %t.session, "secretary mcp turn refused: delegation");
        return Some(delegation_refused(i.lang));
    }
    let req = owner_request(t, i.to, i.kind, i.task, i.workspace);
    Some(delegated_finish(t, d, req, memory).await)
}

/// Run a delegation and turn it into the reply: a finished answer is
/// relayed in Lucina's words, anything else gets a fixed sentence (the
/// card on the timeline carries the details).
async fn delegated_finish(
    t: &Turn<'_>,
    d: &std::sync::Arc<lucina_delegate::Delegator>,
    req: lucina_delegate::Request,
    memory: &str,
) -> Finish {
    use crate::delegation as dg;
    let ran = dg::run(d, req).await;
    let fallback = dg::plain_reply(&ran);
    let reply = match &ran.job {
        Some(job) if ran.finished && job.status == lucina_delegate::JobStatus::Done => {
            let instruction = format!(
                "Relay this answer to Giuseppe in the language of his message: 1-4 short sentences, saying it comes from {}. If it says the full answer is in Notes, say so. Do not add facts that are not in it.",
                job.agent.label()
            );
            t.st.planner
                .generate_skill_reply(
                    &t.input(memory, t.platform_label()),
                    &format!("{} (asked by Lucina)", job.agent.label()),
                    &dg::answer_text(job),
                    &instruction,
                )
                .await
                .ok()
                .filter(|r| !r.trim().is_empty())
                .unwrap_or(fallback)
        }
        _ => fallback,
    };
    Finish::reply(reply, "delegate").with(dg::action_result(&ran), dg::meta(&ran))
}

/// Honcho: a saved note's summary (`rememberNote`).
fn remember_note(st: &AppState, surface: &TurnSurface, session: &str, note: &Value, source: &str) {
    if st.memory.mode() != MemoryMode::ReadWrite {
        return;
    }
    let user = format!(
        "[Secretary note saved by {source}] {} ({})",
        note["title"].as_str().unwrap_or(""),
        note["slug"].as_str().unwrap_or("")
    );
    let assistant = nt::summary_for_memory(note, source);
    let (mem, peer, sid, surface) = (
        st.memory.clone(),
        st.cfg.owner_peer_id.clone(),
        session.to_owned(),
        surface.clone(),
    );
    tokio::spawn(async move {
        if let Err(e) = mem.remember(&surface, &peer, &sid, &user, &assistant).await {
            tracing::warn!(error = %e, "note honcho remember failed");
        }
    });
}

/// Where a job reports: the turn's session, owner item, channel and surface,
/// and for a chat channel its timeline, recent turns, peer and reply sink.
struct JobDest {
    session: String,
    user_item: Option<String>,
    channel: String,
    /// The job keeps the surface of the turn that started it.
    surface: TurnSurface,
    origin: Option<Arc<ChannelOrigin>>,
}

/// `startJob`: run the action, report on the timeline and the job record
/// (and, from a chat channel, in the chat: it has no polling client).
fn spawn_job(st: AppState, job: Job, plan: Value, message: String, memory: String, dest: JobDest) {
    tokio::spawn(async move {
        let JobDest {
            session,
            user_item,
            channel,
            surface,
            origin,
        } = dest;
        let timeline = origin
            .as_ref()
            .map_or_else(|| st.timeline_for(&session), |o| o.timeline.clone());
        let record = |text: String, rich: Option<&lucina_store::rich::RichMessage>| {
            let meta = Some(Meta {
                job_id: Some(job.id.clone()),
                ..Meta::default()
            });
            let meta = match origin.as_ref().and_then(|o| o.timeline_meta.clone()) {
                Some(base) => meta.map(|m| base.merge(m)),
                None => meta,
            };
            timeline.record(lucina_store::timeline::NewItem {
                channel: channel.clone(),
                role: "lucina".into(),
                text,
                reply_to: user_item.clone(),
                meta: with_rich_meta(meta, rich),
                ..Default::default()
            })
        };
        let (recent, recent_key): (&RecentChat, &str) = match &origin {
            Some(o) => (&o.recent, &o.recent_key),
            None => (&st.recent, &session),
        };
        let _ = st.jobs.update(&job.id, |j| {
            j.status = "running".into();
            j.started_at = Some(now_iso());
        });
        let action = plan["action"].as_str().unwrap_or("").to_owned();
        let t = TurnInput {
            user_text: &message,
            memory: &memory,
            user_label: origin
                .as_ref()
                .map_or(USER_LABEL, |o| o.user_label.as_str()),
            platform: origin.as_ref().map_or(PLATFORM_LABEL, |o| o.platform_label),
        };
        let outcome: Result<(String, String), String> = async {
            match action.as_str() {
                "cursor_task" => {
                    let result = jobs::run_harness(&st.tool_env.http, &st.cfg.dsh, plan["prompt"].as_str().unwrap_or(&message)).await?;
                    let reply = st.planner.generate_skill_reply(&t, st.cfg.dsh.skill_name(), &result, "Answer in 1-3 short sentences. Say what was done or found. Do not dump raw logs.").await.map_err(|e| e.to_string())?;
                    Ok((reply, result))
                }
                "spreadsheet_update_task" => {
                    let prompt = jobs::spreadsheet_prompt(plan["spreadsheetName"].as_str().unwrap_or(""), plan["prompt"].as_str().unwrap_or(""), &message, &memory);
                    let result = jobs::run_harness(&st.tool_env.http, &st.cfg.dsh, &prompt).await?;
                    let reply = st.planner.generate_skill_reply(&t, "Google Drive spreadsheet update through Cursor", &result, "Answer in 1-3 short sentences. Say updated only if the result contains \"Verification: PASS\"; otherwise say it was not confirmed.").await.map_err(|e| e.to_string())?;
                    Ok((reply, result))
                }
                "oracle_monitor_task" => Ok(("Oracle SSH monitoring is disabled on nezopt. Oracle peer1 runs Poko only, not Lucina.".into(), String::new())),
                "generate_image" => Err("image generation (ComfyUI) is not available on the Rust Lucina yet".into()),
                "graph_task" => Err("graph_task needs rclone and the Drive cache, which the Rust Lucina does not have".into()),
                "social_post_task" => Err("social posts are published by Poko through the Node Lucina, not from the Rust Lucina".into()),
                other => Err(format!("Unsupported async action: {other}")),
            }
        }
        .await;
        let peer = origin
            .as_ref()
            .map_or(st.cfg.owner_peer_id.as_str(), |o| o.peer_id.as_str());
        let sent = match outcome {
            Ok((reply, action_result)) => {
                let result = if reply.trim().is_empty() {
                    "Done.".to_owned()
                } else {
                    reply.trim().to_owned()
                };
                // Job results are formatted without another model call.
                let (rich_msg, text) = if origin.is_some() {
                    (None, result.clone())
                } else {
                    let (rich_msg, text, _) =
                        rich_reply(&st, &result, &message, &action, "", false, &[], None).await;
                    (rich_msg, text)
                };
                let item = record(text, rich_msg.as_ref());
                let _ = st.jobs.update(&job.id, |j| {
                    j.status = "done".into();
                    j.result = Some(result.clone());
                    j.timeline_item_id = item.map(|i| i.id);
                    j.finished_at = Some(now_iso());
                });
                recent.append(recent_key, "assistant", &result, Utc::now());
                remember_job(
                    &st,
                    &surface,
                    JobMemory {
                        peer,
                        session: &session,
                        message: &message,
                        channel: origin.is_some(),
                    },
                    &action,
                    &result,
                    &action_result,
                );
                result
            }
            Err(reason) => {
                let reason: String = reason.chars().take(500).collect();
                tracing::error!(job_id = %job.id, %action, error = %reason.chars().take(300).collect::<String>(), "secretary job failed");
                let text = format!("{} failed: {reason}", jobs::label(&action));
                let item = record(text.clone(), None);
                let _ = st.jobs.update(&job.id, |j| {
                    j.status = "error".into();
                    j.error = Some(reason.clone());
                    j.timeline_item_id = item.map(|i| i.id);
                    j.finished_at = Some(now_iso());
                });
                remember_job(
                    &st,
                    &surface,
                    JobMemory {
                        peer,
                        session: &session,
                        message: &message,
                        channel: origin.is_some(),
                    },
                    &action,
                    &text,
                    "",
                );
                text
            }
        };
        st.jobs.finish(&job.id);
        if let Some(sink) = origin.as_ref().and_then(|o| o.on_job_done.clone()) {
            sink(sent).await;
        }
    });
}

/// Who a job's exchange is remembered for.
struct JobMemory<'a> {
    peer: &'a str,
    session: &'a str,
    message: &'a str,
    /// A chat channel's job: the user's text as he wrote it (Node), no app label.
    channel: bool,
}

fn remember_job(
    st: &AppState,
    surface: &TurnSurface,
    who: JobMemory<'_>,
    action: &str,
    reply: &str,
    action_result: &str,
) {
    if st.memory.mode() != MemoryMode::ReadWrite {
        return;
    }
    let message = who.message;
    let user = if who.channel {
        message.to_owned()
    } else {
        format!("[{PLATFORM_LABEL}] {message}")
    };
    let assistant = [
        format!("[action:{action}]"),
        reply.to_owned(),
        if action_result.is_empty() {
            String::new()
        } else {
            format!(
                "[result]\n{}",
                action_result.chars().take(3000).collect::<String>()
            )
        },
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    let (mem, peer, sid, surface) = (
        st.memory.clone(),
        who.peer.to_owned(),
        who.session.to_owned(),
        surface.clone(),
    );
    tokio::spawn(async move {
        if let Err(e) = mem.remember(&surface, &peer, &sid, &user, &assistant).await {
            tracing::warn!(error = %e, "secretary honcho remember failed");
        }
    });
}

// ---------------------------------------------------------------- tools mode

/// "Open in Drive" (in the reply's language: it/en/da) for Drive documents.
fn drive_label(lang: &str) -> &'static str {
    match lang {
        "en" => "Open in Drive",
        "da" => "Åbn i Drive",
        _ => "Apri nel Drive",
    }
}

/// "Show in Gallery", for films "Play the film" / "Show N in Films", for Drive documents
/// "Open in Drive" (the app opens the path; web and older clients ignore unknown routes).
fn gallery_action(path: &str, count: usize, lang: &str) -> Value {
    if path == "/drive" || path.starts_with("/drive/") {
        return json!({"id": "drive", "label": drive_label(lang), "emoji": "📁", "kind": "route", "payload": {"path": path}});
    }
    if path.starts_with("/films") {
        let label = if path.starts_with("/films/play/") {
            "Play the film".to_owned()
        } else {
            format!("Show {count} in Films")
        };
        return json!({"id": "films", "label": label, "emoji": "🎬", "kind": "route", "payload": {"path": path}});
    }
    let label = if count == 1 {
        "Show the photo".to_owned()
    } else {
        format!("Show {count} in Gallery")
    };
    json!({"id": "gallery", "label": label, "emoji": "🖼️", "kind": "route", "payload": {"path": path}})
}

/// Tool buttons first, then the composer's (up to the format's limit).
fn with_routes(
    msg: Option<lucina_store::rich::RichMessage>,
    routes: &[Value],
) -> Option<lucina_store::rich::RichMessage> {
    let mut msg = msg?;
    if !routes.is_empty() {
        let preset: Vec<Value> = routes
            .iter()
            .enumerate()
            .filter_map(|(i, a)| lucina_store::rich::normalize_action(a, i))
            .collect();
        msg.actions = lucina_store::rich::merge_actions(preset, std::mem::take(&mut msg.actions));
    }
    Some(msg)
}

fn effects_to_finish(effects: &[Effect], reply: String) -> Finish {
    let lang = crate::rich::guess_language(&reply, "");
    let mut f = Finish::reply(reply, "reply");
    if let Some((path, count)) = effects.iter().rev().find_map(|e| match e {
        Effect::Gallery { path, count } => Some((path.clone(), *count)),
        _ => None,
    }) {
        f.routes.push(gallery_action(&path, count, lang));
    }
    f.calendar_events = effects
        .iter()
        .flat_map(|e| match e {
            Effect::CalendarEvents { events } => events.as_slice(),
            _ => &[],
        })
        .cloned()
        .collect();
    if let Some((reminder, created)) = effects.iter().rev().find_map(|e| match e {
        Effect::ReminderCreated { reminder, created } => Some((reminder.clone(), *created)),
        _ => None,
    }) {
        f.action = "schedule_reminder".into();
        f.extra.insert("reminder".into(), reminder.public());
        f.action_result = format!(
            "[reminder {}]",
            if created { "created" } else { "existing" }
        );
    } else if let Some(list_id) = effects.iter().rev().find_map(|e| match e {
        Effect::Shopping { list_id } => Some(list_id.clone()),
        _ => None,
    }) {
        f.action = "grocery".into();
        f.meta = Some(Meta {
            list_id: Some(list_id),
            ..Meta::kind("shopping")
        });
    } else if effects.iter().any(|e| matches!(e, Effect::RemindersListed)) {
        f.action = "list_reminders".into();
    }
    if let Some((outfit, hair)) = effects.iter().rev().find_map(|e| match e {
        Effect::AvatarOutfit { outfit, hair } => Some((outfit.clone(), hair.clone())),
        _ => None,
    }) {
        if f.action == "reply" {
            f.action = "avatar_outfit".into();
            f.action_result = match (&outfit, &hair) {
                (None, None) => "[look auto]".into(),
                (o, h) => format!(
                    "[look{}{}]",
                    o.as_deref()
                        .map(|o| format!(" outfit {o}"))
                        .unwrap_or_default(),
                    h.as_deref()
                        .map(|h| format!(" hair {h}"))
                        .unwrap_or_default()
                ),
            };
        }
        if f.meta.is_none() {
            f.meta = Some(Meta {
                outfit,
                hair,
                ..Meta::kind("avatar_outfit")
            });
        }
    }
    f
}

fn tool_item_text(r: &ToolRecord) -> String {
    format!("🔧 {} → {}", r.summary, if r.ok { "ok" } else { "error" })
}

/// `LUCINA_PLANNER=tools`: the native tool loop with the confirmation flow.
#[allow(clippy::too_many_arguments)]
async fn tools_turn(
    st: AppState,
    message: String,
    session: String,
    cmid: Option<String>,
    channel: String,
    surface: TurnSurface,
    from_mcp: bool,
    existing: Option<ShareTurn>,
    reply_to: Option<String>,
) -> Result<Value, ApiError> {
    // The tool set follows `origin` (trusted MCP turns get the owner's);
    // memory reads and writes follow `surface`.
    let origin = if from_mcp && !st.cfg.mcp_trusted {
        Origin::Mcp
    } else {
        Origin::Owner
    };
    let mcp = matches!(origin, Origin::Mcp);
    let now = Utc::now();
    let tz = st.cfg.time_zone;
    let persona = st.cfg.persona.clone();
    let reply_meta = existing.as_ref().map(|s| s.reply_meta.clone());
    let secrets_text = existing.as_ref().map(|s| s.owner_text.clone());
    let voice_system = if existing.is_some() {
        crate::share::guidance::VOICE_SYSTEM
    } else {
        ""
    };
    let turn = TimelineTurn::new(st.timeline_for(&session), &channel, cmid.clone())
        .with_reply_to(reply_to);
    let turn = if let Some(s) = existing {
        turn.with_user_item(s.item)
    } else {
        turn
    };
    let user_item = turn.record_user(&message, Vec::new());
    // MCP callers never get the owner-only tools (secrets); a guarded MCP
    // turn gets no delegation either.
    let registry = if mcp {
        st.mcp_registry.clone()
    } else if from_mcp {
        st.trusted_mcp_registry.clone()
    } else {
        st.registry.clone()
    };
    let secrets = secrets_finish(&st, secrets_text.as_deref().unwrap_or(&message), from_mcp).await;
    let (memory_context, game_system) = if secrets.is_some() {
        (String::new(), String::new())
    } else {
        let p = crate::context::prepare_with(
            &st,
            &surface,
            &session,
            &message,
            USER_LABEL,
            PLANNER_PLATFORM,
            true,
        )
        .await;
        let game = if p.game.is_empty() || !mcp {
            p.game
        } else {
            format!("{}\n{}", p.game, crate::bridges::GAME_MCP_NOTE)
        };
        (p.memory_context, game)
    };
    st.recent.append(&session, "user", &message, now);
    let ctx = Ctx::new(st.tool_env.clone(), &channel, &session).with_origin(origin);
    let on_tool = |r: &ToolRecord| {
        if reply_meta.is_some() {
            return;
        }
        turn.record_lucina(
            &tool_item_text(r),
            Vec::new(),
            Some(Meta {
                tool: Some(r.name.clone()),
                ok: Some(r.ok),
                ..Meta::kind("tool")
            }),
        );
    };
    let agent_cfg = AgentConfig {
        deadline: st.cfg.turn_deadline,
        ..AgentConfig::default()
    };
    let t = TurnInput {
        user_text: &message,
        memory: &memory_context,
        user_label: USER_LABEL,
        platform: PLATFORM_LABEL,
    };
    let mut pending_note = String::new();
    let mut settled: Option<String> = None;
    let pending_now = if secrets.is_some() {
        None
    } else {
        st.pending.current(&session, now)
    };
    if let Some(p) = pending_now {
        match settle(true, classify_reply(&message), origin) {
            Settle::Nothing => {}
            Settle::Run => {
                if let Some(action) = st.pending.take(&p.id) {
                    let confirmed = Ctx::new(st.tool_env.clone(), &channel, &session).confirmed();
                    let (ok, output, summary) = agent::execute(
                        &registry,
                        &action.tool,
                        action.args.clone(),
                        &confirmed,
                        Duration::from_secs(30),
                    )
                    .await;
                    on_tool(&ToolRecord {
                        call_id: action.id.clone(),
                        name: action.tool.clone(),
                        args: action.args,
                        ok,
                        output: output.clone(),
                        summary: summary.clone(),
                        pending: None,
                        ms: 0,
                    });
                    settled = Some(format!("The owner confirmed the pending action \"{summary}\" and it has now run (ok: {ok}). Result: {output}"));
                }
            }
            Settle::Discard => {
                st.pending.take(&p.id);
                settled = Some(format!("The owner declined the pending action \"{}\". It was not run and is discarded.", p.summary));
            }
            Settle::StillPending => {
                pending_note = format!("A pending action awaits the owner's explicit confirmation: \"{}\" (expires {}). If this message does not answer it, handle the message normally and mention in one short sentence that the action still needs an explicit yes.", p.summary, p.expires_at);
            }
        }
    }
    let fallback = || {
        format!("{persona}: I misread that turn and could not recover it. Please send the message once more.")
    };
    let delegation = match (&st.delegator, &secrets) {
        (Some(d), None) => delegation_settings(d, &message, mcp).await,
        _ => None,
    };
    // Calendar questions ("cosa ho domani?", "dove si tiene la Hack Night?") are answered from
    // the calendar like on the JSON planner, never by the model from memory or notes.
    let calendar = if secrets.is_none()
        && delegation.is_none()
        && settled.is_none()
        && st.cfg.calendar_enabled
    {
        crate::bridges::calendar_read(&st.secretary, &message, now, tz).await
    } else {
        None
    };
    let finish = if let Some(f) = secrets {
        f
    } else if let Some(f) = delegation {
        f
    } else if let Some(o) = calendar {
        calendar_finish(o)
    } else if let Some(result) = settled {
        let reply = st.planner.generate_skill_reply(&t, "pending action", &result, "Tell the owner what happened in one or two sentences, in the language of the message.").await.unwrap_or_else(|_| fallback());
        effects_to_finish(&ctx.effects(), reply)
    } else {
        let extra = format!("{SECRETARY_EXTRA_SYSTEM} {voice_system} {pending_note}");
        let system = st.planner.prompts.agent_system(
            PLATFORM_LABEL,
            &message,
            &memory_context,
            extra.trim(),
            now,
            tz,
        );
        let system = if game_system.is_empty() {
            system
        } else {
            format!("{system}\n\n{game_system}")
        };
        let msgs = vec![
            Message::system(system),
            Message::user(Prompts::user_block(USER_LABEL, &memory_context, &message)),
        ];
        match agent::run(
            st.model.as_ref(),
            &registry,
            msgs,
            &ctx,
            &st.pending,
            &agent_cfg,
            &on_tool,
        )
        .await
        {
            Ok(out) if !out.text.is_empty() => {
                let mut f = effects_to_finish(&ctx.effects(), out.text);
                if meshy_turn::after_agent(&st, &out.records, &message).await {
                    meshy_turn::mark_pending(&st, &mut f, &message);
                }
                f
            }
            Ok(_) | Err(_) => {
                let reply = st
                    .planner
                    .generate_reply(
                        &t,
                        &format!("{SECRETARY_EXTRA_SYSTEM} {voice_system}"),
                        "",
                        "reply",
                    )
                    .await
                    .unwrap_or_else(|_| fallback());
                effects_to_finish(&ctx.effects(), reply)
            }
        }
    };
    let reply = if finish.reply.trim().is_empty() {
        "I could not generate a reply.".to_owned()
    } else {
        finish.reply.trim().to_owned()
    };
    let (rich_msg, reply, remembered) = rich_reply(
        &st,
        &reply,
        &message,
        &finish.action,
        &finish.action_result,
        // Secrets replies are fixed texts: no model rewrite.
        !finish.action.starts_with("secrets_"),
        &finish.calendar_events,
        finish.agenda.as_ref(),
    )
    .await;
    st.recent
        .append(&session, "assistant", &remembered, Utc::now());
    // MCP turns are remembered with their caller (label and metadata).
    let t2 = Turn {
        st: &st,
        message: message.clone(),
        session: session.clone(),
        channel: channel.clone(),
        mcp: surface.caller().map(|caller| McpTurn {
            caller: caller.to_owned(),
            text: message.clone(),
        }),
        surface: surface.clone(),
        guarded: false,
        user_label: USER_LABEL.into(),
        extra_system: String::new(),
        share: false,
        origin: None,
    };
    remember_turn(&st, &t2, &finish.action, &remembered, &finish.action_result);
    let lucina_item = turn.record_lucina(
        &reply,
        Vec::new(),
        with_rich_meta(finish.meta.clone(), rich_msg.as_ref())
            .map(|m| m.merge(reply_meta.clone().unwrap_or_default()))
            .or(reply_meta),
    );
    let mut out = json!({
        "reply": reply,
        "action": finish.action,
        "job": null,
        "sessionId": session,
        "clientMessageId": cmid,
        "userItemId": user_item.map(|i| i.id),
        "lucinaItemId": lucina_item.map(|i| i.id),
    });
    add_rich(&mut out, rich_msg.as_ref());
    if crate::selftest::is_session(&session_of(&out)) {
        out["conversation"] = json!(crate::selftest::CONVERSATION);
    }
    for (k, v) in finish.extra {
        out[k] = v;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn film_results_get_a_films_button() {
        let one = gallery_action("/films/play/itm_up", 1, "en");
        assert_eq!(
            (one["id"].as_str(), one["label"].as_str()),
            (Some("films"), Some("Play the film"))
        );
        assert_eq!(one["payload"]["path"], "/films/play/itm_up");
        let many = gallery_action("/films", 3, "en");
        assert_eq!(many["label"], "Show 3 in Films");
        assert_eq!(
            gallery_action("/gallery/album/sys-lucina", 4, "it")["label"],
            "Show 4 in Gallery"
        );
        assert_eq!(
            gallery_action("/gallery/album/sys-lucina", 1, "en")["label"],
            "Show the photo"
        );
    }

    #[test]
    fn drive_documents_get_an_open_in_drive_button_in_the_reply_language() {
        let one = gallery_action("/drive/file/itm_df5", 1, "en");
        assert_eq!(
            (
                one["id"].as_str(),
                one["label"].as_str(),
                one["kind"].as_str()
            ),
            (Some("drive"), Some("Open in Drive"), Some("route"))
        );
        assert_eq!(one["payload"]["path"], "/drive/file/itm_df5");
        let folder = gallery_action("/drive/folder/Lezioni/Digital%20Forensics", 4, "it");
        assert_eq!(folder["label"], "Apri nel Drive");
        assert_eq!(gallery_action("/drive", 9, "da")["label"], "Åbn i Drive");
        assert_eq!(gallery_action("/driveway", 2, "en")["id"], "gallery");
        let f = effects_to_finish(
            &[Effect::Gallery {
                path: "/drive/file/itm_df5".into(),
                count: 1,
            }],
            "Ho trovato le slide della lezione 5 di Digital Forensics: è il PDF più recente."
                .into(),
        );
        assert_eq!(f.routes[0]["label"], "Apri nel Drive");
    }
    use chrono::TimeZone;

    #[test]
    fn resolves_reminders_like_node() {
        let o = ParseOptions {
            tz: "Europe/Rome".parse().unwrap(),
            default_hour: 9,
            default_minute: 0,
            now: Utc.with_ymd_and_hms(2026, 10, 21, 15, 0, 0).unwrap(),
        };
        let (text, due) = resolve_reminder(
            "Ricordami alle 18:30 di chiamare la mamma",
            &json!({"text": ""}),
            &o,
        )
        .unwrap();
        assert_eq!(
            (text.as_str(), due.as_str()),
            ("chiamare la mamma", "2026-10-21T16:30:00.000Z")
        );
        let (text, due) = resolve_reminder(
            "domani devo chiamare il dentista, ricordamelo",
            &json!({"text": "chiamare il dentista", "dueDate": "2026-10-22"}),
            &o,
        )
        .unwrap();
        assert_eq!(
            (text.as_str(), due.as_str()),
            ("chiamare il dentista", "2026-10-22T07:00:00.000Z")
        );
        let (_, due) = resolve_reminder(
            "stasera palestra",
            &json!({"text": "palestra", "dueDate": "19:00"}),
            &o,
        )
        .unwrap();
        assert_eq!(due, "2026-10-21T17:00:00.000Z");
        assert!(resolve_reminder("boh", &json!({}), &o).is_none());
    }

    #[test]
    fn an_outfit_change_rides_on_the_reply_meta() {
        let f = effects_to_finish(
            &[Effect::AvatarOutfit {
                outfit: Some("maid".into()),
                hair: None,
            }],
            "Eccomi!".into(),
        );
        assert_eq!(f.action, "avatar_outfit");
        assert_eq!(
            serde_json::to_value(f.meta.unwrap()).unwrap(),
            json!({"kind": "avatar_outfit", "outfit": "maid"})
        );
        let both = effects_to_finish(
            &[
                Effect::Shopping {
                    list_id: "l1".into(),
                },
                Effect::AvatarOutfit {
                    outfit: Some("chef".into()),
                    hair: None,
                },
            ],
            "Ok".into(),
        );
        assert_eq!(both.action, "grocery");
        assert_eq!(both.meta.unwrap().kind.as_deref(), Some("shopping"));
    }
}
