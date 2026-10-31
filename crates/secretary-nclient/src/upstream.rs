//! The HTTP the core leaves to the caller: one client, a configurable host, a timeout, and a
//! conservative rate limit (one request per `min_interval`, 1 s by default, API and images
//! each). Only URLs on the configured host and its `i1`/`t1` image hosts are ever fetched.

use std::time::{Duration, Instant};

use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct UpstreamConfig {
    /// The site host, e.g. `nhentai.net` (the core's default).
    pub host: String,
    /// Tests only: serve `https://<host>/…` from `<base>/…`, `https://i1.<host>/…` from
    /// `<base>/i1/…` and `https://t1.<host>/…` from `<base>/t1/…`.
    pub base_override: Option<String>,
    pub timeout: Duration,
    pub api_interval: Duration,
    pub image_interval: Duration,
    pub user_agent: String,
    /// Optional `Cookie` header (e.g. a Cloudflare clearance cookie), read from a file.
    pub cookie: Option<String>,
    /// Retries after a 429 or a 5xx (so up to `1 + max_retries` attempts per request).
    pub max_retries: u32,
    /// First backoff step; it doubles per retry, with jitter. A `Retry-After` wins when longer.
    pub backoff_base: Duration,
    /// A `Retry-After` longer than this is not waited for: the caller gets `RateLimited`.
    pub retry_after_cap: Duration,
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        UpstreamConfig {
            host: nclientv3_core::DEFAULT_HOST.to_string(),
            base_override: None,
            timeout: Duration::from_secs(20),
            api_interval: Duration::from_secs(1),
            image_interval: Duration::from_secs(1),
            user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15".into(),
            cookie: None,
            max_retries: 2,
            backoff_base: Duration::from_secs(2),
            retry_after_cap: Duration::from_secs(20),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    #[error("url outside the configured host")]
    Forbidden,
    #[error("upstream status {0}")]
    Status(u16),
    /// The site kept answering 429 (or asked for a longer wait than we hold a request for).
    /// The number is the seconds the client should wait.
    #[error("upstream rate limit, retry after {0}s")]
    RateLimited(u64),
    #[error("upstream unreachable")]
    Network,
    #[error("upstream body too large")]
    TooLarge,
}

struct Limiter {
    next: Mutex<Instant>,
    interval: Duration,
}

impl Limiter {
    fn new(interval: Duration) -> Limiter {
        Limiter {
            next: Mutex::new(Instant::now()),
            interval,
        }
    }

    async fn wait(&self) {
        let at = {
            let mut next = self.next.lock().await;
            let now = Instant::now();
            let at = (*next).max(now);
            *next = at + self.interval;
            at
        };
        tokio::time::sleep_until(at.into()).await;
    }

    /// The site asked us to slow down: no request of this kind leaves before `d` has passed.
    async fn penalize(&self, d: Duration) {
        let mut next = self.next.lock().await;
        let until = Instant::now() + d;
        if until > *next {
            *next = until;
        }
    }
}

/// `Retry-After` in whole seconds (the HTTP-date form is not used by the site).
fn retry_after(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// Exponential backoff with up to 25 % jitter.
fn backoff(base: Duration, attempt: u32) -> Duration {
    let step = base.saturating_mul(1u32 << attempt.min(6));
    let mut b = [0u8; 2];
    let _ = getrandom::fill(&mut b);
    let jitter = u32::from(u16::from_le_bytes(b) % 256); // 0..=255 of 1024
    step + step * jitter / 1024
}

pub struct Upstream {
    cfg: UpstreamConfig,
    http: reqwest::Client,
    api: Limiter,
    images: Limiter,
}

const MAX_JSON: usize = 8 * 1024 * 1024;
const MAX_IMAGE: usize = 32 * 1024 * 1024;

impl Upstream {
    pub fn new(cfg: UpstreamConfig) -> anyhow::Result<Upstream> {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(cfg.user_agent.clone())
            .build()?;
        Ok(Upstream {
            api: Limiter::new(cfg.api_interval),
            images: Limiter::new(cfg.image_interval),
            cfg,
            http,
        })
    }

    pub fn host(&self) -> &str {
        &self.cfg.host
    }

    /// The URL actually requested, or `Forbidden` for anything off the site.
    fn map(&self, url: &str) -> Result<String, UpstreamError> {
        let h = &self.cfg.host;
        for (prefix, sub) in [
            (format!("https://{h}/"), ""),
            (format!("https://i1.{h}/"), "i1/"),
            (format!("https://t1.{h}/"), "t1/"),
        ] {
            if let Some(rest) = url.strip_prefix(&prefix) {
                if rest.contains("..") {
                    return Err(UpstreamError::Forbidden);
                }
                return Ok(match &self.cfg.base_override {
                    Some(base) => format!("{}/{sub}{rest}", base.trim_end_matches('/')),
                    None => url.to_string(),
                });
            }
        }
        Err(UpstreamError::Forbidden)
    }

    async fn get(
        &self,
        url: &str,
        limit: usize,
        image: bool,
    ) -> Result<(Vec<u8>, Option<String>), UpstreamError> {
        let target = self.map(url)?;
        let limiter = if image { &self.images } else { &self.api };
        let mut attempt = 0u32;
        loop {
            limiter.wait().await;
            let mut req = self.http.get(&target);
            if let Some(c) = &self.cfg.cookie {
                req = req.header(reqwest::header::COOKIE, c);
            }
            if !image {
                req = req.header(reqwest::header::ACCEPT, "application/json");
            }
            let mut resp = req.send().await.map_err(|_| UpstreamError::Network)?;
            let status = resp.status();
            if status.as_u16() == 429 || status.is_server_error() {
                let asked = retry_after(&resp);
                let wait = backoff(self.cfg.backoff_base, attempt)
                    .max(Duration::from_secs(asked.unwrap_or(0)));
                let too_long =
                    asked.is_some_and(|s| Duration::from_secs(s) > self.cfg.retry_after_cap);
                // Everything after this one waits too, for what the site asked.
                limiter.penalize(wait).await;
                if attempt >= self.cfg.max_retries || too_long {
                    return Err(if status.as_u16() == 429 {
                        UpstreamError::RateLimited(wait.as_secs().max(1))
                    } else {
                        UpstreamError::Status(status.as_u16())
                    });
                }
                attempt += 1;
                continue;
            }
            if !status.is_success() {
                return Err(UpstreamError::Status(status.as_u16()));
            }
            let ctype = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let mut body = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|_| UpstreamError::Network)? {
                if body.len() + chunk.len() > limit {
                    return Err(UpstreamError::TooLarge);
                }
                body.extend_from_slice(&chunk);
            }
            return Ok((body, ctype));
        }
    }

    pub async fn json(&self, url: &str) -> Result<String, UpstreamError> {
        let (body, _) = self.get(url, MAX_JSON, false).await?;
        String::from_utf8(body).map_err(|_| UpstreamError::Network)
    }

    pub async fn image(&self, url: &str) -> Result<(Vec<u8>, Option<String>), UpstreamError> {
        self.get(url, MAX_IMAGE, true).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_site_is_reachable() {
        let up = Upstream::new(UpstreamConfig {
            host: "example.test".into(),
            ..Default::default()
        })
        .unwrap();
        assert!(up.map("https://example.test/api/v2/galleries/1").is_ok());
        assert!(up.map("https://i1.example.test/galleries/1/1.jpg").is_ok());
        assert!(matches!(
            up.map("https://evil.test/x"),
            Err(UpstreamError::Forbidden)
        ));
        assert!(matches!(
            up.map("https://example.test.evil.test/x"),
            Err(UpstreamError::Forbidden)
        ));
        assert!(matches!(
            up.map("https://i1.example.test/../x"),
            Err(UpstreamError::Forbidden)
        ));
    }

    #[tokio::test]
    async fn the_limiter_spaces_requests() {
        let l = Limiter::new(Duration::from_millis(100));
        let start = Instant::now();
        for _ in 0..4 {
            l.wait().await;
        }
        assert!(start.elapsed() >= Duration::from_millis(300));
    }
}
