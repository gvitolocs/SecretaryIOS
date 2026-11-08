//! An Obsidian vault as plain files, for every Lucina platform: Juno links it
//! directly, the Secretary apps through `secretary-ffi` and JNI.
//!
//! Real Obsidian is the source of truth (docs/porting-guidelines.md): `tests/obsidian.rs`
//! holds every rule here against what Obsidian itself decided on a fixture and on a
//! copy of a real vault (`tools/vault-oracle`). The vault stays Markdown files and
//! nothing else: no database, no index or state inside it. A vault opens read-only
//! unless asked otherwise, and `write` says what a write may and may not do.
pub mod graph;
pub mod markdown;
pub mod protocol;
pub mod resolve;
pub mod scan;
pub mod search;
pub mod settings;
pub mod vault;
pub mod write;

pub use vault::{Options, Vault};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("this vault is open read-only")]
    ReadOnly,
    #[error("not a note in this vault: {0}")]
    NotFound(String),
    /// The file is not what the caller last read. Nothing was written.
    #[error("{path} changed since it was read")]
    Conflict { path: String, current: String },
    #[error("{0} already exists")]
    Exists(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
