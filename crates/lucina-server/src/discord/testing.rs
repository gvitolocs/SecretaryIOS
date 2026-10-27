//! A [`DiscordRest`] stub for tests: an in-memory guild, every call recorded.
//! It never touches the network.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use lucina_discord::async_trait;
use lucina_discord::rest::{
    ChannelInfo, ChannelKind, DiscordRest, GuildInfo, MemberInfo, NewChannel, Overwrite, RestError,
    RestResult, RoleInfo,
};

/// A sent message: (channel, text, replied-to message).
pub type Sent = (u64, String, Option<u64>);

pub struct RecordingRest {
    guild: Mutex<GuildInfo>,
    channels: Mutex<Vec<ChannelInfo>>,
    roles: Mutex<Vec<RoleInfo>>,
    members: Mutex<BTreeMap<u64, MemberInfo>>,
    calls: Mutex<Vec<String>>,
    sent: Mutex<Vec<Sent>>,
    next: AtomicU64,
    /// `send_message` fails with this text.
    pub fail_sends: Mutex<Option<String>>,
}

impl RecordingRest {
    pub fn new(guild_id: u64, name: &str, owner_id: u64) -> Self {
        Self {
            guild: Mutex::new(GuildInfo {
                id: guild_id,
                name: name.into(),
                owner_id,
                member_count: None,
            }),
            channels: Mutex::default(),
            roles: Mutex::default(),
            members: Mutex::default(),
            calls: Mutex::default(),
            sent: Mutex::default(),
            next: AtomicU64::new(10_000),
            fail_sends: Mutex::new(None),
        }
    }

    pub fn add_role(&self, id: u64, name: &str, permissions: u64) {
        self.roles.lock().unwrap().push(RoleInfo {
            id,
            name: name.into(),
            permissions,
        });
    }

    pub fn add_member(&self, id: u64, name: &str, roles: Vec<u64>) {
        self.members.lock().unwrap().insert(
            id,
            MemberInfo {
                user_id: id,
                display_name: name.into(),
                roles,
            },
        );
    }

    pub fn add_channel(&self, id: u64, name: &str, kind: ChannelKind, parent: Option<u64>) {
        let guild = self.guild.lock().unwrap().id;
        self.channels.lock().unwrap().push(ChannelInfo {
            id,
            guild_id: Some(guild),
            name: name.into(),
            kind,
            parent_id: parent,
            topic: None,
            overwrites: Vec::new(),
        });
    }

    /// Every call, reads included, in order.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// The calls that change something in Discord (messages and typing aside).
    pub fn writes(&self) -> Vec<String> {
        const READS: [&str; 7] = [
            "guild ",
            "roles ",
            "member ",
            "channel ",
            "guild_channels ",
            "typing ",
            "send_message ",
        ];
        self.calls()
            .into_iter()
            .filter(|c| !READS.iter().any(|r| c.starts_with(r)))
            .collect()
    }

    pub fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap().clone()
    }

    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    fn id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

#[async_trait]
impl DiscordRest for RecordingRest {
    async fn send_message(
        &self,
        channel: u64,
        content: &str,
        reply_to: Option<u64>,
    ) -> RestResult<u64> {
        self.record(format!("send_message {channel}"));
        if let Some(e) = self.fail_sends.lock().unwrap().clone() {
            return Err(RestError(e));
        }
        self.sent
            .lock()
            .unwrap()
            .push((channel, content.to_owned(), reply_to));
        Ok(self.id())
    }

    async fn typing(&self, channel: u64) -> RestResult<()> {
        self.record(format!("typing {channel}"));
        Ok(())
    }

