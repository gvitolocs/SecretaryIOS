//! Lucina diagnostics (`lucina-diagnostics` on nezopt): crash reports and log
//! bundles from every Lucina client (Secretary Code, iOS, Android, web, the
//! servers), behind the same gateway as `/v1/memos` (docs/diagnostics.md).
//!
//! - [`api`]: `/v1/diagnostics/reports*` (auth, rate limit, routes);
//! - [`ingest`]: body parsing, caps, gzip, attachments;
//! - [`scrub`]: secrets out of every text before it is stored;
//! - [`fingerprint`]: dedupe of repeats (counted, not stored again);
//! - [`store`]: report directories plus a SQLite index, retention;
//! - [`notify`]: a short note to the owner on a new crash.
pub mod api;
pub mod fingerprint;
pub mod ingest;
pub mod model;
pub mod notify;
pub mod ratelimit;
pub mod scrub;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;

use chrono_tz::Tz;

/// Everything the service reads from its environment.
#[derive(Debug, Clone)]
pub struct Config {
    /// `<root>/<app>/<YYYY-MM-DD>/<id>/` and `<root>/index.sqlite`.
    pub data_dir: PathBuf,
    /// Listen addresses (`127.0.0.1:8794`, the tailnet address for p1's Caddy).
    pub binds: Vec<String>,
    /// Shared secret Caddy on p1 adds as `X-Diagnostics-Gateway` after the
    /// device passed secretary-server's auth (forward_auth).
    pub gateway_token_file: Option<PathBuf>,
    /// Bearer token for local callers (secretary-mcp, Lucina's tools, scripts).
    pub service_token_file: Option<PathBuf>,
    pub timezone: Tz,
    /// Reports not seen for this many days are deleted.
    pub retention_days: i64,
    pub limits: ingest::Limits,
    /// Reports a device may send per hour, and at once.
    pub rate_per_hour: u32,
    pub rate_burst: u32,
    pub notify: notify::NotifyConfig,
}

impl Config {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String> + Copy) -> Self {
        let home = get("HOME").unwrap_or_else(|| "/home/nez".into());
        let num = |k: &str, d: u64| -> u64 {
            get(k)
                .and_then(|v| v.trim().parse().ok())
                .filter(|v| *v > 0)
                .unwrap_or(d)
        };
        Self {
            data_dir: PathBuf::from(
                get("LUCINA_DIAGNOSTICS_DIR")
                    .unwrap_or_else(|| format!("{home}/data/lucina-diagnostics")),
            ),
            binds: get("LUCINA_DIAGNOSTICS_BIND")
                .unwrap_or_else(|| "127.0.0.1:8794".into())
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
            retention_days: num("LUCINA_DIAGNOSTICS_RETENTION_DAYS", 90) as i64,
            limits: ingest::Limits {
                attachment_bytes: num("LUCINA_DIAGNOSTICS_MAX_ATTACHMENT_BYTES", 20 * 1024 * 1024)
                    as usize,
                attachments: num("LUCINA_DIAGNOSTICS_MAX_ATTACHMENTS", 16) as usize,
            },
            rate_per_hour: num("LUCINA_DIAGNOSTICS_RATE_PER_HOUR", 60) as u32,
            rate_burst: num("LUCINA_DIAGNOSTICS_RATE_BURST", 20) as u32,
            notify: notify::NotifyConfig::from_lookup(get),
        }
    }

    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
    }
}

/// Shared state of the service.
pub struct App {
    pub cfg: Config,
    pub store: store::Store,
    pub limiter: ratelimit::RateLimiter,
    pub gate: notify::Gate,
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
            limiter: ratelimit::RateLimiter::new(cfg.rate_per_hour, cfg.rate_burst),
            gate: notify::Gate::new(cfg.notify.min_interval),
            cfg,
            store,
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

/// Deletes expired reports every hour.
pub async fn retention_loop(app: Arc<App>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
    loop {
        tick.tick().await;
        let a = app.clone();
        let r = tokio::task::spawn_blocking(move || {
            a.store.prune(chrono::Utc::now(), a.cfg.retention_days)
        })
        .await;
        match r {
            Ok(Ok(0)) => {}
            Ok(Ok(n)) => tracing::info!(deleted = n, "diagnostics retention"),
            Ok(Err(e)) => tracing::error!(error = %e, "diagnostics retention failed"),
            Err(e) => tracing::error!(error = %e, "diagnostics retention task"),
        }
    }
}
