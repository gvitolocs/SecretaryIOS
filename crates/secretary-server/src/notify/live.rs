//! The approval Live Activity on the owner's iPhone (`docs/notifications.md`, "Live Activity").
//!
//! - **Tokens** (`live-activity-tokens.json`, 0600): the app sends its push-to-start token
//!   (`kind: start`, one per device) and, for each activity it shows, that activity's update token
//!   (`kind: update`, one per device and approval). `POST /v1/push/liveactivity`.
//! - **Start**: a new approval request sends [`start_body`] to every start token, so the request is on the lock
//!   screen with Approve / Deny even when the app is closed.
//! - **End**: a decision, a withdrawal or the expiry sends [`end_body`] to that approval's update tokens and
//!   forgets them.
//!
//! Both go out as `apns-push-type: liveactivity` to `<topic>.push-type.liveactivity`
//! ([`super::apns::Apns::send_live_activity`]), only while APNs is on. Tokens are never logged.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::model::{clip, now_ms};
use super::push::{classify, Delivery};
use super::store::JsonFile;
use super::Notifier;

pub const LIVE_TOKENS_FILE: &str = "live-activity-tokens.json";
/// Update tokens older than this are dropped on the next write (an approval lasts minutes).
pub const UPDATE_TOKEN_TTL: Duration = Duration::from_secs(2 * 24 * 3600);
/// Upper bound of stored tokens; the oldest go first.
pub const MAX_TOKENS: usize = 256;
/// The Swift `ActivityAttributes` type the push-to-start body names.
pub const ATTRIBUTES_TYPE: &str = "ApprovalActivityAttributes";
/// Command lines the activity shows, and the characters of each.
pub const COMMAND_LINES: usize = 3;
const COMMAND_CHARS: usize = 120;
const SUMMARY_CHARS: usize = 160;
const REQUESTER_CHARS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LiveKind {
    /// Push-to-start: starts a new activity on the device.
    Start,
    /// One activity's own token: updates or ends it.
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveToken {
    pub device_id: String,
    pub kind: LiveKind,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    pub created_at: i64,
}

pub struct LiveTokens {
    file: JsonFile<Vec<LiveToken>>,
}

impl LiveTokens {
    pub fn open(dir: &Path) -> Self {
        Self {
            file: JsonFile::open(dir, LIVE_TOKENS_FILE),
        }
    }

    pub fn list(&self) -> Vec<LiveToken> {
        self.file.get()
    }

    /// Save `t`: one start token per device, one update token per (device, approval); update tokens older
    /// than [`UPDATE_TOKEN_TTL`] are dropped.
    pub fn register(&self, t: LiveToken) -> std::io::Result<()> {
        let cutoff = t.created_at - i64::try_from(UPDATE_TOKEN_TTL.as_millis()).unwrap_or(i64::MAX);
        self.file.update(|all| {
            all.retain(|o| {
                let same = o.device_id == t.device_id
                    && o.kind == t.kind
                    && (o.kind == LiveKind::Start || o.approval_id == t.approval_id);
                let old = o.kind == LiveKind::Update && o.created_at < cutoff;
                !same && !old && o.token != t.token
            });
            all.push(t);
            while all.len() > MAX_TOKENS {
                all.remove(0);
            }
        })
    }

    /// The push-to-start tokens (one per device).
    pub fn starts(&self) -> Vec<LiveToken> {
        self.file.read(|all| {
            all.iter()
                .filter(|t| t.kind == LiveKind::Start)
                .cloned()
                .collect()
        })
    }

    /// Remove and return the update tokens of `approval_id`.
    pub fn take_updates(&self, approval_id: &str) -> std::io::Result<Vec<LiveToken>> {
        let of = |t: &LiveToken| {
            t.kind == LiveKind::Update && t.approval_id.as_deref() == Some(approval_id)
        };
        if !self.file.read(|all| all.iter().any(of)) {
            return Ok(Vec::new());
        }
        self.file.update(|all| {
            let (taken, kept) = std::mem::take(all).into_iter().partition(of);
            *all = kept;
            taken
        })
    }

    /// Forget a token APNs called dead.
    pub fn remove(&self, token: &str) -> std::io::Result<()> {
        if !self.file.read(|all| all.iter().any(|t| t.token == token)) {
            return Ok(());
        }
        self.file.update(|all| all.retain(|t| t.token != token))
    }
}

/// What the activity shows of an approval request (the Swift `ApprovalActivityAttributes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityRequest {
    pub approval_id: String,
    pub requester: String,
    pub summary: String,
    /// The request's `details` (a root plan's commands, one per line).
    pub details: String,
    pub sensitive: bool,
    pub payload_hash: String,
    pub expires_ms: i64,
}

