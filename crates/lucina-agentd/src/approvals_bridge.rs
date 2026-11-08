//! Permission requests in the owner's live approvals flow (agents phase 8).
//!
//! When a permission request becomes pending (a provider's tool request, or an external
//! one) the bridge files it as an owner approval on secretary-server
//! (`POST /v1/approvals`, action `agent.permission`). The owner's devices then show the
//! usual Yes/No prompt (Face ID or the PIN for sensitive ones). The bridge long-polls the
//! approval and answers the request **only from a verified grant**:
//!
//! - It trusts nothing but the `/wait` answer of an approval it created itself, over its
//!   own service-token link. Only an `approved` answer with an Ed25519 grant counts: the
//!   signature must verify with the pinned key, and the grant must be for this approval
//!   id, the action `agent.permission`, and the payload hash of *this* request as it is
//!   in the book right now (agent id, request id, kind, tool, input), before it expires,
//!   requested by `agentd`, decided by an owner **device** (`dev_…`), and used once.
//!   Who decided is read from inside the verified grant, never from the reply around it.
//! - The answer goes through the ordinary owner answer of the manager
//!   ([`Manager::answer_from_approval`]), so the kernel (`permission_gate::answer`) still
//!   decides, with `Origin::OwnerDevice`; the book records `by: owner_device` and the
//!   device. A request that was answered or has expired meanwhile stays as it is.
//! - A denial has no grant. It is taken from the same authenticated `/wait` of an approval
//!   agentd created (only devices can decide on the server) and is always safe: it denies.
//! - An expired approval leaves the request pending (it can still be answered through the
//!   web `/agents` page or `lagent permit`).
//! - A request answered elsewhere, or that expired or was withdrawn by its provider, makes
//!   the bridge call `POST /v1/approvals/{id}/withdraw` so no prompt stays on the phone.
//!
//! Settings (environment; the bridge is off, logged once, when the URL, the token or the
//! key is missing):
//!
//! | variable | meaning |
//! |---|---|
//! | `LUCINA_AGENTD_NOTIFY_URL` | secretary-server base URL (the same as for pushes) |
//! | `LUCINA_AGENTD_NOTIFY_TOKEN` | the `svc_` service token (the same as for pushes) |
//! | `LUCINA_AGENTD_APPROVAL_PUBKEY_FILE` | pinned grant key, default `~/.config/lucina-rs/approval-grant.pub` |
//! | `LUCINA_AGENTD_APPROVAL_USED_FILE` | single-use registry, default `~/.local/state/lucina-agentd/approvals-used.jsonl` |
//! | `LUCINA_AGENTD_APPROVALS=off` | switch the bridge off |

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lucina_agents::external::valid_device_id;
use lucina_agents::permission_gate::{RequestKind, Status, Verdict};
use lucina_approvals::client::{clip, Client, NewApproval, RequesterInfo, Status as ApprovalStatus, WaitReply, MAX_DETAILS, MAX_SUMMARY};
use lucina_approvals::grant::{self, Expect, GrantError, PubKey};
use lucina_approvals::used::UsedRegistry;
use lucina_approvals::{payload_hash, ApprovalError};
use regex::Regex;
use serde_json::{json, Map, Value};
use tokio::sync::{broadcast, watch};
use tokio::task::{JoinHandle, JoinSet};

use crate::journal::Sequenced;
use crate::manager::{ApprovalCandidate, BusEvent, Manager, ManagerError, PermissionView};
use crate::notify::{TOKEN_ENV, URL_ENV};

/// The approval action of every permission request.
pub const ACTION: &str = "agent.permission";
pub const PUBKEY_ENV: &str = "LUCINA_AGENTD_APPROVAL_PUBKEY_FILE";
pub const USED_ENV: &str = "LUCINA_AGENTD_APPROVAL_USED_FILE";
pub const OFF_ENV: &str = "LUCINA_AGENTD_APPROVALS";
/// What the approval's requester is called, and what a grant must say it came from.
pub const REQUESTER_KIND: &str = "agentd";
pub const REQUESTER_NAME: &str = "Lucina Agents";

// -- settings -----------------------------------------------------------------------

#[derive(Clone)]
pub struct BridgeConfig {
    pub url: String,
    token: String,
    pub pubkey_file: PathBuf,
    pub used_file: PathBuf,
    /// First retry delay after a server error; it doubles up to `retry_max`.
    pub retry_base: Duration,
    pub retry_max: Duration,
    /// Seconds of one long poll (the server allows up to 25).
    pub wait_secs: u64,
}

impl std::fmt::Debug for BridgeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeConfig").field("url", &self.url).field("pubkey_file", &self.pubkey_file).finish_non_exhaustive()
    }
}

impl BridgeConfig {
    pub fn new(url: &str, token: &str, pubkey_file: PathBuf, used_file: PathBuf) -> Self {
        BridgeConfig {
            url: url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            pubkey_file,
            used_file,
            retry_base: Duration::from_secs(1),
            retry_max: Duration::from_secs(30),
            wait_secs: 25,
        }
    }

