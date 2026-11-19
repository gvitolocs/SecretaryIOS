//! `POST /api/secretary/chat`: one Lucina text turn.
//!
//! `LUCINA_PLANNER=json` (default) is the Node Lucina's `runChatTurn`, step
//! for step: timeline user item (with `meta: {via: "mcp", caller}` on Lucina
//! MCP turns) → turn context (tier-0 / DeepSeek intent, context policy,
//! gated sources) → outfit → notes → calendar → grocery → the JSON-action
//! planner → MCP guard → reminders / jobs / reply → recent turns, Honcho
//! (`readwrite` only), the Lucina item.
//!
//! LucinaVoice (`crate::voice`): `"voice": true` makes the turn spoken-style; inside `voice::scope` the turn
//! also streams its reply (`generate_reply` deltas) and reports it once settled, before the post-processing.
//!
//! `LUCINA_PLANNER=tools` is the native tool loop (Rust tools, confirmation
//! flow); it is not part of the Node parity surface.

use std::time::{Duration, Instant};

use chrono::{Datelike, Utc};
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
use crate::share::{Outcome, Sizes};
use crate::surface::BearerVerified;
use crate::{jobs, AppState, PlannerMode};
use lucina_store::recent::RecentChat;
use lucina_store::timeline::{Attachment, TimelineStore};

mod about_turn;
mod act_turn;
mod au_turn;
pub mod callers_turn;
pub mod drive_turn;
mod flaw_turn;
mod juno_turn;
pub mod media_turn;
pub mod memo_turn;
pub mod meshy_turn;
mod ops_turn;
mod parcel_turn;
pub mod plugin_turn;
mod reach_turn;
pub mod reply;
mod secret_turn;
mod share_turn;
mod voice_fast;
mod youtube_turn;

/// Counted after the MCP prefix (which may add up to 100 more on the wire).
pub const MAX_MESSAGE_CHARS: usize = 12_000;
const CLIENT_MESSAGE_TTL: Duration = Duration::from_secs(600);
const CLIENT_MESSAGE_KEEP: usize = 200;
pub const PLANNER_PLATFORM: &str = "secretary";
/// The LucinaVoice speakers' timeline channel and conversation (docs/lucinavoice-protocol.md section 7).
pub const VOICE_CHANNEL: &str = "voice";
/// The session of the Lucina Voice thread when the sender names none.
pub const VOICE_SESSION: &str = "lucinavoice";
/// The source label of a speaker turn in Honcho (the app's turns say `Secretary app`).
pub const VOICE_LABEL: &str = "LucinaVoice speaker";

/// The device fact of a turn from Juno's Chat (session `juno-<device id>`).
pub const JUNO_SYSTEM: &str = "The owner is writing from Juno, his laptop (Galaxy Book Go, LucinaOS). Actions on the laptop use the juno_* tools (juno_tools lists what it can do); the computer tool is your own sandbox on nezopt, not his laptop. You act on the laptop only through juno_* tool calls made in this turn. Never say you are opening, typing, capturing or changing anything on the laptop unless a juno_* call in this turn did it; otherwise say what you would do and that it has not happened yet.";

/// [`JUNO_SYSTEM`] when `session` is a Juno Chat session, else nothing.
pub fn juno_system(session: &str) -> &'static str {
    if lucina_tools::tools::juno::device_of_session(session).is_some() {
        JUNO_SYSTEM
    } else {
        ""
    }
}

/// The Juno fact of a turn. A Juno Chat session: [`JUNO_SYSTEM`] and the
/// laptop's sticky state. Another app's owner turn with the juno_* tools on:
/// whether the laptop is online (polled in the last 40 s) or offline since
/// when, with its sticky state while online; nothing if it was never seen.
/// MCP turns: nothing (they do not have the tools).
pub fn juno_fact(st: &crate::AppState, session: &str, mcp: bool) -> String {
    if !juno_system(session).is_empty() {
        return match st.juno.sticky_line() {
            Some(line) => format!("{JUNO_SYSTEM} On the laptop now: {line}"),
            None => JUNO_SYSTEM.to_owned(),
        };
    }
    if mcp || !st.cfg.juno_tools {
        return String::new();
    }
    st.juno.device_fact().unwrap_or_default()
}

pub const SECRETARY_EXTRA_SYSTEM: &str = "This conversation comes from the Secretary app on Giuseppe's iPhone. Reply in English, or in Danish when the message is in Danish; never in Italian. Keep it short: usually 1-3 sentences. Plain text only: no markdown tables or headings.";

/// Validate the body, classify the turn's memory surface (once), dedupe
/// `clientMessageId` retries, run the turn. `bearer`: the request passed the
/// Secretary bearer check.
///
/// LucinaVoice (`crate::voice`): `"voice": true` marks the turn as spoken aloud (a spoken-style rule joins
/// its system text); when the caller runs this inside [`crate::voice::scope`], the turn also reports its
/// reply text as it is written (`generate_reply` deltas) and once settled (`run_turn_scoped`).
pub async fn handle_chat(
    st: AppState,
    bearer: BearerVerified,
    body: Value,
) -> Result<Value, ApiError> {
    let spoken = body["voice"].as_bool().unwrap_or(false);
    // A spoken turn keeps a trace of its Jev layers (protocol section 9); the `done` body reports it.
    let trace = spoken.then(|| Arc::new(crate::voice::Trace::default()));
    let out = crate::voice::SPOKEN
        .scope(
            spoken,
            // Boxed: a debug build moves a future by value through each wrapper, and this one is huge.
            crate::voice::with_trace(trace.clone(), Box::pin(handle_chat_turn(st, bearer, body))),
        )
        .await;
    match (out, trace) {
        (Ok(mut out), Some(trace)) if !trace.is_empty() && out["duplicate"] != json!(true) => {
            out["voice"] = trace.json();
            Ok(out)
        }
        (out, _) => out,
    }
}

/// The `voiceMeta` a voice turn's sender attaches (secretary-server: its STT timing and the vocabulary's
/// doubtful words): `{turnId?, ms?: {..}, ambiguous?: [{heard, candidates}]}`. `base` is what goes on the
/// timeline items (`turnId`, `ms`), cut to size here and checked again by the store.
fn voice_base(body: &Value) -> Option<Value> {
    let m = body.get("voiceMeta")?.as_object()?;
    let mut out = serde_json::Map::new();
    if let Some(id) = m.get("turnId").and_then(Value::as_str) {
        out.insert(
            "turnId".into(),
            json!(id.chars().take(64).collect::<String>()),
        );
    }
    if let Some(ms) = m.get("ms").and_then(Value::as_object) {
        out.insert(
            "ms".into(),
            Value::Object(
                ms.iter()
                    .take(16)
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            ),
        );
    }
    (!out.is_empty()).then_some(Value::Object(out))
}

