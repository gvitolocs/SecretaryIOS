//! "Accendi i Anespici e passami Pokémon Diamante su questo DSi": the owner asks her to do
//! something on one of his devices or machines that she has no tool of her own for. An agent does
//! it ([`crate::act`]); she tells him in one sentence that an agent is on it, and the result comes
//! back on the channel he asked from.
//!
//! Only Jev routes here: a turn Jev read as `agent_task` at or above its threshold
//! ([`crate::jev_route::jev_first_min`]). There is no regex rule for it and no text matching to
//! start an agent; the stops below only decide to ask him first. The owner's own turns in the
//! Secretary app only: not a guarded turn, not an MCP caller, not a channel's sender.

use lucina_store::timeline::Meta;
use serde_json::json;

use super::{Finish, Turn};

pub const ACTION: &str = crate::act::ACTION;

/// What the request is that must not start an agent without him: money, credentials, accepting
/// terms, or a message in his name. Deterministic and cheap: it only decides to ask first, and the
/// agent's brief repeats the same four stops.
pub(super) fn stop_kind(message: &str) -> Option<&'static str> {
    if super::voice_fast::risk_kind(ACTION, message) == Some("purchase") {
        return Some("purchase");
    }
    let creds = lucina_intent::re!(
        r"\b(passwords?|passcode|credentials?|pin code|credenziali|parola d'ordine|adgangskode|log ?in|sign ?in|accedi (con|a)|log ind)\b",
        "i"
    );
    if creds.test(message) {
        return Some("credentials");
    }
    let terms = lucina_intent::re!(
        r"\b(accept\w*|accett\w*|accepter\w*)\b[^.?!\n]{0,30}\b(terms|agreement|licen[cs]e|termini|condizioni|vilkår|licens)\b",
        "i"
    );
    if terms.test(message) {
        return Some("terms");
    }
    let name = lucina_intent::re!(
        r"\b(send|sende|manda\w*|invia\w*|scrivi|skriv)\b[^.?!\n]{0,40}\b(e-?mails?|mail|messaggio|messaggi|message|besked|sms|dm)\b",
        "i"
    );
    if name.test(message) {
        return Some("message");
    }
    None
}

/// The question he is asked when an agent must not start by itself.
fn stop_question(kind: &str) -> (String, &'static str) {
    match kind {
        "purchase" => (
            "Should an agent make a purchase or a payment for this?".into(),
            "this would make a purchase or a payment",
        ),
        "credentials" => (
            "Should an agent use your password or credentials for this?".into(),
            "this would enter the owner's password or credentials",
        ),
        "terms" => (
            "Should an agent accept the terms of an agreement for this?".into(),
            "this would accept the terms of an agreement",
        ),
        _ => (
            "Should an agent send a message in your name for this?".into(),
            "this would send a message on his behalf",
        ),
    }
}

fn ack(lang: &str) -> &'static str {
    match lang {
        "da" => "En agent er på sagen: jeg fortæller dig her, hvordan det gik.",
        _ => "An agent is on it: I'll tell you here how it went.",
    }
}

fn asked(lang: &str, id: &str) -> String {
    match lang {
        "da" => format!("Jeg har ikke startet en agent: det ser ud til at koste penge, kræve adgangsoplysninger eller tale i dit navn. Jeg har spurgt dig først ({id})."),
        _ => format!("I did not start an agent: this looks like it costs money, needs credentials or speaks in your name. I asked you first ({id})."),
    }
}

