//! Lucina Agents: the wire types and pure kernels behind `lucina-agentd`, Lucina's
//! Rust rebuild of Paseo (design: `docs/design/agents.md`; parity:
//! `docs/design/agents-parity.md`).
//!
//! No I/O and no async here: the daemon, secretary-server, secretary-mcp and the
//! apps (through secretary-ffi) share these types, so the API can't drift between
//! them.
//!
//! The JSON shapes follow Paseo 0.13.2 (`@getpaseo/protocol`), field for field, so
//! the differential tests can compare the two daemons' output directly. Each
//! module names the Paseo file it mirrors:
//!
//! - [`types`]: agent status, the stored record, snapshots and list items
//!   (`messages.js` `AgentSnapshotPayloadSchema`, `AgentListItemPayloadSchema`).
//! - [`timeline`]: timeline items, tool-call details, rows and cursors
//!   (`messages.js` `AgentTimelineItemPayloadSchema`).
//! - [`events`]: provider stream events (`AgentStreamEventPayloadSchema`).
//! - [`subagent`]: the children a provider runs inside an agent, each with its own
//!   timeline (`server/agent/provider-subagents/store.js`, `ProviderSubagent*` messages).
//! - [`permission`]: permission requests and responses.
//! - [`projection`]: collapsing streamed rows (`server/agent/timeline-projection.js`).
//! - [`display`]: tool-call display names and summaries (`tool-call-display.js`,
//!   `tool-name-normalization.js`, `paseo-tool-presentation/*`).
//! - [`curator`]: the plain-text activity log (`server/agent/activity-curator.js`).
//! - [`title`]: the provisional title from the first prompt (`create-agent-title.js`).
//! - [`bucket`]: state buckets and status priority (`agent-state-bucket.js`).
//! - [`lifecycle`]: the agent state machine (`server/agent/agent-manager.js`),
//!   proven in `verification/verus/agent_lifecycle.rs`.
//! - [`notification`]: attention notification titles and previews
//!   (`protocol/agent-attention-notification.js`).
//! - [`workspace`]: projects, workspaces, diffs and files (Paseo `ProjectRecord`,
//!   `WorkspaceRecord`, `ParsedDiffFileSchema`); [`diffparse`] reads `git diff`;
//!   [`branch_slug`]: `protocol/branch-slug.js`.
//! - Lucina only: [`permission_gate`] (who may answer a permission request, and
//!   until when) and [`run_gate`] (the finished-run gate's verdict), proven in
//!   `verification/verus/permission_gate.rs` and `run_gate.rs`. [`worktree_guard`]: where agents may write
//!   and when a worktree may be removed (`verification/verus/worktree_guard.rs`).
//! - Lucina only: [`external`] (agents another host runs, their owner device and
//!   queued commands) and [`stream`] (event ids and `Last-Event-ID` resume).
//! - Rewind and fork (A16): [`rewind`] (modes, the capability each needs, finding the target
//!   user message; `agent/rewind/rewind.js`) and [`fork_context`] (the chat-history
//!   attachment of a fork; `buildAgentForkContextAttachment`).
//! - Orchestration (phase 6): [`schedule`] (schedules, heartbeats, cron;
//!   `protocol/schedule/*`), [`envelope`] (messages between agents and finish
//!   notices; `agent-messages/envelope.js`, `finish-subscriptions/internal/notification.js`)
//!   and [`text_match`] (typo-tolerant search ranking; `protocol/search/text-match.js`).
//!
//! String lengths and cuts follow JavaScript (UTF-16 code units, JS whitespace;
//! see [`jsstr`]), so truncated texts match Paseo's byte for byte.

#![forbid(unsafe_code)]

pub mod branch_slug;
pub mod bucket;
pub mod curator;
pub mod device;
pub mod diffparse;
pub mod display;
pub mod envelope;
pub mod events;
pub mod external;
pub mod fork_context;
pub mod jsstr;
pub mod lifecycle;
pub mod notification;
pub mod permission;
pub mod permission_gate;
pub mod projection;
pub mod rewind;
pub mod run_gate;
pub mod schedule;
pub mod stream;
pub mod subagent;
pub mod text_match;
pub mod timeline;
pub mod title;
pub mod types;
pub mod workspace;
pub mod worktree_guard;

pub use events::{StreamEvent, Usage};
pub use permission::{PermissionRequest, PermissionResponse};
pub use timeline::{Cursor, TimelineItem, TimelineRow, ToolCallDetail, ToolCallStatus};
pub use types::{AgentListItem, AgentRecord, AgentSnapshot, AgentStatus, AttentionReason};
