//! External agents in the manager: registering them, the events their owner device
//! posts, the commands it acknowledges, and renaming (`PATCH /agents/{id}`). The
//! queue itself is [`crate::external`].

use lucina_agents::external::{self as ext, AgentCommand, CommandBody, Refusal};
use lucina_agents::{AgentSnapshot, AgentStatus, PermissionResponse, StreamEvent};

use super::{CreateRequest, Manager, ManagerError};
use crate::external::Ack;

/// A title is one line of up to 200 characters.
const MAX_TITLE: usize = 200;
const MAX_CWD: usize = 4096;

fn refusal(r: Refusal) -> ManagerError {
    match r {
        Refusal::NotExternal => ManagerError::Invalid("Not an external agent: its events come from its provider".into()),
        Refusal::NotADevice | Refusal::NotTheOwner => {
            ManagerError::NotOwner("Only the device that registered this agent can do this".into())
        }
    }
}

impl Manager {
    /// Validate an external registration (`provider: "secretary-code"`, `external: true`).
    pub(super) fn check_external(req: &mut CreateRequest, provider_id: &str) -> Result<(), ManagerError> {
        if provider_id != ext::PROVIDER || !req.external {
            return Err(ManagerError::Invalid(format!(
                "external agents use provider \"{}\" with external: true",
                ext::PROVIDER
            )));
        }
        match req.owner_device_id.as_deref() {
            Some(d) if ext::valid_device_id(d) => {}
            _ => return Err(ManagerError::NotOwner("Only one of the owner's devices can register an external agent".into())),
        }
        let host = req.host.as_deref().unwrap_or("");
        if !ext::HOSTS.contains(&host) {
            return Err(ManagerError::Invalid(format!("host: must be one of {}", ext::HOSTS.join(", "))));
        }
        if req.prompt.as_deref().is_some_and(|p| !p.trim().is_empty()) {
            return Err(ManagerError::Invalid("An external agent takes no prompt: send one after it is registered".into()));
        }
        if req.chain.is_some() || req.workspace_id.is_some() || req.verify.is_some() || req.auto_fix > 0 {
            return Err(ManagerError::Invalid("chain, workspaceId, verify and autoFix are for agents that run here".into()));
        }
        let cwd = req.cwd.trim();
        if cwd.is_empty() || cwd.len() > MAX_CWD || cwd.chars().any(char::is_control) {
            return Err(ManagerError::Invalid("cwd: the session's folder on its host, one line".into()));
        }
        req.cwd = cwd.to_string();
        req.allow_main_checkout = false;
        req.model = None;
        req.env.clear();
        Ok(())
    }

    /// `Some((owner device, running))` for an external agent.
    pub(super) fn external_of(&self, id: &str) -> Result<Option<(String, bool)>, ManagerError> {
        let agents = self.lock();
        let a = agents.get(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        Ok(a.record.external.then(|| (a.record.owner_device_id.clone().unwrap_or_default(), a.k.status == AgentStatus::Running)))
    }

    pub fn is_external(&self, id: &str) -> bool {
        self.lock().get(id).is_some_and(|a| a.record.external)
    }

    /// `PATCH /agents/{id} {title}`.
    pub fn rename(&self, id: &str, title: &str) -> Result<AgentSnapshot, ManagerError> {
        let title = title.trim();
        if title.is_empty() || title.chars().count() > MAX_TITLE || title.contains(['\n', '\r']) {
            return Err(ManagerError::Invalid(format!("title: one line, 1 to {MAX_TITLE} characters")));
        }
        let snap = {
            let mut agents = self.lock();
            let agent = agents.get_mut(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            agent.record.title = Some(title.to_string());
            self.persist(agent);
            self.publish_state(agent);
            Self::snapshot_of(agent)
        };
        self.bump();
        Ok(snap)
    }

    /// The owner device posts its session's events; they drive the agent as a
    /// provider's would. Returns how many were applied.
    pub async fn post_events(&self, id: &str, device: Option<&str>, events: Vec<StreamEvent>) -> Result<usize, ManagerError> {
        {
            let agents = self.lock();
            let a = agents.get(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            ext::owner_check(a.record.external, a.record.owner_device_id.as_deref(), device).map_err(refusal)?;
        }
        if events.len() > ext::MAX_EVENTS_PER_POST {
            return Err(ManagerError::Invalid(format!("events: at most {} per post", ext::MAX_EVENTS_PER_POST)));
        }
        if events.iter().any(|e| !ext::postable(e)) {
            return Err(ManagerError::Invalid("events: attention_required is the daemon's own".into()));
        }
        if events.is_empty() {
            return Ok(0);
        }
        self.ensure_loaded(id).await?;
        let n = events.len();
        for event in events {
            self.on_event(id, event);
        }
        Ok(n)
    }

    /// The owner device confirms a command. `Ok(false)`: it had already.
    pub fn ack_command(&self, agent_id: &str, command_id: &str, device: Option<&str>) -> Result<bool, ManagerError> {
        let device = device.ok_or_else(|| ManagerError::NotOwner("Only the owner device acknowledges its commands".into()))?;
        match self.inner.commands.ack(agent_id, command_id, device)? {
            Ack::Acked => Ok(true),
            Ack::Already => Ok(false),
            Ack::NotFound => Err(ManagerError::CommandNotFound(format!("Command not found: {command_id}"))),
            Ack::NotOwner => Err(ManagerError::NotOwner("This command is for another device".into())),
            Ack::Expired => Err(ManagerError::CommandExpired(format!("Command {command_id} expired"))),
        }
    }

    /// The commands waiting for this device (`GET /commands`).
    pub fn pending_commands(&self, device: Option<&str>) -> Result<Vec<AgentCommand>, ManagerError> {
        let device = device.ok_or_else(|| ManagerError::NotOwner("Only a device has commands".into()))?;
        Ok(self.inner.commands.pending(device)?)
    }

    /// Expire overdue commands. A permission answer that never reached the owner device
    /// counts as denied: its request leaves the agent's pending list.
    pub fn expire_commands(&self) {
        let expired = match self.inner.commands.expire_due() {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "command expiry failed");
                return;
            }
        };
        for c in expired {
            let CommandBody::PermissionResponse { request_id, .. } = &c.command else { continue };
            let pending = self.lock().get(&c.agent_id).is_some_and(|a| a.pending.iter().any(|p| &p.id == request_id));
            if pending {
                let message = "The owner device did not get this answer in time; it counts as denied";
                self.on_event(
                    &c.agent_id,
                    StreamEvent::PermissionResolved {
                        provider: ext::PROVIDER.into(),
                        request_id: request_id.clone(),
                        resolution: PermissionResponse::auto_deny(message),
                    },
                );
            }
        }
    }

    pub fn spawn_command_sweeper(&self) {
        let manager = self.clone();
        let every = self.inner.commands.sweep_every();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            loop {
                tick.tick().await;
                manager.expire_commands();
            }
        });
    }
}
