//! `POST /api/secretary/chat`: one Lucina text turn.
//!
//! Pipeline: timeline (owner item) -> pending-confirmation check -> context
//! (turn metadata, recent turns, delivered reminders, Honcho) -> planner
//! (`LUCINA_PLANNER=json`: Node's JSON-action planner; `tools`: the native
//! tool loop) -> actions -> timeline (tool items, reply) -> Honcho remember
//! (readwrite only).

use std::time::{Duration, Instant};

use chrono::Utc;
use futures_util::FutureExt;
use lucina_llm::Message;
use lucina_memory::{secretary_session_id, MemoryMode};
use lucina_store::parser::{parse_at_time, parse_date_part, parse_reminder_message, ParseOptions};
use lucina_store::reminders::{
    delivered_context, format_confirmation, format_list, CreateReminder, Reminder,
};
use lucina_store::timeline::{Meta, TimelineTurn};
use lucina_store::timeutil::{iso_ms, utf16_len};
use lucina_tools::agent::{self, AgentConfig, ToolRecord};
use lucina_tools::pending::{classify_reply, Reply};
use lucina_tools::{Ctx, Effect, Registry, ToolSelection};
use serde_json::{json, Value};

use crate::api::{optional_string, owner_reminders, ApiError, PLATFORM_LABEL, USER_LABEL};
use crate::planner::TurnInput;
use crate::prompts::Prompts;
use crate::{AppState, PlannerMode};

const MAX_MESSAGE_CHARS: usize = 4000;
const CLIENT_MESSAGE_TTL: Duration = Duration::from_secs(600);
const CLIENT_MESSAGE_KEEP: usize = 200;
const PLANNER_PLATFORM: &str = "secretary";
const SECRETARY_EXTRA_SYSTEM: &str = "This conversation comes from the Secretary app on Giuseppe's iPhone. Reply in the language of the message. Keep it short: usually 1-3 sentences. Plain text only: no markdown tables or headings.";
const ASYNC_ACTIONS: [&str; 6] = [
    "cursor_task",
    "oracle_monitor_task",
    "spreadsheet_update_task",
    "graph_task",
    "generate_image",
    "social_post_task",
];

fn async_label(action: &str) -> &'static str {
    match action {
        "cursor_task" => "Harness task",
        "oracle_monitor_task" => "Oracle monitor check",
        "spreadsheet_update_task" => "spreadsheet update",
        "graph_task" => "graph",
        "generate_image" => "image generation",
        "social_post_task" => "social post",
        _ => "task",
    }
}

