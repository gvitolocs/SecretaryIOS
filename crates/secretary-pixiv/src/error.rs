#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SvcError {
    #[error("not found")]
    NotFound,
    #[error("bad request: {0}")]
    BadRequest(&'static str),
    /// An R-18 request or item while the caller did not send `adult=1`.
    #[error("adult content is off")]
    AdultOff,
    /// Upstream said 429 (or we are cooling down after it); retry after this many seconds.
    #[error("rate limited, retry after {0}s")]
    RateLimited(u64),
    #[error("upstream unavailable")]
    Upstream,
}
