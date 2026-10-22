//! Parity tooling against the live Node Lucina (port plan §4):
//!
//! - [`parsers`]: the deterministic layer on a phrase corpus vs the Node
//!   parsers (`lucina parity-parsers`);
//! - [`http`]: the same requests against a Node reference and a Rust
//!   instance (`lucina parity`), with [`fake_secretary`] standing in for
//!   secretary-server;
//! - [`envgen`]: env files from the live Node container (`lucina env-from-node`).

pub mod envgen;
pub mod fake_secretary;
pub mod http;
pub mod parsers;
