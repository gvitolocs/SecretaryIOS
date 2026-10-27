//! Which Discord messages Lucina answers: the Lucina persona path of the Node
//! `src/discord.js`, as pure functions.
//!
//! In Node's order, per `MESSAGE_CREATE`:
//!
//! 1. bot authors (Lucina herself, Poko, any other bot) are ignored. Node
//!    checked bots only after the admin-channel rule, so a bot mentioning
//!    Lucina outside an admin channel got the "disabled" answer; two bots can
//!    loop on that, so here bots never get anything.
//! 2. outside an admin channel ([`is_admin_channel`]: DMs,
//!    `DISCORD_ADMIN_CHANNEL_IDS`, or a channel / parent name equal to or
//!    containing one of `DISCORD_ADMIN_CHANNEL_NAMES`) a mention gets
//!    [`DISABLED_TEXT`] and nothing else; anything else is silence.
//! 3. the allow-lists ([`is_allowed`]: `DISCORD_ALLOWED_CHANNEL_IDS`,
//!    `DISCORD_ALLOWED_USER_IDS`, empty: everyone), then [`should_reply`]
//!    (a DM, or a mention).
//! 4. the text without Lucina's mention ([`cleaned_text`]); empty: silence.

/// Node's answer to a mention outside an admin channel.
pub const DISABLED_TEXT: &str = "I'm currently disabled, ask Poko instead ✨";
/// Node's default `DISCORD_ADMIN_CHANNEL_NAMES`.
pub const DEFAULT_ADMIN_CHANNEL_NAMES: [&str; 5] =
    ["admin", "admin-chat", "operator", "operators", "ops"];
/// Discord's message length limit.
pub const MESSAGE_LIMIT: usize = 2000;

fn csv(v: Option<String>) -> Vec<String> {
    v.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Node's `config.discord` lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoutingConfig {
    pub admin_channel_ids: Vec<String>,
    pub admin_channel_names: Vec<String>,
    pub allowed_channel_ids: Vec<String>,
    pub allowed_user_ids: Vec<String>,
}

impl RoutingConfig {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>) -> Self {
        let names = csv(get("DISCORD_ADMIN_CHANNEL_NAMES"));
        Self {
            admin_channel_ids: csv(get("DISCORD_ADMIN_CHANNEL_IDS")),
            admin_channel_names: if names.is_empty() {
                DEFAULT_ADMIN_CHANNEL_NAMES.map(str::to_owned).to_vec()
            } else {
                names
            },
            allowed_channel_ids: csv(get("DISCORD_ALLOWED_CHANNEL_IDS")),
            allowed_user_ids: csv(get("DISCORD_ALLOWED_USER_IDS")),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboundAttachment {
    pub name: String,
    pub content_type: Option<String>,
}

/// The parts of a `MESSAGE_CREATE` the handler looks at, with the channel
/// and guild names from the session's cache.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inbound {
    pub message_id: u64,
    pub channel_id: u64,
    /// `None`: a DM.
    pub guild_id: Option<u64>,
    pub author_id: u64,
    pub author_bot: bool,
    pub author_username: String,
    pub author_global_name: Option<String>,
    pub author_discriminator: u16,
    pub content: String,
    /// The users mentioned (a reply that pings its author included).
    pub mentions: Vec<u64>,
    /// The message this one replies to.
    pub reference_message_id: Option<u64>,
    pub attachments: Vec<InboundAttachment>,
    /// Milliseconds since the epoch.
    pub timestamp_ms: Option<i64>,
    pub guild_name: Option<String>,
    pub channel_name: Option<String>,
    /// The parent's name: a channel's category, a thread's channel.
    pub parent_name: Option<String>,
    pub channel_kind: crate::rest::ChannelKind,
    /// Lucina's own user id.
    pub bot_id: u64,
}

impl Inbound {
    pub fn is_dm(&self) -> bool {
        self.guild_id.is_none()
    }

