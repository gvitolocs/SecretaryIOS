//! Owner approvals: "when Lucina needs my permission, I get a prompt on my
//! main device and just press Yes". One contract for every requester (Lucina's
//! secrets guard and root commands, the Infisical broker, agentd, a device).
//!
//! - A **requester** (the service token `svc_…`, or a device Bearer token)
//!   asks with `POST /v1/approvals` and long-polls `GET /v1/approvals/{id}/wait`
//!   (only the identity that asked can; anyone else gets 404).
//! - An **approver** is one of the owner's devices (`require_device`, so cookie
//!   decisions need CSRF). The service token never decides (403), and a device
//!   never answers its own request (403 `self_approval`).
//! - The **main device** (a device id in `approvals-settings.json`) is asked
//!   first and alone: for [`ApprovalsConfig::escalate_after`] (30 s) only it sees
//!   and may decide the request; then every device does. No main device: all
//!   at once.
//! - **Sensitive** requests (the `action` starts with one of
//!   [`SENSITIVE_PREFIXES`], or the requester says so) are approved only with a
//!   biometric proof: a P-256 ECDSA signature over
//!   `secretary-approval-v1\n{id}\n{payloadHash}\n{decision}` made by a key that
//!   was registered to the deciding device (`POST /v1/approvals/keys`, proved by
//!   a signature of the new key over the device id and the key) and lives in its
//!   Secure Enclave behind Face ID; or, instead, with the owner's optional
//!   6-digit app PIN (`PUT /v1/approvals/pin`, an Argon2id hash in
//!   `approval-pin.json`; 5 wrong ones in a row lock it for 15 minutes).
//!   Denying never needs either.
//! - **Approver devices**: the first device to register a key does so at once.
//!   Once a device has a key, every later registration (a new device, or a
//!   replacement key) is held as pending and becomes an `approver.enroll`
//!   request of its own: sensitive, so only another keyed device (Face ID or
//!   Touch ID) can approve it, and the requesting device never can. A keyed
//!   device can revoke another device's key with a proof (or the PIN). Keyed
//!   devices see every request at once, without the main device's head start.
//! - Every sensitive decision names the device and how it was verified
//!   (`decidedVia`, e.g. "Touch ID on MacBook Air"), in the audit log, the
//!   requester's view and an ops record sent to Lucina.
//! - Approving mints a **grant**: `b64url(payload_json) + "." + b64url(sig)`,
//!   Ed25519 over `"secretary-approval-v1." + b64url(payload_json)`, key
//!   `approval-grant.key` (a different key and domain string than the "Add
//!   secret" grants of [`crate::owner_secrets`]). The requester presents it to
//!   whoever does the thing; that verifier checks the signature (public key:
//!   `GET /v1/approvals/public-key`), `exp`, `action`, the `payloadHash` of what
//!   it is about to do, and remembers the id (single use).
//! - Pending requests live in memory (a restart drops them). Settings and keys
//!   are small JSON files (0600) in the data dir. Every event is appended to
//!   `approvals-audit.jsonl`: names and ids, never secret values.
//!
//! | route | auth | |
//! |---|---|---|
//! | `POST /v1/approvals` | svc or device | create |
//! | `GET /v1/approvals/{id}/wait?timeout=` | svc or device (the requester) | long poll, at most 30 s |
//! | `POST /v1/approvals/{id}/withdraw` | svc or device (the requester) | the requester no longer needs the answer: a pending request becomes `expired` |
//! | `GET /v1/approvals/public-key` | public | the grant verification key |
//! | `GET /v1/approvals?status=pending\|all&since=&wait=` | device | what this device may decide now; with `since` (the last `cursor`) and `wait`, a long poll of at most 30 s until that changes |
//! | `GET /v1/approvals/{id}` | device | one request, decided or expired too |
//! | `POST /v1/approvals/{id}/decide` | device | approve or deny |
//! | `GET`, `PUT /v1/approvals/settings` | device | main device |
//! | `POST`, `DELETE /v1/approvals/keys` | device | the biometric key of this device (a later one waits for an `approver.enroll` approval) |
//! | `DELETE /v1/approvals/keys/{deviceId}` | device with a key | revoke another device's key (proof or PIN) |
//! | `PUT`, `DELETE /v1/approvals/pin` | device | the optional app PIN |

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use axum::extract::{Extension, Path, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::auth::{Identity, SERVICE_DEVICE_ID};
use crate::error::{ApiError, JsonBody, QueryArgs};
use crate::notify::model::{clip, now_ms};
use crate::notify::store::JsonFile;
use crate::notify::{Audience, Category, NewNotification, Outcome};
use crate::ratelimit::RateLimiter;
use crate::AppState;

pub const GRANT_KEY_FILE: &str = "approval-grant.key";
pub const SETTINGS_FILE: &str = "approvals-settings.json";
pub const KEYS_FILE: &str = "approval-keys.json";
pub const PIN_FILE: &str = "approval-pin.json";
pub const AUDIT_FILE: &str = "approvals-audit.jsonl";
/// Signed grant bytes = this + the base64url payload.
pub const GRANT_CONTEXT: &str = "secretary-approval-v1.";
/// Signed proof bytes = this + `{id}\n{payloadHash}\n{decision}`.
pub const PROOF_CONTEXT: &str = "secretary-approval-v1\n";
/// Key registration proof = this + `{deviceId}\n{publicKey}` (as sent), signed by the new key.
pub const KEY_PROOF_CONTEXT: &str = "secretary-approval-key-v1\n";
/// PIN change proof = this + `{deviceId}`, signed by the device's registered key.
pub const PIN_PROOF_CONTEXT: &str = "secretary-approval-pin-v1\n";
/// Revocation proof = this + `{callerDeviceId}\n{targetDeviceId}`, signed by the caller's registered key.
pub const REVOKE_PROOF_CONTEXT: &str = "secretary-approval-revoke-v1\n";
/// An enrollment request's `payloadHash` is the SHA-256 of this + `{deviceId}\n{publicKey}`.
pub const ENROLL_CONTEXT: &str = "secretary-approval-enroll-v1\n";
/// The action of a request to add an approver key.
pub const ENROLL_ACTION: &str = "approver.enroll";
/// Wrong PINs in a row before the lock.
pub const PIN_MAX_FAILURES: u32 = 5;
/// How long the PIN stays locked.
pub const PIN_LOCK_MS: i64 = 15 * 60 * 1000;
/// Approving an action with one of these prefixes needs a biometric proof.
pub const SENSITIVE_PREFIXES: [&str; 9] = [
    "approver.",
    "secret.",
    "root.",
    "sudo.",
    "message.send",
    "email.send",
    "payment.",
    "account.",
    "broker.",
];
const REQUESTER_KINDS: [&str; 5] = ["lucina", "broker", "agentd", "mcp", "device"];
/// Decided and expired requests stay readable (`GET /v1/approvals/{id}`) this long.
const RETAIN_MS: i64 = 3_600_000;

#[derive(Debug, Clone)]
pub struct ApprovalsConfig {
    pub default_ttl: Duration,
    /// `ttlSecs` is clamped to `min_ttl..=max_ttl` (tests lower the minimum).
    pub min_ttl: Duration,
    pub max_ttl: Duration,
    /// Until then only the main device sees and decides (tests shorten it).
    pub escalate_after: Duration,
    /// A grant is good this long after the decision (`exp = iat + grant_ttl`).
    pub grant_ttl: Duration,
    /// Longest long poll.
    pub wait_max: Duration,
    /// `POST /v1/approvals` per requester per minute.
    pub creates_per_minute: u32,
    /// Undecided requests at once (all requesters).
    pub max_open: usize,
}

impl Default for ApprovalsConfig {
    fn default() -> Self {
        Self {
            default_ttl: Duration::from_secs(300),
            min_ttl: Duration::from_secs(30),
            max_ttl: Duration::from_secs(900),
            escalate_after: Duration::from_secs(30),
            grant_ttl: Duration::from_secs(120),
            wait_max: Duration::from_secs(30),
            creates_per_minute: 20,
            max_open: 100,
        }
    }
}

fn ms(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

fn rfc3339(at_ms: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(at_ms) * 1_000_000)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_default()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 8 hex characters naming a public key.
fn key_id_of(public: &[u8]) -> String {
    hex(&Sha256::digest(public)[..4])
}

/// Whether approving `action` needs a biometric proof by its name alone.
pub fn sensitive_action(action: &str) -> bool {
    SENSITIVE_PREFIXES.iter().any(|p| action.starts_with(p))
}

// -- the grant ---------------------------------------------------------------

/// The signed part of a grant (field order is the wire order).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GrantPayload<'a> {
    v: u8,
    id: &'a str,
    action: &'a str,
    requester: GrantRequester<'a>,
    payload_hash: &'a str,
    decision: &'static str,
    decided_by: &'a str,
    biometric: bool,
    /// `device_key` or `pin`; absent when nobody was verified.
    #[serde(skip_serializing_if = "Option::is_none")]
    verified_by: Option<&'static str>,
    iat: i64,
    exp: i64,
}

#[derive(Debug, Serialize)]
struct GrantRequester<'a> {
    kind: &'a str,
    name: &'a str,
}

/// Signs grants with `approval-grant.key`.
pub struct ApprovalSigner {
    key: Ed25519KeyPair,
}

impl std::fmt::Debug for ApprovalSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApprovalSigner(..)")
    }
}

impl ApprovalSigner {
    /// Load the key, making it (0600) on first start.
    pub fn open(dir: &FsPath) -> io::Result<Self> {
        let raw = crate::owner_secrets::load_or_create(&dir.join(GRANT_KEY_FILE))?;
        let key = Ed25519KeyPair::from_pkcs8(&raw).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "approval-grant.key is not an Ed25519 PKCS#8 key",
            )
        })?;
        Ok(Self { key })
    }

    pub fn public_key(&self) -> &[u8] {
        self.key.public_key().as_ref()
    }

    pub fn key_id(&self) -> String {
        key_id_of(self.public_key())
    }

    fn sign(&self, payload: &GrantPayload<'_>) -> io::Result<String> {
        let json = serde_json::to_vec(payload).map_err(io::Error::other)?;
        let p64 = URL_SAFE_NO_PAD.encode(json);
        let sig = self.key.sign(format!("{GRANT_CONTEXT}{p64}").as_bytes());
        Ok(format!("{p64}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref())))
    }
}

// -- files -------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsDoc {
    #[serde(default)]
    main_device_id: Option<String>,
}

