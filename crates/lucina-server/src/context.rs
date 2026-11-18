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
use lucina_memory::{Scope, Surface, TurnSurface};
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
/// `owner_turn`: the owner's own surface, with nothing in the turn saying someone else wrote it
/// ([`prepare_keyed`] computes it); only such a turn may start something.
/// `device`: the device the owner is talking through (`crate::device::jev_device_of_turn`), a fact in Jev's state.
/// `sample`: a real turn of the owner's own, whose Jev answer is kept (`jev_samples`).
#[allow(clippy::too_many_arguments)]
pub async fn classify(
    st: &AppState,
    user_text: &str,
    recent: &str,
    user_label: &str,
    platform: &str,
    device: &str,
    mcp: bool,
    owner_turn: bool,
    sample: bool,
) -> Classification {
    let s = &st.cfg.intent;
    let t = TurnInput {
        user_text,
        memory: recent,
        user_label,
        platform,
        device,
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
            user_text,
            recent,
            s,
        )
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
                let side_effect =
                    crate::jev_route::SIDE_EFFECT_INTENTS.contains(&c.intent.as_str());
                let guard = match (side_effect && !agreed, mcp) {
                    (true, true) => "side_effect_on_mcp_turn:chat",
                    (true, false) => "side_effect_without_rule:planner",
                    _ => "",
                };
                // Jev won against a deterministic rule that disagreed with it (TURN-01): which one.
                let overrode_rule = rule
                    .as_ref()
                    .filter(|r| crate::jev_route::deterministic_rule(r) && r.intent != c.intent)
                    .map(|r| r.intent.clone())
                    .unwrap_or_default();
                // Such a rule is beaten only by a question (`jev_beats_rule_from`), so the intent that
                // won never starts something: this is the same fact from the other side.
                debug_assert!(
                    !crate::jev_route::acts(&c.intent) || overrode_rule.is_empty(),
                    "an acting intent overrode the rule {}",
                    overrode_rule
                );
                tracing::info!(target: "lucina_jev::route", event = "jev_first", used = true,
                    intent = %c.intent, confidence, tier0 = ?rule.as_ref().map(|r| r.intent.as_str()),
                    overrode_rule = %overrode_rule, guard, "jev_first");
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
                let deterministic = rule
                    .as_ref()
                    .is_some_and(|r| crate::jev_route::deterministic_rule(r));
                // Jev only (owner, 2026-11-01). At the planner's confidence the chat stays chat.
                let confidence = s.confidence_min.max(crate::planner::INTENT_CONFIDENCE_MIN);
                if unsure_plan(mcp, deterministic, owner_turn) {
                    // The words Jev could not place may still ask for something done, so the turn goes
                    // to the JSON planner (`plan_response`): one plan, then that plan's action, under
                    // the guards and confirmations of any other planner turn. It does not get the tool
                    // registry, and nothing here starts a side effect by itself.
                    tracing::info!(target: "lucina_jev::route", event = "unsure_plan", reason,
                        jev = ?pick.map(|p| p.0), confidence = ?pick.map(|p| p.1));
                    let mut c = chat(confidence);
                    c.reply_mode = "full_plan".into();
                    return c;
                }
                return crate::jev_route::rule_when_jev_unsure(rule)
                    .unwrap_or_else(|| chat(confidence));
            }
        }
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

/// `LUCINA_UNSURE_PLAN` (on unless off): a turn Jev was unsure of. Only the owner's own turn with no
/// deterministic route to run and no agent behind it goes to the JSON planner; every other is
/// answered as before. Pure: [`unsure_plan`] reads the environment.
pub fn unsure_plan_from(
    mcp: bool,
    deterministic: bool,
    owner_turn: bool,
    switch: Option<&str>,
) -> bool {
    !mcp && !deterministic && owner_turn && !crate::jev_route::switch_off(switch)
}