    /// `Err(why)` when the bridge stays off.
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let text = |k: &str| get(k).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        if matches!(text(OFF_ENV).map(|v| v.to_ascii_lowercase()).as_deref(), Some("off" | "0" | "false" | "no")) {
            return Err(format!("{OFF_ENV} is off"));
        }
        let (Some(url), Some(token)) = (text(URL_ENV), text(TOKEN_ENV)) else {
            return Err(format!("{URL_ENV} and {TOKEN_ENV} are not both set"));
        };
        let home = text("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
        let pubkey = text(PUBKEY_ENV).map(PathBuf::from).unwrap_or_else(|| home.join(".config/lucina-rs/approval-grant.pub"));
        let used = text(USED_ENV).map(PathBuf::from).unwrap_or_else(|| home.join(".local/state/lucina-agentd/approvals-used.jsonl"));
        PubKey::load(&pubkey).map_err(|e| format!("no pinned approval key ({e})"))?;
        Ok(Self::new(&url, &token, pubkey, used))
    }
}

/// Start the bridge from the environment. Off (logged once) without URL, token or key.
pub fn spawn_from_env(manager: &Manager) -> Option<JoinHandle<()>> {
    let cfg = match BridgeConfig::from_lookup(&|k| std::env::var(k).ok()) {
        Ok(c) => c,
        Err(why) => {
            tracing::info!("permission requests stay out of the owner approvals: {why}");
            return None;
        }
    };
    match Bridge::new(manager.clone(), cfg) {
        Ok(b) => {
            tracing::info!("permission requests are filed as owner approvals");
            Some(b.spawn())
        }
        Err(e) => {
            tracing::warn!(error = %e, "owner approvals for permission requests are off");
            None
        }
    }
}

// -- what is sent to the owner -------------------------------------------------------

fn kind_str(k: RequestKind) -> &'static str {
    match k {
        RequestKind::Agent => "agent",
        RequestKind::External => "external",
    }
}

/// The tool (agent requests) or the asking service (external requests).
fn what_name(v: &PermissionView) -> String {
    match v.kind {
        RequestKind::Agent => v.name.clone().unwrap_or_default(),
        RequestKind::External => v.source.clone().unwrap_or_default(),
    }
}

/// The exact thing the grant is bound to: `{agentId|null, requestId, kind, name, input}`.
pub fn payload_value(v: &PermissionView) -> Value {
    json!({
        "agentId": v.agent_id,
        "requestId": v.id,
        "kind": kind_str(v.kind),
        "name": what_name(v),
        "input": v.details,
    })
}

/// `sha256:<hex>` of the canonical JSON of [`payload_value`].
pub fn payload_hash_of(v: &PermissionView) -> String {
    payload_hash(&payload_value(v))
}

fn re(cell: &'static std::sync::OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("fixed pattern"))
}

/// Names of tools and inputs the owner must confirm with Face ID or the PIN: sudo and
/// root, secrets and credentials. `root` as a file name (`src/root.rs`) does not count.
fn sensitive_words() -> &'static Regex {
    static R: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    re(
        &R,
        r"(?i)(?:^|[^a-z0-9])(?:sudo|doas|nezopt-sudo|infisical|id_rsa|id_ed25519)(?:[^a-z0-9]|$)|(?:^|[^a-z0-9])root(?:[^a-z0-9.]|\.(?:[^a-z0-9]|$)|$)|(?:^|[^a-z0-9])secrets?(?:[^a-z]|$)|passw(?:or)?d|api[_-]?key|private[_-]?key|\.env(?:[^a-z0-9]|$)|(?:^|[^a-z0-9])su\s+-",
    )
}

/// Whether the owner must prove it is him: every external request, and a tool whose
/// name or input mentions sudo, root or secrets.
pub fn is_sensitive(v: &PermissionView) -> bool {
    if v.kind == RequestKind::External {
        return true;
    }
    let mut text = what_name(v);
    if let Some(input) = &v.details {
        text.push('\n');
        text.push_str(&serde_json::to_string(input).unwrap_or_default());
    }
    sensitive_words().is_match(&text)
}

/// Mask what looks like a secret value. The hash still covers the real input; the owner
/// sees the names, not the values (approvals contract: "no secret values in details").
pub fn redact(text: &str) -> String {
    static PEM: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static PREFIXED: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static BEARER: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static ASSIGNED: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let t = re(&PEM, r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)").replace_all(text, "[redacted private key]");
    let t = re(
        &PREFIXED,
        r"\b(?:sk-[A-Za-z0-9_-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|svc_[A-Za-z0-9_-]{16,}|sec_[A-Za-z0-9_-]{16,}|xox[abpr]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_-]{30,})",
    )
    .replace_all(&t, "[redacted]");
    let t = re(&BEARER, r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{8,}").replace_all(&t, "Bearer [redacted]");
    re(
        &ASSIGNED,
        r#"(?i)\b([A-Za-z0-9_.-]*(?:token|secret|passw(?:or)?d|api[_-]?key|authorization|credential)[A-Za-z0-9_.-]*)(["']?\s*[:=]\s*["']?)[^\s"',;&)\]}]{4,}"#,
    )
    .replace_all(&t, "$1$2[redacted]")
    .into_owned()
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

fn str_of<'a>(input: &'a Option<Map<String, Value>>, key: &str) -> Option<&'a str> {
    input.as_ref()?.get(key)?.as_str().filter(|s| !s.trim().is_empty())
}

/// `mcp__server__tool` as `server: tool`; any other name as it is.
fn tool_label(name: &str) -> String {
    name.strip_prefix("mcp__").map_or_else(|| name.to_string(), |rest| rest.replacen("__", ": ", 1))
}