    /// discord.js `message.mentions.has(client.user)` for a user mention.
    pub fn mentions_bot(&self) -> bool {
        self.bot_id != 0
            && (self.mentions.contains(&self.bot_id)
                || self.content.contains(&format!("<@{}>", self.bot_id))
                || self.content.contains(&format!("<@!{}>", self.bot_id)))
    }

    /// discord.js `user.tag`: the username, with `#1234` for legacy names.
    pub fn author_tag(&self) -> String {
        if self.author_discriminator == 0 {
            self.author_username.clone()
        } else {
            format!("{}#{:04}", self.author_username, self.author_discriminator)
        }
    }

    /// Node `userLabelFromMessage`: global name, `@tag`, id.
    pub fn user_label(&self) -> String {
        let tag = self.author_tag();
        let tag = if tag.is_empty() {
            String::new()
        } else {
            format!("@{tag}")
        };
        [
            self.author_global_name.clone().unwrap_or_default(),
            tag,
            self.author_id.to_string(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
    }
}

/// Node `isAdminChannel`.
pub fn is_admin_channel(cfg: &RoutingConfig, m: &Inbound) -> bool {
    if m.is_dm() {
        return true;
    }
    if cfg.admin_channel_ids.contains(&m.channel_id.to_string()) {
        return true;
    }
    let names = [
        m.channel_name.as_deref().unwrap_or("").to_lowercase(),
        m.parent_name.as_deref().unwrap_or("").to_lowercase(),
    ];
    let allowed: Vec<String> = cfg
        .admin_channel_names
        .iter()
        .map(|n| n.to_lowercase())
        .filter(|n| !n.is_empty())
        .collect();
    names.iter().any(|name| {
        allowed
            .iter()
            .any(|a| name == a || name.contains(a.as_str()))
    })
}

/// Node `isAllowedMessage`.
pub fn is_allowed(cfg: &RoutingConfig, m: &Inbound) -> bool {
    if m.author_bot {
        return false;
    }
    let channel_ok = cfg.allowed_channel_ids.is_empty()
        || cfg.allowed_channel_ids.contains(&m.channel_id.to_string());
    let user_ok =
        cfg.allowed_user_ids.is_empty() || cfg.allowed_user_ids.contains(&m.author_id.to_string());
    channel_ok && user_ok
}

/// Node `shouldReplyToMessage`.
pub fn should_reply(m: &Inbound) -> bool {
    m.is_dm() || m.mentions_bot()
}

/// Node `cleanedMessageText`: without `<@bot>` / `<@!bot>`, trimmed.
pub fn cleaned_text(content: &str, bot_id: u64) -> String {
    content
        .replace(&format!("<@!{bot_id}>"), "")
        .replace(&format!("<@{bot_id}>"), "")
        .trim()
        .to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// No answer; the reason is for the log.
    Ignore(&'static str),
    /// Answer [`DISABLED_TEXT`] (a mention outside the admin channels).
    Disabled,
    /// A Lucina turn with this text.
    Turn(String),
}

pub fn route(cfg: &RoutingConfig, m: &Inbound) -> Route {
    if m.author_bot {
        return Route::Ignore("bot author");
    }
    if !is_admin_channel(cfg, m) {
        return if m.mentions_bot() {
            Route::Disabled
        } else {
            Route::Ignore("not an admin channel")
        };
    }
    if !is_allowed(cfg, m) {
        return Route::Ignore("not allowed");
    }
    if !should_reply(m) {
        return Route::Ignore("not addressed");
    }
    let text = cleaned_text(&m.content, m.bot_id);
    if text.is_empty() {
        return Route::Ignore("empty");
    }
    Route::Turn(text)
}

/// Split a reply into Discord messages of at most `limit` characters, at a
/// line break when there is one in the second half of the window, else at a
/// space, else hard.
pub fn split_message(text: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(1);
    let mut out = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        let chars: Vec<(usize, char)> = rest.char_indices().take(limit + 1).collect();
        if chars.len() <= limit {
            out.push(rest.to_owned());
            break;
        }
        // Byte offset just past the `limit`-th character.
        let window_end = chars[limit].0;
        let window = &rest[..window_end];
        let half = chars[limit / 2].0;
        let cut = window
            .rfind('\n')
            .filter(|i| *i >= half)
            .or_else(|| window.rfind(' ').filter(|i| *i >= half))
            .unwrap_or(window_end);
        let (head, tail) = rest.split_at(cut);
        let head = head.trim_end();
        if !head.is_empty() {
            out.push(head.to_owned());
        }
        rest = tail.trim_start();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOT: u64 = 900;

    fn cfg() -> RoutingConfig {
        RoutingConfig::from_lookup(&|_: &str| None)
    }

    fn guild_msg(channel: &str, parent: Option<&str>, content: &str) -> Inbound {
        Inbound {
            message_id: 1,
            channel_id: 10,
            guild_id: Some(5),
            author_id: 77,
            author_username: "pino".into(),
            content: content.into(),
            mentions: if content.contains(&format!("<@{BOT}>")) {
                vec![BOT]
            } else {
                vec![]
            },
            channel_name: Some(channel.into()),
            parent_name: parent.map(Into::into),
            bot_id: BOT,
            ..Inbound::default()
        }
    }

    fn dm(content: &str) -> Inbound {
        Inbound {
            guild_id: None,
            channel_name: None,
            ..guild_msg("", None, content)
        }
    }

    #[test]
    fn config_names_and_defaults() {
        let c = cfg();
        assert_eq!(c.admin_channel_names, DEFAULT_ADMIN_CHANNEL_NAMES);
        assert!(c.admin_channel_ids.is_empty() && c.allowed_user_ids.is_empty());
        let c = RoutingConfig::from_lookup(&|k: &str| match k {
            "DISCORD_ADMIN_CHANNEL_NAMES" => Some("Staff, lucina".into()),
            "DISCORD_ADMIN_CHANNEL_IDS" => Some("10,11".into()),
            "DISCORD_ALLOWED_CHANNEL_IDS" => Some("10".into()),
            "DISCORD_ALLOWED_USER_IDS" => Some(" 77 ".into()),
            _ => None,
        });
        assert_eq!(c.admin_channel_names, ["Staff", "lucina"]);
        assert_eq!(c.admin_channel_ids, ["10", "11"]);
        assert_eq!(c.allowed_channel_ids, ["10"]);
        assert_eq!(c.allowed_user_ids, ["77"]);
    }

    #[test]
    fn dms_are_admin_channels_and_need_no_mention() {
        assert_eq!(route(&cfg(), &dm("ciao")), Route::Turn("ciao".into()));
        assert_eq!(route(&cfg(), &dm("   ")), Route::Ignore("empty"));
    }

    #[test]
    fn admin_channel_by_id_and_by_name_or_parent_substring() {
        let c = RoutingConfig {
            admin_channel_ids: vec!["10".into()],
            ..cfg()
        };
        // By id: any name.
        assert!(is_admin_channel(&c, &guild_msg("general", None, "x")));
        let c = cfg();
        assert!(is_admin_channel(&c, &guild_msg("admin", None, "x")));
        assert!(is_admin_channel(&c, &guild_msg("Admin-Chat", None, "x")));
        // Substring: "lucina-ops" contains "ops".
        assert!(is_admin_channel(&c, &guild_msg("lucina-ops", None, "x")));
        // The parent (category) name counts.
        assert!(is_admin_channel(
            &c,
            &guild_msg("general", Some("Operators"), "x")
        ));
        assert!(!is_admin_channel(
            &c,
            &guild_msg("general", Some("Chat"), "x")
        ));
        assert!(!is_admin_channel(&c, &guild_msg("general", None, "x")));
    }

    #[test]
    fn a_mention_outside_admin_gets_the_disabled_text_and_nothing_else_is_silence() {
        let c = cfg();
        assert_eq!(
            route(&c, &guild_msg("general", None, "<@900> ciao")),
            Route::Disabled
        );
        assert_eq!(
            route(&c, &guild_msg("general", None, "ciao a tutti")),
            Route::Ignore("not an admin channel")
        );
        // In an admin channel: the turn, without the mention.
        assert_eq!(
            route(&c, &guild_msg("admin", None, "<@900> stato?")),
            Route::Turn("stato?".into())
        );
        assert_eq!(
            route(&c, &guild_msg("admin", None, "<@!900>stato?")),
            Route::Turn("stato?".into())
        );
        // Not addressed in an admin channel: silence.
        assert_eq!(
            route(&c, &guild_msg("admin", None, "stato?")),
            Route::Ignore("not addressed")
        );
        // Only the mention: nothing to answer.
        assert_eq!(
            route(&c, &guild_msg("admin", None, "<@900>")),
            Route::Ignore("empty")
        );
        assert_eq!(DISABLED_TEXT, "I'm currently disabled, ask Poko instead ✨");
    }

    #[test]
    fn bot_authors_are_ignored_everywhere() {
        let c = cfg();
        for mut m in [
            dm("ciao"),
            guild_msg("admin", None, "<@900> ciao"),
            guild_msg("general", None, "<@900> ciao"),
        ] {
            m.author_bot = true;
            assert_eq!(route(&c, &m), Route::Ignore("bot author"));
        }
    }

    #[test]
    fn allow_lists_by_channel_and_by_user() {
        let c = RoutingConfig {
            allowed_user_ids: vec!["42".into()],
            ..cfg()
        };
        assert_eq!(route(&c, &dm("ciao")), Route::Ignore("not allowed"));
        let mut m = dm("ciao");
        m.author_id = 42;
        assert_eq!(route(&c, &m), Route::Turn("ciao".into()));
        let c = RoutingConfig {
            allowed_channel_ids: vec!["11".into()],
            ..cfg()
        };
        assert_eq!(
            route(&c, &guild_msg("admin", None, "<@900> x")),
            Route::Ignore("not allowed")
        );
        let mut m = guild_msg("admin", None, "<@900> x");
        m.channel_id = 11;
        assert_eq!(route(&c, &m), Route::Turn("x".into()));
        // The disabled answer comes before the allow-lists (Node's order).
        assert_eq!(
            route(&c, &guild_msg("general", None, "<@900> x")),
            Route::Disabled
        );
    }

    #[test]
    fn labels_and_tags() {
        let mut m = dm("x");
        m.author_global_name = Some("Giuseppe".into());
        assert_eq!(m.user_label(), "Giuseppe @pino 77");
        m.author_discriminator = 7;
        assert_eq!(m.author_tag(), "pino#0007");
        m.author_global_name = None;
        assert_eq!(m.user_label(), "@pino#0007 77");
    }

    #[test]
    fn splitting_at_2000() {
        assert_eq!(split_message("ciao", MESSAGE_LIMIT), ["ciao"]);
        assert!(split_message("  ", MESSAGE_LIMIT).is_empty());
        let exact = "a".repeat(2000);
        assert_eq!(split_message(&exact, MESSAGE_LIMIT), [exact]);
        // Hard split of one long word.
        let long = "b".repeat(4500);
        let parts = split_message(&long, MESSAGE_LIMIT);
        assert_eq!(
            parts.iter().map(|p| p.chars().count()).collect::<Vec<_>>(),
            [2000, 2000, 500]
        );
        // At a line break, characters (not bytes) counted.
        let para = format!("{}\n{}", "è".repeat(1500), "ü".repeat(1500));
        let parts = split_message(&para, MESSAGE_LIMIT);
        assert_eq!(parts, ["è".repeat(1500), "ü".repeat(1500)]);
        // At a space.
        let words = "parola ".repeat(600);
        let parts = split_message(&words, MESSAGE_LIMIT);
        assert!(parts.iter().all(|p| p.chars().count() <= MESSAGE_LIMIT));
        assert!(parts
            .iter()
            .all(|p| !p.starts_with(' ') && !p.ends_with(' ')));
        assert_eq!(
            parts.join(" ").split_whitespace().count(),
            words.split_whitespace().count()
        );
    }
}