/// Validate the body, dedupe `clientMessageId` retries, run the turn.
pub async fn handle_chat(st: AppState, body: Value) -> Result<Value, ApiError> {
    let message = match &body["message"] {
        Value::String(s) if !s.trim().is_empty() => lucina_store::timeutil::js_trim(s).to_owned(),
        _ => return Err(ApiError::bad_request("message is required")),
    };
    if utf16_len(&message) > MAX_MESSAGE_CHARS {
        return Err(ApiError::bad_request(format!(
            "message must be at most {MAX_MESSAGE_CHARS} characters"
        )));
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
    let session = secretary_session_id(&requested, &st.cfg.default_session);
    tracing::info!(session_id = %session, %channel, chars = message.chars().count(), has_client_message_id = !client_message_id.is_empty(), "secretary chat received");
    let cmid = (!client_message_id.is_empty()).then_some(client_message_id);
    let Some(cmid_value) = cmid.clone() else {
        return run_turn(st, message, session, None, channel).await;
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
                let fut = run_turn(st.clone(), message, session, cmid, channel)
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

struct Finish {
    reply: String,
    action: String,
    extra: serde_json::Map<String, Value>,
    meta: Option<Meta>,
    action_result: String,
}

impl Finish {
    fn reply(reply: impl Into<String>, action: &str) -> Self {
        Self {
            reply: reply.into(),
            action: action.into(),
            extra: serde_json::Map::new(),
            meta: None,
            action_result: String::new(),
        }
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

fn effects_to_finish(effects: &[Effect], reply: String) -> Finish {
    let mut f = Finish::reply(reply, "reply");
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
    f
}

fn tool_item_text(r: &ToolRecord) -> String {
    format!("🔧 {} → {}", r.summary, if r.ok { "ok" } else { "error" })
}

async fn run_turn(
    st: AppState,
    message: String,
    session: String,
    cmid: Option<String>,
    channel: String,
) -> Result<Value, ApiError> {
    let now = Utc::now();
    let tz = st.cfg.time_zone;
    let persona = st.cfg.persona.clone();
    let turn = TimelineTurn::new(st.timeline.clone(), &channel, cmid.clone());
    let user_item = turn.record_user(&message, Vec::new());

    // Context: turn metadata, recent turns, delivered reminders, Honcho.
    let owner = owner_reminders(&st).unwrap_or_default();
    let delivered = st
        .cfg
        .owner_chat_id
        .as_deref()
        .map(|c| delivered_context(&owner, c, tz, now))
        .unwrap_or_default();
    let honcho = tokio::time::timeout(
        Duration::from_secs(12),
        st.memory
            .build_context(&st.cfg.owner_peer_id, &session, &message),
    )
    .await
    .unwrap_or_default();
    let presence = if st.cfg.presence_context {
        lucina_discord::store::context_line(&st.cfg.presence_dir, tz, now, st.cfg.presence_max_age)
            .unwrap_or_default()
    } else {
        String::new()
    };
    let memory_context = [
        st.recent.metadata_block(&session, tz, now),
        presence,
        st.recent.render(&session, tz),
        delivered,
        honcho,
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    st.recent.append(&session, "user", &message, now);

    let ctx = Ctx::new(st.tool_env.clone(), &channel, &session);
    let on_tool = |r: &ToolRecord| {
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

    // A pending confirmation from an earlier turn of this session.
    let mut pending_note = String::new();
    let mut settled: Option<String> = None;
    if let Some(p) = st.pending.current(&session, now) {
        match classify_reply(&message) {
            Reply::Yes => {
                if let Some(action) = st.pending.take(&p.id) {
                    let (ok, output, summary) = agent::execute(
                        &st.registry,
                        &action.tool,
                        action.args.clone(),
                        &ctx,
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
            Reply::No => {
                st.pending.take(&p.id);
                settled = Some(format!("The owner declined the pending action \"{}\". It was not run and is discarded.", p.summary));
            }
            Reply::Other => {
                pending_note = format!(
                    "A pending action awaits the owner's explicit confirmation: \"{}\" (expires {}). If this message does not answer it, handle the message normally and mention in one short sentence that the action still needs an explicit yes.",
                    p.summary, p.expires_at
                );
            }
        }
    }

    let fallback = || {
        format!("{persona}: I misread that turn and could not recover it. Please send the message once more.")
    };
    let finish = if let Some(result) = settled {
        let reply = st
            .planner
            .generate_skill_reply(&t, "pending action", &result, "Tell the owner what happened in one or two sentences, in the language of the message.")
            .await
            .unwrap_or_else(|_| fallback());
        effects_to_finish(&ctx.effects(), reply)
    } else {
        match st.cfg.planner {
            PlannerMode::Tools => {
                let extra = format!("{SECRETARY_EXTRA_SYSTEM} {pending_note}");
                let system = st.planner.prompts.agent_system(
                    PLATFORM_LABEL,
                    &message,
                    &memory_context,
                    extra.trim(),
                    now,
                    tz,
                );
                let msgs = vec![
                    Message::system(system),
                    Message::user(Prompts::user_block(USER_LABEL, &memory_context, &message)),
                ];
                match agent::run(
                    st.model.as_ref(),
                    &st.registry,
                    msgs,
                    &ctx,
                    &st.pending,
                    &agent_cfg,
                    &on_tool,
                )
                .await
                {
                    Ok(out) if !out.text.is_empty() => effects_to_finish(&ctx.effects(), out.text),
                    Ok(_) | Err(_) => {
                        let reply = st
                            .planner
                            .generate_reply(&t, SECRETARY_EXTRA_SYSTEM, "", "reply")
                            .await
                            .unwrap_or_else(|_| fallback());
                        effects_to_finish(&ctx.effects(), reply)
                    }
                }
            }
            PlannerMode::Json => json_turn(
                &st,
                &t,
                &ctx,
                &agent_cfg,
                &on_tool,
                &message,
                &pending_note,
                &owner,
            )
            .await
            .unwrap_or_else(|_| Finish::reply(fallback(), "reply")),
        }
    };

    // Finish: recent turns, memory, the Lucina item.
    let reply = if finish.reply.trim().is_empty() {
        "I could not generate a reply.".to_owned()
    } else {
        finish.reply.trim().to_owned()
    };
    st.recent.append(&session, "assistant", &reply, Utc::now());
    if st.memory.mode() == MemoryMode::ReadWrite {
        let (mem, peer, sid) = (
            st.memory.clone(),
            st.cfg.owner_peer_id.clone(),
            session.clone(),
        );
        let user_text = format!("[{PLATFORM_LABEL}] {message}");
        let assistant = [
            format!("[action:{}]", finish.action),
            reply.clone(),
            if finish.action_result.is_empty() {
                String::new()
            } else {
                format!(
                    "[result]\n{}",
                    finish.action_result.chars().take(3000).collect::<String>()
                )
            },
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
        tokio::spawn(async move {
            if let Err(e) = mem.remember(&peer, &sid, &user_text, &assistant).await {
                tracing::warn!(error = %e, "secretary honcho remember failed");
            }
        });
    }
    let lucina_item = turn.record_lucina(&reply, Vec::new(), finish.meta.clone());
    let mut out = json!({
        "reply": reply,
        "action": finish.action,
        "job": null,
        "sessionId": session,
        "clientMessageId": cmid,
        "userItemId": user_item.map(|i| i.id),
        "lucinaItemId": lucina_item.map(|i| i.id),
    });
    for (k, v) in finish.extra {
        out[k] = v;
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
async fn json_turn(
    st: &AppState,
    t: &TurnInput<'_>,
    ctx: &Ctx,
    agent_cfg: &AgentConfig,
    on_tool: &(dyn Fn(&ToolRecord) + Send + Sync),
    message: &str,
    pending_note: &str,
    owner: &[Reminder],
) -> Result<Finish, lucina_llm::LlmError> {
    let tz = st.cfg.time_zone;
    let persona = &st.cfg.persona;
    let extra = format!("{SECRETARY_EXTRA_SYSTEM} {pending_note}")
        .trim()
        .to_owned();
    let planner_input = TurnInput {
        platform: PLANNER_PLATFORM,
        ..*t
    };
    let (plan, class) = match st.planner.route(&planner_input, &extra).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(surface = "secretary", error = %e, "response planning failed");
            let text = st.planner.generate_reply(t, "", "", "reply").await?;
            (
                json!({"action": "reply", "text": text}),
                crate::planner::Classification {
                    intent: "unknown".into(),
                    confidence: 0.0,
                    needs_tools: false,
                    reply_mode: "full_plan".into(),
                    scope: "all".into(),
                },
            )
        }
    };
    let action = plan["action"].as_str().unwrap_or("reply").to_owned();
    match action.as_str() {
        "grocery" => {
            if st.grocery.is_none() {
                let reply = st.planner.generate_reply(t, &format!("{extra} The shopping list is not available on this host right now; say so in one sentence."), "", "reply").await?;
                return Ok(Finish::reply(reply, "reply"));
            }
            let registry = Registry::builtin(&ToolSelection::Only(
                ["shopping".to_owned()].into_iter().collect(),
            ));
            let system = st.planner.prompts.agent_system(
                PLATFORM_LABEL,
                message,
                t.memory,
                &extra,
                Utc::now(),
                tz,
            );
            let msgs = vec![
                Message::system(system),
                Message::user(Prompts::user_block(USER_LABEL, t.memory, message)),
            ];
            match agent::run(st.model.as_ref(), &registry, msgs, ctx, &st.pending, agent_cfg, on_tool).await {
                Ok(out) if !out.text.is_empty() => {
                    let mut f = effects_to_finish(&ctx.effects(), out.text);
                    f.action = "grocery".into();
                    f.action_result = class.intent;
                    Ok(f)
                }
                _ => Ok(Finish::reply("I could not update grocery accounting right now. Your purchase was not recorded.", "grocery")),
            }
        }
        "list_reminders" if st.cfg.reminders_enabled => {
            let today = plan["scope"] == "today"
                || (plan["scope"].is_null()
                    && regex::Regex::new(r"(?i)\b(oggi|today|stamattina|stasera)\b")
                        .map(|r| r.is_match(message))
                        .unwrap_or(false));
            Ok(Finish::reply(
                format_list(owner, tz, persona, today, Utc::now()),
                "list_reminders",
            ))
        }
        "schedule_reminder" => {
            if !st.cfg.reminders_enabled {
                return Ok(Finish::reply(
                    format!("{persona}: timed reminders are disabled on this host."),
                    &action,
                ));
            }
            let Some(chat) = st.cfg.owner_chat_id.clone().filter(|c| !c.is_empty()) else {
                return Ok(Finish::reply(format!("{persona}: no owner Telegram chat is configured, so I cannot schedule reminders."), &action));
            };
            let o = ParseOptions {
                tz,
                default_hour: st.cfg.reminder_default_hour,
                default_minute: st.cfg.reminder_default_minute,
                now: Utc::now(),
            };
            let Some((text, due)) = resolve_reminder(message, &plan, &o) else {
                return Ok(Finish::reply(
                    format!("{persona}: I could not parse that reminder. Examples: \"alle 21:30 di fare la doccia\" or \"28 August 2026 — unsubscribe from Google AI Plus\"."),
                    &action,
                ));
            };
            match st
                .writer
                .create(CreateReminder {
                    text,
                    due_at: due,
                    chat_id: chat,
                    user_peer_id: st.cfg.owner_peer_id.clone(),
                    user_label: USER_LABEL.into(),
                    persona: persona.clone(),
                    time_zone: tz.name().into(),
                    dedupe_key: None,
                    source: "secretary".into(),
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
                    Ok(f)
                }
                Err(e) => Ok(Finish::reply(
                    format!(
                        "{persona}: I could not schedule that reminder ({}).",
                        e.code
                    ),
                    &action,
                )),
            }
        }
        a if ASYNC_ACTIONS.contains(&a) => Ok(Finish::reply(
            format!(
                "The {} cannot start from this Lucina host yet: ask me on Telegram for it.",
                async_label(a)
            ),
            a,
        )),
        "reply" => {
            let text = match plan["text"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(t) => t.to_owned(),
                None => {
                    st.planner
                        .generate_reply(t, SECRETARY_EXTRA_SYSTEM, "", "reply")
                        .await?
                }
            };
            Ok(Finish::reply(text, "reply"))
        }
        other => {
            let extra = format!(
                "{SECRETARY_EXTRA_SYSTEM} This request needs the \"{other}\" tool, which the Secretary app cannot run. Say so in one sentence, answer whatever you can without it, and suggest asking on Telegram for the tool. Never claim the tool ran."
            );
            let text = st
                .planner
                .generate_reply(t, &extra, "", "reply")
                .await
                .unwrap_or_else(|_| {
                    "I cannot do that from the app yet. Ask me on Telegram.".into()
                });
            Ok(Finish::reply(text, "reply"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
