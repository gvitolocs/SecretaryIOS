//! Push delivery: Web Push (VAPID, RFC 8030/8291/8292) for browsers and
//! home-screen web apps, and the APNs adapter for the native app (off until
//! configured, see [`super::apns`]).
//!
//! - **VAPID keys** are generated on first start into `vapid.json` (0600)
//!   in the data dir: the P-256 private key and the public key browsers get
//!   as `applicationServerKey`. Rotating them (delete the file) invalidates
//!   every browser subscription.
//! - **Subscriptions** live in `push-subscriptions.json` (0600; it holds the
//!   per-subscription `auth` secrets). One per browser / home-screen app,
//!   upserted by endpoint.
//! - **Payloads** are encrypted with `aes128gcm` (RFC 8291) by
//!   `web-push-native`, so the push service never sees the text. The VAPID
//!   JWT (RFC 8292: ES256, `aud` = the push service origin, `exp` ≤ 24 h,
//!   `sub` = `SECRETARY_VAPID_SUBJECT`) is signed here with `p256`.
//! - **Endpoints** must be `https` on a known push service
//!   ([`DEFAULT_PUSH_HOSTS`], `SECRETARY_PUSH_HOSTS` to change), so a
//!   subscription cannot make the server call arbitrary URLs. Redirects are
//!   not followed.
//! - **Delivery** retries 429, 5xx and network errors with backoff
//!   ([`DEFAULT_RETRY_DELAYS`], honouring `Retry-After` up to 5 minutes).
//!   404/410 mean the subscription expired: it is removed. Other 4xx count
//!   as failures; after [`MAX_FAILURES`] in a row the subscription is
//!   dropped.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use web_push_native::{Auth, WebPushBuilder};

use super::apns::{Apns, ApnsConfig};
use super::model::{now_ms, Category};
use super::store::JsonFile;
use crate::devices::save_json_atomic;

pub const VAPID_FILE: &str = "vapid.json";
pub const SUBSCRIPTIONS_FILE: &str = "push-subscriptions.json";
pub const DEFAULT_PUSH_HOSTS: &[&str] = &[
    "fcm.googleapis.com",
    "push.services.mozilla.com",
    "push.apple.com",
    "notify.windows.com",
];
pub const DEFAULT_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(15),
    Duration::from_secs(60),
];
pub const MAX_FAILURES: u32 = 10;
pub const MAX_SUBSCRIPTIONS: usize = 32;
/// How long a push service keeps an undelivered message.
pub const DEFAULT_TTL: Duration = Duration::from_secs(6 * 3600);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);
const MAX_ENDPOINT_LEN: usize = 1024;

#[derive(Debug, Clone)]
pub struct PushConfig {
    /// VAPID `sub`: a `mailto:` or `https:` contact for push services.
    pub subject: String,
    /// Allowed push-service host suffixes.
    pub hosts: Vec<String>,
    /// Accept `http://` endpoints (tests, local mock push services).
    pub allow_http: bool,
    pub retry_delays: Vec<Duration>,
    pub timeout: Duration,
    pub ttl: Duration,
    pub apns: Option<ApnsConfig>,
}

impl Default for PushConfig {
    fn default() -> Self {
        Self {
            subject: "https://getsecretary.tech".into(),
            hosts: DEFAULT_PUSH_HOSTS.iter().map(|s| (*s).to_owned()).collect(),
            allow_http: false,
            retry_delays: DEFAULT_RETRY_DELAYS.to_vec(),
            timeout: Duration::from_secs(15),
            ttl: DEFAULT_TTL,
            apns: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Webpush,
    Apns,
}

/// One push target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subscription {
    /// `ps_…`
    pub id: String,
    pub kind: Kind,
    /// The authenticated device that registered it.
    pub device_id: String,
    pub device_name: String,
    /// "iPhone · Safari", from the client.
    pub label: String,
    /// Web Push endpoint, or the APNs device token.
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub p256dh: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth: String,
    /// `en` | `it` | `da`, for push titles.
    #[serde(default)]
    pub lang: Option<String>,
    pub created_at: i64,
    #[serde(default)]
    pub last_success_at: Option<i64>,
    #[serde(default)]
    pub failures: u32,
    #[serde(default)]
    pub last_error: Option<String>,
}

/// What clients see of a subscription (no endpoint secrets).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionView {
    pub id: String,
    pub kind: Kind,
    pub device_id: String,
    pub device_name: String,
    pub label: String,
    /// The push service host (`web.push.apple.com`, `fcm.googleapis.com`).
    pub service: String,
    pub created_at: i64,
    pub last_success_at: Option<i64>,
    pub failures: u32,
    pub last_error: Option<String>,
}

