//! Per-device token bucket, in memory: `per_minute` requests of burst,
//! refilled continuously at `per_minute / 60` per second.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct RateLimiter {
    per_minute: u32,
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
}

impl RateLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute: per_minute.max(1),
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Take one token for `key`; `Err(wait)` when the bucket is empty.
    pub fn check(&self, key: &str) -> Result<(), Duration> {
        self.check_at(key, Instant::now())
    }

    fn check_at(&self, key: &str, now: Instant) -> Result<(), Duration> {
        let cap = f64::from(self.per_minute);
        let rate = cap / 60.0;
        let mut buckets = self.buckets.lock().unwrap_or_else(|p| p.into_inner());
        let (tokens, last) = buckets.entry(key.to_owned()).or_insert((cap, now));
        let elapsed = now.saturating_duration_since(*last).as_secs_f64();
        *tokens = (*tokens + elapsed * rate).min(cap);
        *last = now;
        if *tokens >= 1.0 {
            *tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - *tokens) / rate))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_drains_and_refills() {
        let rl = RateLimiter::new(60);
        let t0 = Instant::now();
        for _ in 0..60 {
            assert!(rl.check_at("a", t0).is_ok());
        }
        let wait = rl.check_at("a", t0).unwrap_err();
        assert!(wait <= Duration::from_secs(1));
        // other devices are independent
        assert!(rl.check_at("b", t0).is_ok());
        // one second later one more request fits
        assert!(rl.check_at("a", t0 + Duration::from_secs(1)).is_ok());
        assert!(rl.check_at("a", t0 + Duration::from_secs(1)).is_err());
    }
}
