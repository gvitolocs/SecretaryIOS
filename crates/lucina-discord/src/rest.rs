//! The Discord REST calls the message session makes, as a trait: the
//! twilight-http client implements it (`twilight_rest`, feature `gateway`),
//! tests implement it with a stub that records calls.
//!
//! Only additive calls exist here: send, typing, reads, create channel /
//! role / invite / thread, set a topic, add role permissions, add a role to
//! a member, write a permission overwrite, pin. There is no delete, kick,
//! ban or role removal.

use async_trait::async_trait;

/// Discord permission bits used by the admin operations.
pub mod perm {
    pub const ADMINISTRATOR: u64 = 1 << 3;
    pub const MANAGE_CHANNELS: u64 = 1 << 4;
    pub const VIEW_CHANNEL: u64 = 1 << 10;
    pub const SEND_MESSAGES: u64 = 1 << 11;
    pub const READ_MESSAGE_HISTORY: u64 = 1 << 16;
    pub const MANAGE_ROLES: u64 = 1 << 28;
    pub const CREATE_PUBLIC_THREADS: u64 = 1 << 35;
    pub const CREATE_PRIVATE_THREADS: u64 = 1 << 36;
    pub const SEND_MESSAGES_IN_THREADS: u64 = 1 << 38;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelKind {
    Text,
    Voice,
    Category,
    Thread,
    Dm,
    #[default]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overwrite {
    /// A role id (the guild id is `@everyone`) or a member id.
    pub id: u64,
    pub member: bool,
    pub allow: u64,
    pub deny: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChannelInfo {
    pub id: u64,
    pub guild_id: Option<u64>,
    pub name: String,
    pub kind: ChannelKind,
    pub parent_id: Option<u64>,
    pub topic: Option<String>,
    pub overwrites: Vec<Overwrite>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleInfo {
    pub id: u64,
    pub name: String,
    pub permissions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberInfo {
    pub user_id: u64,
    /// discord.js `displayName`: nickname, else global name, else username.
    pub display_name: String,
    pub roles: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuildInfo {
    pub id: u64,
    pub name: String,
    pub owner_id: u64,
    pub member_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewChannel {
    pub name: String,
    pub kind: ChannelKind,
    pub parent_id: Option<u64>,
    pub topic: Option<String>,
}

/// A failed call: Discord's error text (never the token).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestError(pub String);

impl std::fmt::Display for RestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RestError {}

pub type RestResult<T> = Result<T, RestError>;

#[async_trait]
pub trait DiscordRest: Send + Sync {
    /// A message (a reply to `reply_to` when set); the new message's id.
    async fn send_message(
        &self,
        channel: u64,
        content: &str,
        reply_to: Option<u64>,
    ) -> RestResult<u64>;
    async fn typing(&self, channel: u64) -> RestResult<()>;
    async fn channel(&self, channel: u64) -> RestResult<ChannelInfo>;
    async fn guild(&self, guild: u64) -> RestResult<GuildInfo>;
    async fn guild_channels(&self, guild: u64) -> RestResult<Vec<ChannelInfo>>;
    async fn roles(&self, guild: u64) -> RestResult<Vec<RoleInfo>>;
    async fn member(&self, guild: u64, user: u64) -> RestResult<MemberInfo>;
    async fn create_channel(
        &self,
        guild: u64,
        new: &NewChannel,
        reason: &str,
    ) -> RestResult<ChannelInfo>;
    async fn set_topic(&self, channel: u64, topic: &str, reason: &str) -> RestResult<()>;
    async fn create_role(
        &self,
        guild: u64,
        name: &str,
        permissions: u64,
        reason: &str,
    ) -> RestResult<RoleInfo>;
    async fn set_role_permissions(
        &self,
        guild: u64,
        role: u64,
        permissions: u64,
        reason: &str,
    ) -> RestResult<()>;
    async fn add_member_role(
        &self,
        guild: u64,
        user: u64,
        role: u64,
        reason: &str,
    ) -> RestResult<()>;
    /// The invite URL.
    async fn create_invite(
        &self,
        channel: u64,
        max_age_secs: u32,
        max_uses: u16,
        unique: bool,
        reason: &str,
    ) -> RestResult<String>;
    /// Write (create or replace) one permission overwrite.
    async fn put_overwrite(
        &self,
        channel: u64,
        overwrite: &Overwrite,
        reason: &str,
    ) -> RestResult<()>;
    /// A public thread without a starter message.
    async fn create_thread(
        &self,
        channel: u64,
        name: &str,
        auto_archive_minutes: u16,
        reason: &str,
    ) -> RestResult<ChannelInfo>;
    async fn pin(&self, channel: u64, message: u64, reason: &str) -> RestResult<()>;
}

/// A member's guild-level permissions (the guild owner has all of them;
/// Administrator implies all).
pub fn guild_permissions(guild: &GuildInfo, member: &MemberInfo, roles: &[RoleInfo]) -> u64 {
    if guild.owner_id == member.user_id {
        return u64::MAX;
    }
    let bits = roles
        .iter()
        .filter(|r| r.id == guild.id || member.roles.contains(&r.id))
        .fold(0u64, |acc, r| acc | r.permissions);
    if bits & perm::ADMINISTRATOR != 0 {
        u64::MAX
    } else {
        bits
    }
}

/// discord.js `permissionOverwrites.edit(id, {...})`: the bits set to `true`
/// are allowed, those set to `false` denied, every other bit of an existing
/// overwrite stays as it was.
pub fn edit_overwrite(
    existing: Option<&Overwrite>,
    id: u64,
    member: bool,
    allow: u64,
    deny: u64,
) -> Overwrite {
    let (old_allow, old_deny) = existing.map_or((0, 0), |o| (o.allow, o.deny));
    Overwrite {
        id,
        member,
        allow: (old_allow & !deny) | allow,
        deny: (old_deny & !allow) | deny,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_and_overwrite_edits() {
        let g = GuildInfo {
            id: 1,
            name: "g".into(),
            owner_id: 9,
            member_count: None,
        };
        let roles = vec![
            RoleInfo {
                id: 1,
                name: "@everyone".into(),
                permissions: perm::VIEW_CHANNEL,
            },
            RoleInfo {
                id: 5,
                name: "Mods".into(),
                permissions: perm::MANAGE_CHANNELS,
            },
            RoleInfo {
                id: 6,
                name: "Admin".into(),
                permissions: perm::ADMINISTRATOR,
            },
        ];
        let m = |roles: Vec<u64>, id| MemberInfo {
            user_id: id,
            display_name: "x".into(),
            roles,
        };
        assert_eq!(
            guild_permissions(&g, &m(vec![5], 2), &roles),
            perm::VIEW_CHANNEL | perm::MANAGE_CHANNELS
        );
        assert_eq!(guild_permissions(&g, &m(vec![6], 2), &roles), u64::MAX);
        assert_eq!(guild_permissions(&g, &m(vec![], 9), &roles), u64::MAX);
        assert_eq!(
            guild_permissions(&g, &m(vec![], 2), &roles),
            perm::VIEW_CHANNEL
        );

        let old = Overwrite {
            id: 1,
            member: false,
            allow: perm::SEND_MESSAGES | perm::VIEW_CHANNEL,
            deny: perm::READ_MESSAGE_HISTORY,
        };
        let new = edit_overwrite(Some(&old), 1, false, 0, perm::SEND_MESSAGES);
        assert_eq!(new.allow, perm::VIEW_CHANNEL);
        assert_eq!(new.deny, perm::READ_MESSAGE_HISTORY | perm::SEND_MESSAGES);
        let new = edit_overwrite(Some(&old), 1, false, perm::READ_MESSAGE_HISTORY, 0);
        assert_eq!(new.deny, 0);
        assert_eq!(
            new.allow,
            perm::SEND_MESSAGES | perm::VIEW_CHANNEL | perm::READ_MESSAGE_HISTORY
        );
    }
}
