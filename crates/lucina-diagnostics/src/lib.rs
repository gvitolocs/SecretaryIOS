//! Lucina diagnostics (`lucina-diagnostics` on nezopt, docs/diagnostics.md).
//!
//! Every Lucina client (Secretary Code, iOS, Android, web, the servers) posts
//! crash reports, errors and log bundles to `/v1/diagnostics/reports`
//! ([`api`]). Text is scrubbed of secrets before anything is written
//! ([`scrub`]); repeats of a known crash only bump its counter
//! ([`fingerprint`]); reports live on disk with a SQLite index ([`store`]) and
//! expire after the retention period. A new crash signature notifies the
//! owner ([`notify`]). Claude sessions and Lucina read them back through the
//! same API (secretary-mcp `diagnostics_*`, Lucina's `diagnostics` tool).
pub mod api;
pub mod fingerprint;
pub mod model;
pub mod notify;
pub mod ratelimit;
pub mod scrub;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono_tz::Tz;

pub const MB: u64 = 1024 * 1024;

/// Everything the service reads from its environment.
#[derive(Debug, Clone)]
pub struct Config {
    /// `<root>/<app>/<YYYY-MM-DD>/<id>/` and `<root>/index.sqlite`.
    pub data_dir: PathBuf,
    /// Listen addresses (`127.0.0.1:8850`, the tailnet address for p1's Caddy
    /// and secretary-mcp).
    pub binds: Vec<String>,
    /// Shared secret Caddy on p1 adds as `X-Diagnostics-Gateway` after the
    /// device passed secretary-server's auth (forward_auth).
    pub gateway_token_file: Option<PathBuf>,
    /// Bearer token for local callers (Lucina's tool, secretary-mcp, servers).
    pub service_token_file: Option<PathBuf>,
    pub timezone: Tz,
    /// Reports not seen for this long are deleted.
    pub retention: chrono::Duration,
    /// Request body cap (report JSON plus every attachment, as uploaded).
    pub max_upload_bytes: u64,
    /// Cap on the attachments after gzip is expanded (all files together).
    pub max_expanded_bytes: u64,
    pub max_files: usize,
    /// Per-device token bucket: burst and refill per hour.
    pub rate_burst: u32,
    pub rate_per_hour: u32,
    /// The same bucket for all devices together.
    pub global_per_hour: u32,
    pub notify: notify::NotifyConfig,
    /// How often the retention sweep runs.
    pub sweep_every: Duration,
}

fn num<T: std::str::FromStr>(get: impl Fn(&str) -> Option<String>, k: &str, d: T) -> T {
    get(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d)
}

impl Config {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String> + Copy) -> Self {
        let home = get("HOME").unwrap_or_else(|| "/home/nez".into());
        Self {
            data_dir: PathBuf::from(
                get("LUCINA_DIAGNOSTICS_DIR")
                    .unwrap_or_else(|| format!("{home}/data/lucina-diagnostics")),
            ),
            binds: get("LUCINA_DIAGNOSTICS_BIND")
                .unwrap_or_else(|| "127.0.0.1:8850".into())
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect(),
            gateway_token_file: get("LUCINA_DIAGNOSTICS_GATEWAY_TOKEN_FILE").map(PathBuf::from),
            service_token_file: get("LUCINA_DIAGNOSTICS_SERVICE_TOKEN_FILE").map(PathBuf::from),
            timezone: get("LUCINA_DIAGNOSTICS_TIMEZONE")
                .or_else(|| get("SECRETARY_TIMEZONE"))
                .or_else(|| get("LUCINA_TIMEZONE"))
                .and_then(|z| z.trim().parse().ok())
                .unwrap_or(chrono_tz::Europe::Copenhagen),
            retention: chrono::Duration::days(
                num(get, "LUCINA_DIAGNOSTICS_RETENTION_DAYS", 90i64).clamp(1, 3650),
            ),
            max_upload_bytes: num(get, "LUCINA_DIAGNOSTICS_MAX_UPLOAD_MB", 20u64).max(1) * MB,
            max_expanded_bytes: num(get, "LUCINA_DIAGNOSTICS_MAX_EXPANDED_MB", 64u64).max(1) * MB,
            max_files: num(get, "LUCINA_DIAGNOSTICS_MAX_FILES", 10usize).clamp(1, 64),
            rate_burst: num(get, "LUCINA_DIAGNOSTICS_RATE_BURST", 20u32).max(1),
            rate_per_hour: num(get, "LUCINA_DIAGNOSTICS_RATE_PER_HOUR", 60u32).max(1),
            global_per_hour: num(get, "LUCINA_DIAGNOSTICS_GLOBAL_PER_HOUR", 600u32).max(1),
            notify: notify::NotifyConfig::from_lookup(get),
            sweep_every: Duration::from_secs(6 * 3600),
        }
    }

    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
    }

    /// Defaults with the data under `dir` and nothing external (tests).
    pub fn for_dir(dir: impl Into<PathBuf>) -> Self {
        let mut c = Self::from_lookup(|_| None);
        c.data_dir = dir.into();
        c.notify = notify::NotifyConfig::default();
        c
    }
}

/// Shared state of the service.
pub struct App {
    pub cfg: Config,
    pub store: store::Store,
    pub limits: ratelimit::Limits,
    pub notifier: Arc<notify::Notifier>,
    gateway_token: Option<String>,
    service_token: Option<String>,
}

fn read_token(p: &Option<PathBuf>) -> anyhow::Result<Option<String>> {
    match p {
        None => Ok(None),
        Some(p) => {
            let t = std::fs::read_to_string(p)
                .map_err(|e| anyhow::anyhow!("token file {}: {e}", p.display()))?
                .trim()
                .to_owned();
            anyhow::ensure!(t.len() >= 24, "token in {} is too short", p.display());
            Ok(Some(t))
        }
    }
}

impl App {
    pub fn new(cfg: Config) -> anyhow::Result<Arc<Self>> {
        let store = store::Store::open(&cfg.data_dir)?;
        Ok(Arc::new(Self {
            gateway_token: read_token(&cfg.gateway_token_file)?,
            service_token: read_token(&cfg.service_token_file)?,
            limits: ratelimit::Limits::new(cfg.rate_burst, cfg.rate_per_hour, cfg.global_per_hour),
            notifier: Arc::new(notify::Notifier::new(cfg.notify.clone(), cfg.timezone)),
            store,
            cfg,
        }))
    }

    /// Tokens set directly (tests).
    pub fn with_tokens(self: Arc<Self>, gateway: Option<&str>, service: Option<&str>) -> Arc<Self> {
        let mut a = Arc::try_unwrap(self).ok().expect("unshared app");
        a.gateway_token = gateway.map(str::to_owned);
        a.service_token = service.map(str::to_owned);
        Arc::new(a)
    }

    pub(crate) fn gateway_token(&self) -> Option<&str> {
        self.gateway_token.as_deref()
    }
    pub(crate) fn service_token(&self) -> Option<&str> {
        self.service_token.as_deref()
    }
}

/// Deletes expired reports now and then every `sweep_every`.
pub async fn retention_loop(app: Arc<App>) {
    loop {
        let a = app.clone();
        match tokio::task::spawn_blocking(move || {
            a.store.purge(chrono::Utc::now() - a.cfg.retention)
        })
        .await
        {
            Ok(Ok(n)) if n > 0 => tracing::info!(deleted = n, "retention sweep"),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "retention sweep failed"),
            Err(e) => tracing::warn!(error = %e, "retention sweep panicked"),
        }
        tokio::time::sleep(app.cfg.sweep_every).await;
    }
}
