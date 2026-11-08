//! Claude sessions as agents: Claude desktop chats on the Mac and Claude CLI runs on nezopt that
//! work in a Lucina workspace. agentd doesn't run them. A hook (`pipeline-guard session-hook`)
//! reports what they do through `lagent session beat`, and each gets an external agent with
//! provider `claude-session`, so it shows in `lagent ls`, `/v1/agents` and the board.
//!
//! - A session is tracked once it has a workspace: it started or adopted a task, its cwd is in a
//!   worktree, it edits files in one, or it names one. A session with none leaves no trace.
//! - Its status comes from its beats. Silence for [`ext::SESSION_TTL_SECS`] ends it (`closed`),
//!   and the next beat brings it back. An ended session is archived after a day.
//! - A message to it (`POST /agents/{id}/send`) waits in the command queue. The hook delivers it
//!   at the next tool call or prompt, or wakes the session when it is idle, then acknowledges it.
//! - Nothing else reaches it: `stop` is refused, and `archive` and `delete` wait until it ended.
//! - A live session's workspaces count as busy for the merge sweep.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use lucina_agents::external::{self as ext, BeatReply, CommandBody, SessionBeat, SessionEvent};
use lucina_agents::timeline::TimelineItem;
use lucina_agents::{AgentStatus, StreamEvent};
use serde_json::{json, Value};

use super::{CreateRequest, Event, Manager, ManagerError};
use crate::external::Ack;
use crate::journal::now_ms;
use crate::pipeline::ClaudeSession;

/// How often silent sessions are looked for.
const SWEEP_EVERY: Duration = Duration::from_secs(60);

