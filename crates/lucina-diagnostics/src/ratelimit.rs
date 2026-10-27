//! Token buckets: one per device (the report's `app` + `device`) and one for
//! everything together, so a crash loop on one machine cannot fill the disk.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

struct Bucket {
    tokens: f64,
    at: Instant,
}

pub struct Limits {
    burst: f64,
    per_sec: f64,
    global_per_sec: f64,
    global_cap: f64,
    devices: Mutex<HashMap<String, Bucket>>,
    global: Mutex<Bucket>,
}

/// Devices remembered at most; idle full buckets are dropped first.
const MAX_DEVICES: usize = 10_000;

fn take(b: &mut Bucket, now: Instant, cap: f64, rate: f64) -> Result<(), Duration> {
    b.tokens = (b.tokens + now.duration_since(b.at).as_secs_f64() * rate).min(cap);
    b.at = now;
    if b.tokens >= 1.0 {
        b.tokens -= 1.0;
        Ok(())
    } else {
        Err(Duration::from_secs_f64(
            ((1.0 - b.tokens) / rate).ceil().max(1.0),
        ))
    }
}

impl Limits {
    pub fn new(burst: u32, per_hour: u32, global_per_hour: u32) -> Self {
        // Ten minutes of the global rate, at least five device bursts.
        let global_cap = (burst as f64 * 5.0).max(global_per_hour as f64 / 6.0);
        Self {
            global_cap,
            burst: burst as f64,
            per_sec: per_hour as f64 / 3600.0,
            global_per_sec: global_per_hour as f64 / 3600.0,
            devices: Mutex::new(HashMap::new()),
            global: Mutex::new(Bucket {
                tokens: global_cap,
                at: Instant::now(),
            }),
        }
    }

    /// One report from `device`; `Err(retry_after)` when over the limit.
    pub fn check(&self, device: &str) -> Result<(), Duration> {
        self.check_at(device, Instant::now())
    }

    pub fn check_at(&self, device: &str, now: Instant) -> Result<(), Duration> {
        {
            let mut d = self.devices.lock().unwrap();
            if d.len() >= MAX_DEVICES && !d.contains_key(device) {
                let (burst, rate) = (self.burst, self.per_sec);
                d.retain(|_, b| b.tokens + now.duration_since(b.at).as_secs_f64() * rate < burst);
            }
            let b = d.entry(device.to_owned()).or_insert(Bucket {
                tokens: self.burst,
                at: now,
            });
            take(b, now, self.burst, self.per_sec)?;
        }
        take(
            &mut self.global.lock().unwrap(),
            now,
            self.global_cap,
            self.global_per_sec,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_then_refill_per_device() {
        let l = Limits::new(3, 3600, 100_000);
        let t = Instant::now();
        for _ in 0..3 {
            assert!(l.check_at("mac", t).is_ok());
        }
        let wait = l.check_at("mac", t).unwrap_err();
        assert_eq!(wait, Duration::from_secs(1));
        assert!(
            l.check_at("iphone", t).is_ok(),
            "another device has its own bucket"
        );
        assert!(l.check_at("mac", t + Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn global_cap() {
        let l = Limits::new(1, 3600, 1);
        let t = Instant::now();
        let mut ok = 0;
        for i in 0..20 {
            if l.check_at(&format!("d{i}"), t).is_ok() {
                ok += 1;
            }
        }
        assert_eq!(ok, 5, "global bucket holds 5 x burst");
    }
}