impl Subscription {
    pub fn view(&self) -> SubscriptionView {
        let service = match self.kind {
            Kind::Apns => "apns".to_owned(),
            Kind::Webpush => reqwest::Url::parse(&self.endpoint)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default(),
        };
        SubscriptionView {
            id: self.id.clone(),
            kind: self.kind,
            device_id: self.device_id.clone(),
            device_name: self.device_name.clone(),
            label: self.label.clone(),
            service,
            created_at: self.created_at,
            last_success_at: self.last_success_at,
            failures: self.failures,
            last_error: self.last_error.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VapidFile {
    private_key: String,
    public_key: String,
    created_at: i64,
}

/// The server's VAPID key pair.
pub struct Vapid {
    key: SigningKey,
    /// base64url uncompressed point: the browser's `applicationServerKey`.
    pub public_key: String,
}

impl Vapid {
    /// Load `vapid.json`, or generate and write it (0600).
    pub fn load_or_create(dir: &Path) -> anyhow::Result<Self> {
        let path = dir.join(VAPID_FILE);
        if path.exists() {
            let raw: VapidFile = serde_json::from_slice(&std::fs::read(&path)?)
                .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            let bytes = URL_SAFE_NO_PAD
                .decode(raw.private_key.trim())
                .map_err(|_| anyhow::anyhow!("{}: privateKey is not base64url", path.display()))?;
            let key = SigningKey::from_slice(&bytes).map_err(|_| {
                anyhow::anyhow!("{}: privateKey is not a P-256 key", path.display())
            })?;
            let public_key = public_b64(&key);
            if raw.public_key != public_key {
                tracing::warn!(
                    "vapid.json publicKey does not match privateKey; using the derived one"
                );
            }
            return Ok(Self { key, public_key });
        }
        let key = loop {
            let mut seed = [0u8; 32];
            getrandom::fill(&mut seed).map_err(|e| anyhow::anyhow!("random: {e}"))?;
            if let Ok(k) = SigningKey::from_slice(&seed) {
                break k;
            }
        };
        let public_key = public_b64(&key);
        let file = VapidFile {
            private_key: URL_SAFE_NO_PAD.encode(key.to_bytes()),
            public_key: public_key.clone(),
            created_at: now_ms(),
        };
        save_json_atomic(dir, VAPID_FILE, &file)?;
        tracing::info!("generated VAPID keys for Web Push");
        Ok(Self { key, public_key })
    }

    /// `Authorization: vapid t=<JWT>, k=<public key>` for `endpoint`.
    pub fn authorization(
        &self,
        endpoint: &str,
        subject: &str,
        valid: Duration,
    ) -> Result<String, String> {
        let url = reqwest::Url::parse(endpoint).map_err(|_| "bad endpoint".to_owned())?;
        let aud = url.origin().ascii_serialization();
        let exp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
            + valid.min(Duration::from_secs(12 * 3600)).as_secs();
        let header = json!({"typ": "JWT", "alg": "ES256"});
        let claims = json!({"aud": aud, "exp": exp, "sub": subject});
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let sig: Signature = self.key.sign(input.as_bytes());
        Ok(format!(
            "vapid t={input}.{}, k={}",
            URL_SAFE_NO_PAD.encode(sig.to_bytes()),
            self.public_key
        ))
    }
}

fn public_b64(key: &SigningKey) -> String {
    URL_SAFE_NO_PAD.encode(key.verifying_key().to_encoded_point(false).as_bytes())
}

/// Lenient base64url (also standard alphabet and padding).
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let clean: String = s
        .trim()
        .trim_end_matches('=')
        .chars()
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            c => c,
        })
        .collect();
    URL_SAFE_NO_PAD.decode(clean).ok()
}

