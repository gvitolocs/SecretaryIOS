//! Notes and Lucina Keep: briefings, notes, documents, checklists and
//! attachments the owner reads in the app and Lucina recalls by name
//! (docs/server-api.md "Notes", docs/keep.md for the design).
//!
//! - `model`: [`Note`] and its parts, request bodies, validation.
//! - `store`: [`NoteStore`], the cached files, revisions, trash, batch, `changes`.
//! - `labels`: the label registry (labels are the notes' `tags`).
//! - `attachments`: content-addressed files.
//! - `import`: the store API of the Google Keep importer.
//! - `seed`: seed files for `secretary-server notes import`.
//! - `routes`: the HTTP routes.
//!
//! Content is markdown, at most [`MAX_CONTENT_BYTES`]. Each note is a JSON
//! file in `$SECRETARY_DATA_DIR/notes/` (trash: `notes/trash/`), written
//! atomically with mode 0600. Who may call: a device (Bearer, or the cookie
//! plus `X-Secretary-CSRF: 1` for writes) or Lucina with the service token.
//! Every write is logged with the caller (never the content).

use std::path::Path;
use std::sync::Arc;

use crate::devices::ensure_dir;

mod attachments;
mod import;
mod labels;
mod model;
mod routes;
mod seed;
mod store;

pub use attachments::{ext_for_mime, mime_for_ext, split_file_name, valid_sha};
pub use import::{ImportOutcome, ImportedNote};
pub use labels::LabelOut;
pub use model::*;
pub use routes::routes;
pub use seed::{parse_seed, Seed};
pub use store::{same_instant, BatchOutcome, Caller, Changes, NoteStore};

/// Shared handle for [`crate::Inner`].
pub fn store(data_dir: &Path) -> Arc<NoteStore> {
    let s = NoteStore::new(data_dir);
    if let Err(e) = ensure_dir(s.dir()) {
        tracing::warn!(dir = %s.dir().display(), error = %e, "notes dir not created yet");
    }
    Arc::new(s)
}

#[cfg(test)]
mod keep_tests;
#[cfg(test)]
mod tests;
