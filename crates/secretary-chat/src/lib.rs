//! The iOS chat engine's pure kernel: the timeline merge and order ([`merge`]), the sync cursor loop
//! ([`sync`]) and the pagination rules ([`page`]), behind one JSON entry point ([`handle`]) that
//! `secretary-ffi` exports as `secretary_chat_request`.
//!
//! The SwiftUI views and the store stay in Swift. They pass small key records, never whole messages. The
//! rules are proved in `verification/verus/chat_merge.rs` (M1-M4 for the merge, C1-C3 for the cursor loop).
//!
//! A merge of 255 current and 200 incoming keys takes about 0.05 ms in a release build (`cargo run --release -p secretary-chat --example merge_speed`).
pub mod merge;
pub mod page;
pub mod request;
pub mod sync;

pub use request::handle;
