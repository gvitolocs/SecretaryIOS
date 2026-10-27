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
//! | `NCLIENT_USER_AGENT` | a Safari UA | |
//! | `NCLIENT_API_INTERVAL_MS` / `NCLIENT_IMAGE_INTERVAL_MS` | `1000` | spacing between requests; never below 1000 |

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use secretary_nclient::cache::DiskCache;
use secretary_nclient::http::{router, Http};
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
    std::fs::write(path, &k)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(k)
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
    let service = Service::new(
        Upstream::new(up)?,
        TagCache::open(Some(data.join("tags.json"))),
        DiskCache::new(cache_dir)?,
    );
    let st = Arc::new(Http {
        service,
        store: Store::open(
            &data.join("owners"),
            &env("NCLIENT_OWNER").unwrap_or_else(|| "owner".into()),
        )?,
        gateway_secret: gateway,
        signing_key: signing_key(&key_path)?,
    });
    let bind = env("NCLIENT_BIND").unwrap_or_else(|| "127.0.0.1:8870".into());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "secretary-nclient listening");
    axum::serve(listener, router(st)).await?;
    Ok(())
}
