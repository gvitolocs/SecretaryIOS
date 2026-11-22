//! A2UI v1.0 core for Lucina: the agent-to-user-interface messages, their validation, the data
//! model components bind to, and the catalogue functions.
//!
//! This is a Rust port of A2UI (<https://github.com/a2ui-project/a2ui>): the behaviour is the
//! original's, the interfaces are ours. The port record — the adopt / adapt / reference / reject
//! verdicts and the state of the steps — is `docs/design/a2ui-port.md`.
//!
//! The oracle is the original's own conformance suite, its catalogues and its v1.0 specification,
//! vendored unmodified under `third_party/a2ui/upstream/` at the commit named by
//! [`UPSTREAM_COMMIT`]. `crates/lucina-a2ui/src/vendored_tree.rs` checks that copy against its
//! manifest; a file under `upstream/` is never edited.

/// The upstream commit we are based on: the vendored oracle and `docs/upstreams.toml` name the
/// same one.
pub const UPSTREAM_COMMIT: &str = "5ff1b05f02d39f969b52a942e78e864ef3ac01fa";

/// The A2UI protocol version this crate implements.
pub const PROTOCOL_VERSION: &str = "1.0";

// The merge gate runs `cargo test --lib` for a touched crate and no integration tests, so the
// checks on the vendored oracle are a module of the library, not a file under tests/.
#[cfg(test)]
mod vendored_tree;
