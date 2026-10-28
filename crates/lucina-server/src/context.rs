//! A text turn's context, as the Node Lucina builds it
//! (`prepareLucinaTurnContext` + `assembleLucinaTurnContext`):
//!
//! turn metadata → intent (tier 0, else Jev or the DeepSeek classifier on the
//! recent turns, see [`crate::jev_route`]) → context policy → gated sources, joined in Node's order:
//! metadata, Discord presence, calendar, named notes, recent turns,
//! delivered reminders, Honcho (only for policies that allow it, clipped to
//! the policy budget).
//!
//! Every owner block (presence, calendar, notes, tasks, shopping, recent
//! turns, delivered reminders, Honcho) is the owner's personal memory: a turn
//! whose surface may not read it gets only the metadata and the game block
//! (`docs/lucina-rust/MEMORY-SURFACES.md`).

use chrono::Utc;
use lucina_intent::policy::{self, Policy};
use lucina_intent::presence;
use lucina_intent::tier0::{self, Classification};
use lucina_memory::{Scope, TurnSurface};
use lucina_store::reminders::delivered_context;

use crate::planner::TurnInput;
use crate::AppState;

pub struct Prepared {
    pub memory_context: String,
    /// The rendered recent turns (also the grocery store context).
    pub recent_context: String,
    pub classification: Classification,
    pub policy: Policy,
    /// The live game block ("" when the game server is off). Not in `memory_context`: it goes into the system
    /// part of every model call of the turn (`planner::TURN_SYSTEM`), so trimming never drops it; owner turns
    /// also take the full planner so the model can choose game_hint.
    pub game: String,
    /// The run is playing right now (`GameContext::running`): only then do owner turns skip the cheap
    /// router and take the full planner with the game rules. A paused run keeps `game` as information.
    pub game_live: bool,
}

/// Node's `clipRecentTurns`: keep the header and the newest turns.
fn clip_recent(recent: &str, budget: usize) -> String {
    if recent.chars().count() <= budget {
        return recent.to_owned();
    }
    let header_end = recent.find('\n').map(|i| i + 1).unwrap_or(0);
    let header = &recent[..header_end];
    let body: Vec<char> = recent[header_end..].chars().collect();
    let body_budget = budget.saturating_sub(header.chars().count() + 30);
    let tail: String = body[body.len().saturating_sub(body_budget)..]
        .iter()
        .collect();
    format!("{header}\u{2026}{tail}\n[recent turns clipped to budget]")
}

/// The shopping list for a compound question that asks about it ("" otherwise
/// or when the grocery database is off or failing).
pub async fn shopping_block(st: &AppState, prompt: &str, now: chrono::DateTime<Utc>) -> String {
    let Some(repo) = st.grocery.as_deref() else {
        return String::new();
    };
    let domains = lucina_intent::domains::read_domains(prompt, now, st.cfg.time_zone, false);
    if domains.len() < 2 || !domains.contains(&lucina_intent::domains::SHOPPING) {
        return String::new();
    }
    match repo.legacy_list(&st.cfg.grocery_user_id).await {
        Ok(items) => lucina_intent::domains::shopping_list_block(&items),
        Err(e) => {
            tracing::warn!(error = %e, "compound shopping context failed");
            String::new()
        }
    }
}

/// The Discord presence block for an owner turn ("" when off or unknown).
pub fn presence_block(st: &AppState, prompt: &str) -> String {
    if !st.cfg.presence_context {
        return String::new();
    }
    let now = Utc::now();
    let tz = st.cfg.time_zone;
    let question = presence::is_presence_question(prompt);
    match lucina_discord::store::context_line(
        &st.cfg.presence_dir,
        tz,
        now,
        st.cfg.presence_max_age,
    ) {
        None if question => presence::UNKNOWN_LINE.to_owned(),
        None => String::new(),
        Some(line) if !question => line,
        Some(line) => {
            let r = lucina_discord::PresenceReport::build(
                &st.cfg.presence_dir,
                tz,
                now,
                st.cfg.presence_max_age,
            );
            format!(
                "{line}\n{}",
                presence::online_today_line(r.today_online_minutes)
            )
        }
    }
}

