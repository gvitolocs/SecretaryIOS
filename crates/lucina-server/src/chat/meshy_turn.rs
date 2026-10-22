//! Meshy in the chat (`LUCINA_MESHY=true`).
//!
//! The JSON planner (the live default) does not know the `meshy` tool, so a
//! clear 3D request ("fammi un modello 3D di…", "turn this photo into a
//! model", "rig it and make it walk") runs a short tool loop with only the
//! `meshy` tool. Lucina plans first (no credits), shows the plan with the
//! clip ids and the cost, and asks. A spending call becomes a pending action
//! (10 minutes): the owner's "sì" in the app (or the ✅ button: the v1
//! `confirm` action sends "sì"), or the Telegram button
//! (`POST /api/secretary/meshy/confirm`), starts the job. MCP callers may
//! plan and list only, and can never confirm.

use std::sync::Arc;
use std::time::Duration;

use lucina_intent::mcp;
use lucina_llm::Message;
use lucina_store::model3d;
use lucina_store::timeline::{Meta, NewItem};
use lucina_tools::agent::{self, AgentConfig, ToolRecord};
use lucina_tools::pending::{classify_reply, PendingAction, Reply};
use lucina_tools::{Ctx, Origin, Registry};
use serde_json::{json, Value};

use super::{Finish, Turn, SECRETARY_EXTRA_SYSTEM};
use crate::api::{PLATFORM_LABEL, USER_LABEL};
use crate::planner::TurnInput;
use crate::prompts::Prompts;
use crate::AppState;

pub const TOOL: &str = "meshy";

const GUIDE: &str = "You can make 3D models with the meshy tool (Meshy AI). Always call meshy with action=plan first, then show the plan in a few short lines: the steps, the animation clips with their ids, the formats, the total credits and what is left of today's budget; end by asking whether to go ahead (\"Procedo?\"). Only after that, call the spending action with the same arguments: it waits for Giuseppe's explicit yes. Prompts for Meshy work best in English. Never claim a model is ready: jobs run in the background and he is notified.";

/// A clear request for 3D work (not every mention of "3D").
pub fn is_meshy_request(text: &str) -> bool {
    let t = text.to_lowercase();
    let has = |w: &[&str]| w.iter().any(|x| t.contains(x));
    if has(&[
        "meshy",
        "modello 3d",
        "modelli 3d",
        "3d model",
        "3d-model",
        "model in 3d",
        "modello in 3d",
        "oggetto 3d",
        "personaggio 3d",
        "3d print",
        "stampa 3d",
        "stampare in 3d",
        "stl",
        "glb",
        "usdz",
        "fbx",
        "low-poly",
        "low poly",
        "lowpoly",
        "turntable",
    ]) {
        return true;
    }
    false
}

/// The tool registry for Meshy turns (just `meshy`).
pub fn registry(st: &AppState) -> Option<Arc<Registry>> {
    st.meshy_registry.clone()
}

fn italian(text: &str) -> bool {
    let t = format!(" {} ", text.to_lowercase());
    [
        " il ",
        " lo ",
        " la ",
        " un ",
        " una ",
        " di ",
        " fammi ",
        " modello ",
        " sì",
        " si ",
        " procedi",
        " grazie",
        " che ",
        " per ",
    ]
    .iter()
    .any(|w| t.contains(w))
}

/// What the rich-reply composer looks for (`crate::rich::is_pending_confirmation`):
/// with `LUCINA_RICH_REPLIES=true` it adds the Sì / No / Modifica buttons.
pub const PENDING_RESULT: &str = "[meshy plan awaiting confirmation]";

/// A reply that asks to confirm a pending Meshy plan: the marker for the
/// composer, and without rich replies the v1 confirm buttons directly.
pub(super) fn mark_pending(st: &AppState, f: &mut Finish, message: &str) {
    f.action = TOOL.into();
    f.action_result = PENDING_RESULT.into();
    if st.cfg.rich_replies {
        return;
    }
    let it = italian(message);
    let actions = model3d::confirm_actions(it);
    f.meta = Some(f.meta.take().unwrap_or_default().merge(Meta {
        kind: Some("meshy_plan".into()),
        format: Some(model3d::FORMAT.into()),
        bubbles: Some(vec![f.reply.trim().to_owned()]),
        lang: Some(if it { "it" } else { "en" }.into()),
        actions: Some(actions.clone()),
        ..Meta::default()
    }));
    f.extra.insert("format".into(), json!(model3d::FORMAT));
    f.extra.insert("bubbles".into(), json!([f.reply.trim()]));
    f.extra.insert("actions".into(), json!(actions));
}