fn ttl_secs() -> u64 {
    std::env::var("LUCINA_AGENTD_SESSION_TTL_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(ext::SESSION_TTL_SECS)
}

/// `user@host` and the like: `lagent` could not name the session.
fn checked_id(id: &str) -> Result<(), ManagerError> {
    if ext::valid_session_id(id) {
        Ok(())
    } else {
        Err(ManagerError::Invalid(format!("Not a session id: {id}")))
    }
}

impl Manager {
    pub fn is_claude_session(&self, id: &str) -> bool {
        self.lock().get(id).is_some_and(|a| ext::is_session(&a.record.provider))
    }

    fn session_host_of(&self, id: &str) -> String {
        self.lock().get(id).and_then(|a| a.record.host.clone()).unwrap_or_else(|| "its host".into())
    }

    /// A Claude session runs in its own app; agentd can tell it something, nothing else.
    pub(super) fn refuse_for_session(&self, id: &str, what: &str) -> Result<(), ManagerError> {
        if !self.is_claude_session(id) {
            return Ok(());
        }
        Err(ManagerError::Forbidden(format!(
            "This is a Claude session running on {}, not a run Lucina Agents supervises: it can't be {what} from here. `lagent send` leaves it a message.",
            self.session_host_of(id)
        )))
    }

    /// Archive and delete wait until the session ended, or its next beat would undo them.
    pub(super) fn refuse_for_live_session(&self, id: &str, what: &str) -> Result<(), ManagerError> {
        let live = self.pipeline().claude_sessions().into_iter().any(|s| s.agent_id == id && s.ended_at_ms.is_none());
        if live {
            self.refuse_for_session(id, what)?;
        }
        Ok(())
    }

    pub(super) fn forget_session_of(&self, agent_id: &str) {
        for s in self.pipeline().claude_sessions().into_iter().filter(|s| s.agent_id == agent_id) {
            if let Err(e) = self.pipeline().drop_claude_session(&s.session_id) {
                tracing::warn!(session = %s.session_id, error = %e, "session not forgotten");
            }
        }
    }

    /// The workspaces live sessions work in.
    pub fn live_session_workspaces(&self) -> HashSet<String> {
        self.pipeline().claude_sessions().into_iter().filter(|s| s.ended_at_ms.is_none()).flat_map(|s| s.workspaces).collect()
    }

    /// Workspace → the live session working in it (the one heard from last).
    pub(super) fn live_sessions_by_workspace(&self) -> HashMap<String, String> {
        let mut live: Vec<ClaudeSession> = self.pipeline().claude_sessions().into_iter().filter(|s| s.ended_at_ms.is_none()).collect();
        live.sort_by_key(|s| s.last_seen_ms);
        live.into_iter().flat_map(|s| s.workspaces.into_iter().map(move |w| (w, s.session_id.clone()))).collect()
    }

    /// `GET /sessions`.
    pub fn sessions_view(&self) -> Vec<Value> {
        let mut list = self.pipeline().claude_sessions();
        list.sort_by_key(|s| std::cmp::Reverse(s.last_seen_ms));
        list.into_iter()
            .map(|s| {
                let (title, status) = self.lock().get(&s.agent_id).map(|a| (a.record.title.clone(), Some(a.k.status))).unwrap_or((None, None));
                let branches: Vec<String> = s.workspaces.iter().filter_map(|w| self.workspaces().get(w)).filter_map(|w| w.branch).collect();
                json!({
                    "sessionId": s.session_id, "agentId": s.agent_id, "host": s.host, "title": title, "status": status,
                    "workspaces": s.workspaces, "branches": branches, "lastSeenMs": s.last_seen_ms, "endedAtMs": s.ended_at_ms,
                })
            })
            .collect()
    }

    /// A session took on a workspace (`lagent task start|adopt`).
    pub async fn session_link(&self, session: &str, workspace: &str) {
        if !ext::valid_session_id(session) {
            return;
        }
        let beat = SessionBeat {
            session_id: session.to_string(),
            event: SessionEvent::Link,
            host: None,
            title: None,
            paths: Vec::new(),
            workspaces: vec![workspace.to_string()],
            prompt: None,
            message: None,
            no_commands: true,
            register: false,
        };
        if let Err(e) = self.session_beat(beat).await {
            tracing::warn!(session, workspace, error = %e, "session not linked");
        }
    }

    /// The workspaces of the session's tasks, and the ones this beat points at.
    fn beat_links(&self, beat: &SessionBeat) -> (Vec<String>, Vec<String>) {
        let ws = self.workspaces();
        let live = |id: &String| ws.get(id).is_some_and(|w| w.archived_at.is_none());
        let mut tasks: Vec<String> = self.pipeline().tasks().into_iter().filter(|t| t.session.as_deref() == Some(beat.session_id.as_str())).map(|t| t.workspace_id).collect();
        tasks.retain(live);
        let mut named: Vec<String> = beat.paths.iter().filter_map(|p| ws.find_by_cwd(p)).map(|w| w.workspace_id).collect();
        named.extend(beat.workspaces.iter().cloned());
        named.retain(live);
        (tasks, named)
    }

    /// One hook call: what the session did, and what waits for it.
    pub async fn session_beat(&self, beat: SessionBeat) -> Result<BeatReply, ManagerError> {
        checked_id(&beat.session_id)?;
        if let Some(h) = beat.host.as_deref().filter(|h| !ext::SESSION_HOSTS.contains(h)) {
            return Err(ManagerError::Invalid(format!("host: {h}: must be one of {}", ext::SESSION_HOSTS.join(", "))));
        }
        let (tasks, named) = self.beat_links(&beat);
        let known = self.pipeline().claude_session(&beat.session_id).filter(|s| self.lock().contains_key(&s.agent_id));
        let mut session = match known {
            Some(s) => s,
            None if tasks.is_empty() && named.is_empty() && !beat.register => return Ok(BeatReply::default()),
            None => self.new_session(&beat, tasks.last().or(named.last())).await?,
        };
        // The last one is the one shown: what the session just pointed at, else its newest task.
        let ws = self.workspaces();
        session.workspaces.retain(|id| ws.get(id).is_some_and(|w| w.archived_at.is_none()));
        for id in tasks.iter().filter(|id| !named.contains(id)) {
            if !session.workspaces.contains(id) {
                session.workspaces.push(id.clone());
            }
        }
        for id in &named {
            session.workspaces.retain(|w| w != id);
            session.workspaces.push(id.clone());
        }
        session.last_seen_ms = now_ms();
        if let Some(h) = &beat.host {
            session.host = h.clone();
        }
        let ending = beat.event == SessionEvent::End;
        session.ended_at_ms = if ending { Some(now_ms()) } else { None };
        self.pipeline().save_claude_session(&session).map_err(ManagerError::Store)?;
        self.show_session(&session, &beat);
        for id in &session.workspaces {
            self.claim_task(id, &session.session_id);
        }
        let agent = session.agent_id.clone();
        if ending {
            self.close_session_agent(&agent).await;
        } else {
            self.revive_session_agent(&agent).await?;
            self.apply_beat(&agent, &beat);
        }
        let commands = if beat.no_commands || ending { Vec::new() } else { self.inner.commands.pending(&ext::session_owner(&session.session_id))? };
        Ok(BeatReply { tracked: true, agent_id: Some(agent), workspaces: session.workspaces, commands })
    }

    async fn new_session(&self, beat: &SessionBeat, link: Option<&String>) -> Result<ClaudeSession, ManagerError> {
        let host = beat.host.clone().unwrap_or_else(|| ext::default_session_host(&beat.session_id).to_string());
        let workspace = link.and_then(|id| self.workspaces().get(id));
        let task_title = link.and_then(|id| self.pipeline().task(id)).map(|t| t.title);
        let cwd = workspace.as_ref().map(|w| w.cwd.clone()).or_else(|| beat.paths.first().cloned()).unwrap_or_else(|| "/".to_string());
        let req = CreateRequest {
            provider: ext::SESSION_PROVIDER.to_string(),
            cwd,
            title: beat.title.clone().or(task_title).or_else(|| Some("Claude session".to_string())),
            workspace_id: workspace.map(|w| w.workspace_id),
            external: true,
            claude_session: true,
            host: Some(host.clone()),
            owner_device_id: Some(ext::session_owner(&beat.session_id)),
            labels: [("session".to_string(), beat.session_id.clone())].into_iter().collect(),
            ..CreateRequest::default()
        };
        let snap = self.create(req).await?;
        let session = ClaudeSession {
            session_id: beat.session_id.clone(),
            agent_id: snap.id,
            host,
            workspaces: Vec::new(),
            last_seen_ms: now_ms(),
            ended_at_ms: None,
        };
        self.pipeline().save_claude_session(&session).map_err(ManagerError::Store)?;
        Ok(session)
    }

    /// The agent's record follows the session: title, host, latest workspace and branch.
    fn show_session(&self, session: &ClaudeSession, beat: &SessionBeat) {
        let workspace = session.workspaces.last().and_then(|id| self.workspaces().get(id));
        let task_title = session.workspaces.last().and_then(|id| self.pipeline().task(id)).map(|t| t.title);
        let mut agents = self.lock();
        let Some(agent) = agents.get_mut(&session.agent_id) else { return };
        let r = &mut agent.record;
        let before = (r.title.clone(), r.host.clone(), r.workspace_id.clone(), r.cwd.clone(), r.labels.clone());
        if let Some(t) = beat.title.as_deref().filter(|t| !t.trim().is_empty()) {
            r.title = Some(t.chars().take(200).collect());
        } else if r.title.as_deref() == Some("Claude session") {
            r.title = task_title.or(r.title.take());
        }
        r.host = Some(session.host.clone());
        r.workspace_id = workspace.as_ref().map(|w| w.workspace_id.clone());
        match workspace.as_ref().and_then(|w| w.branch.clone()) {
            Some(branch) => {
                r.labels.insert("branch".to_string(), branch);
            }
            None => {
                r.labels.remove("branch");
            }
        }
        if let Some(w) = &workspace {
            r.cwd = w.cwd.clone();
        }
        if before != (r.title.clone(), r.host.clone(), r.workspace_id.clone(), r.cwd.clone(), r.labels.clone()) {
            self.persist(agent);
            self.publish_state(agent);
        }
    }

    /// A task taken from a bare `ssh nezopt lagent …` names `user@host`: the session is known now.
    fn claim_task(&self, workspace: &str, session: &str) {
        let unnamed = self.pipeline().task(workspace).is_some_and(|t| t.session.as_deref().is_none_or(|s| !ext::valid_session_id(s)));
        if unnamed {
            self.pipeline().update_task(workspace, |t| t.session = Some(session.to_string()));
        }
    }

    async fn revive_session_agent(&self, agent: &str) -> Result<(), ManagerError> {
        let archived = self.lock().get(agent).is_some_and(|a| a.record.archived_at.is_some());
        if archived {
            self.unarchive(agent)?;
        }
        self.ensure_loaded(agent).await?;
        Ok(())
    }

    async fn close_session_agent(&self, agent: &str) {
        let done = {
            let mut agents = self.lock();
            let Some(a) = agents.get_mut(agent) else { return };
            if a.k.status == AgentStatus::Closed || a.record.archived_at.is_some() {
                return;
            }
            let Ok(effects) = self.transition(a, Event::Close, None) else { return };
            a.active_turn = None;
            let session = a.session.take();
            self.persist(a);
            self.publish_state(a);
            (effects, session)
        };
        self.finish_effects(&done.0, done.1).await;
        self.bump();
    }

    /// What the beat says about the session's turn. A beat's text goes on the timeline, so the
    /// agent's page shows the conversation without its tool calls.
    fn apply_beat(&self, agent: &str, beat: &SessionBeat) {
        let provider = || ext::SESSION_PROVIDER.to_string();
        let running = self.lock().get(agent).is_some_and(|a| a.k.status == AgentStatus::Running);
        match beat.event {
            SessionEvent::Prompt => {
                if !running {
                    self.on_event(agent, StreamEvent::TurnStarted { provider: provider(), turn_id: None });
                }
                if let Some(text) = beat.prompt.as_deref().filter(|p| !p.trim().is_empty()) {
                    let item = TimelineItem::UserMessage { text: ext::clip_session_text(text), message_id: None, client_message_id: None };
                    self.on_event(agent, StreamEvent::Timeline { provider: provider(), item, turn_id: None });
                }
            }
            SessionEvent::Tool if !running => {
                self.on_event(agent, StreamEvent::TurnStarted { provider: provider(), turn_id: None });
            }
            SessionEvent::Stop => {
                if let Some(text) = beat.message.as_deref().filter(|m| !m.trim().is_empty()) {
                    let item = TimelineItem::AssistantMessage { text: ext::clip_session_text(text), message_id: None };
                    self.on_event(agent, StreamEvent::Timeline { provider: provider(), item, turn_id: None });
                }
                if running {
                    self.on_event(agent, StreamEvent::TurnCompleted { provider: provider(), turn_id: None, usage: None });
                }
            }
            _ => {}
        }
    }

    /// The hook delivered these commands. A message goes on the timeline, and an idle session it
    /// just woke is working again. Returns how many were new.
    pub fn session_ack(&self, session: &str, ids: &[String]) -> Result<usize, ManagerError> {
        checked_id(session)?;
        let Some(record) = self.pipeline().claude_session(session) else {
            return Err(ManagerError::NotFound(session.to_string()));
        };
        let owner = ext::session_owner(session);
        let pending = self.inner.commands.pending(&owner)?;
        let mut acked = 0;
        for id in ids {
            if self.inner.commands.ack(&record.agent_id, id, &owner)? != Ack::Acked {
                continue;
            }
            acked += 1;
            let Some(CommandBody::Send { prompt }) = pending.iter().find(|c| &c.command_id == id).map(|c| c.command.clone()) else { continue };
            let item = TimelineItem::UserMessage { text: ext::clip_session_text(&prompt), message_id: None, client_message_id: Some(id.clone()) };
            self.on_event(&record.agent_id, StreamEvent::Timeline { provider: ext::SESSION_PROVIDER.into(), item, turn_id: None });
            let idle = self.lock().get(&record.agent_id).is_some_and(|a| a.k.status == AgentStatus::Idle);
            if idle {
                self.on_event(&record.agent_id, StreamEvent::TurnStarted { provider: ext::SESSION_PROVIDER.into(), turn_id: None });
            }
        }
        Ok(acked)
    }

    /// End the sessions that went silent; archive those ended for a day.
    pub async fn sweep_sessions(&self) {
        let now = now_ms();
        let ttl = ttl_secs();
        for mut s in self.pipeline().claude_sessions() {
            match s.ended_at_ms {
                None if ext::session_silent(now, s.last_seen_ms, ttl) => {
                    tracing::info!(session = %s.session_id, "a Claude session went silent: ended");
                    s.ended_at_ms = Some(now);
                    if let Err(e) = self.pipeline().save_claude_session(&s) {
                        tracing::warn!(session = %s.session_id, error = %e, "session not saved");
                    }
                    self.close_session_agent(&s.agent_id).await;
                }
                Some(ended) if ext::session_silent(now, ended, ext::SESSION_ARCHIVE_AFTER_SECS) => {
                    let visible = self.lock().get(&s.agent_id).is_some_and(|a| a.record.archived_at.is_none());
                    if visible {
                        if let Err(e) = self.archive(&s.agent_id).await {
                            tracing::warn!(session = %s.session_id, error = %e, "ended session not archived");
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub fn spawn_session_sweeper(&self) {
        let manager = self.clone();
        let every = SWEEP_EVERY.min(Duration::from_secs(ttl_secs().max(1)));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            loop {
                tick.tick().await;
                manager.sweep_sessions().await;
            }
        });
    }

    /// The workspace is going away: its sessions stay, without it.
    pub(super) fn unlink_sessions(&self, workspace: &str) {
        for mut s in self.pipeline().claude_sessions().into_iter().filter(|s| s.workspaces.iter().any(|w| w == workspace)) {
            s.workspaces.retain(|w| w != workspace);
            if let Err(e) = self.pipeline().save_claude_session(&s) {
                tracing::warn!(session = %s.session_id, error = %e, "session not saved");
            }
            let mut agents = self.lock();
            if let Some(a) = agents.get_mut(&s.agent_id) {
                a.record.workspace_id = s.workspaces.last().cloned();
                a.record.labels.remove("branch");
                self.persist(a);
                self.publish_state(a);
            }
        }
    }
}
