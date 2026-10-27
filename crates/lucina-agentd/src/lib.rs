//! lucina-agentd: Lucina's agent supervisor, the Rust rebuild of Paseo's daemon
//! (design: `docs/design/agents.md`; parity: `docs/design/agents-parity.md`).
//!
//! - [`store`]: agents in SQLite, one append-only JSONL timeline per agent.
//! - [`provider`]: the `Provider` / `Session` traits every agent backend
//!   implements; [`mock`] is the port of Paseo's `mock` test provider.
//! - [`manager`]: agents, turns, permissions and the event bus. Every state change
//!   goes through `lucina_agents::lifecycle::step`.
//! - [`api`]: the JSON API on a unix socket (and optionally loopback TCP).
//! - [`client`] and [`cli`]: `lagent`, whose commands and `--json` output mirror
//!   Paseo's CLI so the two can be compared directly.
//!
//! - [`peer`]: who is calling (owner, MCP, or a process an agent started), for
//!   permission answers; [`notify`]: pushes through secretary-server.
//! - [`providers`]: Claude (stream-json), Codex (app-server), the ACP agents
//!   (Cursor, OpenCode, Copilot, custom), Pi (rpc) and ZCode, launched through
//!   [`process`] and [`jsonrpc`]; defaults come from the owner's [`lane`] config.

#![forbid(unsafe_code)]

pub mod api;
pub mod cli;
pub mod client;
pub mod config;
pub mod jsonrpc;
pub mod lane;
pub mod manager;
pub mod mock;
pub mod notify;
pub mod peer;
pub mod process;
pub mod provider;
pub mod providers;
pub mod store;

/// RFC 3339 with milliseconds and `Z`, as JavaScript's `Date.toISOString()`.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
