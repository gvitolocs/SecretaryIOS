//! When the worker may use the local Qwen: only when nobody else is.
//!
//! The only model is the local Qwen 3.8 27B behind the gateway (:11436), one
//! GPU slot shared by everyone, loaded on demand and unloaded when idle. The
//! worker is the lowest-priority user there is: before every item it reads
//! the gateway's `GET /gpu/status` and the Qwen Code lock (`qcode-run`'s
//! flock) and goes ahead only when
//!
//! - no Qwen Code task holds the lock,
//! - the gateway is not switching models, has no request running or queued
//!   and nobody waits for the GPU lease,
//! - no other engine (whisper, vision, Strata) holds the card,
//! - Qwen has been quiet for a while (the gateway's `last-used` stamp), so a
//!   batch of back-to-back requests or Qwen Code tasks is not cut into,
//! - and the model is loaded and ready, or, when it is unloaded, a cold start
//!   is allowed (a manual "ingest now", or items older than the cold-start
//!   age) and the card is entirely free.
//!
//! Anything else is "wait", with how long to wait. There is no other model
//! to fall back to: the items stay queued.

use std::fs::OpenOptions;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

/// The gateway's lease name for the dense Qwen server.
const QWEN_LEASE: &str = "llama-server";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "lowercase")]
pub enum Verdict {
    /// Use the model now; `cold`: it has to load first (long timeout).
    Go { cold: bool },
    Wait {
        reason: String,
        #[serde(skip)]
        retry_in: Duration,
    },
}

impl Verdict {
    fn wait(reason: impl Into<String>, minutes: u64) -> Self {
        Verdict::Wait {
            reason: reason.into(),
            retry_in: Duration::from_secs(minutes * 60),
        }
    }
}

