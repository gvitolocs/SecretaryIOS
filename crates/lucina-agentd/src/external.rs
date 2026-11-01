//! External agents (provider `secretary-code`): agents another host runs, mirrored
//! here (wire types and rules: [`lucina_agents::external`]).
//!
//! agentd keeps their record, timeline, state and permission requests like any
//! agent's, but runs no process. Their owner device posts the session's events, which
//! drive the lifecycle exactly as a provider's would. Everything the daemon would ask a
//! provider (a prompt, a steer, a stop, a permission answer) becomes a command queued
//! in SQLite for the owner device, sent on the stream as `agent_command`, and kept
//! until the device acknowledges it or it expires after 24 h.
//!
//! Claude sessions (provider `claude-session`, `manager/sessions.rs`) are external agents too:
//! a hook reports what they do and picks up their messages. Only a message can go to one.

use std::sync::Arc;

use async_trait::async_trait;
use lucina_agents::external::{AgentCommand, CommandBody, COMMAND_TTL_MS, PROVIDER, SESSION_PROVIDER};
use lucina_agents::types::{Capabilities, PersistenceHandle, RuntimeInfo};
use lucina_agents::PermissionResponse;

use crate::journal::{now_ms, Journal};
use crate::manager::BusEvent;
use crate::provider::{EventSink, Provider, ProviderError, ProviderInfo, Session, SessionConfig};
use crate::store::{CommandState, Store, StoreError};

fn iso_at(ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms as i64)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

/// What acknowledging a command did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    Acked,
    /// Acknowledged before: nothing changed.
    Already,
    NotFound,
    /// Another device's command.
    NotOwner,
    Expired,
}

/// The queue of commands for owner devices.
pub struct Commands {
    store: Arc<Store>,
    bus: Journal,
    ttl_ms: u64,
}

impl Commands {
    /// Commands live [`COMMAND_TTL_MS`]; `LUCINA_AGENTD_COMMAND_TTL_SECS` shortens it (tests).
    pub fn new(store: Arc<Store>, bus: Journal) -> Self {
        let ttl_ms = std::env::var("LUCINA_AGENTD_COMMAND_TTL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(|s| s.saturating_mul(1000).clamp(1000, COMMAND_TTL_MS))
            .unwrap_or(COMMAND_TTL_MS);
        Commands { store, bus, ttl_ms }
    }

    /// How often the sweeper looks for expired commands.
    pub fn sweep_every(&self) -> std::time::Duration {
        std::time::Duration::from_millis((self.ttl_ms / 4).clamp(250, 60_000))
    }

    pub fn enqueue(&self, agent_id: &str, device_id: &str, body: CommandBody) -> Result<AgentCommand, StoreError> {
        let now = now_ms();
        let expires = now + self.ttl_ms;
        let command = AgentCommand {
            command_id: format!("cmd_{}", uuid::Uuid::new_v4().simple()),
            agent_id: agent_id.to_string(),
            command: body,
            created_at: iso_at(now),
            expires_at: iso_at(expires),
        };
        self.store.insert_command(&command, device_id, expires)?;
        self.bus.send(BusEvent::AgentCommand {
            agent_id: command.agent_id.clone(),
            command_id: command.command_id.clone(),
            command: command.command.clone(),
            created_at: command.created_at.clone(),
            expires_at: command.expires_at.clone(),
            device_id: device_id.to_string(),
        });
        Ok(command)
    }

    /// The device's commands still waiting for its acknowledgement.
    pub fn pending(&self, device_id: &str) -> Result<Vec<AgentCommand>, StoreError> {
        self.store.pending_commands(device_id, now_ms())
    }

    pub fn ack(&self, agent_id: &str, command_id: &str, device_id: &str) -> Result<Ack, StoreError> {
        let Some(c) = self.store.command(command_id)? else { return Ok(Ack::NotFound) };
        if c.command.agent_id != agent_id {
            return Ok(Ack::NotFound);
        }
        if c.device_id != device_id {
            return Ok(Ack::NotOwner);
        }
        match c.state {
            CommandState::Acked => Ok(Ack::Already),
            CommandState::Expired => Ok(Ack::Expired),
            CommandState::Pending if c.expires_at_ms <= now_ms() => Ok(Ack::Expired),
            CommandState::Pending => {
                self.store.set_command_state(command_id, CommandState::Acked)?;
                Ok(Ack::Acked)
            }
        }
    }

    /// Mark overdue commands expired; returns them.
    pub fn expire_due(&self) -> Result<Vec<AgentCommand>, StoreError> {
        self.store.expire_commands(now_ms())
    }
}

/// The `secretary-code` and `claude-session` providers. The manager registers them itself.
pub struct ExternalProvider {
    commands: Arc<Commands>,
    id: &'static str,
    label: &'static str,
}

impl ExternalProvider {
    pub fn new(commands: Arc<Commands>) -> Self {
        ExternalProvider { commands, id: PROVIDER, label: "Secretary Code" }
    }

