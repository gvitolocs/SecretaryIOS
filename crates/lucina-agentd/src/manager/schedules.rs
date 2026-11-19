//! The scheduler (Paseo `server/schedule/service.js`): schedules in the `schedules`
//! table, a tick every second, and the two kinds of run.
//!
//! - **Heartbeat** (`target.type: agent`): the prompt, wrapped as a system notice
//!   (`<lucina-system>`), is sent to the agent with `steer`; the run fails when the
//!   agent already runs, waits on a permission or errors. A gone or archived agent
//!   completes the schedule. A heartbeat's own turn doesn't push "finished".
//! - **New agent**: one agent per run, labelled `lucina.schedule-id` and
//!   `lucina.schedule-run`, in the configured cwd (`isolation: local`) or a fresh
//!   worktree (`worktree`). When the run ends, `archiveOnFinish` (default true)
//!   archives the worktree workspace, or for a local run only its agent (Lucina:
//!   archiving the shared local workspace would close the owner's other agents
//!   there). A run that ends waiting on a permission keeps its agent, so the owner can
//!   still answer (Paseo archives it).
//!
//! Ownership: the owner (lagent, the owner's devices) changes every schedule; an MCP
//! token or an agent only the schedules it created (`createdBy`), and they get the
//! same supervision as their own runs (no unsupervised provider, no dangerous mode).
//! An agent may give itself heartbeats; new-agent schedules count as delegation.
//!
//! A cron cadence given as `cron` without a time zone runs in the owner's zone
//! (`LUCINA_AGENTD_TIMEZONE`, else `LUCINA_TIMEZONE`, else Europe/Rome, as Lucina's
//! reminders); Paseo's `cadence` object without a zone means UTC, as in Paseo.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use lucina_agents::envelope;
use lucina_agents::schedule::{Cadence, Isolation, NewAgentConfig, RunStatus, Schedule, ScheduleStatus, Target};
use lucina_agents::worktree_guard::Trigger;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use serde_json::Value;

use super::orch::{Caller, DEPTH_LABEL, MAX_DEPTH, ORIGIN_LABEL};
use super::{ActiveTurnBehavior, CreateRequest, Manager, ManagerError};
use crate::schedules::{self as calc, iso, RunEnd};
use crate::workspaces::CreateWorkspace;

pub const SCHEDULE_LABEL: &str = "lucina.schedule-id";
pub const RUN_LABEL: &str = "lucina.schedule-run";
pub const TICK: Duration = Duration::from_secs(1);