/// One device's biometric public key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredKey {
    key_id: String,
    /// Base64 (standard) X9.63 uncompressed P-256 point.
    public_key: String,
    label: String,
    created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct KeysDoc {
    #[serde(default)]
    keys: BTreeMap<String, StoredKey>,
    /// Keys waiting for their `approver.enroll` request, by device id. Never used to verify anything.
    #[serde(default)]
    pending: BTreeMap<String, PendingKey>,
}

/// A registration held until another approver device approves it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingKey {
    approval_id: String,
    payload_hash: String,
    key: StoredKey,
}

/// The `payloadHash` an enrollment request is bound to.
fn enroll_hash(device_id: &str, public_key: &str) -> String {
    format!(
        "sha256:{}",
        hex(&Sha256::digest(
            format!("{ENROLL_CONTEXT}{device_id}\n{public_key}").as_bytes()
        ))
    )
}

/// `approval-pin.json`: the app PIN as an Argon2id PHC string, and the
/// wrong-attempt state (it survives a restart).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PinDoc {
    #[serde(default)]
    hash: Option<String>,
    /// Wrong PINs in a row.
    #[serde(default)]
    failures: u32,
    /// Epoch ms until which the PIN is locked; 0 when it is not.
    #[serde(default)]
    locked_until_ms: i64,
}

/// Why a PIN was not accepted.
#[derive(Debug)]
enum PinError {
    NotSet,
    Wrong { left: u32 },
    Locked { retry_secs: u64 },
    Storage,
}

impl From<PinError> for ApiError {
    fn from(e: PinError) -> Self {
        match e {
            PinError::NotSet => forbidden(
                "biometric_required",
                "approving this needs Face ID on a device with a registered key (no PIN is set)",
            ),
            PinError::Wrong { left } => {
                let mut e = forbidden("pin_wrong", "the PIN is wrong");
                e.attempts_left = Some(left);
                e
            }
            PinError::Locked { retry_secs } => {
                let mut e = ApiError::new(
                    StatusCode::TOO_MANY_REQUESTS,
                    "pin_locked",
                    "too many wrong PINs; the PIN is locked for a while",
                );
                e.retry_after = Some(retry_secs.max(1));
                e
            }
            PinError::Storage => ApiError::internal("could not save the PIN state"),
        }
    }
}

/// Exactly six ASCII digits.
fn pin_format_ok(pin: &str) -> bool {
    pin.len() == 6 && pin.bytes().all(|b| b.is_ascii_digit())
}

/// `approvals-audit.jsonl`: append-only, one JSON object per line, 0600.
struct Audit {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Audit {
    fn log(&self, event: &str, fields: Value) {
        let mut line = json!({"ts": rfc3339(now_ms()), "event": event});
        if let (Some(o), Value::Object(f)) = (line.as_object_mut(), fields) {
            o.extend(f);
        }
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let res = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = res {
            tracing::warn!(error = %e, "could not append to the approvals audit log");
        }
    }
}

// -- requests ----------------------------------------------------------------

#[derive(Debug, Clone)]
struct Requester {
    kind: String,
    name: String,
    detail: Option<String>,
}

#[derive(Debug, Clone)]
struct Decided {
    approve: bool,
    at_ms: i64,
    by_id: String,
    by_name: String,
    biometric: bool,
    verified_by: Option<&'static str>,
    /// "Touch ID on MacBook Air", "PIN on iPhone", or the device's name.
    via: String,
    grant: Option<String>,
}

#[derive(Debug, Clone)]
struct Approval {
    id: String,
    /// Who asked: a device id, or `svc`. Only it can wait, and it can never decide.
    requester_id: String,
    requester: Requester,
    action: String,
    summary: String,
    details: String,
    risk: String,
    sensitive: bool,
    payload_hash: String,
    created_ms: i64,
    expires_ms: i64,
    decided: Option<Decided>,
    expiry_logged: bool,
}

impl Approval {
    /// `pending`, `approved`, `denied` or `expired` (undecided past its time).
    fn status(&self, now: i64) -> &'static str {
        match &self.decided {
            Some(d) if d.approve => "approved",
            Some(_) => "denied",
            None if now >= self.expires_ms => "expired",
            None => "pending",
        }
    }

    fn open(&self, now: i64) -> bool {
        self.decided.is_none() && now < self.expires_ms
    }
}

/// What a created request tells its requester.
#[derive(Debug, Clone)]
pub struct Created {
    pub id: String,
    pub sensitive: bool,
    pub expires_ms: i64,
    summary: String,
    details: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequesterIn {
    kind: String,
    name: String,
    #[serde(default)]
    detail: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateBody {
    requester: RequesterIn,
    action: String,
    summary: String,
    #[serde(default)]
    details: String,
    #[serde(default)]
    risk: Option<String>,
    #[serde(default)]
    sensitive: Option<bool>,
    payload_hash: String,
    #[serde(default)]
    ttl_secs: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProofIn {
    key_id: String,
    signature: String,
}

/// A bare signature (key registration, PIN changes).
#[derive(Debug, Deserialize)]
struct SigIn {
    signature: String,
}

/// Whether `sig_b64` (DER ECDSA P-256/SHA-256, base64) signs `msg` under `vk`.
fn sig_ok(vk: &VerifyingKey, msg: &[u8], sig_b64: &str) -> bool {
    crate::notify::push::b64_decode(sig_b64)
        .and_then(|b| Signature::from_der(&b).ok())
        .is_some_and(|sig| vk.verify(msg, &sig).is_ok())
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", msg)
}

fn forbidden(code: &str, msg: &str) -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, code, msg)
}

fn not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "approval_not_found",
        "no such approval request",
    )
}

fn plain(s: &str, field: &str, max: usize) -> Result<String, ApiError> {
    let t = s.trim();
    if t.is_empty() || t.chars().count() > max || t.chars().any(char::is_control) {
        return Err(bad(format!(
            "{field}: 1-{max} printable characters, no line breaks"
        )));
    }
    Ok(t.to_owned())
}

