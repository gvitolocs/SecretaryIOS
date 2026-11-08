//! Settings (environment, read through a lookup so tests need no globals).
//!
//! | variable | default |
//! |---|---|
//! | `LUCINA_SECRETARY_SERVER_URL` | unset: approvals off |
//! | `LUCINA_SECRETARY_SERVICE_TOKEN_FILE` | unset: approvals off (the `svc_…` token) |
//! | `LUCINA_APPROVAL_PUBKEY_FILE` | `~/.config/lucina-rs/approval-grant.pub` |
//! | `LUCINA_APPROVAL_USED_FILE` | `~/.local/state/lucina-rs/approvals-used.jsonl` |
//! | `LUCINA_APPROVAL_WAIT_SECS` | 90 (secrets guard) |
//! | `LUCINA_APPROVAL_REQUIRE_BIOMETRIC` | true |
//! | `LUCINA_ROOT_SUDO` | `/home/nez/bin/nezopt-sudo` |
//! | `LUCINA_ROOT_AUDIT_FILE` | `~/Projects/Lucina/data/root-exec-audit.jsonl` |
//! | `LUCINA_ROOT_WAIT_SECS` | 300 |
//! | `LUCINA_ROOT_CMD_TIMEOUT_SECS` | 120 |
//! | `LUCINA_ROOT_OUTPUT_CAP` | 16384 (bytes per stream per command) |

use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct ApprovalsConfig {
    pub server_url: Option<String>,
    pub token_file: Option<PathBuf>,
    pub pubkey_file: PathBuf,
    pub used_file: PathBuf,
    /// How long the secrets guard waits for the owner.
    pub guard_wait: Duration,
    /// How long a root plan waits for the owner (the approval's ttl is 300 s).
    pub root_wait: Duration,
    pub require_biometric: bool,
    pub root_sudo: PathBuf,
    pub root_audit: PathBuf,
    pub root_cmd_timeout: Duration,
    pub output_cap: usize,
    /// `ttlSecs` of the approvals Lucina creates (30..=900).
    pub ttl_secs: u64,
}

fn home(get: &impl Fn(&str) -> Option<String>) -> PathBuf {
    get("HOME")
        .or_else(|| std::env::var("HOME").ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/root"))
}

impl Default for ApprovalsConfig {
    /// Unconfigured (no server): approvals are off.
    fn default() -> Self {
        Self::from_lookup(&|_| None)
    }
}

impl ApprovalsConfig {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Self {
        let h = home(get);
        let text = |k: &str| {
            get(k)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };
        let path = |k: &str, default: PathBuf| text(k).map(PathBuf::from).unwrap_or(default);
        let secs = |k: &str, default: u64, min: u64, max: u64| {
            Duration::from_secs(
                text(k)
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(default)
                    .clamp(min, max),
            )
        };
        Self {
            server_url: text("LUCINA_SECRETARY_SERVER_URL")
                .map(|u| u.trim_end_matches('/').to_owned()),
            token_file: text("LUCINA_SECRETARY_SERVICE_TOKEN_FILE").map(PathBuf::from),
            pubkey_file: path(
                "LUCINA_APPROVAL_PUBKEY_FILE",
                h.join(".config/lucina-rs/approval-grant.pub"),
            ),
            used_file: path(
                "LUCINA_APPROVAL_USED_FILE",
                h.join(".local/state/lucina-rs/approvals-used.jsonl"),
            ),
            guard_wait: secs("LUCINA_APPROVAL_WAIT_SECS", 90, 1, 900),
            root_wait: secs("LUCINA_ROOT_WAIT_SECS", 300, 1, 900),
            require_biometric: !matches!(
                text("LUCINA_APPROVAL_REQUIRE_BIOMETRIC")
                    .map(|v| v.to_ascii_lowercase())
                    .as_deref(),
                Some("false" | "0" | "no" | "off")
            ),
            root_sudo: path(
                "LUCINA_ROOT_SUDO",
                PathBuf::from("/home/nez/bin/nezopt-sudo"),
            ),
            root_audit: path(
                "LUCINA_ROOT_AUDIT_FILE",
                h.join("Projects/Lucina/data/root-exec-audit.jsonl"),
            ),
            root_cmd_timeout: secs("LUCINA_ROOT_CMD_TIMEOUT_SECS", 120, 1, 3600),
            output_cap: text("LUCINA_ROOT_OUTPUT_CAP")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(16 * 1024)
                .clamp(256, 1024 * 1024),
            ttl_secs: text("LUCINA_APPROVAL_TTL_SECS")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(300)
                .clamp(30, 900),
        }
    }

    /// Server and token are both set (the pubkey is checked separately).
    pub fn is_configured(&self) -> bool {
        self.server_url.is_some() && self.token_file.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let c = ApprovalsConfig::from_lookup(&|k| (k == "HOME").then(|| "/home/x".to_owned()));
        assert!(!c.is_configured());
        assert_eq!(
            c.pubkey_file,
            PathBuf::from("/home/x/.config/lucina-rs/approval-grant.pub")
        );
        assert_eq!(c.root_sudo, PathBuf::from("/home/nez/bin/nezopt-sudo"));
        assert_eq!(c.guard_wait, Duration::from_secs(90));
        assert!(c.require_biometric);
        let c = ApprovalsConfig::from_lookup(&|k| match k {
            "LUCINA_SECRETARY_SERVER_URL" => Some("http://p1:8787/".into()),
            "LUCINA_SECRETARY_SERVICE_TOKEN_FILE" => Some("/t".into()),
            "LUCINA_ROOT_SUDO" => Some("/fake".into()),
            "LUCINA_APPROVAL_REQUIRE_BIOMETRIC" => Some("false".into()),
            _ => None,
        });
        assert!(c.is_configured());
        assert_eq!(c.server_url.as_deref(), Some("http://p1:8787"));
        assert_eq!(c.root_sudo, PathBuf::from("/fake"));
        assert!(!c.require_biometric);
    }
}
