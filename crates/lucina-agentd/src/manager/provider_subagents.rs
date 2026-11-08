//! Provider subagents: the children a provider runs inside an agent (a Claude `Task`),
//! each with a descriptor and a timeline of its own (Paseo `agent-manager.js`
//! `providerSubagents`, `provider-subagents/store.js`).
//!
//! A provider reports them as [`StreamEvent::ProviderSubagent`] events. They are kept in
//! SQLite apart from the agent's own timeline, shown on the bus as `provider_subagent`
//! events, and read through `/agents/{id}/provider-subagents`. A child's rows count
//! from 1 and use the agent's epoch. A child still running when the daemon restarts,
//! or when its agent is archived, is canceled: its provider process is gone.

use lucina_agents::subagent::{apply_upsert, Subagent, SubagentEvent, SubagentStatus, SubagentUpdate, MAX_ROWS};
use lucina_agents::{TimelineItem, TimelineRow};
use serde::Serialize;

use super::{Agent, BusEvent, Manager, ManagerError};
use crate::now_iso;

/// A child with its rows.
pub(super) struct Sub {
    pub info: Subagent,
    pub rows: Vec<TimelineRow>,
}

/// Which rows of a child's timeline to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Direction {
    /// The rows after `after_seq`, oldest first, up to `limit` (0 = all).
    #[default]
    After,
    /// The last `limit` rows (0 = all).
    Tail,
}

/// A page of a child's timeline (Paseo `agent.provider_subagents.timeline.get.response`,
/// without the projection: the rows are as stored, like the agent's own).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentTimeline {
    pub agent_id: String,
    pub subagent_id: String,
    pub provider: String,
    pub direction: &'static str,
    pub epoch: String,
    pub rows: Vec<TimelineRow>,
    pub next_seq: u64,
    pub window: Window,
    pub has_older: bool,
    pub has_newer: bool,
    pub subagent: Subagent,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    pub min_seq: u64,
    pub max_seq: u64,
    pub next_seq: u64,
}

impl Manager {
    fn publish_subagent(&self, update: SubagentUpdate) {
        let _ = self.inner.bus.send(BusEvent::ProviderSubagent { event: Box::new(update) });
    }

    /// Apply one provider event to an agent's children.
    pub(super) fn apply_subagent(&self, agent: &mut Agent, provider: &str, event: SubagentEvent) {
        let now = now_iso();
        let parent = agent.record.id.clone();
        match &event {
            SubagentEvent::Upsert { id, .. } => {
                let at = agent.subagents.iter().position(|s| s.info.id == *id);
                let Some(info) = apply_upsert(at.map(|i| &agent.subagents[i].info), &parent, provider, &event, &now) else { return };
                if let Err(e) = self.inner.store.save_subagent(&info) {
                    tracing::error!(agent = %parent, error = %e, "failed to save provider subagent");
                }
                match at {
                    Some(i) => agent.subagents[i].info = info.clone(),
                    None => agent.subagents.push(Sub { info: info.clone(), rows: Vec::new() }),
                }
                self.publish_subagent(SubagentUpdate::Upsert { subagent: info });
            }
            SubagentEvent::Timeline { id, item } => {
                let epoch = agent.record.epoch.clone();
                let i = match agent.subagents.iter().position(|s| s.info.id == *id) {
                    Some(i) => i,
                    None => {
                        // A child's first item before its descriptor: make the descriptor.
                        let up = SubagentEvent::status(id, SubagentStatus::Running);
                        self.apply_subagent(agent, provider, up);
                        match agent.subagents.iter().position(|s| s.info.id == *id) {
                            Some(i) => i,
                            None => return,
                        }
                    }
                };
                let sub = &mut agent.subagents[i];
                if sub.rows.len() >= MAX_ROWS {
                    return;
                }
                let row = TimelineRow { seq: sub.rows.last().map(|r| r.seq + 1).unwrap_or(1), timestamp: now, item: item.clone(), turn_id: None };
                if let Err(e) = self.inner.store.append_subagent_row(&parent, id, &row) {
                    tracing::error!(agent = %parent, subagent = %id, error = %e, "failed to append provider subagent row");
                }
                let update = SubagentUpdate::Timeline {
                    parent_agent_id: parent,
                    subagent_id: id.clone(),
                    provider: sub.info.provider.clone(),
                    item: row.item.clone(),
                    timestamp: row.timestamp.clone(),
                    seq: row.seq,
                    epoch,
                };
                sub.rows.push(row);
                self.publish_subagent(update);
            }
            SubagentEvent::Remove { id } => {
                let before = agent.subagents.len();
                agent.subagents.retain(|s| s.info.id != *id);
                if agent.subagents.len() == before {
                    return;
                }
                if let Err(e) = self.inner.store.delete_subagent(&parent, id) {
                    tracing::error!(agent = %parent, subagent = %id, error = %e, "failed to delete provider subagent");
                }
                self.publish_subagent(SubagentUpdate::Remove { parent_agent_id: parent, subagent_id: id.clone() });
            }
        }
    }

