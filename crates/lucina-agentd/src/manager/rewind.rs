//! Rewind and fork (parity row A16; Paseo `AgentManager.rewind`, `agent/rewind/*`,
//! `handleAgentForkContextRequest`).
//!
//! - **Rewind** takes an agent back to just before one of its user messages, in one of
//!   three modes ([`RewindMode`]). The order is Paseo's, minus its habit of cancelling the
//!   run before it knows the rewind is possible: the caller must own the agent, the
//!   provider must have the capability for the mode, the target must be a user message of
//!   the agent, and a files rewind must pass the file rules. Only then is a running turn
//!   cancelled, and the provider asked to revert. A conversation rewind (and `both`) then
//!   takes the stored timeline back to the rows before the target, under a new epoch
//!   (`timeline_replacement` on the stream), and drops the agent from the search index.
//!   While it runs the agent takes no new prompt (`AGENT_BUSY`).
//! - **Fork** starts a new agent whose first prompt carries the source's chat history
//!   ([`lucina_agents::fork_context`], Paseo's attachment, 100,000 characters at most).
//!   Without a prompt the new agent waits idle and the history goes ahead of the first
//!   prompt it gets.
//!
//! Who may do what (`Caller`):
//!
//! | Caller | Fork | Rewind |
//! |---|---|---|
//! | the owner (`lagent`, owner devices) | any agent | any agent, any mode |
//! | an MCP token | agents with its `lucina.mcp-token` label | the same; files only inside the agent's workspace |
//! | an agent | itself and its subagents | its subagents only; files only inside the workspace |
//! | anyone else | nothing | nothing |
//!
//! A files rewind also goes through the worktree guard, as starting the agent there
//! would: not in a main checkout, a stray worktree, or a Codex-restricted repo, unless
//! the owner passes `allowMainCheckout`.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use lucina_agents::external as ext;
use lucina_agents::fork_context::{self, Boundary, ForkContext};
use lucina_agents::rewind::{self as rw, RewindMode};
use lucina_agents::timeline::Cursor;
use lucina_agents::{AgentSnapshot, AgentStatus, TimelineItem};
use serde::{Deserialize, Serialize};

use super::{
    split_provider, BusEvent, Caller, CreateRequest, Manager, ManagerError, MCP_TOKEN_LABEL,
    PARENT_LABEL,
};
use crate::provider::Rewind;

/// How long a rewind waits for the finished-run check to end.
const GATE_WAIT: Duration = Duration::from_secs(30);

/// The label a fork carries: the agent it was made from.
pub const FORKED_LABEL: &str = "lucina.forked-from";

/// `POST /agents/{id}/rewind`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindRequest {
    /// The user message to go back to (its `messageId`, or the `clientMessageId` it was sent with).
    pub message_id: String,
    pub mode: RewindMode,
    /// The owner allows a files rewind in a main checkout or stray worktree (cleared for
    /// other callers).
    #[serde(default)]
    pub allow_main_checkout: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindOutcome {
    pub agent_id: String,
    pub message_id: String,
    pub mode: RewindMode,
    pub ok: bool,
    /// The timeline's epoch now (new after a conversation rewind).
    pub epoch: String,
    /// Timeline rows a conversation rewind removed (the target message and what followed).
    pub removed_rows: usize,
}

/// `POST /agents/{id}/fork`: what the new agent is made from. Everything but the
/// boundary is optional and defaults to the source agent's own.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkRequest {
    /// Fork from this timeline position (a cursor from `/timeline`)…
    #[serde(default)]
    pub boundary_cursor: Option<Cursor>,
    /// …or from the end of this assistant message. Neither: the whole history.
    #[serde(default)]
    pub boundary_message_id: Option<String>,
    /// The new agent's first prompt; without one it waits for its first message.
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    /// `provider` or `provider/model`.
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub mode_id: Option<String>,
    #[serde(default)]
    pub thinking_option_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub allow_main_checkout: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkOutcome {
    pub agent: AgentSnapshot,
    pub forked_from: String,
    /// Timeline entries the history was made from.
    pub item_count: usize,
    pub boundary_cursor: Option<Cursor>,
    pub boundary_message_id: Option<String>,
}

/// Keeps the agent closed to new prompts while a rewind runs.
struct Hold {
    manager: Manager,
    id: String,
}

