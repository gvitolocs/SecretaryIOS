//! Meshy AI 3D for Lucina.
//!
//! Lucina calls Meshy's REST API directly (not Meshy's MCP server): one
//! HTTPS client in the same process, no Node child process, no npm package
//! pulled at start-up, and every credit-spending request passes through
//! Lucina's own plan → confirmation → budget gate before it exists.
//!
//! - [`api`]: the REST client (tasks, polling data, downloads from Meshy's
//!   asset hosts only), [`error`]: status → owner-readable errors.
//! - [`key`]: the API key behind a trait (`MESHY_API_KEY_FILE`, 0600 only).
//! - [`plan`], [`pricing`], [`anim`]: plans with credit costs, the price
//!   table, the animation library (in-place variants).
//! - [`budget`]: daily budget, per-plan cap and a compiled-in hard cap.
//! - [`service`]: confirmed plans as background jobs (job store, timeline,
//!   `/v1/notify`, Telegram), [`gallery`]: secretary-server `/v1/models`,
//!   [`preview`]: CPU-only turntable MP4s, [`telegram`]: the Node bridge.
//! - [`mock`]: a local Meshy for tests and end-to-end runs.

pub mod anim;
pub mod api;
pub mod budget;
pub mod error;
pub mod gallery;
pub mod key;
pub mod mock;
pub mod plan;
pub mod preview;
pub mod pricing;
pub mod service;
pub mod telegram;

pub use api::{MeshyApi, TaskKind};
pub use error::MeshyError;
pub use plan::{Action, Args, Plan};
pub use service::{MeshyConfig, MeshyService, Origin};
