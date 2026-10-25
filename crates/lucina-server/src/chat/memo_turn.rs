//! The owner's voice memos in the chat.
//!
//! The JSON planner (the live default) does not know the `memos` tool, so a
//! question about his memos ("cosa ho detto nel memo di ieri sul preventivo?",
//! "what did I say in yesterday's memo about X", "riassumi di nuovo l'ultimo
//! memo") runs a short tool loop with `memos` and `tasks` (action items become
//! tasks when he asks). Runs before the media turn: "registrazione" alone stays
//! the library's, "memo"/"nota vocale" are ours. MCP callers may ask too.

use std::sync::Arc;

use lucina_intent::mcp;
use lucina_llm::Message;
use lucina_store::timeline::{Meta, NewItem};
use lucina_tools::agent::{self, AgentConfig, ToolRecord};
use lucina_tools::tools::{memos::MemosTool, tasks::TasksTool};
use lucina_tools::{Ctx, Origin, Registry};

use super::{Finish, Turn, SECRETARY_EXTRA_SYSTEM};
use crate::api::PLATFORM_LABEL;
use crate::prompts::Prompts;

const GUIDE: &str = "Giuseppe records voice memos in the Secretary app; each has a transcript with times and your summary, \
reachable with the memos tool. For a question about what he said, search: turn relative days into from/to as YYYY-MM-DD in his \
time zone (today, yesterday = today minus one day, last week = the last 7 days with days=7) and pass the topic words as query; \
if nothing matches, search the same days without query and read the summaries. Answer from what he actually said, briefly, \
in his language, quoting the time in the recording (e.g. \"at 1:05\") and the memo's title. Use read for the full transcript, \
resummarize when he wants a new summary (with his instructions), and the tasks tool (add) when he asks to turn action items \
into tasks. Never invent content that is not in the memo. If he is not asking about his memos, answer without the tools.";

/// A question or request about the owner's voice memos (whole words: "memorizza" is not one).
pub fn is_memo_request(text: &str) -> bool {
    let t = text.to_lowercase();
    let words: Vec<&str> = t.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let word = |w: &[&str]| words.iter().any(|x| w.contains(x));
    let phrase = |p: &[&str]| p.iter().any(|x| t.contains(x));
    word(&["memo", "memos", "memoer", "memoen", "memoet", "lydnote", "lydnoter", "talememo", "diktat"])
        || phrase(&["voice note", "voice-note", "nota vocale", "note vocali", "appunto vocale", "appunti vocali", "vocale di oggi", "vocale di ieri"])
}

pub(super) async fn try_turn(t: &Turn<'_>, memory: &str) -> Option<Finish> {
    if !is_memo_request(&t.message) {
        return None;
    }
    let st = t.st;
    st.tool_env.memo_url.as_ref()?;
    let mut reg = Registry::new();
    reg.register(Arc::new(MemosTool)).ok()?;
    reg.register(Arc::new(TasksTool)).ok()?;
    let origin = if t.mcp.is_some() { Origin::Mcp } else { Origin::Owner };
    let ctx = Ctx::new(st.tool_env.clone(), &t.channel, &t.session).with_origin(origin);
    let mcp_note = t.mcp.as_ref().map(mcp::note).unwrap_or_default();
    let extra = format!("{SECRETARY_EXTRA_SYSTEM} {GUIDE} {mcp_note}");
    let now = chrono::Utc::now();
    let system =
        st.planner.prompts.agent_system(PLATFORM_LABEL, &t.message, memory, extra.trim(), now, st.cfg.time_zone);
    let msgs = vec![Message::system(system), Message::user(Prompts::user_block(&t.user_label, memory, &t.message))];
    let cfg = AgentConfig { deadline: st.cfg.turn_deadline, max_rounds: 5, ..AgentConfig::default() };
    let timeline = st.timeline.clone();
    let channel = t.channel.clone();
    let on_tool = move |r: &ToolRecord| {
        timeline.record(NewItem {
            channel: channel.clone(),
            role: "lucina".into(),
            text: format!("🔧 {} → {}", r.summary, if r.ok { "ok" } else { "error" }),
            meta: Some(Meta { tool: Some(r.name.clone()), ok: Some(r.ok), ..Meta::kind("tool") }),
            ..Default::default()
        });
    };
    match agent::run(st.model.as_ref(), &reg, msgs, &ctx, &st.pending, &cfg, &on_tool).await {
        Ok(out) if !out.text.is_empty() => {
            let mut f = Finish::reply(out.text.clone(), "memos");
            let calls = out.records.iter().filter(|r| r.name == "memos").count();
            f.action_result = format!("[memos {calls} call(s)]");
            Some(f)
        }
        Ok(_) | Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_memo_questions() {
        for yes in [
            "Cosa ho detto nel memo vocale di oggi sulle carte?",
            "what did I say in yesterday's memo about the kitchen?",
            "riassumi di nuovo l'ultimo memo",
            "hvad sagde jeg i min lydnote i går?",
            "Show my voice notes from this week",
            "trasforma le azioni del memo in attività",
        ] {
            assert!(is_memo_request(yes), "{yes}");
        }
        for no in [
            "che tempo fa domani?",
            "fammi vedere le foto di Copenaghen",
            "aggiungi il latte alla lista",
            "memorizza che il mio colore preferito è il blu",
            "remember that I like blue",
        ] {
            assert!(!is_memo_request(no), "{no}");
        }
    }
}