impl Drop for Hold {
    fn drop(&mut self) {
        if let Some(a) = self.manager.lock().get_mut(&self.id) {
            a.rewinding = false;
        }
        self.manager.bump();
    }
}

impl Manager {
    /// Whether `caller` owns agent `id` (module docs). `include_self`: an agent owns itself.
    fn require_owned(
        &self,
        caller: &Caller,
        id: &str,
        what: &str,
        include_self: bool,
    ) -> Result<(), ManagerError> {
        let agents = self.lock();
        let a = agents
            .get(id)
            .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        let labels = &a.record.labels;
        let owned = match caller {
            Caller::Owner(_) => true,
            Caller::Agent(me) => (include_self && me == id) || labels.get(PARENT_LABEL) == Some(me),
            Caller::Mcp(origin) => origin
                .strip_prefix("mcp:")
                .filter(|t| !t.is_empty() && *t != "unknown")
                .is_some_and(|token| {
                    labels.get(MCP_TOKEN_LABEL).map(String::as_str) == Some(token)
                }),
            Caller::Unknown => false,
        };
        if owned {
            Ok(())
        } else {
            Err(ManagerError::NotYourAgent(format!(
                "Agent {id} is not yours to {what}"
            )))
        }
    }

    /// Paseo's `agent.fork_context.request`: the chat-history attachment of agent `id`
    /// up to the boundary.
    pub fn fork_context(
        &self,
        caller: &Caller,
        id: &str,
        cursor: Option<&Cursor>,
        message_id: Option<&str>,
    ) -> Result<ForkContext, ManagerError> {
        self.require_owned(caller, id, "fork", true)?;
        let agents = self.lock();
        let a = agents
            .get(id)
            .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        fork_context::build(
            &a.rows,
            &a.record.epoch,
            Boundary { cursor, message_id },
            a.record.title.as_deref(),
            Some(&a.record.cwd),
        )
        .map_err(|e| ManagerError::ForkBoundary(e.to_string()))
    }

    /// A new agent seeded with agent `id`'s chat history (module docs).
    pub async fn fork(
        &self,
        caller: &Caller,
        id: &str,
        req: ForkRequest,
    ) -> Result<ForkOutcome, ManagerError> {
        let context = self.fork_context(
            caller,
            id,
            req.boundary_cursor.as_ref(),
            req.boundary_message_id.as_deref(),
        )?;
        let source = self
            .lock()
            .get(id)
            .map(|a| a.record.clone())
            .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        let spec = req
            .provider
            .clone()
            .unwrap_or_else(|| source.provider.clone());
        let (provider_id, embedded_model) = split_provider(&spec, None);
        if provider_id == ext::PROVIDER {
            return Err(ManagerError::Invalid(
                "An external agent's history can't seed an agent of the same kind; pass a provider"
                    .into(),
            ));
        }
        // Settings carry over only to the same provider: its modes and models are its own.
        let same = provider_id == source.provider;
        let inherit =
            |own: Option<String>, theirs: Option<String>| own.or(if same { theirs } else { None });
        let (cwd, workspace_id) = match (&req.cwd, &req.workspace_id) {
            (None, None) => (source.cwd.clone(), source.workspace_id.clone()),
            (cwd, workspace) => (cwd.clone().unwrap_or_default(), workspace.clone()),
        };
        let mut create = CreateRequest {
            provider: provider_id,
            model: req
                .model
                .or(embedded_model)
                .or_else(|| inherit(None, source.config.model.clone())),
            cwd,
            workspace_id,
            prompt: req.prompt.filter(|p| !p.trim().is_empty()),
            title: req.title,
            labels: req.labels,
            // Someone who isn't the owner gets a supervised mode, not the source's if that skips prompts.
            mode_id: req.mode_id.or_else(|| {
                caller
                    .is_owner()
                    .then(|| {
                        inherit(
                            None,
                            source
                                .last_mode_id
                                .clone()
                                .or(source.config.mode_id.clone()),
                        )
                    })
                    .flatten()
            }),
            thinking_option_id: inherit(
                req.thinking_option_id,
                source.config.thinking_option_id.clone(),
            ),
            feature_values: if same {
                source.config.feature_values.clone().unwrap_or_default()
            } else {
                Default::default()
            },
            idempotency_key: req.idempotency_key,
            allow_main_checkout: req.allow_main_checkout && caller.is_owner(),
            seed_context: Some(context.attachment.text.clone()),
            // A fork counts toward the caps like a create, for whoever asked for it.
            created_by: Some(caller.origin()),
            ..CreateRequest::default()
        };
        self.prepare_create(caller, &mut create)?;
        create.labels.insert(FORKED_LABEL.into(), source.id.clone());
        // What an MCP token starts, that token owns (and so can fork and rewind later).
        if let Caller::Mcp(origin) = caller {
            if let Some(token) = origin
                .strip_prefix("mcp:")
                .filter(|t| !t.is_empty() && *t != "unknown")
            {
                create
                    .labels
                    .insert(MCP_TOKEN_LABEL.into(), token.to_string());
            }
        }
        let agent = self.create(create).await?;
        Ok(ForkOutcome {
            agent,
            forked_from: source.id,
            item_count: context.item_count,
            boundary_cursor: context.boundary_cursor,
            boundary_message_id: context.boundary_message_id,
        })
    }

