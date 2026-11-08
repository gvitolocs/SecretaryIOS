//! Differential tests for the memory surfaces against a plain-Rust copy of
//! the Verus kernel `verification/verus/memory_surfaces.rs`: `classify`,
//! `may_read`, `may_write`, `le` and `tighten` vs `lucina_memory::surface`.
//! Ids are small numbers (so owner ids collide often), blank strings and
//! missing values; a blank or missing id is `None` in the kernel.

use lucina_memory::surface::{
    self, AlexaFacts, BackgroundOrigin, ChannelKind, DiscordFacts, OwnerIds, Scope, Surface,
    TelegramChatKind, TelegramFacts, TurnFacts, TurnSurface,
};
use proptest::prelude::*;

/// Plain-Rust copy of the Verus kernel (exec part).
mod kernel {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ChannelKind {
        Web,
        Ios,
        Telegram,
        Discord,
        Alexa,
        Other,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum BgOrigin {
        Proactive,
        Scheduler,
        Cron,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Platform {
        Telegram,
        Discord,
        Alexa,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ChatKind {
        Private,
        Group,
        Supergroup,
        Channel,
        Other,
    }

    #[derive(Debug, Clone, Copy)]
    pub struct Facts {
        pub channel: ChannelKind,
        pub bearer: bool,
        pub mcp: Option<u64>,
        pub tg_present: bool,
        pub tg_chat: Option<u64>,
        pub tg_kind: ChatKind,
        pub tg_from: Option<u64>,
        pub dc_present: bool,
        pub dc_author: Option<u64>,
        pub dc_guild: Option<u64>,
        pub dc_channel: Option<u64>,
        pub ax_present: bool,
        pub ax_linked: Option<u64>,
        pub bg_present: bool,
        pub bg_origin: BgOrigin,
        pub owner_addressed: bool,
    }

    #[derive(Debug, Clone, Copy)]
    pub struct Owner {
        pub tg: Option<u64>,
        pub dc: Option<u64>,
        pub ax: Option<u64>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Surface {
        OwnerDirect,
        OwnerDelegate {
            caller: u64,
        },
        Direct {
            platform: Platform,
            id: u64,
        },
        Channel {
            platform: Platform,
            id: u64,
        },
        Background {
            origin: BgOrigin,
            owner_addressed: bool,
        },
        Unknown,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Scope {
        OwnerPersonal,
        Person { platform: Platform, id: u64 },
        Channel { platform: Platform, id: u64 },
    }

    fn same(a: Option<u64>, b: Option<u64>) -> bool {
        matches!((a, b), (Some(x), Some(y)) if x == y)
    }

    pub fn classify(f: &Facts, o: &Owner) -> Surface {
        let platform = f.tg_present || f.dc_present || f.ax_present;
        if f.bg_present {
            if f.mcp.is_some() || platform {
                return Surface::Unknown;
            }
            return Surface::Background {
                origin: f.bg_origin,
                owner_addressed: f.owner_addressed,
            };
        }
        if let Some(caller) = f.mcp {
            let app = matches!(f.channel, ChannelKind::Web | ChannelKind::Ios);
            if app && f.bearer && !platform {
                return Surface::OwnerDelegate { caller };
            }
            return Surface::Unknown;
        }
        match f.channel {
            ChannelKind::Web | ChannelKind::Ios => {
                if f.bearer && !platform {
                    Surface::OwnerDirect
                } else {
                    Surface::Unknown
                }
            }
            ChannelKind::Telegram => {
                if !f.tg_present || f.dc_present || f.ax_present {
                    return Surface::Unknown;
                }
                let Some(chat) = f.tg_chat else {
                    return Surface::Unknown;
                };
                match f.tg_kind {
                    ChatKind::Private => {
                        let from_ok = f.tg_from.is_none() || same(f.tg_from, f.tg_chat);
                        if same(f.tg_chat, o.tg) && from_ok {
                            Surface::OwnerDirect
                        } else {
                            Surface::Direct {
                                platform: Platform::Telegram,
                                id: f.tg_from.unwrap_or(chat),
                            }
                        }
                    }
                    ChatKind::Group | ChatKind::Supergroup | ChatKind::Channel => {
                        Surface::Channel {
                            platform: Platform::Telegram,
                            id: chat,
                        }
                    }
                    ChatKind::Other => Surface::Unknown,
                }
            }
            ChannelKind::Discord => {
                if !f.dc_present || f.tg_present || f.ax_present {
                    return Surface::Unknown;
                }
                let Some(author) = f.dc_author else {
                    return Surface::Unknown;
                };
                if f.dc_guild.is_some() {
                    return match f.dc_channel {
                        Some(c) => Surface::Channel {
                            platform: Platform::Discord,
                            id: c,
                        },
                        None => Surface::Unknown,
                    };
                }
                if same(f.dc_author, o.dc) {
                    Surface::OwnerDirect
                } else {
                    Surface::Direct {
                        platform: Platform::Discord,
                        id: author,
                    }
                }
            }
            ChannelKind::Alexa => {
                if !f.ax_present || f.tg_present || f.dc_present {
                    return Surface::Unknown;
                }
                match f.ax_linked {
                    None => Surface::Unknown,
                    Some(_) if same(f.ax_linked, o.ax) => Surface::OwnerDirect,
                    Some(u) => Surface::Direct {
                        platform: Platform::Alexa,
                        id: u,
                    },
                }
            }
            ChannelKind::Other => Surface::Unknown,
        }
    }

    pub fn may_read(s: &Surface, sc: &Scope) -> bool {
        match (s, sc) {
            (Surface::OwnerDirect, _) => true,
            (Surface::OwnerDelegate { .. }, Scope::OwnerPersonal) => true,
            (Surface::Direct { platform: p, id: i }, Scope::Person { platform: q, id: j }) => {
                p == q && i == j
            }
            (Surface::Channel { platform: p, id: i }, Scope::Channel { platform: q, id: j }) => {
                p == q && i == j
            }
            (
                Surface::Background {
                    owner_addressed, ..
                },
                Scope::OwnerPersonal,
            ) => *owner_addressed,
            _ => false,
        }
    }

    pub fn may_write(s: &Surface, sc: &Scope) -> bool {
        match (s, sc) {
            (Surface::OwnerDirect, _) => true,
            (Surface::OwnerDelegate { .. }, Scope::OwnerPersonal) => true,
            (Surface::Direct { platform: p, id: i }, Scope::Person { platform: q, id: j }) => {
                p == q && i == j
            }
            (Surface::Channel { platform: p, id: i }, Scope::Channel { platform: q, id: j }) => {
                p == q && i == j
            }
            _ => false,
        }
    }

    pub fn le(a: &Surface, b: &Surface) -> bool {
        if a == b {
            return true;
        }
        match a {
            Surface::Unknown => true,
            Surface::Background {
                owner_addressed, ..
            } => {
                if !*owner_addressed {
                    *b != Surface::Unknown
                } else {
                    matches!(
                        b,
                        Surface::Background {
                            owner_addressed: true,
                            ..
                        } | Surface::OwnerDelegate { .. }
                            | Surface::OwnerDirect
                    )
                }
            }
            Surface::OwnerDelegate { .. } | Surface::Direct { .. } | Surface::Channel { .. } => {
                *b == Surface::OwnerDirect
            }
            Surface::OwnerDirect => false,
        }
    }

    pub fn tighten(cur: &mut Surface, to: Surface) -> bool {
        if to == *cur {
            return false;
        }
        if le(&to, cur) {
            *cur = to;
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------- mapping

/// A generated id: missing, blank, or a small number.
#[derive(Debug, Clone, Copy)]
enum Id {
    Missing,
    Blank,
    N(u64),
}

impl Id {
    fn prod(self) -> Option<String> {
        match self {
            Id::Missing => None,
            Id::Blank => Some("  ".into()),
            Id::N(n) => Some(format!(" {n}")), // trimmed by production
        }
    }

    fn kernel(self) -> Option<u64> {
        match self {
            Id::N(n) => Some(n),
            _ => None,
        }
    }
}

fn id() -> impl Strategy<Value = Id> {
    prop_oneof![
        1 => Just(Id::Missing),
        1 => Just(Id::Blank),
        4 => (0u64..4).prop_map(Id::N),
    ]
}

/// MCP callers; blank ones are "unknown" in production.
const CALLERS: &[&str] = &["unknown", "chatgpt", "grok", "claude-code", "", "   "];

fn caller_index(name: &str) -> u64 {
    let n = name.trim();
    let n = if n.is_empty() { "unknown" } else { n };
    CALLERS.iter().position(|c| *c == n).unwrap() as u64
}

const CHANNELS: &[(&str, kernel::ChannelKind)] = &[
    ("web", kernel::ChannelKind::Web),
    ("ios", kernel::ChannelKind::Ios),
    ("telegram", kernel::ChannelKind::Telegram),
    ("discord", kernel::ChannelKind::Discord),
    ("alexa", kernel::ChannelKind::Alexa),
    ("sms", kernel::ChannelKind::Other),
];

const CHATS: &[(&str, kernel::ChatKind)] = &[
    ("private", kernel::ChatKind::Private),
    ("group", kernel::ChatKind::Group),
    ("supergroup", kernel::ChatKind::Supergroup),
    ("channel", kernel::ChatKind::Channel),
    ("bot", kernel::ChatKind::Other),
];

const ORIGINS: &[(BackgroundOrigin, kernel::BgOrigin)] = &[
    (BackgroundOrigin::Proactive, kernel::BgOrigin::Proactive),
    (BackgroundOrigin::Scheduler, kernel::BgOrigin::Scheduler),
    (BackgroundOrigin::Cron, kernel::BgOrigin::Cron),
];

#[derive(Debug, Clone)]
struct Gen {
    channel: usize,
    bearer: bool,
    mcp: Option<usize>,
    tg: Option<(Id, usize, Id)>,
    dc: Option<(Id, Id, Id)>,
    ax: Option<Id>,
    bg: Option<usize>,
    owner_addressed: bool,
    owner: (Id, Id, Id),
}

fn gen() -> impl Strategy<Value = Gen> {
    (
        (
            0..CHANNELS.len(),
            any::<bool>(),
            proptest::option::weighted(0.3, 0..CALLERS.len()),
        ),
        proptest::option::weighted(0.5, (id(), 0..CHATS.len(), id())),
        proptest::option::weighted(0.5, (id(), id(), id())),
        proptest::option::weighted(0.4, id()),
        (
            proptest::option::weighted(0.2, 0..ORIGINS.len()),
            any::<bool>(),
        ),
        (id(), id(), id()),
    )
        .prop_map(
            |((channel, bearer, mcp), tg, dc, ax, (bg, owner_addressed), owner)| Gen {
                channel,
                bearer,
                mcp,
                tg,
                dc,
                ax,
                bg,
                owner_addressed,
                owner,
            },
        )
}

fn prod_facts(g: &Gen) -> (TurnFacts, OwnerIds) {
    let f = TurnFacts {
        channel: ChannelKind::parse(CHANNELS[g.channel].0),
        bearer_verified: g.bearer,
        mcp_caller: g.mcp.map(|i| CALLERS[i].to_owned()),
        telegram: g.tg.map(|(chat, kind, from)| TelegramFacts {
            chat_id: chat.prod(),
            chat_kind: TelegramChatKind::parse(CHATS[kind].0),
            from_user_id: from.prod(),
        }),
        discord: g.dc.map(|(author, guild, channel)| DiscordFacts {
            author_id: author.prod(),
            // Production treats any guild id (even blank) as a guild message.
            guild_id: match guild {
                Id::Missing => None,
                other => Some(other.prod().unwrap()),
            },
            channel_id: channel.prod(),
        }),
        alexa: g.ax.map(|u| AlexaFacts {
            linked_user_id: u.prod(),
        }),
        background: g.bg.map(|i| ORIGINS[i].0),
        owner_addressed: g.owner_addressed,
    };
    let o = OwnerIds {
        telegram_chat_id: g.owner.0.prod(),
        discord_user_id: g.owner.1.prod(),
        alexa_user_id: g.owner.2.prod(),
    };
    (f, o)
}

fn kernel_facts(g: &Gen) -> (kernel::Facts, kernel::Owner) {
    let (tg_chat, tg_kind, tg_from) =
        g.tg.map(|(c, k, f)| (c.kernel(), CHATS[k].1, f.kernel()))
            .unwrap_or((None, kernel::ChatKind::Other, None));
    let (dc_author, dc_guild, dc_channel) =
        g.dc.map(|(a, gd, c)| {
            // Any present guild id (even blank) makes a guild message.
            let guild = match gd {
                Id::Missing => None,
                Id::Blank => Some(u64::MAX),
                Id::N(n) => Some(n),
            };
            (a.kernel(), guild, c.kernel())
        })
        .unwrap_or((None, None, None));
    let f = kernel::Facts {
        channel: CHANNELS[g.channel].1,
        bearer: g.bearer,
        mcp: g.mcp.map(|i| caller_index(CALLERS[i])),
        tg_present: g.tg.is_some(),
        tg_chat,
        tg_kind,
        tg_from,
        dc_present: g.dc.is_some(),
        dc_author,
        dc_guild,
        dc_channel,
        ax_present: g.ax.is_some(),
        ax_linked: g.ax.and_then(Id::kernel),
        bg_present: g.bg.is_some(),
        bg_origin: g.bg.map(|i| ORIGINS[i].1).unwrap_or(kernel::BgOrigin::Cron),
        owner_addressed: g.owner_addressed,
    };
    let o = kernel::Owner {
        tg: g.owner.0.kernel(),
        dc: g.owner.1.kernel(),
        ax: g.owner.2.kernel(),
    };
    (f, o)
}

fn platform_of(name: &str) -> kernel::Platform {
    match name {
        "telegram" => kernel::Platform::Telegram,
        "discord" => kernel::Platform::Discord,
        "alexa" => kernel::Platform::Alexa,
        other => panic!("unknown platform {other}"),
    }
}

fn platform_name(p: kernel::Platform) -> &'static str {
    match p {
        kernel::Platform::Telegram => "telegram",
        kernel::Platform::Discord => "discord",
        kernel::Platform::Alexa => "alexa",
    }
}

fn split(key: &str) -> (kernel::Platform, u64) {
    let (p, i) = key.split_once(':').unwrap();
    (platform_of(p), i.parse().unwrap())
}

fn to_kernel(s: &Surface) -> kernel::Surface {
    match s {
        Surface::OwnerDirect => kernel::Surface::OwnerDirect,
        Surface::OwnerDelegate { caller } => kernel::Surface::OwnerDelegate {
            caller: caller_index(caller),
        },
        Surface::Direct { person } => {
            let (platform, id) = split(person);
            kernel::Surface::Direct { platform, id }
        }
        Surface::Channel { channel } => {
            let (platform, id) = split(channel);
            kernel::Surface::Channel { platform, id }
        }
        Surface::Background {
            origin,
            owner_addressed,
        } => kernel::Surface::Background {
            origin: ORIGINS.iter().find(|(p, _)| p == origin).unwrap().1,
            owner_addressed: *owner_addressed,
        },
        Surface::Unknown => kernel::Surface::Unknown,
    }
}

fn to_prod(s: &kernel::Surface) -> Surface {
    match *s {
        kernel::Surface::OwnerDirect => Surface::OwnerDirect,
        kernel::Surface::OwnerDelegate { caller } => Surface::OwnerDelegate {
            caller: CALLERS[caller as usize].to_owned(),
        },
        kernel::Surface::Direct { platform, id } => Surface::Direct {
            person: format!("{}:{id}", platform_name(platform)),
        },
        kernel::Surface::Channel { platform, id } => Surface::Channel {
            channel: format!("{}:{id}", platform_name(platform)),
        },
        kernel::Surface::Background {
            origin,
            owner_addressed,
        } => Surface::Background {
            origin: ORIGINS.iter().find(|(_, k)| *k == origin).unwrap().0,
            owner_addressed,
        },
        kernel::Surface::Unknown => Surface::Unknown,
    }
}

fn scope_to_prod(s: &kernel::Scope) -> Scope {
    match *s {
        kernel::Scope::OwnerPersonal => Scope::OwnerPersonal,
        kernel::Scope::Person { platform, id } => {
            Scope::Person(format!("{}:{id}", platform_name(platform)))
        }
        kernel::Scope::Channel { platform, id } => {
            Scope::Channel(format!("{}:{id}", platform_name(platform)))
        }
    }
}

fn platform() -> impl Strategy<Value = kernel::Platform> {
    prop_oneof![
        Just(kernel::Platform::Telegram),
        Just(kernel::Platform::Discord),
        Just(kernel::Platform::Alexa),
    ]
}

fn kernel_surface() -> impl Strategy<Value = kernel::Surface> {
    prop_oneof![
        Just(kernel::Surface::OwnerDirect),
        (1u64..4).prop_map(|caller| kernel::Surface::OwnerDelegate { caller }),
        (platform(), 0u64..3).prop_map(|(platform, id)| kernel::Surface::Direct { platform, id }),
        (platform(), 0u64..3).prop_map(|(platform, id)| kernel::Surface::Channel { platform, id }),
        (0..ORIGINS.len(), any::<bool>()).prop_map(|(i, a)| kernel::Surface::Background {
            origin: ORIGINS[i].1,
            owner_addressed: a,
        }),
        Just(kernel::Surface::Unknown),
    ]
}

fn kernel_scope() -> impl Strategy<Value = kernel::Scope> {
    prop_oneof![
        Just(kernel::Scope::OwnerPersonal),
        (platform(), 0u64..3).prop_map(|(platform, id)| kernel::Scope::Person { platform, id }),
        (platform(), 0u64..3).prop_map(|(platform, id)| kernel::Scope::Channel { platform, id }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    #[test]
    fn classify_matches_the_kernel(g in gen()) {
        let (pf, po) = prod_facts(&g);
        let (kf, ko) = kernel_facts(&g);
        let prod = surface::classify(&pf, &po);
        let kern = kernel::classify(&kf, &ko);
        prop_assert_eq!(to_kernel(&prod), kern, "facts {:?}", pf);
    }

    #[test]
    fn policy_matches_the_kernel(a in kernel_surface(), b in kernel_surface(), sc in kernel_scope()) {
        let (pa, pb, ps) = (to_prod(&a), to_prod(&b), scope_to_prod(&sc));
        prop_assert_eq!(surface::may_read(&pa, &ps), kernel::may_read(&a, &sc));
        prop_assert_eq!(surface::may_write(&pa, &ps), kernel::may_write(&a, &sc));
        prop_assert_eq!(surface::le(&pa, &pb), kernel::le(&a, &b));
        // Monotone (S4), checked on production.
        if surface::le(&pa, &pb) {
            prop_assert!(!surface::may_read(&pa, &ps) || surface::may_read(&pb, &ps));
            prop_assert!(!surface::may_write(&pa, &ps) || surface::may_write(&pb, &ps));
        }
    }

    #[test]
    fn tighten_matches_the_kernel(g in gen(), tos in proptest::collection::vec(kernel_surface(), 1..6)) {
        let (pf, po) = prod_facts(&g);
        let (kf, ko) = kernel_facts(&g);
        let mut prod = TurnSurface::classify(&pf, &po);
        let mut kern = kernel::classify(&kf, &ko);
        for to in tos {
            let before = prod.surface().clone();
            let changed_p = prod.tighten(to_prod(&to));
            let changed_k = kernel::tighten(&mut kern, to);
            prop_assert_eq!(changed_p, changed_k);
            prop_assert_eq!(to_kernel(prod.surface()), kern);
            // S5: never looser than before.
            prop_assert!(surface::le(prod.surface(), &before));
        }
    }
}
