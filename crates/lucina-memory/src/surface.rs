//! Memory surface classes: every turn is classified once by the surface it
//! arrives on, each class has a fixed read/write policy for personal memory,
//! and anything that cannot be proven fails closed ([`Surface::Unknown`]).
//!
//! The idea comes from oh-my-hermes' memory model (MIT); this is our own
//! code. Policy table and trust chain: `docs/lucina-rust/MEMORY-SURFACES.md`.
//! Verus kernel: `verification/verus/memory_surfaces.rs`; differential test:
//! `tests/verus_surfaces.rs`.
//!
//! - [`classify`]: pure, from the turn's inputs ([`TurnFacts`]) and the
//!   owner's configured ids ([`OwnerIds`]) to a [`Surface`].
//! - [`may_read`] / [`may_write`]: the policy, per [`Scope`].
//! - [`le`]: "at most as permissive as"; the policy is monotone under it.
//! - [`TurnSurface`]: the classification of one turn, built only by
//!   classifying, passed to every memory read and write, and only ever
//!   tightened.

/// The channel a turn arrives on (`timeline.jsonl` channel names).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelKind {
    Web,
    Ios,
    Telegram,
    Discord,
    Alexa,
    /// Anything else, including a missing channel.
    Other,
}

impl ChannelKind {
    /// Exact names only (after trimming): an unknown name is [`Self::Other`].
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "web" => Self::Web,
            "ios" => Self::Ios,
            "telegram" => Self::Telegram,
            "discord" => Self::Discord,
            "alexa" => Self::Alexa,
            _ => Self::Other,
        }
    }
}

/// Internal code that runs without a user turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundOrigin {
    /// The proactive engine (`lucina-proactive`).
    Proactive,
    /// The reminder scheduler.
    Scheduler,
    /// Any other timer or cron job.
    Cron,
}

impl BackgroundOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proactive => "proactive",
            Self::Scheduler => "scheduler",
            Self::Cron => "cron",
        }
    }
}

/// The owner's ids on each channel (from config). A missing or empty id
/// never matches anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnerIds {
    /// The owner's 1:1 Telegram chat (`LUCINA_OPERATOR_CHAT_ID`).
    pub telegram_chat_id: Option<String>,
    /// `LUCINA_OWNER_DISCORD_USER_ID`.
    pub discord_user_id: Option<String>,
    /// The Alexa account linked to the owner (not configured yet).
    pub alexa_user_id: Option<String>,
}

/// Telegram `chat.type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelegramChatKind {
    Private,
    Group,
    Supergroup,
    Channel,
    Other,
}

impl TelegramChatKind {
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "private" => Self::Private,
            "group" => Self::Group,
            "supergroup" => Self::Supergroup,
            "channel" => Self::Channel,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramFacts {
    pub chat_id: Option<String>,
    pub chat_kind: TelegramChatKind,
    pub from_user_id: Option<String>,
}

/// A Discord message: `guild_id` `None` is a DM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordFacts {
    pub author_id: Option<String>,
    pub guild_id: Option<String>,
    pub channel_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlexaFacts {
    pub linked_user_id: Option<String>,
}

/// Everything [`classify`] looks at. `Default` is the most restrictive
/// input (channel `Other`, nothing verified): it classifies as `Unknown`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFacts {
    pub channel: ChannelKind,
    /// The request passed the Secretary API bearer check: only
    /// secretary-server (which authenticates the owner's web session or
    /// device token) holds that token.
    pub bearer_verified: bool,
    /// Lucina MCP markers (session `mcp-<caller>` or the
    /// `[via Lucina MCP, from <caller>]` prefix) and the caller they name.
    pub mcp_caller: Option<String>,
    pub telegram: Option<TelegramFacts>,
    pub discord: Option<DiscordFacts>,
    pub alexa: Option<AlexaFacts>,
    /// Set only by internal code that runs without a user turn.
    pub background: Option<BackgroundOrigin>,
    /// Background only: the output goes to the owner alone.
    pub owner_addressed: bool,
}

