//! Lucina on Discord.
//!
//! Today: the owner presence watcher (`lucina presence-watch`). It opens its
//! own gateway session, separate from the Node Lucina's message session, with
//! intents `GUILDS | GUILD_PRESENCES` only, so it never receives a message and
//! can never reply.
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
//!
//! Privacy: every other member's presence is dropped as soon as its user id is
//! read; it is never stored or logged. Invisible is indistinguishable from
//! offline on Discord, and is reported as offline.

pub mod engine;
pub mod presence;
pub mod store;
#[cfg(feature = "gateway")]
pub mod watcher;

pub use presence::{Activity, ClientStatus, PresenceState};
pub use store::{PresenceReport, PresenceStore};
