//! `secretary-pixiv`: the Pixiv plugin's service (docs/pixiv.md).
//!
//! | env | default | |
//! |---|---|---|
//! | `PIXIV_BIND` | `127.0.0.1:8872` | nezopt's tailnet address in production (p1's Caddy proxies `/v1/pixiv/*`) |
//! | `PIXIV_DATA_DIR` | `/var/lib/secretary-pixiv` | bookmarks, signing key, spool, cache |
//! | `PIXIV_CACHE_DIR` | `$PIXIV_DATA_DIR/cache` | JSON answers and proxied images |
//! | `PIXIV_CACHE_MAX_BYTES` | 2 GiB | oldest-used-first eviction above this |
//! | `PIXIV_SPOOL_DIR` | `$PIXIV_DATA_DIR/spool` | originals on their way to lucina-media |
//! | `PIXIV_MEDIA_URL` | `http://127.0.0.1:8830` | lucina-media loopback (`/internal/v1/ingest`) |
//! | `PIXIV_OWNER` | `owner` | whose bookmarks this instance keeps |
//! | `PIXIV_GATEWAY_SECRET_FILE` | required | the secret p1's Caddy sends as `X-Lucina-Gateway` |
//! | `PIXIV_SIGNING_KEY_FILE` | `$PIXIV_DATA_DIR/signing.key` | image URL HMAC key, created (0600) when missing |
//! | `PIXIV_API_INTERVAL_MS` | `1500` | spacing between pixiv.net requests; never below 1000 |
//! | `PIXIV_IMAGE_INTERVAL_MS` | `250` | spacing between i.pximg.net request starts; never below 100 |
//! | `PIXIV_IMAGE_CONCURRENCY` | `3` | parallel image requests (1 to 6) |
//! | `PIXIV_LIST_TTL_SECS` / `PIXIV_ILLUST_TTL_SECS` | `300` / `1800` | JSON cache lifetime |
//! | `PIXIV_SESSION_FILE` | none | 0600 file with the pixiv.net `PHPSESSID` value; missing or empty = anonymous |

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use secretary_pixiv::backend::CoreBackend;
use secretary_pixiv::cache::{DiskCache, DEFAULT_MAX_BYTES};
use secretary_pixiv::http::{router, Http};
use secretary_pixiv::images::ImageConfig;
use secretary_pixiv::service::{Config, Service};
use secretary_pixiv::store::Store;
use secretary_pixiv::upstream::GateConfig;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.trim().is_empty())
}

fn num(name: &str) -> Option<u64> {
    env(name).and_then(|v| v.trim().parse::<u64>().ok())
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
    {
        use std::io::Write;
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        o.open(path)?.write_all(&k)?;
    }
    Ok(k)
}

/// The pixiv.net session cookie value, if a non-empty session file is configured. Never logged.
fn session() -> Option<String> {
    let path = env("PIXIV_SESSION_FILE")?;
    let s = std::fs::read_to_string(path).ok()?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
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
    let data = PathBuf::from(
        env("PIXIV_DATA_DIR").unwrap_or_else(|| "/var/lib/secretary-pixiv".into()),
    );
    std::fs::create_dir_all(&data)?;
    let cache_dir = env("PIXIV_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data.join("cache"));
    let spool = env("PIXIV_SPOOL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data.join("spool"));
    let gateway = read_secret(
        &env("PIXIV_GATEWAY_SECRET_FILE").context("PIXIV_GATEWAY_SECRET_FILE is required")?,
    )?;
    let key_path = env("PIXIV_SIGNING_KEY_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| data.join("signing.key"));

    let mut cfg = Config {
        gate: GateConfig {
            interval: Duration::from_millis(num("PIXIV_API_INTERVAL_MS").unwrap_or(1500).max(1000)),
            ..GateConfig::default()
        },
        images: ImageConfig {
            spacing: Duration::from_millis(num("PIXIV_IMAGE_INTERVAL_MS").unwrap_or(250).max(100)),
            concurrency: num("PIXIV_IMAGE_CONCURRENCY").unwrap_or(3).clamp(1, 6) as usize,
            ..ImageConfig::default()
        },
        ..Config::default()
    };
    if let Some(s) = num("PIXIV_LIST_TTL_SECS") {
        cfg.list_ttl = Duration::from_secs(s);
    }
    if let Some(s) = num("PIXIV_ILLUST_TTL_SECS") {
        cfg.illust_ttl = Duration::from_secs(s);
    }
    if let Some(u) = env("PIXIV_MEDIA_URL") {
        cfg.media_url = u;
    }

    let cache = DiskCache::new(
        cache_dir,
        num("PIXIV_CACHE_MAX_BYTES").unwrap_or(DEFAULT_MAX_BYTES),
    )?;
    let session = session();
    tracing::info!(
        session = if session.is_some() { "configured" } else { "none" },
        "pixiv session"
    );
    let service = Service::new(Arc::new(CoreBackend::new(session)), cache, spool, cfg)?;
    let st = Arc::new(Http {
        service,
        store: Store::open(
            &data.join("owners"),
            &env("PIXIV_OWNER").unwrap_or_else(|| "owner".into()),
        )?,
        gateway_secret: gateway,
        signing_key: signing_key(&key_path)?,
    });
    let bind = env("PIXIV_BIND").unwrap_or_else(|| "127.0.0.1:8872".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "secretary-pixiv listening");
    axum::serve(listener, router(st))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}