/// What a client sends to `POST /v1/push/subscribe` (`PushSubscription.toJSON()`
/// plus a label and language).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscribeBody {
    pub endpoint: String,
    pub keys: SubscribeKeys,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub lang: Option<String>,
    /// Ignored (`PushSubscription.toJSON()` sends it).
    #[serde(default)]
    pub expiration_time: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SubscribeKeys {
    pub p256dh: String,
    pub auth: String,
}

pub fn valid_lang(l: &str) -> bool {
    matches!(l, "en" | "it" | "da")
}

pub fn clean_label(label: Option<&str>) -> String {
    let l: String = label
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    let l = l.trim();
    if l.is_empty() {
        "Browser".into()
    } else {
        l.to_owned()
    }
}

impl PushConfig {
    /// Check an endpoint: scheme, host allowlist, length.
    pub fn check_endpoint(&self, endpoint: &str) -> Result<(), String> {
        if endpoint.len() > MAX_ENDPOINT_LEN {
            return Err("endpoint: too long".into());
        }
        let url = reqwest::Url::parse(endpoint).map_err(|_| "endpoint: not a URL".to_owned())?;
        match url.scheme() {
            "https" => {}
            "http" if self.allow_http => {}
            _ => return Err("endpoint: must be https".into()),
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("endpoint: no credentials allowed".into());
        }
        let host = url
            .host_str()
            .ok_or_else(|| "endpoint: no host".to_owned())?
            .to_ascii_lowercase();
        let ok = self.hosts.iter().any(|h| {
            let h = h.trim().to_ascii_lowercase();
            !h.is_empty() && (host == h || host.ends_with(&format!(".{h}")))
        });
        if !ok {
            return Err("endpoint: not a known push service".into());
        }
        Ok(())
    }
}

/// Validate the subscription keys; `(p256dh, auth)` normalised to base64url.
pub fn check_keys(keys: &SubscribeKeys) -> Result<(String, String), String> {
    let p = b64_decode(&keys.p256dh).ok_or("keys.p256dh: not base64url")?;
    if p.len() != 65 || p256::PublicKey::from_sec1_bytes(&p).is_err() {
        return Err("keys.p256dh: not an uncompressed P-256 point".into());
    }
    let a = b64_decode(&keys.auth).ok_or("keys.auth: not base64url")?;
    if a.len() != 16 {
        return Err("keys.auth: must be 16 bytes".into());
    }
    Ok((URL_SAFE_NO_PAD.encode(p), URL_SAFE_NO_PAD.encode(a)))
}

/// Per-push options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOpts {
    pub urgent: bool,
    /// Collapse key: RFC 8030 `Topic`, APNs `apns-collapse-id`.
    pub topic: Option<String>,
}

impl PushOpts {
    pub fn for_category(c: Category) -> Self {
        Self {
            urgent: c.urgent(),
            topic: c.collapse_key().map(str::to_owned),
        }
    }

    /// [`Self::for_category`], plus a per-key topic for categories whose
    /// `dedupKey` names a replaceable slot ([`Category::replaces_by_key`]).
    pub fn for_notification(n: &super::Notification) -> Self {
        let mut opts = Self::for_category(n.category);
        if let Some(k) = replace_key(n) {
            opts.topic = Some(key_topic(k));
        }
        opts
    }
}

fn replace_key(n: &super::Notification) -> Option<&str> {
    n.dedup_key
        .as_deref()
        .filter(|_| n.category.replaces_by_key())
}

/// RFC 8030 `Topic` (≤ 32 base64url characters) and APNs collapse id for a
/// dedup key: the first 24 bytes of its SHA-256, base64url.
pub fn key_topic(key: &str) -> String {
    use sha2::{Digest, Sha256};
    URL_SAFE_NO_PAD.encode(&Sha256::digest(key.as_bytes())[..24])
}

/// The notification tag a client shows it under: a newer one with the same
/// tag replaces it.
pub fn tag_for(n: &super::Notification) -> String {
    if let Some(k) = replace_key(n) {
        return format!("k:{}", key_topic(k));
    }
    n.category
        .collapse_key()
        .map_or_else(|| n.id.clone(), str::to_owned)
}

