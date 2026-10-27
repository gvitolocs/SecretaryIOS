//! Agents, turns and permissions (Paseo `server/agent/agent-manager.js`).
//!
//! Every state change goes through [`lucina_agents::lifecycle::step`]; the manager
//! carries out the effects it returns. Provider events arrive on one channel per
//! session and are applied in order by a pump task. State lives behind one mutex
//! that is never held across an await; provider calls run outside it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use lucina_agents::bucket::{state_bucket, BucketInput};
use lucina_agents::events::{format_turn_failed, system_error_item};
use lucina_agents::lifecycle::{self, AgentState, Effect, Event, LifecycleError};
use lucina_agents::title::resolve_titles;
use lucina_agents::types::{ActiveTurn, AgentConfig, AgentMode, Capabilities};
use lucina_agents::{
    AgentListItem, AgentRecord, AgentSnapshot, AgentStatus, AttentionReason, PermissionRequest,
    PermissionResponse, StreamEvent, TimelineItem, TimelineRow,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{broadcast, mpsc, watch};

use crate::lane::{LaneConfig, Lane, Outcome};
use crate::now_iso;
use crate::provider::{Provider, ProviderInfo, Session, SessionConfig};
use crate::store::{Store, StoreError};

/// Label that links a child agent to its parent (Paseo `paseo.parent-agent-id`).
pub const PARENT_LABEL: &str = "lucina.parent-agent-id";

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("No agent found matching: {0}")]
    NotFound(String),
    #[error("Agent id prefix {0} matches more than one agent")]
    Ambiguous(String),
    #[error("Unknown agent provider: {0}")]
    UnknownProvider(String),
    #[error("{0}")]
    Lifecycle(#[from] LifecycleError),
    #[error("{0}")]
    Provider(String),
    #[error("{0}")]
    Invalid(String),
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

impl ManagerError {
    pub fn code(&self) -> &'static str {
        match self {
            ManagerError::NotFound(_) => "AGENT_NOT_FOUND",
            ManagerError::Ambiguous(_) => "AGENT_AMBIGUOUS",
            ManagerError::UnknownProvider(_) => "UNKNOWN_PROVIDER",
            ManagerError::Lifecycle(_) => "INVALID_STATE",
            ManagerError::Provider(_) => "PROVIDER_ERROR",
            ManagerError::Invalid(_) => "INVALID_REQUEST",
            ManagerError::Store(_) => "STORE_ERROR",
        }
    }
}

/// One message on the event bus (Paseo `agent_stream` / `agent_state`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum BusEvent {
    AgentStream {
        agent_id: String,
        event: Box<StreamEvent>,
        #[serde(skip_serializing_if = "Option::is_none")]
        seq: Option<u64>,
        epoch: String,
        timestamp: String,
    },
    AgentState {
        agent: Box<AgentSnapshot>,
    },
    AgentDeleted {
        agent_id: String,
    },
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRequest {
    /// `provider` or `provider/model`.
    pub provider: String,
    #[serde(default)]
    pub model: Option<String>,
    pub cwd: String,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub mode_id: Option<String>,
    #[serde(default)]
    pub thinking_option_id: Option<String>,
    #[serde(default)]
    pub feature_values: Map<String, Value>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    /// Run through a lane category (`deep`, `routine`, `bulk`) with quota
    /// fall-through instead of one fixed provider.
    #[serde(default)]
    pub chain: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ActiveTurnBehavior {
    Steer,
    #[default]
    Interrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    TurnStarted,
    Steered,
    OutOfBand,
}

/// Paseo `waitForFinish`'s result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitResult {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_message: Option<String>,
    #[serde(rename = "final")]
    pub final_snapshot: AgentSnapshot,
}

struct Agent {
    record: AgentRecord,
    k: AgentState,
    session: Option<Arc<dyn Session>>,
    pending: Vec<PermissionRequest>,
    active_turn: Option<ActiveTurn>,
    rows: Vec<TimelineRow>,
    capabilities: Capabilities,
    modes: Vec<AgentMode>,
    echoes_user_message: bool,
    /// A lane chain is still deciding (between attempts the agent may briefly be
    /// in error): `wait` keeps waiting.
    chain_active: bool,
}

impl Agent {
    fn next_seq(&self) -> u64 {
        self.rows.last().map(|r| r.seq + 1).unwrap_or(1)
    }
}

struct Inner {
    store: Store,
    providers: HashMap<String, Arc<dyn Provider>>,
    lanes: LaneConfig,
    agents: Mutex<HashMap<String, Agent>>,
    bus: broadcast::Sender<BusEvent>,
    generation: watch::Sender<u64>,
}

#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
}

fn split_provider(spec: &str, model: Option<String>) -> (String, Option<String>) {
    match spec.split_once('/') {
        Some((p, m)) if !m.is_empty() => (p.to_string(), model.or_else(|| Some(m.to_string()))),
        _ => (spec.to_string(), model),
    }
}

fn session_config(record: &AgentRecord, env: Vec<(String, String)>) -> SessionConfig {
    SessionConfig {
        cwd: record.cwd.clone(),
        model: record.config.model.clone(),
        mode_id: record.config.mode_id.clone(),
        thinking_option_id: record.config.thinking_option_id.clone(),
        feature_values: record.config.feature_values.clone().unwrap_or_default(),
        env,
    }
}

