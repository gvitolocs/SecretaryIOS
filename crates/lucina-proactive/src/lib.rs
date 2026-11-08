//! Lucina's proactive engine: "she reaches out before you ask", but only
//! when it is truly useful.
//!
//! One loop, every few minutes:
//!
//! ```text
//! sources ──observe──▶ events ──dedup──▶ candidates ──score──▶ decide
//!   (calendar, reminders, notes,        (Postgres:      drop │ digest │ send
//!    timeline, health; presence and      lucina_proactive)      │        │
//!    device are context)                                       Pulse   attention gate
//!                                                                      (quiet hours, presence,
//!                                                                       cooldown, daily budget)
//!                                                                         │
//!                                                     compose (DeepSeek, persona) ─▶ send
//!                                                     (app timeline + push, Telegram) ─▶ record
//! ```
//!
//! - [`score`]: deterministic weights and thresholds (no model); every
//!   decision carries its reason, which the owner can read on tap.
//! - [`attention`]: quiet hours and DND (from secretary-server), Discord
//!   presence (no interruptions while gaming or on DND), per-kind cooldowns,
//!   "Not now" and "Stop this kind" feedback, the daily budget.
//! - [`pulse`]: the Daily Pulse, silent when nothing is new.
//! - [`compose`]: DeepSeek writes the final text only for what passed the
//!   gate (and judges a few open threads); templates when it is unavailable.
//! - [`channels`]: the Secretary app (timeline item + `POST /v1/notify`) and
//!   Telegram (Bot API `sendMessage` with a deep link). Dry-run records what
//!   would have been sent and touches nothing outside the engine's own store.
//!
//! Safety: outreach never takes an external action. It informs, or prepares
//! a draft that is stored as a note.

pub mod attention;
pub mod channels;
pub mod compose;
pub mod config;
pub mod engine;
pub mod envfile;
pub mod lang;
pub mod model;
pub mod pulse;
pub mod replay;
pub mod score;
pub mod secretary;
pub mod setup;
pub mod sources;
pub mod store;
pub mod text;

pub use config::Config;
pub use engine::Engine;
pub use model::{Candidate, Decision, Event, Kind, Outreach, Priority, Scores};
pub use store::{MemStore, PgStore, Store};
