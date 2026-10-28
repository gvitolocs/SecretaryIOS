//! Image fetching from i.pximg.net. Never an open proxy: only `https://i.pximg.net/...` (no
//! userinfo, no port) is ever requested, and the `Referer: https://www.pixiv.net/` the CDN
//! insists on is added here (a browser `<img>` cannot send it). A small pool of requests may run
//! at once, each started at least `spacing` after the previous one; 429/5xx answers are retried
//! with backoff (honouring `Retry-After`), and a 429 that outlasts the retries starts a cool-down.

use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, Semaphore};

use crate::error::SvcError;
use crate::upstream::{backoff, secs_ceil};

pub const IMAGE_HOST: &str = "i.pximg.net";
const REFERER: &str = "https://www.pixiv.net/";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

#[derive(Debug, Clone)]
pub struct ImageConfig {
    /// Tests only: serve `https://i.pximg.net/<rest>` from `<base>/<rest>`.
    pub base_override: Option<String>,
    pub spacing: Duration,
    pub concurrency: usize,
    pub backoff_base: Duration,
    pub attempts: u32,
    pub cooldown: Duration,
    pub timeout: Duration,
}

impl Default for ImageConfig {
    fn default() -> Self {
        ImageConfig {
            base_override: None,
            spacing: Duration::from_millis(250),
            concurrency: 3,
            backoff_base: Duration::from_secs(2),
            attempts: 3,
            cooldown: Duration::from_secs(30),
            timeout: Duration::from_secs(60),
        }
    }
}

pub struct ImageFetcher {
    cfg: ImageConfig,
    http: reqwest::Client,
    pool: Semaphore,
    next_start: Mutex<Instant>,
    cooldown_until: StdMutex<Option<Instant>>,
}

/// `Some(url)` only for `https://i.pximg.net/...`.
pub fn allowed(raw: &str) -> Option<url::Url> {
    let u = url::Url::parse(raw).ok()?;
    let ok = u.scheme() == "https"
        && u.host_str() == Some(IMAGE_HOST)
        && u.port().is_none()
        && u.username().is_empty()
        && u.password().is_none();
    ok.then_some(u)
}

impl ImageFetcher {
    pub fn new(cfg: ImageConfig) -> anyhow::Result<ImageFetcher> {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(UA)
            .build()?;
        Ok(ImageFetcher {
            pool: Semaphore::new(cfg.concurrency.max(1)),
            next_start: Mutex::new(Instant::now()),
            cooldown_until: StdMutex::new(None),
            cfg,
            http,
        })
    }

    fn target(&self, u: &url::Url) -> String {
        match &self.cfg.base_override {
            Some(base) => {
                let q = u.query().map(|q| format!("?{q}")).unwrap_or_default();
                format!("{}{}{}", base.trim_end_matches('/'), u.path(), q)
            }
            None => u.to_string(),
        }
    }

    async fn spaced(&self) {
        let at = {
            let mut next = self.next_start.lock().await;
            let at = (*next).max(Instant::now());
            *next = at + self.cfg.spacing;
            at
        };
        tokio::time::sleep_until(at.into()).await;
    }

    /// The body (at most `limit` bytes) and its content type.
    pub async fn fetch(&self, raw: &str, limit: usize) -> Result<(Vec<u8>, String), SvcError> {
        let u = allowed(raw).ok_or(SvcError::BadRequest("host not allowed"))?;
        if let Some(until) = *self.cooldown_until.lock().unwrap() {
            let now = Instant::now();
            if until > now {
                return Err(SvcError::RateLimited(secs_ceil(until - now)));
            }
        }
        let target = self.target(&u);
        let attempts = self.cfg.attempts.max(1);
        for attempt in 0..attempts {
            let _permit = self.pool.acquire().await.map_err(|_| SvcError::Upstream)?;
            self.spaced().await;
            let resp = self
                .http
                .get(&target)
                .header(reqwest::header::REFERER, REFERER)
                .header(reqwest::header::ACCEPT, "image/*,*/*;q=0.8")
                .send()
                .await;
            let mut resp = match resp {
                Ok(r) => r,
                Err(_) => {
                    if attempt + 1 < attempts {
                        drop(_permit);
                        tokio::time::sleep(backoff(self.cfg.backoff_base, attempt)).await;
                        continue;
                    }
                    return Err(SvcError::Upstream);
                }
            };
            let status = resp.status();
            if status.as_u16() == 429 || status.is_server_error() {
                let retry_after = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                    .map(|s| Duration::from_secs(s.min(30)));
                if attempt + 1 < attempts {
                    drop(_permit);
                    let wait = retry_after.unwrap_or_else(|| backoff(self.cfg.backoff_base, attempt));
                    tokio::time::sleep(wait).await;
                    continue;
                }
                if status.as_u16() == 429 {
                    let cool = retry_after.unwrap_or(self.cfg.cooldown).max(Duration::from_secs(1));
                    *self.cooldown_until.lock().unwrap() = Some(Instant::now() + cool);
                    return Err(SvcError::RateLimited(secs_ceil(cool)));
                }
                return Err(SvcError::Upstream);
            }
            if status.as_u16() == 404 || status.as_u16() == 410 {
                return Err(SvcError::NotFound);
            }
            if !status.is_success() {
                return Err(SvcError::Upstream);
            }
            let ctype = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase())
                .filter(|v| v.starts_with("image/"))
                .unwrap_or_else(|| "image/jpeg".to_string());
            let mut body = Vec::new();
            loop {
                match resp.chunk().await {
                    Ok(Some(chunk)) => {
                        if body.len() + chunk.len() > limit {
                            return Err(SvcError::Upstream);
                        }
                        body.extend_from_slice(&chunk);
                    }
                    Ok(None) => break,
                    Err(_) => return Err(SvcError::Upstream),
                }
            }
            return Ok((body, ctype));
        }
        Err(SvcError::Upstream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pximg_is_allowed() {
        assert!(allowed("https://i.pximg.net/img-master/img/2024/01/01/a_p0_master1200.jpg").is_some());
        for bad in [
            "http://i.pximg.net/a.jpg",
            "https://i.pximg.net.evil.test/a.jpg",
            "https://evil.test/i.pximg.net/a.jpg",
            "https://user@i.pximg.net/a.jpg",
            "https://i.pximg.net:8443/a.jpg",
            "https://www.pixiv.net/ajax/illust/1",
            "https://s.pximg.net/a.jpg",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert!(allowed(bad).is_none(), "{bad}");
        }
    }
}
