//! Everything that leaves the machine.
//!
//! - **API** (`storyview.yodams.com/api/v1/*`): one request at a time with a minimum spacing
//!   (env, default and floor 1000 ms). 429 and 5xx are retried with exponential backoff plus
//!   jitter, honouring `Retry-After`; when the retries run out on a 429 the service refuses to
//!   call the backend again until the `Retry-After` (or a default) has passed, and the client
//!   gets a clean `429 rate_limited`.
//! - **Media** (Instagram CDN): only `*.cdninstagram.com` and `*.fbcdn.net`, https, no
//!   redirects, a small concurrency and a spacing of its own (a CDN is not the rate-limited
//!   API; one request per second would make a profile page take a minute). Streams to a file
//!   and enforces a size limit while it goes.

use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;

use crate::media::{allowed_cdn_url, ctype_from_url};

#[derive(Debug, Clone)]
pub struct UpstreamConfig {
    /// `https://storyview.yodams.com` (tests: the mock server).
    pub base: String,
    /// Tests only: serve `https://<allowlisted host>/<rest>` from `<override>/<host>/<rest>`.
    pub media_base_override: Option<String>,
    pub timeout: Duration,
    pub api_interval: Duration,
    pub media_interval: Duration,
    pub media_concurrency: usize,
    pub user_agent: String,
    /// Retries after the first attempt, for 429 and 5xx on the API.
    pub max_retries: u32,
    pub backoff_base: Duration,
    pub backoff_cap: Duration,
    /// How long to refuse the backend after a 429 with no usable `Retry-After`.
    pub default_block: Duration,
    pub max_image_bytes: u64,
    pub max_video_bytes: u64,
}