/// [`unsure_plan_from`] with `LUCINA_UNSURE_PLAN`.
pub fn unsure_plan(mcp: bool, deterministic: bool, owner_turn: bool) -> bool {
    unsure_plan_from(
        mcp,
        deterministic,
        owner_turn,
        std::env::var("LUCINA_UNSURE_PLAN").ok().as_deref(),
    )
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
        intent_text: None,
        device: "",
    };
    prepare_keyed(
        st, surface, session, keys, user_text, user_label, platform, None, tools_mode,
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
    /// The owner's own words when the turn's text holds more (a share: his note, not the shared content). Intent
    /// rules read only these: a screenshot's "O(m log n)" once made a share a log inspection (5 Nov 2026).
    pub intent_text: Option<&'a str>,
    /// The device the owner is talking through, for Jev's state (`crate::device::jev_device_of_turn`); empty: not known.
    pub device: &'a str,
}

/// How far back the share-sheet session's turns count as this conversation's.
const SHARED_HOURS: i64 = 3;

/// The app's default session (share sheet, clients without a session id), when this turn is another
/// app session of the owner's (`secretary_ios-…`, a web session). Not for a device that is a conversation
/// of its own (Juno), the voice thread, a self-test (it starts from what it is given), or the default session itself.
fn shared_session<'a>(st: &'a AppState, keys: Keys<'_>) -> Option<&'a str> {
    let default = st.cfg.default_session.as_str();
    let key = keys.recent_key;
    let app = key.starts_with("secretary_")
        && !key.starts_with("secretary_juno")
        && !crate::selftest::is_session(key);
    (std::ptr::eq(keys.recent, &*st.recent) && app && key != default && !keys.voice)
        .then_some(default)
}

/// How far back the agents' jobs count as his recent requests.
const DELEGATION_HOURS: i64 = 12;

/// The owner's delegated jobs of the last [`DELEGATION_HOURS`], newest first: who was asked, who answered, the
/// state now and the answer's gist. Empty when there are none (or delegation is off).
fn delegation_block(st: &AppState, session: &str, now: chrono::DateTime<Utc>) -> String {
    let Some(d) = st.delegator.as_ref() else {
        return String::new();
    };
    let tz = st.cfg.time_zone;
    let since = now - chrono::Duration::hours(DELEGATION_HOURS);
    let local = |v: &serde_json::Value| {
        v.as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&tz).format("%H:%M").to_string())
    };
    let mut lines = Vec::new();
    for job in d.recent(10) {
        // His own requests only: a job an agent or an MCP caller started is theirs, and a self-test's jobs
        // belong to the self-test conversation.
        if job.caller.is_some()
            || crate::selftest::is_session(&job.session_id) != crate::selftest::is_session(session)
        {
            continue;
        }
        let Ok(v) = serde_json::to_value(&job) else {
            continue;
        };
        let started = v["createdAt"]
            .as_str()
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
        if started.is_none_or(|t| t.with_timezone(&Utc) < since) {
            continue;
        }
        let asked = v["agent"].as_str().unwrap_or("agent");
        let answered = v["answeredBy"].as_str().filter(|a| *a != asked);
        let backend = v["backend"].as_str().unwrap_or("");
        let id: String = job.id.chars().take(8).collect();
        let head = match answered {
            Some(a) => format!("{asked} (answered by {a}, {backend})"),
            None => format!("{asked} ({backend})"),
        };
        let state = match v["status"].as_str().unwrap_or("") {
            "running" => format!(
                "RUNNING since {} (cancel: delegation_cancel {id})",
                local(&v["createdAt"]).unwrap_or_default()
            ),
            "done" => format!(
                "DONE at {}, answer already posted in the chat",
                local(&v["finishedAt"]).unwrap_or_default()
            ),
            "error" => format!(
                "FAILED at {}: {}",
                local(&v["finishedAt"]).unwrap_or_default(),
                v["error"]
                    .as_str()
                    .unwrap_or("no answer")
                    .chars()
                    .take(200)
                    .collect::<String>()
            ),
            other => format!(
                "{} at {}",
                other.to_uppercase(),
                local(&v["finishedAt"]).unwrap_or_default()
            ),
        };
        let fallback = v["fallback"]["reason"]
            .as_str()
            .map(|r| format!(" Fallback: {r}."))
            .unwrap_or_default();
        let gist: String = v["summary"]
            .as_str()
            .or_else(|| v["result"].as_str())
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(240)
            .collect();
        let gist = if gist.is_empty() {
            String::new()
        } else {
            format!(" Gist: {gist}")
        };
        lines.push(format!(
            "- [{id}] {head}: {state}.{fallback} Task: {}.{gist}",
            job.task.chars().take(120).collect::<String>()
        ));
    }
    if lines.is_empty() {
        return String::new();
    }
    format!(
        "AGENT JOBS you started for him (live state now; it overrides anything said earlier in the chat):\n{}\n\
Never say a job is still running, pending or about to land unless it is RUNNING here. A finished job's answer is \
already in the chat: say so, and say which agent actually answered. To stop a RUNNING job use delegation_cancel; a job \
that is not RUNNING cannot be stopped because it has already ended.",
        lines.join("\n")
    )
}

