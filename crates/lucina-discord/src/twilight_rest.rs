//! [`DiscordRest`] over twilight-http. Mentions in Lucina's messages are
//! limited to users and the replied-to author: a model reply can never ping
//! `@everyone`, `@here` or a role.

use async_trait::async_trait;
use twilight_http::request::AuditLogReason;
use twilight_http::Client;
use twilight_model::channel::message::{AllowedMentions, MentionType};
use twilight_model::channel::permission_overwrite::PermissionOverwriteType;
use twilight_model::channel::thread::AutoArchiveDuration;
use twilight_model::channel::{Channel, ChannelType};
use twilight_model::guild::Permissions;
use twilight_model::http::permission_overwrite::{
    PermissionOverwrite, PermissionOverwriteType as HttpOverwriteType,
};
use twilight_model::id::Id;

use crate::rest::{
    ChannelInfo, ChannelKind, DiscordRest, GuildInfo, MemberInfo, NewChannel, Overwrite, RestError,
    RestResult, RoleInfo,
};

pub struct TwilightRest {
    http: Client,
    mentions: AllowedMentions,
}

impl TwilightRest {
    pub fn new(token: String) -> Self {
        Self {
            http: Client::new(token),
            mentions: AllowedMentions {
                parse: vec![MentionType::Users],
                replied_user: true,
                ..AllowedMentions::default()
            },
        }
    }
}

fn err(e: impl std::fmt::Display) -> RestError {
    RestError(e.to_string().chars().take(500).collect())
}

fn id<M>(v: u64) -> RestResult<Id<M>> {
    Id::new_checked(v).ok_or_else(|| RestError(format!("invalid Discord id {v}")))
}

pub fn kind_of(t: ChannelType) -> ChannelKind {
    match t {
        ChannelType::GuildText | ChannelType::GuildAnnouncement => ChannelKind::Text,
        ChannelType::GuildVoice | ChannelType::GuildStageVoice => ChannelKind::Voice,
        ChannelType::GuildCategory => ChannelKind::Category,
        ChannelType::PublicThread
        | ChannelType::PrivateThread
        | ChannelType::AnnouncementThread => ChannelKind::Thread,
        ChannelType::Private | ChannelType::Group => ChannelKind::Dm,
        _ => ChannelKind::Other,
    }
}

pub fn channel_info(c: &Channel) -> ChannelInfo {
    ChannelInfo {
        id: c.id.get(),
        guild_id: c.guild_id.map(|g| g.get()),
        name: c.name.clone().unwrap_or_default(),
        kind: kind_of(c.kind),
        parent_id: c.parent_id.map(|p| p.get()),
        topic: c.topic.clone(),
        overwrites: c
            .permission_overwrites
            .iter()
            .flatten()
            .map(|o| Overwrite {
                id: o.id.get(),
                member: o.kind == PermissionOverwriteType::Member,
                allow: o.allow.bits(),
                deny: o.deny.bits(),
            })
            .collect(),
    }
}

fn twilight_kind(k: ChannelKind) -> ChannelType {
    match k {
        ChannelKind::Voice => ChannelType::GuildVoice,
        ChannelKind::Category => ChannelType::GuildCategory,
        _ => ChannelType::GuildText,
    }
}

#[async_trait]
impl DiscordRest for TwilightRest {
    async fn send_message(
        &self,
        channel: u64,
        content: &str,
        reply_to: Option<u64>,
    ) -> RestResult<u64> {
        let mut req = self
            .http
            .create_message(id(channel)?)
            .content(content)
            .allowed_mentions(Some(&self.mentions));
        if let Some(r) = reply_to {
            req = req.reply(id(r)?).fail_if_not_exists(false);
        }
        let msg = req.await.map_err(err)?.model().await.map_err(err)?;
        Ok(msg.id.get())
    }

    async fn typing(&self, channel: u64) -> RestResult<()> {
        self.http
            .create_typing_trigger(id(channel)?)
            .await
            .map_err(err)?;
        Ok(())
    }

    async fn channel(&self, channel: u64) -> RestResult<ChannelInfo> {
        let c = self
            .http
            .channel(id(channel)?)
            .await
            .map_err(err)?
            .model()
            .await
            .map_err(err)?;
        Ok(channel_info(&c))
    }

    async fn guild(&self, guild: u64) -> RestResult<GuildInfo> {
        let g = self
            .http
            .guild(id(guild)?)
            .with_counts(true)
            .await
            .map_err(err)?
            .model()
            .await
            .map_err(err)?;
        Ok(GuildInfo {
            id: g.id.get(),
            name: g.name,
            owner_id: g.owner_id.get(),
            member_count: g.approximate_member_count.or(g.member_count),
        })
    }

    async fn guild_channels(&self, guild: u64) -> RestResult<Vec<ChannelInfo>> {
        let list = self
            .http
            .guild_channels(id(guild)?)
            .await
            .map_err(err)?
            .models()
            .await
            .map_err(err)?;
        Ok(list.iter().map(channel_info).collect())
    }