async fn handle_chat_turn(
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
    if !secretary_core::channel::CHAT_CHANNELS.contains(&channel.as_str()) {
        return Err(ApiError::bad_request(
            "channel must be web, ios, dsi, mac or voice",
        ));
    }
    // The Fortuna firmware that predates the `dsi` channel says `web`: its source and session id say DSi.
    let channel = secretary_core::channel::client_channel(
        &channel,
        body["source"].as_str(),
        Some(requested.as_str()),
        None,
    );
    // The speaker's own thread has its own session unless the sender names one.
    let requested = if requested.is_empty() && channel == VOICE_CHANNEL {
        VOICE_SESSION.to_owned()
    } else {
        requested
    };
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
    // "Add secret": `/secret …` never reaches a sink; tokens in a normal
    // message are masked before the turn and offered for saving after it.
    // "Add secret" and Infisical project offers are the owner's own: an MCP caller's prose never
    // makes a card in his chat (2026-10-28: an agent's decision question became a project offer).
    if mcp_turn.is_none() && crate::owner_secrets::is_secret_command(&text) {
        let (reply, lucina_item_id) =
            crate::owner_secrets::answer_secret_command(&st, &text, &st.timeline_for(&session));
        return Ok(json!({
            "reply": reply,
            "action": "secret_command",
            "sensitive": true,
            "sessionId": session,
            "clientMessageId": (!client_message_id.is_empty()).then_some(client_message_id),
            "job": null,
            "userItemId": null,
            "lucinaItemId": lucina_item_id,
        }));
    }
    let (text, held) = crate::owner_secrets::mask_message(&st, &text, mcp_turn.is_none());
    let keep_context = if held.is_empty() {
        String::new()
    } else {
        text.clone()
    };
    // "Salva un segreto": before the model. His words are recorded unless they look like they hold
    // the value (`NAME value`, not token-shaped so not masked above): those go nowhere at all.
    if let Some(words) =
        (held.is_empty() && mcp_turn.is_none() && !crate::selftest::is_session(&session))
            .then(|| crate::owner_secrets::secret_words(&text))
            .flatten()
    {
        let user = (!words.value_suspect)
            .then(|| {
                st.timeline.record(lucina_store::timeline::NewItem {
                    channel: channel.clone(),
                    role: "user".into(),
                    text: text.clone(),
                    client_message_id: (!client_message_id.is_empty())
                        .then(|| client_message_id.clone()),
                    reply_to: reply_to.clone(),
                    ..Default::default()
                })
            })
            .flatten();
        let (reply, lucina_item_id, rich) = crate::owner_secrets::answer_secret_words(
            &st,
            &words,
            crate::owner_secrets::lang_of(&text),
        )
        .await;
        let mut out = json!({
            "reply": reply,
            "action": "secret_request",
            "sessionId": session,
            "clientMessageId": (!client_message_id.is_empty()).then_some(client_message_id),
            "job": null,
            "userItemId": user.map(|u| u.id),
            "lucinaItemId": lucina_item_id,
        });
        if let (Some(o), Some(Value::Object(rich))) = (out.as_object_mut(), rich) {
            o.extend(rich);
        }
        return Ok(out);
    }
    // Voice Learn's `vocab` layer (after the masking: Jev never sees a token): doubtful spans of a spoken
    // sentence, settled by Jev when it is sure.
    let text = if crate::voice::spoken() {
        Box::pin(crate::voice_learn::apply_ambiguity(&st, text, &body)).await
    } else {
        text
    };
    if let Some(name) = (held.is_empty() && mcp_turn.is_none())
        .then(|| crate::owner_secrets::project_request(&text))
        .flatten()
    {
        let timeline = st.timeline_for(&session);
        let user = timeline.record(lucina_store::timeline::NewItem {
            channel: channel.clone(),
            role: "user".into(),
            text: text.clone(),
            client_message_id: (!client_message_id.is_empty()).then(|| client_message_id.clone()),
            reply_to: reply_to.clone(),
            ..Default::default()
        });
        let (reply, lucina_item_id) = crate::owner_secrets::answer_project_request(
            &timeline,
            &name,
            crate::owner_secrets::lang_of(&text),
        );
        return Ok(json!({
            "reply": reply,
            "action": "secret_project_offer",
            "sessionId": session,
            "clientMessageId": (!client_message_id.is_empty()).then_some(client_message_id),
            "job": null,
            "userItemId": user.map(|u| u.id),
            "lucinaItemId": lucina_item_id,
        }));
    }
    let device_meta = Meta {
        device: body["device"].as_str().map(str::to_owned),
        device_name: body["deviceName"].as_str().map(str::to_owned),
        // Who asked, on every item of the turn: the notifier sends the reply where he asked (docs/notifications.md).
        source: lucina_store::timeline::clean_source(body["source"].as_str().map(str::to_owned)),
        from_device: body["device"]
            .as_str()
            .filter(|d| lucina_store::timeline::valid_device(d))
            .map(str::to_owned),
        voice: voice_base(&body).filter(|_| channel == VOICE_CHANNEL),
        ..Meta::default()
    };
    let offer_timeline = st.timeline_for(&session);
    let offer_lang = crate::owner_secrets::lang_of(&text);
    let cmid = (!client_message_id.is_empty()).then_some(client_message_id);
    let (keep_st, keep_session) = (st.clone(), session.clone());
    if mcp_turn.is_none() {
        crate::about::observe(
            &st,
            &session,
            "chat",
            &cmid.clone().unwrap_or_default(),
            &text,
        );
    }
    let Some(cmid_value) = cmid.clone() else {
        let out = run_turn(
            st,
            text,
            session,
            None,
            channel,
            mcp_turn,
            surface,
            reply_to,
            device_meta,
        )
        .await;
        crate::secret_keep::after_turn(
            &keep_st,
            &offer_timeline,
            &held,
            &keep_context,
            offer_lang,
            &keep_session,
        );
        return out;
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
                    device_meta,
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
    crate::secret_keep::after_turn(
        &keep_st,
        &offer_timeline,
        &held,
        &keep_context,
        offer_lang,
        &keep_session,
    );
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
    mut origin: ChannelOrigin,
) -> Result<Value, ApiError> {
    // "Add secret" (see handle_chat): the owner's `/secret …` is answered
    // here; tokens are masked before the turn, then offered in the app.
    if origin.owner && crate::owner_secrets::is_secret_command(&message) {
        let (mut reply, _) =
            crate::owner_secrets::answer_secret_command(&st, &message, &st.timeline);
        reply.push_str(crate::owner_secrets::chat_app_note(
            crate::owner_secrets::lang_of(message.trim_start().get(7..).unwrap_or("")),
        ));
        return Ok(
            json!({"reply": reply, "action": "secret_command", "sensitive": true, "job": null}),
        );
    }
    let offer_lang = crate::owner_secrets::lang_of(&message);
    let (message, held) = if origin.owner {
        crate::owner_secrets::mask_message(&st, &message, true)
    } else {
        (
            lucina_secrets::redactor().redact(&message).into_owned(),
            vec![],
        )
    };
    // "Salva un segreto" in a chat app: the card is in the Secretary app; a value is never asked for here.
    if let Some(words) = (origin.owner && held.is_empty())
        .then(|| crate::owner_secrets::secret_words(&message))
        .flatten()
    {
        let (mut reply, _, _) =
            crate::owner_secrets::answer_secret_words(&st, &words, offer_lang).await;
        reply.push_str(crate::owner_secrets::chat_app_note(offer_lang));
        return Ok(
            json!({"reply": reply, "action": "secret_request", "sensitive": true, "job": null}),
        );
    }
    if let Some(t) = origin.timeline_text.as_mut() {
        let masked = if origin.owner {
            lucina_secrets::labelled::mask(t).into_owned()
        } else {
            t.clone()
        };
        *t = lucina_secrets::redactor().redact(&masked).into_owned();
    }
    let keep_context = if held.is_empty() {
        String::new()
    } else {
        message.clone()
    };
    let keep_session = session.clone();
    if origin.owner {
        crate::about::observe(&st, &session, "chat", &channel, &message);
    }
    let out = run_turn_inner(
        st.clone(),
        message,
        session,
        None,
        channel,
        None,
        surface,
        None,
        Some(Arc::new(origin)),
        None,
        Meta::default(),
    )
    .await;
    crate::secret_keep::after_turn(
        &st,
        &st.timeline,
        &held,
        &keep_context,
        offer_lang,
        &keep_session,
    );
    // The chat app keeps his message: its handler removes it where it can.
    out.map(|mut v| {
        if !held.is_empty() {
            v["secretMasked"] = json!(true);
        }
        v
    })
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
    /// Widgets the turn made itself (an AU map card and image), before the composer's.
    blocks: Vec<Value>,
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
            blocks: Vec::new(),
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

/// `action` does not run for this turn: an agent's consult turn gets a plain answer in Lucina's words
/// (what she thinks, nothing started), any other guarded turn the fixed refusal.
async fn not_run(t: &Turn<'_>, memory: &str, action: &str) -> Finish {
    if !t.consult {
        return Finish::refusal(t.persona(), action);
    }
    tracing::info!(session_id = %t.session, not_run = %action, "secretary mcp consult turn: answered only");
    let extra = format!(
        "{} The agent's message reads like a request for \"{action}\": do not claim you started, scheduled or ran anything. Answer it as a consultation, briefly.",
        t.extra_system
    );
    let text =
        t.st.planner
            .generate_reply(&t.input(memory, t.platform_label()), &extra, "", "reply")
            .await
            .unwrap_or_default();
    let text = if text.trim().is_empty() {
        "I only answer agents here: I did not start anything.".to_owned()
    } else {
        text
    };
    let mut f = Finish::reply(text, "reply");
    f.extra.insert("notRun".into(), json!(action));
    f.extra
        .insert("hint".into(), json!(mcp::consult_hint(action)));
    f
}

/// Node `resolveReminderRequest`: the parser first, then the plan's dueDate.
/// `"19:00"` → (19, 0).
fn hh_mm(s: &str) -> Option<(u32, u32)> {
    let (h, m) = s.split_once(':')?;
    if h.is_empty()
        || h.len() > 2
        || m.len() != 2
        || !h.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let (h, m) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

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
    // A part of the day ("stasera") only gives a default hour: when the message has no
    // time of its own, the planner's HH:MM sets the hour on the parsed day.
    if let (Some(p), Some((h, m))) = (&parsed, hh_mm(due_date)) {
        if !message.bytes().any(|b| b.is_ascii_digit()) {
            if let Some(d) = chrono::NaiveDate::parse_from_str(&p.local_date, "%Y-%m-%d")
                .ok()
                .and_then(|d| {
                    lucina_store::timeutil::zoned_to_utc(
                        o.tz,
                        d.year(),
                        d.month(),
                        d.day(),
                        h,
                        m,
                        0,
                    )
                })
                .filter(|d| *d > o.now)
            {
                due = Some(iso_ms(d));
            }
        }
    }
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
    /// An MCP turn held to the allowlist (`LUCINA_MCP_TRUSTED=false`, or an
    /// agent's consult turn), or a channel turn that is not the owner's.
    guarded: bool,
    /// An agent's MCP turn (not one of `LUCINA_MCP_OWNER_CLIENTS`): answers only, plus the
    /// kinds its prefix allows (`lucina_intent::mcp::CONSULT_ACTIONS`). Implies `guarded`.
    consult: bool,
    user_label: String,
    extra_system: String,
    /// The share pipeline's tool loop.
    share: bool,
    /// What the owner wrote with the attachment: his request (empty: a bare file).
    share_note: String,
    /// `None`: the Secretary app.
    origin: Option<Arc<ChannelOrigin>>,
}

impl Turn<'_> {
    /// The meta that marks this turn's items as an agent's (none for the owner's own turn).
    fn agent_mark(&self) -> Meta {
        self.mcp.as_ref().map_or_else(Meta::default, |m| Meta {
            via: Some("mcp".into()),
            caller: Some(m.caller.clone()),
            conversation: m.conversation.clone(),
            ..Meta::default()
        })
    }

    fn persona(&self) -> &str {
        &self.st.cfg.persona
    }

    /// May this turn run planner action `action`? (The owner: everything; a guarded MCP turn:
    /// `mcp::ALLOWED_ACTIONS`; an agent's consult turn: `mcp::CONSULT_ACTIONS` plus its `allow`.)
    fn may(&self, action: &str) -> bool {
        if self.consult {
            return mcp::CONSULT_ACTIONS.contains(&action)
                || mcp::allow_kind_of(action)
                    .is_some_and(|k| self.mcp.as_ref().is_some_and(|m| m.allows(k)));
        }
        !self.guarded || mcp::ALLOWED_ACTIONS.contains(&action)
    }

    /// May this turn run grocery intent `intent`?
    fn may_grocery(&self, intent: &str) -> bool {
        if self.consult {
            return mcp::CONSULT_GROCERY_INTENTS.contains(&intent)
                || (self.mcp.as_ref().is_some_and(|m| m.allows("shopping"))
                    && mcp::ALLOWED_GROCERY_INTENTS.contains(&intent));
        }
        !self.guarded || mcp::ALLOWED_GROCERY_INTENTS.contains(&intent)
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

    /// The device fact Jev's state gets: the owner's own turn from the DSi or the Mac, nothing else.
    fn device(&self) -> &'static str {
        if self.guarded || self.origin.is_some() {
            return "";
        }
        crate::device::jev_device_of_turn(&self.channel, self.mcp.is_some())
    }

    /// A fast route answered this spoken turn and dropped its context building with the sample it never
    /// got to keep. This is where that line is written instead: the same one [`crate::context::classify`]
    /// writes for the turns it finishes, from the same tier-0 rule and the same recent turns, and the
    /// turn's mark makes it nothing when the building did get there first (CO-C009). The caller runs it
    /// inside that mark ([`crate::voice::in_intent`]): outside it the turn would look unkept and write twice.
    fn record_turn_sample(
        &self,
        keys: crate::context::Keys<'_>,
        verdict: &crate::voice::SharedIntent,
    ) {
        let text = keys.intent_text.unwrap_or(&self.message);
        let recent = crate::context::classifier_recent(self.st, &self.surface, keys, Utc::now());
        crate::context::record_jev_sample(
            self.st,
            crate::context::sample_turn(&self.surface, &self.session),
            &lucina_intent::tier0::classify_tier0(text, &recent, &self.st.cfg.intent),
            text,
            self.planner_platform(),
            verdict,
        );
    }

    fn input<'b>(&'b self, memory: &'b str, platform: &'b str) -> TurnInput<'b> {
        TurnInput {
            user_text: &self.message,
            memory,
            user_label: &self.user_label,
            platform,
            device: self.device(),
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
    device_meta: Meta,
) -> Result<Value, ApiError> {
    run_turn_inner(
        st,
        message,
        session,
        cmid,
        channel,
        mcp_turn,
        surface,
        None,
        None,
        reply_to,
        device_meta,
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
    /// The phases of the share this turn answers: the turn adds its own wait for the intent step, the
    /// intent step, and the tool loop or the direct answer that follows it. What the share does after
    /// the turn (its rich reply, its memory write, its timeline item) falls inside the total of the
    /// summary line but gets no phase of its own.
    pub timing: crate::share::ShareTiming,
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
    // Who asked (the share's `source` and device), on every item the turn writes.
    let mark = Meta {
        source: input.reply_meta.source.clone(),
        from_device: input.reply_meta.from_device.clone(),
        ..Meta::default()
    };
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
        mark,
    )
    .await
}

/// Pins the turn's reply language (`planner::TURN_LANG`) before anything else runs: the language of the
/// owner's own text (a share's note, a voice note's transcript, the message), else of his recent messages,
/// else English. Never the device locale.
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
    device_meta: Meta,
) -> Result<Value, ApiError> {
    let owner_text = existing
        .as_ref()
        .map_or_else(|| message.clone(), |s| s.owner_text.clone());
    let line = match &origin {
        Some(o) => crate::planner::reply_language_line(&o.recent, &o.recent_key, &owner_text),
        None => crate::planner::reply_language_line(&st.recent, &session, &owner_text),
    };
    crate::planner::TURN_LANG
        .scope(
            line,
            run_turn_scoped(
                st,
                message,
                session,
                cmid,
                channel,
                mcp_turn,
                surface,
                existing,
                origin,
                reply_to,
                device_meta,
            ),
        )
        .await
}

