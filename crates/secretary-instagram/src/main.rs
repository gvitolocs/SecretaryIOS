//! `secretary-instagram`.
//!
//! | env | default | |
//! |---|---|---|
//! | `INSTAGRAM_BIND` | `127.0.0.1:8871` | nezopt's tailnet address in production (p1's Caddy proxies `/v1/instagram/*`) |
//! | `INSTAGRAM_DATA_DIR` | `$HOME/.local/share/secretary-instagram` | recent profiles, signing key, spool |
//! | `INSTAGRAM_CACHE_DIR` | `$INSTAGRAM_DATA_DIR/cache` | upstream JSON, the profile store and media. Must already exist: a missing one (its disk not mounted) falls back to the default with the default cap, so nothing lands on the wrong disk |
//! | `INSTAGRAM_CACHE_MAX_BYTES` | `2147483648` | cache size cap, oldest files go first; watched accounts and saved items stay |
//! | `INSTAGRAM_MIRROR` | `1` | `0` turns the background download and backfill off |
//! | `INSTAGRAM_MIRROR_INTERVAL_MS` | `1000` | rest between two background downloads |
//! | `INSTAGRAM_MIRROR_DAILY_MAX_BYTES` | `21474836480` | background download bytes per UTC day |
//! | `INSTAGRAM_BACKFILL_INTERVAL_SECS` | `20` | rest between two older-pages calls to the backend |
//! | `INSTAGRAM_BACKFILL_DAILY_CALLS` | `300` | older-pages calls to the backend per UTC day |
//! | `INSTAGRAM_GATEWAY_SECRET_FILE` | required | the secret p1's Caddy sends as `X-Lucina-Gateway` |
//! | `INSTAGRAM_SIGNING_KEY_FILE` | `$INSTAGRAM_DATA_DIR/signing.key` | media URL HMAC key, created (0600) when missing |
//! | `INSTAGRAM_API_INTERVAL_MS` | `1000` | spacing between backend requests; never below 1000 |
//! | `INSTAGRAM_MEDIA_INTERVAL_MS` | `250` | spacing between Instagram CDN requests (2 at a time) |
//! | `INSTAGRAM_BASE_URL` | `https://storyview.yodams.com` | the backend |
//! | `INSTAGRAM_INGEST_URL` | `http://127.0.0.1:8830` | lucina-media's loopback |
//! | `INSTAGRAM_SPOOL_DIR` | `$INSTAGRAM_DATA_DIR/spool` | saved items wait here until lucina-media takes them |
//! | `INSTAGRAM_WATCH` | `1` | `0` turns the warmer off (no refreshes, no auto-save, no change notices) |
//! | `INSTAGRAM_WATCH_INTERVAL_H` | `23` | hours between two checks of a watched account (1-23, ± 30 min of jitter); the owner's choice in the app (`/v1/instagram/watch-settings`) wins |
//! | `INSTAGRAM_WATCH_PAUSE_SECS` | `1800` | the warmer's rest after the backend says 429 |
//! | `INSTAGRAM_WATCH_DAILY_MAX_BYTES` | `524288000` | auto-saved bytes per account per UTC day |
//! | `INSTAGRAM_NOTIFY_URL` | unset | secretary-server's base URL, for the owner's change notices (`POST /v1/notify`) |
//! | `INSTAGRAM_NOTIFY_TOKEN_FILE` | unset | the file with Lucina's `svc_…` service token (0600; from the broker, never in git) |

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use secretary_instagram::cache::DiskCache;
use secretary_instagram::http::{router, Http};
use secretary_instagram::media::Signer;
use secretary_instagram::recent::Recent;
use secretary_instagram::service::{MirrorConfig, Service, ServiceConfig, WatchConfig};
use secretary_instagram::upstream::{Upstream, UpstreamConfig};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.trim().is_empty())
}

fn millis(name: &str, default: u64, floor: u64) -> Duration {
    Duration::from_millis(
        env(name)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
            .max(floor),
    )
}

fn read_secret(path: &str) -> anyhow::Result<Vec<u8>> {
    let s = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
    let s = s.trim().as_bytes().to_vec();
    anyhow::ensure!(s.len() >= 16, "{path}: secret shorter than 16 bytes");
    Ok(s)
}