impl Default for UpstreamConfig {
    fn default() -> Self {
        UpstreamConfig {
            base: instarust_core::BASE_URL.to_string(),
            media_base_override: None,
            timeout: Duration::from_secs(30),
            api_interval: Duration::from_millis(1000),
            media_interval: Duration::from_millis(250),
            media_concurrency: 2,
            user_agent: "InstaRust/0.1.0".into(),
            max_retries: 2,
            backoff_base: Duration::from_secs(2),
            backoff_cap: Duration::from_secs(30),
            default_block: Duration::from_secs(60),
            max_image_bytes: 32 * 1024 * 1024,
            max_video_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UpstreamError {
    /// The backend refused (after the retries) or is being left alone; seconds to wait.
    #[error("rate limited, retry after {0}s")]
    RateLimited(u64),
    /// A 4xx/5xx from upstream that is not worth retrying (or ran out of retries).
    #[error("upstream status {0}")]
    Status(u16),
    #[error("upstream unreachable")]
    Network,
    #[error("upstream body too large")]
    TooLarge,
    #[error("host not allowed")]
    Forbidden,
    #[error("upstream sent something that is not an image or video")]
    NotMedia,
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

    /// Wait for this caller's slot (slots are `interval` apart).
    async fn wait(&self) {
        let at = {
            let mut next = self.next.lock().await;
            let at = (*next).max(Instant::now());
            *next = at + self.interval;
            at
        };
        tokio::time::sleep_until(at).await;
    }
}

pub struct Upstream {
    cfg: UpstreamConfig,
    http: reqwest::Client,
    /// Earliest start of the next API request; held for a whole request (and its retries) so
    /// there is never more than one in flight.
    api_gate: Mutex<Instant>,
    blocked_until: std::sync::Mutex<Option<Instant>>,
    media_limiter: Limiter,
    media_slots: Semaphore,
    /// Requests from the owner that are waiting for (or holding) the API gate; the warmer yields
    /// to them.
    users_waiting: AtomicUsize,
    pub api_requests: AtomicU64,
}

const MAX_JSON: usize = 8 * 1024 * 1024;

fn jitter_ms(max: u64) -> u64 {
    let mut b = [0u8; 8];
    if getrandom::fill(&mut b).is_err() || max == 0 {
        return 0;
    }
    u64::from_le_bytes(b) % max
}

/// `Retry-After` in seconds (the delta form; an HTTP date is treated as absent).
fn retry_after(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

impl Upstream {
    pub fn new(cfg: UpstreamConfig) -> anyhow::Result<Upstream> {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(cfg.user_agent.clone())
            .build()?;
        Ok(Upstream {
            api_gate: Mutex::new(Instant::now()),
            blocked_until: std::sync::Mutex::new(None),
            media_limiter: Limiter::new(cfg.media_interval),
            media_slots: Semaphore::new(cfg.media_concurrency.max(1)),
            users_waiting: AtomicUsize::new(0),
            api_requests: AtomicU64::new(0),
            cfg,
            http,
        })
    }

    /// Seconds left of a block set by an earlier 429.
    pub fn blocked_for(&self) -> Option<u64> {
        let g = self.blocked_until.lock().unwrap_or_else(|e| e.into_inner());
        let until = (*g)?;
        let now = Instant::now();
        (until > now).then(|| (until - now).as_secs() + 1)
    }

    fn block(&self, secs: u64) {
        let mut g = self.blocked_until.lock().unwrap_or_else(|e| e.into_inner());
        *g = Some(Instant::now() + Duration::from_secs(secs));
    }

    /// `GET <base><path_and_query>` for the owner, the body as text. Owner requests go first:
    /// the background warmer (`api_get_low`) waits until none is pending.
    pub async fn api_get(&self, path_and_query: &str) -> Result<String, UpstreamError> {
        struct Pending<'a>(&'a AtomicUsize);
        impl Drop for Pending<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        self.users_waiting.fetch_add(1, Ordering::SeqCst);
        let _pending = Pending(&self.users_waiting);
        self.api_get_inner(path_and_query).await
    }

    /// The same request for the background warmer: yields to any owner request, and shares the
    /// one gate, spacing, backoff and 429 block.
    pub async fn api_get_low(&self, path_and_query: &str) -> Result<String, UpstreamError> {
        while self.users_waiting.load(Ordering::SeqCst) > 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.api_get_inner(path_and_query).await
    }

    async fn api_get_inner(&self, path_and_query: &str) -> Result<String, UpstreamError> {
        if let Some(s) = self.blocked_for() {
            return Err(UpstreamError::RateLimited(s));
        }
        let url = format!("{}{}", self.cfg.base.trim_end_matches('/'), path_and_query);
        let mut gate = self.api_gate.lock().await;
        // Another caller may have hit a 429 while this one waited for the gate.
        if let Some(s) = self.blocked_for() {
            return Err(UpstreamError::RateLimited(s));
        }
        let mut attempt = 0u32;
        loop {
            tokio::time::sleep_until(*gate).await;
            self.api_requests.fetch_add(1, Ordering::Relaxed);
            let sent = self
                .http
                .get(&url)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await;
            *gate = Instant::now() + self.cfg.api_interval;
            let mut resp = match sent {
                Ok(r) => r,
                Err(_) => return Err(UpstreamError::Network),
            };
            let status = resp.status().as_u16();
            if (200..300).contains(&status) {
                let mut body = Vec::new();
                while let Some(chunk) = resp.chunk().await.map_err(|_| UpstreamError::Network)? {
                    if body.len() + chunk.len() > MAX_JSON {
                        return Err(UpstreamError::TooLarge);
                    }
                    body.extend_from_slice(&chunk);
                }
                return String::from_utf8(body).map_err(|_| UpstreamError::Network);
            }
            let retryable = status == 429 || (500..600).contains(&status);
            if !retryable {
                return Err(UpstreamError::Status(status));
            }
            let hinted = retry_after(&resp);
            if attempt < self.cfg.max_retries {
                let backoff = self.cfg.backoff_base * 2u32.pow(attempt)
                    + Duration::from_millis(jitter_ms(
                        (self.cfg.backoff_base.as_millis() as u64 / 4).max(1),
                    ));
                let wait = match hinted {
                    Some(s) => Duration::from_secs(s),
                    None => backoff,
                };
                // A Retry-After beyond the cap is not waited out inside a request.
                if wait <= self.cfg.backoff_cap {
                    *gate = (*gate).max(Instant::now() + wait);
                    attempt += 1;
                    continue;
                }
            }
            return Err(if status == 429 {
                let secs = hinted.unwrap_or(self.cfg.default_block.as_secs()).max(1);
                self.block(secs);
                UpstreamError::RateLimited(secs)
            } else {
                UpstreamError::Status(status)
            });
        }
    }

    /// Where an allowlisted CDN URL is actually requested.
    fn media_target(&self, cdn: &str) -> Result<String, UpstreamError> {
        let url = allowed_cdn_url(cdn).ok_or(UpstreamError::Forbidden)?;
        match &self.cfg.media_base_override {
            None => Ok(url),
            Some(base) => {
                let u = url::Url::parse(&url).map_err(|_| UpstreamError::Forbidden)?;
                let mut out = format!(
                    "{}/{}{}",
                    base.trim_end_matches('/'),
                    u.host_str().unwrap_or(""),
                    u.path()
                );
                if let Some(q) = u.query() {
                    out.push('?');
                    out.push_str(q);
                }
                Ok(out)
            }
        }
    }

    /// Download an allowlisted CDN URL into `dest`. Returns the content type
    /// (`image/…`/`video/…` only) and the byte count. On any error `dest` is removed.
    pub async fn media_to_file(
        &self,
        cdn: &str,
        dest: &Path,
    ) -> Result<(&'static str, u64), UpstreamError> {
        let r = self.media_inner(cdn, dest).await;
        if r.is_err() {
            let _ = tokio::fs::remove_file(dest).await;
        }
        r
    }

    async fn media_inner(
        &self,
        cdn: &str,
        dest: &Path,
    ) -> Result<(&'static str, u64), UpstreamError> {
        let target = self.media_target(cdn)?;
        let _slot = self
            .media_slots
            .acquire()
            .await
            .map_err(|_| UpstreamError::Network)?;
        self.media_limiter.wait().await;
        let mut resp = self
            .http
            .get(&target)
            .header(reqwest::header::ACCEPT, "image/*,video/*;q=0.9,*/*;q=0.5")
            .send()
            .await
            .map_err(|_| UpstreamError::Network)?;
        let status = resp.status().as_u16();
        if status == 429 {
            return Err(UpstreamError::RateLimited(
                retry_after(&resp).unwrap_or(30).max(1),
            ));
        }
        if !resp.status().is_success() {
            return Err(UpstreamError::Status(status));
        }
        let header_ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default();
        // The CDN sometimes answers `binary/octet-stream` or `application/octet-stream` for
        // video; then the path extension decides. Anything else (HTML error pages…) is refused.
        let ctype: &'static str = crate::cache::MEDIA_TYPES
            .iter()
            .find(|(_, t)| *t == header_ctype)
            .map(|(_, t)| *t)
            .or_else(|| {
                (header_ctype.is_empty() || header_ctype.contains("octet-stream"))
                    .then(|| ctype_from_url(cdn))
                    .flatten()
            })
            .ok_or(UpstreamError::NotMedia)?;
        let limit = if ctype.starts_with("video/") {
            self.cfg.max_video_bytes
        } else {
            self.cfg.max_image_bytes
        };
        if resp.content_length().is_some_and(|n| n > limit) {
            return Err(UpstreamError::TooLarge);
        }
        let mut file = tokio::fs::File::create(dest)
            .await
            .map_err(|_| UpstreamError::Network)?;
        let mut written: u64 = 0;
        while let Some(chunk) = resp.chunk().await.map_err(|_| UpstreamError::Network)? {
            written += chunk.len() as u64;
            if written > limit {
                return Err(UpstreamError::TooLarge);
            }
            file.write_all(&chunk)
                .await
                .map_err(|_| UpstreamError::Network)?;
        }
        file.flush().await.map_err(|_| UpstreamError::Network)?;
        if written == 0 {
            return Err(UpstreamError::Network);
        }
        Ok((ctype, written))
    }
}