/// The JSON a service worker (or the app) receives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub category: String,
    pub title: String,
    pub body: String,
    pub route: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    pub ts: i64,
    /// Inbox unread count (badge).
    pub unread: usize,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub digest: bool,
    /// Approvals only: the web route again as `url`, the approval's id and
    /// whether approving it needs a biometric proof.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<bool>,
}

/// Result of one delivery (after retries).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "result")]
pub enum Delivery {
    Sent {
        status: u16,
    },
    /// The subscription is gone (404/410); it was removed.
    Gone {
        status: u16,
    },
    Failed {
        error: String,
    },
}

impl Delivery {
    pub fn ok(&self) -> bool {
        matches!(self, Delivery::Sent { .. })
    }
}

enum Attempt {
    Done(Delivery),
    Retry(String, Option<Duration>),
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let v = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?;
    v.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Classify an HTTP answer from a push service (shared with APNs).
pub(crate) fn classify(
    status: u16,
    retry: Option<Duration>,
    detail: String,
) -> Result<Delivery, (String, Option<Duration>)> {
    match status {
        200..=299 => Ok(Delivery::Sent { status }),
        404 | 410 => Ok(Delivery::Gone { status }),
        429 | 500..=599 => Err((format!("{status} {detail}").trim().to_owned(), retry)),
        _ => Ok(Delivery::Failed {
            error: format!("{status} {detail}").trim().to_owned(),
        }),
    }
}

pub struct Pusher {
    pub cfg: PushConfig,
    pub vapid: Vapid,
    pub subs: JsonFile<Vec<Subscription>>,
    http: reqwest::Client,
    pub apns: Option<Apns>,
}

impl Pusher {
    pub fn open(dir: &Path, cfg: PushConfig) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("secretary-server/", env!("CARGO_PKG_VERSION")))
            .build()?;
        let apns = match &cfg.apns {
            Some(a) => Some(Apns::new(a.clone())?),
            None => None,
        };
        Ok(Self {
            vapid: Vapid::load_or_create(dir)?,
            subs: JsonFile::open(dir, SUBSCRIPTIONS_FILE),
            http,
            apns,
            cfg,
        })
    }

    pub fn list(&self) -> Vec<Subscription> {
        self.subs.get()
    }

    /// Add or refresh (same endpoint) a subscription.
    pub fn upsert(&self, mut sub: Subscription) -> std::io::Result<Subscription> {
        self.subs.update(|subs| {
            if let Some(old) = subs.iter_mut().find(|s| s.endpoint == sub.endpoint) {
                sub.id = old.id.clone();
                sub.created_at = old.created_at;
                *old = sub.clone();
            } else {
                subs.push(sub.clone());
                // Keep the newest MAX_SUBSCRIPTIONS.
                while subs.len() > MAX_SUBSCRIPTIONS {
                    subs.remove(0);
                }
            }
            sub
        })
    }

    pub fn remove_where(&self, pred: impl Fn(&Subscription) -> bool) -> std::io::Result<usize> {
        if !self.subs.read(|subs| subs.iter().any(&pred)) {
            return Ok(0);
        }
        self.subs.update(|subs| {
            let before = subs.len();
            subs.retain(|s| !pred(s));
            before - subs.len()
        })
    }

    fn record(&self, id: &str, d: &Delivery) {
        let now = now_ms();
        let res = self.subs.update(|subs| match d {
            Delivery::Sent { .. } => {
                if let Some(s) = subs.iter_mut().find(|s| s.id == id) {
                    s.last_success_at = Some(now);
                    s.failures = 0;
                    s.last_error = None;
                }
            }
            Delivery::Gone { .. } => subs.retain(|s| s.id != id),
            Delivery::Failed { error } => {
                if let Some(s) = subs.iter_mut().find(|s| s.id == id) {
                    s.failures += 1;
                    s.last_error = Some(error.chars().take(200).collect());
                }
                subs.retain(|s| s.failures < MAX_FAILURES);
            }
        });
        if let Err(e) = res {
            tracing::warn!(error = %e, "could not update push subscriptions");
        }
    }

    /// Deliver `payload` to `sub` with retries, then record the result.
    pub async fn deliver(
        &self,
        sub: &Subscription,
        payload: &Payload,
        opts: &PushOpts,
    ) -> Delivery {
        let delays = self.cfg.retry_delays.clone();
        let mut attempt = 0;
        let result = loop {
            let a = match sub.kind {
                Kind::Webpush => self.try_webpush(sub, payload, opts).await,
                Kind::Apns => match &self.apns {
                    Some(apns) => match apns.send(&sub.endpoint, payload, opts).await {
                        Ok((status, retry, detail)) => match classify(status, retry, detail) {
                            Ok(d) => Attempt::Done(d),
                            Err((e, r)) => Attempt::Retry(e, r),
                        },
                        Err(e) => Attempt::Retry(e, None),
                    },
                    None => Attempt::Done(Delivery::Failed {
                        error: "apns is not configured".into(),
                    }),
                },
            };
            match a {
                Attempt::Done(d) => break d,
                Attempt::Retry(error, retry) => {
                    let Some(base) = delays.get(attempt) else {
                        break Delivery::Failed { error };
                    };
                    let wait = retry.map_or(*base, |r| r.min(MAX_RETRY_AFTER).max(*base));
                    tracing::info!(sub = %sub.id, attempt, wait_ms = wait.as_millis() as u64, "push failed; retrying");
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                }
            }
        };
        match &result {
            Delivery::Sent { .. } => {}
            Delivery::Gone { status } => {
                tracing::info!(sub = %sub.id, status, "push subscription expired; removed")
            }
            Delivery::Failed { error } => {
                tracing::warn!(sub = %sub.id, error = %error, "push failed")
            }
        }
        self.record(&sub.id, &result);
        result
    }

    async fn try_webpush(&self, sub: &Subscription, payload: &Payload, opts: &PushOpts) -> Attempt {
        let req = match self.build_webpush(sub, payload, opts) {
            Ok(r) => r,
            Err(e) => return Attempt::Done(Delivery::Failed { error: e }),
        };
        match self.http.execute(req).await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let retry = retry_after(&resp);
                let detail: String = resp
                    .text()
                    .await
                    .unwrap_or_default()
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(120)
                    .collect();
                match classify(status, retry, detail) {
                    Ok(d) => Attempt::Done(d),
                    Err((e, r)) => Attempt::Retry(e, r),
                }
            }
            Err(e) => Attempt::Retry(
                if e.is_timeout() {
                    "timeout".into()
                } else {
                    "network error".into()
                },
                None,
            ),
        }
    }

    /// The encrypted, VAPID-signed request (public for tests).
    pub fn build_webpush(
        &self,
        sub: &Subscription,
        payload: &Payload,
        opts: &PushOpts,
    ) -> Result<reqwest::Request, String> {
        let uri: axum::http::Uri = sub
            .endpoint
            .parse()
            .map_err(|_| "bad endpoint".to_owned())?;
        let p = b64_decode(&sub.p256dh).ok_or("bad p256dh")?;
        let key = p256::PublicKey::from_sec1_bytes(&p).map_err(|_| "bad p256dh".to_owned())?;
        let a = b64_decode(&sub.auth)
            .filter(|a| a.len() == 16)
            .ok_or("bad auth")?;
        let body = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
        let mut req = WebPushBuilder::new(uri, key, Auth::clone_from_slice(&a))
            .with_valid_duration(self.cfg.ttl)
            .build(body)
            .map_err(|e| format!("encrypt: {e}"))?;
        let vapid = self
            .vapid
            .authorization(&sub.endpoint, &self.cfg.subject, self.cfg.ttl)?;
        let h = req.headers_mut();
        h.insert(
            "authorization",
            axum::http::HeaderValue::from_str(&vapid).map_err(|e| e.to_string())?,
        );
        h.insert(
            "urgency",
            axum::http::HeaderValue::from_static(if opts.urgent { "high" } else { "normal" }),
        );
        if let Some(t) = opts.topic.as_deref() {
            if let Ok(v) = axum::http::HeaderValue::from_str(t) {
                h.insert("topic", v);
            }
        }
        reqwest::Request::try_from(req).map_err(|e| e.to_string())
    }
}

