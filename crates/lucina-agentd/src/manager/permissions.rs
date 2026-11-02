//! Permission requests: the ones providers raise for their agents, and external ones
//! (the Infisical broker asking the owner before it hands out a secret). Every answer
//! and every expiry goes through `lucina_agents::permission_gate`.
//!
//! - Agent requests appear in the agent's `pendingPermissions` (Paseo's shape) and
//!   in the permission book; answers come from `lagent permit` or, through
//!   secretary-server, the owner's devices. A request whose tool the agent's tool
//!   policy preapproves is allowed at once and never shown as pending.
//! - External requests live only in the book. A caller creates one with
//!   `POST /permissions`, then waits on `GET /permissions/{id}?waitMs=` or the
//!   stream. They are not persisted: after a daemon restart they are gone, and a
//!   caller that gets 404 treats that as denied.
//! - Unanswered requests are denied when they expire (agent requests after the
//!   configured TTL, external ones after their own `expiresInSec`).

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lucina_agents::permission_gate::{self as pg, Gate, Origin, Outcome, RequestKind, Status, ToolPolicy, Verdict};
use lucina_agents::types::AttentionReason;
use lucina_agents::{PermissionRequest, PermissionResponse};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::{Agent, BusEvent, Manager, ManagerError};
use crate::notify::Push;
use crate::now_iso;