impl Manager {
    /// Open the store and bring every stored agent back as `closed` (Paseo's
    /// loader; lifecycle `RestartRepair`).
    pub fn open(store: Store, providers: Vec<Arc<dyn Provider>>, lanes: LaneConfig) -> Result<Self, ManagerError> {
        let providers: HashMap<String, Arc<dyn Provider>> =
            providers.into_iter().map(|p| (p.info().id, p)).collect();
        let (bus, _) = broadcast::channel(4096);
        let (generation, _) = watch::channel(0);
        let manager = Manager {
            inner: Arc::new(Inner { store, providers, lanes, agents: Mutex::new(HashMap::new()), bus, generation }),
        };
        for mut record in manager.inner.store.load_all()? {
            let k = AgentState {
                status: record.last_status,
                archived: record.archived_at.is_some(),
                deleted: false,
                pending_permissions: 0,
                internal: record.internal,
                requires_attention: record.requires_attention,
                attention_reason: record.attention_reason,
            };
            let t = lifecycle::step(&k, Event::RestartRepair)?;
            let mut rows = manager.inner.store.load_rows(&record.id)?;
            if t.effects.contains(&Effect::RecordInterrupted) {
                let row = TimelineRow {
                    seq: rows.last().map(|r| r.seq + 1).unwrap_or(1),
                    timestamp: now_iso(),
                    item: TimelineItem::Notification {
                        level: lucina_agents::timeline::NotificationLevel::Warning,
                        message: "The agent daemon restarted; this run was interrupted.".into(),
                    },
                    turn_id: None,
                };
                manager.inner.store.append_row(&record.id, &row)?;
                rows.push(row);
            }
            record.last_status = t.next.status;
            manager.inner.store.save(&record)?;
            let info = manager.inner.providers.get(&record.provider).map(|p| p.info());
            let agent = Agent {
                capabilities: Capabilities::stored(),
                modes: info.as_ref().map(|i| i.modes.clone()).unwrap_or_default(),
                echoes_user_message: info.as_ref().is_some_and(|i| i.echoes_user_message),
                chain_active: false,
                record,
                k: t.next,
                session: None,
                pending: Vec::new(),
                active_turn: None,
                rows,
            };
            manager.lock().insert(agent.record.id.clone(), agent);
        }
        Ok(manager)
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Agent>> {
        self.inner.agents.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BusEvent> {
        self.inner.bus.subscribe()
    }

    pub fn providers(&self) -> Vec<ProviderInfo> {
        let mut v: Vec<ProviderInfo> = self.inner.providers.values().map(|p| p.info()).collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    fn bump(&self) {
        self.inner.generation.send_modify(|g| *g += 1);
    }

    /// Resolve an id or a unique id prefix.
    pub fn resolve(&self, id_or_prefix: &str) -> Result<String, ManagerError> {
        let agents = self.lock();
        if agents.contains_key(id_or_prefix) {
            return Ok(id_or_prefix.to_string());
        }
        let matches: Vec<&String> = agents.keys().filter(|k| k.starts_with(id_or_prefix)).collect();
        match matches.len() {
            0 => Err(ManagerError::NotFound(id_or_prefix.to_string())),
            1 => Ok(matches[0].clone()),
            _ => Err(ManagerError::Ambiguous(id_or_prefix.to_string())),
        }
    }

    // -----------------------------------------------------------------------
    // Snapshots

    fn snapshot_of(agent: &Agent) -> AgentSnapshot {
        let r = &agent.record;
        let runtime = agent.session.as_ref().map(|s| s.runtime_info()).or_else(|| r.runtime_info.clone());
        let current_mode = agent.session.as_ref().and_then(|s| s.current_mode()).or_else(|| r.last_mode_id.clone());
        AgentSnapshot {
            id: r.id.clone(),
            provider: r.provider.clone(),
            cwd: r.cwd.clone(),
            workspace_id: r.workspace_id.clone(),
            model: r.config.model.clone(),
            features: None,
            thinking_option_id: r.config.thinking_option_id.clone(),
            effective_thinking_option_id: r
                .config
                .thinking_option_id
                .clone()
                .or_else(|| runtime.as_ref().and_then(|ri| ri.thinking_option_id.clone())),
            created_at: r.created_at.clone(),
            updated_at: r.updated_at.clone(),
            last_user_message_at: r.last_user_message_at.clone(),
            status: agent.k.status,
            active_turn: agent.active_turn.clone(),
            capabilities: agent.capabilities.clone(),
            current_mode_id: current_mode,
            available_modes: agent.modes.clone(),
            pending_permissions: agent.pending.clone(),
            persistence: agent.session.as_ref().map(|s| s.persistence()).or_else(|| r.persistence.clone()),
            runtime_info: runtime,
            last_usage: r.last_usage.clone(),
            last_error: r.last_error.clone(),
            title: r.title.clone(),
            labels: r.labels.clone(),
            requires_attention: Some(agent.k.requires_attention),
            attention_reason: agent.k.attention_reason,
            attention_timestamp: r.attention_timestamp.clone(),
            archived_at: r.archived_at.clone(),
            provider_unavailable: None,
            internal: r.internal.then_some(true),
            gate: r.gate.clone(),
        }
    }

    fn list_item_of(agent: &Agent) -> AgentListItem {
        let r = &agent.record;
        AgentListItem {
            id: r.id.clone(),
            short_id: r.short_id(),
            title: r.title.clone(),
            provider: r.provider.clone(),
            model: r.config.model.clone(),
            thinking_option_id: r.config.thinking_option_id.clone(),
            status: agent.k.status,
            cwd: r.cwd.clone(),
            created_at: r.created_at.clone(),
            updated_at: r.updated_at.clone(),
            last_user_message_at: r.last_user_message_at.clone(),
            archived_at: r.archived_at.clone(),
            requires_attention: Some(agent.k.requires_attention),
            attention_reason: agent.k.attention_reason,
            attention_timestamp: r.attention_timestamp.clone(),
            labels: r.labels.clone(),
            provider_unavailable: None,
            bucket: state_bucket(BucketInput {
                status: agent.k.status,
                pending_permission_count: agent.pending.len(),
                requires_attention: agent.k.requires_attention,
                attention_reason: agent.k.attention_reason,
            }),
        }
    }

    pub fn snapshot(&self, id: &str) -> Result<AgentSnapshot, ManagerError> {
        let agents = self.lock();
        agents.get(id).map(Self::snapshot_of).ok_or_else(|| ManagerError::NotFound(id.to_string()))
    }

    /// All agents as snapshots (archived ones only when asked).
    pub fn snapshots(&self, include_archived: bool) -> Vec<AgentSnapshot> {
        let agents = self.lock();
        let mut v: Vec<AgentSnapshot> = agents
            .values()
            .filter(|a| include_archived || a.record.archived_at.is_none())
            .filter(|a| !a.record.internal)
            .map(Self::snapshot_of)
            .collect();
        v.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        v
    }

    pub fn list(&self, include_archived: bool) -> Vec<AgentListItem> {
        let agents = self.lock();
        let mut v: Vec<AgentListItem> = agents
            .values()
            .filter(|a| include_archived || a.record.archived_at.is_none())
            .filter(|a| !a.record.internal)
            .map(Self::list_item_of)
            .collect();
        v.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        v
    }

    pub fn timeline(&self, id: &str, after_seq: u64, limit: usize) -> Result<(String, Vec<TimelineRow>), ManagerError> {
        let agents = self.lock();
        let a = agents.get(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        let rows: Vec<TimelineRow> = a
            .rows
            .iter()
            .filter(|r| r.seq > after_seq)
            .take(if limit == 0 { usize::MAX } else { limit })
            .cloned()
            .collect();
        Ok((a.record.epoch.clone(), rows))
    }

    // -----------------------------------------------------------------------
    // State changes (all under the lock, no awaits)

    fn publish_state(&self, agent: &Agent) {
        let _ = self.inner.bus.send(BusEvent::AgentState { agent: Box::new(Self::snapshot_of(agent)) });
    }

    fn persist(&self, agent: &mut Agent) {
        agent.record.last_status = agent.k.status;
        agent.record.requires_attention = agent.k.requires_attention;
        agent.record.attention_reason = agent.k.attention_reason;
        agent.record.updated_at = now_iso();
        if let Some(s) = &agent.session {
            agent.record.runtime_info = Some(s.runtime_info());
            agent.record.persistence = Some(s.persistence());
            agent.record.last_mode_id = s.current_mode();
        }
        if let Err(e) = self.inner.store.save(&agent.record) {
            tracing::error!(agent = %agent.record.id, error = %e, "failed to save agent");
        }
    }

    fn append(&self, agent: &mut Agent, item: TimelineItem, turn_id: Option<String>) {
        let row = TimelineRow { seq: agent.next_seq(), timestamp: now_iso(), item, turn_id };
        if let Err(e) = self.inner.store.append_row(&agent.record.id, &row) {
            tracing::error!(agent = %agent.record.id, error = %e, "failed to append timeline row");
        }
        if let TimelineItem::UserMessage { .. } = row.item {
            agent.record.last_user_message_at = Some(row.timestamp.clone());
        }
        agent.record.last_activity_at = Some(row.timestamp.clone());
        let _ = self.inner.bus.send(BusEvent::AgentStream {
            agent_id: agent.record.id.clone(),
            event: Box::new(StreamEvent::Timeline {
                provider: agent.record.provider.clone(),
                item: row.item.clone(),
                turn_id: row.turn_id.clone(),
            }),
            seq: Some(row.seq),
            epoch: agent.record.epoch.clone(),
            timestamp: row.timestamp.clone(),
        });
        agent.rows.push(row);
    }

    fn forward(&self, agent: &Agent, event: StreamEvent) {
        let _ = self.inner.bus.send(BusEvent::AgentStream {
            agent_id: agent.record.id.clone(),
            event: Box::new(event),
            seq: None,
            epoch: agent.record.epoch.clone(),
            timestamp: now_iso(),
        });
    }

    /// Run the kernel and apply the effects that need no provider call. Returns
    /// the effects for the caller to finish (Interrupt, CloseRuntime, ...).
    fn transition(&self, agent: &mut Agent, event: Event, error: Option<&str>) -> Result<Vec<Effect>, LifecycleError> {
        let t = lifecycle::step(&agent.k, event)?;
        agent.k = t.next;
        for effect in &t.effects {
            match effect {
                Effect::ClearLastError => agent.record.last_error = None,
                Effect::SetLastError => agent.record.last_error = error.map(str::to_string),
                Effect::AppendSystemError => {
                    if let Some(item) = system_error_item(error.unwrap_or(""), agent.rows.last().map(|r| &r.item)) {
                        let turn = agent.active_turn.as_ref().map(|t| t.turn_id.clone());
                        self.append(agent, item, turn);
                    }
                }
                Effect::AutoDenyPending(message) => {
                    for request in std::mem::take(&mut agent.pending) {
                        self.forward(
                            agent,
                            StreamEvent::PermissionResolved {
                                provider: agent.record.provider.clone(),
                                request_id: request.id,
                                resolution: PermissionResponse::auto_deny(message),
                            },
                        );
                    }
                }
                Effect::SetAttention(reason) | Effect::NotifyAttention(reason) => {
                    let timestamp = now_iso();
                    if matches!(effect, Effect::SetAttention(_)) {
                        agent.record.attention_timestamp = Some(timestamp.clone());
                    }
                    self.forward(agent, attention_event(agent, *reason, timestamp));
                }
                Effect::RecordInterrupted => {}
                Effect::Interrupt | Effect::CloseRuntime | Effect::CascadeArchiveChildren | Effect::RemoveRecord => {}
            }
        }
        Ok(t.effects)
    }

    /// Apply one provider event (called by the session pump, in order).
    fn on_event(&self, agent_id: &str, event: StreamEvent) {
        let mut agents = self.lock();
        let Some(agent) = agents.get_mut(agent_id) else { return };
        let result = match event {
            StreamEvent::ThreadStarted { .. } => Ok(()),
            StreamEvent::TurnStarted { turn_id, .. } => {
                let r = self.transition(agent, Event::TurnStarted, None).map(|_| ());
                if agent.active_turn.is_none() {
                    agent.active_turn = turn_id.map(|t| ActiveTurn { turn_id: t, started_at: Some(now_iso()) });
                }
                r
            }
            StreamEvent::Timeline { item, turn_id, .. } => {
                self.append(agent, item, turn_id);
                Ok(())
            }
            StreamEvent::UsageUpdated { usage, .. } => {
                agent.record.last_usage = Some(agent.record.last_usage.clone().unwrap_or_default().merged(&usage));
                Ok(())
            }
            StreamEvent::TurnCompleted { usage, .. } => {
                if let Some(usage) = usage {
                    agent.record.last_usage = Some(agent.record.last_usage.clone().unwrap_or_default().merged(&usage));
                }
                let r = self.transition(agent, Event::TurnCompleted, None).map(|_| ());
                agent.active_turn = None;
                r
            }
            StreamEvent::TurnFailed { error, code, diagnostic, .. } => {
                let message = format_turn_failed(&error, code.as_deref(), diagnostic.as_deref());
                let r = self.transition(agent, Event::TurnFailed, Some(&message)).map(|_| ());
                agent.record.last_error = Some(error);
                agent.active_turn = None;
                r
            }
            StreamEvent::TurnCanceled { .. } => {
                let r = self.transition(agent, Event::TurnCanceled, None).map(|_| ());
                agent.active_turn = None;
                r
            }
            StreamEvent::PermissionRequested { request, provider } => {
                let r = self.transition(agent, Event::PermissionRequested, None).map(|_| ());
                if r.is_ok() {
                    agent.pending.push((*request).clone());
                    self.forward(agent, StreamEvent::PermissionRequested { provider, request });
                }
                r
            }
            StreamEvent::PermissionResolved { request_id, resolution, provider } => {
                match agent.pending.iter().position(|p| p.id == request_id) {
                    Some(i) => {
                        agent.pending.remove(i);
                        let r = self.transition(agent, Event::PermissionResolved, None).map(|_| ());
                        self.forward(agent, StreamEvent::PermissionResolved { provider, request_id, resolution });
                        r
                    }
                    None => Ok(()),
                }
            }
            other @ StreamEvent::AttentionRequired { .. } => {
                self.forward(agent, other);
                Ok(())
            }
        };
        if let Err(e) = result {
            tracing::debug!(agent = agent_id, error = %e, "ignored provider event in this state");
        }
        self.persist(agent);
        self.publish_state(agent);
        drop(agents);
        self.bump();
    }

    fn spawn_pump(&self, agent_id: String, mut rx: mpsc::UnboundedReceiver<StreamEvent>) {
        let manager = self.clone();
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                manager.on_event(&agent_id, event);
            }
        });
    }

    // -----------------------------------------------------------------------
    // Operations

    pub async fn create(&self, mut req: CreateRequest) -> Result<AgentSnapshot, ManagerError> {
        if let Some(key) = req.idempotency_key.as_deref() {
            if let Some(id) = self.inner.store.find_by_idempotency_key(key)? {
                return self.snapshot(&id);
            }
        }
        if let Some(category) = req.chain.clone() {
            let lanes: Vec<(String, &'static str, Lane)> = self
                .inner
                .lanes
                .chain(&category)
                .into_iter()
                .filter(|(_, p, _)| self.inner.providers.contains_key(*p))
                .collect();
            let Some((first_name, first_provider, first_lane)) = lanes.first().cloned() else {
                return Err(ManagerError::Invalid(format!("no lane of chain \"{category}\" can run here")));
            };
            req.provider = first_provider.to_string();
            req.model = first_lane.model.clone();
            req.thinking_option_id = first_lane.effort.clone();
            req.labels.insert("lucina.chain".into(), category.clone());
            req.labels.insert("lucina.lane".into(), first_name);
            let prompt = req.prompt.take();
            if req.title.is_none() {
                req.title = resolve_titles(None, prompt.as_deref()).1;
            }
            let snap = Box::pin(self.create(CreateRequest { chain: None, ..req })).await?;
            if let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) {
                if let Some(a) = self.lock().get_mut(&snap.id) {
                    a.chain_active = true;
                }
                let manager = self.clone();
                let id = snap.id.clone();
                tokio::spawn(async move { manager.run_chain(id, prompt, lanes).await });
            }
            return self.snapshot(&snap.id);
        }
        let (provider_id, model) = split_provider(&req.provider, req.model.clone());
        let provider = self
            .inner
            .providers
            .get(&provider_id)
            .cloned()
            .ok_or_else(|| ManagerError::UnknownProvider(provider_id.clone()))?;
        if req.cwd.trim().is_empty() {
            return Err(ManagerError::Invalid("cwd is required".into()));
        }
        let info = provider.info();
        let (_, provisional) = resolve_titles(req.title.as_deref(), req.prompt.as_deref());
        let now = now_iso();
        let record = AgentRecord {
            id: uuid::Uuid::new_v4().to_string(),
            provider: provider_id,
            cwd: req.cwd.clone(),
            workspace_id: None,
            created_at: now.clone(),
            updated_at: now,
            last_activity_at: None,
            last_user_message_at: None,
            title: provisional,
            labels: req.labels.clone(),
            last_status: AgentStatus::Initializing,
            last_mode_id: req.mode_id.clone().or_else(|| info.default_mode_id.clone()),
            config: AgentConfig {
                mode_id: req.mode_id.clone(),
                model,
                thinking_option_id: req.thinking_option_id.clone(),
                feature_values: (!req.feature_values.is_empty()).then(|| req.feature_values.clone()),
                ..AgentConfig::default()
            },
            runtime_info: None,
            persistence: None,
            last_error: None,
            last_usage: None,
            requires_attention: false,
            attention_reason: None,
            attention_timestamp: None,
            internal: false,
            archived_at: None,
            idempotency_key: req.idempotency_key.clone(),
            epoch: uuid::Uuid::new_v4().to_string(),
            gate: None,
        };
        self.inner.store.save(&record)?;
        let id = record.id.clone();
        let env: Vec<(String, String)> = req.env.clone().into_iter().collect();
        let config = session_config(&record, env);
        {
            let mut agents = self.lock();
            let mut agent = Agent {
                record,
                k: AgentState::new(false),
                session: None,
                pending: Vec::new(),
                active_turn: None,
                rows: Vec::new(),
                capabilities: info.capabilities.clone(),
                modes: info.modes.clone(),
                echoes_user_message: info.echoes_user_message,
                chain_active: false,
            };
            self.persist(&mut agent);
            self.publish_state(&agent);
            agents.insert(id.clone(), agent);
        }
        let (tx, rx) = mpsc::unbounded_channel();
        match provider.create_session(config, tx).await {
            Ok(session) => {
                self.spawn_pump(id.clone(), rx);
                let mut agents = self.lock();
                if let Some(agent) = agents.get_mut(&id) {
                    agent.session = Some(session);
                    self.transition(agent, Event::HandshakeDone, None)?;
                    self.persist(agent);
                    self.publish_state(agent);
                }
            }
            Err(e) => {
                let mut agents = self.lock();
                if let Some(agent) = agents.get_mut(&id) {
                    self.transition(agent, Event::LoadFailed, Some(&e.0))?;
                    self.persist(agent);
                    self.publish_state(agent);
                }
                drop(agents);
                self.bump();
                return Err(ManagerError::Provider(e.0));
            }
        }
        self.bump();
        if let Some(prompt) = req.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
            self.start_turn(&id, prompt).await?;
        }
        self.snapshot(&id)
    }