/// Deliver to many targets concurrently; returns `(sub id, result)`.
pub async fn deliver_all(
    pusher: Arc<super::Notifier>,
    targets: Vec<(Subscription, Payload)>,
    opts: PushOpts,
) -> Vec<(String, Delivery)> {
    let mut set = tokio::task::JoinSet::new();
    for (sub, payload) in targets {
        let n = pusher.clone();
        let opts = opts.clone();
        set.spawn(async move {
            let d = n.pusher.deliver(&sub, &payload, &opts).await;
            (sub.id, d)
        });
    }
    let mut out = Vec::new();
    while let Some(r) = set.join_next().await {
        if let Ok(v) = r {
            out.push(v);
        }
    }
    out
}

/// JSON for the APNs body (`aps` + our fields).
pub fn apns_body(payload: &Payload) -> Value {
    // The app registers the notification category "APPROVAL" (Approve/Deny actions).
    let aps_category = if payload.approval_id.is_some() {
        "APPROVAL"
    } else {
        payload.category.as_str()
    };
    let mut body = json!({
        "aps": {
            "alert": {"title": payload.title, "body": payload.body},
            "sound": "default",
            "badge": payload.unread,
            "thread-id": payload.category,
            "category": aps_category,
        },
        "id": payload.id,
        "route": payload.route,
        "category": payload.category,
    });
    if let Some(id) = &payload.approval_id {
        body["approvalId"] = json!(id);
        body["sensitive"] = json!(payload.sensitive.unwrap_or(false));
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PushConfig {
        PushConfig::default()
    }

    #[test]
    fn endpoint_allowlist() {
        let c = cfg();
        for ok in [
            "https://fcm.googleapis.com/fcm/send/abc",
            "https://web.push.apple.com/QGx",
            "https://updates.push.services.mozilla.com/wpush/v2/x",
            "https://wns2-par02p.notify.windows.com/w/?token=x",
        ] {
            assert!(c.check_endpoint(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://fcm.googleapis.com/x",
            "https://evil.example/x",
            "https://fcm.googleapis.com.evil.example/x",
            "https://user:pw@fcm.googleapis.com/x",
            "https://127.0.0.1/x",
            "not a url",
        ] {
            assert!(c.check_endpoint(bad).is_err(), "{bad}");
        }
        let mut c = cfg();
        c.allow_http = true;
        c.hosts = vec!["127.0.0.1".into()];
        assert!(c.check_endpoint("http://127.0.0.1:9000/push/1").is_ok());
    }

    #[test]
    fn keys_are_checked() {
        let sk = p256::SecretKey::from_slice(&[7u8; 32]).unwrap();
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        let pt = sk.public_key().to_encoded_point(false);
        let good = SubscribeKeys {
            p256dh: URL_SAFE_NO_PAD.encode(pt.as_bytes()),
            auth: URL_SAFE_NO_PAD.encode([1u8; 16]),
        };
        assert!(check_keys(&good).is_ok());
        // Standard alphabet with padding is accepted too.
        let std_b64 = base64::engine::general_purpose::STANDARD.encode([1u8; 16]);
        assert!(check_keys(&SubscribeKeys {
            auth: std_b64,
            ..good.clone()
        })
        .is_ok());
        assert!(check_keys(&SubscribeKeys {
            auth: "AAAA".into(),
            ..good.clone()
        })
        .is_err());
        assert!(check_keys(&SubscribeKeys {
            p256dh: URL_SAFE_NO_PAD.encode([4u8; 65]),
            ..good
        })
        .is_err());
    }

    #[test]
    fn vapid_keys_persist_with_0600() {
        let dir = tempfile::tempdir().unwrap();
        let a = Vapid::load_or_create(dir.path()).unwrap();
        let b = Vapid::load_or_create(dir.path()).unwrap();
        assert_eq!(a.public_key, b.public_key);
        assert_eq!(b64_decode(&a.public_key).unwrap().len(), 65);
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.path().join(VAPID_FILE))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn classify_statuses() {
        assert_eq!(
            classify(201, None, String::new()),
            Ok(Delivery::Sent { status: 201 })
        );
        assert_eq!(
            classify(410, None, String::new()),
            Ok(Delivery::Gone { status: 410 })
        );
        assert_eq!(
            classify(404, None, String::new()),
            Ok(Delivery::Gone { status: 404 })
        );
        assert!(classify(429, None, String::new()).is_err());
        assert!(classify(503, None, String::new()).is_err());
        assert!(matches!(
            classify(403, None, "x".into()),
            Ok(Delivery::Failed { .. })
        ));
    }
}