    pub fn claude_sessions(commands: Arc<Commands>) -> Self {
        ExternalProvider { commands, id: SESSION_PROVIDER, label: "Claude session" }
    }

    fn session(&self, config: &SessionConfig) -> Result<Arc<dyn Session>, ProviderError> {
        let (Some(agent_id), Some(owner)) = (config.agent_id.clone(), config.owner_device_id.clone()) else {
            return Err(ProviderError::new("an external agent needs its id and owner device"));
        };
        Ok(Arc::new(ExternalSession { agent_id, owner, commands: self.commands.clone(), provider: self.id }))
    }
}

#[async_trait]
impl Provider for ExternalProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.into(),
            label: self.label.into(),
            default_mode_id: None,
            modes: Vec::new(),
            models: Vec::new(),
            capabilities: Capabilities { supports_streaming: true, supports_tool_invocations: true, ..Capabilities::default() },
            // The owner device posts the user's messages with the rest of the timeline.
            echoes_user_message: true,
        }
    }

    async fn create_session(&self, config: SessionConfig, _events: EventSink) -> Result<Arc<dyn Session>, ProviderError> {
        self.session(&config)
    }

    async fn resume_session(&self, _handle: &PersistenceHandle, config: SessionConfig, _events: EventSink) -> Result<Arc<dyn Session>, ProviderError> {
        self.session(&config)
    }
}

/// A session whose every call becomes a command for the owner device.
struct ExternalSession {
    agent_id: String,
    owner: String,
    commands: Arc<Commands>,
    provider: &'static str,
}

impl ExternalSession {
    fn queue(&self, body: CommandBody) -> Result<AgentCommand, ProviderError> {
        self.commands.enqueue(&self.agent_id, &self.owner, body).map_err(|e| ProviderError::new(e.to_string()))
    }
}

#[async_trait]
impl Session for ExternalSession {
    fn session_id(&self) -> String {
        self.agent_id.clone()
    }

    async fn start_turn(&self, prompt: &str, _client_message_id: Option<String>) -> Result<String, ProviderError> {
        Ok(self.queue(CommandBody::Send { prompt: prompt.to_string() })?.command_id)
    }

    async fn steer(&self, prompt: &str, _expected_turn_id: &str) -> Result<bool, ProviderError> {
        self.queue(CommandBody::Steer { prompt: prompt.to_string() })?;
        Ok(true)
    }

    async fn interrupt(&self) -> Result<(), ProviderError> {
        self.queue(CommandBody::Stop {})?;
        Ok(())
    }

    async fn respond_permission(&self, request_id: &str, response: PermissionResponse) -> Result<(), ProviderError> {
        self.queue(CommandBody::PermissionResponse { request_id: request_id.to_string(), response })?;
        Ok(())
    }

    async fn close(&self) {}

    fn runtime_info(&self) -> RuntimeInfo {
        RuntimeInfo { provider: self.provider.into(), session_id: Some(self.agent_id.clone()), model: None, thinking_option_id: None, mode_id: None, extra: None }
    }

    fn persistence(&self) -> PersistenceHandle {
        PersistenceHandle { provider: self.provider.into(), session_id: self.agent_id.clone(), native_handle: None, metadata: None }
    }

    fn current_mode(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_ack_and_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let bus = Journal::with(1, 100, 60_000);
        let q = Commands::new(store, bus.clone());
        let mut sub = bus.subscribe(None);
        let c = q.enqueue("agent-1", "dev_mac", CommandBody::Stop {}).unwrap();
        let live = sub.live.try_recv().unwrap();
        assert!(live.data.starts_with("{\"type\":\"agent_command\""), "{}", live.data);
        assert!(!live.data.contains("dev_mac"), "the owner device id stays internal");
        assert_eq!(q.pending("dev_mac").unwrap(), vec![c.clone()]);
        assert!(q.pending("dev_phone").unwrap().is_empty());
        assert_eq!(q.ack("agent-1", &c.command_id, "dev_phone").unwrap(), Ack::NotOwner);
        assert_eq!(q.ack("agent-2", &c.command_id, "dev_mac").unwrap(), Ack::NotFound);
        assert_eq!(q.ack("agent-1", "cmd_nope", "dev_mac").unwrap(), Ack::NotFound);
        assert_eq!(q.ack("agent-1", &c.command_id, "dev_mac").unwrap(), Ack::Acked);
        assert_eq!(q.ack("agent-1", &c.command_id, "dev_mac").unwrap(), Ack::Already);
        assert!(q.pending("dev_mac").unwrap().is_empty());
        assert!(q.expire_due().unwrap().is_empty(), "an acknowledged command never expires");
    }
}
