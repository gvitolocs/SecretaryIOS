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
/// `sample`: a real turn of the owner's own, whose Jev answer is kept (`jev_samples`).
pub async fn classify(
    st: &AppState,
    user_text: &str,
    recent: &str,
    user_label: &str,
    platform: &str,
    mcp: bool,
    sample: bool,
) -> Classification {
    let s = &st.cfg.intent;
    let t = TurnInput {
        user_text,
        memory: recent,
        user_label,
        platform,
    };
    let rule = tier0::classify_tier0(user_text, recent, s);
    let first = crate::jev_route::jev_first_enabled();
    // One Jev call per turn, shared by JEV-first, the tier-0 double-check and tier 1.
    let wants_jev = first || rule.as_ref().is_none_or(crate::jev_route::checks_tier0);
    // A spoken turn already asked Jev its intent questions at the start of the turn (`chat::voice_fast`):
    // that verdict is used, not a second call.
    let jev = if wants_jev {
        match crate::voice::shared_intent().await {
            Some(verdict) => verdict,
            None => crate::jev_route::classify_intent(lucina_jev::shared(), &t).await,
        }
    } else {
        None
    };
    if let (true, Some(verdict)) = (sample, &jev) {
        let tier0 = rule.as_ref().map(|r| r.intent.as_str());
        crate::jev_samples::record(&st.cfg.data_dir, user_text, platform, verdict, tier0);
    }
    let chat = |confidence: f64| {
        tier0::from_model_json(
            &serde_json::json!({"intent": "chat", "confidence": confidence,
                "reply_mode": "chat", "needs_memory": true, "needs_tools": false}),
            user_text, recent, s)
    };
    // JEV first (owner's rule, 2026-10-28): Jev routes every turn it is sure of; the safety layers
    // stay on top (side-effect intents need their own rule to run directly, MCP turns consult).
    if first {
        let pick = jev
            .as_ref()
            .map(|(v, c)| (v["intent"].as_str().unwrap_or(""), *c));
        match crate::jev_route::jev_first(pick, rule.as_ref()) {
            Ok(()) => {
                let (v, confidence) = jev.clone().expect("jev answered");
                let mut c = tier0::from_model_json(&v, user_text, recent, s);
                let agreed = rule.as_ref().is_some_and(|r| r.intent == c.intent);
                let side_effect = crate::jev_route::SIDE_EFFECT_INTENTS.contains(&c.intent.as_str());
                let guard = match (side_effect && !agreed, mcp) {
                    (true, true) => "side_effect_on_mcp_turn:chat",
                    (true, false) => "side_effect_without_rule:planner",
                    _ => "",
                };
                tracing::info!(target: "lucina_jev::route", event = "jev_first", used = true,
                    intent = %c.intent, confidence, tier0 = ?rule.as_ref().map(|r| r.intent.as_str()),
                    guard, "jev_first");
                if guard.ends_with(":chat") {
                    return chat(confidence);
                }
                if guard.ends_with(":planner") {
                    c.reply_mode = "full_plan".into();
                }
                // Jev agrees with a rule: the rule's own classification (its direct plan, reply mode).
                if agreed {
                    return rule.expect("agreed");
                }
                return c;
            }
            Err(reason) => {
                tracing::info!(target: "lucina_jev::route", event = "jev_first", used = false, reason,
                    intent = ?pick.map(|p| p.0), confidence = ?pick.map(|p| p.1),
                    tier0 = ?rule.as_ref().map(|r| r.intent.as_str()), "jev_first fallback");
            }
        }
        // Jev only (owner, 2026-11-01). At the planner's confidence the chat stays chat.
        return crate::jev_route::rule_when_jev_unsure(rule).unwrap_or_else(|| {
            chat(s.confidence_min.max(crate::planner::INTENT_CONFIDENCE_MIN))
        });
    }
    if let Some(c) = rule {
        // A tier-0 regex rule gets Jev's second opinion; only a sure
        // disagreement wins, and never into a side-effect intent.
        if crate::jev_route::checks_tier0(&c) {
            if let Some((v, confidence)) = jev.clone() {
                let jev_intent = v["intent"].as_str().unwrap_or("");
                // Agents write technical prose that regexes over-match ("restart cloudflared at 17:16"):
                // on an MCP turn, any disagreement with a side-effect rule makes it plain chat.
                if mcp && crate::jev_route::mcp_falls_back_to_chat(&c, jev_intent) {
                    tracing::info!(target: "lucina_jev::route", tier0 = %c.intent, jev = jev_intent,
                        confidence, "jev disagrees with a side-effect tier-0 rule on an mcp turn; plain chat");
                    return chat(confidence);
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
    // Tier 1. With JEV first on, Jev was not sure enough: DeepSeek decides ("our thing"), Jev only
    // when DeepSeek fails. Otherwise Jev first: a sure answer is final, an unsure one is checked by
    // DeepSeek, and a failed call (quota, timeout, breaker) leaves the turn to DeepSeek alone.
    if !first {
        if let Some((v, confidence)) = &jev {
            if *confidence >= crate::jev_route::intent_min() {
                return tier0::from_model_json(v, user_text, recent, s);
            }
        }
    }
    // A spoken turn's Jev route may still make this model call pointless (`chat::voice_fast`).
    crate::voice::hold().await;
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
        voice: false,
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
    /// A turn of the Lucina Voice thread: it also reads the app's latest lines (`voice_thread`); an app turn
    /// reads the speaker's.
    pub voice: bool,
}

/// How far back the share-sheet session's turns count as this conversation's.
const SHARED_HOURS: i64 = 3;

/// The app's default session (share sheet, clients without a session id), when this turn is another
/// app session of the owner's (`secretary_ios-…`, a web session). Not for a device that is a conversation
/// of its own (Juno), the voice thread, a self-test (it starts from what it is given), or the default session itself.
fn shared_session<'a>(st: &'a AppState, keys: Keys<'_>) -> Option<&'a str> {
    let default = st.cfg.default_session.as_str();
    let key = keys.recent_key;
    let app = key.starts_with("secretary_") && !key.starts_with("secretary_juno") && !crate::selftest::is_session(key);
    (std::ptr::eq(keys.recent, &*st.recent) && app && key != default && !keys.voice).then_some(default)
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
        let shared = shared_session(st, keys);
        match shared {
            // A file he shared from the iPhone's share sheet lands in the app's default session: it is the
            // same conversation, so its last hours count as recent turns of this one.
            Some(other) => keys.recent.render_with(keys.recent_key, &[other], now - chrono::Duration::hours(SHARED_HOURS), tz),
            None => keys.recent.render(keys.recent_key, tz),
        }
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
            owner && surface.caller().is_none() && !crate::selftest::is_session(session),
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
    // What she knows about him: every owner turn, whatever the policy (crate::about).
    let about = if owner && st.about.cfg.enabled { crate::about::context_block(st, session) } else { String::new() };
    // The other side of the Lucina Voice thread: the speaker's latest lines for an app turn, the app's for
    // a spoken one (the timeline is the record both read, whatever session each used).
    let other_surface = if owner && policy != Policy::PersonaOnly {
        crate::voice_thread::other_side_block(st, keys.voice, now)
    } else {
        String::new()
    };
    // Voice Learn: what the owner's own words mean (every turn whose text holds one).
    let vocabulary = if owner {
        crate::voice_learn::context_block(st, user_text)
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
        about,
        presence,
        vocabulary,
        other_surface,
        calendar,
        notes,
        attachments,
        tasks,
        shopping,
        reminders,
        recent,
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
