//! The owner's Instagram and Pixiv plugins in the chat.
//!
//! The JSON planner (the live default) does not know these tools, so a clear
//! request ("show me @someone's latest stories", "find pixiv art of Frieren",
//! "save it to the gallery" right after a result) runs a short tool loop with
//! only the five plugin tools ([`lucina_tools::tools::plugins`]).
//!
//! Owner only, strictly: the turn must be the owner's own Secretary app chat.
//! Guests on Discord or Telegram (and the owner there, where a reply could
//! land in a public channel), Lucina MCP callers, and guarded turns never run
//! it, so the planner answers them as if the tools did not exist. Nothing of
//! the exchange goes to Honcho (`PRIVATE_ACTION`).

use std::sync::Arc;

use lucina_llm::Message;
use lucina_store::timeline::{Meta, NewItem};
use lucina_tools::agent::{self, AgentConfig, ToolRecord};
use lucina_tools::tools::plugins as p;
use lucina_tools::{Ctx, Effect, Origin, Registry};

use super::{gallery_action, Finish, Turn, PRIVATE_ACTION, SECRETARY_EXTRA_SYSTEM};
use crate::api::PLATFORM_LABEL;
use crate::prompts::Prompts;

const GUIDE: &str = "Giuseppe's private Instagram and Pixiv plugins are your tools here: instagram_profile and \
instagram_stories for public accounts (a username, with or without @), pixiv_search and pixiv_ranking for safe artwork \
only (there is no adult mode and you never offer one), and save_to_gallery to keep Instagram items or a Pixiv work in \
his Gallery (pass the item `url` exactly as given, all=true for everything just shown, or the Pixiv work `id`). \
His Instagram watchlist: instagram_watchlist lists it, instagram_new says what is new on one account (\"any new stories from X?\"), instagram_watch and instagram_unwatch change it, and instagram_save_latest keeps an account's newest post or story (\"save X's latest post\"). \
Report only what the tools returned; never invent accounts, posts or works. Give 3 to 6 of the most relevant items as \
plain links (the `url` or `thumbUrl` of each, with the title or time), and always write a Pixiv work's id next to its \
title so a later 'save it' can find it. Stories that are not there are normal: say so. If a tool says it is not \
available, say that plainly and stop. Keep the answer short, in the language of the message, and tell him the plugin \
page in the app (the button under your reply) shows everything.";

/// Whole words of `text`, lower-cased.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.' || c == '@'))
        .map(|w| w.trim_matches('.').to_owned())
        .filter(|w| !w.is_empty())
        .collect()
}

/// A clear plugin request. `recent`: a plugin tool answered in this chat in the last 30 minutes.
pub fn is_plugin_request(text: &str, recent: bool) -> bool {
    let w = words(text);
    let has = |list: &[&str]| w.iter().any(|x| list.contains(&x.as_str()));
    if has(&["instagram", "insta", "pixiv"]) {
        return true;
    }
    let handle = w.iter().any(|x| x.len() > 2 && x.starts_with('@'));
    let stories = has(&[
        "stories",
        "story",
        "storie",
        "storia",
        "historier",
        "historie",
    ]);
    if stories && (handle || recent) {
        return true;
    }
    // The watchlist: "my watchlist", "unwatch @x", "watch @x", "latest post of @x".
    if has(&["watchlist", "watch-list", "unwatch", "unwatched"]) {
        return true;
    }
    let watch = has(&[
        "watch", "watching", "segui", "smetti", "osserva", "følg", "folg",
    ]);
    let post = has(&[
        "post", "posts", "latest", "ultimo", "ultima", "ultimi", "nyeste", "seneste",
    ]);
    if (watch || post) && handle {
        return true;
    }
    // "save it to the gallery" after a result.
    let save = has(&[
        "save", "salva", "salvalo", "salvala", "salvali", "salvale", "salvate", "keep", "gem",
        "gemme", "tieni", "aggiungi", "add", "metti", "mettilo", "mettila", "mettili",
    ]);
    let gallery = has(&[
        "gallery",
        "galleria",
        "galleri",
        "galleriet",
        "library",
        "libreria",
    ]) || text.to_lowercase().contains("to my photos")
        || text.to_lowercase().contains("nelle foto");
    recent && save && gallery
}