/// Classify a turn: tier 0, else the model (tier 1) on the recent turns.
/// `mcp`: the turn comes from a Lucina MCP client (an agent's or assistant app's text, not the owner's).
pub async fn classify(
    st: &AppState,
    user_text: &str,
    recent: &str,
    user_label: &str,
    platform: &str,
    mcp: bool,
) -> Classification {
    let s = &st.cfg.intent;
    let t = TurnInput {
        user_text,
        memory: recent,
        user_label,
        platform,
    };
    if let Some(c) = tier0::classify_tier0(user_text, recent, s) {
        // A tier-0 regex rule gets Jev's second opinion; only a sure
        // disagreement wins, and never into a side-effect intent.
        if crate::jev_route::checks_tier0(&c) {
            if let Some((v, confidence)) =
                crate::jev_route::classify_intent(lucina_jev::shared(), &t).await
            {
                let jev_intent = v["intent"].as_str().unwrap_or("");
                // Agents write technical prose that regexes over-match ("restart cloudflared at 17:16"):
                // on an MCP turn, any disagreement with a side-effect rule makes it plain chat.
                if mcp && crate::jev_route::mcp_falls_back_to_chat(&c, jev_intent) {
                    tracing::info!(target: "lucina_jev::route", tier0 = %c.intent, jev = jev_intent,
                        confidence, "jev disagrees with a side-effect tier-0 rule on an mcp turn; plain chat");
                    return tier0::from_model_json(
                        &serde_json::json!({"intent": "chat", "confidence": confidence,
                            "reply_mode": "chat", "needs_memory": true, "needs_tools": false}),
                        user_text, recent, s);
                }
                if crate::jev_route::overrides(&c.intent, jev_intent, confidence) {
                    tracing::info!(target: "lucina_jev::route", tier0 = %c.intent, jev = jev_intent,
                        confidence, "jev overrides a tier-0 rule");
                    return tier0::from_model_json(&v, user_text, recent, s);
                }
                if crate::jev_route::doubts(&c, jev_intent, confidence) {
                    tracing::info!(target: "lucina_jev::route", tier0 = %c.intent, jev = jev_intent,
                        confidence, "jev doubts a tier-0 rule; the planner decides");
                    let mut v = v;
                    v["reply_mode"] = serde_json::json!("full_plan");
                    return tier0::from_model_json(&v, user_text, recent, s);
                }
            }
        }
        return c;
    }
    // Jev first (TypeSafe's decision model: fast, typed, no JSON to parse); a
    // sure answer is final, an unsure one is checked by DeepSeek, and a failed
    // call (quota, timeout, breaker) leaves the turn to DeepSeek alone.
    let jev = crate::jev_route::classify_intent(lucina_jev::shared(), &t).await;
    if let Some((v, confidence)) = &jev {
        if *confidence >= crate::jev_route::intent_min() {
            return tier0::from_model_json(v, user_text, recent, s);
        }
    }
    match st.planner.classify_json(&t).await {
        Ok(Some(v)) => tier0::from_model_json(&v, user_text, recent, s),
        Ok(None) | Err(_) if jev.is_some() => {
            let (v, _) = jev.unwrap();
            tier0::from_model_json(&v, user_text, recent, s)
        }
        Ok(None) => Classification::unparsed(),
        Err(e) => {
            tracing::warn!(error = %e, "intent router tier1 failed; using safe fallback");
            Classification::fallback()
        }
    }
}

/// `prepareLucinaTurnContext` for an owner turn.
pub async fn prepare(
    st: &AppState,
    surface: &TurnSurface,
    session: &str,
    user_text: &str,
    user_label: &str,
    platform: &str,
) -> Prepared {
    prepare_with(st, surface, session, user_text, user_label, platform, false).await
}

/// `tools_mode`: the native tool loop decides by itself, so no tier-1
/// classifier call and the full project context.
pub async fn prepare_with(
    st: &AppState,
    surface: &TurnSurface,
    session: &str,
    user_text: &str,
    user_label: &str,
    platform: &str,
    tools_mode: bool,
) -> Prepared {
    let keys = Keys {
        recent: &st.recent,
        recent_key: session,
        peer: &st.cfg.owner_peer_id,
    };
    prepare_keyed(
        st, surface, session, keys, user_text, user_label, platform, tools_mode,
    )
    .await
}

/// Which recent-turn buffer and Honcho peer a turn reads (the Secretary app:
/// its own buffer by session, the owner peer; Telegram: Node's buffer by
/// chat id, the chat's peer).
#[derive(Clone, Copy)]
pub struct Keys<'a> {
    pub recent: &'a lucina_store::recent::RecentChat,
    pub recent_key: &'a str,
    pub peer: &'a str,
}

