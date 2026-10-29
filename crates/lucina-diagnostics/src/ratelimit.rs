//! Per-device token buckets for `POST /v1/diagnostics/reports`.
//!
//! A device may send `burst` reports at once and then `per_hour` an hour.
//! Repeats count too: a crash loop must not flood the service (the client
//! keeps its queue and retries after `Retry-After`).
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

pub struct RateLimiter {
    per_hour: f64,
    burst: f64,
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
}

impl RateLimiter {
    pub fn new(per_hour: u32, burst: u32) -> Self {
        Self {
            per_hour: f64::from(per_hour.max(1)),
            burst: f64::from(burst.max(1)),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Takes one token for `key`; `Err(seconds)` until the next one.
    pub fn check(&self, key: &str) -> Result<(), u64> {
        self.check_at(key, Instant::now())
    }

    pub fn check_at(&self, key: &str, now: Instant) -> Result<(), u64> {
        let rate = self.per_hour / 3600.0;
        let mut b = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        if b.len() > 10_000 {
            // Full buckets carry no state worth keeping.
            let burst = self.burst;
            b.retain(|_, (tokens, t)| {
                *tokens + now.saturating_duration_since(*t).as_secs_f64() * rate < burst
            });
        }
        let (tokens, last) = b.entry(key.to_owned()).or_insert((self.burst, now));
        let elapsed = now.saturating_duration_since(*last).as_secs_f64();
        *tokens = (*tokens + elapsed * rate).min(self.burst);
        *last = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            Ok(())
        } else {
            Err(((1.0 - *tokens) / rate).ceil().max(1.0) as u64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn burst_then_rate() {
        let rl = RateLimiter::new(60, 3);
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(rl.check_at("a", t0).is_ok());
        }
        let wait = rl.check_at("a", t0).unwrap_err();
        assert!((1..=60).contains(&wait), "{wait}");
        assert!(rl.check_at("b", t0).is_ok(), "devices are separate");
        assert!(rl.check_at("a", t0 + Duration::from_secs(61)).is_ok());
    }
}