/// What a caller (the broker) asks the owner.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalRequest {
    /// Must be `external`.
    pub kind: String,
    /// Who asks, e.g. `infisical-brokerd` (`[a-z0-9-]`, up to 40).
    pub source: String,
    /// One line, up to 120 characters.
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Shown to the owner as-is (up to 8 KB of JSON).
    #[serde(default)]
    pub details: Option<Map<String, Value>>,
    /// 10 to 3600 seconds; 300 when missing.
    #[serde(default)]
    pub expires_in_sec: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolution {
    pub behavior: Verdict,
    /// `owner_device`, `local`, `policy`, `expiry` or `provider` (the provider
    /// withdrew the request, e.g. its turn was interrupted).
    pub by: &'static str,
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The owner device whose approval answered it (verified grant of the approvals bridge).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionView {
    pub id: String,
    pub kind: RequestKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The tool (agent requests).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Map<String, Value>>,
    pub status: Status,
    pub created_at: String,
    pub expires_at: String,
    /// The owner approval (secretary-server) filed for this request, once it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
}

pub(super) struct Entry {
    view: PermissionView,
    gate: Gate,
    resolved_at_ms: Option<u64>,
    /// Questions (AskUserQuestion-style) need an answer, not a Yes: no owner approval for them.
    question: bool,
}

/// A pending request the approvals bridge may file as an owner approval.
#[derive(Debug, Clone)]
pub struct ApprovalCandidate {
    pub view: PermissionView,
    /// The agent's title and provider (agent requests).
    pub agent_title: Option<String>,
    pub provider: Option<String>,
}

/// Resolved entries stay readable this long.
const KEEP_RESOLVED_MS: u64 = 3_600_000;
pub const EXPIRED_MESSAGE: &str = "Permission request expired";

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn iso_at(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn by(origin: Origin) -> &'static str {
    match origin {
        Origin::OwnerDevice => "owner_device",
        Origin::Local => "local",
        Origin::Policy => "policy",
        Origin::Mcp | Origin::Agent | Origin::Unknown => "untrusted",
    }
}

fn verdict_of(r: &PermissionResponse) -> Verdict {
    match r {
        PermissionResponse::Allow { .. } => Verdict::Allow,
        PermissionResponse::Deny { .. } => Verdict::Deny,
    }
}

fn expired_response() -> PermissionResponse {
    PermissionResponse::Deny { selected_action_id: None, message: Some(EXPIRED_MESSAGE.into()), interrupt: None }
}

pub(super) fn agent_key(agent_id: &str, request_id: &str) -> String {
    format!("{agent_id}/{request_id}")
}

/// The book's key: agent requests by agent and request id, external ones by their id.
fn book_key(agent_id: Option<&str>, id: &str) -> String {
    agent_id.map_or_else(|| id.to_string(), |a| agent_key(a, id))
}

fn refusal(outcome: Outcome, what: &str) -> ManagerError {
    match outcome {
        Outcome::NotAllowed => ManagerError::Forbidden(format!("This caller may not answer {what}: only the owner can")),
        Outcome::AlreadyAnswered => ManagerError::AlreadyAnswered(format!("{what} was already answered")),
        Outcome::Expired => ManagerError::Expired(format!("{what} expired and was denied")),
        Outcome::Applied => ManagerError::Invalid("applied".into()),
    }
}

impl Entry {
    fn apply(&mut self, step: &pg::Step, origin_by: &'static str, message: Option<String>, now: u64) {
        self.gate = step.next;
        self.view.status = step.next.status;
        if let Some(v) = step.resolve {
            let expired = step.outcome == Outcome::Expired;
            self.view.resolution = Some(Resolution {
                behavior: v,
                by: if expired { "expiry" } else { origin_by },
                at: iso_at(now),
                message: if expired { Some(EXPIRED_MESSAGE.into()) } else { message },
                device_id: None,
            });
            self.resolved_at_ms = Some(now);
        }
    }
}

impl Manager {
    fn book(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, Entry>> {
        self.inner.book.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn publish_permission(&self, view: &PermissionView) {
        let _ = self.inner.bus.send(BusEvent::Permission { permission: Box::new(view.clone()) });
    }

    fn prune(book: &mut std::collections::HashMap<String, Entry>, now: u64) {
        book.retain(|_, e| e.resolved_at_ms.is_none_or(|t| now.saturating_sub(t) < KEEP_RESOLVED_MS));
    }

    /// The provider pids an agent-origin caller would descend from.
    pub fn provider_pids(&self) -> HashSet<u32> {
        let mut pids = self.inner.options.registry.as_ref().map(|r| r.live_pids()).unwrap_or_default();
        // Whatever runs in a terminal counts as an agent's process, whoever opened it.
        pids.extend(self.inner.terminals.session_pids());
        pids
    }

    /// A provider raised `request` (called from `on_event`, under the agents lock,
    /// before the lifecycle transition). Returns `true` when the agent's tool policy
    /// preapproves it: the caller then answers it right away instead of showing it.
    pub(super) fn register_agent_request(&self, agent: &Agent, request: &PermissionRequest) -> bool {
        let now = now_ms();
        let ttl = self.inner.options.permission_ttl.as_millis() as u64;
        let mut gate = Gate::new(RequestKind::Agent, now.saturating_add(ttl));
        let mut view = PermissionView {
            id: request.id.clone(),
            kind: RequestKind::Agent,
            agent_id: Some(agent.record.id.clone()),
            source: None,
            name: Some(request.name.clone()),
            title: request.title.clone().unwrap_or_else(|| request.name.clone()),
            description: request.description.clone(),
            details: request.input.clone(),
            status: Status::Pending,
            created_at: iso_at(now),
            expires_at: iso_at(gate.expires_at_ms),
            approval_id: None,
            resolution: None,
        };
        let policy: ToolPolicy = agent
            .record
            .config
            .tool_policy
            .clone()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        // agentd's own MCP tools (`lagent mcp`) enforce their limits themselves.
        let preapproved = request.kind == lucina_agents::permission::PermissionKind::Tool
            && (policy.preapproves(&request.name) || crate::mcp::is_own_tool(&request.name));
        let mut resolved_at_ms = None;
        if preapproved {
            let step = pg::answer(&gate, Origin::Policy, Verdict::Allow, now);
            gate = step.next;
            view.status = gate.status;
            view.resolution = Some(Resolution { behavior: Verdict::Allow, by: "policy", at: iso_at(now), message: None, device_id: None });
            resolved_at_ms = Some(now);
        }
        {
            let mut book = self.book();
            Self::prune(&mut book, now);
            let question = request.kind == lucina_agents::permission::PermissionKind::Question;
            book.insert(agent_key(&agent.record.id, &request.id), Entry { view: view.clone(), gate, resolved_at_ms, question });
        }
        self.publish_permission(&view);
        if !preapproved {
            let manager = self.clone();
            let (agent_id, request_id) = (agent.record.id.clone(), request.id.clone());
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(ttl)).await;
                manager.expire_agent_request(&agent_id, &request_id).await;
            });
        }
        preapproved
    }

    /// The provider resolved a request itself, or the turn ended and every pending
    /// request was denied: close its entry (it can't be answered any more).
    pub(super) fn withdraw_agent_request(&self, agent_id: &str, request_id: &str, resolution: &PermissionResponse) {
        let now = now_ms();
        let view = {
            let mut book = self.book();
            let Some(e) = book.get_mut(&agent_key(agent_id, request_id)) else { return };
            if e.gate.status != Status::Pending {
                return;
            }
            let v = verdict_of(resolution);
            e.gate.status = match v {
                Verdict::Allow => Status::Allowed,
                Verdict::Deny => Status::Denied,
            };
            e.view.status = e.gate.status;
            let message = match resolution {
                PermissionResponse::Deny { message, .. } => message.clone(),
                PermissionResponse::Allow { .. } => None,
            };
            e.view.resolution = Some(Resolution { behavior: v, by: "provider", at: iso_at(now), message, device_id: None });
            e.resolved_at_ms = Some(now);
            e.view.clone()
        };
        self.publish_permission(&view);
    }

    /// Allow a preapproved request on the provider (called right after
    /// `register_agent_request` returned `true`).
    pub(super) fn auto_allow(&self, agent: &Agent, request_id: &str) {
        let Some(session) = agent.session.clone() else { return };
        let request_id = request_id.to_string();
        tokio::spawn(async move {
            let allow = PermissionResponse::Allow { selected_action_id: None, updated_input: None, updated_permissions: None };
            if let Err(e) = session.respond_permission(&request_id, allow).await {
                tracing::warn!(request = %request_id, error = %e, "preapproved permission: provider refused the answer");
            }
        });
    }

    /// Answer an agent's permission request (Paseo `respondToPermission`), then wait
    /// up to 15 s for the provider to confirm it (Paseo `respondToPermissionAndWait`).
    pub async fn respond_permission(&self, agent_id: &str, request_id: &str, response: PermissionResponse, origin: Origin) -> Result<(), ManagerError> {
        self.respond_permission_as(agent_id, request_id, response, origin, None).await
    }

    /// [`Self::respond_permission`], remembering which owner device's approval answered.
    pub(super) async fn respond_permission_as(&self, agent_id: &str, request_id: &str, response: PermissionResponse, origin: Origin, device: Option<&str>) -> Result<(), ManagerError> {
        let what = format!("Permission request {request_id}");
        let key = agent_key(agent_id, request_id);
        let now = now_ms();
        let (session, to_send, step, external) = {
            let agents = self.lock();
            let agent = agents.get(agent_id).ok_or_else(|| ManagerError::NotFound(agent_id.to_string()))?;
            if !agent.pending.iter().any(|p| p.id == request_id) {
                return Err(ManagerError::PermissionNotFound(format!("Permission request not found: {request_id}")));
            }
            let mut book = self.book();
            let entry = book.get_mut(&key).ok_or_else(|| ManagerError::PermissionNotFound(format!("Permission request not found: {request_id}")))?;
            let step = pg::answer(&entry.gate, origin, verdict_of(&response), now);
            if step.outcome == Outcome::NotAllowed || step.outcome == Outcome::AlreadyAnswered {
                return Err(refusal(step.outcome, &what));
            }
            let message = match &response {
                PermissionResponse::Deny { message, .. } => message.clone(),
                PermissionResponse::Allow { .. } => None,
            };
            entry.apply(&step, by(origin), message, now);
            if step.outcome == Outcome::Applied {
                if let Some(r) = entry.view.resolution.as_mut() {
                    r.device_id = device.map(str::to_string);
                }
            }
            let view = entry.view.clone();
            drop(book);
            self.publish_permission(&view);
            let to_send = if step.outcome == Outcome::Expired { expired_response() } else { response };
            (agent.session.clone(), to_send, step, agent.record.external)
        };
        let Some(session) = session else {
            return Err(ManagerError::Provider("the agent has no running session".into()));
        };
        if let Err(e) = session.respond_permission(request_id, to_send).await {
            // The answer never reached the provider: the request stays open (Paseo
            // keeps it pending too).
            let mut book = self.book();
            if let Some(entry) = book.get_mut(&key) {
                entry.gate.status = Status::Pending;
                entry.view.status = Status::Pending;
                entry.view.resolution = None;
                entry.resolved_at_ms = None;
            }
            return Err(ManagerError::Provider(e.0));
        }
        if step.outcome == Outcome::Expired {
            return Err(refusal(Outcome::Expired, &what));
        }
        if external {
            // Queued for the owner device, which may be asleep: it confirms by posting
            // `permission_resolved` when it applies the answer.
            return Ok(());
        }
        let mut rx = self.inner.generation.subscribe();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let still = self.lock().get(agent_id).is_some_and(|a| a.pending.iter().any(|p| p.id == request_id));
            if !still {
                return Ok(());
            }
            if tokio::time::timeout_at(deadline, rx.changed()).await.is_err() {
                return Err(ManagerError::Unconfirmed(format!("Permission response unconfirmed for request {request_id}: timed out")));
            }
        }
    }

    async fn expire_agent_request(&self, agent_id: &str, request_id: &str) {
        let now = now_ms();
        let session = {
            let mut agents = self.lock();
            let mut book = self.book();
            let Some(entry) = book.get_mut(&agent_key(agent_id, request_id)) else { return };
            let step = pg::expire(&entry.gate, now);
            if step.resolve.is_none() {
                return;
            }
            entry.apply(&step, "expiry", None, now);
            let view = entry.view.clone();
            drop(book);
            self.publish_permission(&view);
            let Some(agent) = agents.get_mut(agent_id) else { return };
            self.append(
                agent,
                lucina_agents::TimelineItem::Notification {
                    level: lucina_agents::timeline::NotificationLevel::Warning,
                    message: format!("{EXPIRED_MESSAGE}: {} was denied.", view.title),
                },
                agent.active_turn.as_ref().map(|t| t.turn_id.clone()),
            );
            self.persist(agent);
            agent.session.clone()
        };
        if let Some(s) = session {
            if let Err(e) = s.respond_permission(request_id, expired_response()).await {
                tracing::warn!(agent = agent_id, request = request_id, error = %e, "expired permission: provider refused the deny");
            }
        }
        self.bump();
    }

    // -- external requests -------------------------------------------------------

    pub fn request_external(&self, req: ExternalRequest, origin: Origin) -> Result<PermissionView, ManagerError> {
        if matches!(origin, Origin::Agent | Origin::Mcp | Origin::Unknown) {
            return Err(ManagerError::Forbidden("This caller may not ask the owner for permission".into()));
        }
        if req.kind != "external" {
            return Err(ManagerError::Invalid("kind: must be \"external\"".into()));
        }
        let source_ok = !req.source.is_empty()
            && req.source.len() <= 40
            && req.source.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !source_ok {
            return Err(ManagerError::Invalid("source: 1 to 40 of [a-z0-9-]".into()));
        }
        let title = req.title.trim();
        if title.is_empty() || title.chars().count() > 120 || title.contains('\n') {
            return Err(ManagerError::Invalid("title: one line, 1 to 120 characters".into()));
        }
        if req.description.as_deref().is_some_and(|d| d.chars().count() > 2000) {
            return Err(ManagerError::Invalid("description: up to 2000 characters".into()));
        }
        if req.details.as_ref().is_some_and(|d| serde_json::to_string(d).map(|s| s.len()).unwrap_or(0) > 8192) {
            return Err(ManagerError::Invalid("details: up to 8 KB of JSON".into()));
        }
        let ttl_s = req.expires_in_sec.unwrap_or(300);
        if !(10..=3600).contains(&ttl_s) {
            return Err(ManagerError::Invalid("expiresInSec: 10 to 3600".into()));
        }
        let now = now_ms();
        let gate = Gate::new(RequestKind::External, now + ttl_s * 1000);
        let id = format!("ext_{}", uuid::Uuid::new_v4().simple());
        let view = PermissionView {
            id: id.clone(),
            kind: RequestKind::External,
            agent_id: None,
            source: Some(req.source.clone()),
            name: None,
            title: title.to_string(),
            description: req.description.clone(),
            details: req.details.clone(),
            status: Status::Pending,
            created_at: iso_at(now),
            expires_at: iso_at(gate.expires_at_ms),
            approval_id: None,
            resolution: None,
        };
        {
            let mut book = self.book();
            Self::prune(&mut book, now);
            book.insert(id.clone(), Entry { view: view.clone(), gate, resolved_at_ms: None, question: false });
        }
        self.publish_permission(&view);
        if let Some(n) = &self.inner.options.notifier {
            n.send(Push {
                category: "agent",
                title: Some(view.title.clone()),
                body: lucina_agents::notification::preview(view.description.as_deref())
                    .unwrap_or_else(|| format!("{} asks for your OK.", req.source)),
                route: format!("/agents/permissions/{id}"),
                dedup_key: format!("perm:{id}"),
                data: json!({"kind": "external", "permissionId": id, "source": req.source, "reason": "permission",
                    "actions": [{"id": "allow", "behavior": "allow"}, {"id": "deny", "behavior": "deny"}]}),
            });
        }
        let manager = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(ttl_s)).await;
            manager.expire_external(&id);
        });
        Ok(view)
    }

    fn expire_external(&self, id: &str) {
        let now = now_ms();
        let view = {
            let mut book = self.book();
            let Some(entry) = book.get_mut(id) else { return };
            let step = pg::expire(&entry.gate, now);
            if step.resolve.is_none() {
                return;
            }
            entry.apply(&step, "expiry", None, now);
            entry.view.clone()
        };
        self.publish_permission(&view);
        self.bump();
    }

    pub fn answer_external(&self, id: &str, verdict: Verdict, message: Option<String>, origin: Origin) -> Result<PermissionView, ManagerError> {
        self.answer_external_as(id, verdict, message, origin, None)
    }

    fn answer_external_as(&self, id: &str, verdict: Verdict, message: Option<String>, origin: Origin, device: Option<&str>) -> Result<PermissionView, ManagerError> {
        let now = now_ms();
        let (view, step) = {
            let mut book = self.book();
            let entry = book
                .get_mut(id)
                .filter(|e| e.view.kind == RequestKind::External)
                .ok_or_else(|| ManagerError::PermissionNotFound(format!("Permission request not found: {id}")))?;
            let step = pg::answer(&entry.gate, origin, verdict, now);
            if step.outcome == Outcome::NotAllowed || step.outcome == Outcome::AlreadyAnswered {
                return Err(refusal(step.outcome, &format!("Permission request {id}")));
            }
            entry.apply(&step, by(origin), message, now);
            if step.outcome == Outcome::Applied {
                if let Some(r) = entry.view.resolution.as_mut() {
                    r.device_id = device.map(str::to_string);
                }
            }
            (entry.view.clone(), step)
        };
        self.publish_permission(&view);
        self.bump();
        if step.outcome == Outcome::Expired {
            return Err(refusal(Outcome::Expired, &format!("Permission request {id}")));
        }
        Ok(view)
    }

    /// One external request (agent requests: the agent's snapshot).
    pub fn permission(&self, id: &str) -> Result<PermissionView, ManagerError> {
        self.book()
            .get(id)
            .filter(|e| e.view.kind == RequestKind::External)
            .map(|e| e.view.clone())
            .ok_or_else(|| ManagerError::PermissionNotFound(format!("Permission request not found: {id}")))
    }

    /// Until the request is answered or expires, or `timeout` passes.
    pub async fn wait_permission(&self, id: &str, timeout: Duration) -> Result<PermissionView, ManagerError> {
        let mut rx = self.inner.generation.subscribe();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let view = self.permission(id)?;
            if view.status != Status::Pending {
                return Ok(view);
            }
            if tokio::time::timeout_at(deadline, rx.changed()).await.is_err() {
                return Ok(view);
            }
        }
    }

    /// Every request, agent and external: pending ones, or all kept ones.
    pub fn permissions(&self, include_resolved: bool) -> Vec<PermissionView> {
        let mut v: Vec<PermissionView> = self
            .book()
            .values()
            .filter(|e| include_resolved || e.view.status == Status::Pending)
            .map(|e| e.view.clone())
            .collect();
        v.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.id.cmp(&b.id)));
        v
    }

    // -- owner approvals (the approvals bridge) ---------------------------------------

    /// Pending requests that have no owner approval yet and need a Yes (not questions).
    pub fn approval_candidates(&self) -> Vec<ApprovalCandidate> {
        let agents = self.lock();
        let book = self.book();
        book.values().filter(|e| e.view.approval_id.is_none()).filter_map(|e| Self::candidate_of(&agents, e)).collect()
    }

    /// One pending request that needs a Yes (with or without an approval already filed).
    pub fn approval_candidate(&self, agent_id: Option<&str>, id: &str) -> Option<ApprovalCandidate> {
        let agents = self.lock();
        let book = self.book();
        book.get(&book_key(agent_id, id)).and_then(|e| Self::candidate_of(&agents, e))
    }

    fn candidate_of(agents: &std::collections::HashMap<String, Agent>, e: &Entry) -> Option<ApprovalCandidate> {
        if e.view.status != Status::Pending || e.question {
            return None;
        }
        let agent = e.view.agent_id.as_deref().and_then(|id| agents.get(id));
        Some(ApprovalCandidate {
            view: e.view.clone(),
            agent_title: agent.and_then(|a| a.record.title.clone()),
            provider: agent.map(|a| a.record.provider.clone()),
        })
    }

    /// The request as it is now, any status (agent requests by `agent_id`).
    pub fn permission_view(&self, agent_id: Option<&str>, id: &str) -> Option<PermissionView> {
        self.book().get(&book_key(agent_id, id)).map(|e| e.view.clone())
    }

    /// Record the owner approval filed for a still-pending request (it replaces an
    /// earlier one) and tell the streams. `false`: the request is no longer pending.
    pub fn attach_approval(&self, agent_id: Option<&str>, id: &str, approval_id: &str) -> bool {
        let view = {
            let mut book = self.book();
            let Some(e) = book.get_mut(&book_key(agent_id, id)) else { return false };
            if e.gate.status != Status::Pending {
                return false;
            }
            e.view.approval_id = Some(approval_id.to_string());
            e.view.clone()
        };
        self.publish_permission(&view);
        true
    }

    /// The owner answered through approval `approval_id` (on device `device_id`, taken
    /// from a grant the caller verified; `None` for a denial, which has no grant). This is an ordinary owner answer: the gate
    /// decides with `Origin::OwnerDevice`, so a request that was answered or expired
    /// meanwhile is refused as `AlreadyAnswered` / `Expired`, and the request must be
    /// the one this very approval was filed for.
    pub async fn answer_from_approval(&self, agent_id: Option<&str>, id: &str, approval_id: &str, verdict: Verdict, device_id: Option<&str>) -> Result<(), ManagerError> {
        let filed = self.book().get(&book_key(agent_id, id)).map(|e| e.view.approval_id.clone());
        match filed {
            None => return Err(ManagerError::PermissionNotFound(format!("Permission request not found: {id}"))),
            Some(a) if a.as_deref() != Some(approval_id) => {
                return Err(ManagerError::Forbidden(format!("Approval {approval_id} was not filed for permission request {id}")));
            }
            Some(_) => {}
        }
        let message = (verdict == Verdict::Deny).then(|| "The owner denied this on their device.".to_string());
        match agent_id {
            Some(agent) => {
                let response = match verdict {
                    Verdict::Allow => PermissionResponse::Allow { selected_action_id: None, updated_input: None, updated_permissions: None },
                    Verdict::Deny => PermissionResponse::Deny { selected_action_id: None, message, interrupt: None },
                };
                self.respond_permission_as(agent, id, response, Origin::OwnerDevice, device_id).await
            }
            None => self.answer_external_as(id, verdict, message, Origin::OwnerDevice, device_id).map(|_| ()),
        }
    }

    /// A push for an agent that needs attention (Paseo: push unless a client is
    /// present; secretary-server decides that).
    pub(super) fn push_attention(&self, agent: &Agent, reason: AttentionReason, body: &str) {
        let Some(n) = &self.inner.options.notifier else { return };
        if reason == AttentionReason::Error || agent.record.internal {
            return;
        }
        // A heartbeat's turn, or a subagent whose parent hears about it, stays quiet;
        // permission requests always reach the owner (only the owner answers).
        if reason == AttentionReason::Finished && (self.take_quiet_turn(&agent.record.id) || self.parent_hears(&agent.record.id)) {
            return;
        }
        // So does any run he did not start on one of his devices, whichever provider and whoever
        // did (Lucina, a session through `lagent`, the MCP, an agent): it is not a message to him.
        if reason == AttentionReason::Finished && !agent.record.labels.contains_key(super::STARTED_ON_DEVICE_LABEL) {
            return;
        }
        let id = &agent.record.id;
        let mut data = json!({"kind": "agent", "agentId": id, "reason": reason, "agentTitle": agent.record.title});
        if let Some(s) = agent.record.labels.get(super::SCHEDULE_LABEL) {
            data["scheduleId"] = json!(s);
        }
        if let Some(p) = agent.record.labels.get(super::PARENT_LABEL) {
            data["parentAgentId"] = json!(p);
        }
        let dedup = match (reason, agent.pending.last()) {
            (AttentionReason::Permission, Some(p)) => {
                data["requestId"] = json!(p.id);
                data["actions"] = json!([{"id": "allow", "behavior": "allow"}, {"id": "deny", "behavior": "deny"}]);
                format!("perm:{}", agent_key(id, &p.id))
            }
            _ => format!("agent:{id}:{}", agent.record.attention_timestamp.clone().unwrap_or_else(now_iso)),
        };
        n.send(Push { category: "agent", title: None, body: body.to_string(), route: format!("/agents/{id}"), dedup_key: dedup, data });
    }
}