/// The first [`COMMAND_LINES`] non-empty lines of `details` (each clipped) and how many there are.
pub fn command_lines(details: &str) -> (String, usize) {
    let lines: Vec<&str> = details
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let head: Vec<String> = lines
        .iter()
        .take(COMMAND_LINES)
        .map(|l| clip(l, COMMAND_CHARS))
        .collect();
    (head.join("\n"), lines.len())
}

/// The push-to-start body for `r` at `now_s` (seconds). Its `alert` has no sound: the approval notification already rings.
pub fn start_body(r: &ActivityRequest, now_s: i64) -> Value {
    let (commands, count) = command_lines(&r.details);
    let expires_s = r.expires_ms.div_euclid(1000);
    json!({
        "aps": {
            "timestamp": now_s,
            "event": "start",
            "content-state": {"status": "pending"},
            "attributes-type": ATTRIBUTES_TYPE,
            "attributes": {
                "approvalId": r.approval_id,
                "requester": clip(&r.requester, REQUESTER_CHARS),
                "summary": clip(&r.summary, SUMMARY_CHARS),
                "commands": commands,
                "commandCount": count,
                "sensitive": r.sensitive,
                "payloadHash": r.payload_hash,
                "expiresAtUnix": expires_s,
            },
            // iOS drops a push-to-start without an alert. No sound: the approval
            // notification itself already buzzes.
            "alert": {
                "title": format!("{} needs your OK", clip(&r.requester, REQUESTER_CHARS)),
                "body": clip(&r.summary, SUMMARY_CHARS),
            },
            "stale-date": expires_s,
            "relevance-score": 100,
            "input-push-token": 1,
        }
    })
}

/// How long an ended activity stays on the Lock Screen, so its outcome can be read.
pub const END_LINGER_SECS: i64 = 4;

/// The body that ends an activity with `status` (`approved`, `denied`, `expired`, `withdrawn`), dismissed
/// [`END_LINGER_SECS`] later.
pub fn end_body(status: &str, now_s: i64) -> Value {
    json!({
        "aps": {
            "timestamp": now_s,
            "event": "end",
            "content-state": {"status": status},
            "dismissal-date": now_s + END_LINGER_SECS,
        }
    })
}

/// One live-activity push with the pusher's retries; a dead token is forgotten.
async fn deliver(
    n: &Notifier,
    apns: &super::apns::Apns,
    t: &LiveToken,
    body: &Value,
    expiration: u64,
) -> Delivery {
    let delays = n.pusher.cfg.retry_delays.clone();
    let mut attempt = 0;
    let d = loop {
        let err = match apns.send_live_activity(&t.token, body, expiration).await {
            Ok((status, retry, reason)) => match classify(status, retry, reason) {
                Ok(d) => break d,
                Err(e) => e,
            },
            Err(e) => (e, None),
        };
        let Some(base) = delays.get(attempt) else {
            break Delivery::Failed { error: err.0 };
        };
        tokio::time::sleep(
            err.1
                .map_or(*base, |r| r.min(Duration::from_secs(60)).max(*base)),
        )
        .await;
        attempt += 1;
    };
    if let Delivery::Gone { .. } = d {
        if let Err(e) = n.live.remove(&t.token) {
            tracing::warn!(error = %e, "could not update the live activity tokens");
        }
    }
    d
}