fn action_ok(a: &str) -> bool {
    (1..=64).contains(&a.len())
        && a.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

fn hash_ok(h: &str) -> bool {
    h.strip_prefix("sha256:").is_some_and(|x| {
        x.len() == 64
            && x.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

pub struct Approvals {
    pub cfg: ApprovalsConfig,
    map: Mutex<HashMap<String, Approval>>,
    signer: ApprovalSigner,
    settings: JsonFile<SettingsDoc>,
    keys: JsonFile<KeysDoc>,
    pin: JsonFile<PinDoc>,
    /// Serialises PIN checks, so parallel guesses cannot outrun the lock.
    pin_gate: Mutex<()>,
    audit: Audit,
    /// Bumped on every new request and every decision, to wake long polls.
    changed: tokio::sync::watch::Sender<u64>,
    /// Part of the list's `cursor`, so a cursor from before a restart never matches.
    started_ms: i64,
    limiter: RateLimiter,
}

impl Approvals {
    pub fn open(dir: &FsPath, cfg: ApprovalsConfig) -> anyhow::Result<Self> {
        crate::devices::ensure_dir(dir)?;
        Ok(Self {
            limiter: RateLimiter::new(cfg.creates_per_minute),
            signer: ApprovalSigner::open(dir)?,
            settings: JsonFile::open(dir, SETTINGS_FILE),
            keys: JsonFile::open(dir, KEYS_FILE),
            pin: JsonFile::open(dir, PIN_FILE),
            pin_gate: Mutex::new(()),
            audit: Audit {
                path: dir.join(AUDIT_FILE),
                lock: Mutex::new(()),
            },
            changed: tokio::sync::watch::Sender::new(0),
            started_ms: now_ms(),
            map: Mutex::new(HashMap::new()),
            cfg,
        })
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Approval>> {
        self.map.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn signer(&self) -> &ApprovalSigner {
        &self.signer
    }

    fn wake(&self) {
        self.changed.send_modify(|v| *v = v.wrapping_add(1));
    }

    /// Log expiries, drop what has been over for [`RETAIN_MS`].
    fn sweep_locked(&self, map: &mut HashMap<String, Approval>, now: i64) {
        for a in map.values_mut() {
            if a.decided.is_none() && now >= a.expires_ms && !a.expiry_logged {
                a.expiry_logged = true;
                self.audit
                    .log("expired", json!({"id": a.id, "action": a.action}));
            }
        }
        map.retain(|_, a| {
            let over = a
                .decided
                .as_ref()
                .map_or(a.expires_ms, |d| d.at_ms.max(a.expires_ms));
            now < over + RETAIN_MS
        });
    }

    /// Expire and purge now (the sweeper's tick).
    pub fn sweep(&self) {
        let now = now_ms();
        self.sweep_locked(&mut self.lock(), now);
    }

    fn create_at(&self, who: &Identity, body: CreateBody, now: i64) -> Result<Created, ApiError> {
        self.limiter
            .check(&who.device_id)
            .map_err(ApiError::rate_limited)?;
        let by_service = who.device_id == SERVICE_DEVICE_ID;
        let kind = body.requester.kind.trim().to_owned();
        if !REQUESTER_KINDS.contains(&kind.as_str()) {
            return Err(bad("requester.kind: lucina, broker, agentd, mcp or device"));
        }
        // A device cannot pass for Lucina or the broker, and the service
        // token is not a device: the prompt shows who really asks.
        let (kind, name) = if by_service {
            if kind == "device" {
                return Err(bad("requester.kind: device is for device tokens"));
            }
            (kind, plain(&body.requester.name, "requester.name", 64)?)
        } else {
            ("device".to_owned(), who.device_name.clone())
        };
        let detail = match body.requester.detail.as_deref() {
            None | Some("") => None,
            Some(d) => Some(plain(d, "requester.detail", 200)?),
        };
        let action = body.action.trim().to_owned();
        if !action_ok(&action) {
            return Err(bad("action: 1-64 of lowercase letters, digits, . _ -"));
        }
        let summary = plain(&body.summary, "summary", 200)?;
        if body.details.chars().count() > 4000 || body.details.contains('\0') {
            return Err(bad("details: at most 4000 characters"));
        }
        let risk = body.risk.as_deref().unwrap_or("medium");
        if !matches!(risk, "low" | "medium" | "high") {
            return Err(bad("risk: low, medium or high"));
        }
        if !hash_ok(&body.payload_hash) {
            return Err(bad("payloadHash: sha256:<64 lowercase hex>"));
        }
        let ttl = match body.ttl_secs {
            None => self.cfg.default_ttl,
            Some(s) => Duration::from_secs(s.max(0).unsigned_abs())
                .clamp(self.cfg.min_ttl, self.cfg.max_ttl),
        };
        let sensitive = body.sensitive.unwrap_or(false) || sensitive_action(&action);
        let id = format!(
            "apr_{}",
            crate::devices::random_b64::<16>()
                .map_err(|_| ApiError::internal("random source failed"))?
        );
        let a = Approval {
            id: id.clone(),
            requester_id: who.device_id.clone(),
            requester: Requester { kind, name, detail },
            action,
            summary,
            details: body.details,
            risk: risk.to_owned(),
            sensitive,
            payload_hash: body.payload_hash,
            created_ms: now,
            expires_ms: now + ms(ttl),
            decided: None,
            expiry_logged: false,
        };
        let mut map = self.lock();
        self.sweep_locked(&mut map, now);
        if map.values().filter(|a| a.open(now)).count() >= self.cfg.max_open {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "approvals_busy",
                "too many approval requests are waiting; try again later",
            ));
        }
        self.audit.log(
            "created",
            json!({
                "id": a.id, "requester": a.requester.kind, "requesterName": a.requester.name,
                "requesterId": a.requester_id, "action": a.action, "sensitive": a.sensitive,
                "risk": a.risk, "payloadHash": a.payload_hash, "expiresAt": rfc3339(a.expires_ms),
            }),
        );
        let created = Created {
            id,
            sensitive,
            expires_ms: a.expires_ms,
            summary: a.summary.clone(),
            details: a.details.clone(),
        };
        map.insert(a.id.clone(), a);
        drop(map);
        self.wake();
        Ok(created)
    }

    /// Whether `device_id` may decide `a` now, given the main device.
    fn visible_to(&self, a: &Approval, device_id: &str, main: Option<&str>, now: i64) -> bool {
        a.requester_id != device_id && self.in_scope(a, device_id, main, now)
    }

    /// The main device, and every approver device (one with a key), at once; the rest after the escalation.
    fn in_scope(&self, a: &Approval, device_id: &str, main: Option<&str>, now: i64) -> bool {
        match main {
            None => true,
            Some(m) => m == device_id || self.escalated(a, main, now) || self.has_key(device_id),
        }
    }

    /// Open to every device (no main device, or its head start is over).
    fn escalated(&self, a: &Approval, main: Option<&str>, now: i64) -> bool {
        main.is_none() || now >= a.created_ms + ms(self.cfg.escalate_after)
    }

    fn item(&self, a: &Approval, me: &str, main: Option<&str>, now: i64) -> Value {
        let mut requester = json!({"kind": a.requester.kind, "name": a.requester.name});
        if let Some(d) = &a.requester.detail {
            requester["detail"] = json!(d);
        }
        let mut v = json!({
            "id": a.id,
            "status": a.status(now),
            "requester": requester,
            "action": a.action,
            "summary": a.summary,
            "details": a.details,
            "risk": a.risk,
            "sensitive": a.sensitive,
            "payloadHash": a.payload_hash,
            "createdAt": rfc3339(a.created_ms),
            "expiresAt": rfc3339(a.expires_ms),
            "escalated": self.escalated(a, main, now),
            "youAreMain": main == Some(me),
        });
        if let Some(d) = &a.decided {
            v["decidedAt"] = json!(rfc3339(d.at_ms));
            v["decidedBy"] = json!(d.by_name);
            v["decidedById"] = json!(d.by_id);
            v["decidedVia"] = json!(d.via);
            v["biometric"] = json!(d.biometric);
            if let Some(by) = d.verified_by {
                v["verifiedBy"] = json!(by);
            }
        }
        v
    }

    fn list_at(&self, me: &str, main: Option<&str>, all: bool, now: i64) -> Vec<Value> {
        let mut map = self.lock();
        self.sweep_locked(&mut map, now);
        let mut items: Vec<&Approval> = map
            .values()
            .filter(|a| a.requester_id != me)
            .filter(|a| {
                if all {
                    true
                } else {
                    a.open(now) && self.visible_to(a, me, main, now)
                }
            })
            .collect();
        items.sort_by(|a, b| {
            b.created_ms
                .cmp(&a.created_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        items
            .into_iter()
            .map(|a| self.item(a, me, main, now))
            .collect()
    }

    /// When `me`'s pending list next changes by itself: a request expires, or one held for the main device
    /// escalates to `me`.
    fn next_change_ms(&self, me: &str, main: Option<&str>, now: i64) -> Option<i64> {
        let map = self.lock();
        map.values()
            .filter(|a| a.requester_id != me && a.open(now))
            .map(|a| {
                if self.in_scope(a, me, main, now) {
                    a.expires_ms
                } else {
                    (a.created_ms + ms(self.cfg.escalate_after)).min(a.expires_ms)
                }
            })
            .min()
    }

    /// Changes whenever a long poll on the list should answer; it differs after a restart too.
    fn cursor(&self) -> String {
        format!("{}.{}", self.started_ms, *self.changed.borrow())
    }

    fn get_at(&self, id: &str, me: &str, main: Option<&str>, now: i64) -> Result<Value, ApiError> {
        let mut map = self.lock();
        self.sweep_locked(&mut map, now);
        let a = map.get(id).ok_or_else(not_found)?;
        Ok(self.item(a, me, main, now))
    }

    /// The requester's view of its own request, with the grant once approved.
    fn requester_view(&self, id: &str, who: &str, now: i64) -> Result<Waited, ApiError> {
        let mut map = self.lock();
        self.sweep_locked(&mut map, now);
        let a = map
            .get(id)
            .filter(|a| a.requester_id == who)
            .ok_or_else(not_found)?;
        let mut v = json!({"id": a.id});
        let mut grant = false;
        let status = match &a.decided {
            // The grant is handed out until the request itself runs out.
            Some(d) if d.approve && now < a.expires_ms => {
                v["grant"] = json!(d.grant);
                grant = true;
                "approved"
            }
            Some(d) if d.approve => "expired",
            Some(_) => "denied",
            None if now >= a.expires_ms => "expired",
            None => "pending",
        };
        v["status"] = json!(status);
        if let Some(d) = a.decided.as_ref().filter(|_| status != "expired") {
            v["decidedAt"] = json!(rfc3339(d.at_ms));
            v["decidedBy"] = json!(d.by_name);
            v["decidedVia"] = json!(d.via);
        }
        Ok(Waited {
            pending: status == "pending",
            grant,
            expires_ms: a.expires_ms,
            action: a.action.clone(),
            json: v,
        })
    }

    /// The requester gives up: a pending request expires now (status
    /// `expired`, so old clients need no new value) and leaves every
    /// approver's list. Idempotent; a request that was already decided keeps
    /// its status. Only the identity that asked can; anyone else gets 404.
    fn withdraw_at(&self, id: &str, who: &str, now: i64) -> Result<&'static str, ApiError> {
        let mut map = self.lock();
        self.sweep_locked(&mut map, now);
        let a = map
            .get_mut(id)
            .filter(|a| a.requester_id == who)
            .ok_or_else(not_found)?;
        if !a.open(now) {
            return Ok(match &a.decided {
                Some(d) if d.approve && now < a.expires_ms => "approved",
                Some(d) if d.approve => "expired",
                Some(_) => "denied",
                None => "expired",
            });
        }
        a.expires_ms = now;
        // The audit line below replaces the sweeper's `expired` one.
        a.expiry_logged = true;
        self.audit
            .log("withdrawn", json!({"id": a.id, "action": a.action}));
        drop(map);
        self.wake();
        Ok("expired")
    }

    fn still_open(&self, id: &str, now: i64) -> bool {
        self.lock().get(id).is_some_and(|a| a.open(now))
    }

    /// The verifying key registered to `device_id`, with its id.
    fn device_key(&self, device_id: &str) -> Option<(String, VerifyingKey)> {
        let stored = self.keys.read(|k| k.keys.get(device_id).cloned())?;
        let vk = STANDARD
            .decode(&stored.public_key)
            .ok()
            .and_then(|b| VerifyingKey::from_sec1_bytes(&b).ok())?;
        Some((stored.key_id, vk))
    }

    /// Whether `proof` is a valid signature by the key registered to `device_id`.
    fn proof_ok(
        &self,
        device_id: &str,
        id: &str,
        hash: &str,
        decision: &str,
        proof: &ProofIn,
    ) -> bool {
        let Some((key_id, vk)) = self.device_key(device_id) else {
            return false;
        };
        let msg = format!("{PROOF_CONTEXT}{id}\n{hash}\n{decision}");
        proof.key_id == key_id && sig_ok(&vk, msg.as_bytes(), &proof.signature)
    }

    /// Whether `sig_b64` signs `secretary-approval-pin-v1\n{device_id}` with
    /// the key registered to `device_id`.
    fn pin_proof_ok(&self, device_id: &str, sig_b64: &str) -> bool {
        self.device_key(device_id).is_some_and(|(_, vk)| {
            sig_ok(
                &vk,
                format!("{PIN_PROOF_CONTEXT}{device_id}").as_bytes(),
                sig_b64,
            )
        })
    }

    fn has_pin(&self) -> bool {
        self.pin.read(|p| p.hash.is_some())
    }

    /// Check `pin` against the stored hash, counting the failure. Runs one
    /// Argon2 verify: call it from `spawn_blocking`.
    fn check_pin(&self, pin: &str, device_id: &str, now: i64) -> Result<(), PinError> {
        let _gate = self.pin_gate.lock().unwrap_or_else(|p| p.into_inner());
        let doc = self.pin.get();
        let Some(hash) = doc.hash.as_deref() else {
            return Err(PinError::NotSet);
        };
        if doc.locked_until_ms > now {
            let retry_secs = u64::try_from((doc.locked_until_ms - now + 999) / 1000).unwrap_or(1);
            return Err(PinError::Locked { retry_secs });
        }
        // A lock that ran out starts a fresh count.
        let failures = if doc.locked_until_ms != 0 {
            0
        } else {
            doc.failures
        };
        if crate::owner::verify_hash(pin, hash) {
            if doc.failures != 0 || doc.locked_until_ms != 0 {
                let _ = self.pin.update(|p| {
                    p.failures = 0;
                    p.locked_until_ms = 0;
                });
            }
            return Ok(());
        }
        let failures = failures + 1;
        let locked = failures >= PIN_MAX_FAILURES;
        self.pin
            .update(|p| {
                p.failures = failures;
                p.locked_until_ms = if locked { now + PIN_LOCK_MS } else { 0 };
            })
            .map_err(|e| {
                tracing::error!(error = %e, "approval PIN state not saved");
                PinError::Storage
            })?;
        if locked {
            self.audit.log("pin_locked", json!({"device": device_id}));
            Err(PinError::Locked {
                retry_secs: u64::try_from(PIN_LOCK_MS / 1000).unwrap_or(900),
            })
        } else {
            self.audit.log(
                "pin_wrong",
                json!({"device": device_id, "attemptsLeft": PIN_MAX_FAILURES - failures}),
            );
            Err(PinError::Wrong {
                left: PIN_MAX_FAILURES - failures,
            })
        }
    }

    /// The checks every decision passes first, in the contract's order. A PIN
    /// never helps with any of them.
    fn gate(
        &self,
        a: &Approval,
        who: &Identity,
        payload_hash: &str,
        main: Option<&str>,
        now: i64,
    ) -> Result<(), ApiError> {
        if a.decided.is_some() {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "approval_decided",
                "this request was already answered",
            ));
        }
        if now >= a.expires_ms {
            return Err(ApiError::new(
                StatusCode::GONE,
                "approval_expired",
                "this request expired",
            ));
        }
        if a.requester_id == who.device_id {
            return Err(forbidden(
                "self_approval",
                "a device cannot answer its own request",
            ));
        }
        if !self.in_scope(a, &who.device_id, main, now) {
            return Err(forbidden(
                "not_main_device",
                "only the main device can answer this yet",
            ));
        }
        if a.payload_hash != payload_hash {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "payload_mismatch",
                "payloadHash is not the one of this request",
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    /// Run the gate before any PIN is spent; says whether the request is
    /// sensitive and whether the device-key proof already holds.
    fn precheck(
        &self,
        who: &Identity,
        id: &str,
        approve: bool,
        payload_hash: &str,
        proof: Option<&ProofIn>,
        main: Option<&str>,
        now: i64,
    ) -> Result<Pre, ApiError> {
        let mut map = self.lock();
        self.sweep_locked(&mut map, now);
        let a = map.get(id).ok_or_else(not_found)?;
        self.gate(a, who, payload_hash, main, now)?;
        let decision = if approve { "approve" } else { "deny" };
        Ok(Pre {
            sensitive: a.sensitive,
            key_ok: approve
                && proof.is_some_and(|p| {
                    self.proof_ok(&who.device_id, id, &a.payload_hash, decision, p)
                }),
        })
    }

    /// `pin_ok`: the handler verified the body's PIN (see [`Self::check_pin`]).
    #[allow(clippy::too_many_arguments)]
    fn decide_at(
        &self,
        who: &Identity,
        id: &str,
        approve: bool,
        payload_hash: &str,
        proof: Option<&ProofIn>,
        pin_ok: bool,
        main: Option<&str>,
        now: i64,
    ) -> Result<DecidedInfo, ApiError> {
        let mut map = self.lock();
        self.sweep_locked(&mut map, now);
        let a = map.get_mut(id).ok_or_else(not_found)?;
        self.gate(a, who, payload_hash, main, now)?;
        let decision = if approve { "approve" } else { "deny" };
        let key_ok = approve
            && proof
                .is_some_and(|p| self.proof_ok(&who.device_id, id, &a.payload_hash, decision, p));
        let verified_by = if key_ok {
            Some("device_key")
        } else if approve && pin_ok {
            Some("pin")
        } else {
            None
        };
        let biometric = verified_by.is_some();
        let via = match verified_by {
            Some("device_key") => self
                .keys
                .read(|k| k.keys.get(&who.device_id).map(|s| s.label.clone()))
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| format!("device key on {}", who.device_name)),
            Some(_) => format!("PIN on {}", who.device_name),
            None => who.device_name.clone(),
        };
        if approve && a.action == ENROLL_ACTION && !key_ok {
            // Adding an approver takes another approver's own key: Face ID or Touch ID, never the PIN.
            return Err(forbidden(
                "biometric_required",
                "adding an approver device needs Face ID or Touch ID on a device that already approves",
            ));
        }
        if approve && a.sensitive && !biometric {
            return Err(forbidden(
                "biometric_required",
                "approving this needs Face ID on a device with a registered key, or the app PIN",
            ));
        }
        let grant = if approve {
            let iat = now / 1000;
            let payload = GrantPayload {
                v: 1,
                id: &a.id,
                action: &a.action,
                requester: GrantRequester {
                    kind: &a.requester.kind,
                    name: &a.requester.name,
                },
                payload_hash: &a.payload_hash,
                decision: "approved",
                decided_by: &who.device_id,
                biometric,
                verified_by,
                iat,
                exp: iat + i64::try_from(self.cfg.grant_ttl.as_secs()).unwrap_or(120),
            };
            Some(self.signer.sign(&payload).map_err(|e| {
                tracing::error!(error = %e, "approval grant could not be signed");
                ApiError::internal("approval grant key unavailable")
            })?)
        } else {
            None
        };
        a.decided = Some(Decided {
            approve,
            at_ms: now,
            by_id: who.device_id.clone(),
            by_name: who.device_name.clone(),
            biometric,
            verified_by,
            via: via.clone(),
            grant,
        });
        self.audit.log(
            "decided",
            json!({
                "id": a.id, "action": a.action, "decision": decision, "device": who.device_id,
                "deviceName": who.device_name, "biometric": biometric, "verifiedBy": verified_by,
                "via": via, "sensitive": a.sensitive,
            }),
        );
        let out = DecidedInfo {
            id: a.id.clone(),
            action: a.action.clone(),
            summary: a.summary.clone(),
            details: a.details.clone(),
            requester_id: a.requester_id.clone(),
            requester_name: a.requester.name.clone(),
            payload_hash: a.payload_hash.clone(),
            sensitive: a.sensitive,
            approve,
            via,
        };
        drop(map);
        self.wake();
        Ok(out)
    }

    fn main_device_id(&self) -> Option<String> {
        self.settings.read(|s| s.main_device_id.clone())
    }

    fn has_key(&self, device_id: &str) -> bool {
        self.keys.read(|k| k.keys.contains_key(device_id))
    }

    /// Whether a device other than `device_id` has a key (then a new key needs an approver's OK).
    fn other_key_exists(&self, device_id: &str) -> bool {
        self.keys.read(|k| k.keys.keys().any(|d| d != device_id))
    }

    /// An approved `approver.enroll` request: its pending key becomes the device's key. Only the pending key bound
    /// to this very request (id and payload hash) counts. Returns the key id when one was activated.
    fn activate_enrollment(&self, d: &DecidedInfo) -> io::Result<Option<String>> {
        self.keys.update(|k| {
            let matches = k
                .pending
                .get(&d.requester_id)
                .is_some_and(|p| p.approval_id == d.id && p.payload_hash == d.payload_hash);
            if !matches {
                return None;
            }
            let p = k.pending.remove(&d.requester_id)?;
            if d.approve {
                let key_id = p.key.key_id.clone();
                k.keys.insert(d.requester_id.clone(), p.key);
                Some(key_id)
            } else {
                None
            }
        })
    }
}

/// What a decision was about, for the follow-ups outside the lock (enrollment, the ops record).
#[derive(Debug, Clone)]
struct DecidedInfo {
    id: String,
    action: String,
    summary: String,
    details: String,
    requester_id: String,
    requester_name: String,
    payload_hash: String,
    sensitive: bool,
    approve: bool,
    via: String,
}

/// What [`Approvals::precheck`] found.
struct Pre {
    sensitive: bool,
    /// The body's device-key proof is valid.
    key_ok: bool,
}

/// A requester's poll result.
struct Waited {
    pending: bool,
    grant: bool,
    expires_ms: i64,
    action: String,
    json: Value,
}

// -- handlers ----------------------------------------------------------------

/// The main device, if it is still an active device.
fn effective_main(st: &AppState) -> Option<String> {
    let id = st.approvals.main_device_id()?;
    matches!(st.devices.active_device(&id), Ok(Some(_))).then_some(id)
}

/// `POST /v1/approvals`
async fn create(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
    JsonBody(body): JsonBody<CreateBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let created = st.approvals.create_at(&who, body, now_ms())?;
    tracing::info!(id = %created.id, requester = %who.device_id, "approval requested");
    let main = effective_main(&st);
    announce(&st, &created, main).await;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": created.id,
            "status": "pending",
            "sensitive": created.sensitive,
            "expiresAt": rfc3339(created.expires_ms),
        })),
    ))
}

