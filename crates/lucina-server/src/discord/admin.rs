//! `discord_admin_task`: Node `runDiscordAdminTask`, every operation of it,
//! with Node's names, defaults and texts, over [`DiscordRest`]. Owner only
//! (`LUCINA_OWNER_DISCORD_USER_ID`; Node let anyone in an admin channel run
//! them): the handler checks that before calling [`run`].
//!
//! Operations: `server_summary`, `create_role` (`makeAdmin`,
//! `assignToRequester`), `assign_role`, `create_category`,
//! `create_text_channel`, `create_voice_channel`, `create_invite` (24 h,
//! unlimited uses), `lock_channel` (Node's overwrites: `@everyone` may no
//! longer write or open threads, the requester and Lucina may), `create_thread`,
//! `pin_message` (the message replied to), and anything else, as in Node, the
//! support setup (`setup_support`, `grant_channel_access`: a category, its
//! text and voice channels, optional access for Poko). Nothing deletes,
//! kicks, bans or removes a role; `lock_channel` is the only operation that
//! takes a permission away, exactly as Node's did.
//!
//! Node read the guild from discord.js's cache; here the channels and roles
//! are read once per task over REST and the ones the task creates are added
//! to that list.

use lucina_discord::rest::{
    edit_overwrite, guild_permissions, perm, ChannelInfo, ChannelKind, DiscordRest, GuildInfo,
    NewChannel, RoleInfo,
};
use serde_json::Value;

pub const SKILL_NAME: &str = "Discord server administration";
pub const SKILL_INSTRUCTION: &str = "Tell the user what I changed in the Discord server. If Poko still needs to be invited, say that clearly and include the Poko invite link if useful.";

/// The message the task was asked in.
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    pub guild_id: Option<u64>,
    pub channel_id: u64,
    pub channel_name: Option<String>,
    pub channel_kind: ChannelKind,
    pub author_id: u64,
    pub author_tag: String,
    pub reference_message_id: Option<u64>,
    pub bot_id: u64,
}

/// Node's failure reply around a reason.
pub fn failure(persona: &str, reason: &str) -> String {
    format!(
        "{persona}: non sono riuscito ad aggiornare il server Discord: {}",
        reason.chars().take(1000).collect::<String>()
    )
}

/// Not the owner.
pub fn refusal(persona: &str) -> String {
    failure(
        persona,
        "only the owner can ask me to change a Discord server.",
    )
}

/// Node's `plan.ack` default.
pub fn default_ack(persona: &str) -> String {
    format!("Sono {persona}: aggiorno questo server Discord.")
}

/// Node `normalizeChannelName`.
pub fn normalize_channel_name(name: &str) -> String {
    let lower = name.trim().to_lowercase().replace(['\'', '’'], "");
    let mut out = String::new();
    let mut dash = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            dash = false;
        } else if !dash {
            out.push('-');
            dash = true;
        }
    }
    let out: String = out.trim_matches('-').chars().take(90).collect();
    if out.is_empty() {
        "support".into()
    } else {
        out
    }
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

fn channel_matches(c: &ChannelInfo, name: &str) -> bool {
    same_name(&c.name, &normalize_channel_name(name)) || same_name(&c.name, name)
}

fn s<'a>(plan: &'a Value, key: &str) -> Option<&'a str> {
    plan[key].as_str().map(str::trim).filter(|v| !v.is_empty())
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::Array(_) | Value::Object(_) => true,
        Value::Null => false,
    }
}