fn n(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

fn names(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|e| match e {
                    Value::String(s) => s.clone(),
                    other => other
                        .get("name")
                        .or_else(|| other.get("cmd"))
                        .and_then(Value::as_str)
                        .unwrap_or("?")
                        .to_owned(),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The verdict from the gateway status, the Qwen Code lock and how long
/// Qwen has been idle (`None`: unknown, no stamp).
pub fn judge(
    status: &Value,
    qcode_busy: bool,
    allow_cold: bool,
    idle: Option<Duration>,
    quiet: Duration,
) -> Verdict {
    if qcode_busy {
        return Verdict::wait("a Qwen Code task is running", 5);
    }
    let state = status.get("state").and_then(Value::as_str).unwrap_or("");
    if state == "SWITCHING" || status.get("switching").is_some_and(|s| !s.is_null()) {
        return Verdict::wait("the gateway is switching models", 5);
    }
    let running: u64 = status
        .get("active_requests")
        .and_then(Value::as_object)
        .map(|m| m.values().filter_map(Value::as_u64).sum())
        .unwrap_or(0);
    let queued =
        n(status, "queued_requests") + n(status, "pending_dense") + n(status, "pending_strata");
    if running > 0 || queued > 0 {
        return Verdict::wait(
            format!("Qwen is busy ({running} running, {queued} queued)"),
            3,
        );
    }
    let waiters = names(status.get("lease_waiters"));
    if !waiters.is_empty() {
        return Verdict::wait(
            format!("the GPU lease has waiters ({})", waiters.join(", ")),
            5,
        );
    }
    let others: Vec<String> = names(status.get("gpu_owner"))
        .into_iter()
        .filter(|o| o != QWEN_LEASE)
        .collect();
    if !others.is_empty() {
        return Verdict::wait(format!("the GPU is held by {}", others.join(", ")), 10);
    }
    let loaded = status
        .get("model_loaded")
        .and_then(|m| m.get("qwen"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if loaded {
        if let Some(i) = idle.filter(|i| *i < quiet) {
            return Verdict::wait(
                format!(
                    "Qwen was used {} s ago; waiting for {} s of quiet",
                    i.as_secs(),
                    quiet.as_secs()
                ),
                3,
            );
        }
    }
    match state {
        "QWEN_READY" if loaded => Verdict::Go { cold: false },
        "IDLE" if !loaded => {
            let holders = names(status.get("vram_holders"));
            if !holders.is_empty() {
                Verdict::wait(format!("the GPU is in use ({})", holders.join(", ")), 10)
            } else if allow_cold {
                Verdict::Go { cold: true }
            } else {
                Verdict::wait("Qwen is unloaded; waiting until it is loaded or the queue is old enough for a cold start", 15)
            }
        }
        "QWEN_LOADING" => Verdict::wait("Qwen is loading for another client", 3),
        "ERROR" => Verdict::wait("the gateway reported an error in the last minute", 10),
        "" => Verdict::wait("the gateway status has no state", 10),
        other => Verdict::wait(format!("the gateway is {other}"), 5),
    }
}

/// Reads the gateway status and the Qwen Code lock.
#[derive(Debug, Clone)]
pub struct Gate {
    http: reqwest::Client,
    base: String,
    key: Option<PathBuf>,
    qcode_lock: Option<PathBuf>,
    /// The gateway's `last-used` stamp (epoch seconds, written when a dense
    /// request is admitted and when it ends).
    last_used: Option<PathBuf>,
    quiet: Duration,
    /// When the worker's own last request ended (epoch seconds): a stamp no
    /// later than that is ours and needs no quiet period.
    own_last: std::sync::Arc<std::sync::Mutex<Option<f64>>>,
}

fn epoch_now() -> Option<f64> {
    Some(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs_f64(),
    )
}

impl Gate {
    pub fn new(base: &str, key: Option<PathBuf>, qcode_lock: Option<PathBuf>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(8))
                .build()
                .unwrap_or_default(),
            base: base.trim_end_matches('/').to_owned(),
            key,
            qcode_lock,
            last_used: None,
            quiet: Duration::ZERO,
            own_last: Default::default(),
        }
    }

    /// Require `quiet` since the gateway's last request (`last_used` stamp).
    pub fn with_quiet(mut self, last_used: Option<PathBuf>, quiet: Duration) -> Self {
        self.last_used = last_used;
        self.quiet = quiet;
        self
    }

    /// The worker's own request just ended (its stamp is not someone else's use).
    pub fn note_own_use(&self) {
        if let Ok(mut g) = self.own_last.lock() {
            *g = epoch_now();
        }
    }

    /// How long ago someone else last had a dense request admitted or
    /// finished; `None` when unknown or when the last use was the worker's own.
    pub fn idle(&self) -> Option<Duration> {
        let s = std::fs::read_to_string(self.last_used.as_ref()?).ok()?;
        let t = s.trim().parse::<f64>().ok()?;
        let own = self.own_last.lock().ok().and_then(|g| *g);
        if own.is_some_and(|o| t <= o + 2.0) {
            return None;
        }
        Some(Duration::from_secs_f64((epoch_now()? - t).max(0.0)))
    }

    /// Whether a `qcode-run` holds its lock (probed with a non-blocking flock
    /// that is released at once).
    pub fn qcode_busy(&self) -> bool {
        let Some(p) = &self.qcode_lock else {
            return false;
        };
        let Ok(f) = OpenOptions::new().read(true).open(p) else {
            return false;
        };
        // SAFETY: flock on a descriptor we own; released before it closes.
        unsafe {
            if libc::flock(f.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) == 0 {
                libc::flock(f.as_raw_fd(), libc::LOCK_UN);
                false
            } else {
                true
            }
        }
    }

    pub async fn status(&self) -> Result<Value, String> {
        let mut req = self.http.get(format!("{}/gpu/status", self.base));
        if let Some(k) = crate::model::read_key(self.key.as_deref()) {
            req = req.bearer_auth(k);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("gateway unreachable: {}", e.without_url()))?;
        if !resp.status().is_success() {
            return Err(format!(
                "gateway status answered {}",
                resp.status().as_u16()
            ));
        }
        resp.json::<Value>()
            .await
            .map_err(|e| format!("gateway status unreadable: {}", e.without_url()))
    }

    pub async fn check(&self, allow_cold: bool) -> Verdict {
        let busy = self.qcode_busy();
        if busy {
            return judge(&Value::Null, true, allow_cold, None, self.quiet);
        }
        match self.status().await {
            Ok(s) => judge(&s, false, allow_cold, self.idle(), self.quiet),
            Err(e) => Verdict::wait(e, 10),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const Q: Duration = Duration::from_secs(120);

    fn idle() -> Value {
        json!({"state": "IDLE", "switching": null, "gpu_owner": [], "lease_waiters": [],
               "active_requests": {"qwen": 0}, "queued_requests": 0, "pending_strata": 0,
               "pending_dense": 0, "model_loaded": {"qwen": false}, "vram_holders": []})
    }

    fn ready() -> Value {
        let mut s = idle();
        s["state"] = json!("QWEN_READY");
        s["model_loaded"] = json!({"qwen": true});
        s["gpu_owner"] = json!([{"name": "llama-server", "state": "held"}]);
        s
    }

    fn waits(v: Verdict) -> String {
        match v {
            Verdict::Wait { reason, .. } => reason,
            Verdict::Go { .. } => panic!("expected wait"),
        }
    }

    #[test]
    fn go_only_when_free() {
        assert_eq!(
            judge(&ready(), false, false, None, Q),
            Verdict::Go { cold: false }
        );
        assert_eq!(
            judge(&idle(), false, true, None, Q),
            Verdict::Go { cold: true }
        );
        assert!(waits(judge(&idle(), false, false, None, Q)).contains("unloaded"));
        assert!(waits(judge(&ready(), true, true, None, Q)).contains("Qwen Code"));

        let mut s = ready();
        s["state"] = json!("QWEN_BUSY");
        s["active_requests"] = json!({"qwen": 1});
        assert!(waits(judge(&s, false, true, None, Q)).contains("busy"));

        let mut s = ready();
        s["queued_requests"] = json!(2);
        assert!(waits(judge(&s, false, true, None, Q)).contains("2 queued"));

        let mut s = idle();
        s["lease_waiters"] = json!([{"name": "lucina-vision", "state": "wait"}]);
        assert!(waits(judge(&s, false, true, None, Q)).contains("lucina-vision"));

        let mut s = idle();
        s["gpu_owner"] = json!([{"name": "whisper", "state": "held"}]);
        assert!(waits(judge(&s, false, true, None, Q)).contains("whisper"));

        let mut s = idle();
        s["vram_holders"] = json!([{"cmd": "ffmpeg", "pid": 1}]);
        assert!(waits(judge(&s, false, true, None, Q)).contains("ffmpeg"));

        let mut s = ready();
        s["switching"] = json!({"to": "strata"});
        assert!(waits(judge(&s, false, true, None, Q)).contains("switching"));

        let mut s = idle();
        s["state"] = json!("QWEN_LOADING");
        assert!(waits(judge(&s, false, true, None, Q)).contains("loading"));

        assert!(waits(judge(&json!({}), false, true, None, Q)).contains("no state"));
    }

    #[test]
    fn a_recent_request_means_wait() {
        let secs = Duration::from_secs;
        assert!(waits(judge(&ready(), false, false, Some(secs(30)), Q)).contains("30 s ago"));
        assert_eq!(
            judge(&ready(), false, false, Some(secs(200)), Q),
            Verdict::Go { cold: false }
        );
        // Unloaded means idle anyway (the stamp is about the loaded model).
        assert_eq!(
            judge(&idle(), false, true, Some(secs(5)), Q),
            Verdict::Go { cold: true }
        );
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("last-used");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        std::fs::write(&p, format!("{}\n", now - 50)).unwrap();
        let g = Gate::new("http://127.0.0.1:1", None, None).with_quiet(Some(p.clone()), Q);
        let idle = g.idle().unwrap().as_secs();
        assert!((49..=52).contains(&idle), "{idle}");
        // The stamp of the worker's own request is no reason to wait.
        g.note_own_use();
        assert_eq!(g.idle(), None);
        std::fs::write(&p, format!("{}\n", now + 30)).unwrap();
        assert!(g.idle().is_some(), "a later use is someone else's");
    }

    #[test]
    fn qcode_lock_is_probed_not_taken() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("lock");
        let gate = Gate::new("http://127.0.0.1:1", None, Some(p.clone()));
        assert!(!gate.qcode_busy(), "no lock file: not busy");
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&p)
            .unwrap();
        assert!(!gate.qcode_busy());
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
        assert!(gate.qcode_busy());
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_UN) };
        assert!(!gate.qcode_busy());
    }
}