/// One line for the prompt's title ("Claude wants to run: …").
pub fn summary(c: &ApprovalCandidate) -> String {
    let v = &c.view;
    let line = match v.kind {
        RequestKind::External => format!("{} asks: {}", v.source.as_deref().unwrap_or("a service"), v.title),
        RequestKind::Agent => {
            let who = capitalised(c.provider.as_deref().unwrap_or("An agent"));
            let name = v.name.as_deref().unwrap_or("a tool");
            let what = if let Some(cmd) = str_of(&v.details, "command") {
                cmd.to_string()
            } else if let Some(target) = ["file_path", "path", "url", "pattern", "query"].iter().find_map(|k| str_of(&v.details, k)) {
                format!("{} {target}", tool_label(name))
            } else {
                tool_label(name)
            };
            format!("{who} wants to run: {what}")
        }
    };
    clip(&redact(&one_line(&line)), MAX_SUMMARY)
}

/// The tool name and its input, as exactly as fits; `true` when it had to be cut.
pub fn details(c: &ApprovalCandidate) -> (String, bool) {
    let v = &c.view;
    let mut out = String::new();
    match v.kind {
        RequestKind::Agent => {
            out.push_str(&format!("Tool: {}\n", v.name.as_deref().unwrap_or("?")));
            if let Some(t) = &c.agent_title {
                out.push_str(&format!("Agent: {t}\n"));
            }
        }
        RequestKind::External => {
            out.push_str(&format!("From: {}\nRequest: {}\n", v.source.as_deref().unwrap_or("?"), v.title));
            if let Some(d) = &v.description {
                out.push_str(&format!("{d}\n"));
            }
        }
    }
    if let Some(input) = &v.details {
        out.push_str("Input:\n");
        for (k, val) in input {
            match val {
                Value::String(s) if s.contains('\n') => out.push_str(&format!("{k}:\n{s}\n")),
                Value::String(s) => out.push_str(&format!("{k}: {s}\n")),
                other => out.push_str(&format!("{k}: {other}\n")),
            }
        }
    }
    let text = redact(out.trim_end());
    let total = text.chars().count();
    if total <= MAX_DETAILS {
        return (text, false);
    }
    let note = format!("\n... (cut: first {} of {total} characters)", MAX_DETAILS - 60);
    let mut cut: String = text.chars().take(MAX_DETAILS - 60).collect();
    cut.push_str(&note);
    (cut, true)
}

/// Seconds the approval should live: what the request has left, within 30..=900.
pub fn ttl_secs(v: &PermissionView, now_unix: i64) -> u64 {
    let end = chrono::DateTime::parse_from_rfc3339(&v.expires_at).map(|d| d.timestamp()).unwrap_or(now_unix + 900);
    (end - now_unix).clamp(30, 900) as u64
}

/// The `POST /v1/approvals` body for a request.
pub fn build_request(c: &ApprovalCandidate, now_unix: i64) -> NewApproval {
    let v = &c.view;
    let (details, cut) = details(c);
    let sensitive = is_sensitive(v);
    let detail = match v.kind {
        RequestKind::Agent => format!("{} · {}", c.agent_title.as_deref().unwrap_or("agent"), c.provider.as_deref().unwrap_or("?")),
        RequestKind::External => v.source.clone().unwrap_or_default(),
    };
    NewApproval {
        requester: RequesterInfo { kind: REQUESTER_KIND.into(), name: REQUESTER_NAME.into(), detail: clip(&one_line(&detail), 120) },
        action: ACTION.into(),
        summary: summary(c),
        details,
        // A cut text means the owner cannot read all of what he approves.
        risk: if sensitive || cut { "high" } else { "medium" }.into(),
        sensitive: sensitive || cut,
        payload_hash: payload_hash_of(v),
        ttl_secs: ttl_secs(v, now_unix),
    }
}

// -- the bridge ----------------------------------------------------------------------

/// A request in the book: the agent (agent requests) and the request id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PermKey {
    pub agent_id: Option<String>,
    pub id: String,
}

impl PermKey {
    pub fn of(v: &PermissionView) -> Self {
        PermKey { agent_id: v.agent_id.clone(), id: v.id.clone() }
    }
}

/// The approval filed for a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filed {
    pub approval_id: String,
    /// It was filed as sensitive: its grant must carry the owner's biometric/PIN proof.
    pub sensitive: bool,
}

/// What a `/wait` answer did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// A verified grant answered the request: allowed, by this device.
    Allowed { device_id: String },
    Denied,
    /// Still waiting for the owner.
    Waiting,
    /// The approval expired: the request stays pending.
    Expired,
    /// Nothing to do: the request was answered, has expired or is gone (not an error).
    NoOp(&'static str),
    /// The answer was not trusted and changed nothing.
    Refused(String),
}

pub struct Bridge {
    manager: Manager,
    client: Client,
    cfg: BridgeConfig,
    used: Arc<UsedRegistry>,
    /// Requests being handled.
    active: Mutex<HashSet<PermKey>>,
    /// Requests the bridge gave up on (a refusal that retrying cannot fix).
    gave_up: Mutex<HashSet<PermKey>>,
}

/// Removes a request from `active` when its task ends, whatever way.
struct ActiveGuard {
    bridge: Arc<Bridge>,
    key: PermKey,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.bridge.active.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.key);
    }
}

enum Pause {
    Elapsed,
    Resolved,
    Shutdown,
}