/// Tell the owner: the main device first, every other device after
/// [`ApprovalsConfig::escalate_after`] if it is still undecided.
async fn announce(st: &AppState, c: &Created, main: Option<String>) {
    let mut n = NewNotification::new(Category::Approval, clip(&c.summary, 160));
    n.route = Some(format!("/approvals/{}", c.id));
    n.dedup_key = Some(format!("approval:{}", c.id));
    n.data = Some(json!({
        "approvalId": c.id,
        "sensitive": c.sensitive,
        "details": clip(&c.details, 2000),
    }));
    let audience = main.clone().map_or(Audience::All, Audience::Only);
    let scope = if main.is_some() { "main" } else { "all" };
    let stored = match st.notify.publish_to(n, &audience).await {
        Ok(Outcome::Created {
            notification,
            pushed,
            ..
        }) => {
            st.approvals.audit.log(
                "notified",
                json!({"id": c.id, "scope": scope, "targets": pushed}),
            );
            Some(notification)
        }
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(error = %e, "could not store the approval notification");
            None
        }
    };
    let (Some(main), Some(stored)) = (main, stored) else {
        return;
    };
    let st = st.clone();
    let id = c.id.clone();
    let after = st.approvals.cfg.escalate_after;
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        if !st.approvals.still_open(&id, now_ms()) {
            return;
        }
        let targets = st
            .notify
            .push_stored(&stored, &Audience::Except(main))
            .await;
        st.approvals.audit.log(
            "notified",
            json!({"id": id, "scope": "escalated", "targets": targets}),
        );
    });
}

