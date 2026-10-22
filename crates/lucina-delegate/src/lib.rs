//! Lucina delegates work to other AI agents on her own: **Claude** (the
//! Claude Code CLI on nezopt), **Grok** (the Grok Bot desktop session through
//! `gbot`, or the xAI API) and **ChatGPT** (the OpenAI API, or the Codex CLI
//! signed in with the owner's ChatGPT account).
//!
//! The owner's policy (2026-10-22): "make Lucina act and be judgemental on
//! herself". There is no owner confirmation per delegation. Before every
//! delegated call Lucina runs a short **self-review** ([`judge`]): a structured
//! judgement `{goal, risk, reversible, why, decision: act|skip|revise}`. She
//! acts on her own decision and records it on the timeline card ("I did X
//! because Y"), with an undo command where one exists ([`card`]).
//!
//! What does not move (security, not autonomy; [`policy`]):
//!
//! 1. Secrets and secret refs never reach a delegated model: every outbound
//!    prompt goes through [`policy::sanitize_outbound`] (the process-wide
//!    redactor plus `secret://` removal).
//! 2. No purchases and no money movement ([`policy::money_intent`]).
//! 3. Delegation depth is at most 1 and turns that come from Lucina MCP (or
//!    from a delegated agent) can never start a delegation: outside agents
//!    calling Lucina must not be able to drive her. Never back to the caller.
//! 4. Delegated code runs only in a sandbox ([`sandbox`]): a fresh directory
//!    under `~/scratch/lucina-delegate/<job>/`, isolated with bubblewrap (no
//!    other repos, no SSH keys, no systemd or docker); repo work is a clone
//!    on branch `lucina/<job>`, never `main`, never pushed or deployed.
//!
//! Budgets ([`state`]) are a silent runaway guard, not an approval gate:
//! generous daily caps; hitting one pauses delegation for the day and tells
//! the owner once. The owner can switch agents off and on ("non usare Grok",
//! "stop delegating"); that is persisted.
//!
//! Every delegation is a background job ([`engine::Delegator`]): the caller
//! may wait a few seconds for a quick answer, otherwise the answer arrives
//! as a timeline card plus an app notification. Long answers are saved as a
//! note. The audit log ([`audit`]) records agent, kind, duration, outcome and
//! budget used, never prompt contents.

pub mod agents;
pub mod audit;
pub mod card;
pub mod config;
pub mod engine;
pub mod intent;
pub mod judge;
pub mod policy;
pub mod proactive;
pub mod sandbox;
pub mod state;
pub mod types;

pub use config::DelegateConfig;
pub use engine::{Delegator, Sink, StartOutcome, Started};
pub use types::{AgentId, JobRecord, JobStatus, Judgement, Kind, Origin, Request};
