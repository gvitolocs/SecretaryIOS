//! Politeness towards pixiv.net: the blocking core is called one request at a time, with a
//! minimum spacing between them, exponential backoff with jitter on 429 and 5xx, and a cool-down
//! after a 429 that survived the retries (further calls fail fast with the time left, so a busy
//! upstream is not hammered by every open tab).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Mutex;

use crate::backend::{classify, Backend, Fail};
use crate::error::SvcError;

#[derive(Debug, Clone)]
pub struct GateConfig {
    /// Minimum time between the end of one upstream request and the start of the next.
    pub interval: Duration,
    /// First backoff step; doubles each attempt, plus up to 50 % jitter.
    pub backoff_base: Duration,
    /// Attempts per call (the first one included).
    pub attempts: u32,
    /// How long calls fail fast after a 429 that outlasted the retries.
    pub cooldown: Duration,
}

impl Default for GateConfig {
    fn default() -> Self {
        GateConfig {
            interval: Duration::from_millis(1500),
            backoff_base: Duration::from_secs(2),
            attempts: 3,
            cooldown: Duration::from_secs(60),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Op {
    Ranking { mode: String, page: u32 },
    Search { args: Vec<String> },
    Illust { id: String },
    SelfStatus,
}

#[derive(Default)]
struct State {
    last_end: Option<Instant>,
    cooldown_until: Option<Instant>,
}

pub struct ApiGate {
    cfg: GateConfig,
    state: Mutex<State>,
}

pub fn jitter(base: Duration) -> Duration {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    base.mul_f64((n % 500) as f64 / 1000.0)
}

pub fn backoff(base: Duration, attempt: u32) -> Duration {
    let step = base.saturating_mul(1u32 << attempt.min(6));
    step + jitter(step)
}

impl ApiGate {
    pub fn new(cfg: GateConfig) -> ApiGate {
        ApiGate {
            cfg,
            state: Mutex::new(State::default()),
        }
    }

    pub fn config(&self) -> &GateConfig {
        &self.cfg
    }

    /// Runs `op` on the backend. `Err(NotFound)` for a missing work; a search without results
    /// comes back as `Ok(Value::Null)`.
    pub async fn call(&self, backend: &Arc<dyn Backend>, op: &Op) -> Result<Value, SvcError> {
        let mut st = self.state.lock().await;
        if let Some(until) = st.cooldown_until {
            let now = Instant::now();
            if until > now {
                return Err(SvcError::RateLimited(secs_ceil(until - now)));
            }
            st.cooldown_until = None;
        }
        let attempts = self.cfg.attempts.max(1);
        for attempt in 0..attempts {
            if let Some(end) = st.last_end {
                tokio::time::sleep_until((end + self.cfg.interval).into()).await;
            }
            let b = backend.clone();
            let o = op.clone();
            let res = tokio::task::spawn_blocking(move || match o {
                Op::Ranking { mode, page } => b.ranking(&mode, page),
                Op::Search { args } => b.search(&args),
                Op::Illust { id } => b.illust(&id),
                Op::SelfStatus => b.self_status(),
            })
            .await
            .map_err(|_| SvcError::Upstream)?;
            st.last_end = Some(Instant::now());
            let e = match res {
                Ok(v) => return Ok(v),
                Err(e) => e,
            };
            match classify(&e) {
                Fail::NotFound => return Err(SvcError::NotFound),
                Fail::Empty => return Ok(Value::Null),
                Fail::Other => {
                    tracing::warn!("upstream refused a request");
                    return Err(SvcError::Upstream);
                }
                f @ (Fail::RateLimited | Fail::Transient) => {
                    if attempt + 1 < attempts {
                        tracing::warn!(attempt, "upstream busy; backing off");
                        tokio::time::sleep(backoff(self.cfg.backoff_base, attempt)).await;
                        st.last_end = Some(Instant::now());
                        continue;
                    }
                    if f == Fail::RateLimited {
                        st.cooldown_until = Some(Instant::now() + self.cfg.cooldown);
                        return Err(SvcError::RateLimited(secs_ceil(self.cfg.cooldown)));
                    }
                    return Err(SvcError::Upstream);
                }
            }
        }
        Err(SvcError::Upstream)
    }
}

pub fn secs_ceil(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_with_jitter() {
        let b = Duration::from_millis(100);
        for a in 0..4 {
            let d = backoff(b, a);
            let step = b * (1 << a);
            assert!(d >= step && d <= step + step / 2, "{a}: {d:?}");
        }
    }
}