fn names(plan: &Value, key: &str) -> Vec<String> {
    plan[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Node `defaultTextChannelNames`.
fn text_channel_names(plan: &Value) -> Vec<String> {
    let n = names(plan, "textChannelNames");
    if n.is_empty() {
        vec!["support".into()]
    } else {
        n
    }
}

/// Node `defaultRoleName`.
fn role_name(plan: &Value) -> String {
    s(plan, "roleName").unwrap_or("Admin").to_owned()
}

fn lines(parts: &[String]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n")
}

fn list(label: &str, items: &[String]) -> String {
    if items.is_empty() {
        String::new()
    } else {
        format!("{label}: {}", items.join(", "))
    }
}

const WRITE: u64 = perm::SEND_MESSAGES
    | perm::CREATE_PUBLIC_THREADS
    | perm::CREATE_PRIVATE_THREADS
    | perm::SEND_MESSAGES_IN_THREADS;
const WRITER: u64 = perm::VIEW_CHANNEL | perm::READ_MESSAGE_HISTORY | WRITE;

/// One task's view of the guild.
struct Guild<'a> {
    rest: &'a dyn DiscordRest,
    info: GuildInfo,
    channels: Vec<ChannelInfo>,
    reason: String,
    created: Vec<String>,
    reused: Vec<String>,
}

impl Guild<'_> {
    fn server(&self) -> String {
        format!("Server: {}", self.info.name)
    }

    /// Node `findTextChannel`.
    fn find_text(&self, name: &str, parent: Option<u64>) -> Option<ChannelInfo> {
        if name.is_empty() {
            return None;
        }
        self.channels
            .iter()
            .find(|c| {
                c.kind == ChannelKind::Text
                    && (parent.is_none() || c.parent_id == parent)
                    && channel_matches(c, name)
            })
            .cloned()
    }

    /// Node `ensureCategory`.
    async fn ensure_category(&mut self, name: &str) -> Result<ChannelInfo, String> {
        let name = if name.is_empty() { "Support" } else { name };
        if let Some(c) = self
            .channels
            .iter()
            .find(|c| c.kind == ChannelKind::Category && same_name(&c.name, name))
            .cloned()
        {
            self.reused.push(format!("category \"{}\"", c.name));
            return Ok(c);
        }
        let c = self
            .rest
            .create_channel(
                self.info.id,
                &NewChannel {
                    name: name.to_owned(),
                    kind: ChannelKind::Category,
                    parent_id: None,
                    topic: None,
                },
                &self.reason,
            )
            .await
            .map_err(|e| e.to_string())?;
        self.created.push(format!("category \"{}\"", c.name));
        self.channels.push(c.clone());
        Ok(c)
    }

    /// Node `ensureTextChannel`.
    async fn ensure_text(
        &mut self,
        requested: &str,
        parent: Option<u64>,
        topic: Option<&str>,
    ) -> Result<ChannelInfo, String> {
        let name = normalize_channel_name(requested);
        if let Some(c) = self.find_text(&name, parent) {
            self.reused.push(format!("channel #{}", c.name));
            if let Some(t) = topic.filter(|t| !t.is_empty()) {
                if c.topic.as_deref() != Some(t) {
                    self.rest
                        .set_topic(c.id, t, &self.reason)
                        .await
                        .map_err(|e| e.to_string())?;
                    self.created.push(format!("updated topic for #{}", c.name));
                }
            }
            return Ok(c);
        }
        let c = self
            .rest
            .create_channel(
                self.info.id,
                &NewChannel {
                    name,
                    kind: ChannelKind::Text,
                    parent_id: parent,
                    topic: topic.filter(|t| !t.is_empty()).map(str::to_owned),
                },
                &self.reason,
            )
            .await
            .map_err(|e| e.to_string())?;
        self.created.push(format!("channel #{}", c.name));
        self.channels.push(c.clone());
        Ok(c)
    }

    /// Node `ensureVoiceChannel`.
    async fn ensure_voice(
        &mut self,
        requested: &str,
        parent: Option<u64>,
    ) -> Result<ChannelInfo, String> {
        let name = normalize_channel_name(requested);
        if let Some(c) = self
            .channels
            .iter()
            .find(|c| {
                c.kind == ChannelKind::Voice
                    && (parent.is_none() || c.parent_id == parent)
                    && channel_matches(c, &name)
            })
            .cloned()
        {
            self.reused.push(format!("voice channel {}", c.name));
            return Ok(c);
        }
        let c = self
            .rest
            .create_channel(
                self.info.id,
                &NewChannel {
                    name,
                    kind: ChannelKind::Voice,
                    parent_id: parent,
                    topic: None,
                },
                &self.reason,
            )
            .await
            .map_err(|e| e.to_string())?;
        self.created.push(format!("voice channel {}", c.name));
        self.channels.push(c.clone());
        Ok(c)
    }

    /// discord.js `permissionOverwrites.edit`: merged into the existing one.
    async fn edit_overwrite(
        &self,
        channel: &ChannelInfo,
        target: u64,
        member: bool,
        allow: u64,
        deny: u64,
    ) -> Result<(), String> {
        let existing = channel
            .overwrites
            .iter()
            .find(|o| o.id == target && o.member == member);
        let o = edit_overwrite(existing, target, member, allow, deny);
        self.rest
            .put_overwrite(channel.id, &o, &self.reason)
            .await
            .map_err(|e| e.to_string())
    }

    fn report(&self) -> String {
        lines(&[
            self.server(),
            list("Created", &self.created),
            list("Already existed", &self.reused),
        ])
    }
}

