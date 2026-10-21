//! Lucina's file stores, byte-compatible with the Node implementation
//! (`/home/nez/Projects/Lucina/src/{timeline,reminders,secretary-api}.js`):
//!
//! - [`timeline`]: the owner conversation timeline, `data/timeline.jsonl`
//!   (one JSON item per line, ULID ids, rotated `<path>.1`, `<path>.2`).
//! - [`reminders`]: `data/reminders.json` (`{reminders, updatedAt}`) plus the
//!   natural-language reminder parser ([`parser`]) and the formatters.
//! - [`jobs`]: `data/secretary-jobs.json` (`{jobs, updatedAt}`, last 200).
//! - [`scheduler`]: due-reminder delivery loop, **off by default** (one
//!   writer only while the Node Lucina is live).
//!
//! JSON stores are written atomically (temp file + rename) with mode 0600.
//! Never log item or reminder text: it is the owner's private conversation.

pub mod atomic;
pub mod device;
pub mod jobs;
pub mod parser;
pub mod recent;
pub mod reminders;
pub mod scheduler;
pub mod timeline;
pub mod timeutil;
pub mod ulid;

pub use atomic::write_atomic;
pub use jobs::{Job, JobStore};
pub use reminders::{CreateReminder, Reminder, ReminderStore};
pub use timeline::{Attachment, Meta, NewItem, TimelineItem, TimelineStore, TimelineTurn};

/// Store errors.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid: {0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;
