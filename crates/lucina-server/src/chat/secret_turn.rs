//! "Salvami la password del Wi-Fi nel progetto lucinaos", "ask me for the OpenAI key": the owner
//! wants a secret stored and said it in his own words, so Lucina picks the name and asks for it.
//!
//! The JSON planner offers no tools, so a message that talks about storing a secret
//! ([`crate::owner_secrets::secret_talk`]) runs a short tool loop with only `secret_request`, which
//! puts a card in his chat ([`crate::secret_requests`]). The exact "salva un segreto …" never gets
//! here: it is answered before the model. The owner's own turns only, on every channel.

use std::sync::Arc;

use lucina_llm::Message;
use lucina_store::timeline::Meta;
use lucina_tools::agent::{self, AgentConfig, ToolRecord};
use lucina_tools::tools::secret_keep::{SecretKeepTool, SecretScanTool};
use lucina_tools::tools::secret_request::SecretRequestTool;
use lucina_tools::{Ctx, Origin, Registry};

use super::{Finish, Turn};
use crate::prompts::Prompts;

pub const ACTION: &str = "secret_request";

const GUIDE: &str = "Giuseppe talks about storing a secret. Your tools: secret_keep moves a secret that already sits in one of his \
notes or chat messages to Infisical by itself: a local model on his machine reads it, you never see it, and it clears the source. \
secret_scan finds where one may be. secret_request puts a card in his chat in the Secretary app, where he pastes a value that is \
nowhere yet; it goes straight to Infisical, never to you. Rules: \
1. If the secret is in a note or a chat message, use secret_keep and don't ask him for anything: he said a local model handling it is \
fine. With no project he named, leave the project out: it goes to the default place. Use secret_scan when you don't know the source. \
A note that is only a list of passwords: whole_note. \
2. Only when it is nowhere yet: secret_request, once per secret, with a clear UPPER_SNAKE name, the Infisical project and environment \
he said, and a one-line reason. If he didn't say the project or the environment there, ask in one short question. \
3. Never ask him to write a value in chat, on any channel, and never repeat anything that looks like one. \
4. If he isn't actually asking to store a secret, answer without the tools. \
Answer in one short sentence, in English (Danish when he writes Danish; never Italian): it's on its way, or the card is waiting in the app.";

pub(super) async fn try_turn(t: &Turn<'_>, memory: &str) -> Option<Finish> {
    let st = t.st;
    if t.guarded || t.mcp.is_some() || !t.surface.is_owner() || crate::selftest::is_session(&t.session) {
        return None;
    }
    if !crate::owner_secrets::secret_talk(&t.message) || !st.cfg.tools.allows("secret_request") {
        return None;
    }
    let mut reg = Registry::new();
    reg.register(Arc::new(SecretRequestTool { asker: st.secret_requests.clone() })).ok()?;
    if st.secret_keep.cfg.enabled {
        reg.register(Arc::new(SecretKeepTool { keeper: st.secret_keep.clone() })).ok()?;
        reg.register(Arc::new(SecretScanTool { keeper: st.secret_keep.clone() })).ok()?;
    }
    let ctx = Ctx::new(st.tool_env.clone(), &t.channel, &t.session).with_origin(Origin::Owner);
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
        max_rounds: 4,
        max_tool_output_chars: 4_000,
        ..AgentConfig::default()
    };
    let on_tool = |_: &ToolRecord| {};
    match agent::run(st.model.as_ref(), &reg, msgs, &ctx, &st.pending, &cfg, &on_tool).await {
        Ok(out) if !out.text.is_empty() => {
            let calls = out.records.len();
            tracing::info!(calls, "secret turn");
            Some(Finish::reply(out.text.clone(), ACTION).with(format!("[secret request: {calls} call(s)]"), Some(Meta::kind("secret_turn"))))
        }
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(error = %e, "secret turn failed");
            None
        }
    }
}
