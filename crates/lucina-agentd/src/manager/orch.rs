//! Agents driving agents (Paseo `agent-messages/*`, `finish-subscriptions/*`,
//! `mcp-server.js` create/send for agent callers).
//!
//! - **Who:** a request from a process an agent started is that agent's
//!   ([`Caller::Agent`], see [`crate::peer::calling_agent`]).
//! - **Subagents:** an agent's `create` makes a child: labels
//!   [`PARENT_LABEL`] and [`DEPTH_LABEL`], the parent's workspace by default, and the
//!   same supervision as the MCP (no provider without permission prompts, no mode that
//!   skips them, no `verify`, `env`, `toolPolicy` or chain). Depth is limited as in
//!   lucina-delegate: the owner's agents are depth 0, their children 1, and a child
//!   starts nothing ([`MAX_DEPTH`]). An agent may steer, stop, archive or detach only
//!   its own children.
//! - **Messages:** what an agent sends another is wrapped in a `<lucina-system
//!   kind="agent-message">` envelope and delivered after 2 quiet seconds per
//!   recipient, several in one prompt (Paseo `delivery.js`).
//! - **Notify on finish:** a parent subscribes to its child (by default when an agent
//!   creates or prompts one). When the child finishes, errors, is closed or needs
//!   permission, the parent gets one message with the child's last answer
//!   ([`envelope::finish_body`]). Subscriptions are in the database, so they survive a
//!   restart, and each is delivered once. A child the parent hears about doesn't push
//!   "finished" to the owner's phone; permission pushes always go (only the owner
//!   answers them).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;

use lucina_agents::envelope::{self, FinishReason, Message, Source, PERMISSION_HINT};
use lucina_agents::permission_gate::Origin;
use lucina_agents::{AgentStatus, PermissionRequest};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::{last_assistant_text, ActiveTurnBehavior, CreateRequest, Manager, ManagerError, PARENT_LABEL};
use crate::now_iso;
use crate::store::Store;

/// The child's delegation depth (`0` for the owner's agents).
pub const DEPTH_LABEL: &str = "lucina.delegation-depth";
/// Who started it (`agent:<id>`, `mcp:<name>`), as the MCP labels its runs.
pub const ORIGIN_LABEL: &str = "lucina.origin";
/// The MCP token that started an agent (set by secretary-mcp); that token owns the agent.
pub const MCP_TOKEN_LABEL: &str = "lucina.mcp-token";
/// Deepest delegation (lucina-delegate's `max_depth`, secretary-mcp's `MAX_DEPTH`).
pub const MAX_DEPTH: u32 = 1;
/// Paseo `schedule(2000, flush)`.
pub const QUIET: Duration = Duration::from_secs(2);
/// Providers with no permission prompts at all (as secretary-mcp's `UNSUPERVISED`).
pub const UNSUPERVISED: [&str; 2] = ["zcode", "pi"];
/// The finish notices' `Server:` line.
pub const SERVER_ID: &str = "lucina-agentd";

/// Who is asking, for the rules above and schedule ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// `lagent` on the host or one of the owner's devices: `local` / `device:<id>`.
    Owner(String),
    /// `mcp:<token id>` (or `mcp`).
    Mcp(String),
    /// A process the agent with this id started.
    Agent(String),
    /// Loopback TCP or an unknown origin header: as before phase 6 for agents (the
    /// permission gate already refuses its answers); schedules it may only read.
    Unknown,
}

impl Caller {
    pub fn from_origin(origin: Origin, header: Option<&str>, agent: Option<String>) -> Caller {
        match origin {
            Origin::Local => Caller::Owner("local".into()),
            Origin::OwnerDevice => Caller::Owner(header.unwrap_or("device").to_string()),
            Origin::Mcp => Caller::Mcp(header.filter(|h| h.starts_with("mcp")).unwrap_or("mcp").to_string()),
            // An agent process agentd can't place gets an id no agent has: it may do
            // nothing an agent may do only to its own.
            Origin::Agent => Caller::Agent(agent.unwrap_or_default()),
            Origin::Policy | Origin::Unknown => Caller::Unknown,
        }
    }

    /// The `createdBy` of what this caller makes.
    pub fn origin(&self) -> String {
        match self {
            Caller::Owner(o) | Caller::Mcp(o) => o.clone(),
            Caller::Agent(id) => format!("agent:{id}"),
            Caller::Unknown => "unknown".into(),
        }
    }

