//! The presence state machine: raw gateway messages in, files out. No network.
//!
//! Only four dispatch events matter: `GUILD_CREATE` (current presences of
//! a guild), `PRESENCE_UPDATE`, `GUILD_MEMBERS_CHUNK` (the reply to the
//! owner-only member request sent after each `GUILD_CREATE`) and
//! `GUILD_DELETE`. For every presence the user id is read first; anything
//! that is not the owner is dropped right there, unparsed.

use std::borrow::Cow;
use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::value::RawValue;

use crate::presence::{
    iso, map_presence, parse_iso, Observed, PeekUser, PresenceState, RawPresence,
};
use crate::store::{HistoryEvent, PresenceStore};

/// A gateway command the watcher must send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outbound {
    /// `REQUEST_GUILD_MEMBERS` for the owner only, with `presences: true`.
    RequestOwner { guild_id: u64 },
}

#[derive(Deserialize)]
struct Envelope<'a> {
    op: u8,
    #[serde(borrow, default)]
    t: Option<Cow<'a, str>>,
    #[serde(borrow, default)]
    d: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct GuildCreate<'a> {
    #[serde(borrow)]
    id: Cow<'a, str>,
    #[serde(default)]
    unavailable: Option<bool>,
    #[serde(borrow, default)]
    presences: Vec<&'a RawValue>,
}

#[derive(Deserialize)]
struct GuildDelete<'a> {
    #[serde(borrow)]
    id: Cow<'a, str>,
    #[serde(default)]
    unavailable: Option<bool>,
}

#[derive(Deserialize)]
struct PresenceGuild<'a> {
    #[serde(borrow, default)]
    guild_id: Option<Cow<'a, str>>,
}

#[derive(Deserialize)]
struct MembersChunk<'a> {
    #[serde(borrow)]
    guild_id: Cow<'a, str>,
    #[serde(borrow, default)]
    members: Vec<&'a RawValue>,
    #[serde(borrow, default)]
    presences: Option<Vec<&'a RawValue>>,
    #[serde(default)]
    not_found: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct Ready<'a> {
    #[serde(borrow, default)]
    guilds: Vec<&'a RawValue>,
}

/// The pseudo guild used when no guild reported the owner at all.
const NOBODY: &str = "";

pub struct Engine {
    owner_id: String,
    store: PresenceStore,
    per_guild: HashMap<String, (u64, Observed)>,
    seq: u64,
    /// Last merged presence that was applied (seeds activity diffs).
    last: Option<Observed>,
    /// Last state written to `discord.json`.
    state: Option<PresenceState>,
    /// A fresh `discord.json` from before this start: (status, since).
    restored: Option<(String, String)>,
    status_logged: bool,
    deadline: DateTime<Utc>,
}

impl Engine {
    /// Open the store, close the previous run in the history (an inferred
    /// stop at the last `updatedAt` when it ended without one), and log the
    /// start. A previous `discord.json` younger than `max_age` seeds the
    /// activity diff and `since`, so a quick restart does not re-log them.
    pub fn start(
        owner_id: impl Into<String>,
        mut store: PresenceStore,
        now: DateTime<Utc>,
        max_age: Duration,
        first_observation_timeout: Duration,
    ) -> std::io::Result<Self> {
        let prev = store.read_current();
        let mut events = Vec::new();
        let ended_cleanly = matches!(store.last_event(), Some(HistoryEvent::Watcher { ref state, .. }) if state == "stop");
        if let Some(p) = &prev {
            if !ended_cleanly && parse_iso(&p.updated_at).is_some() {
                events.push(HistoryEvent::Watcher {
                    at: p.updated_at.clone(),
                    state: "stop".into(),
                    inferred: true,
                });
            }
        }
        events.push(HistoryEvent::Watcher {
            at: iso(now),
            state: "start".into(),
            inferred: false,
        });
        store.append(&events, now)?;
        let fresh = prev.filter(|p| {
            parse_iso(&p.updated_at).is_some_and(|u| {
                let age = (now - u).num_seconds();
                age >= 0 && age as u64 <= max_age.as_secs()
            })
        });
        Ok(Self {
            owner_id: owner_id.into(),
            store,
            per_guild: HashMap::new(),
            seq: 0,
            last: fresh.as_ref().map(|p| Observed {
                status: p.status.clone(),
                client_status: p.client_status.clone(),
                activities: p.activities.clone(),
            }),
            restored: fresh.map(|p| (p.status, p.since)),
            state: None,
            status_logged: false,
            deadline: now
                + chrono::Duration::from_std(first_observation_timeout)
                    .unwrap_or(chrono::Duration::seconds(60)),
        })
    }

