//! Lucina's deterministic turn routing, ported from the Node Lucina so the
//! Rust server decides like the live one:
//!
//! - [`heuristics`]: `src/intent-heuristics.js` (IT/EN/DA regexes);
//! - [`tier0`]: `src/lucina-intent.js` tier 0 (no LLM), direct plans and
//!   the full-plan rules; [`policy`]: `src/context-policy.js`;
//! - [`grocery`]: `src/grocery/intent.js` (it/en/da grocery intents, store
//!   context);
//! - [`outfit`], [`notes`], [`calendar`]: the parsers of the Secretary chat
//!   bridges (`src/avatar-outfit.js`, `src/notes-bridge.js`,
//!   `src/calendar-bridge.js`);
//! - [`tasks`]: the tasks bridge (`src/tasks-bridge.js`);
//! - [`mcp`]: Lucina MCP turns (prefix, caller, allowlists).
//!
//! Patterns are copied verbatim and run through [`jsre`], which gives them
//! V8 semantics (ASCII `\b`, `\d`, `\w`).

pub mod jsre;

pub mod calendar;
pub mod calendar_event;
pub mod computer_route;
pub mod domains;
pub mod game;
pub mod grocery;
pub mod heuristics;
pub mod hints;
pub mod lang;
pub mod mcp;
pub mod name_match;
pub mod notes;
pub mod outfit;
pub mod policy;
pub mod presence;
pub mod reach;
pub mod tasks;
pub mod tier0;