    /// Load a closed agent's session again (Paseo `ensureAgentLoaded`).
    async fn ensure_loaded(&self, id: &str) -> Result<Arc<dyn Session>, ManagerError> {
        let (record, needs_resume) = {
            let mut agents = self.lock();
            let agent = agents.get_mut(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            if let Some(s) = &agent.session {
                return Ok(s.clone());
            }
            self.transition(agent, Event::Resume, None)?;
            self.publish_state(agent);
            (agent.record.clone(), true)
        };
        debug_assert!(needs_resume);
        let provider = self
            .inner
            .providers
            .get(&record.provider)
            .cloned()
            .ok_or_else(|| ManagerError::UnknownProvider(record.provider.clone()))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let config = session_config(&record, Vec::new());
        let result = match &record.persistence {
            Some(handle) => provider.resume_session(handle, config, tx).await,
            None => provider.create_session(config, tx).await,
        };
        let mut agents = self.lock();
        let agent = agents.get_mut(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        match result {
            Ok(session) => {
                agent.session = Some(session.clone());
                self.transition(agent, Event::HandshakeDone, None)?;
                self.persist(agent);
                self.publish_state(agent);
                drop(agents);
                self.spawn_pump(id.to_string(), rx);
                self.bump();
                Ok(session)
            }
            Err(e) => {
                self.transition(agent, Event::LoadFailed, Some(&e.0))?;
                self.persist(agent);
                self.publish_state(agent);
                Err(ManagerError::Provider(e.0))
            }
        }
    }

    /// A foreground turn: running right away (Paseo `streamAgent`), the provider's
    /// own `turn_started` is then a no-op.
    async fn start_turn(&self, id: &str, prompt: &str) -> Result<(), ManagerError> {
        let session = self.ensure_loaded(id).await?;
        {
            let mut agents = self.lock();
            let agent = agents.get_mut(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            self.transition(agent, Event::TurnStarted, None)?;
            if !agent.echoes_user_message {
                self.append(agent, TimelineItem::UserMessage { text: prompt.to_string(), message_id: None, client_message_id: None }, None);
            }
            agent.record.last_user_message_at = Some(now_iso());
            self.persist(agent);
            self.publish_state(agent);
        }
        self.bump();
        match session.start_turn(prompt, None).await {
            Ok(turn_id) => {
                let mut agents = self.lock();
                if let Some(agent) = agents.get_mut(id) {
                    if agent.active_turn.is_none() && agent.k.status == AgentStatus::Running {
                        agent.active_turn = Some(ActiveTurn { turn_id, started_at: Some(now_iso()) });
                    }
                }
                Ok(())
            }
            Err(e) => {
                let message = format_turn_failed(&e.0, None, None);
                let mut agents = self.lock();
                if let Some(agent) = agents.get_mut(id) {
                    let _ = self.transition(agent, Event::TurnFailed, Some(&message));
                    agent.record.last_error = Some(e.0.clone());
                    self.persist(agent);
                    self.publish_state(agent);
                }
                drop(agents);
                self.bump();
                Err(ManagerError::Provider(e.0))
            }
        }
    }

    pub async fn send(&self, id: &str, prompt: &str, behavior: ActiveTurnBehavior) -> Result<Disposition, ManagerError> {
        if prompt.trim().is_empty() {
            return Err(ManagerError::Invalid("A prompt is required".into()));
        }
        let session = self.ensure_loaded(id).await?;
        if let Some(events) = session.out_of_band(prompt) {
            for ev in events {
                self.on_event(id, ev);
            }
            return Ok(Disposition::OutOfBand);
        }
        let (running, turn) = {
            let agents = self.lock();
            let a = agents.get(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            (a.k.status == AgentStatus::Running, a.active_turn.as_ref().map(|t| t.turn_id.clone()))
        };
        if running {
            if behavior == ActiveTurnBehavior::Steer {
                if let Some(turn) = &turn {
                    if session.steer(prompt, turn).await.map_err(|e| ManagerError::Provider(e.0))? {
                        let mut agents = self.lock();
                        if let Some(agent) = agents.get_mut(id) {
                            self.append(agent, TimelineItem::UserMessage { text: prompt.to_string(), message_id: None, client_message_id: None }, Some(turn.clone()));
                            self.persist(agent);
                        }
                        return Ok(Disposition::Steered);
                    }
                }
            }
            session.interrupt().await.map_err(|e| ManagerError::Provider(e.0))?;
            self.wait_not_running(id, Duration::from_secs(10)).await;
        }
        self.start_turn(id, prompt).await?;
        Ok(Disposition::TurnStarted)
    }

    async fn wait_not_running(&self, id: &str, limit: Duration) {
        let mut rx = self.inner.generation.subscribe();
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            let running = self.lock().get(id).is_some_and(|a| a.k.status == AgentStatus::Running);
            if !running {
                return;
            }
            if tokio::time::timeout_at(deadline, rx.changed()).await.is_err() {
                return;
            }
        }
    }

    /// Interrupt a running agent; `false` when it wasn't running.
    pub async fn stop(&self, id: &str) -> Result<bool, ManagerError> {
        let session = {
            let agents = self.lock();
            let a = agents.get(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            if a.k.status != AgentStatus::Running {
                return Ok(false);
            }
            a.session.clone()
        };
        match session {
            Some(s) => s.interrupt().await.map_err(|e| ManagerError::Provider(e.0))?,
            None => {
                let mut agents = self.lock();
                if let Some(agent) = agents.get_mut(id) {
                    self.transition(agent, Event::TurnCanceled, None)?;
                    self.persist(agent);
                    self.publish_state(agent);
                }
            }
        }
        self.wait_not_running(id, Duration::from_secs(10)).await;
        Ok(true)
    }

    async fn finish_effects(&self, effects: &[Effect], session: Option<Arc<dyn Session>>) {
        if let Some(s) = session {
            if effects.contains(&Effect::Interrupt) {
                let _ = s.interrupt().await;
            }
            if effects.contains(&Effect::CloseRuntime) || effects.contains(&Effect::RemoveRecord) {
                s.close().await;
            }
        }
    }

    pub async fn archive(&self, id: &str) -> Result<String, ManagerError> {
        let (effects, session, children, archived_at) = {
            let mut agents = self.lock();
            let agent = agents.get_mut(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            let effects = self.transition(agent, Event::Archive, None)?;
            let archived_at = now_iso();
            agent.record.archived_at = Some(archived_at.clone());
            agent.active_turn = None;
            let session = if effects.contains(&Effect::CloseRuntime) { agent.session.take() } else { None };
            self.persist(agent);
            self.publish_state(agent);
            let children: Vec<String> = agents
                .values()
                .filter(|a| a.record.archived_at.is_none() && a.record.labels.get(PARENT_LABEL).map(String::as_str) == Some(id))
                .map(|a| a.record.id.clone())
                .collect();
            (effects, session, children, archived_at)
        };
        self.finish_effects(&effects, session).await;
        if effects.contains(&Effect::CascadeArchiveChildren) {
            for child in children {
                if let Err(e) = Box::pin(self.archive(&child)).await {
                    tracing::warn!(child = %child, error = %e, "cascade archive failed");
                }
            }
        }
        self.bump();
        Ok(archived_at)
    }

    pub fn unarchive(&self, id: &str) -> Result<AgentSnapshot, ManagerError> {
        let mut agents = self.lock();
        let agent = agents.get_mut(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        self.transition(agent, Event::Unarchive, None)?;
        agent.record.archived_at = None;
        self.persist(agent);
        self.publish_state(agent);
        let snap = Self::snapshot_of(agent);
        drop(agents);
        self.bump();
        Ok(snap)
    }

    pub async fn delete(&self, id: &str) -> Result<(), ManagerError> {
        let (effects, session) = {
            let mut agents = self.lock();
            let agent = agents.get_mut(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            let effects = self.transition(agent, Event::Delete, None)?;
            let session = agent.session.take();
            agents.remove(id);
            (effects, session)
        };
        self.finish_effects(&effects, session).await;
        self.inner.store.delete(id)?;
        let _ = self.inner.bus.send(BusEvent::AgentDeleted { agent_id: id.to_string() });
        self.bump();
        Ok(())
    }

    /// Paseo `waitForFinish`: until the agent stops running, needs permission,
    /// or `timeout` passes (`None` = no limit).
    pub async fn wait(&self, id: &str, timeout: Option<Duration>) -> Result<WaitResult, ManagerError> {
        let mut rx = self.inner.generation.subscribe();
        let deadline = timeout.map(|t| tokio::time::Instant::now() + t);
        loop {
            {
                let agents = self.lock();
                let a = agents.get(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
                let status = if a.chain_active && a.pending.is_empty() {
                    None
                } else if !a.pending.is_empty() {
                    Some("permission")
                } else {
                    match a.k.status {
                        AgentStatus::Error => Some("error"),
                        AgentStatus::Idle | AgentStatus::Closed => Some("idle"),
                        AgentStatus::Running | AgentStatus::Initializing => None,
                    }
                };
                if let Some(status) = status {
                    return Ok(WaitResult {
                        status,
                        error: (status == "error").then(|| a.record.last_error.clone()).flatten(),
                        last_message: last_assistant_text(&a.rows),
                        final_snapshot: Self::snapshot_of(a),
                    });
                }
            }
            let changed = rx.changed();
            match deadline {
                Some(d) => {
                    if tokio::time::timeout_at(d, changed).await.is_err() {
                        let agents = self.lock();
                        let a = agents.get(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
                        return Ok(WaitResult {
                            status: "timeout",
                            error: None,
                            last_message: last_assistant_text(&a.rows),
                            final_snapshot: Self::snapshot_of(a),
                        });
                    }
                }
                None => {
                    if changed.await.is_err() {
                        return Err(ManagerError::Provider("daemon shutting down".into()));
                    }
                }
            }
        }
    }

    /// Note something on an agent's timeline (lane decisions).
    fn note(&self, id: &str, level: lucina_agents::timeline::NotificationLevel, message: String) {
        let mut agents = self.lock();
        if let Some(agent) = agents.get_mut(id) {
            self.append(agent, TimelineItem::Notification { level, message }, None);
            self.persist(agent);
        }
    }

    /// Close the session and continue on another lane's provider (same agent).
    async fn switch_lane(&self, id: &str, lane_name: &str, provider: &str, lane: &Lane) -> Result<(), ManagerError> {
        let info = self.inner.providers.get(provider).map(|p| p.info()).ok_or_else(|| ManagerError::UnknownProvider(provider.into()))?;
        let session = {
            let mut agents = self.lock();
            let agent = agents.get_mut(id).ok_or_else(|| ManagerError::NotFound(id.into()))?;
            if agent.k.status != AgentStatus::Closed {
                self.transition(agent, Event::Close, None)?;
            }
            agent.session.take()
        };
        if let Some(s) = session {
            s.close().await;
        }
        let mut agents = self.lock();
        let agent = agents.get_mut(id).ok_or_else(|| ManagerError::NotFound(id.into()))?;
        agent.record.provider = provider.to_string();
        agent.record.config.model = lane.model.clone();
        agent.record.config.thinking_option_id = lane.effort.clone();
        agent.record.config.mode_id = None;
        agent.record.persistence = None;
        agent.record.runtime_info = None;
        agent.record.labels.insert("lucina.lane".into(), lane_name.to_string());
        agent.capabilities = info.capabilities.clone();
        agent.modes = info.modes.clone();
        agent.echoes_user_message = info.echoes_user_message;
        self.persist(agent);
        self.publish_state(agent);
        Ok(())
    }

    /// The text a failed attempt is classified on: the error and the last system
    /// error item (which carries the provider's code, e.g. `code: quota`).
    fn failure_text(&self, id: &str) -> String {
        let agents = self.lock();
        let Some(a) = agents.get(id) else { return String::new() };
        let last_system = a.rows.iter().rev().find_map(|r| match &r.item {
            TimelineItem::AssistantMessage { text, .. } if text.starts_with(lucina_agents::events::SYSTEM_ERROR_PREFIX) => Some(text.clone()),
            _ => None,
        });
        format!("{}\n{}", a.record.last_error.clone().unwrap_or_default(), last_system.unwrap_or_default())
    }

    async fn worktree_status(cwd: &str) -> Option<String> {
        let out = tokio::process::Command::new("git").args(["-C", cwd, "status", "--porcelain"]).output().await.ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// lane's rules: try each lane in order; fall through only on quota or
    /// unavailable, and never when the failed attempt left changes in the workspace.
    async fn run_chain(&self, id: String, prompt: String, lanes: Vec<(String, &'static str, Lane)>) {
        use lucina_agents::timeline::NotificationLevel::{Info, Warning};
        let cwd = self.lock().get(&id).map(|a| a.record.cwd.clone()).unwrap_or_default();
        let baseline = Self::worktree_status(&cwd).await;
        for (i, (name, provider, lane)) in lanes.iter().enumerate() {
            if i > 0 {
                if let Err(e) = self.switch_lane(&id, name, provider, lane).await {
                    self.note(&id, Warning, format!("lane {name}: cannot switch ({e}); the chain stops"));
                    break;
                }
            }
            let started = self.start_turn(&id, &prompt).await;
            if started.is_ok() {
                let mut rx = self.inner.generation.subscribe();
                loop {
                    let busy = self.lock().get(&id).is_some_and(|a| {
                        a.pending.is_empty() && matches!(a.k.status, AgentStatus::Running | AgentStatus::Initializing)
                    });
                    if !busy || rx.changed().await.is_err() {
                        break;
                    }
                }
            }
            let (status, waiting) = match self.lock().get(&id) {
                Some(a) => (a.k.status, !a.pending.is_empty()),
                None => return,
            };
            if waiting || (started.is_ok() && status != AgentStatus::Error) {
                break;
            }
            let text = match &started {
                Err(e) => format!("{e}\n{}", self.failure_text(&id)),
                Ok(()) => self.failure_text(&id),
            };
            let outcome = if text.contains("code: quota") {
                Outcome::Quota("provider-quota-reply".into())
            } else {
                self.inner.lanes.classify(name, &lane.kind, &text, None)
            };
            let (what, detector) = match &outcome {
                Outcome::Quota(d) => ("quota", d.clone()),
                Outcome::Unavailable(d) => ("unavailable", d.clone()),
                Outcome::Failed => {
                    self.note(&id, Warning, format!("lane {name}: failed; the chain stops"));
                    break;
                }
            };
            if Self::worktree_status(&cwd).await != baseline {
                self.note(&id, Warning, format!("lane {name}: {what} ({detector}) after changing the workspace; the chain stops (dirty_after_quota)"));
                break;
            }
            match lanes.get(i + 1) {
                Some((next, _, _)) => self.note(&id, Info, format!("lane {name}: {what} ({detector}); trying lane {next}")),
                None => self.note(&id, Warning, format!("lane {name}: {what} ({detector}); no lanes left")),
            }
        }
        if let Some(a) = self.lock().get_mut(&id) {
            a.chain_active = false;
        }
        self.bump();
    }

    /// Stop every running turn and close every session, all at once (shutdown).
    pub async fn shutdown(&self) {
        let sessions: Vec<Arc<dyn Session>> = self.lock().values_mut().filter_map(|a| a.session.take()).collect();
        let mut set = tokio::task::JoinSet::new();
        for s in sessions {
            set.spawn(async move { s.close().await });
        }
        while set.join_next().await.is_some() {}
    }
}

fn last_assistant_text(rows: &[TimelineRow]) -> Option<String> {
    let items: Vec<TimelineItem> = rows.iter().map(|r| r.item.clone()).collect();
    lucina_agents::projection::project_rows(&lucina_agents::projection::rows_from_items(&items))
        .into_iter()
        .rev()
        .find_map(|e| match e.item {
            TimelineItem::AssistantMessage { text, .. } => Some(text),
            _ => None,
        })
}

/// Paseo `agent-attention-notification.js` titles.
fn attention_event(agent: &Agent, reason: AttentionReason, timestamp: String) -> StreamEvent {
    let title = match reason {
        AttentionReason::Finished => "Agent finished",
        AttentionReason::Permission => "Agent needs permission",
        AttentionReason::Error => "Agent needs attention",
    };
    StreamEvent::AttentionRequired {
        provider: agent.record.provider.clone(),
        reason,
        timestamp,
        should_notify: reason != AttentionReason::Error,
        notification: Some(lucina_agents::events::AttentionNotification {
            title: title.to_string(),
            body: agent.record.title.clone().unwrap_or_default(),
            data: lucina_agents::events::AttentionNotificationData {
                server_id: "lucina-agentd".into(),
                workspace_id: agent.record.workspace_id.clone(),
                agent_id: agent.record.id.clone(),
                reason,
            },
        }),
    }
}