/// The plan as text for Telegram (buttons are added by the bridge).
fn plan_text(st: &AppState, p: &PendingAction, it: bool) -> String {
    let svc = st.meshy.as_ref();
    let plan = svc.and_then(|s| {
        lucina_meshy::Args::from_value(&p.args)
            .ok()
            .and_then(|a| s.estimate(&a).ok())
    });
    let body = plan.map(|p| p.text()).unwrap_or_else(|| p.summary.clone());
    if it {
        format!("{body}\n\nProcedo? Nessun credito è stato speso finora.")
    } else {
        format!("{body}\n\nGood to go? No credits spent so far.")
    }
}

/// After an agent run: is a Meshy plan now waiting for the owner's yes? It
/// also goes to Telegram with buttons.
pub async fn after_agent(st: &AppState, records: &[ToolRecord], message: &str) -> bool {
    let Some(p) = records
        .iter()
        .rev()
        .find_map(|r| r.pending.clone().filter(|p| p.tool == TOOL))
    else {
        return false;
    };
    let it = italian(message);
    if let Some(svc) = st.meshy.as_ref() {
        if svc.telegram.configured() {
            if let Err(e) = svc.telegram.send_plan(&p.id, &plan_text(st, &p, it)).await {
                tracing::warn!(error = %e, "meshy plan not sent to telegram");
            }
        }
    }
    true
}

/// Run a confirmed Meshy pending action.
pub async fn run_confirmed(
    st: &AppState,
    action: &PendingAction,
    channel: &str,
) -> (bool, Value, String) {
    let Some(reg) = registry(st) else {
        return (
            false,
            json!({"error": "Meshy is off"}),
            action.summary.clone(),
        );
    };
    let ctx = Ctx::new(st.tool_env.clone(), channel, &action.session_id).confirmed();
    agent::execute(
        &reg,
        &action.tool,
        action.args.clone(),
        &ctx,
        Duration::from_secs(30),
    )
    .await
}

/// `POST /api/secretary/meshy/confirm` `{pendingId, answer: "yes"|"no", channel?}`
/// (the Telegram buttons). Only Meshy pending actions, each at most once.
pub async fn confirm_by_id(st: &AppState, body: &Value) -> Result<Value, crate::api::ApiError> {
    use crate::api::ApiError;
    if st.meshy.is_none() {
        return Err(ApiError::not_found());
    }
    let id = body["pendingId"].as_str().unwrap_or("").trim();
    let answer = body["answer"].as_str().unwrap_or("");
    if id.is_empty() || id.len() > 64 || !(answer == "yes" || answer == "no") {
        return Err(ApiError::bad_request(
            "pendingId and answer (yes or no) are required",
        ));
    }
    let channel = match body["channel"].as_str() {
        Some("telegram") | None => "telegram",
        Some("web") => "web",
        Some("ios") => "ios",
        Some(_) => {
            return Err(ApiError::bad_request(
                "channel must be telegram, web or ios",
            ))
        }
    };
    // Look before taking: never consume someone else's pending action.
    let now = chrono::Utc::now();
    let found = st.pending.peek(id, now);
    let Some(p) = found.filter(|p| p.tool == TOOL) else {
        return Ok(
            json!({"ok": false, "status": "expired", "text": "Questa conferma è scaduta o è già stata usata. Nessun credito speso."}),
        );
    };
    let Some(action) = st.pending.take(&p.id) else {
        return Ok(json!({"ok": false, "status": "expired", "text": "Già confermato."}));
    };
    let (ok, output, text) = if answer == "no" {
        (
            true,
            json!({"status": "declined"}),
            format!("Annullato: {}. Nessun credito speso.", action.summary),
        )
    } else {
        let (ok, output, _) = run_confirmed(st, &action, channel).await;
        let text = if ok {
            format!(
                "✅ Avviato: {}. Ti avviso quando il modello è pronto.",
                action.summary
            )
        } else {
            format!(
                "Non avviato: {}",
                output["error"].as_str().unwrap_or("errore")
            )
        };
        (ok, output, text)
    };
    st.timeline.record(NewItem {
        channel: channel.into(),
        role: "lucina".into(),
        text: text.clone(),
        meta: Some(Meta {
            tool: Some(TOOL.into()),
            ok: Some(ok),
            ..Meta::kind("tool")
        }),
        ..Default::default()
    });
    Ok(
        json!({"ok": ok, "status": if answer == "no" { "declined" } else if ok { "started" } else { "refused" }, "text": text, "result": output}),
    )
}

