//! Lucina on Discord.
//!
//! Two gateway sessions on the same bot token, each with its own process:
//!
//! - the owner presence watcher (`lucina presence-watch`), intents
//!   `GUILDS | GUILD_PRESENCES` only, so it never receives a message and can
//!   never reply;
//! - the message session ([`session`], run by `lucina serve` when
//!   `LUCINA_DISCORD_ENABLED=true`), which replaces the Node Lucina's
//!   discord.js client, behind the flock lease of [`lease`].
//!
//! - [`presence`]: the owner's presence model and the mapping from Discord's
//!   presence payloads (Spotify, games, custom status, client status).
//! - [`engine`]: the gateway-event state machine (pure, no network): keeps
//!   only the owner's presence, merges it across guilds (freshest wins),
//!   writes the current state and transition-only history.
//! - [`store`]: `data/presence/discord.json` (current, atomic, 0600) and
//!   `data/presence/discord.jsonl` (history, retention), plus the readers used
//!   by the server and the `presence` tool (today's online minutes, the
//!   one-line summary).
//! - `watcher` (feature `gateway`): the twilight gateway loop.
//! - [`routing`]: which messages Lucina answers (Node's admin-channel,
//!   allow-list and mention rules), and the 2000-character splitting.
//! - [`lease`], [`cache`], [`rest`] (the REST calls as a trait;
//!   `twilight_rest` implements it), [`session`] (health; the gateway loop
//!   with feature `gateway`).
//!
//! Privacy: every other member's presence is dropped as soon as its user id is
//! read; it is never stored or logged. Invisible is indistinguishable from
//! offline on Discord, and is reported as offline.

pub mod cache;
pub mod engine;
pub mod lease;
pub mod presence;
pub mod rest;
pub mod routing;
pub mod session;
pub mod store;
#[cfg(feature = "gateway")]
pub mod twilight_rest;
#[cfg(feature = "gateway")]
pub mod watcher;

/// For implementors of [`rest::DiscordRest`].
pub use async_trait::async_trait;

pub use presence::{Activity, ClientStatus, PresenceState};
pub use store::{PresenceReport, PresenceStore};
