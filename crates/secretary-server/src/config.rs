//! Configuration, read once from the environment at startup.
//!
//! | variable | default | |
//! |---|---|---|
//! | `SECRETARY_BIND` | `127.0.0.1:8800` | listen address |
//! | `SECRETARY_LUCINA_URL` | `http://127.0.0.1:18788` | Lucina origin (SSH relay on p1) |
//! | `SECRETARY_LUCINA_TOKEN_FILE` | — | file with the Lucina Bearer token; required for the /v1 data routes |
//! | `SECRETARY_DATA_DIR` | `/var/lib/secretary` | holds `devices.json`, `sessions.json`, `gym.json`, `body.json` |
//! | `SECRETARY_SERVICE_TOKEN_FILE` | — | file with the `svc_…` service token (Lucina); enables `/v1/insights/summary` |
//! | `SECRETARY_UPSTREAM_LEGACY` | — | enables `/v1/upstream/*` (legacy shopping API base) |
//! | `SECRETARY_UPSTREAM_SERVICES` | — | enables `/v1/upstream/*` (service-family base) |
//! | `SECRETARY_UPSTREAM_CATALOG` | — | local catalog JSON for `/v1/upstream/catalog_local` |
//! | `SECRETARY_WEB_DIR` | — | static web app served at `/` (SPA fallback) |
//! | `SECRETARY_TRUST_PROXY` | `0` | `1`: take the client IP from `X-Forwarded-For` (Caddy in front) |
//! | `SECRETARY_PRIVATE_ASSETS_DIR` | `/var/lib/secretary/private-assets` | files for `GET /v1/assets/{name}` (authenticated only) |
//! | `SECRETARY_TTS_URL` | `http://100.107.164.33:8880` | Kokoro (OpenAI-compatible) for `POST /v1/tts` |
//! | `SECRETARY_CATALOG_DIR` | `/var/lib/secretary/catalog` | `catalog.{it,en,da}.json` for `GET /v1/shopping/catalog` |
//! | `SECRETARY_DEVICE_EVENTS_PER_MINUTE` | `120` | `POST /v1/device/events` requests per device per minute |
//! | `SECRETARY_TTS_VOICES` | `if_sara,af_heart,af_bella,bf_emma` | voices a client may ask for |
//! | `SECRETARY_NOTIFY_INGEST` | `1` | `0`: no notifications from Lucina's timeline stream |
//! | `SECRETARY_VAPID_SUBJECT` | `https://getsecretary.tech` | VAPID contact (`mailto:` or `https:`) |
//! | `SECRETARY_PUSH_HOSTS` | FCM, Mozilla, Apple, WNS | comma-separated push-service host suffixes allowed as endpoints |
//! | `SECRETARY_PUSH_ALLOW_HTTP` | `0` | `1`: accept `http://` push endpoints (local mock push services only) |
//! | `SECRETARY_APNS_KEY_FILE`, `_KEY_ID`, `_TEAM_ID`, `_TOPIC`, `_ENV` | — | APNs for the native app; all three of file/key/team or none (see `notify::apns`) |
//! | `SECRETARY_TIMEZONE` | `Europe/Copenhagen` | the owner's zone (calendar days, all-day events) |
//! | `SECRETARY_GOOGLE_CLIENT_ID_FILE` | — | Google OAuth client id (one line); with the secret file, enables Google in Links |
//! | `SECRETARY_GOOGLE_CLIENT_SECRET_FILE` | — | Google OAuth client secret (one line, 0600) |
//! | `SECRETARY_GOOGLE_REDIRECT_URI` | `https://getsecretary.tech/v1/links/google/callback` | must be registered on the OAuth client |
//! | `SECRETARY_CALENDAR_SYNC_SECS` | `300` | background sync of Google and ICS calendars; `0` turns it off |
//! | `SECRETARY_MCP_STATUS_URL` | `http://127.0.0.1:8810/internal/status` | secretary-mcp's client list (AI clients in Links) |
//! | `SECRETARY_MCP_STATUS_TOKEN_FILE` | — | the shared status token (same file content as secretary-mcp's) |

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context};
use secretary_core::Timeouts;

