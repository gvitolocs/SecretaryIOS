// Memory surface kernel (Lucina memory: which turn may read or write whose records).
//
// Production (branch lucina-surfaces, crates/lucina-memory/src/surface.rs):
//   Facts / Owner        <-> TurnFacts / OwnerIds (ids: a trimmed, non-empty string is Some(id);
//                            an empty or blank one is None; the diff test maps strings to u64)
//   classify             <-> surface::classify
//   may_read / may_write <-> surface::may_read / surface::may_write
//   le                   <-> surface::le
//   tighten              <-> TurnSurface::tighten
//   Surface::Direct{p,i} <-> Surface::Direct { person: "<p>:<i>" }, Channel likewise
// Enforcement: lucina-memory Memory::{build_context, remember, search} (scope from the peer:
// owner peer = OwnerPersonal), lucina-server context::prepare_with (owner blocks),
// chat::handle_chat (classified once; non-owner surfaces refused), lucina-proactive
// HonchoMemory (Background, owner-addressed).
// Differential test: crates/lucina-memory/tests/verus_surfaces.rs.
//
// Proven:
//   S1 unknown_and_channel_never_touch_owner: Unknown and Channel surfaces never read or
//      write the owner's personal scope; Unknown touches nothing at all.
//   S2 background_never_writes: a Background surface writes no scope.
//   S3 fail_closed_*: missing or contradictory inputs classify as Unknown (no bearer on
//      web/ios, MCP markers without the bearer or off web/ios, Telegram without a chat id,
//      Discord without an author, Alexa without a linked id, background mixed with
//      channel facts, any other channel, the default facts); MCP markers never give
//      OwnerDirect; a Discord guild message (even from the owner's id) and a Telegram
//      group (even with the owner in it) are never OwnerDirect; OwnerDirect needs a
//      positive proof (bearer on web/ios, or an id equal to a configured owner id).
//   S4 le_*: le is reflexive and transitive, Unknown is the bottom and OwnerDirect the
//      top, and may_read / may_write are monotone under le.
//   S5 tighten_never_loosens: the surface after tighten is le the one before, and
//      tightening Unknown leaves Unknown.
//
// No Stateright model: classification is a pure function of one turn's inputs and the
// policy is a pure function of (surface, scope); no state is shared between processes
// (a spawned job carries an immutable copy of its turn's surface).
use vstd::prelude::*;