    pub fn state(&self) -> Option<&PresenceState> {
        self.state.as_ref()
    }

    pub fn store(&self) -> &PresenceStore {
        &self.store
    }

    fn is_owner(&self, raw: &RawValue) -> bool {
        serde_json::from_str::<PeekUser>(raw.get())
            .ok()
            .and_then(|p| p.user)
            .is_some_and(|u| u.id == self.owner_id)
    }

    fn owner_presence(&self, raw: &RawValue) -> Option<Observed> {
        if !self.is_owner(raw) {
            return None; // someone else: dropped here, never parsed further
        }
        serde_json::from_str::<RawPresence>(raw.get())
            .ok()
            .map(|p| map_presence(&p))
    }

    /// Handle one raw gateway message (text frame).
    pub fn handle_text(&mut self, text: &str, now: DateTime<Utc>) -> Vec<Outbound> {
        let Ok(env) = serde_json::from_str::<Envelope>(text) else {
            return Vec::new();
        };
        if env.op != 0 {
            return Vec::new();
        }
        let (Some(t), Some(d)) = (env.t.as_deref(), env.d) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match t {
            "READY" => {
                if let Ok(r) = serde_json::from_str::<Ready>(d.get()) {
                    tracing::info!(guilds = r.guilds.len(), "presence gateway ready");
                }
            }
            "GUILD_CREATE" => {
                let Ok(g) = serde_json::from_str::<GuildCreate>(d.get()) else {
                    return out;
                };
                if g.unavailable == Some(true) {
                    return out;
                }
                let found = g.presences.iter().find_map(|p| self.owner_presence(p));
                let guild = g.id.into_owned();
                tracing::debug!(
                    presences = g.presences.len(),
                    owner = found.is_some(),
                    "guild create"
                );
                if let Some(obs) = found {
                    self.observe(&guild, obs, now);
                }
                if let Ok(id) = guild.parse::<u64>() {
                    out.push(Outbound::RequestOwner { guild_id: id });
                }
            }
            "PRESENCE_UPDATE" => {
                if let Some(obs) = self.owner_presence(d) {
                    let guild = serde_json::from_str::<PresenceGuild>(d.get())
                        .ok()
                        .and_then(|g| g.guild_id)
                        .map(Cow::into_owned)
                        .unwrap_or_default();
                    self.observe(&guild, obs, now);
                }
            }
            "GUILD_MEMBERS_CHUNK" => {
                let Ok(c) = serde_json::from_str::<MembersChunk>(d.get()) else {
                    return out;
                };
                let guild = c.guild_id.into_owned();
                let presence = c
                    .presences
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .find_map(|p| self.owner_presence(p));
                let member = c.members.iter().any(|m| self.is_owner(m));
                let not_found = c.not_found.iter().any(|v| match v {
                    serde_json::Value::String(s) => *s == self.owner_id,
                    serde_json::Value::Number(n) => {
                        n.as_u64().map(|n| n.to_string()).as_deref() == Some(self.owner_id.as_str())
                    }
                    _ => false,
                });
                tracing::debug!(
                    members = c.members.len(),
                    presences = c.presences.as_ref().map(Vec::len),
                    owner_presence = presence.is_some(),
                    owner_member = member,
                    not_found,
                    "members chunk"
                );
                if let Some(obs) = presence {
                    self.observe(&guild, obs, now);
                } else if member {
                    // A member with no presence in the reply is offline (or invisible).
                    self.observe(&guild, Observed::offline(), now);
                } else if not_found {
                    self.forget_guild(&guild, now);
                }
            }
            "GUILD_DELETE" => {
                if let Ok(g) = serde_json::from_str::<GuildDelete>(d.get()) {
                    if g.unavailable != Some(true) {
                        let guild = g.id.into_owned();
                        self.forget_guild(&guild, now);
                    }
                }
            }
            _ => {}
        }
        out
    }

    /// Record what one guild says about the owner, then apply the merge.
    pub fn observe(&mut self, guild: &str, obs: Observed, now: DateTime<Utc>) {
        if guild != NOBODY {
            self.per_guild.remove(NOBODY);
        }
        self.seq += 1;
        self.per_guild.insert(guild.to_owned(), (self.seq, obs));
        self.apply(now);
    }