/// [`prepare_with`] for any channel.
#[allow(clippy::too_many_arguments)]
pub async fn prepare_keyed(
    st: &AppState,
    surface: &TurnSurface,
    session: &str,
    keys: Keys<'_>,
    user_text: &str,
    user_label: &str,
    platform: &str,
    tools_mode: bool,
) -> Prepared {
    let now = Utc::now();
    let tz = st.cfg.time_zone;
    let metadata = keys.recent.metadata_block(keys.recent_key, tz, now);
    let first_after_idle = metadata.contains("- first turn after idle: yes");
    // The owner's personal memory, per the turn's surface (recent turns
    // included: they also feed the intent classifier).
    let owner = surface.may_read(&Scope::OwnerPersonal);
    if !owner {
        surface.log_denied("read", &Scope::OwnerPersonal, "turn_context");
    }
    let recent_context = if owner {
        keys.recent.render(keys.recent_key, tz)
    } else {
        String::new()
    };
    let (classification, policy, reason) = if tools_mode {
        let c = tier0::classify_tier0(user_text, &recent_context, &st.cfg.intent)
            .unwrap_or_else(Classification::fallback);
        (c, Policy::ProjectContext, "tools_mode".to_owned())
    } else {
        let c = classify(
            st,
            user_text,
            &recent_context,
            user_label,
            platform,
            surface.caller().is_some(),
        )
        .await;
        let (p, r) = policy::resolve(&c, first_after_idle, st.cfg.intent.confidence_min);
        (c, p, r)
    };
    tracing::info!(intent = %classification.intent, confidence = classification.confidence, tier0 = classification.tier0(), policy = policy.as_str(), %reason, "lucina intent classified");
    let budgets = policy.budgets();

    let presence = if owner {
        presence_block(st, user_text)
    } else {
        String::new()
    };
    // Her own Pokémon Red run, live, so she answers as the player and can pass it advice (game_hint).
    let crate::bridges::GameContext {
        block: game,
        running: game_live,
    } = crate::bridges::game_context(&st.secretary.http).await;
    let calendar = if owner && st.cfg.calendar_enabled {
        crate::bridges::calendar_context(&st.secretary, user_text, now, tz).await
    } else {
        String::new()
    };
    let notes = if owner && st.cfg.notes_enabled {
        crate::bridges::notes_context(&st.secretary, &st.bridge_state, user_text).await
    } else {
        String::new()
    };
    // Files the owner shared earlier, when the message talks about one.
    let attachments = if owner {
        let memory = st.share.memory.clone();
        let text = user_text.to_owned();
        let at = now.timestamp_millis();
        tokio::task::spawn_blocking(move || memory.context_block(&text, at))
            .await
            .unwrap_or_default()
    } else {
        String::new()
    };
    // LUCINA_TASKS=true: owner turns about the day or tasks get today's tasks.
    let tasks = if owner && st.cfg.tasks_enabled {
        crate::bridges::tasks_context(&st.secretary, user_text, tz).await
    } else {
        String::new()
    };
    // A compound question ("cosa c'è sulla lista della spesa e cosa ho nella
    // nota X?"): no grocery shortcut answers it, so the shopping list comes
    // here (calendar and named notes are above).
    let shopping = if owner {
        shopping_block(st, user_text, now).await
    } else {
        String::new()
    };
    let recent = if policy == Policy::PersonaOnly || recent_context.is_empty() {
        String::new()
    } else {
        clip_recent(&recent_context, budgets.recent_turns)
    };
    let reminders = match (&st.cfg.owner_chat_id, policy) {
        (Some(chat), p) if owner && p != Policy::PersonaOnly && st.cfg.reminders_enabled => {
            let owner = crate::api::owner_reminders(st).unwrap_or_default();
            delivered_context(&owner, chat, tz, now)
        }
        _ => String::new(),
    };
    let honcho = if owner && policy.uses_honcho() {
        let ctx = tokio::time::timeout(
            std::time::Duration::from_secs(12),
            st.memory
                .build_context(surface, keys.peer, session, user_text),
        )
        .await
        .unwrap_or_default();
        let budget = policy.honcho_chars()
            + if policy == Policy::HonchoRecall {
                800
            } else {
                0
            }
            + 400;
        if policy != Policy::LegacyFull && ctx.chars().count() > budget {
            format!(
                "{}\n[honcho context clipped to budget]",
                ctx.chars().take(budget).collect::<String>()
            )
        } else {
            ctx
        }
    } else {
        String::new()
    };
    let memory_context = [
        metadata,
        presence,
        calendar,
        notes,
        attachments,
        tasks,
        shopping,
        recent,
        reminders,
        honcho,
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n")
    .trim()
    .to_owned();
    Prepared {
        memory_context,
        recent_context,
        classification,
        policy,
        game,
        game_live,
    }
}
