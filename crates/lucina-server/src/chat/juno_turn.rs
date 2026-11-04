//! Juno's tools on the JSON planner (the live default), for turns from Juno's
//! Chat (session `juno-<device id>`). In tools mode they are in her registry
//! like every other tool; the JSON planner does not know them, so:
//!
//! - a pending `juno_*` action (`delete_file`, `write_file` with `overwrite`,
//!   `wifi_connect`) is settled by this turn: yes runs it, no drops it;
//! - otherwise Jev's first-tool guess over her owner registry decides: when it
//!   names a `juno_*` tool, a short tool loop runs with the four Juno tools.
//!   No message text is matched; without Jev the turn goes on as itself.

use std::sync::Arc;
use std::time::Duration;

use lucina_llm::Message;
use lucina_store::timeline::NewItem;
use lucina_tools::agent::{self, AgentConfig, ToolRecord, ToolRouter};
use lucina_tools::pending::{classify_reply, Reply};
use lucina_tools::tools::juno::NAMES;
use lucina_tools::{Ctx, Origin, Registry};

use super::{effects_to_finish, Finish, Turn, JUNO_SYSTEM};
use crate::api::{PLATFORM_LABEL, USER_LABEL};
use crate::planner::TurnInput;
use crate::prompts::Prompts;

const GUIDE: &str = "You have Juno's tools: juno_tools lists what the laptop can do now, juno_screenshot shows its screen, juno_call runs one driver tool (apps, input, files, status, settings) and juno_bash one shell command. Report what the tools actually returned; never invent the laptop's state. If Juno is offline, say so.";

/// Jev's guess may take at most this long.
const ROUTE_BUDGET: Duration = Duration::from_secs(4);

/// The four Juno tools of her owner registry (`None`: off).
fn registry(st: &crate::AppState) -> Option<Registry> {
    let mut reg = Registry::new();
    for n in NAMES {
        reg.register(st.registry.get(n)?.clone()).ok()?;
    }
    Some(reg)
}

/// Jev's first tool is one of Juno's.
fn wants_juno(first: Option<&str>) -> bool {
    first.is_some_and(|t| NAMES.contains(&t))
}

fn record_tool(t: &Turn<'_>, r: &ToolRecord) {
    t.st.timeline_for(&t.session).record(NewItem {
        channel: t.channel.clone(),
        role: "lucina".into(),
        text: format!("🔧 {} → {}", r.summary, if r.ok { "ok" } else { "error" }),
        meta: Some(super::tool_meta(&t.agent_mark(), r)),
        ..Default::default()
    });
}

pub(super) async fn try_turn(t: &Turn<'_>, memory: &str) -> Option<Finish> {
    let st = t.st;
    if t.guarded || t.consult || t.mcp.is_some() || super::juno_system(&t.session).is_empty() {
        return None;
    }
    let reg = registry(st)?;
    let now = chrono::Utc::now();
    if let Some(p) = st
        .pending
        .current(&t.session, now)
        .filter(|p| NAMES.contains(&p.tool.as_str()))
    {
        match classify_reply(&t.message) {
            Reply::Yes => {
                let action = st.pending.take(&p.id)?;
                let ctx = Ctx::new(st.tool_env.clone(), &t.channel, &t.session).confirmed();
                let started = std::time::Instant::now();
                let (ok, output, summary) = agent::execute(
                    &reg,
                    &action.tool,
                    action.args.clone(),
                    &ctx,
                    agent::PER_TOOL_TIMEOUT,
                )
                .await;
                record_tool(
                    t,
                    &ToolRecord {
                        call_id: action.id.clone(),
                        name: action.tool.clone(),
                        args: action.args,
                        ok,
                        output: output.clone(),
                        summary: summary.clone(),
                        pending: None,
                        ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    },
                );
                let result = format!("The owner confirmed \"{summary}\" and it has now run (ok: {ok}). Result: {output}");
                let input = TurnInput {
                    user_text: &t.message,
                    memory,
                    user_label: USER_LABEL,
                    platform: PLATFORM_LABEL,
                };
                let reply = st
                    .planner
                    .generate_skill_reply(&input, "Juno", &result, "Tell the owner what happened on his laptop in one or two sentences, in English (Danish when he writes Danish; never Italian).")
                    .await
                    .unwrap_or_else(|_| if ok { format!("Done: {summary}.") } else { format!("That did not work: {summary}.") });
                return Some(effects_to_finish(&ctx.effects(), reply));
            }
            Reply::No => {
                st.pending.take(&p.id);
                return Some(Finish::reply(
                    format!("OK, not done: {}.", p.summary),
                    "reply",
                ));
            }
            Reply::Other => {}
        }
    }
    let router: Arc<dyn ToolRouter> = crate::jev_route::tool_router()?;
    let tools: Vec<(String, String)> = st
        .registry
        .specs()
        .into_iter()
        .map(|s| (s.function.name, s.function.description))
        .collect();
    let first = tokio::time::timeout(ROUTE_BUDGET, router.first_tool(&t.message, &tools))
        .await
        .ok()
        .flatten();
    if !wants_juno(first.as_deref()) {
        return None;
    }
    let ctx = Ctx::new(st.tool_env.clone(), &t.channel, &t.session)
        .with_origin(Origin::Owner)
        .owner_turn();
    let extra = crate::capabilities::subloop_extra(&format!("{JUNO_SYSTEM} {GUIDE}"), "");
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
        max_rounds: 5,
        ..AgentConfig::default()
    };
    let on_tool = |r: &ToolRecord| record_tool(t, r);
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
        Ok(out) if !out.text.is_empty() => Some(effects_to_finish(&ctx.effects(), out.text)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_juno_tool_starts_the_juno_turn() {
        assert!(wants_juno(Some("juno_screenshot")));
        assert!(wants_juno(Some("juno_call")));
        assert!(!wants_juno(Some("computer")));
        assert!(!wants_juno(Some("juno_other")));
        assert!(!wants_juno(None));
    }
}