fn now_s() -> i64 {
    now_ms().div_euclid(1000)
}

/// Start the activity for `r` on every device with a push-to-start token (APNs on only).
pub async fn send_start(n: Arc<Notifier>, r: ActivityRequest) {
    let Some(apns) = n.pusher.apns.get() else {
        return;
    };
    let body = start_body(&r, now_s());
    let expiration = u64::try_from(r.expires_ms.div_euclid(1000)).unwrap_or(0);
    let mut set = tokio::task::JoinSet::new();
    for t in n.live.starts() {
        let (n, apns, body) = (n.clone(), apns.clone(), body.clone());
        set.spawn(async move {
            let d = deliver(&n, &apns, &t, &body, expiration).await;
            (t.device_id, d)
        });
    }
    while let Some(Ok((device, d))) = set.join_next().await {
        match d {
            Delivery::Sent { .. } => {
                tracing::info!(approval = %r.approval_id, device = %device, "live activity start sent")
            }
            Delivery::Gone { status } => {
                tracing::info!(approval = %r.approval_id, device = %device, status, "live activity start token expired; removed")
            }
            Delivery::Failed { error } => {
                tracing::warn!(approval = %r.approval_id, device = %device, error = %error, "live activity start failed")
            }
        }
    }
}

/// End `approval_id`'s activities with `status` and forget their tokens (also while APNs is off: they
/// are of no use once the request is over).
pub async fn send_end(n: Arc<Notifier>, approval_id: String, status: &'static str) {
    let tokens = match n.live.take_updates(&approval_id) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "could not update the live activity tokens");
            return;
        }
    };
    let Some(apns) = n.pusher.apns.get().filter(|_| !tokens.is_empty()) else {
        return;
    };
    let now = now_s();
    let body = end_body(status, now);
    let expiration = u64::try_from(now).unwrap_or(0) + 3600;
    let mut set = tokio::task::JoinSet::new();
    for t in tokens {
        let (n, apns, body) = (n.clone(), apns.clone(), body.clone());
        set.spawn(async move {
            let d = deliver(&n, &apns, &t, &body, expiration).await;
            (t.device_id, d)
        });
    }
    while let Some(Ok((device, d))) = set.join_next().await {
        match d {
            Delivery::Sent { .. } => {
                tracing::info!(approval = %approval_id, device = %device, status, "live activity end sent")
            }
            Delivery::Gone { .. } => {}
            Delivery::Failed { error } => {
                tracing::warn!(approval = %approval_id, device = %device, error = %error, "live activity end failed")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ActivityRequest {
        ActivityRequest {
            approval_id: "apr_AbCdEfGhIjKlMnOpQrStUv".into(),
            requester: "Lucina".into(),
            summary: "Restart the tunnel on nezopt".into(),
            details: "systemctl restart cloudflared\n\njournalctl -u cloudflared -n 20\nsystemctl status cloudflared\nuptime\n".into(),
            sensitive: true,
            payload_hash: format!("sha256:{}", "ab".repeat(32)),
            expires_ms: 1_761_048_300_500,
        }
    }

    #[test]
    fn start_body_is_exact() {
        let b = start_body(&sample(), 1_761_048_000);
        assert_eq!(
            b,
            json!({"aps": {
                "timestamp": 1_761_048_000,
                "event": "start",
                "content-state": {"status": "pending"},
                "attributes-type": "ApprovalActivityAttributes",
                "attributes": {
                    "approvalId": "apr_AbCdEfGhIjKlMnOpQrStUv",
                    "requester": "Lucina",
                    "summary": "Restart the tunnel on nezopt",
                    "commands": "systemctl restart cloudflared\njournalctl -u cloudflared -n 20\nsystemctl status cloudflared",
                    "commandCount": 4,
                    "sensitive": true,
                    "payloadHash": format!("sha256:{}", "ab".repeat(32)),
                    "expiresAtUnix": 1_761_048_300,
                },
                "alert": {"title": "Lucina needs your OK", "body": "Restart the tunnel on nezopt"},
                "stale-date": 1_761_048_300,
                "relevance-score": 100,
                "input-push-token": 1,
            }})
        );
        assert!(b["aps"]["alert"].get("sound").is_none());
    }

    #[test]
    fn long_text_is_clipped() {
        let mut r = sample();
        r.summary = "s".repeat(500);
        r.details = format!("{}\nshort", "x".repeat(500));
        let b = start_body(&r, 0);
        let a = &b["aps"]["attributes"];
        assert_eq!(a["summary"].as_str().unwrap().chars().count(), 160);
        let cmds = a["commands"].as_str().unwrap();
        assert_eq!(cmds.lines().next().unwrap().chars().count(), 120);
        assert!(cmds.ends_with("…\nshort"), "{cmds}");
        assert_eq!(a["commandCount"], 2);
        r.details = String::new();
        assert_eq!(command_lines(&r.details), (String::new(), 0));
    }

    #[test]
    fn end_body_is_exact() {
        assert_eq!(
            end_body("denied", 1_761_048_010),
            json!({"aps": {
                "timestamp": 1_761_048_010,
                "event": "end",
                "content-state": {"status": "denied"},
                "dismissal-date": 1_761_048_014,
            }})
        );
    }

    fn tok(
        device: &str,
        kind: LiveKind,
        token: &str,
        approval: Option<&str>,
        at: i64,
    ) -> LiveToken {
        LiveToken {
            device_id: device.into(),
            kind,
            token: token.into(),
            approval_id: approval.map(str::to_owned),
            created_at: at,
        }
    }

    #[test]
    fn tokens_upsert_dedupe_and_expire() {
        let dir = tempfile::tempdir().unwrap();
        let s = LiveTokens::open(dir.path());
        let now = 10 * 24 * 3600 * 1000;
        s.register(tok("dev_a", LiveKind::Start, "aa", None, now))
            .unwrap();
        s.register(tok("dev_a", LiveKind::Start, "ab", None, now))
            .unwrap();
        s.register(tok("dev_b", LiveKind::Start, "bb", None, now))
            .unwrap();
        let starts: Vec<String> = s.starts().into_iter().map(|t| t.token).collect();
        assert_eq!(starts, ["ab", "bb"], "one start token per device");

        let three_days = 3 * 24 * 3600 * 1000;
        s.register(tok(
            "dev_a",
            LiveKind::Update,
            "u0",
            Some("apr_old"),
            now - three_days,
        ))
        .unwrap();
        s.register(tok("dev_a", LiveKind::Update, "u1", Some("apr_1"), now))
            .unwrap();
        s.register(tok("dev_a", LiveKind::Update, "u2", Some("apr_1"), now))
            .unwrap();
        s.register(tok("dev_b", LiveKind::Update, "u3", Some("apr_1"), now))
            .unwrap();
        s.register(tok("dev_a", LiveKind::Update, "u4", Some("apr_2"), now))
            .unwrap();
        let updates: Vec<String> = s
            .list()
            .into_iter()
            .filter(|t| t.kind == LiveKind::Update)
            .map(|t| t.token)
            .collect();
        assert_eq!(
            updates,
            ["u2", "u3", "u4"],
            "per (device, approval); old ones dropped"
        );

        let taken: Vec<String> = s
            .take_updates("apr_1")
            .unwrap()
            .into_iter()
            .map(|t| t.token)
            .collect();
        assert_eq!(taken, ["u2", "u3"]);
        assert!(s.take_updates("apr_1").unwrap().is_empty());
        s.remove("ab").unwrap();
        assert_eq!(s.list().len(), 2);
        // Persisted.
        assert_eq!(LiveTokens::open(dir.path()).list(), s.list());
    }
}
