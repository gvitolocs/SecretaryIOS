//! A text turn's context, as the Node Lucina builds it
//! (`prepareLucinaTurnContext` + `assembleLucinaTurnContext`):
//!
//! turn metadata → intent (tier 0, else the DeepSeek classifier on the
//! recent turns) → context policy → gated sources, joined in Node's order:
//! metadata, Discord presence, calendar, named notes, recent turns,
//! delivered reminders, Honcho (only for policies that allow it, clipped to
//! the policy budget).

use chrono::Utc;
use lucina_intent::policy::{self, Policy};
use lucina_intent::presence;
use lucina_intent::tier0::{self, Classification};
use lucina_store::reminders::delivered_context;

use crate::planner::TurnInput;
use crate::AppState;

pub struct Prepared {
    pub memory_context: String,
    /// The rendered recent turns (also the grocery store context).
    pub recent_context: String,
    pub classification: Classification,
    pub policy: Policy,
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
pub async fn classify(
    st: &AppState,
    user_text: &str,
    recent: &str,
    user_label: &str,
    platform: &str,
) -> Classification {
    let s = &st.cfg.intent;
    if let Some(c) = tier0::classify_tier0(user_text, recent, s) {
        return c;
    }
    let t = TurnInput {
        user_text,
        memory: recent,
        user_label,
        platform,
    };
    match st.planner.classify_json(&t).await {
        Ok(Some(v)) => tier0::from_model_json(&v, user_text, recent, s),
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
    session: &str,
    user_text: &str,
    user_label: &str,
    platform: &str,
) -> Prepared {
    prepare_with(st, session, user_text, user_label, platform, false).await
}

/// `tools_mode`: the native tool loop decides by itself, so no tier-1
/// classifier call and the full project context.
pub async fn prepare_with(
    st: &AppState,
    session: &str,
    user_text: &str,
    user_label: &str,
    platform: &str,
    tools_mode: bool,
) -> Prepared {
    let now = Utc::now();
    let tz = st.cfg.time_zone;
    let metadata = st.recent.metadata_block(session, tz, now);
    let first_after_idle = metadata.contains("- first turn after idle: yes");
    let recent_context = st.recent.render(session, tz);
    let (classification, policy, reason) = if tools_mode {
        let c = tier0::classify_tier0(user_text, &recent_context, &st.cfg.intent)
            .unwrap_or_else(Classification::fallback);
        (c, Policy::ProjectContext, "tools_mode".to_owned())
    } else {
        let c = classify(st, user_text, &recent_context, user_label, platform).await;
        let (p, r) = policy::resolve(&c, first_after_idle, st.cfg.intent.confidence_min);
        (c, p, r)
    };
    tracing::info!(intent = %classification.intent, confidence = classification.confidence, tier0 = classification.tier0(), policy = policy.as_str(), %reason, "lucina intent classified");
    let budgets = policy.budgets();

    let presence = presence_block(st, user_text);
    let calendar = if st.cfg.calendar_enabled {
        crate::bridges::calendar_context(&st.secretary, user_text, now, tz).await
    } else {
        String::new()
    };
    let notes = if st.cfg.notes_enabled {
        crate::bridges::notes_context(&st.secretary, &st.bridge_state, user_text).await
    } else {
        String::new()
    };
    // A compound question ("cosa c'è sulla lista della spesa e cosa ho nella
    // nota X?"): no grocery shortcut answers it, so the shopping list comes
    // here (calendar and named notes are above).
    let shopping = shopping_block(st, user_text, now).await;
    let recent = if policy == Policy::PersonaOnly || recent_context.is_empty() {
        String::new()
    } else {
        clip_recent(&recent_context, budgets.recent_turns)
    };
    let reminders = match (&st.cfg.owner_chat_id, policy) {
        (Some(chat), p) if p != Policy::PersonaOnly && st.cfg.reminders_enabled => {
            let owner = crate::api::owner_reminders(st).unwrap_or_default();
            delivered_context(&owner, chat, tz, now)
        }
        _ => String::new(),
    };
    let honcho = if policy.uses_honcho() {
        let ctx = tokio::time::timeout(
            std::time::Duration::from_secs(12),
            st.memory
                .build_context(&st.cfg.owner_peer_id, session, user_text),
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
        metadata, presence, calendar, notes, shopping, recent, reminders, honcho,
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
    }
}
