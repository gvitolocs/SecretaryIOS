//! `secretary-nclient`: the reader plugin's service (docs/nclient.md).
//!
//! | env | default | |
//! |---|---|---|
//! | `NCLIENT_BIND` | `127.0.0.1:8870` | nezopt's tailnet address in production (p1's Caddy proxies `/v1/nclient/*`) |
//! | `NCLIENT_HOST` | `nhentai.net` | the site host |
//! | `NCLIENT_DATA_DIR` | `/var/lib/secretary-nclient` | favorites, progress, tag cache, signing key |
//! | `NCLIENT_CACHE_DIR` | `$NCLIENT_DATA_DIR/cache` | vetted JSON and proxied images |
//! | `NCLIENT_OWNER` | `owner` | the owner whose favorites/progress this instance keeps |
//! | `NCLIENT_GATEWAY_SECRET_FILE` | required | the secret p1's Caddy sends as `X-Lucina-Gateway` |
//! | `NCLIENT_SIGNING_KEY_FILE` | `$NCLIENT_DATA_DIR/signing.key` | image URL HMAC key, created (0600) when missing |
//! | `NCLIENT_COOKIE_FILE` | unset | optional `Cookie` header for the site |
//! | `NCLIENT_CACHE_MAX_BYTES` | `2147483648` (2 GiB) | cap on the cache directory, oldest files removed first; `0` = no cap |
//! | `NCLIENT_SPOOL_DIR` | `$NCLIENT_DATA_DIR/spool` | pages on their way into lucina-media (emptied at start) |
//! | `NCLIENT_MEDIA_URL` | `http://127.0.0.1:8830` | lucina-media's loopback ingest; must be a loopback address |
//! | `NCLIENT_USER_AGENT` | a Safari UA | |
//! | `NCLIENT_API_INTERVAL_MS` / `NCLIENT_IMAGE_INTERVAL_MS` | `1000` | spacing between requests; never below 1000 |

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use secretary_nclient::cache::{DiskCache, DEFAULT_MAX_BYTES};
use secretary_nclient::http::{router, Http};
use secretary_nclient::saves::Saves;
use secretary_nclient::service::Service;
use secretary_nclient::store::Store;
use secretary_nclient::tags::TagCache;
use secretary_nclient::upstream::{Upstream, UpstreamConfig};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.trim().is_empty())
}

fn interval(name: &str) -> Duration {
    let ms = env(name)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1000);
    Duration::from_millis(ms.max(1000))
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
    // Private from its first byte, and a new file: never readable by others, even for a moment.
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    std::io::Write::write_all(&mut o.open(&tmp)?, &k)?;
    std::fs::rename(&tmp, path)?;
    Ok(k)
}

/// lucina-media is reached over loopback only: the spool paths are sent to it.
fn loopback_url(u: &str) -> anyhow::Result<()> {
    let rest = u
        .strip_prefix("http://")
        .with_context(|| format!("NCLIENT_MEDIA_URL must start with http://, got {u}"))?;
    let host = rest.split(['/', '?']).next().unwrap_or("");
    let host = if host.starts_with('[') {
        host.split(']').next().unwrap_or("").trim_start_matches('[')
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    };
    anyhow::ensure!(
        matches!(host, "127.0.0.1" | "localhost" | "::1"),
        "NCLIENT_MEDIA_URL must be a loopback address"
    );
    Ok(())
}

/// SIGTERM (systemd stop) or Ctrl-C: finish the requests in flight, then exit 0.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = term.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
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
    let data = PathBuf::from(
        env("NCLIENT_DATA_DIR").unwrap_or_else(|| "/var/lib/secretary-nclient".into()),
    );
    std::fs::create_dir_all(&data)?;
    let cache_dir = env("NCLIENT_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data.join("cache"));
    let gateway = read_secret(
        &env("NCLIENT_GATEWAY_SECRET_FILE").context("NCLIENT_GATEWAY_SECRET_FILE is required")?,
    )?;
    let key_path = env("NCLIENT_SIGNING_KEY_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| data.join("signing.key"));
    let cookie = match env("NCLIENT_COOKIE_FILE") {
        Some(p) => Some(
            std::fs::read_to_string(&p)
                .with_context(|| format!("reading {p}"))?
                .trim()
                .to_string(),
        ),
        None => None,
    };
    let mut up = UpstreamConfig {
        host: env("NCLIENT_HOST").unwrap_or_else(|| nclientv3_core::DEFAULT_HOST.into()),
        api_interval: interval("NCLIENT_API_INTERVAL_MS"),
        image_interval: interval("NCLIENT_IMAGE_INTERVAL_MS"),
        cookie,
        ..Default::default()
    };
    if let Some(ua) = env("NCLIENT_USER_AGENT") {
        up.user_agent = ua;
    }
    let cache_max = env("NCLIENT_CACHE_MAX_BYTES")
        .map(|v| v.parse::<u64>().context("NCLIENT_CACHE_MAX_BYTES"))
        .transpose()?
        .unwrap_or(DEFAULT_MAX_BYTES);
    let media_url = env("NCLIENT_MEDIA_URL").unwrap_or_else(|| "http://127.0.0.1:8830".into());
    loopback_url(&media_url)?;
    let spool = env("NCLIENT_SPOOL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data.join("spool"));
    let service = Service::new(
        Upstream::new(up)?,
        TagCache::open(Some(data.join("tags.json"))),
        DiskCache::with_cap(cache_dir, cache_max)?,
    );
    let st = Arc::new(Http {
        service,
        store: Store::open(
            &data.join("owners"),
            &env("NCLIENT_OWNER").unwrap_or_else(|| "owner".into()),
        )?,
        gateway_secret: gateway,
        signing_key: signing_key(&key_path)?,
        saves: Saves::new(spool, media_url)?,
    });
    let bind = env("NCLIENT_BIND").unwrap_or_else(|| "127.0.0.1:8870".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "secretary-nclient listening");
    axum::serve(listener, router(st))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &std::path::Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_new_signing_key_is_private_and_a_good_one_stays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signing.key");
        let key = signing_key(&path).unwrap();
        assert_eq!(key.len(), 32);
        assert_eq!(mode(&path), 0o600);
        assert_eq!(signing_key(&path).unwrap(), key);
    }

    #[test]
    fn a_short_key_open_to_others_is_replaced_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signing.key");
        std::fs::write(&path, b"short").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        // Someone holding the old file open must not see the new key.
        let old = std::fs::File::open(&path).unwrap();
        let key = signing_key(&path).unwrap();
        assert_eq!(mode(&path), 0o600);
        let mut seen = Vec::new();
        std::io::Read::read_to_end(&mut &old, &mut seen).unwrap();
        assert_eq!(seen, b"short");
        assert_eq!(std::fs::read(&path).unwrap(), key);
    }
}
