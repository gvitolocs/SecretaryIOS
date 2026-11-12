//! secretary-core: the Secretary domain in Rust.
//!
//! - [`models`]: serde types for Lucina's `/api/secretary/*` contract (chat,
//!   jobs, reminders, agenda, shopping).
//! - [`LucinaClient`]: typed client for that contract; unwraps the
//!   `{ok, data}` envelope and classifies failures into [`Error`].
//! - [`shopping`]: the former `lucina-core` crate (grocery models, upstream
//!   shopping client, sync op-log, catalog).
//!
//! Product logic lives here or in Lucina — never in the iOS shell, and the
//! server only authenticates, validates and forwards.

pub mod channel;
pub mod chat_stream;
pub mod client;
pub mod error;
pub mod models;
pub mod shopping;
pub mod validate;

pub use chat_stream::{ChatEvent, ChatStream, RouteLayer};
pub use client::{Call, LucinaClient, TimeoutClass, Timeouts};
pub use error::{Error, Result};
