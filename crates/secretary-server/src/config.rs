//! Configuration, read once from the environment at startup.
//!
//! | variable | default | |
//! |---|---|---|
//! | `SECRETARY_BIND` | `127.0.0.1:8800` | listen address |
//! | `SECRETARY_LUCINA_URL` | `http://127.0.0.1:18788` | Lucina origin (SSH relay on p1) |
//! | `SECRETARY_LUCINA_TOKEN_FILE` | — | file with the Lucina Bearer token; required for the /v1 data routes |
//! | `SECRETARY_DATA_DIR` | `/var/lib/secretary` | holds `devices.json` |
//! | `SECRETARY_UPSTREAM_LEGACY` | — | enables `/v1/upstream/*` (legacy shopping API base) |
//! | `SECRETARY_UPSTREAM_SERVICES` | — | enables `/v1/upstream/*` (service-family base) |
//! | `SECRETARY_UPSTREAM_CATALOG` | — | local catalog JSON for `/v1/upstream/catalog_local` |

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context};
use secretary_core::Timeouts;

pub const DEFAULT_BIND: &str = "127.0.0.1:8800";
pub const DEFAULT_LUCINA_URL: &str = "http://127.0.0.1:18788";
pub const DEFAULT_DATA_DIR: &str = "/var/lib/secretary";
pub const BODY_LIMIT: usize = 256 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UpstreamConfig {
    pub legacy: Option<String>,
    pub services: Option<String>,
    pub catalog: Option<PathBuf>,
}

#[derive(Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub lucina_url: String,
    /// `None` when `SECRETARY_LUCINA_TOKEN_FILE` is unset: data routes 503.
    pub lucina_token: Option<String>,
    pub data_dir: PathBuf,
    pub upstream: Option<UpstreamConfig>,
    pub timeouts: Timeouts,
    pub upstream_timeout: Duration,
    pub rate_limit_per_minute: u32,
    pub health_timeout: Duration,
    pub health_ttl: Duration,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("lucina_url", &self.lucina_url)
            .field("lucina_token", &self.lucina_token.as_ref().map(|_| "<set>"))
            .field("data_dir", &self.data_dir)
            .field("upstream", &self.upstream)
            .field("timeouts", &self.timeouts)
            .field("rate_limit_per_minute", &self.rate_limit_per_minute)
            .finish()
    }
}

impl Config {
    /// Defaults with an explicit Lucina token; used by tests and as the base
    /// of [`Config::from_lookup`].
    pub fn new(lucina_url: impl Into<String>, lucina_token: Option<String>) -> Self {
        Self {
            bind: DEFAULT_BIND.parse().expect("default bind"),
            lucina_url: lucina_url.into(),
            lucina_token,
            data_dir: PathBuf::from(DEFAULT_DATA_DIR),
            upstream: None,
            timeouts: Timeouts::default(),
            upstream_timeout: Duration::from_secs(15),
            rate_limit_per_minute: 60,
            health_timeout: Duration::from_secs(3),
            health_ttl: Duration::from_secs(10),
        }
    }

    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let mut cfg = Self::new(
            get("SECRETARY_LUCINA_URL").unwrap_or_else(|| DEFAULT_LUCINA_URL.into()),
            None,
        );
        if let Some(b) = get("SECRETARY_BIND") {
            cfg.bind = b
                .parse()
                .with_context(|| format!("SECRETARY_BIND={b} is not host:port"))?;
        }
        if let Some(d) = get("SECRETARY_DATA_DIR") {
            cfg.data_dir = PathBuf::from(d);
        }
        if let Some(path) = get("SECRETARY_LUCINA_TOKEN_FILE") {
            cfg.lucina_token = Some(read_token_file(&path)?);
        }
        let upstream = UpstreamConfig {
            legacy: get("SECRETARY_UPSTREAM_LEGACY"),
            services: get("SECRETARY_UPSTREAM_SERVICES"),
            catalog: get("SECRETARY_UPSTREAM_CATALOG").map(PathBuf::from),
        };
        if upstream.legacy.is_some() || upstream.services.is_some() {
            cfg.upstream = Some(upstream);
        }
        Ok(cfg)
    }
}

/// Read and trim the token file. Never includes the content in errors.
pub fn read_token_file(path: &str) -> anyhow::Result<String> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("SECRETARY_LUCINA_TOKEN_FILE {path}"))?;
    if meta.permissions().mode() & 0o077 != 0 {
        tracing::warn!(
            path,
            "Lucina token file is readable by group/others; chmod 600 it"
        );
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading SECRETARY_LUCINA_TOKEN_FILE {path}"))?;
    let token = raw.trim().to_owned();
    if token.is_empty() {
        bail!("SECRETARY_LUCINA_TOKEN_FILE {path} is empty");
    }
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn env_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let tok = dir.path().join("tok");
        std::fs::write(&tok, "  abc\n").unwrap();
        let env: HashMap<&str, String> = [
            ("SECRETARY_BIND", "0.0.0.0:9000".to_owned()),
            (
                "SECRETARY_LUCINA_TOKEN_FILE",
                tok.to_string_lossy().into_owned(),
            ),
            ("SECRETARY_UPSTREAM_LEGACY", "http://up".to_owned()),
        ]
        .into_iter()
        .collect();
        let cfg = Config::from_lookup(|k| env.get(k).cloned()).unwrap();
        assert_eq!(cfg.bind.port(), 9000);
        assert_eq!(cfg.lucina_token.as_deref(), Some("abc"));
        assert_eq!(cfg.lucina_url, DEFAULT_LUCINA_URL);
        assert!(cfg.upstream.unwrap().services.is_none());
        assert!(!format!("{:?}", Config::new("x", Some("abc".into()))).contains("abc"));

        let none = Config::from_lookup(|_| None).unwrap();
        assert!(none.lucina_token.is_none() && none.upstream.is_none());
        std::fs::write(&tok, "\n").unwrap();
        let path = tok.to_string_lossy().into_owned();
        assert!(
            Config::from_lookup(|k| (k == "SECRETARY_LUCINA_TOKEN_FILE").then(|| path.clone()))
                .is_err()
        );
    }
}