/// The json-mode hook: a pending Meshy answer, or a 3D request.
pub(super) async fn try_turn(t: &Turn<'_>, memory: &str) -> Option<Finish> {
    let st = t.st;
    let reg = registry(st)?;
    let now = chrono::Utc::now();
    let it = italian(&t.message);
    if let Some(p) = st
        .pending
        .current(&t.session, now)
        .filter(|p| p.tool == TOOL)
    {
        match classify_reply(&t.message) {
            Reply::Yes if t.mcp.is_some() => {
                return Some(Finish::refusal(t.persona(), TOOL));
            }
            Reply::Yes => {
                let action = st.pending.take(&p.id)?;
                let (ok, output, summary) = run_confirmed(st, &action, &t.channel).await;
                let result =
                    format!("The owner confirmed \"{summary}\" (ok: {ok}). Result: {output}");
                let input = TurnInput {
                    user_text: &t.message,
                    memory,
                    user_label: USER_LABEL,
                    platform: PLATFORM_LABEL,
                };
                let reply = st
                    .planner
                    .generate_skill_reply(&input, "Meshy 3D", &result, "Say in one or two sentences that the 3D job started (or why not), that it runs in the background and that he will be notified. Language of the message.")
                    .await
                    .unwrap_or_else(|_| if ok { "Avviato: ti avviso quando il modello 3D è pronto.".into() } else { "Non sono riuscita ad avviarlo.".into() });
                let mut f = Finish::reply(reply, "meshy");
                f.action_result = format!("[meshy {}]", if ok { "started" } else { "refused" });
                if let Some(job) = output["jobId"].as_str() {
                    f.meta = Some(Meta {
                        job_id: Some(job.into()),
                        kind: Some("meshy".into()),
                        ..Meta::default()
                    });
                }
                return Some(f);
            }
            Reply::No => {
                st.pending.take(&p.id);
                let text = if it {
                    "Va bene, annullato. Nessun credito speso."
                } else {
                    "OK, cancelled. No credits spent."
                };
                return Some(Finish::reply(text, "meshy"));
            }
            Reply::Other => {}
        }
    }
    if !is_meshy_request(&t.message) {
        return None;
    }
    let origin = if t.mcp.is_some() {
        Origin::Mcp
    } else {
        Origin::Owner
    };
    let ctx = Ctx::new(st.tool_env.clone(), &t.channel, &t.session).with_origin(origin);
    let mcp_note = t.mcp.as_ref().map(mcp::note).unwrap_or_default();
    let extra = format!("{SECRETARY_EXTRA_SYSTEM} {GUIDE} {mcp_note}");
    let system = st.planner.prompts.agent_system(
        PLATFORM_LABEL,
        &t.message,
        memory,
        extra.trim(),
        now,
        st.cfg.time_zone,
    );
    let msgs = vec![
        Message::system(system),
        Message::user(Prompts::user_block(&t.user_label, memory, &t.message)),
    ];
    let cfg = AgentConfig {
        deadline: st.cfg.turn_deadline,
        max_rounds: 4,
        ..AgentConfig::default()
    };
    let timeline = st.timeline.clone();
    let channel = t.channel.clone();
    let on_tool = move |r: &ToolRecord| {
        timeline.record(NewItem {
            channel: channel.clone(),
            role: "lucina".into(),
            text: format!("🔧 {} → {}", r.summary, if r.ok { "ok" } else { "error" }),
            meta: Some(Meta {
                tool: Some(r.name.clone()),
                ok: Some(r.ok),
                ..Meta::kind("tool")
            }),
            ..Default::default()
        });
    };
    match agent::run(
        st.model.as_ref(),
        &reg,
        msgs,
        &ctx,
        &st.pending,
        &cfg,
        &on_tool,
    )
    .await
    {
        Ok(out) if !out.text.is_empty() => {
            let mut f = Finish::reply(out.text.clone(), "meshy");
            if after_agent(st, &out.records, &t.message).await {
                mark_pending(st, &mut f, &t.message);
            }
            Some(f)
        }
        Ok(_) | Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_3d_requests() {
        for yes in [
            "Fammi un modello 3D di un forziere",
            "make a 3D model of a mug",
            "turn this photo into an STL",
            "rig it with Meshy",
            "un personaggio low poly per Godot",
        ] {
            assert!(is_meshy_request(yes), "{yes}");
        }
        for no in [
            "che tempo fa domani?",
            "aggiungi il latte alla lista",
            "ho visto un film in 3D",
        ] {
            assert!(!is_meshy_request(no), "{no}");
        }
        assert!(italian("fammi un modello di un forziere"));
        assert!(!italian("make me a model of a chest"));
    }
}
