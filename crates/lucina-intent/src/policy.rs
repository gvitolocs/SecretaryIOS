//! `src/context-policy.js`: which context sources a turn may load, decided
//! from the intent alone (never from the model).

use crate::tier0::Classification;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    PersonaOnly,
    CurrentSession,
    RecentContext,
    NewTopic,
    ProactiveCheckin,
    HonchoRecall,
    ProjectContext,
    LegacyFull,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budgets {
    pub recent_turns: usize,
    pub session_summary: usize,
    pub query_recall: usize,
    pub representation: usize,
    pub reminders: usize,
}

impl Policy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PersonaOnly => "PERSONA_ONLY",
            Self::CurrentSession => "CURRENT_SESSION",
            Self::RecentContext => "RECENT_CONTEXT",
            Self::NewTopic => "NEW_TOPIC",
            Self::ProactiveCheckin => "PROACTIVE_CHECKIN",
            Self::HonchoRecall => "HONCHO_RECALL",
            Self::ProjectContext => "PROJECT_CONTEXT",
            Self::LegacyFull => "LEGACY_FULL",
        }
    }

    pub fn budgets(self) -> Budgets {
        let b = |recent_turns, session_summary, query_recall, representation, reminders| Budgets {
            recent_turns,
            session_summary,
            query_recall,
            representation,
            reminders,
        };
        match self {
            Self::PersonaOnly => b(0, 0, 0, 0, 0),
            Self::CurrentSession => b(2200, 0, 0, 0, 900),
            Self::RecentContext => b(2200, 1200, 0, 0, 900),
            Self::NewTopic => b(700, 0, 0, 0, 600),
            Self::ProactiveCheckin => b(2000, 1200, 0, 0, 900),
            Self::HonchoRecall => b(1200, 1200, 2200, 900, 600),
            Self::ProjectContext => b(3000, 1600, 2200, 1200, 900),
            Self::LegacyFull => b(5000, 4000, 3000, 6000, 900),
        }
    }

    /// Node only asks Honcho outside PERSONA_ONLY and CURRENT_SESSION.
    pub fn uses_honcho(self) -> bool {
        !matches!(self, Self::PersonaOnly | Self::CurrentSession)
    }

    /// Characters of Honcho context the policy allows in total.
    pub fn honcho_chars(self) -> usize {
        let b = self.budgets();
        b.session_summary + b.query_recall + b.representation
    }
}

pub const TOOL_INTENTS: &[&str] = &[
    "list_reminders",
    "schedule_reminder",
    "calendar_event",
    "study_focus_reminders",
    "generate_image",
    "graph_task",
    "spreadsheet_update",
    "web_crawl",
    "oracle_monitor",
    "model_status",
    "revive_qwen",
    "poko_news_scout_retry",
    "social_post",
    "cursor_task",
    "lucina_log_inspect",
    "discord_admin",
];

/// `resolveContextPolicy(classification, turnMetadata)`: `(policy, reason)`.
pub fn resolve(
    c: &Classification,
    first_turn_after_idle: bool,
    confidence_min: f64,
) -> (Policy, String) {
    let intent = c.intent.as_str();
    if intent == "explicit_memory_recall" {
        return (Policy::HonchoRecall, "explicit_memory_recall".into());
    }
    if intent == "unknown" || c.confidence < confidence_min {
        return (
            Policy::CurrentSession,
            format!("safe_fallback:{intent}:{}", c.confidence),
        );
    }
    match intent {
        "social_greeting" => (Policy::PersonaOnly, "pure_greeting".into()),
        "agent_status_check" => (Policy::PersonaOnly, "status_ping".into()),
        i if TOOL_INTENTS.contains(&i) => (Policy::ProjectContext, format!("tool:{i}")),
        "temporal_query" => (Policy::CurrentSession, "temporal_with_clock".into()),
        "new_topic" => (Policy::NewTopic, "clean_boundary_restricted".into()),
        "continue_current_topic" => (Policy::CurrentSession, "continuation".into()),
        "chat" if first_turn_after_idle => {
            (Policy::CurrentSession, "post_idle_chat_no_summary".into())
        }
        "chat" => (Policy::CurrentSession, "casual_chat".into()),
        i => (Policy::CurrentSession, format!("default_safe:{i}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heuristics::Settings;
    use crate::tier0::classify_tier0;

    #[test]
    fn policies() {
        let s = Settings {
            fast_chat: true,
            ..Settings::default()
        };
        let p = |t: &str| resolve(&classify_tier0(t, "", &s).unwrap(), false, 0.72).0;
        assert_eq!(p("ciao"), Policy::PersonaOnly);
        assert_eq!(p("ti ricordi cosa avevamo deciso?"), Policy::HonchoRecall);
        assert_eq!(p("ricordami alle 18:30 di uscire"), Policy::ProjectContext);
        assert_eq!(p("che bella giornata"), Policy::CurrentSession);
        // Grocery intents are not tool intents in the Node policy.
        assert_eq!(p("aggiungi il latte alla lista"), Policy::CurrentSession);
        assert!(!Policy::CurrentSession.uses_honcho());
    }
}