    pub fn forget_guild(&mut self, guild: &str, now: DateTime<Utc>) {
        if self.per_guild.remove(guild).is_some() {
            self.apply(now);
        }
    }

    /// The freshest observation across guilds.
    pub fn merged(&self) -> Option<&Observed> {
        self.per_guild
            .values()
            .max_by_key(|(seq, _)| *seq)
            .map(|(_, o)| o)
    }

    fn apply(&mut self, now: DateTime<Utc>) {
        let Some(merged) = self.merged().cloned() else {
            return;
        };
        let at = iso(now);
        let mut events = Vec::new();
        let prev_status = if self.status_logged {
            self.last.as_ref().map(|l| l.status.clone())
        } else {
            self.restored.as_ref().map(|(s, _)| s.clone())
        };
        let platforms_changed = self
            .last
            .as_ref()
            .is_some_and(|l| l.client_status.platforms() != merged.client_status.platforms());
        let status_changed = prev_status.as_deref() != Some(merged.status.as_str());
        if !self.status_logged || status_changed || platforms_changed {
            events.push(HistoryEvent::Status {
                at: at.clone(),
                from: prev_status.clone(),
                to: merged.status.clone(),
                client_status: merged.client_status.clone(),
            });
            if status_changed {
                tracing::info!(from = prev_status.as_deref().unwrap_or("unknown"), to = %merged.status, "owner discord status");
            }
        }
        let before = self
            .last
            .as_ref()
            .map(|l| l.activities.clone())
            .unwrap_or_default();
        for a in &merged.activities {
            if !before.iter().any(|b| b.key() == a.key()) {
                events.push(HistoryEvent::ActivityStart {
                    at: at.clone(),
                    activity: a.clone(),
                });
            }
        }
        for b in &before {
            if !merged.activities.iter().any(|a| a.key() == b.key()) {
                events.push(HistoryEvent::ActivityStop {
                    at: at.clone(),
                    activity: b.clone(),
                });
            }
        }
        let since = if status_changed {
            at.clone()
        } else {
            self.state
                .as_ref()
                .map(|s| s.since.clone())
                .or_else(|| self.restored.as_ref().map(|(_, s)| s.clone()))
                .unwrap_or_else(|| at.clone())
        };
        let next = PresenceState {
            status: merged.status.clone(),
            client_status: merged.client_status.clone(),
            activities: merged.activities.clone(),
            since,
            updated_at: at,
        };
        let unchanged = self.state.as_ref().is_some_and(|s| {
            s.status == next.status
                && s.client_status == next.client_status
                && s.activities == next.activities
        });
        self.status_logged = true;
        self.last = Some(merged);
        if let Err(e) = self.store.append(&events, now) {
            tracing::warn!(error = %e.kind(), "presence history append failed");
        }
        if !unchanged {
            if let Err(e) = self.store.write_current(&next) {
                tracing::warn!(error = %e.kind(), "presence state write failed");
            }
            self.state = Some(next);
        }
    }

    /// Every minute: refresh `updatedAt` (readers use it to tell a live
    /// watcher from a dead one), and after the first-observation timeout
    /// with nothing from any guild, record the owner as offline.
    pub fn tick(&mut self, now: DateTime<Utc>) {
        if self.per_guild.is_empty() && now >= self.deadline {
            tracing::info!("owner not reported by any guild: offline");
            self.observe(NOBODY, Observed::offline(), now);
        }
        if let Some(s) = self.state.as_mut() {
            s.updated_at = iso(now);
            if let Err(e) = self.store.write_current(s) {
                tracing::warn!(error = %e.kind(), "presence state refresh failed");
            }
        }
        if let Err(e) = self.store.maybe_prune(now) {
            tracing::warn!(error = %e.kind(), "presence history prune failed");
        }
    }

    /// Clean shutdown: log the stop and stamp `updatedAt`.
    pub fn stop(&mut self, now: DateTime<Utc>) {
        let ev = HistoryEvent::Watcher {
            at: iso(now),
            state: "stop".into(),
            inferred: false,
        };
        if let Err(e) = self.store.append(&[ev], now) {
            tracing::warn!(error = %e.kind(), "presence history append failed");
        }
        if let Some(s) = self.state.as_mut() {
            s.updated_at = iso(now);
            let _ = self.store.write_current(s);
        }
    }
}