    /// Wait until no finished-run check runs for `id`; `false` when `limit` passed first.
    async fn wait_gate_settled(&self, id: &str, limit: Duration) -> bool {
        let mut rx = self.inner.generation.subscribe();
        let deadline = tokio::time::Instant::now() + limit;
        loop {
            if !self.lock().get(id).is_some_and(|a| a.gate_running) {
                return true;
            }
            if tokio::time::timeout_at(deadline, rx.changed())
                .await
                .is_err()
            {
                return false;
            }
        }
    }

    fn hold(&self, id: &str) -> Result<Hold, ManagerError> {
        let mut agents = self.lock();
        let a = agents
            .get_mut(id)
            .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        if a.rewinding {
            return Err(ManagerError::Busy(
                "The agent is already being rewound".into(),
            ));
        }
        a.rewinding = true;
        Ok(Hold {
            manager: self.clone(),
            id: id.to_string(),
        })
    }

    /// What a files rewind must pass beyond the capability: the directory is there, a
    /// caller who isn't the owner stays inside the agent's workspace, and the worktree guard
    /// lets an agent edit there.
    async fn check_files(
        &self,
        caller: &Caller,
        id: &str,
        owner_override: bool,
    ) -> Result<(), ManagerError> {
        let record = self
            .lock()
            .get(id)
            .map(|a| a.record.clone())
            .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        if !Path::new(&record.cwd).is_dir() {
            return Err(ManagerError::Invalid(format!(
                "The agent's directory {} no longer exists",
                record.cwd
            )));
        }
        if !caller.is_owner() {
            let outside = || {
                ManagerError::RewindForbidden(
                    "Only the owner rewinds files outside an agent's workspace".into(),
                )
            };
            let workspace = record
                .workspace_id
                .as_deref()
                .and_then(|w| self.inner.workspaces.get(w))
                .filter(|w| w.archived_at.is_none())
                .ok_or_else(outside)?;
            let root = std::fs::canonicalize(&workspace.cwd).map_err(|_| outside())?;
            let dir = std::fs::canonicalize(&record.cwd).map_err(|_| outside())?;
            if !dir.starts_with(&root) {
                return Err(outside());
            }
        }
        let mode = record
            .last_mode_id
            .as_deref()
            .or(record.config.mode_id.as_deref());
        self.guard_start(&record.cwd, &record.provider, mode, owner_override)
            .await
    }