verus! {

#[derive(PartialEq, Eq)]
pub enum ChannelKind {
    Web,
    Ios,
    Telegram,
    Discord,
    Alexa,
    Other,
}

#[derive(PartialEq, Eq)]
pub enum BgOrigin {
    Proactive,
    Scheduler,
    Cron,
}

#[derive(PartialEq, Eq)]
pub enum Platform {
    Telegram,
    Discord,
    Alexa,
}

#[derive(PartialEq, Eq)]
pub enum ChatKind {
    Private,
    Group,
    Supergroup,
    Channel,
    Other,
}

/// TurnFacts, flattened: `*_present` is the Option of the platform facts.
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

pub struct Owner {
    pub tg: Option<u64>,
    pub dc: Option<u64>,
    pub ax: Option<u64>,
}

#[derive(PartialEq, Eq)]
pub enum Surface {
    OwnerDirect,
    OwnerDelegate { caller: u64 },
    Direct { platform: Platform, id: u64 },
    Channel { platform: Platform, id: u64 },
    Background { origin: BgOrigin, owner_addressed: bool },
    Unknown,
}

#[derive(PartialEq, Eq)]
pub enum Scope {
    OwnerPersonal,
    Person { platform: Platform, id: u64 },
    Channel { platform: Platform, id: u64 },
}

// ---------------------------------------------------------------- spec model

pub open spec fn same(a: Option<u64>, b: Option<u64>) -> bool {
    match a {
        Some(x) => match b {
            Some(y) => x == y,
            None => false,
        },
        None => false,
    }
}

pub open spec fn platform_facts(f: Facts) -> bool {
    f.tg_present || f.dc_present || f.ax_present
}

pub open spec fn classify_spec(f: Facts, o: Owner) -> Surface {
    if f.bg_present {
        if f.mcp.is_some() || platform_facts(f) {
            Surface::Unknown
        } else {
            Surface::Background { origin: f.bg_origin, owner_addressed: f.owner_addressed }
        }
    } else if f.mcp.is_some() {
        if (f.channel == ChannelKind::Web || f.channel == ChannelKind::Ios) && f.bearer
            && !platform_facts(f) {
            Surface::OwnerDelegate { caller: f.mcp.unwrap() }
        } else {
            Surface::Unknown
        }
    } else {
        match f.channel {
            ChannelKind::Web | ChannelKind::Ios => {
                if f.bearer && !platform_facts(f) {
                    Surface::OwnerDirect
                } else {
                    Surface::Unknown
                }
            },
            ChannelKind::Telegram => {
                if !f.tg_present || f.dc_present || f.ax_present || f.tg_chat.is_none() {
                    Surface::Unknown
                } else {
                    match f.tg_kind {
                        ChatKind::Private => {
                            if same(f.tg_chat, o.tg) && (f.tg_from.is_none() || same(
                                f.tg_from,
                                f.tg_chat,
                            )) {
                                Surface::OwnerDirect
                            } else if f.tg_from.is_some() {
                                Surface::Direct { platform: Platform::Telegram, id: f.tg_from.unwrap() }
                            } else {
                                Surface::Direct { platform: Platform::Telegram, id: f.tg_chat.unwrap() }
                            }
                        },
                        ChatKind::Group | ChatKind::Supergroup | ChatKind::Channel => {
                            Surface::Channel { platform: Platform::Telegram, id: f.tg_chat.unwrap() }
                        },
                        ChatKind::Other => Surface::Unknown,
                    }
                }
            },
            ChannelKind::Discord => {
                if !f.dc_present || f.tg_present || f.ax_present || f.dc_author.is_none() {
                    Surface::Unknown
                } else if f.dc_guild.is_some() {
                    if f.dc_channel.is_some() {
                        Surface::Channel { platform: Platform::Discord, id: f.dc_channel.unwrap() }
                    } else {
                        Surface::Unknown
                    }
                } else if same(f.dc_author, o.dc) {
                    Surface::OwnerDirect
                } else {
                    Surface::Direct { platform: Platform::Discord, id: f.dc_author.unwrap() }
                }
            },
            ChannelKind::Alexa => {
                if !f.ax_present || f.tg_present || f.dc_present || f.ax_linked.is_none() {
                    Surface::Unknown
                } else if same(f.ax_linked, o.ax) {
                    Surface::OwnerDirect
                } else {
                    Surface::Direct { platform: Platform::Alexa, id: f.ax_linked.unwrap() }
                }
            },
            ChannelKind::Other => Surface::Unknown,
        }
    }
}

pub open spec fn may_read_spec(s: Surface, sc: Scope) -> bool {
    match s {
        Surface::OwnerDirect => true,
        Surface::OwnerDelegate { .. } => sc == Scope::OwnerPersonal,
        Surface::Direct { platform, id } => sc == (Scope::Person { platform, id }),
        Surface::Channel { platform, id } => sc == (Scope::Channel { platform, id }),
        Surface::Background { owner_addressed, .. } => owner_addressed && sc
            == Scope::OwnerPersonal,
        Surface::Unknown => false,
    }
}

pub open spec fn may_write_spec(s: Surface, sc: Scope) -> bool {
    match s {
        Surface::OwnerDirect => true,
        Surface::OwnerDelegate { .. } => sc == Scope::OwnerPersonal,
        Surface::Direct { platform, id } => sc == (Scope::Person { platform, id }),
        Surface::Channel { platform, id } => sc == (Scope::Channel { platform, id }),
        Surface::Background { .. } => false,
        Surface::Unknown => false,
    }
}

pub open spec fn le_spec(a: Surface, b: Surface) -> bool {
    if a == b {
        true
    } else {
        match a {
            Surface::Unknown => true,
            Surface::Background { owner_addressed, .. } => {
                if !owner_addressed {
                    b != Surface::Unknown
                } else {
                    match b {
                        Surface::Background { owner_addressed: b_addressed, .. } => b_addressed,
                        Surface::OwnerDelegate { .. } => true,
                        Surface::OwnerDirect => true,
                        _ => false,
                    }
                }
            },
            Surface::OwnerDelegate { .. } | Surface::Direct { .. } | Surface::Channel { .. } => {
                b == Surface::OwnerDirect
            },
            Surface::OwnerDirect => false,
        }
    }
}

pub open spec fn tighten_spec(cur: Surface, to: Surface) -> Surface {
    if le_spec(to, cur) {
        to
    } else {
        cur
    }
}

pub open spec fn is_unknown_or_channel(s: Surface) -> bool {
    match s {
        Surface::Unknown => true,
        Surface::Channel { .. } => true,
        _ => false,
    }
}

// ---------------------------------------------------------------- exec kernel

pub fn same_exec(a: Option<u64>, b: Option<u64>) -> (r: bool)
    ensures
        r == same(a, b),
{
    match a {
        Some(x) => match b {
            Some(y) => x == y,
            None => false,
        },
        None => false,
    }
}

fn is_some(a: Option<u64>) -> (r: bool)
    ensures
        r == a.is_some(),
{
    match a {
        Some(_) => true,
        None => false,
    }
}

fn copy_origin(o: &BgOrigin) -> (r: BgOrigin)
    ensures
        r == *o,
{
    match o {
        BgOrigin::Proactive => BgOrigin::Proactive,
        BgOrigin::Scheduler => BgOrigin::Scheduler,
        BgOrigin::Cron => BgOrigin::Cron,
    }
}

fn copy_platform(p: &Platform) -> (r: Platform)
    ensures
        r == *p,
{
    match p {
        Platform::Telegram => Platform::Telegram,
        Platform::Discord => Platform::Discord,
        Platform::Alexa => Platform::Alexa,
    }
}

/// surface::classify.
pub fn classify(f: &Facts, o: &Owner) -> (r: Surface)
    ensures
        r == classify_spec(*f, *o),
{
    let platform = f.tg_present || f.dc_present || f.ax_present;
    if f.bg_present {
        if is_some(f.mcp) || platform {
            return Surface::Unknown;
        }
        return Surface::Background {
            origin: copy_origin(&f.bg_origin),
            owner_addressed: f.owner_addressed,
        };
    }
    match f.mcp {
        Some(caller) => {
            let app = match f.channel {
                ChannelKind::Web | ChannelKind::Ios => true,
                _ => false,
            };
            if app && f.bearer && !platform {
                return Surface::OwnerDelegate { caller };
            }
            return Surface::Unknown;
        },
        None => {},
    }
    match f.channel {
        ChannelKind::Web | ChannelKind::Ios => {
            if f.bearer && !platform {
                Surface::OwnerDirect
            } else {
                Surface::Unknown
            }
        },
        ChannelKind::Telegram => {
            if !f.tg_present || f.dc_present || f.ax_present {
                return Surface::Unknown;
            }
            let chat = match f.tg_chat {
                Some(c) => c,
                None => {
                    return Surface::Unknown;
                },
            };
            match f.tg_kind {
                ChatKind::Private => {
                    let from_ok = !is_some(f.tg_from) || same_exec(f.tg_from, f.tg_chat);
                    if same_exec(f.tg_chat, o.tg) && from_ok {
                        Surface::OwnerDirect
                    } else {
                        match f.tg_from {
                            Some(who) => Surface::Direct { platform: Platform::Telegram, id: who },
                            None => Surface::Direct { platform: Platform::Telegram, id: chat },
                        }
                    }
                },
                ChatKind::Group | ChatKind::Supergroup | ChatKind::Channel => {
                    Surface::Channel { platform: Platform::Telegram, id: chat }
                },
                ChatKind::Other => Surface::Unknown,
            }
        },
        ChannelKind::Discord => {
            if !f.dc_present || f.tg_present || f.ax_present {
                return Surface::Unknown;
            }
            let author = match f.dc_author {
                Some(a) => a,
                None => {
                    return Surface::Unknown;
                },
            };
            if is_some(f.dc_guild) {
                return match f.dc_channel {
                    Some(c) => Surface::Channel { platform: Platform::Discord, id: c },
                    None => Surface::Unknown,
                };
            }
            if same_exec(f.dc_author, o.dc) {
                Surface::OwnerDirect
            } else {
                Surface::Direct { platform: Platform::Discord, id: author }
            }
        },
        ChannelKind::Alexa => {
            if !f.ax_present || f.tg_present || f.dc_present {
                return Surface::Unknown;
            }
            match f.ax_linked {
                None => Surface::Unknown,
                Some(u) => {
                    if same_exec(f.ax_linked, o.ax) {
                        Surface::OwnerDirect
                    } else {
                        Surface::Direct { platform: Platform::Alexa, id: u }
                    }
                },
            }
        },
        ChannelKind::Other => Surface::Unknown,
    }
}

fn platform_eq(a: &Platform, b: &Platform) -> (r: bool)
    ensures
        r == (*a == *b),
{
    match (a, b) {
        (Platform::Telegram, Platform::Telegram) => true,
        (Platform::Discord, Platform::Discord) => true,
        (Platform::Alexa, Platform::Alexa) => true,
        _ => false,
    }
}

fn origin_eq(a: &BgOrigin, b: &BgOrigin) -> (r: bool)
    ensures
        r == (*a == *b),
{
    match (a, b) {
        (BgOrigin::Proactive, BgOrigin::Proactive) => true,
        (BgOrigin::Scheduler, BgOrigin::Scheduler) => true,
        (BgOrigin::Cron, BgOrigin::Cron) => true,
        _ => false,
    }
}

fn surface_eq(a: &Surface, b: &Surface) -> (r: bool)
    ensures
        r == (*a == *b),
{
    match (a, b) {
        (Surface::OwnerDirect, Surface::OwnerDirect) => true,
        (Surface::OwnerDelegate { caller: x }, Surface::OwnerDelegate { caller: y }) => *x == *y,
        (Surface::Direct { platform: p, id: i }, Surface::Direct { platform: q, id: j }) => {
            platform_eq(p, q) && *i == *j
        },
        (Surface::Channel { platform: p, id: i }, Surface::Channel { platform: q, id: j }) => {
            platform_eq(p, q) && *i == *j
        },
        (
            Surface::Background { origin: o1, owner_addressed: a1 },
            Surface::Background { origin: o2, owner_addressed: a2 },
        ) => origin_eq(o1, o2) && *a1 == *a2,
        (Surface::Unknown, Surface::Unknown) => true,
        _ => false,
    }
}

/// surface::may_read.
pub fn may_read(s: &Surface, sc: &Scope) -> (r: bool)
    ensures
        r == may_read_spec(*s, *sc),
{
    match (s, sc) {
        (Surface::OwnerDirect, _) => true,
        (Surface::OwnerDelegate { .. }, Scope::OwnerPersonal) => true,
        (Surface::Direct { platform: p, id: i }, Scope::Person { platform: q, id: j }) => {
            platform_eq(p, q) && *i == *j
        },
        (Surface::Channel { platform: p, id: i }, Scope::Channel { platform: q, id: j }) => {
            platform_eq(p, q) && *i == *j
        },
        (Surface::Background { owner_addressed, .. }, Scope::OwnerPersonal) => *owner_addressed,
        _ => false,
    }
}

/// surface::may_write.
pub fn may_write(s: &Surface, sc: &Scope) -> (r: bool)
    ensures
        r == may_write_spec(*s, *sc),
{
    match (s, sc) {
        (Surface::OwnerDirect, _) => true,
        (Surface::OwnerDelegate { .. }, Scope::OwnerPersonal) => true,
        (Surface::Direct { platform: p, id: i }, Scope::Person { platform: q, id: j }) => {
            platform_eq(p, q) && *i == *j
        },
        (Surface::Channel { platform: p, id: i }, Scope::Channel { platform: q, id: j }) => {
            platform_eq(p, q) && *i == *j
        },
        _ => false,
    }
}

/// surface::le.
pub fn le(a: &Surface, b: &Surface) -> (r: bool)
    ensures
        r == le_spec(*a, *b),
{
    if surface_eq(a, b) {
        return true;
    }
    match a {
        Surface::Unknown => true,
        Surface::Background { owner_addressed, .. } => {
            if !*owner_addressed {
                match b {
                    Surface::Unknown => false,
                    _ => true,
                }
            } else {
                match b {
                    Surface::Background { owner_addressed: b_addressed, .. } => *b_addressed,
                    Surface::OwnerDelegate { .. } => true,
                    Surface::OwnerDirect => true,
                    _ => false,
                }
            }
        },
        Surface::OwnerDelegate { .. } | Surface::Direct { .. } | Surface::Channel { .. } => {
            match b {
                Surface::OwnerDirect => true,
                _ => false,
            }
        },
        Surface::OwnerDirect => false,
    }
}

/// TurnSurface::tighten: returns whether the surface changed.
pub fn tighten(cur: &mut Surface, to: Surface) -> (changed: bool)
    ensures
        *final(cur) == tighten_spec(*old(cur), to),
        le_spec(*final(cur), *old(cur)),
{
    proof {
        le_reflexive(*old(cur));
    }
    if surface_eq(&to, cur) {
        return false;
    }
    if le(&to, cur) {
        *cur = to;
        true
    } else {
        false
    }
}

// ---------------------------------------------------------------- proofs

// S1
pub proof fn unknown_and_channel_never_touch_owner(s: Surface)
    requires
        is_unknown_or_channel(s),
    ensures
        !may_read_spec(s, Scope::OwnerPersonal),
        !may_write_spec(s, Scope::OwnerPersonal),
{
}

pub proof fn unknown_touches_nothing(sc: Scope)
    ensures
        !may_read_spec(Surface::Unknown, sc),
        !may_write_spec(Surface::Unknown, sc),
{
}

// S2
pub proof fn background_never_writes(origin: BgOrigin, owner_addressed: bool, sc: Scope)
    ensures
        !may_write_spec(Surface::Background { origin, owner_addressed }, sc),
        may_read_spec(Surface::Background { origin, owner_addressed }, sc) ==> (owner_addressed
            && sc == Scope::OwnerPersonal),
{
}

// S3
pub proof fn fail_closed_app_without_bearer(f: Facts, o: Owner)
    requires
        !f.bg_present,
        f.channel == ChannelKind::Web || f.channel == ChannelKind::Ios,
        !f.bearer,
    ensures
        classify_spec(f, o) == Surface::Unknown,
{
}

pub proof fn fail_closed_mcp_without_proof(f: Facts, o: Owner)
    requires
        f.mcp.is_some(),
        !f.bearer || (f.channel != ChannelKind::Web && f.channel != ChannelKind::Ios)
            || f.bg_present || platform_facts(f),
    ensures
        classify_spec(f, o) == Surface::Unknown,
{
}

pub proof fn mcp_never_owner_direct(f: Facts, o: Owner)
    requires
        f.mcp.is_some(),
    ensures
        classify_spec(f, o) != Surface::OwnerDirect,
{
}

pub proof fn fail_closed_telegram_without_chat(f: Facts, o: Owner)
    requires
        !f.bg_present,
        f.mcp.is_none(),
        f.channel == ChannelKind::Telegram,
        !f.tg_present || f.tg_chat.is_none(),
    ensures
        classify_spec(f, o) == Surface::Unknown,
{
}

pub proof fn fail_closed_discord_without_author(f: Facts, o: Owner)
    requires
        !f.bg_present,
        f.mcp.is_none(),
        f.channel == ChannelKind::Discord,
        !f.dc_present || f.dc_author.is_none(),
    ensures
        classify_spec(f, o) == Surface::Unknown,
{
}

pub proof fn fail_closed_alexa_without_link(f: Facts, o: Owner)
    requires
        !f.bg_present,
        f.mcp.is_none(),
        f.channel == ChannelKind::Alexa,
        !f.ax_present || f.ax_linked.is_none(),
    ensures
        classify_spec(f, o) == Surface::Unknown,
{
}

pub proof fn fail_closed_mixed_background(f: Facts, o: Owner)
    requires
        f.bg_present,
        f.mcp.is_some() || platform_facts(f),
    ensures
        classify_spec(f, o) == Surface::Unknown,
{
}

pub proof fn fail_closed_other_channel(f: Facts, o: Owner)
    requires
        !f.bg_present,
        f.channel == ChannelKind::Other,
    ensures
        classify_spec(f, o) == Surface::Unknown,
{
}

/// The most restrictive input (TurnFacts::default()).
pub open spec fn default_facts() -> Facts {
    Facts {
        channel: ChannelKind::Other,
        bearer: false,
        mcp: None,
        tg_present: false,
        tg_chat: None,
        tg_kind: ChatKind::Other,
        tg_from: None,
        dc_present: false,
        dc_author: None,
        dc_guild: None,
        dc_channel: None,
        ax_present: false,
        ax_linked: None,
        bg_present: false,
        bg_origin: BgOrigin::Cron,
        owner_addressed: false,
    }
}

pub proof fn fail_closed_default_facts(o: Owner)
    ensures
        classify_spec(default_facts(), o) == Surface::Unknown,
{
}

pub proof fn discord_guild_never_owner_direct(f: Facts, o: Owner)
    requires
        f.dc_present,
        f.dc_guild.is_some(),
    ensures
        classify_spec(f, o) != Surface::OwnerDirect,
{
}

pub proof fn telegram_group_never_owner_direct(f: Facts, o: Owner)
    requires
        f.tg_present,
        f.tg_kind != ChatKind::Private,
    ensures
        classify_spec(f, o) != Surface::OwnerDirect,
{
}

/// OwnerDirect needs a positive proof.
pub proof fn owner_direct_needs_proof(f: Facts, o: Owner)
    requires
        classify_spec(f, o) == Surface::OwnerDirect,
    ensures
        !f.bg_present,
        f.mcp.is_none(),
        ((f.channel == ChannelKind::Web || f.channel == ChannelKind::Ios) && f.bearer
            && !platform_facts(f)) || (f.channel == ChannelKind::Telegram && f.tg_present
            && f.tg_kind == ChatKind::Private && same(f.tg_chat, o.tg)) || (f.channel
            == ChannelKind::Discord && f.dc_present && f.dc_guild.is_none() && same(
            f.dc_author,
            o.dc,
        )) || (f.channel == ChannelKind::Alexa && f.ax_present && same(f.ax_linked, o.ax)),
{
}

/// No configured owner ids and no bearer: never OwnerDirect.
pub proof fn no_owner_ids_no_bearer_never_owner_direct(f: Facts)
    requires
        !f.bearer,
    ensures
        classify_spec(f, Owner { tg: None, dc: None, ax: None }) != Surface::OwnerDirect,
{
}

// S4
pub proof fn le_reflexive(a: Surface)
    ensures
        le_spec(a, a),
{
}

pub proof fn le_bottom_top(a: Surface)
    ensures
        le_spec(Surface::Unknown, a),
        le_spec(a, Surface::OwnerDirect),
{
}

pub proof fn le_transitive(a: Surface, b: Surface, c: Surface)
    requires
        le_spec(a, b),
        le_spec(b, c),
    ensures
        le_spec(a, c),
{
}

pub proof fn le_monotone(a: Surface, b: Surface, sc: Scope)
    requires
        le_spec(a, b),
    ensures
        may_read_spec(a, sc) ==> may_read_spec(b, sc),
        may_write_spec(a, sc) ==> may_write_spec(b, sc),
{
}

// S5
pub proof fn tighten_never_loosens(cur: Surface, to: Surface)
    ensures
        le_spec(tighten_spec(cur, to), cur),
        tighten_spec(Surface::Unknown, to) == Surface::Unknown,
{
    le_reflexive(cur);
}

pub proof fn tighten_keeps_policy_within(cur: Surface, to: Surface, sc: Scope)
    ensures
        may_read_spec(tighten_spec(cur, to), sc) ==> may_read_spec(cur, sc),
        may_write_spec(tighten_spec(cur, to), sc) ==> may_write_spec(cur, sc),
{
    tighten_never_loosens(cur, to);
    le_monotone(tighten_spec(cur, to), cur, sc);
}

} // verus!
fn main() {}