/// The task's text channel: the named one, else the current one when it is
/// a text channel.
fn target_text(g: &Guild<'_>, ctx: &Ctx, plan: &Value) -> Option<ChannelInfo> {
    let name = s(plan, "channelName")
        .map(str::to_owned)
        .or_else(|| ctx.channel_name.clone())
        .unwrap_or_default();
    g.find_text(&name, None).or_else(|| {
        (ctx.channel_kind == ChannelKind::Text)
            .then(|| g.channels.iter().find(|c| c.id == ctx.channel_id).cloned())
            .flatten()
    })
}

/// Run one operation; `Ok`: the skill result for the reply, `Err`: the
/// reason for Node's failure text.
pub async fn run(
    rest: &dyn DiscordRest,
    ctx: &Ctx,
    plan: &Value,
    poko_bot_id: Option<u64>,
) -> Result<String, String> {
    let Some(guild_id) = ctx.guild_id else {
        return Err("Discord admin actions can only run inside a server, not in DMs.".into());
    };
    let e = |e: lucina_discord::rest::RestError| e.to_string();
    let info = rest.guild(guild_id).await.map_err(e)?;
    let roles: Vec<RoleInfo> = rest.roles(guild_id).await.map_err(e)?;
    let me = rest.member(guild_id, ctx.bot_id).await.map_err(e)?;
    let perms = guild_permissions(&info, &me, &roles);
    let has = |p: u64| perms & p != 0;
    if !has(perm::ADMINISTRATOR) && !has(perm::MANAGE_CHANNELS) {
        return Err("I need Administrator or Manage Channels permission in this server.".into());
    }
    let who = if !ctx.author_tag.is_empty() {
        ctx.author_tag.clone()
    } else if ctx.author_id != 0 {
        ctx.author_id.to_string()
    } else {
        "user".into()
    };
    let channels = rest.guild_channels(guild_id).await.map_err(e)?;
    let mut g = Guild {
        rest,
        info,
        channels,
        reason: format!("Requested by {who} through Hermes"),
        created: Vec::new(),
        reused: Vec::new(),
    };
    let op = s(plan, "operation").unwrap_or("");

    match op {
        "server_summary" => {
            let count = |k: ChannelKind| g.channels.iter().filter(|c| c.kind == k).count();
            return Ok([
                g.server(),
                format!(
                    "Members: {}",
                    g.info
                        .member_count
                        .map_or("unknown".to_owned(), |n| n.to_string())
                ),
                format!("Categories: {}", count(ChannelKind::Category)),
                format!("Text channels: {}", count(ChannelKind::Text)),
                format!("Voice channels: {}", count(ChannelKind::Voice)),
                format!("Roles: {}", roles.len()),
            ]
            .join("\n"));
        }
        "create_role" => {
            if !has(perm::ADMINISTRATOR) && !has(perm::MANAGE_ROLES) {
                return Err(
                    "I need Administrator or Manage Roles permission to create roles.".into(),
                );
            }
            let name = role_name(plan);
            let make_admin = truthy(&plan["makeAdmin"]);
            let mut assigned = Vec::new();
            let role = match roles.iter().find(|r| same_name(&r.name, &name)) {
                Some(r) => {
                    g.reused.push(format!("role @{}", r.name));
                    if make_admin && r.permissions & perm::ADMINISTRATOR == 0 {
                        rest.set_role_permissions(
                            guild_id,
                            r.id,
                            r.permissions | perm::ADMINISTRATOR,
                            &g.reason,
                        )
                        .await
                        .map_err(e)?;
                        assigned.push(format!("updated @{} with Administrator permission", r.name));
                    }
                    r.clone()
                }
                None => {
                    let r = rest
                        .create_role(
                            guild_id,
                            &name,
                            if make_admin { perm::ADMINISTRATOR } else { 0 },
                            &g.reason,
                        )
                        .await
                        .map_err(e)?;
                    g.created.push(format!(
                        "role @{}{}",
                        r.name,
                        if make_admin {
                            " with Administrator permission"
                        } else {
                            ""
                        }
                    ));
                    r
                }
            };
            if truthy(&plan["assignToRequester"]) {
                let requester = rest.member(guild_id, ctx.author_id).await.map_err(e)?;
                rest.add_member_role(guild_id, ctx.author_id, role.id, &g.reason)
                    .await
                    .map_err(e)?;
                assigned.push(format!(
                    "assigned @{} to {}",
                    role.name, requester.display_name
                ));
            }
            return Ok(lines(&[
                g.server(),
                list("Created", &g.created),
                list("Already existed", &g.reused),
                list("Updated", &assigned),
            ]));
        }
        "assign_role" => {
            if !has(perm::ADMINISTRATOR) && !has(perm::MANAGE_ROLES) {
                return Err(
                    "I need Administrator or Manage Roles permission to assign roles.".into(),
                );
            }
            let name = role_name(plan);
            let Some(role) = roles.iter().find(|r| same_name(&r.name, &name)) else {
                return Err(format!("Role \"{name}\" does not exist yet."));
            };
            let requester = rest.member(guild_id, ctx.author_id).await.map_err(e)?;
            rest.add_member_role(guild_id, ctx.author_id, role.id, &g.reason)
                .await
                .map_err(e)?;
            return Ok(format!(
                "{}\nUpdated: assigned @{} to {}",
                g.server(),
                role.name,
                requester.display_name
            ));
        }
        "create_category" => {
            let name = s(plan, "categoryName")
                .or_else(|| s(plan, "channelName"))
                .unwrap_or("new-category")
                .to_owned();
            g.ensure_category(&name).await?;
            return Ok(g.report());
        }
        "create_text_channel" => {
            let category = match s(plan, "categoryName") {
                Some(c) => Some(g.ensure_category(c).await?),
                None => None,
            };
            let name = s(plan, "channelName")
                .map(str::to_owned)
                .unwrap_or_else(|| text_channel_names(plan)[0].clone());
            g.ensure_text(&name, category.map(|c| c.id), s(plan, "topic"))
                .await?;
            return Ok(g.report());
        }
        "create_voice_channel" => {
            let category = match s(plan, "categoryName") {
                Some(c) => Some(g.ensure_category(c).await?),
                None => None,
            };
            let name = s(plan, "channelName")
                .map(str::to_owned)
                .or_else(|| names(plan, "voiceChannelNames").into_iter().next())
                .unwrap_or_else(|| "voice".into());
            g.ensure_voice(&name, category.map(|c| c.id)).await?;
            return Ok(g.report());
        }
        "create_invite" => {
            let Some(channel) = target_text(&g, ctx, plan) else {
                return Err("I need a text channel to create an invite.".into());
            };
            let url = rest
                .create_invite(channel.id, 86_400, 0, true, &g.reason)
                .await
                .map_err(e)?;
            return Ok(format!(
                "{}\nCreated invite for #{}: {url}",
                g.server(),
                channel.name
            ));
        }
        "lock_channel" => {
            if !has(perm::ADMINISTRATOR) && !has(perm::MANAGE_CHANNELS) {
                return Err(
                    "I need Administrator or Manage Channels permission to lock channels.".into(),
                );
            }
            let Some(channel) = target_text(&g, ctx, plan) else {
                return Err("I need a text channel to lock.".into());
            };
            let requester = rest.member(guild_id, ctx.author_id).await.map_err(e)?;
            // @everyone is the role whose id is the guild id.
            g.edit_overwrite(&channel, guild_id, false, 0, WRITE)
                .await?;
            g.edit_overwrite(&channel, requester.user_id, true, WRITER, 0)
                .await?;
            g.edit_overwrite(&channel, me.user_id, true, WRITER, 0)
                .await?;
            return Ok([
                g.server(),
                format!(
                    "Updated: locked #{} so @everyone cannot write there.",
                    channel.name
                ),
                format!(
                    "Allowed: {} and {} can write there.",
                    requester.display_name, me.display_name
                ),
            ]
            .join("\n"));
        }
        "create_thread" => {
            let Some(channel) = target_text(&g, ctx, plan) else {
                return Err("I need a text channel to create a thread.".into());
            };
            let name = s(plan, "topic")
                .or_else(|| s(plan, "channelName"))
                .unwrap_or("discussion");
            let thread = rest
                .create_thread(channel.id, name, 1440, &g.reason)
                .await
                .map_err(e)?;
            return Ok(format!(
                "{}\nCreated thread \"{}\" in #{}.",
                g.server(),
                thread.name,
                channel.name
            ));
        }
        "pin_message" => {
            let Some(target) = ctx.reference_message_id else {
                return Err(
                    "Reply to the message you want me to pin, then ask me to pin it.".into(),
                );
            };
            rest.pin(ctx.channel_id, target, &g.reason)
                .await
                .map_err(e)?;
            return Ok(format!(
                "{}\nPinned the referenced message in #{}.",
                g.server(),
                ctx.channel_name.as_deref().unwrap_or("")
            ));
        }
        _ => {}
    }

    // Node's fallthrough: the support setup.
    let category = g
        .ensure_category(s(plan, "categoryName").unwrap_or("Support"))
        .await?;
    let mut text_channels = Vec::new();
    for name in text_channel_names(plan) {
        text_channels.push(
            g.ensure_text(&name, Some(category.id), s(plan, "topic"))
                .await?,
        );
    }
    for name in names(plan, "voiceChannelNames") {
        g.ensure_voice(&name, Some(category.id)).await?;
    }
    let mut notes = Vec::new();
    if truthy(&plan["includePoko"]) || s(plan, "targetBot").is_some() {
        let target = s(plan, "targetBot").unwrap_or("Poko");
        // Node `fetchBotMemberByName`: only Poko is known, by its token's id.
        let member = match poko_bot_id.filter(|_| target.eq_ignore_ascii_case("poko")) {
            Some(id) => rest.member(guild_id, id).await.ok(),
            None => None,
        };
        match member {
            Some(m) => {
                for c in &text_channels {
                    g.edit_overwrite(
                        c,
                        m.user_id,
                        true,
                        perm::VIEW_CHANNEL | perm::SEND_MESSAGES | perm::READ_MESSAGE_HISTORY,
                        0,
                    )
                    .await?;
                }
                notes.push(format!(
                    "{} has access to {}.",
                    m.display_name,
                    text_channels
                        .iter()
                        .map(|c| format!("#{}", c.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            None => notes.push(format!(
                "{target} is not in this server yet. Invite it, then ask me to add access again."
            )),
        }
    }
    Ok(lines(&[
        g.server(),
        list("Created", &g.created),
        list("Already existed", &g.reused),
        notes.join(" "),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::testing::RecordingRest;
    use serde_json::json;

    const GUILD: u64 = 100;
    const BOT: u64 = 900;
    const OWNER: u64 = 42;

    fn ctx() -> Ctx {
        Ctx {
            guild_id: Some(GUILD),
            channel_id: 11,
            channel_name: Some("admin".into()),
            channel_kind: ChannelKind::Text,
            author_id: OWNER,
            author_tag: "giuseppe".into(),
            reference_message_id: None,
            bot_id: BOT,
        }
    }

    fn rest() -> RecordingRest {
        let r = RecordingRest::new(GUILD, "Casa", 1);
        r.add_role(GUILD, "@everyone", perm::VIEW_CHANNEL);
        r.add_role(500, "Bots", perm::MANAGE_CHANNELS | perm::MANAGE_ROLES);
        r.add_member(BOT, "Lucina", vec![500]);
        r.add_member(OWNER, "Giuseppe", vec![]);
        r.add_channel(10, "Ops", ChannelKind::Category, None);
        r.add_channel(11, "admin", ChannelKind::Text, Some(10));
        r
    }

    #[test]
    fn channel_names_as_node() {
        assert_eq!(normalize_channel_name("Help Desk!"), "help-desk");
        assert_eq!(normalize_channel_name("  L'aiuto’s  "), "laiutos");
        assert_eq!(normalize_channel_name("!!!"), "support");
        assert_eq!(normalize_channel_name(&"a".repeat(120)).len(), 90);
        assert_eq!(normalize_channel_name("caffè"), "caff");
    }

    #[tokio::test]
    async fn dms_and_missing_permissions_are_refused_without_calls() {
        let r = rest();
        let mut c = ctx();
        c.guild_id = None;
        let err = run(&r, &c, &json!({"operation": "server_summary"}), None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            "Discord admin actions can only run inside a server, not in DMs."
        );
        assert!(r.calls().is_empty());
        let r = RecordingRest::new(GUILD, "Casa", 1);
        r.add_role(GUILD, "@everyone", 0);
        r.add_member(BOT, "Lucina", vec![]);
        let err = run(&r, &ctx(), &json!({"operation": "server_summary"}), None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            "I need Administrator or Manage Channels permission in this server."
        );
        assert!(r.writes().is_empty());
    }

    #[tokio::test]
    async fn summary_text_channel_and_category() {
        let r = rest();
        let out = run(&r, &ctx(), &json!({"operation": "server_summary"}), None)
            .await
            .unwrap();
        assert_eq!(
            out,
            "Server: Casa\nMembers: 2\nCategories: 1\nText channels: 1\nVoice channels: 0\nRoles: 2"
        );
        assert!(r.writes().is_empty());

        let out = run(
            &r,
            &ctx(),
            &json!({"operation": "create_text_channel", "channelName": "Help Desk", "categoryName": "Ops", "topic": "Ask here"}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            out,
            "Server: Casa\nCreated: channel #help-desk\nAlready existed: category \"Ops\""
        );
        assert_eq!(
            r.writes(),
            ["create_channel Text help-desk parent=10 topic=Ask here reason=Requested by giuseppe through Hermes"]
        );
        // Again: reused, nothing written.
        let out = run(
            &r,
            &ctx(),
            &json!({"operation": "create_text_channel", "channelName": "help-desk", "categoryName": "Ops"}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            out,
            "Server: Casa\nAlready existed: category \"Ops\", channel #help-desk"
        );
        assert_eq!(r.writes().len(), 1);
        let out = run(&r, &ctx(), &json!({"operation": "create_category"}), None)
            .await
            .unwrap();
        assert_eq!(out, "Server: Casa\nCreated: category \"new-category\"");
    }

    #[tokio::test]
    async fn roles_lock_thread_invite_pin() {
        let r = rest();
        let out = run(
            &r,
            &ctx(),
            &json!({"operation": "create_role", "roleName": "Crew", "assignToRequester": true}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            out,
            "Server: Casa\nCreated: role @Crew\nUpdated: assigned @Crew to Giuseppe"
        );
        let err = run(
            &r,
            &ctx(),
            &json!({"operation": "assign_role", "roleName": "Nope"}),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err, "Role \"Nope\" does not exist yet.");

        let out = run(&r, &ctx(), &json!({"operation": "lock_channel"}), None)
            .await
            .unwrap();
        assert_eq!(out, "Server: Casa\nUpdated: locked #admin so @everyone cannot write there.\nAllowed: Giuseppe and Lucina can write there.");
        let w = r.writes();
        let locks: Vec<&String> = w
            .iter()
            .filter(|c| c.starts_with("put_overwrite"))
            .collect();
        assert_eq!(locks.len(), 3, "{w:?}");
        assert_eq!(
            locks[0],
            &format!("put_overwrite 11 role 100 allow=0 deny={WRITE}")
        );
        assert_eq!(
            locks[1],
            &format!("put_overwrite 11 member 42 allow={WRITER} deny=0")
        );

        let out = run(
            &r,
            &ctx(),
            &json!({"operation": "create_thread", "topic": "Idee"}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(out, "Server: Casa\nCreated thread \"Idee\" in #admin.");
        let out = run(&r, &ctx(), &json!({"operation": "create_invite"}), None)
            .await
            .unwrap();
        assert_eq!(
            out,
            "Server: Casa\nCreated invite for #admin: https://discord.gg/stub11"
        );
        let err = run(&r, &ctx(), &json!({"operation": "pin_message"}), None)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            "Reply to the message you want me to pin, then ask me to pin it."
        );
        let mut c = ctx();
        c.reference_message_id = Some(777);
        let out = run(&r, &c, &json!({"operation": "pin_message"}), None)
            .await
            .unwrap();
        assert_eq!(
            out,
            "Server: Casa\nPinned the referenced message in #admin."
        );
        // No call that deletes, kicks, bans or removes a role exists at all.
        assert!(r
            .calls()
            .iter()
            .all(|c| !c.contains("delete") && !c.contains("remove") && !c.contains("ban")));
    }

    #[tokio::test]
    async fn the_support_setup_is_the_fallthrough_and_poko_access_is_optional() {
        let r = rest();
        r.add_member(800, "Poko", vec![]);
        let out = run(
            &r,
            &ctx(),
            &json!({"operation": "setup_support", "voiceChannelNames": ["Voce"], "includePoko": true}),
            Some(800),
        )
        .await
        .unwrap();
        assert_eq!(out, "Server: Casa\nCreated: category \"Support\", channel #support, voice channel voce\nPoko has access to #support.");
        let out = run(
            &r,
            &ctx(),
            &json!({"operation": "grant_channel_access", "targetBot": "Other"}),
            Some(800),
        )
        .await
        .unwrap();
        assert_eq!(out, "Server: Casa\nAlready existed: category \"Support\", channel #support\nOther is not in this server yet. Invite it, then ask me to add access again.");
    }
}
