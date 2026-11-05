//! Memory surfaces for Secretary API requests (`lucina_memory::surface`).
//!
//! Trust chain: the owner's web session / iOS device token, or an MCP client
//! whose Lucina MCP token secretary-mcp checked, reaches secretary-server,
//! which authenticates it and calls this server with `SECRETARY_API_TOKEN`.
//! [`BearerVerified`] is the proof, inside this crate, that a request passed
//! that check in `api::secretary`; the MCP markers
//! ([`lucina_intent::mcp::turn_info`]) then tell an owner delegate from the
//! owner himself.

use lucina_intent::mcp::McpTurn;
use lucina_memory::surface::OwnerIds;
use lucina_memory::{TurnFacts, TurnSurface};

use crate::config::Config;

/// The request passed the Secretary bearer check. Only `api::secretary`
/// builds one, after the constant-time token comparison.
#[derive(Debug, Clone, Copy)]
pub struct BearerVerified(());

impl BearerVerified {
    pub(crate) fn checked() -> Self {
        Self(())
    }
}

/// The owner's ids from config (Alexa: not configured yet).
pub fn owner_ids(cfg: &Config) -> OwnerIds {
    OwnerIds {
        telegram_chat_id: cfg.owner_chat_id.clone(),
        discord_user_id: cfg.owner_discord_user_id.clone(),
        alexa_user_id: None,
    }
}

/// A Secretary chat turn: `channel` is the validated body channel.
pub fn chat_surface(
    cfg: &Config,
    bearer: Option<BearerVerified>,
    channel: &str,
    mcp: Option<&McpTurn>,
) -> TurnSurface {
    // A LucinaVoice speaker is an owner device behind the same bearer as the app: its turns classify like
    // an iOS turn (the memory surface kernel has no `voice` channel, and the timeline channel stays `voice`).
    let channel = if channel == crate::chat::VOICE_CHANNEL {
        "ios"
    } else {
        channel
    };
    let facts = TurnFacts::secretary(channel, bearer.is_some(), mcp.map(|m| m.caller.clone()));
    TurnSurface::classify(&facts, &owner_ids(cfg))
}

/// An authenticated app request that is not a chat turn (the agenda
/// briefing): the owner's own web/iOS client.
pub fn app_surface(cfg: &Config, bearer: BearerVerified) -> TurnSurface {
    chat_surface(cfg, Some(bearer), "web", None)
}
