//! Meshy errors, mapped from HTTP status and body to something Lucina can
//! say to the owner. Messages never contain the API key or a signed URL.

use crate::key::KeyError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MeshyError {
    #[error("{0}")]
    Key(#[from] KeyError),
    /// 400: Meshy refused the parameters; no task was created.
    #[error("Meshy refused the request: {0}")]
    BadRequest(String),
    /// 401: missing, wrong or revoked key.
    #[error("Meshy rejected the API key (401). Check MESHY_API_KEY_FILE; the key may be revoked.")]
    Unauthorized,
    /// 402: not enough credits, or the plan has no API access.
    #[error("Not enough Meshy credits for this (402): {0}")]
    PaymentRequired(String),
    /// 403: the plan or the key's IP allowlist does not allow it.
    #[error("Meshy refused access (403): {0}")]
    Forbidden(String),
    #[error("Meshy has no such task or model (404)")]
    NotFound,
    /// 409: e.g. deleting a task that is already running.
    #[error("Meshy conflict (409): {0}")]
    Conflict(String),
    /// 422: well-formed but not processable (e.g. pose estimation failed).
    #[error("Meshy could not process it (422): {0}")]
    Unprocessable(String),
    /// 429 "Rate limit exceeded" (with `Retry-After`).
    #[error("Meshy rate limit hit; retry in {retry_after}s")]
    RateLimited { retry_after: u64 },
    /// 429 `NoMorePendingTasks` / `NoMoreConcurrentTasks`: the queue is full.
    #[error("Meshy's task queue for this account is full; wait for a running task to finish")]
    QueueFull,
    /// 5xx.
    #[error("Meshy server error ({status})")]
    Server { status: u16 },
    /// Transport failure (DNS, TLS, timeout). The URL is never included.
    #[error("Meshy unreachable: {0}")]
    Network(String),
    #[error("unexpected answer from Meshy: {0}")]
    Decode(String),
    /// The task ended FAILED or CANCELED.
    #[error("Meshy task {task_id} {status}: {message}")]
    TaskFailed {
        task_id: String,
        status: String,
        message: String,
    },
    /// Polling gave up; the task keeps running at Meshy and is not re-created.
    #[error("Meshy task {task_id} still running after {after_s}s (kept, not re-created)")]
    Timeout { task_id: String, after_s: u64 },
    /// A result URL outside Meshy's asset hosts (never fetched).
    #[error("refused to download from {0}")]
    AssetHost(String),
    #[error("download larger than {0} MB")]
    TooLarge(u64),
}

impl MeshyError {
    /// After a create request failed this way, a task may still exist (and be
    /// charged): never resend automatically.
    pub fn create_outcome_unknown(&self) -> bool {
        matches!(self, MeshyError::Server { .. } | MeshyError::Network(_))
    }

    /// While polling, these are transient: wait and poll again.
    pub fn transient(&self) -> bool {
        matches!(
            self,
            MeshyError::Server { .. } | MeshyError::Network(_) | MeshyError::RateLimited { .. }
        )
    }

    /// A short machine code for job records and tests.
    pub fn code(&self) -> &'static str {
        match self {
            MeshyError::Key(_) => "key",
            MeshyError::BadRequest(_) => "bad_request",
            MeshyError::Unauthorized => "unauthorized",
            MeshyError::PaymentRequired(_) => "insufficient_credits",
            MeshyError::Forbidden(_) => "forbidden",
            MeshyError::NotFound => "not_found",
            MeshyError::Conflict(_) => "conflict",
            MeshyError::Unprocessable(_) => "unprocessable",
            MeshyError::RateLimited { .. } => "rate_limited",
            MeshyError::QueueFull => "queue_full",
            MeshyError::Server { .. } => "server_error",
            MeshyError::Network(_) => "network",
            MeshyError::Decode(_) => "decode",
            MeshyError::TaskFailed { .. } => "task_failed",
            MeshyError::Timeout { .. } => "timeout",
            MeshyError::AssetHost(_) => "asset_host",
            MeshyError::TooLarge(_) => "too_large",
        }
    }
}

/// Clip a server message (it is shown to the owner).
pub fn clip(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_owned()
    } else {
        let mut out: String = t.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Map a non-2xx answer. `message` is the body's `message` field (or the
/// raw body), `retry_after` the `Retry-After` header.
pub fn from_status(status: u16, message: &str, retry_after: Option<u64>) -> MeshyError {
    let msg = clip(message, 300);
    match status {
        400 => MeshyError::BadRequest(msg),
        401 => MeshyError::Unauthorized,
        402 => MeshyError::PaymentRequired(msg),
        403 => MeshyError::Forbidden(msg),
        404 => MeshyError::NotFound,
        409 => MeshyError::Conflict(msg),
        422 => MeshyError::Unprocessable(msg),
        429 => {
            if msg.starts_with("NoMorePendingTasks") || msg.starts_with("NoMoreConcurrentTasks") {
                MeshyError::QueueFull
            } else {
                MeshyError::RateLimited {
                    retry_after: retry_after.unwrap_or(5).clamp(1, 300),
                }
            }
        }
        s if s >= 500 => MeshyError::Server { status: s },
        s => MeshyError::Decode(format!("HTTP {s}: {msg}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        assert_eq!(
            from_status(401, "Invalid API key", None),
            MeshyError::Unauthorized
        );
        assert_eq!(
            from_status(402, "Insufficient credits", None).code(),
            "insufficient_credits"
        );
        assert_eq!(
            from_status(429, "Rate limit exceeded", Some(7)),
            MeshyError::RateLimited { retry_after: 7 }
        );
        assert_eq!(
            from_status(429, "NoMorePendingTasks: 10 tasks queued", None),
            MeshyError::QueueFull
        );
        assert_eq!(
            from_status(429, "NoMoreConcurrentTasks", None),
            MeshyError::QueueFull
        );
        assert_eq!(
            from_status(503, "", None),
            MeshyError::Server { status: 503 }
        );
        assert!(from_status(502, "", None).create_outcome_unknown());
        assert!(!from_status(400, "bad", None).create_outcome_unknown());
        assert!(from_status(500, "", None).transient());
        assert!(!from_status(402, "", None).transient());
        assert_eq!(from_status(422, "pose", None).code(), "unprocessable");
        assert_eq!(from_status(409, "running", None).code(), "conflict");
        let long = "x".repeat(1000);
        assert!(from_status(400, &long, None).to_string().chars().count() < 340);
    }
}