#[allow(clippy::too_many_arguments)]
async fn run_turn_scoped(
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
    device_meta: Meta,
) -> Result<Value, ApiError> {
    // What was said, for the timeline and the recent turns (a confirmed request below runs as the held text).
    let heard = message.clone();
    let is_mcp = mcp_turn.is_some();
    let voice_eligible = crate::voice::spoken()
        && existing.is_none()
        && origin.is_none()
        && mcp_turn.is_none()
        && st.cfg.planner == PlannerMode::Json
        && !crate::selftest::is_session(&session);
    // A pending "sei sicuro?" (the confirm layer, section 9) is settled by this turn: a yes runs the request
    // that was held, a no cancels it, anything else drops it and the turn goes on as itself.
    let (message, confirmed, cancelled) = if voice_eligible {
        match voice_fast::settle_confirmation(&st, &session, &message) {
            voice_fast::Held::Run(original) => (original, true, None),
            voice_fast::Held::Cancel(text) => (message, false, Some(text)),
            voice_fast::Held::None => (message, false, None),
        }
    } else {
        (message, false, None)
    };
    // LucinaVoice, Jev fast path (docs/lucinavoice-protocol.md section 6): a spoken turn of the owner starts
    // Jev's intent call first of all, so it runs while the rest of the turn is prepared.
    let voice_route =
        (voice_eligible && cancelled.is_none()).then(|| voice_fast::start(&st, &message));
    // A routed spoken turn writes its owner item once the route is known, so that the item carries it.
    let defer_user_item = voice_route.is_some();

    let normal_share = existing.as_ref().is_some_and(|s| !s.voice_only);
    let secrets_text = existing.as_ref().map(|s| s.owner_text.clone());
    let voice_only = existing.as_ref().is_some_and(|s| s.voice_only);
    let reply_meta = existing.as_ref().map(|s| s.reply_meta.clone());
    // The phases of a share carry on inside its turn: what the turn waits for before the intent step
    // (the quoted message, the secrets pre-check) is time the owner waits too, and the share cannot
    // see it from its side.
    let timing = existing.as_ref().map(|s| s.timing.clone());
    let mut intent_wait = timing.as_ref().map(|t| t.begin("intent_wait", true));
    // Swipe to reply: the quoted message rides in every model call's user part.
    let reply_block = match reply_to.as_deref() {
        Some(id) => reply::block(&st, &surface, &session, id).await,
        None => String::new(),
    };
    // An id that names nothing readable is ignored, on the timeline too.
    let reply_to = reply_to.filter(|_| !reply_block.is_empty());
    // MCP turns are labelled with their caller; app turns with their device.
    let turn_meta = mcp_turn
        .as_ref()
        .map(|m| Meta {
            via: Some("mcp".into()),
            caller: Some(m.caller.clone()),
            conversation: m.conversation.clone(),
            ..Meta::default()
        })
        .unwrap_or_default()
        .merge(device_meta);
    if st.cfg.planner == PlannerMode::Tools && !normal_share && origin.is_none() {
        let from_mcp = mcp_turn.is_some();
        // Tools mode has no intent step: the turn goes straight to the tool loop.
        if let Some(wait) = intent_wait.take() {
            wait.finish(Outcome::Skipped, Sizes::NONE);
        }
        return crate::planner::TURN_REPLY
            .scope(
                reply_block,
                tools_turn(
                    st,
                    message,
                    session,
                    cmid,
                    channel,
                    surface,
                    from_mcp,
                    existing,
                    reply_to,
                    turn_meta.clone(),
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
    // The device the message came from, as in tools mode.
    let juno = juno_fact(&st, &session, mcp_turn.is_some());
    let base_system = match juno.as_str() {
        "" => base_system,
        juno => format!("{base_system} {juno}"),
    };
    // The DSi or the Mac he is writing from: on, connected, and not something she can act on herself.
    let base_system = match crate::device::system_fact(&channel, mcp_turn.is_some()) {
        "" => base_system,
        fact => format!("{base_system} {fact}"),
    };
    // Agents (every caller but the owner's own assistant apps) consult; they never act as the owner.
    let consult = mcp_turn
        .as_ref()
        .is_some_and(|m| !mcp::is_owner_client(&m.caller, &st.cfg.mcp_owner_clients));
    let note = |m: &McpTurn| {
        if consult {
            mcp::note_consult(m)
        } else if st.cfg.mcp_trusted {
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
    let share_note = if normal_share {
        secrets_text.clone().unwrap_or_default()
    } else {
        origin
            .as_ref()
            .filter(|o| o.share)
            .and_then(|o| o.timeline_text.clone())
            .unwrap_or_default()
    };
    let (timeline_store, turn_meta) = match &origin {
        Some(o) => (o.timeline.clone(), o.timeline_meta.clone()),
        None => (st.timeline_for(&session), Some(turn_meta)),
    };
    let timeline = TimelineTurn::new(timeline_store, &channel, cmid.clone())
        .with_meta(turn_meta.clone())
        .with_reply_to(reply_to);
    let mut timeline = if let Some(item) = existing {
        timeline.with_user_item(item.item)
    } else {
        timeline
    };
    let mut user_item = match &origin {
        Some(o) => timeline.record_user(
            o.timeline_text.as_deref().unwrap_or(&heard),
            o.user_attachments.clone(),
        ),
        None if defer_user_item => None,
        None => timeline.record_user(&heard, Vec::new()),
    };
    let guarded = (mcp_turn.is_some() && (!st.cfg.mcp_trusted || consult))
        || origin.as_ref().is_some_and(|o| !o.owner);
    let t = Turn {
        st: &st,
        message,
        session,
        channel,
        mcp: mcp_turn,
        surface,
        guarded,
        consult,
        user_label,
        extra_system,
        share,
        share_note,
        origin,
    };
    // Secrets first, without the model: a fixed refusal for reveal requests
    // (MCP included), status and audit for the owner.
    let not_owner = t.mcp.is_some() || t.origin.as_ref().is_some_and(|o| !o.owner);
    // A fast voice turn (voice_fast) is answered without the model: no rich-reply composer either.
    let mut voice_fast_turn = cancelled.is_some();
    let early = match cancelled {
        Some(text) => Some(Finish::reply(text, voice_fast::CONFIRM_ACTION)),
        None => {
            secrets_finish(
                &st,
                secrets_text.as_deref().unwrap_or(&t.message),
                not_owner,
                t.mcp.as_ref().map(|m| m.caller.as_str()),
            )
            .await
        }
    };
    let (finish, prepared) = match early {
        Some(f) => {
            t.recent()
                .append(t.recent_key(), "user", &heard, Utc::now());
            (f, None)
        }
        None => {
            // Everything above was the share waiting: the quoted message, the secrets pre-check, the
            // turn's own setup. The intent step begins with the context building.
            if let Some(wait) = intent_wait.take() {
                wait.finish(Outcome::Ok, Sizes::NONE);
            }
            let intent_phase = timing.as_ref().map(|t| t.begin("intent", false));
            let keys = crate::context::Keys {
                recent: t.recent(),
                recent_key: t.recent_key(),
                peer: t.peer(),
                voice: t.channel == VOICE_CHANNEL,
                // A share's intent is his note, never the shared content's words.
                intent_text: t.share.then_some(t.share_note.as_str()),
                device: t.device(),
            };
            let prepare = crate::context::prepare_keyed(
                &st,
                &t.surface,
                &t.session,
                keys,
                &t.message,
                &t.user_label,
                t.planner_platform(),
                t.origin.as_ref().map(|o| o.owner),
                false,
            );
            // Voice: Jev's route and the context building run together (the building holds its
            // model calls until the route is known, and takes the route's intent verdict instead of
            // asking Jev again); a fast intent answers the turn here and drops it.
            let (open, gate) = crate::voice::gate();
            // The turn's verdict channel: the building reads Jev's answer through it, and a route that
            // answers before the building does keeps the sample through the same one (CO-C009).
            let slot = voice_route.as_ref().map(voice_fast::Started::slot);
            let prepare = crate::voice::with_intent(
                slot.clone(),
                Box::pin(crate::voice::gated(gate, prepare)),
            );
            let mut prepare = std::pin::pin!(prepare);
            let mut early = None;
            let mut fast = None;
            if let Some(started) = voice_route {
                let (outcome, announce, done) = started.verdict(prepare.as_mut()).await;
                early = done;
                fast = Box::pin(voice_fast::route(&t, outcome, announce, confirmed)).await;
                // The route is known: the owner item carries it, and the Lucina item after it.
                if let Some(trace) = crate::voice::trace() {
                    let mut meta = turn_meta.clone().unwrap_or_default();
                    meta.voice = Some(trace.meta(meta.voice.as_ref()));
                    timeline.set_meta(Some(meta));
                }
                user_item = timeline.record_user(&heard, Vec::new());
            }
            let _ = open.send(true);
            if let Some(finish) = fast {
                voice_fast_turn = true;
                // The dropped building left its sample unwritten; and this is before the turn's own words
                // join the recent turns, where the rule the classifier reads would not be the same one.
                if let Some(slot) = slot {
                    let verdict = slot.verdict();
                    crate::voice::in_intent(slot, || t.record_turn_sample(keys, &verdict));
                }
                // Jev answered the intent on its own: the step is over here, without the context building.
                if let Some(intent) = intent_phase {
                    intent.finish(Outcome::Ok, Sizes::NONE);
                }
                t.recent()
                    .append(t.recent_key(), "user", &heard, Utc::now());
                (finish, None)
            } else {
                let prepared = match early {
                    Some(p) => p,
                    None => prepare.await,
                };
                if let Some(intent) = intent_phase {
                    intent.finish(
                        Outcome::Ok,
                        Sizes::chars(prepared.memory_context.chars().count()),
                    );
                }
                t.recent()
                    .append(t.recent_key(), "user", &heard, Utc::now());
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
                // `"voice": true`: every model call of the turn (the plan, the replies) is told it is spoken.
                let game_system = if crate::voice::spoken() {
                    format!("{game_system}\n{}", crate::voice::SPOKEN_RULE)
                        .trim()
                        .to_owned()
                } else {
                    game_system
                };
                // The Lucina Voice thread is an owner device, not an outside platform.
                let game_system = if t.channel == VOICE_CHANNEL {
                    format!("{game_system}\n{}", crate::prompts::VOICE_SURFACE_RULE)
                        .trim()
                        .to_owned()
                } else {
                    game_system
                };
                // The DSi or the Mac he is writing from, in every model call of the turn.
                let game_system = match crate::device::system_fact(&t.channel, t.mcp.is_some()) {
                    "" => game_system,
                    fact => format!("{game_system}\n{fact}").trim().to_owned(),
                };
                let agent_phase = timing.as_ref().map(|t| t.begin("agent", false));
                let finish = crate::planner::TURN_REPLY
                    .scope(
                        reply_block,
                        crate::planner::TURN_SYSTEM
                            .scope(game_system, json_turn(&t, &prepared, cmid.as_deref())),
                    )
                    .await;
                if let Some(agent) = agent_phase {
                    agent.finish(
                        if finish.reply.trim().is_empty() {
                            Outcome::Error
                        } else {
                            Outcome::Ok
                        },
                        Sizes::chars(finish.reply.chars().count()),
                    );
                }
                (finish, Some(prepared))
            }
        }
    };

    // LucinaVoice: the reply text is settled here, before rich-reply, Honcho, memory and the timeline.
    if !finish.reply.trim().is_empty() {
        crate::voice::settle(finish.reply.trim(), false);
    }

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
            // Secrets replies are fixed texts, AU Find, caller and Jev yes/no answers keep their wording: no model rewrite.
            !voice_fast_turn
                && !finish.action.starts_with("secrets_")
                && finish.action != au_turn::ACTION
                && finish.action != callers_turn::ACTION
                && finish.action != crate::jev_yesno::ACTION,
            &finish.calendar_events,
            finish.agenda.as_ref(),
            is_mcp,
        )
        .await
    };
    // A turn's own blocks and buttons (an AU map, "Search au.dk") need a rich message even when the
    // composer is off or gave none.
    let rich_msg = match rich_msg {
        None if t.origin.is_none()
            && (((finish.action == au_turn::ACTION || finish.action == callers_turn::ACTION)
                && (!finish.blocks.is_empty() || !finish.routes.is_empty()))
                // A tool's picture (a Juno screenshot).
                || finish.blocks.iter().any(|b| b["type"] == "image")) =>
        {
            Some(lucina_store::rich::RichMessage {
                format: "v1".into(),
                bubbles: vec![reply.clone()],
                lang: Some(crate::rich::guess_language(&reply, &t.message).to_owned()),
                blocks: Vec::new(),
                actions: Vec::new(),
            })
        }
        m => m,
    };
    let rich_msg = with_blocks(with_routes(rich_msg, &finish.routes), &finish.blocks);
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
    // A spoken turn that never reached the route (a secrets answer) writes its owner item now.
    if user_item.is_none() {
        user_item = timeline.record_user(&heard, Vec::new());
    }
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
                turn_meta,
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
    if t.consult {
        out["consult"] = json!(true);
    }
    if crate::selftest::is_session(&session_of(&out)) {
        out["conversation"] = json!(crate::selftest::CONVERSATION);
    }
    for (k, v) in finish.extra {
        out[k] = v;
    }
    Ok(out)
}

/// A calendar bridge answer as the turn's finish.
/// Who a calendar write is for: the owner (created at once), one of his MCP clients (a
/// proposal), or a guarded turn (refused).
fn calendar_caller(t: &Turn<'_>) -> crate::bridges::CalendarCaller {
    match &t.mcp {
        _ if t.guarded => crate::bridges::CalendarCaller::Guarded,
        Some(m) => crate::bridges::CalendarCaller::Client {
            from: format!("mcp:{}", m.caller),
        },
        None => crate::bridges::CalendarCaller::Owner {
            channel: t.channel.clone(),
        },
    }
}

/// The full planner for a turn whose tier-0 plan carries no fields: `(plan, action)`, the
/// original ones when planning fails.
async fn replan(t: &Turn<'_>, memory: &str, plan: Value, action: String) -> (Value, String) {
    match t
        .st
        .planner
        .plan_response(&t.input(memory, t.planner_platform()))
        .await
    {
        Ok(p) => {
            let a = p["action"]
                .as_str()
                .filter(|a| !a.is_empty())
                .unwrap_or("reply")
                .to_owned();
            tracing::info!(from = %action, to = %a, "replanned a tier-0 plan without fields");
            (p, a)
        }
        Err(e) => {
            tracing::warn!(error = %e, "replanning failed");
            (plan, action)
        }
    }
}

/// The planner's `calendar_event`: created in the owner's calendar like the calendar bridge
/// does, or one specific question when the name or the day is missing.
async fn calendar_event_turn(t: &Turn<'_>, plan: &Value, memory: &str) -> Finish {
    let st = t.st;
    let tz = st.cfg.time_zone;
    let lang = lucina_intent::calendar::guess_lang(&t.message);
    if !st.cfg.calendar_enabled {
        return Finish::reply(
            "The calendar is not enabled on this host.".to_owned(),
            "calendar_event",
        );
    }
    let caller = calendar_caller(t);
    if caller == crate::bridges::CalendarCaller::Guarded {
        return not_run(t, memory, "calendar.create").await;
    }
    match lucina_intent::calendar::create_from_plan(plan, Utc::now(), tz, lang) {
        Ok(created) => {
            let key = if t.session.is_empty() {
                "default".to_owned()
            } else {
                t.session.clone()
            };
            calendar_finish(
                crate::bridges::create_event(
                    &st.secretary,
                    &st.bridge_state,
                    created,
                    key,
                    &caller,
                    tz,
                )
                .await,
            )
        }
        Err(missing) => Finish::reply(
            lucina_intent::calendar::event_question(
                lang,
                plan["title"].as_str().unwrap_or(""),
                missing,
            ),
            "calendar_event",
        )
        .with(format!("[calendar event unclear: {missing}]"), None),
    }
}

fn calendar_finish(o: crate::bridges::Outcome) -> Finish {
    let mut f = Finish::reply(o.reply, &o.action).with(o.action_result, o.meta);
    f.calendar_events = o.events;
    f.agenda = o.agenda;
    f.routes = o.routes;
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
    mcp: bool,
) -> (Option<lucina_store::rich::RichMessage>, String, String) {
    // An MCP client is an agent reading text: no bubbles, no buttons, nothing cut.
    if !st.cfg.rich_replies || mcp {
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
async fn secrets_finish(
    st: &AppState,
    text: &str,
    mcp: bool,
    mcp_caller: Option<&str>,
) -> Option<Finish> {
    // An MCP caller may get the owner's secrets answers when the owner approves (crate lucina_approvals).
    let gate = match (mcp_caller, st.tool_env.approvals.as_deref()) {
        (Some(caller), Some(approvals)) => Some(crate::secrets::McpApproval { approvals, caller }),
        _ => None,
    };
    let (reply, action) =
        crate::secrets::secrets_turn(st.secrets.as_deref(), text, mcp, gate).await?;
    let mut f = Finish::reply(reply, action);
    if action == "secrets_refused" {
        f.extra.insert("blockedAction".into(), json!("secrets"));
    }
    f.action_result = format!("[{action}]");
    Some(f)
}

/// The action of a turn that used the owner-only plugin tools: never sent to long-term memory.
pub(crate) const PRIVATE_ACTION: &str = "re_plugins";

/// Honcho `remember` (`readwrite` only), Node's `rememberSecretaryTurn`.
fn remember_turn(st: &AppState, t: &Turn<'_>, action: &str, reply: &str, action_result: &str) {
    if st.memory.mode() != MemoryMode::ReadWrite || crate::selftest::is_session(&t.session) {
        return;
    }
    if action == PRIVATE_ACTION {
        // The owner-only plugins (Instagram, Pixiv): nothing of the exchange goes to Honcho.
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
            "[{PLATFORM_LABEL}, via Lucina MCP from {}; written by that agent, not by Giuseppe] {}",
            m.label(),
            t.message
        ),
        None if t.channel == VOICE_CHANNEL => format!("[{VOICE_LABEL}] {}", t.message),
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

/// Queue an async action as a job: the ack now, the result on the timeline when it finishes
/// (`spawn_job`). `computer_task` carries its own ack in English (Danish when he writes Danish; never Italian) (`plan["ack"]`).
fn queue_job(t: &Turn<'_>, plan: &Value, action: &str, cmid: Option<&str>) -> Finish {
    let st = t.st;
    let blocker = if action == "generate_image" && st.cfg.image_jobs {
        None
    } else {
        jobs::blocker(plan, &st.cfg.dsh)
    };
    if let Some(b) = blocker {
        return Finish::reply(b, action);
    }
    let now = now_iso();
    let job = Job {
        id: uuid::Uuid::new_v4().to_string(),
        action: action.to_owned(),
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
            let ack = plan["ack"]
                .as_str()
                .map(str::trim)
                .filter(|a| matches!(action, "computer_task" | "agent_task") && !a.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    format!(
                        "On it: I started the {}. The result will appear here when it finishes.",
                        jobs::label(action)
                    )
                });
            let mut f = Finish::reply(ack, action);
            f.job = Some(job);
            f.job_plan = Some(plan.clone());
            f
        }
        Err(e) => {
            tracing::error!(error = %e, "secretary job insert failed");
            Finish::reply(
                format!("I could not start the {}.", jobs::label(action)),
                action,
            )
        }
    }
}

/// Lucina's own computer (lucina_computer_use): she does the job herself in her sandbox, as a job. The owner
/// gets her ack at once ("I can check via my computer, starting now") and the result lands on the timeline.
/// A planner `web_crawl_task` (read a page, or search the web) as a read-only `computer_task` in her
/// one Chromium window. The ack is the planner's when it gave one.
fn web_crawl_as_computer_task(plan: &Value, message: &str) -> Value {
    let s = |k: &str| plan[k].as_str().map(str::trim).unwrap_or_default();
    let goal = if s("query").is_empty() {
        message.trim()
    } else {
        s("query")
    };
    let what = if s("url").is_empty() {
        format!("Search the web for this in a new tab (Google): {goal}")
    } else {
        format!("Open {} in a new tab and read it. Goal: {goal}", s("url"))
    };
    let ack = if s("ack").is_empty() {
        crate::planner::web_read_ack()
    } else {
        s("ack")
    };
    json!({
        "action": "computer_task",
        "task": format!("{what}. Use your one Chromium window through accessibility tools (get_window_state, click, type_text, press_key; ctrl+t opens a new tab). Read only: do not log in, buy, post, send or change anything. Answer briefly with what the page actually says and its URL."),
        "ack": ack,
    })
}

async fn computer_task_finish(t: &Turn<'_>, plan: &Value, cmid: Option<&str>) -> Finish {
    queue_job(t, plan, "computer_task", cmid)
}

/// The job body of a `computer_task`: runs the task and words the outcome for the owner.
async fn run_computer_task(task: &str) -> Result<(String, String), String> {
    if !lucina_computer_use::shared_client().ready() {
        return Err("My own computer is switched off on this runtime.".into());
    }
    match lucina_computer_use::run_task("lucina", task, &lucina_computer_use::Cancel::new()).await {
        Ok(r) => {
            let status = r.status.as_str();
            let text = if r.reply.trim().is_empty() {
                format!("Done on my computer ({status}).")
            } else {
                r.reply.clone()
            };
            Ok((
                text,
                format!("[computer {status}: {} tool calls]", r.executed_tools.len()),
            ))
        }
        Err(lucina_computer_use::Error::OwnerDriving) => Ok((
            "You're driving my computer from the live view right now, so I left it alone. Give it back and ask me again.".into(),
            "[computer: owner driving]".into(),
        )),
        Err(e) => {
            tracing::warn!(error = %e, "computer task failed");
            Err(format!("My computer couldn't finish that ({}).", e.code()))
        }
    }
}

async fn json_turn(
    t: &Turn<'_>,
    prepared: &crate::context::Prepared,
    cmid: Option<&str>,
) -> Finish {
    let st = t.st;
    let memory = prepared.memory_context.as_str();

    // The owner's answer to a decision Lucina asked him about (crate::decide): his own turns only.
    if !t.guarded && t.mcp.is_none() {
        if let Some(f) = crate::decide::owner_answer(st, &t.message).await {
            return Finish::reply(f.reply, "decision")
                .with(f.action_result, Some(Meta::kind("decision")));
        }
    }
    // A decision's status ("what's the state of dc-…?"), for anyone who asks: from the log, no routing.
    if let Some(reply) = crate::decide::status_answer(st, &t.message) {
        return Finish::reply(reply, "decision")
            .with("[decision status]", Some(Meta::kind("decision")));
    }

    // A YouTube link and "transcribe / report / summarise" (crate::video): a background job.
    if let Some(f) = youtube_turn::try_turn(t, cmid) {
        return f;
    }

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

    // A message for Claude, Grok or Lucina Agents ("tell Claude that…", "use Lucina agents"): before
    // her computer, the plugins and delegation, so a link or a plugin word in it cannot take it.
    if let Some(f) = reach_turn::try_turn(
        t,
        memory,
        &prepared.recent_context,
        &prepared.classification.intent,
    )
    .await
    {
        return f;
    }

    // A flaw he noticed ("emoji are boxes"): into the tracker in his words (crate::flaws).
    if let Some(f) = flaw_turn::try_turn(
        t,
        &prepared.classification.intent,
        prepared.classification.confidence,
    ) {
        return f;
    }

    // He asks for something on one of his devices or machines that she has no tool for: an agent does
    // it. Jev routes this; no text is matched (crate::act).
    if let Some(f) = act_turn::try_turn(
        t,
        &prepared.classification.intent,
        prepared.classification.confidence,
        cmid,
    )
    .await
    {
        return f;
    }

    // A parcel to follow ("track this 3UW1S8IAAKA23"), or where his parcels are (crate::parcels).
    if let Some(f) = parcel_turn::try_turn(
        t,
        &prepared.classification.intent,
        prepared.classification.confidence,
    )
    .await
    {
        return f;
    }

    // "What tools do you have?": her real list, from the registry and her desktop.
    if !t.guarded && lucina_intent::computer_route::asks_for_her_tools(&t.message) {
        return Finish::reply(crate::computer::her_tools(&st.registry).await, "reply")
            .with("[her tools listed]", None);
    }

    // Her own computer ("explore Outlook in your sandbox", "check my inbox"): before every other shortcut,
    // so the notes, tasks, calendar, memo and media routes, and a playing game, cannot take the message.
    if let Some(plan) = lucina_intent::computer_route::plan(&t.message) {
        if !t.may("computer_task") {
            return not_run(t, memory, "computer_task").await;
        }
        return computer_task_finish(t, &plan, cmid).await;
    }

    // "Salvami la password del Wi-Fi": a card in his chat, by name (crate::secret_requests).
    if let Some(f) = secret_turn::try_turn(t, memory).await {
        return f;
    }

    // "That's wrong, …", "remember that I …": the memory about him, now (crate::about).
    if let Some(f) = about_turn::try_turn(t, memory).await {
        return f;
    }

    // The ops record (crate::ops): "what happened with X" from the record; the owner's decisions recorded.
    if let Some(f) = ops_turn::try_turn(t, memory, Utc::now()).await {
        return f;
    }

    // AU Find (Aarhus University people, buildings, campus maps): read-only, MCP turns included.
    if let Some(f) = au_turn::try_turn(t, memory).await {
        return f;
    }

    // "Who called me from <number>?" / a pasted list: Jev's table, read-only, MCP turns included.
    if let Some(f) = callers_turn::try_turn(t, memory).await {
        return f;
    }

    // A yes/no question (crate::jev_yesno): Jev answers from what she knows, or she says she isn't sure
    // (or hedges a medical, legal or financial one) before the normal answer. The owner's own turns only
    // (no MCP, no other senders); it answers and never acts.
    if t.guarded || t.consult || t.mcp.is_some() || !crate::jev_yesno::enabled() {
        return json_turn_rest(t, prepared, cmid).await;
    }
    let yn = crate::jev_yesno::run(
        st,
        &t.message,
        memory,
        &prepared.recent_context,
        crate::jev_yesno::turn_lang_code(),
        Utc::now(),
    )
    .await;
    yn.log("turn");
    let badge = |f: Finish| {
        let jev = Meta {
            jev: yn.badge(),
            ..Meta::default()
        };
        Finish {
            meta: Some(f.meta.clone().unwrap_or_default().merge(jev)),
            ..f
        }
    };
    match &yn.outcome {
        crate::jev_yesno::Outcome::Answer { reply } => {
            badge(Finish::reply(reply.clone(), crate::jev_yesno::ACTION).with("[jev yes/no]", None))
        }
        crate::jev_yesno::Outcome::Pass { .. } => json_turn_rest(t, prepared, cmid).await,
        crate::jev_yesno::Outcome::Unsure { line } | crate::jev_yesno::Outcome::Hedge { line } => {
            let mut f = json_turn_rest(t, prepared, cmid).await;
            // A job's ack or a refusal is said as it is.
            if f.job.is_some() || f.action == "needs_confirmation" {
                return f;
            }
            f.reply = format!("{line}\n\n{}", f.reply.trim());
            badge(f)
        }
    }
}

/// What she asks when a message reads like a reminder but neither she nor the planner could place it.
fn reminder_question(message: &str) -> String {
    match lucina_intent::calendar::guess_lang(message) {
        "da" => "Hvornår skal det være? Hvis det er en aftale, så sig navn, dag og tidspunkt, så sætter jeg den i din kalender. Skal jeg minde dig om noget, så sig hvad og hvornår.",
        _ => "When is that? If it's an appointment, tell me its name, day and time and I'll put it in your calendar. If you want a reminder, tell me what and when.",
    }
    .to_owned()
}

/// The reminder write of a turn: the JSON planner's `schedule_reminder` and the voice fast path
/// (`voice_fast`) both go through it.
async fn create_turn_reminder(
    t: &Turn<'_>,
    chat: String,
    text: String,
    due: String,
) -> Result<(bool, lucina_store::reminders::Reminder), lucina_tools::reminder_writer::WriteError> {
    t.st.writer
        .create(CreateReminder {
            text,
            due_at: due,
            chat_id: chat,
            user_peer_id: t.peer().to_owned(),
            user_label: t.user_label.clone(),
            persona: t.persona().to_owned(),
            time_zone: t.st.cfg.time_zone.name().into(),
            dedupe_key: None,
            source: t
                .origin
                .as_ref()
                .map_or("secretary", |o| o.reminder_source)
                .into(),
        })
        .await
}

/// The JSON turn after the read-only shortcuts that come before yes/no questions (decisions, YouTube,
/// shares, her computer, the ops record, AU Find, caller screening).
async fn json_turn_rest(
    t: &Turn<'_>,
    prepared: &crate::context::Prepared,
    cmid: Option<&str>,
) -> Finish {
    let memory = prepared.memory_context.as_str();

    // Meshy 3D (LUCINA_MESHY=true): a pending plan's yes/no, or a 3D request. Not for agents.
    if !t.consult {
        if let Some(f) = meshy_turn::try_turn(t, memory).await {
            return f;
        }
    }

    // Juno's Chat: a pending laptop action, or Jev's guess names a juno_* tool.
    // In the Juno window the planner path that follows ran no tool loop: no narrated actions.
    let juno_rule = juno_turn::planner_system(&t.session);
    if let Some(f) = juno_turn::try_turn(t, memory).await {
        return f;
    }
    match juno_rule {
        Some(rule) => {
            crate::planner::TURN_SYSTEM
                .scope(
                    with_juno_rule(&rule),
                    Box::pin(json_turn_planner(t, prepared, cmid)),
                )
                .await
        }
        None => json_turn_planner(t, prepared, cmid).await,
    }
}

/// The turn's fixed system text (`TURN_SYSTEM`) with the Juno planner rule after it.
fn with_juno_rule(rule: &str) -> String {
    let base = crate::planner::TURN_SYSTEM
        .try_with(|g| g.trim().to_owned())
        .unwrap_or_default();
    format!("{base}\n{rule}").trim().to_owned()
}

/// [`json_turn_rest`] after the Juno turn: the other skills, then the JSON planner.
async fn json_turn_planner(
    t: &Turn<'_>,
    prepared: &crate::context::Prepared,
    cmid: Option<&str>,
) -> Finish {
    let st = t.st;
    let persona = t.persona().to_owned();
    let memory = prepared.memory_context.as_str();

    // Lucina's own Google Drive (LUCINA_DRIVE=true with a grant), read-only.
    if let Some(f) = drive_turn::try_turn(t, memory).await {
        return f;
    }

    // The owner's Instagram and Pixiv plugins (owner's app chat only).
    if let Some(f) = plugin_turn::try_turn(t, memory).await {
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
            if t.consult {
                return not_run(t, memory, "delegation").await;
            }
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
        // "Read this photo and save it as a note" is a photo first: the media turn reads it and saves the reading.
        let notes = nt::parse_notes_intent(&t.message, pending)
            .filter(|_| !media_turn::is_media_request(&t.message));
        if let Some(intent) = notes {
            let read = matches!(
                intent,
                nt::NotesIntent::Read { .. } | nt::NotesIntent::List { .. }
            );
            if !(t.may("notes") || (t.consult && read)) {
                return not_run(t, memory, "notes").await;
            }
            // Who wrote it: the MCP caller, a channel sender who is not the owner, or the owner.
            let source = match (&t.mcp, &t.origin) {
                (Some(m), _) => mcp::note_source(&m.caller),
                (None, Some(o)) if !o.owner => mcp::note_source(o.planner_platform),
                _ => "owner".into(),
            };
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
    // Agents read tasks with tasks_list and write with task_add (or allow=tasks).
    if st.cfg.tasks_enabled && (!t.consult || t.may("tasks")) {
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

    // Calendar questions and event creation (LUCINA_CALENDAR=true): the owner's own message
    // creates the event at once (with an Undo), one of his MCP clients files a proposal.
    if st.cfg.calendar_enabled {
        let caller = calendar_caller(t);
        match crate::bridges::calendar_turn(
            &st.secretary,
            &st.bridge_state,
            &t.message,
            &t.session,
            &caller,
            Utc::now(),
            st.cfg.time_zone,
        )
        .await
        {
            Some(Err(blocked)) => {
                tracing::warn!(session_id = %t.session, %blocked, "secretary mcp turn refused");
                return not_run(t, memory, blocked).await;
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
    if let Some(intent) = grocery_intent {
        if !t.may_grocery(intent) {
            tracing::warn!(session_id = %t.session, blocked = intent, "secretary mcp turn refused");
            return not_run(t, memory, intent).await;
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

    // The JSON-action planner, with the turn's classification. While her game is playing (not paused or
    // finished), owner turns skip the casual chat shortcut (which clips the context and cannot choose actions)
    // so she reasons over the game state and can pass advice to her playing self (game_hint).
    let game_live = !t.guarded && prepared.game_live;
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
            // Planning failed twice (JSON mode, then one repair retry): say so briefly instead of a
            // tool-less guess that may answer the wrong thing.
            json!({"action": "reply", "text": crate::planner::plan_failed_text()})
        }
    };
    let action = plan["action"]
        .as_str()
        .filter(|a| !a.is_empty())
        .unwrap_or("reply")
        .to_owned();
    // Tier 0 settles "calendar_event" without its fields ("6 november at 12:30", answering her
    // question about his meeting): the planner, which sees the recent turns, fills them in.
    // A title that is the owner's whole sentence names no event either, and is the planner's to
    // choose.
    let title_is_sentence = |p: &Value| {
        lucina_intent::calendar_event::title_is_sentence(
            p["title"].as_str().unwrap_or(""),
            &t.message,
        )
    };
    let (mut plan, action) = if action == "calendar_event"
        && (plan["start"].as_str().is_none_or(|v| v.trim().is_empty()) || title_is_sentence(&plan))
    {
        replan(t, memory, plan, action).await
    } else {
        (plan, action)
    };
    // The planner can hand back the sentence as it is: keep its first words, so a whole message
    // never becomes the name of an event.
    if action == "calendar_event" {
        let title = plan["title"].as_str().unwrap_or("").trim().to_owned();
        if !title.is_empty() && title_is_sentence(&plan) {
            plan["title"] = json!(lucina_intent::calendar_event::short_title(&title));
        }
    }

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

    if !t.may(&action) {
        tracing::warn!(session_id = %t.session, blocked = %action, "secretary mcp turn refused");
        return not_run(t, memory, &action).await;
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
    if action == "calendar_event" {
        return calendar_event_turn(t, &plan, memory).await;
    }
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
        let resolved = match resolve_reminder(&t.message, &plan, &o) {
            Some(r) => Some(r),
            // Tier 0's plan carries no fields: one planner pass (it sees the recent turns) before
            // giving up; it may also find that this is an event, or ask a question.
            None if plan["dueDate"].as_str().is_none_or(|v| v.trim().is_empty()) => {
                let (again, next) = replan(t, memory, plan.clone(), action.clone()).await;
                match next.as_str() {
                    "calendar_event" => return calendar_event_turn(t, &again, memory).await,
                    "schedule_reminder" => resolve_reminder(&t.message, &again, &o),
                    "reply" => {
                        if let Some(text) = again["text"].as_str().filter(|x| !x.trim().is_empty())
                        {
                            return Finish::reply(text.to_owned(), &action);
                        }
                        None
                    }
                    _ => None,
                }
            }
            None => None,
        };
        // Never a dead end: he gets a question he can answer, and the next turn sees this one.
        let Some((text, due)) = resolved else {
            return Finish::reply(reminder_question(&t.message), &action)
                .with("[reminder unclear: asked when]", None);
        };
        return match create_turn_reminder(t, chat, text, due).await {
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

    // Reading a public page or searching the web: the Rust Lucina has no crawler, but her computer's
    // Chromium can, so the job goes there (read-only) instead of "this app can't run it".
    if action == "web_crawl_task" && lucina_computer_use::shared_client().ready() {
        let plan = web_crawl_as_computer_task(&plan, &t.message);
        return queue_job(t, &plan, "computer_task", cmid);
    }

    if jobs::ASYNC_ACTIONS.contains(&action.as_str()) {
        return queue_job(t, &plan, &action, cmid);
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
            "{} This request needs the \"{action}\" tool, which this Lucina cannot run yet. Say so in one sentence and answer whatever you can without it. Never claim the tool ran.",
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

/// A delegation request from the owner's own turn. On an MCP turn the caller (ChatGPT, Grok, …) is
/// set, so the request never goes back to the agent that sent it.
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
        caller: t.mcp.as_ref().map(|m| m.caller.clone()),
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
    // A trusted MCP turn relays the owner, but the regexes also match an agent's report: Jev checks.
    if let Some(m) = &t.mcp {
        let (yes, p) =
            crate::jev_route::mcp_delegation(lucina_jev::shared(), &m.caller, &t.message).await;
        tracing::info!(event = "mcp_delegation", caller = %m.caller, p = ?p, delegate = yes, "mcp delegation check");
        if !yes {
            return None;
        }
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
                "Relay this answer to Giuseppe in English (Danish when he writes Danish; never Italian): 1-4 short sentences, saying it comes from {}. If it says the full answer is in Notes, say so. Do not add facts that are not in it.",
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
        "[Secretary note saved by {source}] {} ({}) {}",
        note["title"].as_str().unwrap_or(""),
        note["slug"].as_str().unwrap_or(""),
        nt::cite(note["slug"].as_str().unwrap_or(""))
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
    turn_meta: Option<Meta>,
    session: String,
    user_item: Option<String>,
    channel: String,
    /// The job keeps the surface of the turn that started it.
    surface: TurnSurface,
    origin: Option<Arc<ChannelOrigin>>,
}

/// Video report jobs a restart interrupted (crate::video::Resume) start again, reporting on
/// the app's timeline (a Telegram reply sink does not survive a restart). Once per process.
pub(crate) fn resume_video_jobs(st: &AppState) {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let cfg = &st.cfg.video;
    for mut r in crate::video::pending_resumes(cfg) {
        let job = match st.jobs.adopt(&r.job_id) {
            Ok(Some(job)) if r.attempts < crate::video::MAX_ATTEMPTS => job,
            Ok(Some(_)) => {
                tracing::warn!(job_id = %r.job_id, "youtube report: interrupted too often; not resuming");
                let _ = st.jobs.update(&r.job_id, |j| {
                    j.status = "error".into();
                    j.error = Some("interrupted by restarts".into());
                    j.finished_at = Some(now_iso());
                });
                st.jobs.finish(&r.job_id);
                crate::video::clear_resume(cfg, &r.plan);
                continue;
            }
            _ => {
                crate::video::clear_resume(cfg, &r.plan);
                continue;
            }
        };
        tracing::info!(job_id = %job.id, attempt = r.attempts + 1, "youtube report: resuming after a restart");
        r.attempts += 1;
        crate::video::save_resume(cfg, &r);
        let surface = crate::surface::chat_surface(
            &st.cfg,
            Some(BearerVerified::checked()),
            &r.channel,
            None,
        );
        spawn_job(
            st.clone(),
            job,
            r.plan.clone(),
            r.message.clone(),
            String::new(),
            JobDest {
                turn_meta: None,
                session: r.session.clone(),
                user_item: r.user_item.clone(),
                channel: r.channel.clone(),
                surface,
                origin: None,
            },
        );
    }
}

/// A finished job on the owner's devices (`/v1/notify`), keyed to its timeline item so it is
/// one notification with secretary-server's timeline ingester.
fn notify_job(
    st: &AppState,
    job_id: &str,
    title: &str,
    text: &str,
    timeline_id: Option<&str>,
    channel: &str,
) {
    if !st.cfg.app_notify {
        return;
    }
    let mut body = json!({
        "category": "lucina_message", "title": title,
        "body": text.chars().take(600).collect::<String>(),
        "route": "/", "channel": channel,
        "dedupKey": format!("lucina:job:{job_id}"),
    });
    if let Some(id) = timeline_id {
        body["timelineId"] = json!(id);
    }
    let secretary = st.secretary.clone();
    tokio::spawn(async move {
        let r = crate::bridges::notify(&secretary, &body).await;
        if !r.ok {
            tracing::warn!(status = r.status, "job notification failed");
        }
    });
}

/// `startJob`: run the action, report on the timeline and the job record
/// (and, from a chat channel, in the chat: it has no polling client).
fn spawn_job(st: AppState, job: Job, plan: Value, message: String, memory: String, dest: JobDest) {
    tokio::spawn(async move {
        let JobDest {
            turn_meta,
            session,
            user_item,
            channel,
            surface,
            origin,
        } = dest;
        // An agent's job: the agent hears about it, the owner's phone does not ring.
        let from_agent = turn_meta.as_ref().is_some_and(|m| m.via.is_some());
        let timeline = origin
            .as_ref()
            .map_or_else(|| st.timeline_for(&session), |o| o.timeline.clone());
        let record = |text: String, rich: Option<&lucina_store::rich::RichMessage>| {
            let meta = Some(Meta {
                job_id: Some(job.id.clone()),
                ..Meta::default()
            });
            let meta = match turn_meta.clone() {
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
            device: "",
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
                "computer_task" => {
                    run_computer_task(plan["task"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(&message)).await
                }
                "agent_task" => crate::act::job(&st, &t, &plan, &job.id).await,
                crate::video::ACTION => {
                    let cfg = &st.cfg.video;
                    crate::video::save_resume(cfg, &crate::video::Resume {
                        job_id: job.id.clone(),
                        plan: plan.clone(),
                        message: message.clone(),
                        session: session.clone(),
                        channel: channel.clone(),
                        user_item: user_item.clone(),
                        attempts: 1,
                    });
                    let out = crate::video::run(&st, &plan, &job.id).await;
                    crate::video::clear_resume(cfg, &plan);
                    out
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
                        rich_reply(&st, &result, &message, &action, "", false, &[], None, false)
                            .await;
                    (rich_msg, text)
                };
                let item = record(text, rich_msg.as_ref());
                if action == crate::video::ACTION && !from_agent {
                    notify_job(
                        &st,
                        &job.id,
                        "Video report ready",
                        &result,
                        item.as_ref().map(|i| i.id.as_str()),
                        &channel,
                    );
                }
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
                if action == crate::video::ACTION && !from_agent {
                    notify_job(
                        &st,
                        &job.id,
                        "Video report failed",
                        &text,
                        item.as_ref().map(|i| i.id.as_str()),
                        &channel,
                    );
                }
                let _ = st.jobs.update(&job.id, |j| {
                    j.status = "error".into();
                    j.error = Some(reason.clone());
                    j.timeline_item_id = item.map(|i| i.id);
                    j.finished_at = Some(now_iso());
                });
                // The asking conversation hears about the failure too (an MCP caller's next turn,
                // the app's chat), not only the timeline.
                recent.append(recent_key, "assistant", &text, Utc::now());
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
    if path == "/instagram" || path.starts_with("/instagram/") {
        return json!({"id": "instagram", "label": "Open Instagram", "emoji": "📷", "kind": "route", "payload": {"path": path}});
    }
    if path == "/pixiv" || path.starts_with("/pixiv/") {
        return json!({"id": "pixiv", "label": "Open Pixiv", "emoji": "🎨", "kind": "route", "payload": {"path": path}});
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

/// The turn's own blocks first, then the composer's (within the format's limits).
fn with_blocks(
    msg: Option<lucina_store::rich::RichMessage>,
    blocks: &[Value],
) -> Option<lucina_store::rich::RichMessage> {
    let mut msg = msg?;
    if !blocks.is_empty() {
        msg.blocks = lucina_store::rich::merge_blocks(blocks, std::mem::take(&mut msg.blocks));
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
    f.blocks = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Image { src, alt, caption } => Some(json!({
                "type": "image", "src": src, "alt": alt, "caption": caption, "width": 800, "height": 600,
            })),
            _ => None,
        })
        .collect();
    if effects.iter().any(|e| matches!(e, Effect::PrivateTurn)) {
        // Instagram / Pixiv: never remembered (see `remember_turn`).
        f.action = PRIVATE_ACTION.into();
    }
    f.calendar_events = effects
        .iter()
        .flat_map(|e| match e {
            Effect::CalendarEvents { events } => events.as_slice(),
            _ => &[],
        })
        .cloned()
        .collect();
    if effects
        .iter()
        .any(|e| matches!(e, Effect::CalendarCreated { .. }))
    {
        f.routes.push(crate::bridges::undo_action(lang));
    }
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
    // A note she saved, read or changed with the notes tool: the item carries its slug, like the
    // notes bridge's (`meta.kind = "note"`), so the apps can link it and refresh Notes.
    if let Some(slug) = effects.iter().rev().find_map(|e| match e {
        Effect::Note {
            slug: Some(slug), ..
        } => Some(slug.clone()),
        _ => None,
    }) {
        if f.meta.is_none() {
            f.meta = Some(Meta {
                slug: Some(slug),
                ..Meta::kind("note")
            });
        }
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

/// A sub-turn's tool-trace line: the turn's agent mark plus the tool. An agent's turn alerts
/// nobody, its trace lines included (secretary-server `notify::ingest`; 2026-11-03 the photo
/// searches of an agent's question pushed 19 "🔧 media search" lines to his phone).
fn tool_meta(mark: &Meta, r: &ToolRecord) -> Meta {
    mark.clone().merge(Meta {
        tool: Some(r.name.clone()),
        ok: Some(r.ok),
        ..Meta::kind("tool")
    })
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
    device_meta: Meta,
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
        crate::share::guidance::VOICE_SYSTEM.to_owned()
    } else {
        String::new()
    };
    let voice_system = if crate::voice::spoken() {
        format!("{voice_system} {}", crate::voice::SPOKEN_RULE)
    } else {
        voice_system
    };
    let voice_system = if channel == VOICE_CHANNEL {
        format!("{voice_system} {}", crate::prompts::VOICE_SURFACE_RULE)
    } else {
        voice_system
    };
    // The device the message came from, as a fact of the turn (no message text is matched).
    let voice_system = format!("{voice_system} {}", juno_fact(&st, &session, mcp));
    let voice_system = format!(
        "{voice_system} {}",
        crate::device::system_fact(&channel, mcp)
    );
    let turn = TimelineTurn::new(st.timeline_for(&session), &channel, cmid.clone())
        .with_meta(Some(device_meta))
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
    let secrets = secrets_finish(
        &st,
        secrets_text.as_deref().unwrap_or(&message),
        from_mcp,
        from_mcp.then(|| surface.caller().unwrap_or("mcp")),
    )
    .await;
    let (memory_context, game_system) = if secrets.is_some() {
        (String::new(), String::new())
    } else {
        let p = crate::context::prepare_keyed(
            &st,
            &surface,
            &session,
            crate::context::Keys {
                recent: &st.recent,
                recent_key: &session,
                peer: &st.cfg.owner_peer_id,
                voice: channel == VOICE_CHANNEL,
                intent_text: None,
                device: "",
            },
            &message,
            USER_LABEL,
            PLANNER_PLATFORM,
            // `tools_mode`: the classifier is not asked, so no turn of this path starts anything
            // through it.
            None,
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
    // The owner's own message from the app: calendar creates go straight to Google.
    let ctx = if from_mcp { ctx } else { ctx.owner_turn() };
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
        // Owner turns only: Jev's first-tool hint (TypeSafe; off without JEV_API_KEY).
        router: if mcp {
            None
        } else {
            crate::jev_route::tool_router()
        },
        route_text: Some(message.clone()),
        ..AgentConfig::default()
    };
    let t = TurnInput {
        user_text: &message,
        memory: &memory_context,
        user_label: USER_LABEL,
        platform: PLATFORM_LABEL,
        device: "",
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
    // Her own computer ("explore Outlook in your sandbox"): before every other route, like the JSON turn.
    let computer = if secrets.is_none() {
        lucina_intent::computer_route::plan(&message)
    } else {
        None
    };
    let delegation = match (&st.delegator, &secrets, &computer) {
        (Some(d), None, None) => delegation_settings(d, &message, mcp).await,
        _ => None,
    };
    // Calendar questions ("cosa ho domani?", "dove si tiene la Hack Night?") are answered from
    // the calendar like on the JSON planner, never by the model from memory or notes.
    let calendar = if secrets.is_none()
        && delegation.is_none()
        && computer.is_none()
        && settled.is_none()
        && st.cfg.calendar_enabled
    {
        crate::bridges::calendar_read(&st.secretary, &message, now, tz).await
    } else {
        None
    };
    let finish = if let Some(f) = secrets {
        f
    } else if let Some(plan) = computer {
        // No job queue in tools mode: her ack, then the result, in one reply.
        if mcp {
            Finish::refusal(&persona, "computer_task")
        } else {
            let ack = plan["ack"].as_str().unwrap_or("").trim().to_owned();
            let task = plan["task"].as_str().unwrap_or(&message).to_owned();
            let (text, result) = match run_computer_task(&task).await {
                Ok(ok) => ok,
                Err(e) => (e, "[computer failed]".into()),
            };
            let text = if ack.is_empty() {
                text
            } else {
                format!("{ack}\n\n{text}")
            };
            Finish::reply(text, "computer").with(result, Some(Meta::kind("computer")))
        }
    } else if let Some(f) = delegation {
        f
    } else if let Some(o) = calendar {
        calendar_finish(o)
    } else if let Some(result) = settled {
        let reply = st.planner.generate_skill_reply(&t, "pending action", &result, "Tell the owner what happened in one or two sentences, in English (Danish when he writes Danish; never Italian).").await.unwrap_or_else(|_| fallback());
        effects_to_finish(&ctx.effects(), reply)
    } else {
        let extra = format!(
            "{SECRETARY_EXTRA_SYSTEM} {voice_system} {pending_note} {}",
            crate::capabilities::SUBLOOP_SCOPE_RULE
        );
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
                        &format!(
                            "{SECRETARY_EXTRA_SYSTEM} {voice_system} {}",
                            crate::capabilities::SUBLOOP_SCOPE_RULE
                        ),
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
    // LucinaVoice: settled before rich-reply (see run_turn_scoped).
    crate::voice::settle(&reply, false);
    let (rich_msg, reply, remembered) = rich_reply(
        &st,
        &reply,
        &message,
        &finish.action,
        &finish.action_result,
        // Secrets replies are fixed texts, AU Find, caller and Jev yes/no answers keep their wording: no model rewrite.
        !finish.action.starts_with("secrets_")
            && finish.action != au_turn::ACTION
            && finish.action != callers_turn::ACTION
            && finish.action != crate::jev_yesno::ACTION,
        &finish.calendar_events,
        finish.agenda.as_ref(),
        mcp,
    )
    .await;
    // The tools' own blocks and buttons (a Juno screenshot, an AU map), as on the JSON planner: they
    // need a rich message even when the composer gave none.
    let rich_msg = match rich_msg {
        None if !finish.blocks.is_empty() => Some(lucina_store::rich::RichMessage {
            format: "v1".into(),
            bubbles: vec![reply.clone()],
            lang: Some(crate::rich::guess_language(&reply, &message).to_owned()),
            blocks: Vec::new(),
            actions: Vec::new(),
        }),
        m => m,
    };
    let rich_msg = with_blocks(with_routes(rich_msg, &finish.routes), &finish.blocks);
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
            allow: Vec::new(),
            conversation: None,
        }),
        surface: surface.clone(),
        guarded: false,
        consult: false,
        user_label: USER_LABEL.into(),
        extra_system: String::new(),
        share: false,
        share_note: String::new(),
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
    fn an_agents_tool_lines_carry_its_mark() {
        let r = ToolRecord {
            call_id: "c1".into(),
            name: "media".into(),
            args: json!({}),
            ok: true,
            output: json!({}),
            summary: "media search Fortuna".into(),
            pending: None,
            ms: 1,
        };
        let mark = Meta {
            via: Some("mcp".into()),
            caller: Some("some-new-provider".into()),
            conversation: Some("lastphoto".into()),
            ..Meta::default()
        };
        let m = tool_meta(&mark, &r);
        assert_eq!(
            (
                m.kind.as_deref(),
                m.tool.as_deref(),
                m.ok,
                m.via.as_deref(),
                m.caller.as_deref(),
                m.conversation.as_deref()
            ),
            (
                Some("tool"),
                Some("media"),
                Some(true),
                Some("mcp"),
                Some("some-new-provider"),
                Some("lastphoto")
            )
        );
        // His own turn: no mark.
        let m = tool_meta(&Meta::default(), &r);
        assert!(m.via.is_none() && m.caller.is_none());
        assert!(crate::secret_keep::agent_session(
            "secretary_mcp-claude-lastphoto"
        ));
        assert!(crate::secret_keep::agent_session("mcp-some-new-provider"));
        assert!(!crate::secret_keep::agent_session("secretary_ios-1"));
    }

    #[test]
    fn a_page_read_runs_read_only_on_her_computer() {
        let p = web_crawl_as_computer_task(
            &json!({"action": "web_crawl_task", "url": "https://example.com", "query": "the page title"}),
            "Read https://example.com and tell me the page title.",
        );
        assert_eq!(p["action"], "computer_task");
        let task = p["task"].as_str().unwrap();
        assert!(task.starts_with(
            "Open https://example.com in a new tab and read it. Goal: the page title."
        ));
        assert!(task.contains("Read only"));
        assert_eq!(
            p["ack"],
            "Ok, I'm opening it in my computer's browser and will report back."
        );
        let p = web_crawl_as_computer_task(
            &json!({"action": "web_crawl_task", "url": "", "ack": "Cerco subito."}),
            "Search online: when does daylight saving time start in Denmark in 2027?",
        );
        assert!(p["task"].as_str().unwrap().starts_with(
            "Search the web for this in a new tab (Google): Search online: when does daylight saving time start in Denmark in 2027?"
        ));
        assert_eq!(p["ack"], "Cerco subito.");
    }

    #[tokio::test]
    async fn a_failed_plan_is_said_in_the_turn_language() {
        let da = crate::planner::TURN_LANG
            .scope(
                lucina_intent::lang::language_line(lucina_intent::lang::Lang::Da),
                async { crate::planner::plan_failed_text() },
            )
            .await;
        assert!(da.starts_with("Undskyld"), "{da}");
        // An Italian message is answered in English.
        let it = crate::planner::TURN_LANG
            .scope(
                lucina_intent::lang::language_line(lucina_intent::lang::Lang::It),
                async { crate::planner::plan_failed_text() },
            )
            .await;
        assert!(it.starts_with("Sorry"), "{it}");
        assert!(crate::planner::plan_failed_text().starts_with("Sorry"));
    }

    #[test]
    fn plugin_turns_are_private_and_get_their_page_button() {
        let f = effects_to_finish(
            &[
                Effect::PrivateTurn,
                Effect::Gallery {
                    path: "/pixiv".into(),
                    count: 3,
                },
            ],
            "ok".into(),
        );
        assert_eq!(f.action, PRIVATE_ACTION);
        assert_eq!(f.routes[0]["label"], "Open Pixiv");
        assert_eq!(f.routes[0]["payload"]["path"], "/pixiv");
        assert_eq!(
            gallery_action("/instagram", 2, "en")["label"],
            "Open Instagram"
        );
        // an ordinary reply is not private
        assert_eq!(effects_to_finish(&[], "ok".into()).action, "reply");
    }

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
        assert_eq!(f.routes[0]["label"], "Open in Drive");
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

    #[test]
    fn a_note_she_saved_or_changed_rides_on_the_reply_meta() {
        let f = effects_to_finish(
            &[Effect::Note {
                title: "Shopping".into(),
                slug: Some("shopping".into()),
            }],
            "Fatto.".into(),
        );
        assert_eq!(f.action, "reply");
        assert_eq!(
            serde_json::to_value(f.meta.unwrap()).unwrap(),
            json!({"kind": "note", "slug": "shopping"})
        );
        // A local-file note has no slug on the server: no link.
        let local = effects_to_finish(
            &[Effect::Note {
                title: "Offline".into(),
                slug: None,
            }],
            "Fatto.".into(),
        );
        assert!(local.meta.is_none());
        // Another meta (a shopping list) wins.
        let both = effects_to_finish(
            &[
                Effect::Note {
                    title: "Shopping".into(),
                    slug: Some("shopping".into()),
                },
                Effect::Shopping {
                    list_id: "l1".into(),
                },
            ],
            "Ok".into(),
        );
        assert_eq!(both.meta.unwrap().kind.as_deref(), Some("shopping"));
    }
}
