//! Rust core of the Lucina OTP app (`docs/lucina-android.md`).
//!
//! Built as `liblucina_android.so` and called by the thin Kotlin shell of
//! `tech.getsecretary.lucinaotp` through a few JNI functions that take
//! strings and return JSON strings ([`jni_api`]). All logic lives in the pure
//! Rust [`Bridge`], which the host tests drive directly.
//!
//! - [`bridge`]: parse first, dedup, forward, queue and status.
//! - [`store`]: AES-256-GCM-encrypted `state.bin` and `queue.bin`, atomic
//!   0600 writes in a 0700 directory.
//! - [`net`]: one protocol-2 `POST /incoming` over rustls ([`tls`]: a
//!   client-only graviola provider),
//!   HTTPS to `getsecretary.tech` only, and the status-code mapping.
//!
//! Codes, tokens and message bodies are never logged or returned; secrets
//! are wiped from memory when dropped.

pub mod bridge;
pub mod jni_api;
pub mod net;
pub mod store;
pub mod tls;

pub use bridge::{Bridge, FlushReport, Forward, Probe, Report, SelfTest, Status};

#[cfg(test)]
mod tests;
