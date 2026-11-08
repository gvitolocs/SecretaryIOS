//! Owner approvals for Lucina (contract v1: `approvals-contract.md`).
//!
//! When Lucina needs the owner's permission she asks secretary-server
//! (`POST /v1/approvals` with her service token), the owner answers on his
//! main device (Face ID for sensitive actions), and she proceeds only with a
//! **grant**: an Ed25519-signed token she verifies against a pinned public key
//! (signature, `exp`, action, payload hash) and uses once.
//!
//! - [`Approvals`]: client + verifier + single-use registry; [`Approvals::ask`]
//!   creates (or reuses a pending) request and waits.
//! - [`root`]: `root_exec`, the plan of argv arrays that runs through
//!   `nezopt-sudo` only after an approval of exactly those commands.
//! - Users: the secrets guard (`secret.reveal`, lucina-server), the
//!   `root_exec` tool (lucina-tools) and the `lucina-root` binary.

pub mod canon;
pub mod client;
pub mod config;
pub mod envfile;
pub mod flow;
pub mod grant;
pub mod root;
pub mod used;

#[cfg(feature = "testing")]
pub mod testing;

pub use canon::{canonical_json, payload_hash, plan_hash, shell_line, shell_quote};
pub use client::{NewApproval, RequesterInfo};
pub use config::ApprovalsConfig;
pub use flow::{Approvals, Begun, Outcome};
pub use grant::{Claims, GrantError, PubKey};
pub use root::{RootOutcome, RootReport, StepResult, Who};

/// Everything that can go wrong asking for or using an approval.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApprovalError {
    #[error("approvals are not configured: {0}")]
    NotConfigured(String),
    #[error("secretary-server unreachable or answered badly: {0}")]
    Http(String),
    #[error("secretary-server refused the request (HTTP {status} {code})")]
    Server { status: u16, code: String },
    #[error("the approval is unknown to secretary-server")]
    NotFound,
    #[error("the approval was approved but no grant came with it")]
    NoGrant,
    #[error("{0}")]
    Grant(#[from] GrantError),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Io(String),
}

/// Current unix time in seconds.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