#[derive(Deserialize)]
struct WaitQuery {
    timeout: Option<u64>,
}

/// `GET /v1/approvals/{id}/wait?timeout=25`
async fn wait(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
    Path(id): Path<String>,
    QueryArgs(q): QueryArgs<WaitQuery>,
) -> Result<Json<Value>, ApiError> {
    let ap = &st.approvals;
    let secs = q.timeout.unwrap_or(25).min(ap.cfg.wait_max.as_secs());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut changed = ap.changed.subscribe();
    let mut shutdown = st.shutdown.subscribe();
    loop {
        let now = now_ms();
        let w = ap.requester_view(&id, &who.device_id, now)?;
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if !w.pending || left.is_zero() || *shutdown.borrow() {
            if w.grant {
                ap.audit.log(
                    "waited",
                    json!({"id": id, "action": w.action, "requesterId": who.device_id}),
                );
            }
            return Ok(Json(w.json));
        }
        let to_expiry = Duration::from_millis(u64::try_from(w.expires_ms - now).unwrap_or(0) + 5);
        tokio::select! {
            _ = changed.changed() => {}
            () = tokio::time::sleep(left.min(to_expiry)) => {}
            () = crate::notify::stopped(&mut shutdown) => {}
        }
    }
}

/// `POST /v1/approvals/{id}/withdraw`
async fn withdraw(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let status = st.approvals.withdraw_at(&id, &who.device_id, now_ms())?;
    tracing::info!(id = %id, requester = %who.device_id, status, "approval withdrawn");
    Ok(Json(json!({"id": id, "status": status})))
}

/// `GET /v1/approvals/public-key`
async fn public_key(State(st): State<AppState>) -> Json<Value> {
    let s = st.approvals.signer();
    Json(json!({
        "alg": "ed25519",
        "publicKey": STANDARD.encode(s.public_key()),
        "keyId": s.key_id(),
    }))
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    status: Option<String>,
    /// The `cursor` of the list the caller already has.
    #[serde(default)]
    since: Option<String>,
    /// Seconds to hold the request while the list still matches `since` (at most `wait_max`).
    #[serde(default)]
    wait: Option<u64>,
}

/// `GET /v1/approvals?status=pending|all[&since=<cursor>&wait=25]`
///
/// With `since` and `wait`, a long poll: it answers when a request is created, decided, withdrawn, expires or
/// escalates to this device, or after `wait` seconds. The app holds one open while it is active instead of
/// polling every few seconds.
async fn list(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
    QueryArgs(q): QueryArgs<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let all = match q.status.as_deref() {
        None | Some("pending") => false,
        Some("all") => true,
        Some(_) => return Err(bad("status: pending or all")),
    };
    let ap = &st.approvals;
    let secs = q.wait.unwrap_or(0).min(ap.cfg.wait_max.as_secs());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    let mut changed = ap.changed.subscribe();
    let mut shutdown = st.shutdown.subscribe();
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        let cursor = ap.cursor();
        let main = effective_main(&st);
        let now = now_ms();
        if q.since.as_deref() != Some(cursor.as_str()) || left.is_zero() || *shutdown.borrow() {
            let items = ap.list_at(&who.device_id, main.as_deref(), all, now);
            return Ok(Json(json!({"approvals": items, "cursor": cursor})));
        }
        let by_itself = ap
            .next_change_ms(&who.device_id, main.as_deref(), now)
            .map_or(left, |at| {
                Duration::from_millis(u64::try_from(at - now).unwrap_or(0) + 5)
            });
        tokio::select! {
            _ = changed.changed() => {}
            () = tokio::time::sleep(left.min(by_itself)) => {
                // The deadline, or the list changed by itself (an expiry or an escalation).
                let items = ap.list_at(&who.device_id, main.as_deref(), all, now_ms());
                return Ok(Json(json!({"approvals": items, "cursor": ap.cursor()})));
            }
            () = crate::notify::stopped(&mut shutdown) => {}
        }
    }
}