    async fn channel(&self, channel: u64) -> RestResult<ChannelInfo> {
        self.record(format!("channel {channel}"));
        self.channels
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.id == channel)
            .cloned()
            .ok_or_else(|| RestError("Unknown Channel".into()))
    }

    async fn guild(&self, guild: u64) -> RestResult<GuildInfo> {
        self.record(format!("guild {guild}"));
        let mut g = self.guild.lock().unwrap().clone();
        g.member_count = Some(self.members.lock().unwrap().len() as u64);
        Ok(g)
    }

    async fn guild_channels(&self, guild: u64) -> RestResult<Vec<ChannelInfo>> {
        self.record(format!("guild_channels {guild}"));
        Ok(self.channels.lock().unwrap().clone())
    }

    async fn roles(&self, guild: u64) -> RestResult<Vec<RoleInfo>> {
        self.record(format!("roles {guild}"));
        Ok(self.roles.lock().unwrap().clone())
    }

    async fn member(&self, guild: u64, user: u64) -> RestResult<MemberInfo> {
        self.record(format!("member {guild} {user}"));
        self.members
            .lock()
            .unwrap()
            .get(&user)
            .cloned()
            .ok_or_else(|| RestError("Unknown Member".into()))
    }

    async fn create_channel(
        &self,
        _guild: u64,
        new: &NewChannel,
        reason: &str,
    ) -> RestResult<ChannelInfo> {
        self.record(format!(
            "create_channel {:?} {} parent={} topic={} reason={reason}",
            new.kind,
            new.name,
            new.parent_id.map_or("-".into(), |p| p.to_string()),
            new.topic.as_deref().unwrap_or("-"),
        ));
        let id = self.id();
        self.add_channel(id, &new.name, new.kind, new.parent_id);
        let mut c = self.channel_unrecorded(id);
        c.topic = new.topic.clone();
        Ok(c)
    }

    async fn set_topic(&self, channel: u64, topic: &str, _reason: &str) -> RestResult<()> {
        self.record(format!("set_topic {channel} {topic}"));
        Ok(())
    }

    async fn create_role(
        &self,
        _guild: u64,
        name: &str,
        permissions: u64,
        _reason: &str,
    ) -> RestResult<RoleInfo> {
        self.record(format!("create_role {name} {permissions}"));
        let id = self.id();
        self.add_role(id, name, permissions);
        Ok(RoleInfo {
            id,
            name: name.into(),
            permissions,
        })
    }

    async fn set_role_permissions(
        &self,
        _guild: u64,
        role: u64,
        permissions: u64,
        _reason: &str,
    ) -> RestResult<()> {
        self.record(format!("set_role_permissions {role} {permissions}"));
        Ok(())
    }

    async fn add_member_role(
        &self,
        _guild: u64,
        user: u64,
        role: u64,
        _reason: &str,
    ) -> RestResult<()> {
        self.record(format!("add_member_role {user} {role}"));
        Ok(())
    }

    async fn create_invite(
        &self,
        channel: u64,
        max_age_secs: u32,
        max_uses: u16,
        unique: bool,
        _reason: &str,
    ) -> RestResult<String> {
        self.record(format!(
            "create_invite {channel} {max_age_secs} {max_uses} {unique}"
        ));
        Ok(format!("https://discord.gg/stub{channel}"))
    }

    async fn put_overwrite(&self, channel: u64, o: &Overwrite, _reason: &str) -> RestResult<()> {
        self.record(format!(
            "put_overwrite {channel} {} {} allow={} deny={}",
            if o.member { "member" } else { "role" },
            o.id,
            o.allow,
            o.deny
        ));
        Ok(())
    }

    async fn create_thread(
        &self,
        channel: u64,
        name: &str,
        auto_archive_minutes: u16,
        _reason: &str,
    ) -> RestResult<ChannelInfo> {
        self.record(format!(
            "create_thread {channel} {name} {auto_archive_minutes}"
        ));
        let id = self.id();
        self.add_channel(id, name, ChannelKind::Thread, Some(channel));
        Ok(self.channel_unrecorded(id))
    }

    async fn pin(&self, channel: u64, message: u64, _reason: &str) -> RestResult<()> {
        self.record(format!("pin {channel} {message}"));
        Ok(())
    }
}

impl RecordingRest {
    fn channel_unrecorded(&self, id: u64) -> ChannelInfo {
        self.channels
            .lock()
            .unwrap()
            .iter()
            .find(|c| c.id == id)
            .cloned()
            .unwrap_or_default()
    }
}
