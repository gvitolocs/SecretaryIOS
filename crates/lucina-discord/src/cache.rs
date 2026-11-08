//! The little the message session remembers about guilds: guild names and
//! each channel's name, kind and parent, filled from `GUILD_CREATE` and the
//! channel / thread events. It is what the admin-channel rule (channel or
//! parent name) and the timeline's `guildName` / `channelName` read.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::rest::{ChannelInfo, ChannelKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedChannel {
    pub guild_id: Option<u64>,
    pub name: Option<String>,
    pub parent_id: Option<u64>,
    pub kind: ChannelKind,
}

impl From<&ChannelInfo> for CachedChannel {
    fn from(c: &ChannelInfo) -> Self {
        Self {
            guild_id: c.guild_id,
            name: (!c.name.is_empty()).then(|| c.name.clone()),
            parent_id: c.parent_id,
            kind: c.kind,
        }
    }
}

/// Names of a message's channel: its own, its parent's, its kind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelNames {
    pub name: Option<String>,
    pub parent_name: Option<String>,
    pub kind: ChannelKind,
    pub known: bool,
}

#[derive(Debug, Default)]
pub struct GuildCache {
    guilds: HashMap<u64, String>,
    channels: HashMap<u64, CachedChannel>,
}

pub type SharedCache = Arc<Mutex<GuildCache>>;

impl GuildCache {
    pub fn set_guild(&mut self, id: u64, name: &str) {
        self.guilds.insert(id, name.to_owned());
    }

    /// The guild and its channels (the bot left, or the guild was deleted).
    pub fn remove_guild(&mut self, id: u64) {
        self.guilds.remove(&id);
        self.channels.retain(|_, c| c.guild_id != Some(id));
    }

    pub fn upsert_channel(&mut self, id: u64, c: CachedChannel) {
        self.channels.insert(id, c);
    }

    pub fn remove_channel(&mut self, id: u64) {
        self.channels.remove(&id);
    }

    pub fn guild_name(&self, id: u64) -> Option<String> {
        self.guilds.get(&id).cloned()
    }

    pub fn names(&self, channel_id: u64) -> ChannelNames {
        let Some(c) = self.channels.get(&channel_id) else {
            return ChannelNames::default();
        };
        ChannelNames {
            name: c.name.clone(),
            parent_name: c
                .parent_id
                .and_then(|p| self.channels.get(&p))
                .and_then(|p| p.name.clone()),
            kind: c.kind,
            known: true,
        }
    }

    pub fn contains(&self, channel_id: u64) -> bool {
        self.channels.contains_key(&channel_id)
    }

    pub fn len(&self) -> (usize, usize) {
        (self.guilds.len(), self.channels.len())
    }

    pub fn is_empty(&self) -> bool {
        self.guilds.is_empty() && self.channels.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(guild: u64, name: &str, parent: Option<u64>, kind: ChannelKind) -> CachedChannel {
        CachedChannel {
            guild_id: Some(guild),
            name: Some(name.into()),
            parent_id: parent,
            kind,
        }
    }

    #[test]
    fn names_with_parents_and_guild_removal() {
        let mut c = GuildCache::default();
        c.set_guild(1, "Casa");
        c.upsert_channel(10, ch(1, "Ops", None, ChannelKind::Category));
        c.upsert_channel(11, ch(1, "general", Some(10), ChannelKind::Text));
        c.upsert_channel(12, ch(1, "idea", Some(11), ChannelKind::Thread));
        assert_eq!(c.guild_name(1).as_deref(), Some("Casa"));
        let n = c.names(11);
        assert_eq!(
            (n.name.as_deref(), n.parent_name.as_deref(), n.kind, n.known),
            (Some("general"), Some("Ops"), ChannelKind::Text, true)
        );
        assert_eq!(c.names(12).parent_name.as_deref(), Some("general"));
        assert!(!c.names(99).known);
        c.remove_channel(10);
        assert_eq!(c.names(11).parent_name, None);
        c.remove_guild(1);
        assert!(c.is_empty());
    }
}
