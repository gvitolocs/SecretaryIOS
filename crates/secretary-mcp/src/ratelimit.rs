//! Sliding-window attempt limiter (login, client registration).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Limiter {
    max: usize,
    window: Duration,
    hits: Mutex<HashMap<String, Vec<Instant>>>,
}

impl Limiter {
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            max,
            window,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Record an attempt for `key`. `Err(seconds)` when over the limit
    /// (the attempt is not recorded then).
    pub fn check(&self, key: &str) -> Result<(), u64> {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap_or_else(|p| p.into_inner());
        if hits.len() > 10_000 {
            hits.retain(|_, v| {
                v.last()
                    .is_some_and(|t| now.duration_since(*t) < self.window)
            });
        }
        let v = hits.entry(key.to_owned()).or_default();
        v.retain(|t| now.duration_since(*t) < self.window);
        if v.len() >= self.max {
            let oldest = v[0];
            let wait = self.window.saturating_sub(now.duration_since(oldest));
            return Err(wait.as_secs().max(1));
        }
        v.push(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_per_key() {
        let l = Limiter::new(2, Duration::from_secs(60));
        assert!(l.check("a").is_ok());
        assert!(l.check("a").is_ok());
        let wait = l.check("a").unwrap_err();
        assert!((1..=60).contains(&wait));
        assert!(l.check("b").is_ok());
    }
}
