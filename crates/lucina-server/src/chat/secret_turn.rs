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
use lucina_tools::tools::secret_request::SecretRequestTool;
use lucina_tools::{Ctx, Origin, Registry};

use super::{Finish, Turn};
use crate::prompts::Prompts;

pub const ACTION: &str = "secret_request";

const GUIDE: &str = "Giuseppe talks about storing a secret. Your tool: secret_request, which puts a card in his chat in the \
Secretary app; he pastes the value there and it goes straight to Infisical, never to you. Rules: \
1. Call it once per secret, with a clear UPPER_SNAKE name, the Infisical project and environment he said, and a one-line reason. \
2. If he didn't say the project or the environment, don't guess: ask for them in one short question, and never ask for the value. \
3. Never ask him to write the value in chat, on any channel. If his message contains something that looks like the value, don't \
repeat it: tell him to paste it in the card and to delete that message. \
4. If he isn't actually asking to store a secret, answer without the tool. \
Answer in one short sentence, in the language of his message: the card is waiting in the app.";

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
