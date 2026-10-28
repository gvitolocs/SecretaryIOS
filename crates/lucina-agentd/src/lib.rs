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
//! - [`workspaces`]: projects, workspaces and worktrees (through `wt`), `paseo.json`
//!   setup, diffs, the file explorer and checkout operations.
//! - [`journal`]: the numbered event bus behind the resumable `/stream`;
//!   [`external`]: agents another host runs (Secretary Code sessions) and the
//!   commands queued for their owner device.
//! - [`peer`]: who is calling (owner, MCP, or a process an agent started), for
//!   permission answers; [`notify`]: pushes through secretary-server.
//! - [`approvals_bridge`]: files permission requests as owner approvals and answers them from
//!   verified grants.
//! - [`providers`]: Claude (stream-json), Codex (app-server), the ACP agents
//!   (Cursor, OpenCode, Copilot, custom), Pi (rpc) and ZCode, launched through
//!   [`process`] and [`jsonrpc`]; defaults come from the owner's [`lane`] config.
//! - Phase 6: [`schedules`] (cron and run bookkeeping; the scheduler is in
//!   [`manager`]), and [`mcp`]: `lagent mcp`, the tools agents use to start subagents,
//!   message them, set heartbeats and search.

#![forbid(unsafe_code)]

pub mod api;
pub mod approvals_bridge;
pub mod cli;
pub mod client;
pub mod config;
pub mod external;
pub mod journal;
pub mod jsonrpc;
pub mod lane;
pub mod manager;
pub mod mcp;
pub mod mock;
pub mod notify;
pub mod peer;
pub mod process;
pub mod provider;
pub mod providers;
pub mod schedules;
pub mod store;
pub mod workspaces;

/// RFC 3339 with milliseconds and `Z`, as JavaScript's `Date.toISOString()`.
pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