impl Default for TurnFacts {
    fn default() -> Self {
        Self {
            channel: ChannelKind::Other,
            bearer_verified: false,
            mcp_caller: None,
            telegram: None,
            discord: None,
            alexa: None,
            background: None,
            owner_addressed: false,
        }
    }
}

impl TurnFacts {
    /// A Secretary API request (`web` | `ios`, after the bearer check).
    pub fn secretary(channel: &str, bearer_verified: bool, mcp_caller: Option<String>) -> Self {
        Self {
            channel: ChannelKind::parse(channel),
            bearer_verified,
            mcp_caller,
            ..Self::default()
        }
    }

    /// Internal background work.
    pub fn background(origin: BackgroundOrigin, owner_addressed: bool) -> Self {
        Self {
            background: Some(origin),
            owner_addressed,
            ..Self::default()
        }
    }
}

/// The class of a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Surface {
    /// The owner himself: the authenticated Secretary app (web/iOS), his 1:1
    /// Telegram chat, his Discord DM, Alexa linked to him.
    OwnerDirect,
    /// An MCP client acting for the owner (valid Lucina MCP token upstream).
    OwnerDelegate { caller: String },
    /// 1:1 with someone who is not the owner (`telegram:<id>`, `discord:<id>`, `alexa:<id>`).
    Direct { person: String },
    /// A group chat or guild channel (`telegram:<chat>`, `discord:<channel>`).
    Channel { channel: String },
    /// Internal work without a user turn.
    Background {
        origin: BackgroundOrigin,
        owner_addressed: bool,
    },
    /// Nothing could be proven: no personal memory, no writes.
    Unknown,
}

impl Surface {
    /// For logs (no ids).
    pub fn class_name(&self) -> &'static str {
        match self {
            Self::OwnerDirect => "owner_direct",
            Self::OwnerDelegate { .. } => "owner_delegate",
            Self::Direct { .. } => "direct",
            Self::Channel { .. } => "channel",
            Self::Background { .. } => "background",
            Self::Unknown => "unknown",
        }
    }

    pub fn is_owner(&self) -> bool {
        matches!(self, Self::OwnerDirect | Self::OwnerDelegate { .. })
    }

    /// The MCP caller of an [`Self::OwnerDelegate`] turn.
    pub fn caller(&self) -> Option<&str> {
        match self {
            Self::OwnerDelegate { caller } => Some(caller),
            _ => None,
        }
    }
}

/// Whose records a read or write touches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The owner's personal memory (Honcho owner peer, his notes, calendar,
    /// tasks, reminders, recent turns, presence, shopping).
    OwnerPersonal,
    /// Records bound to one other person (same id form as [`Surface::Direct`]).
    Person(String),
    /// Records bound to one group chat / channel (reserved: no Rust surface
    /// reads channel memory yet).
    Channel(String),
}

impl Scope {
    /// For logs (no ids).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::OwnerPersonal => "owner_personal",
            Self::Person(_) => "person",
            Self::Channel(_) => "channel",
        }
    }
}