fn signing_key(path: &PathBuf) -> anyhow::Result<Vec<u8>> {
    if let Ok(k) = std::fs::read(path) {
        if k.len() >= 32 {
            return Ok(k);
        }
    }
    let mut k = vec![0u8; 32];
    getrandom::fill(&mut k).map_err(|e| anyhow::anyhow!("getrandom: {e}"))?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(&k)?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, &k)?;
    Ok(k)
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler");
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    tracing::info!("shutting down");
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,reqwest=warn,hyper=warn".into()),
        )
        .init();
    let data = match env("INSTAGRAM_DATA_DIR") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/var/lib".into()))
            .join(".local/share/secretary-instagram"),
    };
    std::fs::create_dir_all(&data)?;
    const SMALL_CACHE: u64 = 2 * 1024 * 1024 * 1024;
    let max_bytes = env("INSTAGRAM_CACHE_MAX_BYTES")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(SMALL_CACHE);
    let (cache_dir, max_bytes) = match env("INSTAGRAM_CACHE_DIR").map(PathBuf::from) {
        Some(d) if d.is_dir() => (d, max_bytes),
        Some(_) => {
            // The big disk is not mounted: never fill the small one in its place.
            tracing::warn!("INSTAGRAM_CACHE_DIR is missing; using the small local cache");
            (data.join("cache"), SMALL_CACHE)
        }
        None => (data.join("cache"), max_bytes),
    };
    let gateway = read_secret(
        &env("INSTAGRAM_GATEWAY_SECRET_FILE")
            .context("INSTAGRAM_GATEWAY_SECRET_FILE is required")?,
    )?;
    let key_path = env("INSTAGRAM_SIGNING_KEY_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| data.join("signing.key"));
    let mut up_cfg = UpstreamConfig {
        api_interval: millis("INSTAGRAM_API_INTERVAL_MS", 1000, 1000),
        media_interval: millis("INSTAGRAM_MEDIA_INTERVAL_MS", 250, 50),
        ..Default::default()
    };
    if let Some(b) = env("INSTAGRAM_BASE_URL") {
        up_cfg.base = b;
    }
    let secs = |name: &str, default: u64| {
        Duration::from_secs(env(name).and_then(|v| v.parse().ok()).unwrap_or(default))
    };
    let interval_h = env("INSTAGRAM_WATCH_INTERVAL_H")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(23)
        .clamp(1, 23);
    let mut watch = WatchConfig {
        enabled: env("INSTAGRAM_WATCH").is_none_or(|v| v != "0"),
        interval: Duration::from_secs(interval_h * 3600),
        pause: secs("INSTAGRAM_WATCH_PAUSE_SECS", 1800),
        ..Default::default()
    };
    if let Some(n) = env("INSTAGRAM_WATCH_DAILY_MAX_BYTES").and_then(|v| v.parse().ok()) {
        watch.daily_cap_bytes = n;
    }
    let mut svc_cfg = ServiceConfig::new(
        env("INSTAGRAM_SPOOL_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| data.join("spool")),
        env("INSTAGRAM_INGEST_URL").unwrap_or_else(|| "http://127.0.0.1:8830".into()),
        data.clone(),
    );
    svc_cfg.notify_url = env("INSTAGRAM_NOTIFY_URL");
    svc_cfg.notify_token_file = env("INSTAGRAM_NOTIFY_TOKEN_FILE").map(PathBuf::from);
    svc_cfg.watch = watch;
    let mut mirror = MirrorConfig {
        interval: millis("INSTAGRAM_MIRROR_INTERVAL_MS", 1000, 100),
        backfill_interval: secs("INSTAGRAM_BACKFILL_INTERVAL_SECS", 20).max(Duration::from_secs(5)),
        ..Default::default()
    };
    if let Some(n) = env("INSTAGRAM_MIRROR_DAILY_MAX_BYTES").and_then(|v| v.parse().ok()) {
        mirror.daily_max_bytes = n;
    }
    if let Some(n) = env("INSTAGRAM_BACKFILL_DAILY_CALLS").and_then(|v| v.parse().ok()) {
        mirror.backfill_daily_calls = n;
    }
    svc_cfg.mirror = mirror;
    let mirror_on = env("INSTAGRAM_MIRROR").is_none_or(|v| v != "0");
    let service = Service::new(
        Upstream::new(up_cfg)?,
        DiskCache::new(cache_dir, max_bytes)?,
        Recent::open(data.join("recent.json")),
        Signer::new(signing_key(&key_path)?),
        svc_cfg,
    )?;
    let service = Arc::new(service);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let warmer = {
        let (service, stop) = (service.clone(), stop_rx.clone());
        tokio::spawn(async move { service.run_warmer(stop).await })
    };
    let mirror = mirror_on.then(|| {
        let (service, stop) = (service.clone(), stop_rx.clone());
        tokio::spawn(async move { service.run_mirror(stop).await })
    });
    let backfill = mirror_on.then(|| {
        let (service, stop) = (service.clone(), stop_rx.clone());
        tokio::spawn(async move { service.run_backfill(stop).await })
    });
    let st = Arc::new(Http {
        service,
        gateway_secret: gateway,
    });
    let bind = env("INSTAGRAM_BIND").unwrap_or_else(|| "127.0.0.1:8871".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "secretary-instagram listening");
    axum::serve(listener, router(st))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            let _ = stop_tx.send(true);
        })
        .await?;
    let _ = warmer.await;
    for task in [mirror, backfill].into_iter().flatten() {
        let _ = task.await;
    }
    tracing::info!("stopped");
    Ok(())
}