/// `GET /v1/approvals/{id}`
async fn get_one(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let main = effective_main(&st);
    st.approvals
        .get_at(&id, &who.device_id, main.as_deref(), now_ms())
        .map(Json)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DecideBody {
    decision: String,
    payload_hash: String,
    #[serde(default)]
    proof: Option<ProofIn>,
    /// The app PIN, instead of `proof`.
    #[serde(default)]
    pin: Option<String>,
}

/// Verify a PIN off the async runtime (Argon2 is slow on purpose).
async fn verify_pin(st: &AppState, pin: String, device_id: String) -> Result<(), ApiError> {
    let ap = st.approvals.clone();
    tokio::task::spawn_blocking(move || ap.check_pin(&pin, &device_id, now_ms()))
        .await
        .map_err(|_| ApiError::internal("PIN check failed"))?
        .map_err(ApiError::from)
}

/// `POST /v1/approvals/{id}/decide`
async fn decide(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
    Path(id): Path<String>,
    JsonBody(body): JsonBody<DecideBody>,
) -> Result<Json<Value>, ApiError> {
    let approve = match body.decision.as_str() {
        "approve" => true,
        "deny" => false,
        _ => return Err(bad("decision: approve or deny")),
    };
    let main = effective_main(&st);
    let pre = st.approvals.precheck(
        &who,
        &id,
        approve,
        &body.payload_hash,
        body.proof.as_ref(),
        main.as_deref(),
        now_ms(),
    )?;
    // The PIN is the fallback: only a sensitive approve whose device-key
    // proof does not hold spends an attempt.
    let mut pin_ok = false;
    if approve && pre.sensitive && !pre.key_ok {
        if let Some(pin) = body.pin {
            if !pin_format_ok(&pin) {
                return Err(bad("pin: exactly 6 digits"));
            }
            verify_pin(&st, pin, who.device_id.clone()).await?;
            pin_ok = true;
        }
    }
    let decided = st.approvals.decide_at(
        &who,
        &id,
        approve,
        &body.payload_hash,
        body.proof.as_ref(),
        pin_ok,
        main.as_deref(),
        now_ms(),
    )?;
    tracing::info!(id = %id, device = %who.device_id, approve, "approval answered");
    if decided.action == ENROLL_ACTION {
        finish_enrollment(&st, &decided).await;
    }
    if decided.sensitive {
        record_decision(&st, &decided);
    }
    Ok(Json(
        json!({"status": if approve { "approved" } else { "denied" }}),
    ))
}

/// An answered `approver.enroll` request: activate (or drop) the pending key and tell the owner.
async fn finish_enrollment(st: &AppState, d: &DecidedInfo) {
    match st.approvals.activate_enrollment(d) {
        Ok(Some(key_id)) => {
            st.approvals.audit.log(
                "key_registered",
                json!({"device": d.requester_id, "deviceName": d.requester_name, "keyId": key_id,
                       "approvedVia": d.via, "approval": d.id}),
            );
            tell_owner(
                st,
                &format!(
                    "{} can now approve requests (added on {})",
                    d.requester_name, d.via
                ),
            )
            .await;
        }
        Ok(None) => {
            if !d.approve {
                st.approvals.audit.log(
                    "key_refused",
                    json!({"device": d.requester_id, "reason": "enrollment denied", "approval": d.id}),
                );
            }
        }
        Err(e) => tracing::error!(error = %e, "approval keys not saved"),
    }
}

/// A sensitive decision goes to Lucina's ops record, naming the device ("approved on Touch ID on MacBook Air").
/// Fire and forget: Lucina may be away; the audit log has it anyway.
fn record_decision(st: &AppState, d: &DecidedInfo) {
    if st.lucina().is_err() {
        return;
    }
    let st = st.clone();
    let verb = if d.approve { "Approved" } else { "Denied" };
    let root = d.action.starts_with("root.") || d.action.starts_with("sudo.");
    let mut body = json!({
        "kind": "decision",
        "project": if root { "nezopt" } else { "secretary" },
        "title": clip(&format!("{verb} on {}: {}", d.via, d.summary), 200),
        "body": format!("{verb} by the owner on **{}** ({}).\n\nRequested by {}.\n\n```\n{}\n```",
                        d.via, d.action, d.requester_name, clip(&d.details, 4000)),
        "decided_by": "owner",
        "question": clip(&d.summary, 4000),
        "asked_by": clip(&d.requester_name, 100),
        "dedup_key": format!("approval:{}", d.id),
    });
    if root {
        body["host"] = json!("nezopt");
    }
    tokio::spawn(async move {
        let call = secretary_core::Call::ops_record(body).with_caller("secretary:approvals");
        let Ok(lucina) = st.lucina() else { return };
        if let Err(e) = lucina.call_with_status::<Value>(&call).await {
            tracing::warn!(error = %e, "approval decision not recorded in the ops record");
        }
    });
}

fn settings_json(st: &AppState, me: &str) -> Result<Value, ApiError> {
    let main = effective_main(st);
    let devices = st.devices.store().list().map_err(|e| {
        tracing::error!(error = %e, "device registry unreadable");
        ApiError::internal("device registry unavailable")
    })?;
    let devices: Vec<Value> = devices
        .iter()
        .filter(|d| !d.revoked)
        .map(|d| {
            let mut v = json!({
                "id": d.id,
                "name": d.name,
                "isMain": main.as_deref() == Some(d.id.as_str()),
                "hasBiometricKey": st.approvals.has_key(&d.id),
            });
            st.approvals.keys.read(|k| {
                if let Some(key) = k.keys.get(&d.id) {
                    v["keyLabel"] = json!(key.label);
                    v["keyCreatedAt"] = json!(key.created_at);
                }
                if let Some(p) = k.pending.get(&d.id) {
                    v["pendingApprovalId"] = json!(p.approval_id);
                }
            });
            if let Some(seen) = &d.last_seen_at {
                v["lastSeen"] = json!(seen);
            }
            v
        })
        .collect();
    Ok(json!({
        "mainDeviceId": main,
        "escalateAfterSecs": st.approvals.cfg.escalate_after.as_secs(),
        "devices": devices,
        "thisDeviceId": me,
        "hasPin": st.approvals.has_pin(),
    }))
}

/// `GET /v1/approvals/settings`
async fn get_settings(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
) -> Result<Json<Value>, ApiError> {
    settings_json(&st, &who.device_id).map(Json)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsBody {
    /// Required: a device id, or `null` for none.
    main_device_id: Value,
}

/// `PUT /v1/approvals/settings {"mainDeviceId": "<id>|null"}`
async fn put_settings(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
    JsonBody(body): JsonBody<SettingsBody>,
) -> Result<Json<Value>, ApiError> {
    let next = match body.main_device_id {
        Value::Null => None,
        Value::String(id) => {
            match st.devices.active_device(&id) {
                Ok(Some(d)) if !d.web_login => {}
                Ok(_) => {
                    return Err(ApiError::new(
                        StatusCode::NOT_FOUND,
                        "device_not_found",
                        "mainDeviceId: no such active device",
                    ))
                }
                Err(e) => {
                    tracing::error!(error = %e, "device registry unreadable");
                    return Err(ApiError::internal("device registry unavailable"));
                }
            }
            Some(id)
        }
        _ => return Err(bad("mainDeviceId: a device id or null")),
    };
    st.approvals
        .settings
        .update(|s| s.main_device_id.clone_from(&next))
        .map_err(|e| {
            tracing::error!(error = %e, "approvals settings not saved");
            ApiError::internal("could not save the settings")
        })?;
    st.approvals.audit.log(
        "main_device_set",
        json!({"mainDeviceId": next, "by": who.device_id}),
    );
    settings_json(&st, &who.device_id).map(Json)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyBody {
    public_key: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    proof: Option<SigIn>,
}

fn proof_invalid() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "proof_invalid",
        "proof.signature must be this key's signature over the device id and the public key",
    )
}

/// Tell every device about a change to the approval setup. Not an approval
/// prompt: it carries no `approvalId`, so no client offers Approve.
async fn tell_owner(st: &AppState, text: &str) {
    let mut n = NewNotification::new(Category::Approval, clip(text, 160));
    n.title = Some("Approvals".to_owned());
    if let Err(e) = st.notify.publish(n).await {
        tracing::warn!(error = %e, "could not store the approvals notice");
    }
}

/// `POST /v1/approvals/keys {publicKey, label, proof: {signature}}`
///
/// The device authorizes itself (its token); the new key proves it exists by
/// signing `secretary-approval-key-v1\n{deviceId}\n{publicKey}`.
async fn register_key(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
    JsonBody(body): JsonBody<KeyBody>,
) -> Result<Json<Value>, ApiError> {
    let point = crate::notify::push::b64_decode(&body.public_key)
        .filter(|b| b.len() == 65 && b[0] == 0x04)
        .ok_or_else(|| bad("publicKey: base64 of an uncompressed P-256 point (65 bytes)"))?;
    let vk = VerifyingKey::from_sec1_bytes(&point)
        .map_err(|_| bad("publicKey: base64 of an uncompressed P-256 point (65 bytes)"))?;
    let label = body.label.trim();
    if label.chars().count() > 64 || label.chars().any(char::is_control) {
        return Err(bad("label: at most 64 printable characters"));
    }
    let msg = format!("{KEY_PROOF_CONTEXT}{}\n{}", who.device_id, body.public_key);
    if !body
        .proof
        .as_ref()
        .is_some_and(|p| sig_ok(&vk, msg.as_bytes(), &p.signature))
    {
        st.approvals.audit.log(
            "key_refused",
            json!({"device": who.device_id, "reason": "proof invalid"}),
        );
        return Err(proof_invalid());
    }
    let key_id = key_id_of(&point);
    let stored = StoredKey {
        key_id: key_id.clone(),
        public_key: STANDARD.encode(&point),
        label: label.to_owned(),
        created_at: crate::devices::now_rfc3339(),
    };
    if st.approvals.other_key_exists(&who.device_id) {
        return enroll(&st, &who, stored).await;
    }
    st.approvals
        .keys
        .update(|k| {
            k.keys.insert(who.device_id.clone(), stored);
        })
        .map_err(|e| {
            tracing::error!(error = %e, "approval keys not saved");
            ApiError::internal("could not save the key")
        })?;
    st.approvals.audit.log(
        "key_registered",
        json!({"device": who.device_id, "deviceName": who.device_name, "keyId": key_id, "label": label}),
    );
    tell_owner(&st, &format!("Approvals set up on {}", who.device_name)).await;
    Ok(Json(json!({"keyId": key_id})))
}

/// A key registered while another device already has one: held until an approver device approves the
/// `approver.enroll` request it opens (asked by this device, so it can wait on it but never answer it).
async fn enroll(st: &AppState, who: &Identity, key: StoredKey) -> Result<Json<Value>, ApiError> {
    let hash = enroll_hash(&who.device_id, &key.public_key);
    let label = if key.label.is_empty() {
        "a device key".to_owned()
    } else {
        key.label.clone()
    };
    let body = CreateBody {
        requester: RequesterIn {
            kind: "device".into(),
            name: who.device_name.clone(),
            detail: Some("approver set-up".into()),
        },
        action: ENROLL_ACTION.into(),
        summary: clip(&format!("Add {} as an approver device ({label})", who.device_name), 200),
        details: format!(
            "Device: {} ({})\nKey: {} ({label})\nOnce added, this device can approve root commands and other sensitive requests with its own Touch ID or Face ID.\nYou can revoke it in Settings → Approvals.",
            who.device_name, who.device_id, key.key_id
        ),
        risk: Some("high".into()),
        sensitive: Some(true),
        payload_hash: hash.clone(),
        ttl_secs: None,
    };
    let created = st.approvals.create_at(who, body, now_ms())?;
    let key_id = key.key_id.clone();
    st.approvals
        .keys
        .update(|k| {
            k.pending.insert(
                who.device_id.clone(),
                PendingKey {
                    approval_id: created.id.clone(),
                    payload_hash: hash,
                    key,
                },
            );
        })
        .map_err(|e| {
            tracing::error!(error = %e, "approval keys not saved");
            ApiError::internal("could not save the key")
        })?;
    st.approvals.audit.log(
        "key_pending",
        json!({"device": who.device_id, "deviceName": who.device_name, "keyId": key_id, "approval": created.id}),
    );
    announce(st, &created, effective_main(st)).await;
    Ok(Json(
        json!({"keyId": key_id, "status": "pending_approval", "approvalId": created.id}),
    ))
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevokeBody {
    #[serde(default)]
    proof: Option<SigIn>,
    #[serde(default)]
    pin: Option<String>,
}

/// `DELETE /v1/approvals/keys/{deviceId} {proof: {signature}} | {pin}`: revoke another device's key (or its
/// pending one). The caller must be an approver itself and prove it: its key signs
/// `secretary-approval-revoke-v1\n{callerId}\n{targetId}`, or the PIN.
async fn revoke_key(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
    Path(target): Path<String>,
    raw: axum::body::Bytes,
) -> Result<Json<Value>, ApiError> {
    let body: RevokeBody = if raw.iter().all(u8::is_ascii_whitespace) {
        RevokeBody::default()
    } else {
        serde_json::from_slice(&raw).map_err(|_| bad("body: {proof?: {signature}, pin?}"))?
    };
    let msg = format!("{REVOKE_PROOF_CONTEXT}{}\n{target}", who.device_id);
    let key_ok = body.proof.as_ref().is_some_and(|p| {
        st.approvals
            .device_key(&who.device_id)
            .is_some_and(|(_, vk)| sig_ok(&vk, msg.as_bytes(), &p.signature))
    });
    if !key_ok {
        match body.pin {
            Some(pin) if pin_format_ok(&pin) => verify_pin(&st, pin, who.device_id.clone()).await?,
            Some(_) => return Err(bad("pin: exactly 6 digits")),
            None => {
                return Err(forbidden(
                    "biometric_required",
                    "revoking a device needs this device's key (Face ID / Touch ID) or the PIN",
                ))
            }
        }
    }
    let (removed, pending) = st
        .approvals
        .keys
        .update(|k| {
            (
                k.keys.remove(&target).is_some(),
                k.pending.remove(&target).is_some(),
            )
        })
        .map_err(|e| {
            tracing::error!(error = %e, "approval keys not saved");
            ApiError::internal("could not remove the key")
        })?;
    if removed || pending {
        let name = st
            .devices
            .active_device(&target)
            .ok()
            .flatten()
            .map_or_else(|| target.clone(), |d| d.name);
        st.approvals.audit.log(
            "key_revoked",
            json!({"device": target, "deviceName": name, "by": who.device_id, "byName": who.device_name,
                   "verifiedBy": if key_ok { "device_key" } else { "pin" }}),
        );
        if removed {
            tell_owner(
                &st,
                &format!(
                    "{name} can no longer approve requests (revoked on {})",
                    who.device_name
                ),
            )
            .await;
        }
    }
    Ok(Json(json!({"removed": removed, "pendingRemoved": pending})))
}

/// `DELETE /v1/approvals/keys`
async fn delete_key(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
) -> Result<Json<Value>, ApiError> {
    let removed = st
        .approvals
        .keys
        .update(|k| k.keys.remove(&who.device_id).is_some())
        .map_err(|e| {
            tracing::error!(error = %e, "approval keys not saved");
            ApiError::internal("could not remove the key")
        })?;
    if removed {
        st.approvals
            .audit
            .log("key_removed", json!({"device": who.device_id}));
    }
    Ok(Json(json!({"removed": removed})))
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PinBody {
    #[serde(default)]
    pin: Option<String>,
    #[serde(default)]
    current_pin: Option<String>,
    #[serde(default)]
    proof: Option<SigIn>,
}

/// Replacing or removing an existing PIN needs the current one, or a proof
/// from this device's registered key. No PIN yet: nothing to prove.
async fn authorize_pin_change(
    st: &AppState,
    who: &Identity,
    current_pin: Option<String>,
    proof: Option<&SigIn>,
) -> Result<(), ApiError> {
    if !st.approvals.has_pin() {
        return Ok(());
    }
    if proof.is_some_and(|p| st.approvals.pin_proof_ok(&who.device_id, &p.signature)) {
        return Ok(());
    }
    match current_pin {
        Some(cur) if pin_format_ok(&cur) => verify_pin(st, cur, who.device_id.clone()).await,
        Some(_) => Err(bad("currentPin: exactly 6 digits")),
        None => Err(forbidden(
            "pin_auth_required",
            "send currentPin, or a proof from this device's approval key",
        )),
    }
}

fn pin_state_json(st: &AppState) -> Value {
    json!({"hasPin": st.approvals.has_pin()})
}

/// `PUT /v1/approvals/pin {pin, currentPin?, proof?: {signature}}`
async fn put_pin(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
    JsonBody(body): JsonBody<PinBody>,
) -> Result<Json<Value>, ApiError> {
    let pin = body.pin.unwrap_or_default();
    if !pin_format_ok(&pin) {
        return Err(bad("pin: exactly 6 digits"));
    }
    let replaced = st.approvals.has_pin();
    authorize_pin_change(&st, &who, body.current_pin, body.proof.as_ref()).await?;
    let hash = tokio::task::spawn_blocking(move || crate::owner::hash_password(&pin))
        .await
        .map_err(|_| ApiError::internal("PIN hashing failed"))?
        .map_err(|e| {
            tracing::error!(error = %e, "PIN hashing failed");
            ApiError::internal("could not hash the PIN")
        })?;
    st.approvals
        .pin
        .update(|p| {
            *p = PinDoc {
                hash: Some(hash),
                failures: 0,
                locked_until_ms: 0,
            };
        })
        .map_err(|e| {
            tracing::error!(error = %e, "approval PIN not saved");
            ApiError::internal("could not save the PIN")
        })?;
    st.approvals.audit.log(
        "pin_set",
        json!({"device": who.device_id, "deviceName": who.device_name, "replaced": replaced}),
    );
    tell_owner(&st, &format!("Approval PIN changed on {}", who.device_name)).await;
    Ok(Json(pin_state_json(&st)))
}

/// `DELETE /v1/approvals/pin` (optional body `{currentPin?, proof?}`)
async fn delete_pin(
    State(st): State<AppState>,
    Extension(who): Extension<Identity>,
    raw: axum::body::Bytes,
) -> Result<Json<Value>, ApiError> {
    let body: PinBody = if raw.iter().all(u8::is_ascii_whitespace) {
        PinBody::default()
    } else {
        serde_json::from_slice(&raw).map_err(|_| bad("body: {currentPin?, proof?}"))?
    };
    if !st.approvals.has_pin() {
        return Ok(Json(pin_state_json(&st)));
    }
    authorize_pin_change(&st, &who, body.current_pin, body.proof.as_ref()).await?;
    st.approvals
        .pin
        .update(|p| *p = PinDoc::default())
        .map_err(|e| {
            tracing::error!(error = %e, "approval PIN not saved");
            ApiError::internal("could not remove the PIN")
        })?;
    st.approvals.audit.log(
        "pin_removed",
        json!({"device": who.device_id, "deviceName": who.device_name}),
    );
    tell_owner(&st, &format!("Approval PIN changed on {}", who.device_name)).await;
    Ok(Json(pin_state_json(&st)))
}

/// Approver routes: a device, never the service token.
async fn approver_only(
    State(st): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let is_service = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .is_some_and(|(scheme, t)| {
            scheme.eq_ignore_ascii_case("bearer")
                && t.trim().starts_with(crate::service::SERVICE_PREFIX)
        });
    if is_service {
        return Err(forbidden(
            "service_cannot_decide",
            "the service token can ask for approvals but never answers them",
        ));
    }
    crate::auth::require_device(State(st), req, next).await
}

/// All the approval routes, with their auth layers.
pub fn routes(state: AppState) -> Router<AppState> {
    let requester = Router::new()
        .route("/v1/approvals", post(create))
        .route("/v1/approvals/{id}/wait", get(wait))
        .route("/v1/approvals/{id}/withdraw", post(withdraw))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::require_device_or_service,
        ));
    let approver = Router::new()
        .route("/v1/approvals", get(list))
        .route(
            "/v1/approvals/settings",
            get(get_settings).put(put_settings),
        )
        .route("/v1/approvals/keys", post(register_key).delete(delete_key))
        .route(
            "/v1/approvals/keys/{device_id}",
            axum::routing::delete(revoke_key),
        )
        .route("/v1/approvals/pin", put(put_pin).delete(delete_pin))
        .route("/v1/approvals/{id}", get(get_one))
        .route("/v1/approvals/{id}/decide", post(decide))
        .route_layer(middleware::from_fn_with_state(state, approver_only));
    Router::new()
        .route("/v1/approvals/public-key", get(public_key))
        .merge(requester)
        .merge(approver)
}

/// Expire and purge requests every few seconds (also done lazily on access).
pub fn spawn_sweeper(st: &AppState) {
    let st = st.clone();
    tokio::spawn(async move {
        let mut shutdown = st.shutdown.subscribe();
        let mut every = tokio::time::interval(Duration::from_secs(5));
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = every.tick() => st.approvals.sweep(),
                () = crate::notify::stopped(&mut shutdown) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Via;

    fn who(id: &str) -> Identity {
        Identity {
            device_id: id.into(),
            device_name: id.to_uppercase(),
            via: Via::Bearer,
        }
    }

    fn body(action: &str) -> CreateBody {
        CreateBody {
            requester: RequesterIn {
                kind: "lucina".into(),
                name: "Lucina".into(),
                detail: None,
            },
            action: action.into(),
            summary: "Do the thing".into(),
            details: String::new(),
            risk: None,
            sensitive: None,
            payload_hash: format!("sha256:{}", "ab".repeat(32)),
            ttl_secs: None,
        }
    }

    fn store(dir: &FsPath) -> Approvals {
        Approvals::open(dir, ApprovalsConfig::default()).unwrap()
    }

    #[test]
    fn sensitive_by_action_prefix() {
        for a in [
            "secret.reveal",
            "root.exec",
            "sudo.run",
            "message.send",
            "message.send.telegram",
            "email.send",
            "payment.charge",
            "account.delete",
            "broker.put_secret",
        ] {
            assert!(sensitive_action(a), "{a}");
        }
        for a in [
            "agent.permission",
            "calendar.add",
            "message.read",
            "secrets",
        ] {
            assert!(!sensitive_action(a), "{a}");
        }
    }

    #[test]
    fn ttl_defaults_and_clamps_and_sensitive_is_forced() {
        let dir = tempfile::tempdir().unwrap();
        let ap = store(dir.path());
        let t0 = 1_000_000;
        let c = ap.create_at(&who("svc"), body("root.exec"), t0).unwrap();
        assert!(c.sensitive);
        assert_eq!(c.expires_ms, t0 + 300_000);
        let mut b = body("calendar.add");
        b.ttl_secs = Some(5);
        assert_eq!(
            ap.create_at(&who("svc"), b, t0).unwrap().expires_ms,
            t0 + 30_000
        );
        let mut b = body("calendar.add");
        b.ttl_secs = Some(100_000);
        assert_eq!(
            ap.create_at(&who("svc"), b, t0).unwrap().expires_ms,
            t0 + 900_000
        );
        // The requester can raise sensitivity but not lower it.
        let mut b = body("root.exec");
        b.sensitive = Some(false);
        assert!(ap.create_at(&who("svc"), b, t0).unwrap().sensitive);
        let mut b = body("calendar.add");
        b.sensitive = Some(true);
        assert!(ap.create_at(&who("svc"), b, t0).unwrap().sensitive);
    }

    #[test]
    fn bodies_are_validated() {
        let dir = tempfile::tempdir().unwrap();
        let ap = store(dir.path());
        let t0 = 1_000_000;
        let bad_body = |f: fn(&mut CreateBody)| {
            let mut b = body("calendar.add");
            f(&mut b);
            let e = ap.create_at(&who("svc"), b, t0).unwrap_err();
            assert_eq!(e.status, StatusCode::BAD_REQUEST, "{}", e.message);
        };
        bad_body(|b| b.action = "Root.Exec".into());
        bad_body(|b| b.action = String::new());
        bad_body(|b| b.summary = "  ".into());
        bad_body(|b| b.summary = "x".repeat(201));
        bad_body(|b| b.details = "x".repeat(4001));
        bad_body(|b| b.risk = Some("extreme".into()));
        bad_body(|b| b.payload_hash = "sha256:ABC".into());
        bad_body(|b| b.payload_hash = "md5:00".into());
        bad_body(|b| b.requester.kind = "root".into());
        bad_body(|b| b.requester.kind = "device".into());
        bad_body(|b| b.requester.name = String::new());
    }

    #[test]
    fn devices_cannot_pass_for_lucina() {
        let dir = tempfile::tempdir().unwrap();
        let ap = store(dir.path());
        let c = ap
            .create_at(&who("dev_a"), body("calendar.add"), 1)
            .unwrap();
        let item = ap.get_at(&c.id, "dev_b", None, 1).unwrap();
        assert_eq!(item["requester"]["kind"], "device");
        assert_eq!(item["requester"]["name"], "DEV_A");
    }

    #[test]
    fn grant_has_the_contract_format() {
        let dir = tempfile::tempdir().unwrap();
        let signer = ApprovalSigner::open(dir.path()).unwrap();
        // Same key on the next start.
        assert_eq!(
            ApprovalSigner::open(dir.path()).unwrap().public_key(),
            signer.public_key()
        );
        let g = signer
            .sign(&GrantPayload {
                v: 1,
                id: "apr_x",
                action: "root.exec",
                requester: GrantRequester {
                    kind: "lucina",
                    name: "Lucina",
                },
                payload_hash: "sha256:00",
                decision: "approved",
                decided_by: "dev_1",
                biometric: true,
                verified_by: Some("device_key"),
                iat: 100,
                exp: 220,
            })
            .unwrap();
        let (p64, s64) = g.split_once('.').unwrap();
        let payload: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p64).unwrap()).unwrap();
        assert_eq!(
            payload["requester"],
            json!({"kind": "lucina", "name": "Lucina"})
        );
        assert_eq!(payload["payloadHash"], "sha256:00");
        assert_eq!(payload["exp"], 220);
        assert_eq!(payload["verifiedBy"], "device_key");
        ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, signer.public_key())
            .verify(
                format!("secretary-approval-v1.{p64}").as_bytes(),
                &URL_SAFE_NO_PAD.decode(s64).unwrap(),
            )
            .unwrap();
        // The "Add secret" domain does not verify it.
        assert!(ring::signature::UnparsedPublicKey::new(
            &ring::signature::ED25519,
            signer.public_key()
        )
        .verify(
            format!("infisical-broker-grant-v1\n{p64}").as_bytes(),
            &URL_SAFE_NO_PAD.decode(s64).unwrap(),
        )
        .is_err());
    }

    #[test]
    fn visibility_follows_the_main_device_and_escalation() {
        let dir = tempfile::tempdir().unwrap();
        let ap = store(dir.path());
        let t0 = 1_000_000;
        let c = ap.create_at(&who("svc"), body("calendar.add"), t0).unwrap();
        let ids = |me: &str, main: Option<&str>, now: i64| ap.list_at(me, main, false, now).len();
        // No main device: everybody at once.
        assert_eq!(ids("dev_b", None, t0), 1);
        // Main device set: only it for 30 s, then everybody.
        assert_eq!(ids("dev_a", Some("dev_a"), t0 + 29_999), 1);
        assert_eq!(ids("dev_b", Some("dev_a"), t0 + 29_999), 0);
        assert_eq!(ids("dev_b", Some("dev_a"), t0 + 30_000), 1);
        // Never to the requester itself, and not once expired.
        assert_eq!(ids("svc", None, t0), 0);
        assert_eq!(ids("dev_a", Some("dev_a"), t0 + 300_000), 0);
        let item = ap.get_at(&c.id, "dev_a", Some("dev_a"), t0).unwrap();
        assert_eq!(
            (item["youAreMain"].clone(), item["escalated"].clone()),
            (json!(true), json!(false))
        );
        assert_eq!(
            ap.get_at(&c.id, "dev_a", Some("dev_a"), t0 + 300_000)
                .unwrap()["status"],
            "expired"
        );
    }

    #[test]
    fn expired_requests_are_purged_after_the_retention() {
        let dir = tempfile::tempdir().unwrap();
        let ap = store(dir.path());
        let c = ap.create_at(&who("svc"), body("calendar.add"), 0).unwrap();
        assert!(ap
            .get_at(&c.id, "dev_a", None, 300_000 + RETAIN_MS - 1)
            .is_ok());
        assert!(ap
            .get_at(&c.id, "dev_a", None, 300_000 + RETAIN_MS)
            .is_err());
        let audit = std::fs::read_to_string(dir.path().join(AUDIT_FILE)).unwrap();
        assert!(audit.lines().any(|l| l.contains("\"expired\"")), "{audit}");
    }

    #[test]
    fn creation_is_rate_limited_per_requester() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = ApprovalsConfig {
            creates_per_minute: 2,
            ..ApprovalsConfig::default()
        };
        let ap = Approvals::open(dir.path(), cfg).unwrap();
        assert!(ap.create_at(&who("svc"), body("a.b"), 1).is_ok());
        assert!(ap.create_at(&who("svc"), body("a.b"), 1).is_ok());
        let e = ap.create_at(&who("svc"), body("a.b"), 1).unwrap_err();
        assert_eq!(e.status, StatusCode::TOO_MANY_REQUESTS);
        // Another requester has its own bucket.
        assert!(ap.create_at(&who("dev_a"), body("a.b"), 1).is_ok());
    }

    fn with_pin(ap: &Approvals, pin: &str) {
        let hash = crate::owner::hash_password(pin).unwrap();
        ap.pin
            .update(|p| {
                *p = PinDoc {
                    hash: Some(hash),
                    ..PinDoc::default()
                }
            })
            .unwrap();
    }

    #[test]
    fn the_pin_locks_after_five_failures_persists_and_expires() {
        let dir = tempfile::tempdir().unwrap();
        let ap = store(dir.path());
        assert!(matches!(
            ap.check_pin("123456", "d", 0),
            Err(PinError::NotSet)
        ));
        with_pin(&ap, "123456");
        let t0 = 1_000_000;
        for left in [4, 3, 2] {
            assert!(matches!(
                ap.check_pin("000000", "d", t0),
                Err(PinError::Wrong { left: l }) if l == left
            ));
        }
        // A right one resets the count.
        ap.check_pin("123456", "d", t0).unwrap();
        for left in [4, 3, 2, 1] {
            assert!(matches!(
                ap.check_pin("000000", "d", t0),
                Err(PinError::Wrong { left: l }) if l == left
            ));
        }
        assert!(matches!(
            ap.check_pin("000000", "d", t0),
            Err(PinError::Locked { retry_secs: 900 })
        ));
        // Locked: even the right PIN is refused, and the lock survives a restart.
        let reopened = store(dir.path());
        assert!(matches!(
            reopened.check_pin("123456", "d", t0 + 1_000),
            Err(PinError::Locked { retry_secs: 899 })
        ));
        assert!(matches!(
            reopened.check_pin("123456", "d", t0 + PIN_LOCK_MS - 1),
            Err(PinError::Locked { retry_secs: 1 })
        ));
        // When it runs out the count starts again from five.
        assert!(matches!(
            reopened.check_pin("000000", "d", t0 + PIN_LOCK_MS),
            Err(PinError::Wrong { left: 4 })
        ));
        reopened.check_pin("123456", "d", t0 + PIN_LOCK_MS).unwrap();
        assert!(matches!(
            store(dir.path()).check_pin("000000", "d", t0 + PIN_LOCK_MS),
            Err(PinError::Wrong { left: 4 })
        ));
    }

    #[tokio::test]
    async fn approval_pushes_go_to_the_main_device_first() {
        use crate::notify::push::{Kind, Subscription};
        let dir = tempfile::tempdir().unwrap();
        let notifier = std::sync::Arc::new(
            crate::notify::Notifier::open(dir.path(), crate::notify::NotifyConfig::default())
                .unwrap(),
        );
        for (i, dev) in ["dev_main", "dev_other"].into_iter().enumerate() {
            notifier
                .pusher
                .upsert(Subscription {
                    id: format!("ps_{i}"),
                    kind: Kind::Apns,
                    device_id: dev.into(),
                    device_name: dev.into(),
                    label: String::new(),
                    endpoint: format!("{i:02x}").repeat(32),
                    p256dh: String::new(),
                    auth: String::new(),
                    lang: None,
                    created_at: 0,
                    last_success_at: None,
                    failures: 0,
                    last_error: None,
                })
                .unwrap();
        }
        assert_eq!(notifier.pusher.list().len(), 2);
        let mut n = NewNotification::new(Category::Approval, "Restart the tunnel");
        n.route = Some("/approvals/apr_1".into());
        n.data = Some(json!({"approvalId": "apr_1", "sensitive": true}));
        let Outcome::Created { notification, .. } = notifier
            .publish_to(n, &Audience::Only("dev_main".into()))
            .await
            .unwrap()
        else {
            panic!("not created")
        };
        let now = now_ms();
        let first =
            notifier.targets_for_audience(&notification, now, &Audience::Only("dev_main".into()));
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].0.device_id, "dev_main");
        let later =
            notifier.targets_for_audience(&notification, now, &Audience::Except("dev_main".into()));
        assert_eq!(later.len(), 1);
        assert_eq!(later[0].0.device_id, "dev_other");
        let p = &first[0].1;
        assert_eq!(p.category, "approval");
        assert_eq!(p.url.as_deref(), Some("/approvals/apr_1"));
        assert_eq!(p.route, "/approvals/apr_1");
        let apns = crate::notify::push::apns_body(p);
        assert_eq!(apns["aps"]["category"], "APPROVAL_SENSITIVE");
        assert_eq!(apns["approvalId"], "apr_1");
        assert_eq!(apns["sensitive"], true);
        assert_eq!(apns["approvalSensitive"], true);
        assert_eq!(apns["aps"]["content-available"], 1);
        // Quiet hours and DND do not hold it, and no switch mutes it.
        notifier
            .settings
            .update(|s| {
                s.dnd_until = Some(now_ms() + 60_000);
                s.devices.insert(
                    "dev_main".into(),
                    crate::notify::settings::DevicePrefs {
                        enabled: false,
                        categories: Default::default(),
                    },
                );
            })
            .unwrap();
        let o = notifier
            .publish(NewNotification::new(Category::Approval, "again"))
            .await
            .unwrap();
        assert!(
            matches!(
                o,
                Outcome::Created {
                    deferred: false,
                    ..
                }
            ),
            "{o:?}"
        );
    }
}