    /// Take agent `id` back to just before the user message `req.message_id` (module docs).
    pub async fn rewind(
        &self,
        caller: &Caller,
        id: &str,
        req: RewindRequest,
    ) -> Result<RewindOutcome, ManagerError> {
        let mode = req.mode;
        self.require_owned(caller, id, "rewind", false)?;
        let (provider_id, external, chain, found) = {
            let agents = self.lock();
            let a = agents
                .get(id)
                .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            (
                a.record.provider.clone(),
                a.record.external,
                a.chain_active,
                rw::find_target(&a.rows, &req.message_id),
            )
        };
        if external {
            return Err(ManagerError::RewindUnsupported(
                "External agents run on another host; rewind the session there".into(),
            ));
        }
        // The provider's own flags, not the agent's: a closed agent reports none.
        let info = self
            .inner
            .providers
            .get(&provider_id)
            .map(|p| p.info())
            .ok_or_else(|| ManagerError::UnknownProvider(provider_id.clone()))?;
        if !rw::supports(&info.capabilities, mode) {
            return Err(ManagerError::RewindUnsupported(rw::unsupported_message(
                mode,
            )));
        }
        if found.is_none() {
            return Err(ManagerError::RewindTargetNotFound(format!(
                "Rewind target {} is not a user message of agent {id}",
                req.message_id
            )));
        }
        if chain {
            return Err(ManagerError::Busy(
                "The agent is running a lane chain; wait for it".into(),
            ));
        }
        if mode.changes_files() {
            self.check_files(caller, id, req.allow_main_checkout && caller.is_owner())
                .await?;
        }
        let _hold = self.hold(id)?;
        // The finished-run check reads the files a rewind may change: let it end first.
        if !self.wait_gate_settled(id, GATE_WAIT).await {
            return Err(ManagerError::Busy(
                "The agent's finished-run check is still running; try again".into(),
            ));
        }
        if self
            .lock()
            .get(id)
            .is_some_and(|a| a.k.status == AgentStatus::Running)
        {
            self.stop_turn(id).await?;
            if self
                .lock()
                .get(id)
                .is_some_and(|a| a.k.status == AgentStatus::Running)
            {
                return Err(ManagerError::Busy(
                    "The running turn didn't stop; try again".into(),
                ));
            }
        }
        let session = self.ensure_loaded(id).await?;
        // The stopped turn may have added rows after the target; the target itself stays.
        let (target, provider_message_id) = {
            let agents = self.lock();
            let a = agents
                .get(id)
                .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            let target = rw::find_target(&a.rows, &req.message_id).ok_or_else(|| {
                ManagerError::RewindTargetNotFound(format!(
                    "Rewind target {} is not a user message of agent {id}",
                    req.message_id
                ))
            })?;
            let provider_message_id =
                a.rows
                    .iter()
                    .find(|r| r.seq == target.seq)
                    .and_then(|r| match &r.item {
                        TimelineItem::UserMessage { message_id, .. } => message_id.clone(),
                        _ => None,
                    });
            (
                target,
                provider_message_id.unwrap_or_else(|| req.message_id.clone()),
            )
        };
        tracing::info!(agent = %id, message = %req.message_id, mode = mode.as_str(), "agent.rewind.start");
        session
            .rewind(&Rewind {
                mode,
                message_id: provider_message_id,
                user_turn: target.user_turn,
            })
            .await
            .map_err(|e| ManagerError::Provider(e.0))?;
        let (epoch, removed) = {
            let mut agents = self.lock();
            let agent = agents
                .get_mut(id)
                .ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
            let mut removed = 0;
            if mode.changes_conversation() {
                let mut rows = agent.rows.clone();
                rw::keep_before(&mut rows, &target);
                removed = agent.rows.len() - rows.len();
                self.inner.store.replace_rows(id, &rows)?;
                agent.record.last_user_message_at = rows
                    .iter()
                    .rev()
                    .find(|r| matches!(r.item, TimelineItem::UserMessage { .. }))
                    .map(|r| r.timestamp.clone());
                agent.record.last_activity_at = rows.last().map(|r| r.timestamp.clone());
                agent.rows = rows;
                agent.record.epoch = uuid::Uuid::new_v4().to_string();
                agent.record.gate = None;
            }
            // The files (or the run) it was going to check are not what they were.
            agent.turn_base = None;
            self.persist(agent);
            self.publish_state(agent);
            (agent.record.epoch.clone(), removed)
        };
        if mode.changes_conversation() {
            self.forget_index(id);
            let _ = self.inner.bus.send(BusEvent::TimelineReplacement {
                agent_id: id.to_string(),
                epoch: epoch.clone(),
            });
        }
        tracing::info!(agent = %id, message = %req.message_id, mode = mode.as_str(), removed, "agent.rewind.complete");
        self.bump();
        Ok(RewindOutcome {
            agent_id: id.to_string(),
            message_id: req.message_id,
            mode,
            ok: true,
            epoch,
            removed_rows: removed,
        })
    }
}