    /// Children still running go to `canceled` (Paseo `cancelRunningProviderSubagents`).
    pub(super) fn cancel_running_subagents(&self, agent: &mut Agent) {
        let running: Vec<(String, String)> = agent.subagents.iter().filter(|s| s.info.status == SubagentStatus::Running).map(|s| (s.info.id.clone(), s.info.provider.clone())).collect();
        for (id, provider) in running {
            self.apply_subagent(agent, &provider, SubagentEvent::status(&id, SubagentStatus::Canceled));
        }
    }

    /// An agent is deleted: its children go with it.
    pub(super) fn remove_subagents(&self, agent: &mut Agent) {
        for sub in std::mem::take(&mut agent.subagents) {
            self.publish_subagent(SubagentUpdate::Remove { parent_agent_id: agent.record.id.clone(), subagent_id: sub.info.id });
        }
    }

    /// Bring an agent's stored children back at start: the ones that were running are canceled.
    pub(super) fn load_subagents(&self, agent_id: &str) -> Vec<Sub> {
        let mut out: Vec<Sub> = match self.inner.store.load_subagents(agent_id) {
            Ok(v) => v.into_iter().map(|(info, rows)| Sub { info, rows }).collect(),
            Err(e) => {
                tracing::error!(agent = %agent_id, error = %e, "failed to load provider subagents");
                Vec::new()
            }
        };
        for sub in &mut out {
            if sub.info.status == SubagentStatus::Running {
                sub.info.status = SubagentStatus::Canceled;
                sub.info.updated_at = now_iso();
                if let Err(e) = self.inner.store.save_subagent(&sub.info) {
                    tracing::error!(agent = %agent_id, error = %e, "failed to save provider subagent");
                }
            }
        }
        out
    }

    /// The agent's provider subagents, oldest first.
    pub fn provider_subagents(&self, id: &str) -> Result<Vec<Subagent>, ManagerError> {
        let agents = self.lock();
        let a = agents.get(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        Ok(a.subagents.iter().map(|s| s.info.clone()).collect())
    }

    /// One provider subagent, by its id or a unique prefix of it.
    pub fn provider_subagent(&self, id: &str, sub: &str) -> Result<Subagent, ManagerError> {
        let agents = self.lock();
        let a = agents.get(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        Ok(find(a, sub)?.info.clone())
    }

    /// A page of one provider subagent's timeline.
    pub fn provider_subagent_timeline(&self, id: &str, sub: &str, direction: Direction, after_seq: u64, limit: usize) -> Result<SubagentTimeline, ManagerError> {
        let agents = self.lock();
        let a = agents.get(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        let s = find(a, sub)?;
        let cap = if limit == 0 { usize::MAX } else { limit };
        let rows: Vec<TimelineRow> = match direction {
            Direction::After => s.rows.iter().filter(|r| r.seq > after_seq).take(cap).cloned().collect(),
            Direction::Tail => s.rows.iter().skip(s.rows.len().saturating_sub(cap)).cloned().collect(),
        };
        let min = s.rows.first().map(|r| r.seq).unwrap_or(0);
        let max = s.rows.last().map(|r| r.seq).unwrap_or(0);
        let first = rows.first().map(|r| r.seq);
        let last = rows.last().map(|r| r.seq);
        Ok(SubagentTimeline {
            agent_id: a.record.id.clone(),
            subagent_id: s.info.id.clone(),
            provider: s.info.provider.clone(),
            direction: match direction {
                Direction::After => "after",
                Direction::Tail => "tail",
            },
            epoch: a.record.epoch.clone(),
            next_seq: last.unwrap_or(after_seq),
            window: Window { min_seq: min, max_seq: max, next_seq: max + 1 },
            has_older: first.is_some_and(|f| f > min),
            has_newer: last.is_some_and(|l| l < max),
            rows,
            subagent: s.info.clone(),
        })
    }

    /// The items of one provider subagent's timeline, for the activity log.
    pub fn provider_subagent_items(&self, id: &str, sub: &str) -> Result<Vec<TimelineItem>, ManagerError> {
        let agents = self.lock();
        let a = agents.get(id).ok_or_else(|| ManagerError::NotFound(id.to_string()))?;
        Ok(find(a, sub)?.rows.iter().map(|r| r.item.clone()).collect())
    }
}

fn find<'a>(agent: &'a Agent, sub: &str) -> Result<&'a Sub, ManagerError> {
    if let Some(s) = agent.subagents.iter().find(|s| s.info.id == sub) {
        return Ok(s);
    }
    let mut hits = agent.subagents.iter().filter(|s| !sub.is_empty() && s.info.id.starts_with(sub));
    match (hits.next(), hits.next()) {
        (Some(s), None) => Ok(s),
        (Some(_), Some(_)) => Err(ManagerError::Invalid(format!("Provider subagent id prefix {sub} matches more than one subagent"))),
        _ => Err(ManagerError::SubagentNotFound(sub.to_string())),
    }
}
