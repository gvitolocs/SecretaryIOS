//! The owner's Pixiv plugin (Plugins -> Pixiv): the HTTP around the `pixiv` core
//! (`/home/nez/Projects/ReverseEngineering/pixiv-rust`), signed image URLs for i.pximg.net, a
//! disk cache with a size cap, local bookmarks and save-to-gallery (lucina-media ingest).
//! Owner-only: p1 gates `/v1/pixiv/*` with `/v1/auth/owner` and adds the gateway header.

pub mod backend;
pub mod cache;
pub mod error;
pub mod http;
pub mod images;
pub mod model;
pub mod service;
pub mod store;
pub mod upstream;
