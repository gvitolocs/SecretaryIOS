//! Local-first synchronisation core for Lucina Ink (reader devices ↔ Secretary Library).
//!
//! - [`op`]: the wire ops. Every op has a client-generated id `device:counter`
//!   (a monotonic counter per device) and a device timestamp `at`.
//! - [`outbox`]: the device side. Ops are appended durably (JSONL + fsync)
//!   before any network I/O; acks are appended too, so a restart never
//!   re-sends an acked op and never loses an unacked one. A torn last line is
//!   truncated on open.
//! - [`state`]: the server side. [`state::apply`] is idempotent by op id and
//!   commutative for every op kind (the registers are last-writer-wins by
//!   [`op::Stamp`], grow-only sets or min/max registers), so two servers that
//!   receive the same ops in different orders hold the same content.
//!   [`state::project`] writes the registers into the library records.
//! - [`client`]: the pure client sync loop (batches, acks, bounded exponential
//!   backoff with jitter).
//! - [`transport`]: the IO shell: the [`transport::Transport`] trait, the pure
//!   request handler, an in-memory server and a flaky transport for tests.
//!
//! Formal models: `verification/models/src/ink_sync.rs` (Stateright),
//! `formal/inksync/InkSync.tla` (TLC), `verification/verus/library_merge.rs`
//! (Verus). See `docs/verification.md` §6.
pub mod client;
pub mod op;
pub mod outbox;
pub mod state;
pub mod transport;
pub mod watermark;

pub use op::{Op, OpId, OpKind, Stamp};
pub use state::{apply, apply_mut, Outcome, SyncState, Verdict};
