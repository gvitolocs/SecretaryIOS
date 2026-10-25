//! The owner's photo library in the chat.
//!
//! The JSON planner (the live default) does not know the `media` tool, so a
//! request about his photos, videos or albums ("fammi vedere le foto di
//! Copenaghen dell'estate scorsa", "find the videos from my birthday") runs a
//! short tool loop with only the `media` tool. What Lucina finds lands in the
//! "Lucina's picks" album and the reply gets a button that opens it in the
//! Gallery. MCP callers may search too (a token is the trust boundary).

use std::sync::Arc;

use lucina_intent::mcp;
use lucina_llm::Message;
use lucina_store::timeline::{Meta, NewItem};
use lucina_tools::agent::{self, AgentConfig, ToolRecord};
use lucina_tools::tools::media::MediaTool;
use lucina_tools::{Ctx, Effect, Origin, Registry};

use super::{gallery_action, Finish, Turn, SECRETARY_EXTRA_SYSTEM};
use crate::api::PLATFORM_LABEL;
use crate::prompts::Prompts;

const GUIDE: &str = "Giuseppe's photos, videos and audio are in his own library (the Gallery tab), reachable with the media tool. \
For a request about them call media with action=search: turn relative dates into from/to (YYYY-MM or YYYY-MM-DD; \"last summer\" = June to August of last year, \
\"this summer\" = this year's), and places into near with the coordinates you know (city: km 20-30; country: query with its name). \
Then answer in one or two short sentences: how many you found, when and where, and that the button below shows them. \
If nothing matches, say so and suggest a wider search. Never list item ids. If he is not asking about his library, answer without the tool.";

/// A request about the owner's own photos, videos or albums.
pub fn is_media_request(text: &str) -> bool {
    let t = format!(" {} ", text.to_lowercase());
    let has = |w: &[&str]| w.iter().any(|x| t.contains(x));
    let noun = has(&[
        " foto",
        "fotografi",
        " photo",
        "picture",
        " pics",
        " pic ",
        "selfie",
        "screenshot",
        "billed",
        " video",
        "filmat",
        "filmini",
        "gallery",
        "galleria",
        "galleri ",
        " album",
        "my films",
        "miei film",
        "mine film",
        "registrazion",
        "recordings",
        "media library",
        "mediateca",
        "medietek",
    ]);
    if !noun {
        return false;
    }
    // Making pictures is the image tool's job, not the library's.
    let making = has(&[
        "genera ",
        "generate",
        "crea un",
        "create a",
        "create an",
        "disegna",
        "draw ",
        "fammi un'immagine",
        "make me a",
        "make a ",
        "lav et billede",
        "modello 3d",
        "3d model",
    ]);
    !making
}

pub(super) async fn try_turn(t: &Turn<'_>, memory: &str) -> Option<Finish> {
    if !is_media_request(&t.message) {
        return None;
    }
    let st = t.st;
    let mut reg = Registry::new();
    reg.register(Arc::new(MediaTool)).ok()?;
    let origin = if t.mcp.is_some() {
        Origin::Mcp
    } else {
        Origin::Owner
    };
    let ctx = Ctx::new(st.tool_env.clone(), &t.channel, &t.session).with_origin(origin);
    let mcp_note = t.mcp.as_ref().map(mcp::note).unwrap_or_default();
    let extra = format!("{SECRETARY_EXTRA_SYSTEM} {GUIDE} {mcp_note}");
    let now = chrono::Utc::now();
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
            let mut f = Finish::reply(out.text.clone(), "media");
            let found = out.records.iter().filter(|r| r.name == "media").count();
            f.action_result = format!("[media {found} call(s)]");
            if let Some((path, count)) = ctx.effects().iter().rev().find_map(|e| match e {
                Effect::Gallery { path, count } => Some((path.clone(), *count)),
                _ => None,
            }) {
                f.routes.push(gallery_action(&path, count));
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
    fn recognizes_library_requests() {
        for yes in [
            "Fammi vedere le foto di Copenaghen dell'estate scorsa",
            "show me the photos from Copenhagen last summer",
            "which Pokémon films do I have in my media library?",
            "Find the videos from my birthday",
            "vis mine billeder fra Rom",
            "metti queste foto in un album",
            "Quante foto ho del 2024?",
        ] {
            assert!(is_media_request(yes), "{yes}");
        }
        for no in [
            "che tempo fa domani?",
            "genera un'immagine di un gatto con la foto di sfondo",
            "make a 3D model of my photo",
            "aggiungi il latte alla lista",
            "draw me a picture of a dragon",
        ] {
            assert!(!is_media_request(no), "{no}");
        }
    }
}