/// A trimmed, non-empty id.
fn id(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// Both present, non-empty and equal.
fn same(fact: &Option<String>, owner: &Option<String>) -> bool {
    matches!((id(fact), id(owner)), (Some(a), Some(b)) if a == b)
}

/// The surface of a turn (first matching rule wins; see the module doc).
pub fn classify(f: &TurnFacts, owner: &OwnerIds) -> Surface {
    let platform_facts = f.telegram.is_some() || f.discord.is_some() || f.alexa.is_some();
    if let Some(origin) = f.background {
        if f.mcp_caller.is_some() || platform_facts {
            return Surface::Unknown;
        }
        return Surface::Background {
            origin,
            owner_addressed: f.owner_addressed,
        };
    }
    if let Some(caller) = &f.mcp_caller {
        // The channel (web|ios) is client-declared: the bearer is the proof.
        let app = matches!(f.channel, ChannelKind::Web | ChannelKind::Ios);
        if app && f.bearer_verified && !platform_facts {
            let caller = caller.trim();
            return Surface::OwnerDelegate {
                caller: if caller.is_empty() { "unknown" } else { caller }.to_owned(),
            };
        }
        return Surface::Unknown;
    }
    match f.channel {
        ChannelKind::Web | ChannelKind::Ios => {
            if f.bearer_verified && !platform_facts {
                Surface::OwnerDirect
            } else {
                Surface::Unknown
            }
        }
        ChannelKind::Telegram => {
            let Some(t) = &f.telegram else {
                return Surface::Unknown;
            };
            if f.discord.is_some() || f.alexa.is_some() {
                return Surface::Unknown;
            }
            let Some(chat) = id(&t.chat_id) else {
                return Surface::Unknown;
            };
            match t.chat_kind {
                TelegramChatKind::Private => {
                    let from_ok =
                        id(&t.from_user_id).is_none() || same(&t.from_user_id, &t.chat_id);
                    if same(&t.chat_id, &owner.telegram_chat_id) && from_ok {
                        Surface::OwnerDirect
                    } else {
                        let who = id(&t.from_user_id).unwrap_or(chat);
                        Surface::Direct {
                            person: format!("telegram:{who}"),
                        }
                    }
                }
                TelegramChatKind::Group
                | TelegramChatKind::Supergroup
                | TelegramChatKind::Channel => Surface::Channel {
                    channel: format!("telegram:{chat}"),
                },
                TelegramChatKind::Other => Surface::Unknown,
            }
        }
        ChannelKind::Discord => {
            let Some(d) = &f.discord else {
                return Surface::Unknown;
            };
            if f.telegram.is_some() || f.alexa.is_some() {
                return Surface::Unknown;
            }
            let Some(author) = id(&d.author_id) else {
                return Surface::Unknown;
            };
            if d.guild_id.is_some() {
                return match id(&d.channel_id) {
                    Some(c) => Surface::Channel {
                        channel: format!("discord:{c}"),
                    },
                    None => Surface::Unknown,
                };
            }
            if same(&d.author_id, &owner.discord_user_id) {
                Surface::OwnerDirect
            } else {
                Surface::Direct {
                    person: format!("discord:{author}"),
                }
            }
        }
        ChannelKind::Alexa => {
            let Some(a) = &f.alexa else {
                return Surface::Unknown;
            };
            if f.telegram.is_some() || f.discord.is_some() {
                return Surface::Unknown;
            }
            match id(&a.linked_user_id) {
                None => Surface::Unknown,
                Some(_) if same(&a.linked_user_id, &owner.alexa_user_id) => Surface::OwnerDirect,
                Some(u) => Surface::Direct {
                    person: format!("alexa:{u}"),
                },
            }
        }
        ChannelKind::Other => Surface::Unknown,
    }
}

/// May a turn of class `s` read records of `scope`?
pub fn may_read(s: &Surface, scope: &Scope) -> bool {
    match (s, scope) {
        (Surface::OwnerDirect, _) => true,
        (Surface::OwnerDelegate { .. }, Scope::OwnerPersonal) => true,
        (Surface::Direct { person }, Scope::Person(p)) => person == p,
        (Surface::Channel { channel }, Scope::Channel(c)) => channel == c,
        (
            Surface::Background {
                owner_addressed, ..
            },
            Scope::OwnerPersonal,
        ) => *owner_addressed,
        _ => false,
    }
}

/// May a turn of class `s` write records of `scope`? Background never writes.
pub fn may_write(s: &Surface, scope: &Scope) -> bool {
    match (s, scope) {
        (Surface::OwnerDirect, _) => true,
        (Surface::OwnerDelegate { .. }, Scope::OwnerPersonal) => true,
        (Surface::Direct { person }, Scope::Person(p)) => person == p,
        (Surface::Channel { channel }, Scope::Channel(c)) => channel == c,
        _ => false,
    }
}

/// `a` is at most as permissive as `b` (a preorder with `Unknown` at the
/// bottom and `OwnerDirect` at the top). [`may_read`] and [`may_write`]
/// are monotone under it.
pub fn le(a: &Surface, b: &Surface) -> bool {
    if a == b {
        return true;
    }
    match (a, b) {
        (Surface::Unknown, _) => true,
        (
            Surface::Background {
                owner_addressed: false,
                ..
            },
            b,
        ) => *b != Surface::Unknown,
        (
            Surface::Background {
                owner_addressed: true,
                ..
            },
            Surface::Background {
                owner_addressed: true,
                ..
            }
            | Surface::OwnerDelegate { .. }
            | Surface::OwnerDirect,
        ) => true,
        (
            Surface::OwnerDelegate { .. } | Surface::Direct { .. } | Surface::Channel { .. },
            Surface::OwnerDirect,
        ) => true,
        _ => false,
    }
}

/// One turn's classification. The only constructor classifies; the value
/// can only be tightened afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnSurface {
    surface: Surface,
}