fn retryable(e: &ApprovalError) -> bool {
    match e {
        ApprovalError::Http(_) | ApprovalError::NotFound => true,
        ApprovalError::Server { status, .. } => *status == 429 || *status >= 500,
        _ => false,
    }
}

fn is_resolved_event(ev: &Sequenced, key: &PermKey) -> bool {
    matches!(&ev.event, BusEvent::Permission { permission } if permission.id == key.id && permission.agent_id == key.agent_id && permission.status != Status::Pending)
}

/// An owner device's id on secretary-server (`dev_…`); never `svc`, `mcp…` or `service:…`.
fn is_owner_device(id: &str) -> bool {
    id.starts_with("dev_") && valid_device_id(id)
}

impl Bridge {
    pub fn new(manager: Manager, cfg: BridgeConfig) -> Result<Arc<Self>, ApprovalError> {
        PubKey::load(&cfg.pubkey_file).map_err(ApprovalError::NotConfigured)?;
        let client = Client::with_token(&cfg.url, cfg.token.clone())?;
        Ok(Arc::new(Bridge {
            manager,
            client,
            used: Arc::new(UsedRegistry::new(cfg.used_file.clone())),
            cfg,
            active: Mutex::new(HashSet::new()),
            gave_up: Mutex::new(HashSet::new()),
        }))
    }

    /// Run until the daemon shuts down (`Manager::close_streams`). Never blocks the
    /// runtime: everything is async, and the single-use file is touched on a blocking thread.
    pub fn spawn(self: &Arc<Self>) -> JoinHandle<()> {
        let me = self.clone();
        tokio::spawn(async move { me.supervise().await })
    }

    fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(|p| p.into_inner())
    }

    async fn supervise(self: Arc<Self>) {
        let mut rx = self.manager.journal().subscribe(None).live;
        let mut closing = self.manager.closing();
        let mut tasks: JoinSet<()> = JoinSet::new();
        let mut tick = tokio::time::interval(Duration::from_secs(5));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            if *closing.borrow() {
                break;
            }
            self.start_new(&mut tasks);
            loop {
                tokio::select! {
                    _ = closing.changed() => break,
                    _ = tick.tick() => break,
                    Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
                    ev = rx.recv() => match ev {
                        Ok(e) if matches!(e.event, BusEvent::Permission { .. }) => break,
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(_)) => break,
                        Err(broadcast::error::RecvError::Closed) => {
                            while tasks.join_next().await.is_some() {}
                            return;
                        }
                    },
                }
            }
        }
        // Every task watches the same flag: they withdraw what is open and end.
        while tasks.join_next().await.is_some() {}
    }

    fn start_new(self: &Arc<Self>, tasks: &mut JoinSet<()>) {
        for c in self.manager.approval_candidates() {
            let key = PermKey::of(&c.view);
            if Self::lock(&self.gave_up).contains(&key) || !Self::lock(&self.active).insert(key.clone()) {
                continue;
            }
            let guard = ActiveGuard { bridge: self.clone(), key: key.clone() };
            let me = self.clone();
            tasks.spawn(async move {
                let _guard = guard;
                me.run_one(key).await;
            });
        }
    }

    fn pending(&self, key: &PermKey) -> bool {
        self.manager.permission_view(key.agent_id.as_deref(), &key.id).is_some_and(|v| v.status == Status::Pending)
    }

    async fn pause(&self, rx: &mut broadcast::Receiver<Arc<Sequenced>>, closing: &mut watch::Receiver<bool>, key: &PermKey, d: Duration) -> Pause {
        let sleep = tokio::time::sleep(d);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                biased;
                _ = closing.changed() => return Pause::Shutdown,
                () = &mut sleep => return Pause::Elapsed,
                ev = rx.recv() => match ev {
                    Ok(e) if is_resolved_event(&e, key) => return Pause::Resolved,
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) if !self.pending(key) => return Pause::Resolved,
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return Pause::Shutdown,
                },
            }
        }
    }

    /// File one request, wait for the owner, act on the answer, withdraw if the request
    /// is answered elsewhere. See the module docs.
    async fn run_one(self: &Arc<Self>, key: PermKey) {
        let mut rx = self.manager.journal().subscribe(None).live;
        let mut closing = self.manager.closing();
        let mut delay = self.cfg.retry_base;
        let mut warned = false;
        'file: loop {
            // 1. File the approval (retrying on server errors).
            let filed = loop {
                if *closing.borrow() {
                    return;
                }
                let Some(c) = self.manager.approval_candidate(key.agent_id.as_deref(), &key.id) else { return };
                let req = build_request(&c, lucina_approvals::now_unix());
                match self.client.create(&req).await {
                    Ok(created) => {
                        if warned {
                            tracing::info!(request = %key.id, "approvals server answers again");
                        }
                        break Filed { approval_id: created.id, sensitive: req.sensitive };
                    }
                    Err(e) if retryable(&e) => {
                        if warned {
                            tracing::debug!(request = %key.id, error = %e, "could not file the owner approval; retrying");
                        } else {
                            warned = true;
                            tracing::warn!(request = %key.id, error = %e, "could not file the owner approval; retrying");
                        }
                        match self.pause(&mut rx, &mut closing, &key, delay).await {
                            Pause::Elapsed => delay = (delay * 2).min(self.cfg.retry_max),
                            Pause::Resolved | Pause::Shutdown => return,
                        }
                    }
                    Err(e) => {
                        tracing::warn!(request = %key.id, error = %e, "the approvals server refused the request; it stays with the web and lagent");
                        Self::lock(&self.gave_up).insert(key.clone());
                        return;
                    }
                }
            };
            delay = self.cfg.retry_base;
            if !self.manager.attach_approval(key.agent_id.as_deref(), &key.id, &filed.approval_id) {
                // Answered while the approval was being filed.
                self.withdraw(&filed.approval_id, 1).await;
                return;
            }
            tracing::info!(request = %key.id, approval = %filed.approval_id, sensitive = filed.sensitive, "permission request filed as an owner approval");

            // 2. Wait for the owner, or for the request to be answered elsewhere.
            loop {
                let polled = tokio::time::Instant::now();
                tokio::select! {
                    biased;
                    _ = closing.changed() => {
                        self.withdraw(&filed.approval_id, 1).await;
                        return;
                    }
                    ev = rx.recv() => match ev {
                        Ok(e) if is_resolved_event(&e, &key) => {
                            self.withdraw(&filed.approval_id, 3).await;
                            return;
                        }
                        Ok(_) => {}
                        Err(broadcast::error::RecvError::Lagged(_)) => {
                            if !self.pending(&key) {
                                self.withdraw(&filed.approval_id, 3).await;
                                return;
                            }
                        }
                        Err(broadcast::error::RecvError::Closed) => return,
                    },
                    r = self.client.wait_once(&filed.approval_id, self.cfg.wait_secs, Duration::from_secs(self.cfg.wait_secs + 10)) => match r {
                        Ok(reply) => match self.apply_reply(&key, &filed, reply).await {
                            Applied::Waiting => {
                                warned = false;
                                if polled.elapsed() < Duration::from_millis(200) {
                                    tokio::time::sleep(Duration::from_millis(200)).await;
                                }
                            }
                            Applied::Allowed { device_id } => {
                                tracing::info!(request = %key.id, approval = %filed.approval_id, device = %device_id, "permission allowed from an owner approval");
                                return;
                            }
                            Applied::Denied => {
                                tracing::info!(request = %key.id, approval = %filed.approval_id, "permission denied from an owner approval");
                                return;
                            }
                            Applied::Expired => {
                                tracing::debug!(request = %key.id, approval = %filed.approval_id, "the owner approval expired; the request stays pending");
                                return;
                            }
                            Applied::NoOp(why) => {
                                tracing::debug!(request = %key.id, approval = %filed.approval_id, why, "owner approval answer ignored");
                                return;
                            }
                            Applied::Refused(why) => {
                                tracing::warn!(request = %key.id, approval = %filed.approval_id, why = %why, "owner approval answer refused; the request stays pending");
                                return;
                            }
                        },
                        Err(ApprovalError::NotFound) => {
                            // The server lost it (a restart drops pending approvals): file it again.
                            tracing::warn!(request = %key.id, approval = %filed.approval_id, "the approvals server forgot the approval; filing it again");
                            continue 'file;
                        }
                        Err(e) if retryable(&e) => {
                            if warned {
                                tracing::debug!(request = %key.id, error = %e, "waiting for the owner approval failed; retrying");
                            } else {
                                warned = true;
                                tracing::warn!(request = %key.id, error = %e, "waiting for the owner approval failed; retrying");
                            }
                            match self.pause(&mut rx, &mut closing, &key, delay).await {
                                Pause::Elapsed => delay = (delay * 2).min(self.cfg.retry_max),
                                Pause::Resolved => {
                                    self.withdraw(&filed.approval_id, 3).await;
                                    return;
                                }
                                Pause::Shutdown => {
                                    self.withdraw(&filed.approval_id, 1).await;
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(request = %key.id, error = %e, "owner approval wait refused; the request stays pending");
                            Self::lock(&self.gave_up).insert(key.clone());
                            return;
                        }
                    },
                }
            }
        }
    }

    /// `POST …/withdraw`, idempotent; a server that no longer knows the approval is fine.
    async fn withdraw(&self, approval_id: &str, attempts: u32) {
        let mut delay = self.cfg.retry_base;
        for attempt in 1..=attempts {
            match tokio::time::timeout(Duration::from_secs(3), self.client.withdraw(approval_id)).await {
                Ok(Ok(status)) => {
                    tracing::debug!(approval = approval_id, ?status, "approval withdrawn");
                    return;
                }
                Ok(Err(ApprovalError::NotFound)) => return,
                Ok(Err(e)) if attempt == attempts => {
                    tracing::warn!(approval = approval_id, error = %e, "could not withdraw the owner approval");
                }
                Err(_) if attempt == attempts => {
                    tracing::warn!(approval = approval_id, "withdrawing the owner approval timed out");
                }
                _ => tokio::time::sleep(delay).await,
            }
            delay = (delay * 2).min(self.cfg.retry_max);
        }
    }

    /// Act on one `/wait` answer of the approval `filed` for request `key`. This is the
    /// only place an answer from the owner's approval reaches the permission book.
    pub async fn apply_reply(&self, key: &PermKey, filed: &Filed, reply: WaitReply) -> Applied {
        if !reply.id.is_empty() && reply.id != filed.approval_id {
            return Applied::Refused(format!("the answer is for another approval ({})", reply.id));
        }
        match reply.status {
            ApprovalStatus::Pending => Applied::Waiting,
            ApprovalStatus::Expired => Applied::Expired,
            ApprovalStatus::Denied => {
                if !self.pending(key) {
                    return Applied::NoOp("the request was answered or has expired already");
                }
                self.answer(key, filed, Verdict::Deny, None).await
            }
            ApprovalStatus::Approved => {
                let Some(grant_text) = reply.grant else {
                    return Applied::Refused("approved without a grant".into());
                };
                // Bind the grant to the request as it is now, and to this approval.
                let Some(view) = self.manager.permission_view(key.agent_id.as_deref(), &key.id) else {
                    return Applied::NoOp("the request is gone");
                };
                if view.status != Status::Pending {
                    return Applied::NoOp("the request was answered or has expired already");
                }
                if view.approval_id.as_deref() != Some(filed.approval_id.as_str()) {
                    return Applied::NoOp("another approval is attached to the request");
                }
                let expected_hash = payload_hash_of(&view);
                let verified = {
                    let pubkey_file = self.cfg.pubkey_file.clone();
                    let (approval_id, sensitive) = (filed.approval_id.clone(), filed.sensitive);
                    tokio::task::spawn_blocking(move || -> Result<grant::Claims, String> {
                        let key = PubKey::load(&pubkey_file).map_err(|e| format!("pinned key: {e}"))?;
                        grant::verify(
                            &key,
                            &grant_text,
                            &Expect { action: ACTION, payload_hash: &expected_hash, id: Some(&approval_id), require_biometric: sensitive, now: lucina_approvals::now_unix() },
                        )
                        .map_err(|e: GrantError| e.to_string())
                    })
                    .await
                };
                let claims = match verified {
                    Ok(Ok(c)) => c,
                    Ok(Err(why)) => return Applied::Refused(why),
                    Err(_) => return Applied::Refused("grant check failed".into()),
                };
                if claims.requester.kind != REQUESTER_KIND {
                    return Applied::Refused(format!("the grant is for a request made by {:?}, not agentd", claims.requester.kind));
                }
                if !is_owner_device(&claims.decided_by) {
                    return Applied::Refused("the grant was not decided by an owner device".into());
                }
                let used = self.used.clone();
                let (id, exp) = (claims.id.clone(), claims.exp);
                let spent = tokio::task::spawn_blocking(move || used.consume(&id, exp, lucina_approvals::now_unix())).await;
                match spent {
                    Ok(Ok(())) => {}
                    Ok(Err(ApprovalError::Grant(GrantError::Replay))) => return Applied::Refused("the grant was already used".into()),
                    Ok(Err(e)) => return Applied::Refused(format!("could not record the grant as used: {e}")),
                    Err(_) => return Applied::Refused("could not record the grant as used".into()),
                }
                self.answer(key, filed, Verdict::Allow, Some(&claims.decided_by)).await
            }
        }
    }

    async fn answer(&self, key: &PermKey, filed: &Filed, verdict: Verdict, device_id: Option<&str>) -> Applied {
        match self.manager.answer_from_approval(key.agent_id.as_deref(), &key.id, &filed.approval_id, verdict, device_id).await {
            Ok(()) => match verdict {
                Verdict::Allow => Applied::Allowed { device_id: device_id.unwrap_or_default().to_string() },
                Verdict::Deny => Applied::Denied,
            },
            // Answered, expired or gone in the meantime: nothing to do, and nothing to log loudly.
            Err(ManagerError::AlreadyAnswered(_) | ManagerError::Expired(_) | ManagerError::PermissionNotFound(_)) => {
                Applied::NoOp("the request was answered or has expired already")
            }
            Err(e) => Applied::Refused(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lucina_agents::permission_gate::RequestKind;

    fn view(kind: RequestKind, name: Option<&str>, input: Value) -> PermissionView {
        PermissionView {
            id: "req_1".into(),
            kind,
            agent_id: (kind == RequestKind::Agent).then(|| "agent_1".to_string()),
            source: (kind == RequestKind::External).then(|| "infisical-brokerd".to_string()),
            name: name.map(str::to_string),
            title: "Reveal a secret".into(),
            description: None,
            details: input.as_object().cloned(),
            status: Status::Pending,
            created_at: "2026-10-28T10:00:00.000Z".into(),
            expires_at: "2026-10-28T11:00:00.000Z".into(),
            approval_id: None,
            resolution: None,
        }
    }

    fn cand(v: PermissionView) -> ApprovalCandidate {
        ApprovalCandidate { view: v, agent_title: Some("Fix the tests".into()), provider: Some("claude".into()) }
    }

    fn tool(name: &str, input: Value) -> PermissionView {
        view(RequestKind::Agent, Some(name), input)
    }

    #[test]
    fn the_hash_binds_agent_request_kind_name_and_input() {
        let a = tool("Bash", json!({"command": "ls"}));
        let h = payload_hash_of(&a);
        assert!(h.starts_with("sha256:") && h.len() == 71);
        assert_eq!(payload_hash_of(&a.clone()), h);
        let mut other = a.clone();
        other.agent_id = Some("agent_2".into());
        assert_ne!(payload_hash_of(&other), h, "agent id");
        let mut other = a.clone();
        other.id = "req_2".into();
        assert_ne!(payload_hash_of(&other), h, "request id");
        let mut other = a.clone();
        other.name = Some("Edit".into());
        assert_ne!(payload_hash_of(&other), h, "tool");
        assert_ne!(payload_hash_of(&tool("Bash", json!({"command": "ls -la"}))), h, "input");
        // The titles and the status do not matter, the content does.
        let mut same = a.clone();
        same.title = "other".into();
        same.approval_id = Some("apr_x".into());
        assert_eq!(payload_hash_of(&same), h);
        // The canonical form: key order in the input is irrelevant.
        assert_eq!(payload_hash_of(&tool("T", json!({"a": 1, "b": 2}))), payload_hash_of(&tool("T", json!({"b": 2, "a": 1}))));
        // The documented shape.
        let v = payload_value(&a);
        assert_eq!(v, json!({"agentId": "agent_1", "requestId": "req_1", "kind": "agent", "name": "Bash", "input": {"command": "ls"}}));
        let ext = view(RequestKind::External, None, json!({}));
        assert_eq!(payload_value(&ext)["agentId"], Value::Null);
        assert_eq!(payload_value(&ext)["kind"], "external");
        assert_eq!(payload_value(&ext)["name"], "infisical-brokerd");
        let none = tool("Bash", json!(null));
        assert_eq!(payload_value(&none)["input"], Value::Null);
    }

    #[test]
    fn what_needs_face_id() {
        for (name, input) in [
            ("Bash", json!({"command": "sudo systemctl restart nginx"})),
            ("Bash", json!({"command": "ssh root@host uptime"})),
            ("Bash", json!({"command": "chown root:root /etc/x"})),
            ("Bash", json!({"command": "cat /root/.ssh/id_rsa"})),
            ("Bash", json!({"command": "cat ~/.env"})),
            ("Bash", json!({"command": "echo $API_KEY"})),
            ("Bash", json!({"command": "doas ls"})),
            ("Bash", json!({"command": "export PASSWORD=hunter2"})),
            ("Bash", json!({"command": "infisical run -- env"})),
            ("mcp__lucina__secret_reveal", json!({})),
            ("mcp__lucina__root_exec", json!({"plan": []})),
            ("Read", json!({"file_path": "/home/nez/.ssh/id_ed25519"})),
            ("Write", json!({"file_path": "/x", "content": "show the secrets"})),
        ] {
            assert!(is_sensitive(&tool(name, input.clone())), "{name} {input}");
        }
        for (name, input) in [
            ("Bash", json!({"command": "ls -la"})),
            ("Bash", json!({"command": "cargo test -p lucina-agentd"})),
            ("Bash", json!({"command": "git status"})),
            ("Edit", json!({"file_path": "crates/lucina-approvals/src/root.rs"})),
            ("Read", json!({"file_path": "crates/secretary-server/src/lib.rs"})),
            ("Bash", json!({"command": "grep -rn tokenizer src"})),
            ("Bash", json!({"command": "echo pseudo environment"})),
            ("mcp__lucina__notes_search", json!({"query": "groceries"})),
            ("Bash", json!(null)),
        ] {
            assert!(!is_sensitive(&tool(name, input.clone())), "{name} {input}");
        }
        // Every external request is sensitive.
        assert!(is_sensitive(&view(RequestKind::External, None, json!({"name": "x"}))));
    }

    #[test]
    fn secret_looking_values_are_masked_but_names_stay() {
        let t = redact("curl -H 'Authorization: Bearer abcdef1234567890' https://x --data token=abcd1234efgh");
        assert!(!t.contains("abcdef1234567890") && !t.contains("abcd1234efgh"), "{t}");
        assert!(t.contains("Authorization: [redacted]") && t.contains("token=[redacted]"), "{t}");
        assert_eq!(redact("send Bearer abcdef1234567890 now"), "send Bearer [redacted] now");
        let t = redact("export GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123 && echo svc_abcdefghijklmnop1234 sk-abcdefghijklmnopqrstu");
        for leaked in ["ghp_abcdef", "svc_abcdefgh", "sk-abcdefgh"] {
            assert!(!t.contains(leaked), "{t}");
        }
        assert!(t.contains("GITHUB_TOKEN="), "{t}");
        let t = redact("-----BEGIN OPENSSH PRIVATE KEY-----\nAAAAB3Nza\n-----END OPENSSH PRIVATE KEY-----\nafter");
        assert_eq!(t, "[redacted private key]\nafter");
        let t = redact(r#"{"password": "correct horse"}"#);
        assert!(!t.contains("correct"), "{t}");
        assert_eq!(redact("ls -la /tmp"), "ls -la /tmp");
        assert_eq!(redact("the secretary app"), "the secretary app");
    }

    #[test]
    fn the_summary_is_one_line_and_names_the_provider_and_the_thing() {
        let c = cand(tool("Bash", json!({"command": "cargo test -p x\ncargo clippy"})));
        assert_eq!(summary(&c), "Claude wants to run: cargo test -p x cargo clippy");
        let c = cand(tool("Edit", json!({"file_path": "/repo/src/lib.rs", "old_string": "a"})));
        assert_eq!(summary(&c), "Claude wants to run: Edit /repo/src/lib.rs");
        let c = cand(tool("mcp__lucina__notes_search", json!({"q": 1})));
        assert_eq!(summary(&c), "Claude wants to run: lucina: notes_search");
        let c = cand(tool("mcp__lucina__notes_search", json!({"query": "groceries"})));
        assert_eq!(summary(&c), "Claude wants to run: lucina: notes_search groceries");
        let c = cand(tool("Bash", json!({"command": "x".repeat(500)})));
        assert!(summary(&c).chars().count() <= MAX_SUMMARY);
        let c = cand(tool("Bash", json!({"command": "curl -H 'Authorization: Bearer abcdef1234567890' x"})));
        assert!(!summary(&c).contains("abcdef1234567890"));
        let c = cand(view(RequestKind::External, None, json!({})));
        assert_eq!(summary(&c), "infisical-brokerd asks: Reveal a secret");
    }

    #[test]
    fn details_show_the_tool_and_the_input_and_say_when_they_are_cut() {
        let c = cand(tool("Bash", json!({"command": "echo hi\necho there", "timeout": 5})));
        let (d, cut) = details(&c);
        assert!(!cut);
        assert!(d.starts_with("Tool: Bash\nAgent: Fix the tests\nInput:\n"), "{d}");
        assert!(d.contains("command:\necho hi\necho there\n") && d.contains("timeout: 5"), "{d}");
        let long = cand(tool("Write", json!({"content": "y".repeat(9000)})));
        let (d, cut) = details(&long);
        assert!(cut && d.chars().count() <= MAX_DETAILS && d.contains("cut: first"), "{}", d.len());
        // A cut text cannot be read in full: it is asked as a sensitive one.
        let req = build_request(&long, 0);
        assert!(req.sensitive && req.risk == "high");
        assert!(req.details.chars().count() <= MAX_DETAILS);
    }

    #[test]
    fn the_request_follows_the_plan() {
        let c = cand(tool("Bash", json!({"command": "ls"})));
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-28T10:30:00Z").unwrap().timestamp();
        let r = build_request(&c, now);
        assert_eq!((r.action.as_str(), r.requester.kind.as_str(), r.requester.name.as_str()), ("agent.permission", "agentd", "Lucina Agents"));
        assert_eq!(r.requester.detail, "Fix the tests · claude");
        assert!(!r.sensitive && r.risk == "medium");
        assert_eq!(r.payload_hash, payload_hash_of(&c.view));
        assert_eq!(r.ttl_secs, 900, "an hour left clamps to 900");
        let mut soon = c.view.clone();
        soon.expires_at = "2026-10-28T10:30:05Z".into();
        assert_eq!(ttl_secs(&soon, now), 30, "a few seconds left clamps to 30");
        soon.expires_at = "2026-10-28T10:35:00Z".into();
        assert_eq!(ttl_secs(&soon, now), 300);
        let ext = build_request(&cand(view(RequestKind::External, None, json!({}))), now);
        assert!(ext.sensitive && ext.risk == "high");
        assert_eq!(ext.requester.detail, "infisical-brokerd");
        let sudo = build_request(&cand(tool("Bash", json!({"command": "sudo ls"}))), now);
        assert!(sudo.sensitive && sudo.risk == "high");
    }

    #[test]
    fn only_owner_devices_count_as_deciders() {
        assert!(is_owner_device("dev_abc-123"));
        for bad in ["", "svc", "mcp:hermes", "service:infisical", "agent_1", "device:dev_x", "dev_a/b", "xdev_1"] {
            assert!(!is_owner_device(bad), "{bad}");
        }
    }

    #[test]
    fn settings_come_from_the_environment_and_the_bridge_is_off_without_them() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("k.pub");
        let no = |_: &str| None::<String>;
        assert!(BridgeConfig::from_lookup(&no).unwrap_err().contains("not both set"));
        let env = |extra: &'static [(&'static str, &str)], key: String| {
            let extra: Vec<(String, String)> = extra.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            move |k: &str| match k {
                URL_ENV => Some("http://127.0.0.1:1/".to_string()),
                TOKEN_ENV => Some("svc_x".to_string()),
                PUBKEY_ENV => Some(key.clone()),
                "HOME" => Some("/home/x".to_string()),
                other => extra.iter().find(|(n, _)| n == other).map(|(_, v)| v.clone()),
            }
        };
        // No key file: off.
        let err = BridgeConfig::from_lookup(&env(&[], key.display().to_string())).unwrap_err();
        assert!(err.contains("no pinned approval key"), "{err}");
        std::fs::write(&key, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n").unwrap();
        let c = BridgeConfig::from_lookup(&env(&[], key.display().to_string())).unwrap();
        assert_eq!(c.url, "http://127.0.0.1:1");
        assert_eq!(c.used_file, PathBuf::from("/home/x/.local/state/lucina-agentd/approvals-used.jsonl"));
        assert!(!format!("{c:?}").contains("svc_x"), "the token never shows in Debug");
        // Switched off.
        let err = BridgeConfig::from_lookup(&env(&[(OFF_ENV, "off")], key.display().to_string())).unwrap_err();
        assert!(err.contains("off"), "{err}");
        // Token missing.
        let err = BridgeConfig::from_lookup(&|k: &str| (k == URL_ENV).then(|| "http://x".to_string())).unwrap_err();
        assert!(err.contains("not both set"));
        // The default key path.
        let err = BridgeConfig::from_lookup(&|k: &str| match k {
            URL_ENV => Some("http://x".into()),
            TOKEN_ENV => Some("svc_x".into()),
            "HOME" => Some(dir.path().display().to_string()),
            _ => None,
        })
        .unwrap_err();
        assert!(err.contains("approval-grant.pub"), "{err}");
    }
}
