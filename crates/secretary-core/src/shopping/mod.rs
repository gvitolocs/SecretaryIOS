//! shopping: the former `lucina-core` crate (grocery list models, client for
//! the upstream shopping routes, sync op-log, catalog), moved in unchanged.
//!
//! Original crate docs:
//!
//! lucina-core: the client application logic, entirely in Rust.
//!
//! Design goal: Kotlin only drives screens and platform services; every
//! decision about auth, list state, item edits, sync and catalog lookup is
//! made here, so the same core can later be exposed to the Android shell via
//! UniFFI or run headless (CLI/desktop).

pub mod catalog;
pub mod client;
pub mod models;
pub mod sync;

pub use catalog::Catalog;
pub use client::LucinaClient;
pub use models::{ItemOp, LucinaItem, LucinaList, Token};
pub use sync::OpLog;

#[derive(Debug, thiserror::Error)]
pub enum LucinaError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("bad response {status}: {body}")]
    Api { status: u16, body: String },
    #[error("not authenticated")]
    NoToken,
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, LucinaError>;
