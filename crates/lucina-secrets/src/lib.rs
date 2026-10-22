//! Lucina's secrets skill.
//!
//! Lucina reasons with an external model (DeepSeek). That model never sees a
//! secret value:
//!
//! - Tools and config name secrets by **reference**,
//!   `secret://<project>/<env>/<path>#<KEY>` ([`SecretRef`]). A tool resolves
//!   the value through [`SecretStore::resolve`] right before the outbound
//!   call (an HTTP header, a child's environment or stdin) and never returns
//!   it: [`SecretValue`] has no `Display`, no `Serialize`, a redacted `Debug`,
//!   and zeroizes on drop.
//! - There is **no reveal path**: no tool or intent returns a value. Reveal
//!   requests get a fixed refusal ([`reveal`]), decided without the model.
//!   Lucina may say which secrets exist, whether they are set, when they were
//!   last rotated and who used them ([`SecretStore::status`],
//!   [`SecretStore::audit_entries`]).
//! - **One redactor** ([`Redactor`], process-wide through [`redactor()`])
//!   sits in front of every sink: model prompts and replies, API replies, the
//!   timeline, Honcho, logs, notifications. It masks every known secret value
//!   (raw, base64 at any alignment, URL- and JSON-escaped) as
//!   `[secret:<NAME>]` and keeps only salted digests of them, never the
//!   values; a detector masks token shapes (`sk-`, `ghp_`, `mcp_`, `sec_`,
//!   `svc_`, JWTs, PEM blocks, `password=…`, high-entropy strings).
//! - Values live in memory only: a short TTL cache ([`cache`], 10 minutes by
//!   default), zeroized on eviction; never written to disk.
//! - Least privilege: one Infisical folder (read-only machine identity) and
//!   the same scope enforced here ([`policy`]); login, sudo and root
//!   passwords are refused by name ([`policy::HARD_DENY`]).
//! - Every resolve is audited (name, tool, time, outcome; never the value),
//!   see [`audit`].
//!
//! The `client` feature (default) adds the Infisical client, the store, the
//! TOML config and the tracing writer. Storage crates use the redactor only
//! (`default-features = false`).

pub mod cache;
pub mod policy;
pub mod redact;
pub mod reference;
pub mod reveal;
pub mod value;

#[cfg(feature = "client")]
pub mod audit;
#[cfg(feature = "client")]
pub mod config;
#[cfg(feature = "client")]
pub mod infisical;
#[cfg(feature = "client")]
pub mod log;
#[cfg(feature = "client")]
pub mod outbound;
#[cfg(feature = "client")]
pub mod status;
#[cfg(feature = "client")]
pub mod store;

pub use redact::{redactor, Redactor};
pub use reference::SecretRef;
pub use value::SecretValue;

#[cfg(feature = "client")]
pub use config::SecretsConfig;
#[cfg(feature = "client")]
pub use store::SecretStore;

/// Why a secret could not be resolved. Never carries a value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SecretError {
    #[error("invalid secret reference: {0}")]
    BadRef(String),
    #[error("secret {0} is outside Lucina's Infisical folder")]
    OutOfScope(String),
    #[error("secret {0} is refused: {1}")]
    Denied(String, String),
    #[error("secret {0} is not set")]
    NotFound(String),
    #[error("secret {0} is too short to be redacted safely (minimum {1} characters)")]
    TooShort(String, usize),
    #[error("secret {0} cannot be redacted safely ({1}); rotate it to a value without [ or ]")]
    Unredactable(String, String),
    #[error("no secret is configured for {0}")]
    NotConfigured(String),
    #[error("Infisical refused the machine identity ({0})")]
    Unauthorized(String),
    #[error("Infisical HTTP {0}: {1}")]
    Http(u16, String),
    #[error("Infisical unreachable: {0}")]
    Network(String),
    #[error("secrets config: {0}")]
    Config(String),
}

impl SecretError {
    /// A short code for the audit log.
    pub fn code(&self) -> &'static str {
        match self {
            SecretError::BadRef(_) => "bad_ref",
            SecretError::OutOfScope(_) => "out_of_scope",
            SecretError::Denied(..) => "denied",
            SecretError::NotFound(_) => "not_found",
            SecretError::TooShort(..) => "too_short",
            SecretError::Unredactable(..) => "unredactable",
            SecretError::NotConfigured(_) => "not_configured",
            SecretError::Unauthorized(_) => "unauthorized",
            SecretError::Http(..) => "http",
            SecretError::Network(_) => "network",
            SecretError::Config(_) => "config",
        }
    }
}
