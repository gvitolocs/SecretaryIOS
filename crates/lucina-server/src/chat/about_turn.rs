//! "That's wrong, I live in Aarhus", "remember that I start a new job in January", "what do you know about
//! me?": the owner talks about the memory about him, so a short tool loop with `about_me` updates or reads
//! it now (crate::about). Everything else he says reaches the memory through the local extractor after
//! the turn. Owner turns only; a self-test writes its own store.

use std::sync::Arc;

use lucina_llm::Message;
use lucina_store::timeline::Meta;
use lucina_tools::agent::{self, AgentConfig, ToolRecord};
use lucina_tools::tools::about_me::AboutMeTool;
use lucina_tools::{Ctx, Origin, Registry};

use super::{Finish, Turn};
use crate::prompts::Prompts;

pub const ACTION: &str = "about_me";

const GUIDE: &str = "Giuseppe is talking about what you know about him. Your memory about him is in the context (\"About Giuseppe\", with \
fact ids); about_me reads detail and history and changes it. Rules: \
1. He tells you something new about himself: add it, one fact per call, a short third-person sentence, with validUntil when it has a \
last day or validUntilCondition when it holds until something happens. \
2. He corrects you (\"that's wrong\", \"not anymore\"): retract what was never true, end what stopped holding, and add the right fact. \
3. He asks what you know: answer from the memory, briefly. \
4. Never store a password, key, token or ID number: those go to Infisical. \
Answer in one or two short sentences, in English (Danish when he writes Danish; never Italian), saying what you changed.";

/// The words that make a turn about the memory itself.
pub fn about_talk(text: &str) -> bool {
    let t = format!(
        " {} ",
        text.to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    );
    const PHRASES: &[&str] = &[
        " remember that ",
        " remember i ",
        " remember my ",
        " please remember ",
        " don't forget that i ",
        " keep in mind that ",
        " that's wrong",
        " that is wrong",
        " thats wrong",
        " that's not true",
        " not true anymore",
        " no longer true",
        " forget that ",
        " i no longer ",
        " not anymore",
        " what do you know about me",
        " what you know about me",
        " about me you know",
        " update what you know",
        " ricordati che ",
        " ricorda che ",
        " non è vero",
        " non è più vero",
        " cosa sai di me",
        " husk at ",
        " det er forkert",
        " hvad ved du om mig",
    ];
    PHRASES.iter().any(|p| t.contains(p))
}

pub(super) async fn try_turn(t: &Turn<'_>, memory: &str) -> Option<Finish> {
    let st = t.st;
    if t.guarded
        || t.mcp.is_some()
        || !t.surface.is_owner()
        || !st.about.cfg.enabled
        || !about_talk(&t.message)
    {
        return None;
    }
    let mut reg = Registry::new();
    reg.register(Arc::new(AboutMeTool {
        memory: st.about.clone(),
    }))
    .ok()?;
    let ctx = Ctx::new(st.tool_env.clone(), &t.channel, &t.session)
        .with_origin(Origin::Owner)
        .owner_turn();
    let extra = format!("{} {GUIDE}", t.extra_system);
    let system = st.planner.prompts.agent_system(
        t.platform_label(),
        &t.message,
        memory,
        extra.trim(),
        chrono::Utc::now(),
        st.cfg.time_zone,
    );
    let msgs = vec![
        Message::system(system),
        Message::user(Prompts::user_block(&t.user_label, memory, &t.message)),
    ];
    let cfg = AgentConfig {
        deadline: st.cfg.turn_deadline,
        max_rounds: 5,
        max_tool_output_chars: 6_000,
        ..AgentConfig::default()
    };
    let on_tool = |_: &ToolRecord| {};
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
            tracing::info!(calls = out.records.len(), "about-me turn");
            Some(Finish::reply(out.text.clone(), ACTION).with(
                format!("[about me: {} call(s)]", out.records.len()),
                Some(Meta::kind("about_turn")),
            ))
        }
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(error = %e, "about-me turn failed");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_counts_as_talk_about_the_memory() {
        for yes in [
            "That's wrong, I live in Aarhus now",
            "Remember that I start at Novo in January",
            "what do you know about me?",
            "I no longer work at the bar",
            "husk at jeg har SU",
            "ricordati che sono vegetariano",
        ] {
            assert!(about_talk(yes), "{yes}");
        }
        for no in [
            "remember to buy milk",
            "what's the weather",
            "that's great",
            "forget it",
            "tell me about Rust",
        ] {
            assert!(!about_talk(no), "{no}");
        }
    }
}