/// The owner's own Secretary app chat: no guest, no channel, no MCP caller.
fn is_owner_app_turn(t: &Turn<'_>) -> bool {
    !t.guarded && t.origin.is_none() && t.mcp.is_none() && t.surface.is_owner()
}

pub(super) async fn try_turn(t: &Turn<'_>, memory: &str) -> Option<Finish> {
    let st = t.st;
    if !is_owner_app_turn(t) {
        return None;
    }
    let recent = st.tool_env.plugins.recently_used(&t.session);
    if !is_plugin_request(&t.message, recent) {
        return None;
    }
    let mut reg = Registry::new();
    for tool in [
        Arc::new(p::InstagramProfileTool) as Arc<dyn lucina_tools::Tool>,
        Arc::new(p::InstagramStoriesTool),
        Arc::new(p::InstagramWatchlistTool),
        Arc::new(p::InstagramWatchTool),
        Arc::new(p::InstagramUnwatchTool),
        Arc::new(p::InstagramNewTool),
        Arc::new(p::InstagramSaveLatestTool),
        Arc::new(p::PixivSearchTool),
        Arc::new(p::PixivRankingTool),
        Arc::new(p::SaveToGalleryTool),
    ] {
        reg.register(tool).ok()?;
    }
    let ctx = Ctx::new(st.tool_env.clone(), &t.channel, &t.session).with_origin(Origin::Owner);
    let extra = format!("{SECRETARY_EXTRA_SYSTEM} {GUIDE}");
    let system = st.planner.prompts.agent_system(
        PLATFORM_LABEL,
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
        max_tool_output_chars: 12_000,
        ..AgentConfig::default()
    };
    let timeline = st.timeline_for(&t.session);
    let channel = t.channel.clone();
    // The tools' summaries carry no username, query or id.
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
            let mut f = Finish::reply(out.text.clone(), PRIVATE_ACTION);
            let calls = out.records.len();
            // No names or ids in the memo of the turn either.
            f.action_result = format!("[plugins {calls} call(s)]");
            let lang = crate::rich::guess_language(&out.text, &t.message);
            if let Some((path, count)) = ctx.effects().iter().rev().find_map(|e| match e {
                Effect::Gallery { path, count } => Some((path.clone(), *count)),
                _ => None,
            }) {
                f.routes.push(gallery_action(&path, count, lang));
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
    fn recognizes_plugin_requests() {
        for yes in [
            "show me the latest instagram stories of natgeo",
            "fammi vedere le storie instagram di @natgeo",
            "what are @natgeo's latest stories?",
            "find pixiv art of Frieren",
            "cerca su Pixiv illustrazioni di Hatsune Miku",
            "vis mig dagens pixiv ranking",
            "check insta for anything new",
            "who is on my instagram watchlist?",
            "unwatch @natgeo",
            "watch @natgeo and save everything it posts",
            "save @natgeo's latest post",
            "any new stories from @natgeo?",
        ] {
            assert!(is_plugin_request(yes, false), "{yes}");
        }
        for no in [
            "che tempo fa domani?",
            "tell me stories about dragons",
            "save it to the gallery",
            "what is the plot of the story?",
            "fammi vedere le foto di Copenaghen",
            "aggiungi il latte alla lista",
            "watch the match tonight",
            "email me the latest post from the team",
        ] {
            assert!(!is_plugin_request(no, false), "{no}");
        }
    }

    #[test]
    fn follow_ups_need_a_recent_plugin_result() {
        for yes in [
            "save it to the gallery",
            "salvalo nella galleria",
            "salva tutte le storie nella galleria",
            "gem dem i galleriet",
            "show me their stories",
        ] {
            assert!(is_plugin_request(yes, true), "{yes}");
            assert!(
                !is_plugin_request(yes, false) || yes.contains("insta"),
                "{yes}"
            );
        }
        assert!(!is_plugin_request("che tempo fa domani?", true));
        assert!(!is_plugin_request("aggiungi il latte alla lista", true));
    }
}