/// [`prepare_with`] for any channel. `channel_owner`: a channel turn's own verdict on whether the
/// owner himself wrote it ([`crate::chat::ChannelOrigin::owner`]); `None` on the Secretary path,
/// which has no origin.
#[allow(clippy::too_many_arguments)]
pub async fn prepare_keyed(
    st: &AppState,
    surface: &TurnSurface,
    session: &str,
    keys: Keys<'_>,
    user_text: &str,
    user_label: &str,
    platform: &str,
    channel_owner: Option<bool>,
    tools_mode: bool,
) -> Prepared {
    let now = Utc::now();
    let tz = st.cfg.time_zone;
    // Whose words these are, for what may start something on them: his own surface — never a
    // delegate, a group, another person or background work — and no channel fact saying otherwise.
    let owner_turn =
        matches!(surface.surface(), Surface::OwnerDirect) && channel_owner != Some(false);
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
            Some(other) => keys.recent.render_with(
                keys.recent_key,
                &[other],
                now - chrono::Duration::hours(SHARED_HOURS),
                tz,
            ),
            None => keys.recent.render(keys.recent_key, tz),
        }
    } else {
        String::new()
    };
    let intent_text = keys.intent_text.unwrap_or(user_text);
    let (classification, policy, reason) = if tools_mode {
        let c = tier0::classify_tier0(intent_text, &recent_context, &st.cfg.intent)
            .unwrap_or_else(Classification::fallback);
        (c, Policy::ProjectContext, "tools_mode".to_owned())
    } else {
        let c = classify(
            st,
            intent_text,
            &recent_context,
            user_label,
            platform,
            keys.device,
            surface.caller().is_some(),
            owner_turn,
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
    let about = if owner && st.about.cfg.enabled {
        crate::about::context_block(st, session)
    } else {
        String::new()
    };
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
    // The agents she asked for him: live state on every owner turn, whatever the policy, so a casual "thanks"
    // never gets "I'll post them when they land" about jobs that already answered (5 Nov 2026).
    let delegations = if owner {
        delegation_block(st, session, now)
    } else {
        String::new()
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
        delegations,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// TURN-02: of the sixteen turns Jev was unsure of, exactly one goes to the planner: the owner's
    /// own, with no deterministic rule to run. The switch's off words leave none.
    #[test]
    fn only_the_owners_own_unsure_turn_goes_to_the_planner() {
        // (an MCP turn, a deterministic rule, the owner's own surface) → the planner?
        let rows = [
            ((false, false, false), false),
            ((false, false, true), true),
            ((false, true, false), false),
            ((false, true, true), false),
            ((true, false, false), false),
            ((true, false, true), false),
            ((true, true, false), false),
            ((true, true, true), false),
        ];
        for switch in ["1", "true", "on", "yes", "", "x", " ON "] {
            for ((mcp, deterministic, owner_turn), plan) in rows {
                assert_eq!(
                    unsure_plan_from(mcp, deterministic, owner_turn, Some(switch)),
                    plan,
                    "{switch} mcp={mcp} rule={deterministic} owner={owner_turn}"
                );
            }
        }
        for switch in ["0", "false", "off", "no", "OFF", " No "] {
            for ((mcp, deterministic, owner_turn), _) in rows {
                assert!(
                    !unsure_plan_from(mcp, deterministic, owner_turn, Some(switch)),
                    "{switch} mcp={mcp} rule={deterministic} owner={owner_turn}"
                );
            }
        }
        // Unset is on.
        assert!(unsure_plan_from(false, false, true, None));
        assert!(!unsure_plan_from(false, false, false, None));
    }
}
