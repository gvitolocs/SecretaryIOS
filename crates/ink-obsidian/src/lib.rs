//! Read-only Obsidian vault adapter for Lucina Ink: walk a vault safely, parse
//! titles, tags and links, and find references to library books. Nothing here
//! writes to a vault; `render` only plans generated book notes.
pub mod books;
pub mod config;
pub mod index;
pub mod parse;
pub mod render;
pub mod resolve;
pub mod walk;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

/// Body of `POST /v1/library/notes-index`: every reference found in one vault.
/// The server replaces that vault's previous references.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NotesIndexBatch {
    pub vault: String,
    pub generated_at: String,
    pub refs: Vec<books::BookRef>,
}