pub(super) async fn try_turn(
    t: &Turn<'_>,
    routed_intent: &str,
    confidence: f64,
    cmid: Option<&str>,
) -> Option<Finish> {
    if routed_intent != ACTION || confidence < crate::jev_route::jev_first_min(ACTION) {
        return None;
    }
    if t.guarded || t.consult || t.mcp.is_some() || t.origin.is_some() || !t.surface.is_owner() {
        return None;
    }
    let lang = crate::rich::guess_language(&t.message, &t.message);
    if let Some(kind) = stop_kind(&t.message) {
        let (question, reason) = stop_question(kind);
        let body = json!({
            "question": question,
            "options": ["Go ahead", "Not now"],
            "reversible": false,
            "impact": "high",
            "topic": "agent_task",
            "context": format!("The owner asked: {} ({reason}).", t.message),
            "recommendation": "Not now",
            "asking_session": "lucina-act",
        });
        return match crate::decide::create(t.st, &body).await {
            Ok(d) => {
                tracing::info!(decision = %d.decision_id, kind, "agent task: asked the owner first");
                Some(Finish::reply(asked(lang, &d.decision_id), ACTION).with(
                    format!("[agent task not started: {kind}; asked {}]", d.decision_id),
                    Some(Meta::kind("act_turn")),
                ))
            }
            Err(e) => {
                tracing::warn!(error = ?e, kind, "agent task: couldn't ask the owner; nothing started");
                None
            }
        };
    }
    let brief = crate::act::brief(&t.message, &t.channel);
    if crate::selftest::is_session(&t.session) {
        // Routed and briefed exactly the same; no agent, no job, no push.
        let reply = format!("{} [selftest: agent not started]", ack(lang));
        return Some(Finish::reply(reply, ACTION).with(
            format!("[selftest: agent not started] brief:\n{brief}"),
            Some(Meta::kind("act_turn")),
        ));
    }
    let plan = json!({
        "action": ACTION,
        "task": t.message,
        "device": crate::device::label(&t.channel),
        "channel": t.channel,
        "brief": brief,
        "ack": ack(lang),
    });
    let mut f = super::queue_job(t, &plan, ACTION, cmid);
    f.action = ACTION.into();
    Some(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppState, Config};
    use lucina_intent::mcp::McpTurn;

    const DSI_REQUEST: &str = "Accendi i Anespici e passami Pokémon Diamante su questo DSi";

    /// A state on a temp dir, and an agentd socket path that exists (the job's blocker looks for it).
    fn state() -> (AppState, tempfile::TempDir) {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let sock = std::env::temp_dir()
                .join(format!("act-turn-test-agentd-{}.sock", std::process::id()));
            std::fs::write(&sock, "").unwrap();
            unsafe { std::env::set_var("LUCINA_AGENTD_SOCKET", &sock) };
        });
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::for_data_dir(dir.path());
        cfg.memory_mode = lucina_memory::MemoryMode::Off;
        (AppState::build(cfg, None).unwrap(), dir)
    }

    fn turn<'a>(st: &'a AppState, message: &str, channel: &str, session: &str) -> Turn<'a> {
        Turn {
            st,
            message: message.into(),
            session: session.into(),
            channel: channel.into(),
            mcp: None,
            surface: crate::surface::chat_surface(
                &st.cfg,
                Some(crate::surface::BearerVerified::checked()),
                channel,
                None,
            ),
            guarded: false,
            consult: false,
            user_label: "Giuseppe".into(),
            extra_system: String::new(),
            share: false,
            share_note: String::new(),
            origin: None,
        }
    }

    #[tokio::test]
    async fn act_turn_queues_an_agent_job_when_jev_routes_it() {
        let (st, _d) = state();
        let t = turn(&st, DSI_REQUEST, "dsi", "secretary_fortuna-abc");
        let f = try_turn(&t, "agent_task", 0.9, Some("cm-1"))
            .await
            .expect("routed");
        assert_eq!(f.action, "agent_task");
        let job = f.job.as_ref().expect("a queued job");
        assert_eq!(
            (job.action.as_str(), job.status.as_str()),
            ("agent_task", "queued")
        );
        assert_eq!(job.client_message_id.as_deref(), Some("cm-1"));
        assert_eq!(
            f.reply,
            "An agent is on it: I'll tell you here how it went."
        );
        for refusal in ["can't", "cannot", "not connected", "unreachable"] {
            assert!(!f.reply.to_lowercase().contains(refusal), "{}", f.reply);
        }
        let plan = f.job_plan.as_ref().expect("the job's plan");
        assert_eq!(plan["action"], "agent_task");
        assert_eq!(plan["task"], DSI_REQUEST);
        assert_eq!(plan["device"], "Nintendo DSi");
        assert_eq!(plan["channel"], "dsi");
        let brief = plan["brief"].as_str().unwrap();
        assert!(
            brief.contains(DSI_REQUEST)
                && brief.contains("Nintendo DSi")
                && brief.contains("connected")
        );
    }

    #[tokio::test]
    async fn act_turn_answers_in_danish_when_he_writes_danish() {
        let (st, _d) = state();
        let t = turn(
            &st,
            "Tænd for min pc og læg Pokemon Diamond på min DSi, tak",
            "dsi",
            "secretary_fortuna-abc",
        );
        let f = try_turn(&t, "agent_task", 0.9, None).await.expect("routed");
        assert!(f.reply.starts_with("En agent er på sagen"), "{}", f.reply);
    }

    #[tokio::test]
    async fn act_turn_below_the_threshold_or_for_another_intent_is_not_taken() {
        let (st, _d) = state();
        let t = turn(&st, DSI_REQUEST, "dsi", "secretary_fortuna-abc");
        assert!(try_turn(&t, "agent_task", 0.5, None).await.is_none());
        assert!(try_turn(&t, "computer_task", 0.99, None).await.is_none());
        assert!(try_turn(&t, "chat", 0.99, None).await.is_none());
    }

    #[tokio::test]
    async fn act_turn_never_for_a_guarded_or_mcp_turn() {
        let (st, _d) = state();
        let mut t = turn(&st, DSI_REQUEST, "dsi", "secretary_fortuna-abc");
        t.guarded = true;
        assert!(
            try_turn(&t, "agent_task", 0.99, None).await.is_none(),
            "guarded"
        );
        let mut t = turn(&st, DSI_REQUEST, "dsi", "secretary_fortuna-abc");
        t.mcp = Some(McpTurn {
            caller: "chatgpt".into(),
            text: DSI_REQUEST.into(),
            allow: Vec::new(),
            conversation: None,
        });
        assert!(
            try_turn(&t, "agent_task", 0.99, None).await.is_none(),
            "mcp"
        );
    }

    #[tokio::test]
    async fn act_turn_a_purchase_asks_the_owner_and_starts_nothing() {
        let (st, _d) = state();
        let t = turn(
            &st,
            "Ordina una pizza su Wolt e pagala dal mio DSi",
            "dsi",
            "secretary_fortuna-abc",
        );
        let f = try_turn(&t, "agent_task", 0.95, None).await.expect("taken");
        assert!(f.job.is_none() && f.job_plan.is_none(), "no job");
        assert!(
            f.reply.starts_with("I did not start an agent") && f.reply.contains("dc-"),
            "{}",
            f.reply
        );
        assert!(f.action_result.contains("purchase"));
        let id = f
            .action_result
            .split("asked ")
            .nth(1)
            .unwrap()
            .trim_end_matches(']')
            .to_owned();
        let d = crate::decide::store(&st.cfg.data_dir)
            .get(&id, chrono::Utc::now())
            .expect("the decision exists");
        assert_eq!(d.status, "pending");
    }

    #[test]
    fn act_turn_stops_are_money_credentials_terms_and_messages_in_his_name() {
        assert_eq!(
            stop_kind("compra Pokémon Diamond sull'eShop"),
            Some("purchase")
        );
        assert_eq!(
            stop_kind("log in to my Steam account and download it"),
            Some("credentials")
        );
        assert_eq!(
            stop_kind("usa la mia password di GitHub"),
            Some("credentials")
        );
        assert_eq!(
            stop_kind("accept the terms of the licence and install it"),
            Some("terms")
        );
        assert_eq!(
            stop_kind("send an email to Marianne about it"),
            Some("message")
        );
        assert_eq!(stop_kind(DSI_REQUEST), None);
        assert_eq!(
            stop_kind("copy the Pokémon Diamond ROM from nespc to the DSi"),
            None
        );
    }

    #[tokio::test]
    async fn act_turn_selftest_routes_and_briefs_but_starts_nothing() {
        let (st, _d) = state();
        let t = turn(&st, DSI_REQUEST, "dsi", crate::selftest::SESSION);
        let f = try_turn(&t, "agent_task", 0.9, None).await.expect("routed");
        assert!(
            f.job.is_none() && f.job_plan.is_none(),
            "no job in a self-test"
        );
        assert!(f.reply.contains("An agent is on it"), "{}", f.reply);
        assert!(
            f.reply.ends_with("[selftest: agent not started]"),
            "{}",
            f.reply
        );
        assert!(
            f.action_result.contains(DSI_REQUEST) && f.action_result.contains("Nintendo DSi"),
            "the brief is kept"
        );
    }
}