    pub fn is_owner(&self) -> bool {
        matches!(self, Caller::Owner(_))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FinishSub {
    id: String,
    caller_agent_id: String,
    child_agent_id: String,
    created_at: String,
    /// The child has run since: an idle child now means it finished.
    seen_active: bool,
    /// Permission requests already reported.
    #[serde(default)]
    notified: Vec<String>,
}

#[derive(Default)]
struct Queue {
    prompts: Vec<String>,
    token: u64,
}

/// Orchestration state kept by the manager.
pub(crate) struct Orch {
    subs: Mutex<Vec<FinishSub>>,
    queues: Mutex<HashMap<String, Queue>>,
    /// Agents whose current turn a heartbeat started (no "finished" push).
    pub(super) quiet_turns: Mutex<HashSet<String>>,
    pub(super) running_schedules: Mutex<HashSet<String>>,
    /// Serializes schedule read-modify-writes (Paseo `serializeScheduleMutation`).
    pub(super) schedule_lock: Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Orch {
    pub(crate) fn open(store: &Store) -> Result<Self, crate::store::StoreError> {
        let subs = store.with_db(|db| -> rusqlite::Result<Vec<FinishSub>> {
            db.execute_batch(
                "CREATE TABLE IF NOT EXISTS finish_subs (id TEXT PRIMARY KEY, record TEXT NOT NULL);
                 CREATE TABLE IF NOT EXISTS schedules (id TEXT PRIMARY KEY, created_at TEXT NOT NULL, record TEXT NOT NULL);",
            )?;
            let mut stmt = db.prepare("SELECT record FROM finish_subs")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.flatten().filter_map(|j| serde_json::from_str(&j).ok()).collect())
        })?;
        super::search::init(store)?;
        Ok(Orch {
            subs: Mutex::new(subs),
            queues: Mutex::new(HashMap::new()),
            quiet_turns: Mutex::new(HashSet::new()),
            running_schedules: Mutex::new(HashSet::new()),
            schedule_lock: Mutex::new(()),
        })
    }
}

fn depth_of(labels: &std::collections::BTreeMap<String, String>) -> u32 {
    labels.get(DEPTH_LABEL).and_then(|d| d.parse().ok()).unwrap_or(0)
}

impl Manager {
    fn orch(&self) -> &Orch {
        &self.inner.orch
    }

    fn save_sub(&self, sub: &FinishSub) {
        let json = serde_json::to_string(sub).unwrap_or_default();
        let r = self.inner.store.with_db(|db| {
            db.execute("INSERT INTO finish_subs (id, record) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET record = excluded.record", params![sub.id, json])
        });
        if let Err(e) = r {
            tracing::error!(error = %e, "failed to save finish subscription");
        }
    }

    fn drop_sub(&self, id: &str) {
        let _ = self.inner.store.with_db(|db| db.execute("DELETE FROM finish_subs WHERE id = ?1", params![id]));
    }

    /// The agent behind a process agentd launched (`None`: not an agent's).
    pub fn calling_agent(&self, pid: Option<i32>) -> Option<String> {
        let provider = crate::peer::provider_of(pid?, &self.provider_pids())?;
        // A terminal acts for the agent it serves (phase 5); one no agent owns acts for none.
        let id = match self.terminals().by_pid(provider) {
            Some(t) => t.agent_id.clone()?,
            None => crate::peer::agent_of(provider)?,
        };
        self.lock().contains_key(&id).then_some(id)
    }

    /// The same rules for every run neither the owner nor a schedule of the owner's
    /// starts: a provider with permission prompts and a mode that keeps them.
    pub(crate) fn supervise(&self, provider: &str, mode: &mut Option<String>) -> Result<(), ManagerError> {
        let (pid, _) = super::split_provider(provider, None);
        if UNSUPERVISED.contains(&pid.as_str()) {
            return Err(ManagerError::Forbidden(format!("{pid} runs without permission prompts, so only the owner may start it")));
        }
        let Some(info) = self.inner.providers.get(&pid).map(|p| p.info()) else {
            return Err(ManagerError::UnknownProvider(pid));
        };
        let safe = |m: &lucina_agents::types::AgentMode| m.color_tier.as_deref() != Some("dangerous");
        match mode.as_deref() {
            Some(asked) => match info.modes.iter().find(|m| m.id == asked) {
                Some(m) if !safe(m) => Err(ManagerError::Forbidden(format!("Mode {asked:?} skips permission prompts; only the owner may use it"))),
                Some(_) => Ok(()),
                None if info.modes.is_empty() => Ok(()),
                None => Err(ManagerError::Invalid(format!("{pid} has no mode {asked:?}"))),
            },
            None if info.modes.is_empty() => Ok(()),
            None => {
                let default = info.default_mode_id.as_deref();
                let pick = info
                    .modes
                    .iter()
                    .filter(|m| safe(m))
                    .find(|m| Some(m.id.as_str()) == default)
                    .or_else(|| info.modes.iter().find(|m| safe(m)))
                    .ok_or_else(|| ManagerError::Forbidden(format!("{pid} has no mode that keeps permission prompts")))?;
                *mode = Some(pick.id.clone());
                Ok(())
            }
        }
    }

    /// Prepare a create from a caller that isn't the owner (module docs). Returns
    /// whether the caller subscribes to the child's finish by default.
    pub fn prepare_create(&self, caller: &Caller, req: &mut CreateRequest) -> Result<bool, ManagerError> {
        if caller.is_owner() || *caller == Caller::Unknown {
            return Ok(false);
        }
        req.verify = None;
        req.auto_fix = 0;
        req.tool_policy = None;
        req.allow_main_checkout = false;
        if req.chain.is_some() {
            return Err(ManagerError::Forbidden("Only the owner runs lane chains".into()));
        }
        let parent = match caller {
            Caller::Agent(id) => {
                let agents = self.lock();
                let p = agents.get(id).ok_or_else(|| ManagerError::NotFound(id.clone()))?;
                let depth = depth_of(&p.record.labels);
                if depth >= MAX_DEPTH {
                    return Err(ManagerError::DelegationDepth(format!(
                        "This agent is a subagent (delegation depth {depth}); subagents can't start further agents. Do the work yourself, or report back to your parent."
                    )));
                }
                Some((id.clone(), depth, p.record.workspace_id.clone(), p.record.cwd.clone()))
            }
            Caller::Mcp(_) => None,
            Caller::Owner(_) | Caller::Unknown => return Ok(false),
        };
        if matches!(caller, Caller::Agent(_)) {
            req.env.clear();
            req.labels.retain(|k, _| !k.starts_with("lucina.") && !k.starts_with("paseo."));
        }
        // Only the owner opts an agent into the Juno shell tool; an agent or an MCP token can't hand it on.
        req.labels.remove(super::JUNO_LABEL);
        let mut mode = req.mode_id.take();
        self.supervise(&req.provider, &mut mode)?;
        req.mode_id = mode;
        if let Some((id, depth, workspace, cwd)) = parent {
            req.labels.insert(PARENT_LABEL.into(), id.clone());
            req.labels.insert(DEPTH_LABEL.into(), (depth + 1).to_string());
            req.labels.insert(ORIGIN_LABEL.into(), format!("agent:{id}"));
            if req.cwd.trim().is_empty() && req.workspace_id.is_none() {
                req.workspace_id = workspace;
                req.cwd = cwd;
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// May `caller` change agent `id`? An agent only its own children.
    pub fn may_touch(&self, caller: &Caller, id: &str) -> Result<(), ManagerError> {
        match caller {
            Caller::Agent(me) => {
                let agents = self.lock();
                let a = agents.get(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
                if a.record.labels.get(PARENT_LABEL) == Some(me) {
                    Ok(())
                } else {
                    Err(ManagerError::NotYourAgent(format!("Agent {id} is not a subagent of this agent")))
                }
            }
            _ => Ok(()),
        }
    }

    /// Paseo `detach`: the subagent keeps running, on its own.
    pub fn detach(&self, id: &str) -> Result<lucina_agents::AgentSnapshot, ManagerError> {
        let snap = {
            let mut agents = self.lock();
            let a = agents.get_mut(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            a.record.labels.remove(PARENT_LABEL);
            self.persist(a);
            self.publish_state(a);
            super::Manager::snapshot_of(a)
        };
        let gone: Vec<String> = {
            let mut subs = lock(&self.orch().subs);
            let ids = subs.iter().filter(|s| s.child_agent_id == id).map(|s| s.id.clone()).collect();
            subs.retain(|s| s.child_agent_id != id);
            ids
        };
        for g in gone {
            self.drop_sub(&g);
        }
        self.bump();
        Ok(snap)
    }

    /// `caller` hears when `child`'s run ends. A newer subscription replaces an older one
    /// for the same pair (Paseo supersession).
    pub fn subscribe_finish(&self, caller: &str, child: &str, running: bool) {
        let sub = FinishSub {
            id: uuid::Uuid::new_v4().to_string(),
            caller_agent_id: caller.to_string(),
            child_agent_id: child.to_string(),
            created_at: now_iso(),
            seen_active: running,
            notified: Vec::new(),
        };
        let old: Vec<String> = {
            let mut subs = lock(&self.orch().subs);
            let old = subs.iter().filter(|s| s.caller_agent_id == caller && s.child_agent_id == child).map(|s| s.id.clone()).collect();
            subs.retain(|s| !(s.caller_agent_id == caller && s.child_agent_id == child));
            subs.push(sub.clone());
            old
        };
        for o in old {
            self.drop_sub(&o);
        }
        self.save_sub(&sub);
        self.bump();
    }

    /// Whether someone hears about `id`'s finish (its own push stays quiet).
    pub(super) fn parent_hears(&self, id: &str) -> bool {
        lock(&self.orch().subs).iter().any(|s| s.child_agent_id == id)
    }

    /// One pass over the subscriptions: deliver what happened (called on every change).
    pub fn check_finishes(&self) {
        let subs = lock(&self.orch().subs).clone();
        for mut sub in subs {
            let outcome = self.finish_outcome(&mut sub);
            match outcome {
                Outcome::Wait { changed } => {
                    if changed {
                        self.save_sub(&sub);
                        let mut all = lock(&self.orch().subs);
                        if let Some(s) = all.iter_mut().find(|s| s.id == sub.id) {
                            *s = sub;
                        }
                    }
                }
                Outcome::Drop => self.remove_sub(&sub.id),
                Outcome::Permission(requests, title) => {
                    for r in &requests {
                        self.notify_parent(&sub, &title, FinishReason::NeedsPermission, None, Some(r));
                        sub.notified.push(r.id.clone());
                    }
                    self.save_sub(&sub);
                    let mut all = lock(&self.orch().subs);
                    if let Some(s) = all.iter_mut().find(|s| s.id == sub.id) {
                        *s = sub;
                    }
                }
                Outcome::Done(reason, title, last) => {
                    // Removed first: whatever happens to the delivery, it is sent once.
                    self.remove_sub(&sub.id);
                    self.notify_parent(&sub, &title, reason, last.as_deref(), None);
                }
            }
        }
    }

    fn remove_sub(&self, id: &str) {
        lock(&self.orch().subs).retain(|s| s.id != id);
        self.drop_sub(id);
    }

    fn finish_outcome(&self, sub: &mut FinishSub) -> Outcome {
        let agents = self.lock();
        let caller_ok = agents.get(&sub.caller_agent_id).is_some_and(|a| a.record.archived_at.is_none());
        if !caller_ok {
            return Outcome::Drop;
        }
        let Some(child) = agents.get(&sub.child_agent_id) else {
            return Outcome::Done(FinishReason::IsUnavailable, sub.child_agent_id.clone(), None);
        };
        if child.record.labels.get(PARENT_LABEL) != Some(&sub.caller_agent_id) {
            return Outcome::Drop;
        }
        let title = child.record.title.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| sub.child_agent_id.clone());
        let fresh: Vec<PermissionRequest> = child.pending.iter().filter(|p| !sub.notified.contains(&p.id)).cloned().collect();
        if !child.pending.is_empty() {
            sub.notified.retain(|n| child.pending.iter().any(|p| &p.id == n));
            if !fresh.is_empty() {
                sub.seen_active = true;
                return Outcome::Permission(fresh, title);
            }
            let changed = !sub.seen_active;
            sub.seen_active = true;
            return Outcome::Wait { changed };
        }
        let busy = matches!(child.k.status, AgentStatus::Running | AgentStatus::Initializing) || child.chain_active || child.gate_pending;
        if busy {
            let changed = !sub.seen_active || !sub.notified.is_empty();
            sub.seen_active = true;
            sub.notified.clear();
            return Outcome::Wait { changed };
        }
        if !sub.seen_active {
            return Outcome::Wait { changed: false };
        }
        let reason = match child.k.status {
            AgentStatus::Closed => FinishReason::WasClosed,
            AgentStatus::Error => FinishReason::Errored,
            _ => FinishReason::Finished,
        };
        Outcome::Done(reason, title, last_assistant_text(&child.rows))
    }

    fn notify_parent(&self, sub: &FinishSub, title: &str, reason: FinishReason, last: Option<&str>, request: Option<&PermissionRequest>) {
        let body = envelope::finish_body(&sub.child_agent_id, Some(SERVER_ID), title, reason, last, request, PERMISSION_HINT);
        let message = Message {
            id: uuid::Uuid::new_v4().to_string(),
            source: Some(Source::AgentNotification {
                agent_id: sub.child_agent_id.clone(),
                server_id: Some(SERVER_ID.into()),
                title: Some(title.to_string()),
                event: reason.event(),
            }),
            text: body,
        };
        self.enqueue_message(&sub.caller_agent_id, envelope::format_message(&message));
    }

    /// An agent's message to another (Paseo `send_agent_prompt` from an agent caller):
    /// queued for 2 quiet seconds. Returns the message id.
    pub fn send_from_agent(&self, from: &str, to: &str, prompt: &str, notify_on_finish: bool) -> Result<String, ManagerError> {
        let title = self.lock().get(from).and_then(|a| a.record.title.clone()).filter(|t| !t.is_empty());
        let id = uuid::Uuid::new_v4().to_string();
        let message = Message {
            id: id.clone(),
            source: Some(Source::AgentMessage { agent_id: from.to_string(), server_id: None, title }),
            text: prompt.to_string(),
        };
        self.enqueue_message(to, envelope::format_message(&message));
        if notify_on_finish {
            self.subscribe_finish(from, to, true);
        }
        Ok(id)
    }

    fn enqueue_message(&self, recipient: &str, text: String) {
        let token = {
            let mut queues = lock(&self.orch().queues);
            let q = queues.entry(recipient.to_string()).or_default();
            q.prompts.push(text);
            q.token += 1;
            q.token
        };
        let manager = self.clone();
        let recipient = recipient.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(QUIET).await;
            let prompts = {
                let mut queues = lock(&manager.orch().queues);
                match queues.get(&recipient) {
                    Some(q) if q.token == token => queues.remove(&recipient).map(|q| q.prompts).unwrap_or_default(),
                    _ => return,
                }
            };
            manager.deliver(&recipient, prompts.join("\n\n")).await;
        });
    }

    async fn deliver(&self, recipient: &str, prompt: String) {
        for attempt in 0..3u32 {
            let gone = self.lock().get(recipient).is_none_or(|a| a.record.archived_at.is_some());
            if gone {
                tracing::info!(recipient, "agent message dropped: the recipient is gone");
                return;
            }
            match self.send(recipient, &prompt, ActiveTurnBehavior::Steer).await {
                Ok(d) => {
                    tracing::debug!(recipient, disposition = ?d, "agent message delivered");
                    return;
                }
                Err(e) => {
                    tracing::warn!(recipient, attempt, error = %e, "agent message delivery failed");
                    tokio::time::sleep(Duration::from_millis(1000 * 2u64.pow(attempt))).await;
                }
            }
        }
    }

    /// May agent `from` message agent `to`? Its children and its parent.
    pub fn may_message(&self, from: &str, to: &str) -> Result<(), ManagerError> {
        let agents = self.lock();
        let target = agents.get(to).ok_or_else(|| ManagerError::NotFound(to.to_string()))?;
        let parent_of_from = agents.get(from).and_then(|a| a.record.labels.get(PARENT_LABEL));
        if target.record.labels.get(PARENT_LABEL).map(String::as_str) == Some(from) || parent_of_from.map(String::as_str) == Some(to) {
            Ok(())
        } else {
            Err(ManagerError::NotYourAgent(format!("Agent {to} is neither a subagent nor the parent of this agent")))
        }
    }

    /// Whether `child` is `parent`'s subagent.
    pub fn is_child_of(&self, child: &str, parent: &str) -> bool {
        self.lock().get(child).is_some_and(|a| a.record.labels.get(PARENT_LABEL).map(String::as_str) == Some(parent))
    }

    /// Watch the subscriptions: on every change, and every 5 s.
    pub fn spawn_finish_watcher(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            let mut rx = manager.inner.generation.subscribe();
            let mut closing = manager.closing();
            loop {
                manager.check_finishes();
                tokio::select! {
                    r = rx.changed() => if r.is_err() { return },
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    _ = closing.wait_for(|c| *c) => return,
                }
            }
        });
    }
}

enum Outcome {
    Wait { changed: bool },
    Drop,
    Permission(Vec<PermissionRequest>, String),
    Done(FinishReason, String, Option<String>),
}
