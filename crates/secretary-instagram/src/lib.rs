//! `secretary-instagram`: the Instagram viewer plugin's service (docs/instagram.md).
//!
//! The backend is the anonymous StoryView API (`instarust-core` has its URL builders and
//! parsers); this crate adds the HTTP the core leaves to the caller, signed media URLs, a disk
//! cache with a size cap, upstream spacing and backoff, and "save to Lucina".

pub mod archive;
pub mod cache;
pub mod http;
pub mod media;
pub mod recent;
pub mod service;
pub mod store;
pub mod upstream;
pub mod watch;