/// The owner's time zone for `cron` without one.
pub fn owner_timezone() -> String {
    ["LUCINA_AGENTD_TIMEZONE", "LUCINA_TIMEZONE"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
        .unwrap_or_else(|| "Europe/Rome".into())
}

/// `POST /schedules`: Paseo's shape (`cadence`, `target`) or the short one (`cron`,
/// `provider`, `cwd`… for a new agent, `agentId` for a heartbeat).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleInput {
    pub prompt: String,
    #[serde(default)]
    pub name: Option<String>,
    /// Short form: the name (secretary-mcp's `title`).
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cadence: Option<Cadence>,
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub every_ms: Option<u64>,
    #[serde(default)]
    pub target: Option<Target>,
    #[serde(default)]
    pub agent_id: Option<String>,
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
    pub isolation: Option<Isolation>,
    #[serde(default)]
    pub archive_on_finish: Option<bool>,
    #[serde(default)]
    pub max_runs: Option<u32>,
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Relative expiry (`lagent --expires-in`).
    #[serde(default)]
    pub expires_in_ms: Option<u64>,
    #[serde(default)]
    pub run_on_create: Option<bool>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

fn invalid(m: impl Into<String>) -> ManagerError {
    ManagerError::Invalid(m.into())
}

fn trim_name(v: Option<&str>) -> Option<String> {
    v.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

impl Manager {
    // -- storage ------------------------------------------------------------------

    fn sched_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.inner.orch.schedule_lock.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn put_schedule(&self, s: &Schedule) -> Result<(), ManagerError> {
        let json = serde_json::to_string(s).map_err(|e| invalid(e.to_string()))?;
        self.inner
            .store
            .with_db(|db| {
                db.execute(
                    "INSERT INTO schedules (id, created_at, record) VALUES (?1, ?2, ?3) ON CONFLICT(id) DO UPDATE SET record = excluded.record",
                    params![s.id, s.created_at, json],
                )
            })
            .map_err(|e| ManagerError::Store(e.into()))?;
        Ok(())
    }

    fn get_schedule(&self, id: &str) -> Option<Schedule> {
        let json: Option<String> = self
            .inner
            .store
            .with_db(|db| db.query_row("SELECT record FROM schedules WHERE id = ?1", params![id], |r| r.get(0)).optional())
            .ok()
            .flatten();
        json.and_then(|j| serde_json::from_str(&j).ok())
    }

    pub fn schedules(&self) -> Vec<Schedule> {
        self.inner
            .store
            .with_db(|db| -> rusqlite::Result<Vec<String>> {
                let mut stmt = db.prepare("SELECT record FROM schedules ORDER BY created_at")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
                Ok(rows.flatten().collect())
            })
            .unwrap_or_default()
            .iter()
            .filter_map(|j| serde_json::from_str(j).ok())
            .collect()
    }

    pub fn schedule(&self, id: &str) -> Result<Schedule, ManagerError> {
        self.get_schedule(id).ok_or_else(|| ManagerError::ScheduleNotFound(format!("Schedule not found: {id}")))
    }

    /// Read, change and write one schedule under the schedule lock.
    fn update_schedule<T>(&self, id: &str, f: impl FnOnce(&mut Schedule) -> Result<T, ManagerError>) -> Result<(Schedule, T), ManagerError> {
        let _g = self.sched_lock();
        let mut s = self.schedule(id)?;
        let out = f(&mut s)?;
        self.put_schedule(&s)?;
        Ok((s, out))
    }

    // -- ownership ------------------------------------------------------------------

    /// The owner changes every schedule; anyone else only its own.
    pub fn may_change_schedule(&self, caller: &Caller, s: &Schedule) -> Result<(), ManagerError> {
        match caller {
            Caller::Owner(_) => Ok(()),
            Caller::Unknown => Err(ManagerError::ScheduleForbidden("This caller can only read schedules".into())),
            c if s.created_by == c.origin() => Ok(()),
            _ => Err(ManagerError::ScheduleForbidden(format!(
                "Schedule {} was not created by this caller, so it can't change it. The owner can, in the Secretary app.",
                s.id
            ))),
        }
    }

    fn check_own(&self, caller: &Caller, id: &str) -> Result<(), ManagerError> {
        let s = self.schedule(id)?;
        self.may_change_schedule(caller, &s)
    }

    // -- create and change ------------------------------------------------------------

    fn cadence_of(input: &ScheduleInput) -> Result<Cadence, ManagerError> {
        let cadence = match (&input.cadence, &input.cron, input.every_ms) {
            (Some(c), None, None) => c.clone(),
            (None, Some(expr), None) => Cadence::Cron {
                expression: expr.trim().to_string(),
                timezone: Some(input.timezone.clone().filter(|t| !t.trim().is_empty()).unwrap_or_else(owner_timezone)),
            },
            (None, None, Some(ms)) => Cadence::Every { every_ms: ms },
            (None, None, None) => return Err(invalid("A schedule needs `cron`, `everyMs` or `cadence`")),
            _ => return Err(invalid("Give only one of `cron`, `everyMs` and `cadence`")),
        };
        calc::validate(&cadence).map_err(ManagerError::Invalid)?;
        Ok(cadence)
    }

    fn target_of(&self, caller: &Caller, input: &ScheduleInput) -> Result<Target, ManagerError> {
        let target = match (&input.target, &input.agent_id, &input.provider) {
            (Some(t), None, None) => t.clone(),
            (None, Some(a), None) => Target::Agent { agent_id: a.clone() },
            (None, None, Some(p)) => {
                let (cwd, workspace_id) = match (&input.cwd, &input.workspace_id) {
                    (_, Some(w)) => {
                        let ws = self.inner.workspaces.get(w).filter(|w| w.archived_at.is_none()).ok_or_else(|| ManagerError::Workspace(crate::workspaces::WsError::NotFound(format!("Workspace not found: {w}"))))?;
                        (ws.cwd, Some(w.clone()))
                    }
                    (Some(c), None) => (c.clone(), None),
                    (None, None) => return Err(invalid("A new-agent schedule needs `cwd` or `workspaceId`")),
                };
                let (provider, model) = super::split_provider(p, input.model.clone());
                Target::NewAgent {
                    config: NewAgentConfig {
                        provider,
                        model,
                        mode_id: input.mode_id.clone(),
                        thinking_option_id: input.thinking_option_id.clone(),
                        cwd,
                        workspace_id,
                        isolation: input.isolation,
                        archive_on_finish: input.archive_on_finish,
                        title: None,
                    },
                }
            }
            (None, None, None) => return Err(invalid("A schedule needs a `target`, an `agentId` (heartbeat) or a `provider` (new agent)")),
            _ => return Err(invalid("Give only one of `target`, `agentId` and `provider`")),
        };
        match (&target, caller) {
            (Target::Agent { agent_id }, Caller::Agent(me)) if agent_id != me => {
                Err(ManagerError::ScheduleForbidden("An agent can only give itself heartbeats".into()))
            }
            (Target::Agent { .. }, Caller::Mcp(_)) => Err(ManagerError::ScheduleForbidden("Heartbeats are set by the agent itself or the owner".into())),
            (Target::Agent { agent_id }, _) => {
                let agents = self.lock();
                let a = agents.get(agent_id).ok_or_else(|| ManagerError::NotFound(agent_id.clone()))?;
                if a.record.archived_at.is_some() {
                    return Err(invalid(format!("Agent {agent_id} is archived")));
                }
                Ok(target)
            }
            (Target::NewAgent { config }, _) => {
                if config.provider.trim().is_empty() || config.cwd.trim().is_empty() {
                    return Err(invalid("A new-agent schedule needs `provider` and `cwd`"));
                }
                Ok(target)
            }
        }
    }

    /// Supervision and labels for what a non-owner caller schedules.
    fn supervise_schedule(&self, caller: &Caller, target: &mut Target, labels: &mut BTreeMap<String, String>) -> Result<(), ManagerError> {
        if caller.is_owner() {
            return Ok(());
        }
        if let Caller::Agent(me) = caller {
            labels.retain(|k, _| !k.starts_with("lucina.") && !k.starts_with("paseo."));
            if let Target::NewAgent { .. } = target {
                let depth = self.lock().get(me).and_then(|a| a.record.labels.get(DEPTH_LABEL).and_then(|d| d.parse::<u32>().ok())).unwrap_or(0);
                if depth >= MAX_DEPTH {
                    return Err(ManagerError::DelegationDepth("Subagents can't schedule new agents".into()));
                }
                labels.insert(DEPTH_LABEL.into(), (depth + 1).to_string());
                labels.insert(ORIGIN_LABEL.into(), format!("agent:{me}"));
            }
        }
        if let Target::NewAgent { config } = target {
            let spec = match &config.model {
                Some(m) => format!("{}/{m}", config.provider),
                None => config.provider.clone(),
            };
            self.supervise(&spec, &mut config.mode_id)?;
        }
        Ok(())
    }

    /// Paseo `create` / `createOrReplace`: a named schedule from the same creator with
    /// the same target is refreshed in place instead of duplicated.
    pub fn create_schedule(&self, caller: &Caller, input: ScheduleInput) -> Result<Schedule, ManagerError> {
        if matches!(caller, Caller::Unknown) || *caller == Caller::Agent(String::new()) {
            return Err(ManagerError::ScheduleForbidden("This caller can only read schedules".into()));
        }
        let prompt = input.prompt.trim().to_string();
        if prompt.is_empty() {
            return Err(invalid("Schedule prompt is required"));
        }
        if input.max_runs == Some(0) {
            return Err(invalid("maxRuns must be a positive integer"));
        }
        let cadence = Self::cadence_of(&input)?;
        let mut target = self.target_of(caller, &input)?;
        let mut labels = input.labels.clone();
        self.supervise_schedule(caller, &mut target, &mut labels)?;
        let now = Utc::now();
        let expires_at = match (input.expires_at.as_deref(), input.expires_in_ms) {
            (Some(e), _) => Some(iso(calc::parse_iso(e).ok_or_else(|| invalid(format!("Invalid expiresAt: {e}")))?)),
            (None, Some(ms)) => Some(iso(now + chrono::Duration::milliseconds(ms as i64))),
            (None, None) => None,
        };
        let name = trim_name(input.name.as_deref().or(input.title.as_deref()));
        let next = calc::first_run(&cadence, input.run_on_create, now).map_err(ManagerError::Invalid)?;
        let created_by = caller.origin();
        let _g = self.sched_lock();
        if let Some(n) = &name {
            let existing = self.schedules().into_iter().find(|s| {
                s.status != ScheduleStatus::Completed && s.name.as_deref() == Some(n.as_str()) && s.target == target && s.created_by == created_by
            });
            if let Some(mut s) = existing {
                s.cadence = calc::merge_timezone(&s.cadence, cadence);
                s.prompt = prompt;
                s.status = ScheduleStatus::Active;
                s.paused_at = None;
                s.next_run_at = Some(calc::first_run(&s.cadence, input.run_on_create, now).map_err(ManagerError::Invalid)?);
                s.expires_at = expires_at;
                s.max_runs = input.max_runs;
                s.labels.extend(labels);
                s.updated_at = iso(now);
                self.put_schedule(&s)?;
                return Ok(s);
            }
        }
        let s = Schedule {
            id: format!("{:08x}", rand_u32()),
            name,
            prompt,
            cadence,
            target,
            status: ScheduleStatus::Active,
            created_at: iso(now),
            updated_at: iso(now),
            next_run_at: Some(next),
            last_run_at: None,
            paused_at: None,
            expires_at,
            max_runs: input.max_runs,
            runs: Vec::new(),
            labels,
            created_by,
        };
        self.put_schedule(&s)?;
        Ok(s)
    }

    pub fn pause_schedule(&self, caller: &Caller, id: &str) -> Result<Schedule, ManagerError> {
        self.check_own(caller, id)?;
        Ok(self.update_schedule(id, |s| calc::pause(s, Utc::now()).map_err(ManagerError::ScheduleConflict))?.0)
    }

    pub fn resume_schedule(&self, caller: &Caller, id: &str) -> Result<Schedule, ManagerError> {
        self.check_own(caller, id)?;
        Ok(self.update_schedule(id, |s| calc::resume(s, Utc::now()).map_err(ManagerError::ScheduleConflict))?.0)
    }

    pub fn delete_schedule(&self, caller: &Caller, id: &str) -> Result<(), ManagerError> {
        self.check_own(caller, id)?;
        let _g = self.sched_lock();
        self.inner
            .store
            .with_db(|db| db.execute("DELETE FROM schedules WHERE id = ?1", params![id]))
            .map_err(|e| ManagerError::Store(e.into()))?;
        Ok(())
    }

    /// Paseo `update`: `name` (null or "" clears), `prompt`, `cadence` / `cron` /
    /// `timezone` / `everyMs`, `maxRuns` (null clears), `expiresAt` (null clears),
    /// `newAgentConfig` (new-agent schedules only).
    pub fn update_schedule_with(&self, caller: &Caller, id: &str, patch: &Value) -> Result<Schedule, ManagerError> {
        self.check_own(caller, id)?;
        let patch = patch.as_object().ok_or_else(|| invalid("Expected a JSON object"))?;
        let mut target_patch = None;
        if let Some(cfg) = patch.get("newAgentConfig") {
            target_patch = Some(cfg.clone());
        }
        let (s, _) = self.update_schedule(id, |s| {
            let now = Utc::now();
            if let Some(p) = patch.get("prompt") {
                let p = p.as_str().map(str::trim).filter(|p| !p.is_empty()).ok_or_else(|| invalid("Schedule prompt is required"))?;
                s.prompt = p.to_string();
            }
            if let Some(n) = patch.get("name") {
                s.name = trim_name(n.as_str());
            }
            let cadence = if let Some(c) = patch.get("cadence") {
                Some(serde_json::from_value::<Cadence>(c.clone()).map_err(|e| invalid(format!("cadence: {e}")))?)
            } else if let Some(expr) = patch.get("cron").and_then(Value::as_str) {
                let tz = patch.get("timezone").and_then(Value::as_str).map(str::to_string);
                Some(Cadence::Cron { expression: expr.trim().to_string(), timezone: tz })
            } else if let Some(ms) = patch.get("everyMs").and_then(Value::as_u64) {
                Some(Cadence::Every { every_ms: ms })
            } else if let (Some(tz), Cadence::Cron { expression, .. }) = (patch.get("timezone").and_then(Value::as_str), &s.cadence) {
                Some(Cadence::Cron { expression: expression.clone(), timezone: Some(tz.to_string()) })
            } else {
                None
            };
            if let Some(c) = cadence {
                let c = calc::merge_timezone(&s.cadence, c);
                calc::validate(&c).map_err(ManagerError::Invalid)?;
                s.cadence = c;
                s.next_run_at = calc::next_after_update(s, now);
            }
            if let Some(m) = patch.get("maxRuns") {
                s.max_runs = match m {
                    Value::Null => None,
                    v => Some(v.as_u64().filter(|n| *n > 0 && *n <= u32::MAX as u64).ok_or_else(|| invalid("maxRuns must be a positive integer"))? as u32),
                };
            }
            if let Some(e) = patch.get("expiresAt") {
                s.expires_at = match e {
                    Value::Null => None,
                    v => Some(iso(v.as_str().and_then(calc::parse_iso).ok_or_else(|| invalid("Invalid expiresAt"))?)),
                };
            }
            if let Some(cfg) = &target_patch {
                let Target::NewAgent { config } = &mut s.target else {
                    return Err(invalid("new-agent config updates are only valid for new-agent target schedules"));
                };
                apply_config(config, cfg)?;
            }
            s.updated_at = iso(now);
            Ok(())
        })?;
        if target_patch.is_some() && !caller.is_owner() {
            // Re-check supervision on the new config; a refused one is rolled back by
            // pausing (the owner sees why).
            let mut t = s.target.clone();
            let mut labels = BTreeMap::new();
            if let Err(e) = self.supervise_schedule(caller, &mut t, &mut labels) {
                let _ = self.update_schedule(id, |s| calc::pause(s, Utc::now()).map_err(ManagerError::ScheduleConflict));
                return Err(e);
            }
            return Ok(self.update_schedule(id, |s| {
                s.target = t;
                Ok(())
            })?
            .0);
        }
        Ok(s)
    }

    // -- running ----------------------------------------------------------------------

    /// Paseo `runOnce`: one run now, the cadence untouched.
    pub async fn run_schedule_once(&self, caller: &Caller, id: &str) -> Result<Schedule, ManagerError> {
        self.check_own(caller, id)?;
        let s = self.schedule(id)?;
        if s.status == ScheduleStatus::Completed {
            return Err(ManagerError::ScheduleConflict(format!("Schedule {id} is already completed")));
        }
        if !self.reserve_schedule(id) {
            return Err(ManagerError::ScheduleConflict(format!("Schedule {id} is already running")));
        }
        let run_id = uuid::Uuid::new_v4().to_string();
        let started = self.update_schedule(id, |s| {
            calc::start_manual(s, run_id.clone(), Utc::now());
            Ok(())
        });
        match started {
            Ok((s, _)) => {
                let manager = self.clone();
                let id2 = id.to_string();
                tokio::spawn(async move {
                    manager.execute_run(s, run_id, true).await;
                    manager.release_schedule(&id2);
                });
            }
            Err(e) => {
                self.release_schedule(id);
                return Err(e);
            }
        }
        self.schedule(id)
    }

    fn reserve_schedule(&self, id: &str) -> bool {
        self.inner.orch.running_schedules.lock().unwrap_or_else(|p| p.into_inner()).insert(id.to_string())
    }

    fn release_schedule(&self, id: &str) {
        self.inner.orch.running_schedules.lock().unwrap_or_else(|p| p.into_inner()).remove(id);
    }

    /// One tick: complete what is due to complete, start what is due to run.
    pub fn schedule_tick(&self, now: DateTime<Utc>) {
        for s in self.schedules() {
            if s.status != ScheduleStatus::Active || s.next_run_at.is_none() {
                continue;
            }
            if self.inner.orch.running_schedules.lock().unwrap_or_else(|p| p.into_inner()).contains(&s.id) {
                continue;
            }
            // A heartbeat whose agent was archived or deleted ends (Paseo `completeForAgent`).
            if let Target::Agent { agent_id } = &s.target {
                let gone = self.lock().get(agent_id).is_none_or(|a| a.record.archived_at.is_some());
                if gone {
                    let _ = self.update_schedule(&s.id, |s| {
                        if s.status != ScheduleStatus::Completed {
                            calc::complete(s, now);
                        }
                        Ok(())
                    });
                    continue;
                }
            }
            if calc::should_complete(&s, now) {
                let _ = self.update_schedule(&s.id, |s| {
                    if s.status == ScheduleStatus::Active && calc::should_complete(s, now) {
                        calc::complete(s, now);
                    }
                    Ok(())
                });
                continue;
            }
            if s.next_run_at.as_deref().and_then(calc::parse_iso).is_some_and(|n| n > now) {
                continue;
            }
            if !self.reserve_schedule(&s.id) {
                continue;
            }
            let run_id = uuid::Uuid::new_v4().to_string();
            match self.update_schedule(&s.id, |s| Ok(calc::claim_due(s, run_id.clone(), now))) {
                Ok((claimed, Some(_))) => {
                    let manager = self.clone();
                    tokio::spawn(async move {
                        let id = claimed.id.clone();
                        manager.execute_run(claimed, run_id, false).await;
                        manager.release_schedule(&id);
                    });
                }
                _ => self.release_schedule(&s.id),
            }
        }
    }

    /// Paseo `start`: repair interrupted runs, end orphaned heartbeats, then tick.
    pub fn spawn_scheduler(&self) {
        let manager = self.clone();
        tokio::spawn(async move {
            let now = Utc::now();
            for s in manager.schedules() {
                if let Ok((_, (dirty, archive))) = manager.update_schedule(&s.id, |s| Ok(calc::recover(s, now))) {
                    if dirty {
                        tracing::info!(schedule = %s.id, "repaired a schedule after the restart");
                    }
                    if let Some(w) = archive {
                        if let Err(e) = manager.archive_workspace(&w, Trigger::Archive, Some("scheduled run interrupted by a restart".into())).await {
                            tracing::warn!(workspace = %w, error = %e, "could not archive an interrupted scheduled workspace");
                        }
                    }
                }
            }
            let mut closing = manager.closing();
            loop {
                manager.schedule_tick(Utc::now());
                tokio::select! {
                    _ = tokio::time::sleep(TICK) => {}
                    _ = closing.wait_for(|c| *c) => return,
                }
            }
        });
    }

    async fn execute_run(&self, s: Schedule, run_id: String, manual: bool) {
        let result = match &s.target {
            Target::Agent { agent_id } => self.run_heartbeat(&s, &run_id, agent_id).await,
            Target::NewAgent { config } => self.run_new_agent(&s, &run_id, config).await,
        };
        let end = match result {
            Ok((agent_id, output)) => RunEnd { status: RunStatus::Succeeded, agent_id: Some(agent_id), output, error: None, target_gone: false, manual },
            Err(RunError { agent_id, message, gone }) => RunEnd { status: RunStatus::Failed, agent_id, output: None, error: Some(message), target_gone: gone, manual },
        };
        if let Err(e) = self.update_schedule(&s.id, |sched| {
            calc::finish_run(sched, &run_id, end, Utc::now());
            Ok(())
        }) {
            tracing::warn!(schedule = %s.id, error = %e, "could not record the end of a scheduled run");
        }
    }

    async fn run_heartbeat(&self, s: &Schedule, run_id: &str, agent_id: &str) -> Result<(String, Option<String>), RunError> {
        let fail = |m: String| RunError { agent_id: Some(agent_id.to_string()), message: m, gone: false };
        {
            let agents = self.lock();
            match agents.get(agent_id) {
                None => return Err(RunError { agent_id: None, message: format!("Agent {agent_id} no longer exists"), gone: true }),
                Some(a) if a.record.archived_at.is_some() => return Err(RunError { message: format!("Agent {agent_id} is archived"), gone: true, ..fail(String::new()) }),
                Some(a) if matches!(a.k.status, lucina_agents::AgentStatus::Running) || !a.pending.is_empty() => {
                    return Err(fail(format!("Agent {agent_id} already has an active run")))
                }
                _ => {}
            }
        }
        let body = envelope::schedule_fire_body(s.name.as_deref(), &s.id, run_id, &s.prompt);
        let prompt = envelope::format_system_notification(&body);
        self.inner.orch.quiet_turns.lock().unwrap_or_else(|p| p.into_inner()).insert(agent_id.to_string());
        if let Err(e) = self.send(agent_id, &prompt, ActiveTurnBehavior::Steer).await {
            self.inner.orch.quiet_turns.lock().unwrap_or_else(|p| p.into_inner()).remove(agent_id);
            return Err(fail(e.to_string()));
        }
        let done = self.wait(agent_id, None, false).await;
        // The finished push (if any) happened before `wait` returned.
        self.take_quiet_turn(agent_id);
        let done = done.map_err(|e| fail(e.to_string()))?;
        match done.status {
            "permission" => Err(fail(format!("Scheduled agent {agent_id} is waiting for permission"))),
            "error" => Err(fail(done.last_message.or(done.error).unwrap_or_else(|| format!("Scheduled agent {agent_id} failed")))),
            _ => Ok((agent_id.to_string(), done.last_message.filter(|m| !m.trim().is_empty()).map(|m| m.trim().to_string()))),
        }
    }

    async fn run_new_agent(&self, s: &Schedule, run_id: &str, config: &NewAgentConfig) -> Result<(String, Option<String>), RunError> {
        let fail = |agent_id: Option<String>, m: String| RunError { agent_id, message: m, gone: false };
        match std::fs::metadata(&config.cwd) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err(RunError { agent_id: None, message: format!("Working directory {} is not a directory", config.cwd), gone: true }),
            Err(_) => return Err(RunError { agent_id: None, message: format!("Working directory {} no longer exists", config.cwd), gone: true }),
        }
        let isolation = config.isolation.unwrap_or_default();
        let provider = match &config.model {
            Some(m) => format!("{}/{m}", config.provider),
            None => config.provider.clone(),
        };
        let (workspace_id, cwd) = match isolation {
            Isolation::Worktree => {
                let req = CreateWorkspace {
                    isolation: "worktree".into(),
                    path: Some(config.cwd.clone()),
                    prompt: Some(s.prompt.clone()),
                    provider: Some(config.provider.clone()),
                    background: true,
                    ..CreateWorkspace::default()
                };
                let (w, _) = self.create_workspace(req).await.map_err(|e| fail(None, e.to_string()))?;
                (Some(w.workspace_id), w.cwd)
            }
            Isolation::Local => (config.workspace_id.clone(), config.cwd.clone()),
        };
        if isolation == Isolation::Worktree {
            let w = workspace_id.clone();
            let _ = self.update_schedule(&s.id, |sched| {
                if let Some(r) = sched.runs.iter_mut().find(|r| r.id == run_id) {
                    r.workspace_id = w;
                }
                Ok(())
            });
        }
        let mut labels: BTreeMap<String, String> = s.labels.iter().filter(|(k, _)| k.as_str() != lucina_agents::schedule::DROPPED_RUNS_LABEL).map(|(k, v)| (k.clone(), v.clone())).collect();
        labels.insert(SCHEDULE_LABEL.into(), s.id.clone());
        labels.insert(RUN_LABEL.into(), run_id.to_string());
        let req = CreateRequest {
            provider,
            cwd,
            workspace_id: workspace_id.clone(),
            prompt: Some(s.prompt.clone()),
            title: config.title.clone().or_else(|| s.name.clone()),
            labels,
            mode_id: config.mode_id.clone(),
            thinking_option_id: config.thinking_option_id.clone(),
            // A fire counts toward the host cap and its owner's per-caller cap.
            created_by: Some(s.created_by.clone()),
            ..CreateRequest::default()
        };
        let archive = config.archive_on_finish.unwrap_or(true);
        let result = match self.create(req).await {
            Err(e) => {
                if matches!(e, ManagerError::TooManyAgents(_) | ManagerError::ProviderUnavailable(_)) {
                    tracing::warn!(schedule = %s.id, run = run_id, reason = %e, "scheduled run skipped");
                }
                Err(fail(None, e.to_string()))
            }
            Ok(snap) => {
                let id = snap.id.clone();
                let _ = self.update_schedule(&s.id, |sched| {
                    if let Some(r) = sched.runs.iter_mut().find(|r| r.id == run_id) {
                        r.agent_id = Some(id.clone());
                    }
                    Ok(())
                });
                match self.wait(&id, None, false).await {
                    Err(e) => Err(fail(Some(id), e.to_string())),
                    Ok(done) => match done.status {
                        "permission" => {
                            // Kept for the owner to answer (module docs).
                            return Err(fail(Some(id.clone()), format!("Scheduled agent {id} is waiting for permission")));
                        }
                        "error" => Err(fail(Some(id.clone()), done.last_message.or(done.error).unwrap_or_else(|| format!("Scheduled agent {id} failed")))),
                        _ => Ok((id, done.last_message.filter(|m| !m.trim().is_empty()).map(|m| m.trim().to_string()))),
                    },
                }
            }
        };
        if archive {
            let agent = match &result {
                Ok((id, _)) => Some(id.clone()),
                Err(e) => e.agent_id.clone(),
            };
            match (isolation, &workspace_id, agent) {
                (Isolation::Worktree, Some(w), _) => {
                    if let Err(e) = self.archive_workspace(w, Trigger::Archive, Some("scheduled run finished".into())).await {
                        tracing::warn!(workspace = %w, error = %e, "could not archive a scheduled workspace");
                    }
                }
                (Isolation::Local, _, Some(a)) => {
                    if let Err(e) = self.archive(&a).await {
                        tracing::warn!(agent = %a, error = %e, "could not archive a scheduled agent");
                    }
                }
                _ => {}
            }
        }
        result
    }

    /// Whether `id`'s current turn is a heartbeat's (no "finished" push); clears it.
    pub(super) fn take_quiet_turn(&self, id: &str) -> bool {
        self.inner.orch.quiet_turns.lock().unwrap_or_else(|p| p.into_inner()).remove(id)
    }
}

struct RunError {
    agent_id: Option<String>,
    message: String,
    gone: bool,
}

/// Paseo `applyNewAgentConfig`.
fn apply_config(config: &mut NewAgentConfig, patch: &Value) -> Result<(), ManagerError> {
    let str_of = |k: &str| patch.get(k).map(|v| v.as_str().map(str::trim).unwrap_or("").to_string());
    if let Some(p) = str_of("provider") {
        if p.is_empty() {
            return Err(invalid("provider cannot be empty"));
        }
        config.provider = p;
    }
    if let Some(c) = str_of("cwd") {
        if c.is_empty() {
            return Err(invalid("cwd cannot be empty"));
        }
        config.cwd = c;
    }
    let set = |v: Option<String>| v.filter(|s| !s.is_empty());
    if patch.get("model").is_some() {
        config.model = set(str_of("model"));
    }
    if patch.get("modeId").is_some() {
        config.mode_id = set(str_of("modeId"));
    }
    if patch.get("thinkingOptionId").is_some() {
        config.thinking_option_id = set(str_of("thinkingOptionId"));
    }
    if let Some(a) = patch.get("archiveOnFinish").and_then(Value::as_bool) {
        config.archive_on_finish = Some(a);
    }
    if let Some(i) = patch.get("isolation") {
        config.isolation = Some(serde_json::from_value(i.clone()).map_err(|e| invalid(format!("isolation: {e}")))?);
    }
    Ok(())
}

/// Paseo `generateScheduleId` (4 random bytes, hex).
fn rand_u32() -> u32 {
    let b = uuid::Uuid::new_v4();
    let b = b.as_bytes();
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}