impl TurnSurface {
    pub fn classify(facts: &TurnFacts, owner: &OwnerIds) -> Self {
        Self {
            surface: classify(facts, owner),
        }
    }

    pub fn surface(&self) -> &Surface {
        &self.surface
    }

    pub fn class_name(&self) -> &'static str {
        self.surface.class_name()
    }

    pub fn is_owner(&self) -> bool {
        self.surface.is_owner()
    }

    pub fn caller(&self) -> Option<&str> {
        self.surface.caller()
    }

    pub fn may_read(&self, scope: &Scope) -> bool {
        may_read(&self.surface, scope)
    }

    pub fn may_write(&self, scope: &Scope) -> bool {
        may_write(&self.surface, scope)
    }

    /// Narrow the class to `to` when `to` is at most as permissive as the
    /// current one; anything else is ignored (and logged). Returns whether
    /// the class changed.
    pub fn tighten(&mut self, to: Surface) -> bool {
        if to == self.surface {
            return false;
        }
        if le(&to, &self.surface) {
            tracing::info!(
                from = self.surface.class_name(),
                to = to.class_name(),
                "memory surface tightened"
            );
            self.surface = to;
            true
        } else {
            tracing::warn!(
                from = self.surface.class_name(),
                to = to.class_name(),
                "memory surface loosening refused"
            );
            false
        }
    }

    /// Log a denied read or write (class and scope kind only, never ids or
    /// content).
    pub fn log_denied(&self, op: &'static str, scope: &Scope, what: &'static str) {
        tracing::info!(
            surface = self.surface.class_name(),
            scope = scope.kind(),
            op,
            what,
            "memory access denied by surface policy"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER_TG: &str = "1001";
    const OWNER_DC: &str = "649020113026023475";
    const OWNER_AX: &str = "amzn1.owner";

    fn owner() -> OwnerIds {
        OwnerIds {
            telegram_chat_id: Some(OWNER_TG.into()),
            discord_user_id: Some(OWNER_DC.into()),
            alexa_user_id: Some(OWNER_AX.into()),
        }
    }

    fn tg(chat: Option<&str>, kind: TelegramChatKind, from: Option<&str>) -> TurnFacts {
        TurnFacts {
            channel: ChannelKind::Telegram,
            telegram: Some(TelegramFacts {
                chat_id: chat.map(Into::into),
                chat_kind: kind,
                from_user_id: from.map(Into::into),
            }),
            ..TurnFacts::default()
        }
    }

    fn dc(author: Option<&str>, guild: Option<&str>, channel: Option<&str>) -> TurnFacts {
        TurnFacts {
            channel: ChannelKind::Discord,
            discord: Some(DiscordFacts {
                author_id: author.map(Into::into),
                guild_id: guild.map(Into::into),
                channel_id: channel.map(Into::into),
            }),
            ..TurnFacts::default()
        }
    }

    fn ax(user: Option<&str>) -> TurnFacts {
        TurnFacts {
            channel: ChannelKind::Alexa,
            alexa: Some(AlexaFacts {
                linked_user_id: user.map(Into::into),
            }),
            ..TurnFacts::default()
        }
    }

    fn c(f: &TurnFacts) -> Surface {
        classify(f, &owner())
    }

    #[test]
    fn secretary_app_turns() {
        assert_eq!(
            c(&TurnFacts::secretary("web", true, None)),
            Surface::OwnerDirect
        );
        assert_eq!(
            c(&TurnFacts::secretary("ios", true, None)),
            Surface::OwnerDirect
        );
        assert_eq!(
            c(&TurnFacts::secretary("web", false, None)),
            Surface::Unknown
        );
        assert_eq!(
            c(&TurnFacts::secretary("ios", false, None)),
            Surface::Unknown
        );
        assert_eq!(
            c(&TurnFacts::secretary("WEB", true, None)),
            Surface::Unknown
        );
        assert_eq!(c(&TurnFacts::secretary("", true, None)), Surface::Unknown);
        assert_eq!(
            c(&TurnFacts::secretary("other", true, None)),
            Surface::Unknown
        );
    }

    #[test]
    fn mcp_turns() {
        let caller = Some("chatgpt".to_owned());
        assert_eq!(
            c(&TurnFacts::secretary("web", true, caller.clone())),
            Surface::OwnerDelegate {
                caller: "chatgpt".into()
            }
        );
        // The owner's own app with the markers: tightened to a delegate.
        assert_eq!(
            c(&TurnFacts::secretary("ios", true, caller.clone())),
            Surface::OwnerDelegate {
                caller: "chatgpt".into()
            }
        );
        // Spoofs: no bearer, or not a Secretary channel.
        assert_eq!(
            c(&TurnFacts::secretary("web", false, caller.clone())),
            Surface::Unknown
        );
        assert_eq!(
            c(&TurnFacts::secretary("ios", false, caller.clone())),
            Surface::Unknown
        );
        assert_eq!(
            c(&TurnFacts::secretary("telegram", true, caller.clone())),
            Surface::Unknown
        );
        let mut t = tg(Some(OWNER_TG), TelegramChatKind::Private, Some(OWNER_TG));
        t.mcp_caller = caller.clone();
        assert_eq!(c(&t), Surface::Unknown);
        let mut d = dc(Some(OWNER_DC), None, Some("9"));
        d.mcp_caller = caller.clone();
        assert_eq!(c(&d), Surface::Unknown);
        // Web with platform facts mixed in.
        let mut w = TurnFacts::secretary("web", true, caller);
        w.discord = dc(Some(OWNER_DC), None, None).discord;
        assert_eq!(c(&w), Surface::Unknown);
        // Blank caller.
        assert_eq!(
            c(&TurnFacts::secretary("web", true, Some("  ".into()))),
            Surface::OwnerDelegate {
                caller: "unknown".into()
            }
        );
    }

    #[test]
    fn mcp_markers_never_give_owner_direct() {
        for ch in ["web", "ios", "telegram", "discord", "alexa", "x"] {
            for bearer in [false, true] {
                let s = c(&TurnFacts::secretary(ch, bearer, Some("grok".into())));
                assert_ne!(s, Surface::OwnerDirect, "{ch} {bearer}");
            }
        }
    }

    #[test]
    fn telegram_turns() {
        use TelegramChatKind::*;
        assert_eq!(
            c(&tg(Some(OWNER_TG), Private, Some(OWNER_TG))),
            Surface::OwnerDirect
        );
        assert_eq!(c(&tg(Some(OWNER_TG), Private, None)), Surface::OwnerDirect);
        assert_eq!(
            c(&tg(Some("77"), Private, Some("77"))),
            Surface::Direct {
                person: "telegram:77".into()
            }
        );
        // The owner's chat id with another sender: not the owner.
        assert_eq!(
            c(&tg(Some(OWNER_TG), Private, Some("77"))),
            Surface::Direct {
                person: "telegram:77".into()
            }
        );
        // A group containing (or sent by) the owner is a channel.
        for kind in [Group, Supergroup, Channel] {
            assert_eq!(
                c(&tg(Some("-100"), kind, Some(OWNER_TG))),
                Surface::Channel {
                    channel: "telegram:-100".into()
                }
            );
        }
        // Owner chat id used as a group id: still a channel.
        assert_eq!(
            c(&tg(Some(OWNER_TG), Group, Some(OWNER_TG))),
            Surface::Channel {
                channel: format!("telegram:{OWNER_TG}")
            }
        );
        assert_eq!(
            c(&tg(Some(OWNER_TG), Other, Some(OWNER_TG))),
            Surface::Unknown
        );
        assert_eq!(c(&tg(None, Private, Some(OWNER_TG))), Surface::Unknown);
        assert_eq!(c(&tg(Some(" "), Private, Some(OWNER_TG))), Surface::Unknown);
        // Telegram channel without facts.
        let f = TurnFacts {
            channel: ChannelKind::Telegram,
            ..TurnFacts::default()
        };
        assert_eq!(c(&f), Surface::Unknown);
    }

    #[test]
    fn discord_turns() {
        assert_eq!(
            c(&dc(Some(OWNER_DC), None, Some("5"))),
            Surface::OwnerDirect
        );
        assert_eq!(
            c(&dc(Some("42"), None, Some("5"))),
            Surface::Direct {
                person: "discord:42".into()
            }
        );
        // Guild message from the owner's user id: a channel, never OwnerDirect.
        assert_eq!(
            c(&dc(Some(OWNER_DC), Some("g1"), Some("c1"))),
            Surface::Channel {
                channel: "discord:c1".into()
            }
        );
        assert_eq!(c(&dc(Some(OWNER_DC), Some("g1"), None)), Surface::Unknown);
        assert_eq!(c(&dc(None, None, Some("5"))), Surface::Unknown);
        assert_eq!(c(&dc(Some(""), None, Some("5"))), Surface::Unknown);
    }

    #[test]
    fn alexa_turns() {
        assert_eq!(c(&ax(Some(OWNER_AX))), Surface::OwnerDirect);
        assert_eq!(
            c(&ax(Some("amzn1.other"))),
            Surface::Direct {
                person: "alexa:amzn1.other".into()
            }
        );
        assert_eq!(c(&ax(None)), Surface::Unknown);
        // No alexa id configured (today): never the owner.
        assert_eq!(
            classify(&ax(Some(OWNER_AX)), &OwnerIds::default()),
            Surface::Direct {
                person: format!("alexa:{OWNER_AX}")
            }
        );
    }

    #[test]
    fn empty_owner_ids_never_match() {
        let none = OwnerIds {
            telegram_chat_id: Some("".into()),
            discord_user_id: Some(" ".into()),
            alexa_user_id: None,
        };
        assert_ne!(
            classify(&tg(Some(""), TelegramChatKind::Private, None), &none),
            Surface::OwnerDirect
        );
        assert_ne!(
            classify(&dc(Some(" "), None, Some("1")), &none),
            Surface::OwnerDirect
        );
        assert_ne!(classify(&ax(Some("")), &none), Surface::OwnerDirect);
        assert_ne!(
            classify(&dc(Some(OWNER_DC), None, Some("1")), &OwnerIds::default()),
            Surface::OwnerDirect
        );
    }

    #[test]
    fn background_turns() {
        assert_eq!(
            c(&TurnFacts::background(BackgroundOrigin::Proactive, true)),
            Surface::Background {
                origin: BackgroundOrigin::Proactive,
                owner_addressed: true
            }
        );
        let mut f = TurnFacts::background(BackgroundOrigin::Cron, true);
        f.discord = dc(Some(OWNER_DC), None, Some("1")).discord;
        assert_eq!(c(&f), Surface::Unknown);
        let mut f = TurnFacts::background(BackgroundOrigin::Scheduler, true);
        f.mcp_caller = Some("x".into());
        assert_eq!(c(&f), Surface::Unknown);
        // Background with an app channel and bearer is still background.
        let mut f = TurnFacts::background(BackgroundOrigin::Scheduler, false);
        f.channel = ChannelKind::Web;
        f.bearer_verified = true;
        assert!(matches!(c(&f), Surface::Background { .. }));
    }

    #[test]
    fn missing_inputs_are_unknown() {
        assert_eq!(c(&TurnFacts::default()), Surface::Unknown);
        for ch in [
            ChannelKind::Telegram,
            ChannelKind::Discord,
            ChannelKind::Alexa,
            ChannelKind::Other,
        ] {
            let f = TurnFacts {
                channel: ch,
                bearer_verified: true,
                ..TurnFacts::default()
            };
            assert_eq!(c(&f), Surface::Unknown, "{ch:?}");
        }
    }

    fn all_surfaces() -> Vec<Surface> {
        let mut v = vec![Surface::OwnerDirect, Surface::Unknown];
        for o in [BackgroundOrigin::Proactive, BackgroundOrigin::Cron] {
            for a in [false, true] {
                v.push(Surface::Background {
                    origin: o,
                    owner_addressed: a,
                });
            }
        }
        for x in ["a", "b"] {
            v.push(Surface::OwnerDelegate { caller: x.into() });
            v.push(Surface::Direct { person: x.into() });
            v.push(Surface::Channel { channel: x.into() });
        }
        v
    }

    fn all_scopes() -> Vec<Scope> {
        let mut v = vec![Scope::OwnerPersonal];
        for x in ["a", "b"] {
            v.push(Scope::Person(x.into()));
            v.push(Scope::Channel(x.into()));
        }
        v
    }

    #[test]
    fn policy_table() {
        let o = Scope::OwnerPersonal;
        let p = Scope::Person("discord:1".into());
        let ch = Scope::Channel("discord:9".into());
        let d = Surface::OwnerDelegate {
            caller: "grok".into(),
        };
        assert!(may_read(&Surface::OwnerDirect, &o) && may_write(&Surface::OwnerDirect, &p));
        assert!(may_read(&d, &o) && may_write(&d, &o));
        assert!(!may_read(&d, &p) && !may_write(&d, &ch));
        let direct = Surface::Direct {
            person: "discord:1".into(),
        };
        assert!(may_read(&direct, &p) && may_write(&direct, &p));
        assert!(!may_read(&direct, &o) && !may_read(&direct, &Scope::Person("discord:2".into())));
        let chan = Surface::Channel {
            channel: "discord:9".into(),
        };
        assert!(may_read(&chan, &ch) && may_write(&chan, &ch));
        assert!(!may_read(&chan, &o) && !may_write(&chan, &o));
        let bg = |a| Surface::Background {
            origin: BackgroundOrigin::Proactive,
            owner_addressed: a,
        };
        assert!(may_read(&bg(true), &o) && !may_read(&bg(false), &o));
        for s in all_scopes() {
            assert!(!may_write(&bg(true), &s) && !may_write(&bg(false), &s));
            assert!(!may_read(&Surface::Unknown, &s) && !may_write(&Surface::Unknown, &s));
        }
    }

    #[test]
    fn order_is_partial_and_policy_monotone() {
        let ss = all_surfaces();
        for a in &ss {
            assert!(le(a, a));
            assert!(le(&Surface::Unknown, a));
            assert!(le(a, &Surface::OwnerDirect));
            for b in &ss {
                for c2 in &ss {
                    if le(a, b) && le(b, c2) {
                        assert!(le(a, c2), "transitive {a:?} {b:?} {c2:?}");
                    }
                }
                if le(a, b) {
                    for s in all_scopes() {
                        assert!(!may_read(a, &s) || may_read(b, &s), "{a:?} {b:?} {s:?}");
                        assert!(!may_write(a, &s) || may_write(b, &s), "{a:?} {b:?} {s:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn tighten_never_loosens() {
        let mut t = TurnSurface::classify(&TurnFacts::secretary("web", true, None), &owner());
        assert!(t.tighten(Surface::OwnerDelegate { caller: "x".into() }));
        assert!(!t.tighten(Surface::OwnerDirect));
        assert!(!t.tighten(Surface::OwnerDelegate { caller: "y".into() }));
        assert_eq!(t.caller(), Some("x"));
        assert!(t.tighten(Surface::Unknown));
        for s in all_surfaces() {
            assert!(!t.tighten(s));
            assert_eq!(t.surface(), &Surface::Unknown);
        }
    }
}
