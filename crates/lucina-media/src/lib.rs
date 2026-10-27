//! Lucina's media library: the owner's photos, videos, audio and files on nezopt.
//!
//! - Originals are content-addressed (blake3) on the big disk (`store_dir`, mybook).
//!   Uploads that arrive in the quiet hours wait in an NVMe spool and move in the day window.
//! - The catalog (SQLite + FTS5), thumbnails and posters live on NVMe (`data_dir`),
//!   so browsing never wakes the big disk.
//! - Nothing here ever deletes a file on a phone. A server delete is a soft delete (trash).
//!
//! - The Drive (`drive.rs`) indexes every file of the Drive folders in place, with its folder
//!   hierarchy, type and text, for browsing, previews and search.
//!
//! The HTTP API is reached only through the p1 secretary-server proxy, which authenticates the
//! device and adds the upstream key (see [`http::upstream_guard`]).

pub mod archive;
pub mod config;
pub mod db;
pub mod doctype;
pub mod drive;
pub mod drive_kernel;
pub mod extract;
pub mod geo;
pub mod highlight;
pub mod http;
pub mod import;
pub mod ingest;
pub mod meta;
pub mod protocol;
pub mod share;
pub mod share_http;
pub mod source;
pub mod store;
pub mod thumbs;

pub use config::Config;
pub use db::Db;