    async fn roles(&self, guild: u64) -> RestResult<Vec<RoleInfo>> {
        let list = self
            .http
            .roles(id(guild)?)
            .await
            .map_err(err)?
            .models()
            .await
            .map_err(err)?;
        Ok(list
            .into_iter()
            .map(|r| RoleInfo {
                id: r.id.get(),
                name: r.name,
                permissions: r.permissions.bits(),
            })
            .collect())
    }

    async fn member(&self, guild: u64, user: u64) -> RestResult<MemberInfo> {
        let m = self
            .http
            .guild_member(id(guild)?, id(user)?)
            .await
            .map_err(err)?
            .model()
            .await
            .map_err(err)?;
        Ok(MemberInfo {
            user_id: m.user.id.get(),
            display_name: m.nick.or(m.user.global_name).unwrap_or(m.user.name),
            roles: m.roles.iter().map(|r| r.get()).collect(),
        })
    }

    async fn create_channel(
        &self,
        guild: u64,
        new: &NewChannel,
        reason: &str,
    ) -> RestResult<ChannelInfo> {
        let mut req = self
            .http
            .create_guild_channel(id(guild)?, &new.name)
            .kind(twilight_kind(new.kind))
            .reason(reason);
        if let Some(p) = new.parent_id {
            req = req.parent_id(id(p)?);
        }
        if let Some(t) = new.topic.as_deref().filter(|t| !t.is_empty()) {
            req = req.topic(t);
        }
        let c = req.await.map_err(err)?.model().await.map_err(err)?;
        Ok(channel_info(&c))
    }

    async fn set_topic(&self, channel: u64, topic: &str, reason: &str) -> RestResult<()> {
        self.http
            .update_channel(id(channel)?)
            .topic(topic)
            .reason(reason)
            .await
            .map_err(err)?;
        Ok(())
    }

    async fn create_role(
        &self,
        guild: u64,
        name: &str,
        permissions: u64,
        reason: &str,
    ) -> RestResult<RoleInfo> {
        let r = self
            .http
            .create_role(id(guild)?)
            .name(name)
            .permissions(Permissions::from_bits_truncate(permissions))
            .reason(reason)
            .await
            .map_err(err)?
            .model()
            .await
            .map_err(err)?;
        Ok(RoleInfo {
            id: r.id.get(),
            name: r.name,
            permissions: r.permissions.bits(),
        })
    }

    async fn set_role_permissions(
        &self,
        guild: u64,
        role: u64,
        permissions: u64,
        reason: &str,
    ) -> RestResult<()> {
        self.http
            .update_role(id(guild)?, id(role)?)
            .permissions(Permissions::from_bits_truncate(permissions))
            .reason(reason)
            .await
            .map_err(err)?;
        Ok(())
    }

    async fn add_member_role(
        &self,
        guild: u64,
        user: u64,
        role: u64,
        reason: &str,
    ) -> RestResult<()> {
        self.http
            .add_guild_member_role(id(guild)?, id(user)?, id(role)?)
            .reason(reason)
            .await
            .map_err(err)?;
        Ok(())
    }

    async fn create_invite(
        &self,
        channel: u64,
        max_age_secs: u32,
        max_uses: u16,
        unique: bool,
        reason: &str,
    ) -> RestResult<String> {
        let invite = self
            .http
            .create_invite(id(channel)?)
            .max_age(max_age_secs)
            .max_uses(max_uses)
            .unique(unique)
            .reason(reason)
            .await
            .map_err(err)?
            .model()
            .await
            .map_err(err)?;
        Ok(format!("https://discord.gg/{}", invite.code))
    }

    async fn put_overwrite(
        &self,
        channel: u64,
        overwrite: &Overwrite,
        reason: &str,
    ) -> RestResult<()> {
        let o = PermissionOverwrite {
            allow: Some(Permissions::from_bits_truncate(overwrite.allow)),
            deny: Some(Permissions::from_bits_truncate(overwrite.deny)),
            id: id(overwrite.id)?,
            kind: if overwrite.member {
                HttpOverwriteType::Member
            } else {
                HttpOverwriteType::Role
            },
        };
        self.http
            .update_channel_permission(id(channel)?, &o)
            .reason(reason)
            .await
            .map_err(err)?;
        Ok(())
    }

    async fn create_thread(
        &self,
        channel: u64,
        name: &str,
        auto_archive_minutes: u16,
        _reason: &str,
    ) -> RestResult<ChannelInfo> {
        // Thread creation takes no audit-log reason in twilight-http 0.17.
        let c = self
            .http
            .create_thread(id(channel)?, name, ChannelType::PublicThread)
            .auto_archive_duration(AutoArchiveDuration::from(auto_archive_minutes))
            .await
            .map_err(err)?
            .model()
            .await
            .map_err(err)?;
        Ok(channel_info(&c))
    }

    async fn pin(&self, channel: u64, message: u64, reason: &str) -> RestResult<()> {
        self.http
            .create_pin(id(channel)?, id(message)?)
            .reason(reason)
            .await
            .map_err(err)?;
        Ok(())
    }
}