pub const DEFAULT_BIND: &str = "127.0.0.1:8800";
pub const DEFAULT_LUCINA_URL: &str = "http://127.0.0.1:18788";
pub const DEFAULT_DATA_DIR: &str = "/var/lib/secretary";
pub const DEFAULT_PRIVATE_ASSETS_DIR: &str = "/var/lib/secretary/private-assets";
pub const DEFAULT_CATALOG_DIR: &str = "/var/lib/secretary/catalog";
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
    pub web_dir: Option<PathBuf>,
    pub trust_proxy: bool,
    /// `None` when `SECRETARY_SERVICE_TOKEN_FILE` is unset: insights 503.
    pub service_token: Option<crate::service::ServiceToken>,
    pub private_assets_dir: PathBuf,
    /// Holds `catalog.{lang}.json` (shopping catalog).
    pub catalog_dir: PathBuf,
    pub tts_url: String,
    pub tts_voices: Vec<String>,
    pub tts_timeout: Duration,
    pub timeouts: Timeouts,
    pub upstream_timeout: Duration,
    pub rate_limit_per_minute: u32,
    /// `POST /v1/device/events` requests per device per minute (own bucket).
    pub device_events_per_minute: u32,
    /// `POST /v1/session` attempts per client IP per minute.
    pub login_rate_per_minute: u32,
    pub health_timeout: Duration,
    pub health_ttl: Duration,
    pub notify: crate::notify::NotifyConfig,
    /// The owner's time zone.
    pub timezone: chrono_tz::Tz,
    /// `None` without both Google client files: Google shows as unavailable.
    pub google: Option<crate::google::GoogleConfig>,
    pub calendar_sync_every: Duration,
    pub mcp_status: Option<crate::links::McpStatusConfig>,
    /// Tests only: allow `http://` and local ICS URLs.
    pub ics_allow_local: bool,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("lucina_url", &self.lucina_url)
            .field("lucina_token", &self.lucina_token.as_ref().map(|_| "<set>"))
            .field("data_dir", &self.data_dir)
            .field("upstream", &self.upstream)
            .field("web_dir", &self.web_dir)
            .field("trust_proxy", &self.trust_proxy)
            .field(
                "service_token",
                &self.service_token.as_ref().map(|_| "<set>"),
            )
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
            web_dir: None,
            trust_proxy: false,
            service_token: None,
            private_assets_dir: PathBuf::from(DEFAULT_PRIVATE_ASSETS_DIR),
            catalog_dir: PathBuf::from(DEFAULT_CATALOG_DIR),
            tts_url: crate::tts::DEFAULT_URL.into(),
            tts_voices: crate::tts::DEFAULT_VOICES
                .split(',')
                .map(str::to_owned)
                .collect(),
            tts_timeout: crate::tts::TIMEOUT,
            timeouts: Timeouts::default(),
            upstream_timeout: Duration::from_secs(15),
            rate_limit_per_minute: 60,
            device_events_per_minute: 120,
            login_rate_per_minute: 5,
            health_timeout: Duration::from_secs(3),
            health_ttl: Duration::from_secs(10),
            notify: crate::notify::NotifyConfig::default(),
            timezone: chrono_tz::Europe::Copenhagen,
            google: None,
            calendar_sync_every: Duration::from_secs(300),
            mcp_status: None,
            ics_allow_local: false,
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
        cfg.web_dir = get("SECRETARY_WEB_DIR").map(PathBuf::from);
        if let Some(d) = get("SECRETARY_PRIVATE_ASSETS_DIR") {
            cfg.private_assets_dir = PathBuf::from(d);
        }
        if let Some(d) = get("SECRETARY_CATALOG_DIR") {
            cfg.catalog_dir = PathBuf::from(d);
        }
        if let Some(u) = get("SECRETARY_TTS_URL") {
            cfg.tts_url = u;
        }
        if let Some(v) = get("SECRETARY_TTS_VOICES") {
            cfg.tts_voices = v
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
            if cfg.tts_voices.is_empty() {
                bail!("SECRETARY_TTS_VOICES is empty");
            }
        }
        if let Some(v) = get("SECRETARY_DEVICE_EVENTS_PER_MINUTE") {
            cfg.device_events_per_minute =
                v.parse().ok().filter(|n| *n > 0).with_context(|| {
                    format!("SECRETARY_DEVICE_EVENTS_PER_MINUTE={v}: expected a positive integer")
                })?;
        }
        cfg.trust_proxy = match get("SECRETARY_TRUST_PROXY").as_deref() {
            None | Some("0") | Some("false") => false,
            Some("1") | Some("true") => true,
            Some(v) => bail!("SECRETARY_TRUST_PROXY={v}: expected 0 or 1"),
        };
        if let Some(path) = get("SECRETARY_LUCINA_TOKEN_FILE") {
            cfg.lucina_token = Some(read_token_file(&path)?);
        }
        if let Some(path) = get("SECRETARY_SERVICE_TOKEN_FILE") {
            cfg.service_token = Some(crate::service::ServiceToken::read_file(&path)?);
        }
        cfg.notify.ingest = match get("SECRETARY_NOTIFY_INGEST").as_deref() {
            None | Some("1") | Some("true") => true,
            Some("0") | Some("false") => false,
            Some(v) => bail!("SECRETARY_NOTIFY_INGEST={v}: expected 0 or 1"),
        };
        if let Some(s) = get("SECRETARY_VAPID_SUBJECT") {
            if !(s.starts_with("mailto:") || s.starts_with("https://")) {
                bail!("SECRETARY_VAPID_SUBJECT must start with mailto: or https://");
            }
            cfg.notify.push.subject = s;
        }
        if let Some(h) = get("SECRETARY_PUSH_HOSTS") {
            cfg.notify.push.hosts = h
                .split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
        }
        cfg.notify.push.allow_http = match get("SECRETARY_PUSH_ALLOW_HTTP").as_deref() {
            None | Some("0") | Some("false") => false,
            Some("1") | Some("true") => true,
            Some(v) => bail!("SECRETARY_PUSH_ALLOW_HTTP={v}: expected 0 or 1"),
        };
        cfg.notify.push.apns = crate::notify::apns::ApnsConfig::from_lookup(&get)?;
        if let Some(tz) = get("SECRETARY_TIMEZONE") {
            cfg.timezone = tz
                .parse()
                .map_err(|_| anyhow::anyhow!("SECRETARY_TIMEZONE={tz}: unknown IANA time zone"))?;
        }
        match (
            get("SECRETARY_GOOGLE_CLIENT_ID_FILE"),
            get("SECRETARY_GOOGLE_CLIENT_SECRET_FILE"),
        ) {
            (Some(id), Some(secret)) => {
                let mut g = crate::google::GoogleConfig::new(
                    read_secret_file(&id, "SECRETARY_GOOGLE_CLIENT_ID_FILE")?,
                    read_secret_file(&secret, "SECRETARY_GOOGLE_CLIENT_SECRET_FILE")?,
                );
                if let Some(r) = get("SECRETARY_GOOGLE_REDIRECT_URI") {
                    g.redirect_uri = r;
                }
                cfg.google = Some(g);
            }
            (None, None) => {}
            _ => bail!("set both SECRETARY_GOOGLE_CLIENT_ID_FILE and SECRETARY_GOOGLE_CLIENT_SECRET_FILE, or neither"),
        }
        if let Some(v) = get("SECRETARY_CALENDAR_SYNC_SECS") {
            let n: u64 = v
                .parse()
                .with_context(|| format!("SECRETARY_CALENDAR_SYNC_SECS={v}: expected seconds"))?;
            if n != 0 && n < 60 {
                bail!("SECRETARY_CALENDAR_SYNC_SECS={v}: 0 (off) or at least 60");
            }
            cfg.calendar_sync_every = Duration::from_secs(n);
        }
        if let Some(path) = get("SECRETARY_MCP_STATUS_TOKEN_FILE") {
            cfg.mcp_status = Some(crate::links::McpStatusConfig {
                url: get("SECRETARY_MCP_STATUS_URL")
                    .unwrap_or_else(|| "http://127.0.0.1:8810/internal/status".into()),
                token: read_secret_file(&path, "SECRETARY_MCP_STATUS_TOKEN_FILE")?,
            });
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

/// Read a one-line secret file (trimmed). Never includes the content in
/// errors; warns when group/others can read it.
pub fn read_secret_file(path: &str, var: &str) -> anyhow::Result<String> {
    let meta = std::fs::metadata(path).with_context(|| format!("{var} {path}"))?;
    if meta.permissions().mode() & 0o077 != 0 {
        tracing::warn!(
            path,
            var,
            "secret file is readable by group/others; chmod 600 it"
        );
    }
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading {var} {path}"))?;
    let v = raw.trim().to_owned();
    if v.is_empty() || v.contains(char::is_whitespace) {
        bail!("{var} {path} must hold exactly one value");
    }
    Ok(v)
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
