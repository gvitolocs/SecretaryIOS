//! The Lucina Discord bot: the Node `src/discord.js` Lucina persona path on
//! the Rust turn pipeline, fed by the message session of
//! [`lucina_discord::session`] (`LUCINA_DISCORD_ENABLED=true`).
//!
//! Per message, in Node's order ([`lucina_discord::routing`]): bot authors
//! are ignored; outside the admin channels (DMs, `DISCORD_ADMIN_CHANNEL_IDS`,
//! `DISCORD_ADMIN_CHANNEL_NAMES`) a mention gets "I'm currently disabled,
//! ask Poko instead ✨" and anything else silence; then the allow-lists
//! (`DISCORD_ALLOWED_CHANNEL_IDS`, `DISCORD_ALLOWED_USER_IDS`), a DM or a
//! mention, the text without the mention. The turn is
//! [`crate::chat::channel_turn`] with channel `discord`, Honcho session
//! `discord_channel_<channel id>`, peer `discord_user_<id>` (the owner,
//! `LUCINA_OWNER_DISCORD_USER_ID`: the canonical owner peer), planner
//! platform `discord`, reply platform `Discord`. Replies are Discord replies
//! to the message (split at 2000 characters), with the typing indicator
//! while the turn runs.
//!
//! - Timeline: the owner's messages and Lucina's replies to them, channel
//!   `discord`, `meta.isDm`, and `guildName` / `channelName` in servers.
//!   Nobody else's message is recorded; with no owner id configured nothing
//!   is (Node recorded every DM then).
//! - Reminders asked for on Discord go to the owner's Telegram chat
//!   (`LUCINA_OPERATOR_CHAT_ID`), for the owner only; anyone else gets a
//!   short refusal for every reminder action.
//! - `discord_admin_task` ([`admin`]): owner only, Node's operations.
//!
//! Not ported (Node-only actions the planner path answers with "cannot run
//! yet"): image generation, web crawl, Cursor tasks, graphs, spreadsheet
//! updates, Ollama / Qwen / Oracle status; rich multi-message replies (one
//! reply, split only at the length limit); every Poko branch.
pub mod admin;
pub mod testing;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use lucina_discord::cache::{CachedChannel, SharedCache};
use lucina_discord::rest::DiscordRest;
use lucina_discord::routing::{self, Inbound, Route, RoutingConfig, MESSAGE_LIMIT};
use lucina_memory::surface::{ChannelKind as SurfaceChannel, DiscordFacts};
use lucina_memory::{TurnFacts, TurnSurface};
use lucina_store::recent::RecentChat;
use lucina_store::timeline::{Attachment, Meta, TimelineStore, TimelineTurn};
use serde_json::Value;

use crate::chat::{ChannelActionOutcome, ChannelActions, ChannelOrigin, ReplySink};
use crate::AppState;

/// Discord's typing indicator lasts about 10 s.
const TYPING_EVERY: Duration = Duration::from_secs(8);

/// `LUCINA_DISCORD_ENABLED` and Node's Discord variables.
#[derive(Debug, Clone)]
pub struct DiscordConfig {
    pub enabled: bool,
    pub routing: RoutingConfig,
    /// `HERMES_GATEWAY_LOCK_DIR` (default `~/.local/state/hermes-gateway-locks`).
    pub lock_dir: Option<PathBuf>,
    /// `HERMES_GATEWAY_INSTANCE` (default `lucina-rs`): the lease's holder note.
    pub instance: String,
    /// Poko's user id, from `DISCORD_BOT_TOKEN_POKO` (the id only; the token
    /// is not kept): the support setup's `includePoko`.
    pub poko_bot_id: Option<u64>,
    /// `LUCINA_DISCORD_RECENT_PATH` (default `<data>/lucina-rs/recent-discord-chat.json`).
    pub recent_path: PathBuf,
}

impl DiscordConfig {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>, data_dir: &Path) -> Self {
        Self {
            enabled: get("LUCINA_DISCORD_ENABLED")
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("true")),
            routing: RoutingConfig::from_lookup(get),
            lock_dir: get("HERMES_GATEWAY_LOCK_DIR")
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from),
            instance: get("HERMES_GATEWAY_INSTANCE")
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "lucina-rs".into()),
            poko_bot_id: get("DISCORD_BOT_TOKEN_POKO")
                .and_then(|t| lucina_discord::lease::application_id_from_token(&t)),
            recent_path: get("LUCINA_DISCORD_RECENT_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| data_dir.join("lucina-rs/recent-discord-chat.json")),
        }
    }
}

/// `DISCORD_BOT_TOKEN_LUCINA`, else the file `LUCINA_DISCORD_TOKEN_FILE`
/// (default `~/.config/lucina-rs/discord-bot-token-lucina.token`).
pub fn token_from_lookup(get: &impl Fn(&str) -> Option<String>) -> Option<String> {
    get("DISCORD_BOT_TOKEN_LUCINA")
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let path = get("LUCINA_DISCORD_TOKEN_FILE").or_else(|| {
                get("HOME").map(|h| format!("{h}/.config/lucina-rs/discord-bot-token-lucina.token"))
            })?;
            std::fs::read_to_string(path)
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        })
}

/// The owner's message (author id = `LUCINA_OWNER_DISCORD_USER_ID`, not a bot).
pub fn is_owner(owner_id: Option<&str>, m: &Inbound) -> bool {
    !m.author_bot && owner_id.is_some_and(|o| !o.is_empty() && o == m.author_id.to_string())
}

/// The turn's timeline meta, `None` when the message is not recorded
/// (Node `attachDiscordTimeline`, owner messages only).
pub fn timeline_meta(owner_id: Option<&str>, m: &Inbound) -> Option<Meta> {
    if !is_owner(owner_id, m) {
        return None;
    }
    Some(if m.is_dm() {
        Meta {
            is_dm: Some(true),
            ..Meta::default()
        }
    } else {
        Meta {
            is_dm: Some(false),
            guild_name: Some(m.guild_name.clone().unwrap_or_default()),
            channel_name: Some(m.channel_name.clone().unwrap_or_default()),
            ..Meta::default()
        }
    })
}

/// Node `discordAttachmentKind`.
fn attachment_kind(content_type: Option<&str>, name: &str) -> &'static str {
    let t = content_type.unwrap_or("").to_lowercase();
    let n = name.to_lowercase();
    let ext = |exts: &[&str]| exts.iter().any(|e| n.ends_with(e));
    if t.starts_with("image/") || ext(&[".png", ".jpg", ".jpeg", ".gif", ".webp"]) {
        "image"
    } else if t.starts_with("audio/") || ext(&[".ogg", ".mp3", ".m4a", ".wav"]) {
        "voice"
    } else {
        "file"
    }
}

fn attachments(m: &Inbound) -> Vec<Attachment> {
    m.attachments
        .iter()
        .map(|a| Attachment {
            kind: attachment_kind(a.content_type.as_deref(), &a.name).into(),
            name: Some(a.name.clone()),
            ..Attachment::default()
        })
        .collect()
}

/// The bot: the Rust state, the REST client, the session's cache.
pub struct Bot {
    st: AppState,
    rest: Arc<dyn DiscordRest>,
    cfg: DiscordConfig,
    cache: SharedCache,
    recent: Arc<RecentChat>,
    /// For turns that are not recorded on the owner timeline.
    off_timeline: Arc<TimelineStore>,
}

impl Bot {
    pub fn new(st: AppState, rest: Arc<dyn DiscordRest>, cache: SharedCache) -> Arc<Self> {
        let cfg = st.cfg.discord.clone();
        Arc::new(Self {
            recent: Arc::new(RecentChat::new(Some(cfg.recent_path.clone()))),
            off_timeline: Arc::new(TimelineStore::new(
                st.cfg.data_dir.join("lucina-rs/discord-unrecorded.jsonl"),
                1,
                false,
            )),
            cfg,
            rest,
            cache,
            st,
        })
    }

    fn persona(&self) -> &str {
        &self.st.cfg.persona
    }

    fn owner_id(&self) -> Option<&str> {
        self.st.cfg.owner_discord_user_id.as_deref()
    }

    fn is_owner(&self, m: &Inbound) -> bool {
        is_owner(self.owner_id(), m)
    }

    fn surface(&self, m: &Inbound) -> TurnSurface {
        let facts = TurnFacts {
            channel: SurfaceChannel::Discord,
            discord: Some(DiscordFacts {
                author_id: Some(m.author_id.to_string()),
                guild_id: m.guild_id.map(|g| g.to_string()),
                channel_id: Some(m.channel_id.to_string()),
            }),
            ..TurnFacts::default()
        };
        TurnSurface::classify(&facts, &crate::surface::owner_ids(&self.st.cfg))
    }

    fn timeline_for(&self, m: &Inbound) -> (Arc<TimelineStore>, Option<Meta>) {
        match timeline_meta(self.owner_id(), m) {
            Some(meta) => (self.st.timeline.clone(), Some(meta)),
            None => (self.off_timeline.clone(), None),
        }
    }

    /// Node `canonicalizeUserPeerId(discord_user_<id>)`.
    fn peer(&self, m: &Inbound) -> String {
        let owner_peer = self.st.cfg.owner_peer_id.as_str();
        if self.is_owner(m) && !owner_peer.is_empty() {
            owner_peer.to_owned()
        } else {
            format!("discord_user_{}", m.author_id)
        }
    }

    /// The channel and parent names of a guild message the cache does not
    /// know yet (a thread opened before the session connected), over REST.
    async fn fill_names(&self, m: &mut Inbound) {
        let Some(guild) = m.guild_id else {
            return;
        };
        let known = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(m.channel_id);
        if !known {
            if let Ok(c) = self.rest.channel(m.channel_id).await {
                let parent = c.parent_id;
                self.cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .upsert_channel(c.id, CachedChannel::from(&c));
                if let Some(p) = parent {
                    let have = self
                        .cache
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .contains(p);
                    if !have {
                        if let Ok(pc) = self.rest.channel(p).await {
                            self.cache
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .upsert_channel(pc.id, CachedChannel::from(&pc));
                        }
                    }
                }
            }
        }
        if m.guild_name.is_none() {
            let cached = self
                .cache
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .guild_name(guild);
            m.guild_name = match cached {
                Some(n) => Some(n),
                None => match self.rest.guild(guild).await {
                    Ok(g) => {
                        self.cache
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .set_guild(guild, &g.name);
                        Some(g.name)
                    }
                    Err(_) => None,
                },
            };
        }
        let names = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .names(m.channel_id);
        if names.known {
            m.channel_name = names.name;
            m.parent_name = names.parent_name;
            m.channel_kind = names.kind;
        }
    }

    /// One message, as Node's `MessageCreate` handler.
    pub async fn handle(self: Arc<Self>, mut m: Inbound) {
        if m.author_bot {
            return;
        }
        self.fill_names(&mut m).await;
        match routing::route(&self.cfg.routing, &m) {
            Route::Ignore(reason) => {
                tracing::debug!(
                    message_id = m.message_id,
                    channel_id = m.channel_id,
                    reason,
                    "lucina discord message skipped"
                );
            }
            Route::Disabled => {
                tracing::info!(
                    message_id = m.message_id,
                    channel_id = m.channel_id,
                    "lucina discord mention outside the admin channels"
                );
                self.reply(&m, routing::DISABLED_TEXT).await;
            }
            Route::Turn(text) => self.turn(&m, text).await,
        }
    }

    /// Node `message.reply`: a reply to the message, split at 2000.
    async fn reply(&self, m: &Inbound, text: &str) -> bool {
        let mut ok = true;
        for part in routing::split_message(text, MESSAGE_LIMIT) {
            if let Err(e) = self
                .rest
                .send_message(m.channel_id, &part, Some(m.message_id))
                .await
            {
                tracing::warn!(channel_id = m.channel_id, error = %e, "discord reply failed");
                ok = false;
                break;
            }
        }
        ok
    }

    fn typing(&self, channel: u64) -> TypingGuard {
        let rest = self.rest.clone();
        TypingGuard(tokio::spawn(async move {
            loop {
                let _ = rest.typing(channel).await;
                tokio::time::sleep(TYPING_EVERY).await;
            }
        }))
    }

    fn job_sink(&self, channel: u64) -> ReplySink {
        let rest = self.rest.clone();
        Arc::new(move |text: String| {
            let rest = rest.clone();
            Box::pin(async move {
                for part in routing::split_message(&text, MESSAGE_LIMIT) {
                    if let Err(e) = rest.send_message(channel, &part, None).await {
                        tracing::warn!(error = %e, "discord job result not delivered");
                        break;
                    }
                }
            })
        })
    }

    /// `discord_admin_task`, run by the turn before its guard.
    fn channel_actions(self: &Arc<Self>, m: &Inbound) -> ChannelActions {
        let bot = self.clone();
        let m = m.clone();
        Arc::new(move |action: String, plan: Value| {
            if action != "discord_admin_task" {
                return Box::pin(async { None });
            }
            let bot = bot.clone();
            let m = m.clone();
            Box::pin(async move { Some(bot.admin(&m, &plan).await) })
        })
    }

    /// Node's `discord_admin_task` branch: the ack, the operation, the
    /// skill reply (by the turn) or Node's failure text. Owner only.
    async fn admin(&self, m: &Inbound, plan: &Value) -> ChannelActionOutcome {
        let persona = self.persona().to_owned();
        if !self.is_owner(m) {
            tracing::warn!(
                author_id = m.author_id,
                operation = plan["operation"].as_str().unwrap_or(""),
                "discord admin task refused: not the owner"
            );
            return ChannelActionOutcome::Reply(admin::refusal(&persona));
        }
        let ack = plan["ack"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| admin::default_ack(&persona));
        if self.reply(m, &ack).await {
            let (store, meta) = self.timeline_for(m);
            TimelineTurn::new(store, "discord", None)
                .with_meta(meta)
                .record_lucina(&ack, Vec::new(), None);
        }
        let ctx = admin::Ctx {
            guild_id: m.guild_id,
            channel_id: m.channel_id,
            channel_name: m.channel_name.clone(),
            channel_kind: m.channel_kind,
            author_id: m.author_id,
            author_tag: m.author_tag(),
            reference_message_id: m.reference_message_id,
            bot_id: m.bot_id,
        };
        let typing = self.typing(m.channel_id);
        let result = admin::run(&*self.rest, &ctx, plan, self.cfg.poko_bot_id).await;
        drop(typing);
        tracing::info!(
            operation = plan["operation"].as_str().unwrap_or(""),
            ok = result.is_ok(),
            "discord admin task finished"
        );
        match result {
            Ok(result) => ChannelActionOutcome::Skill {
                name: admin::SKILL_NAME.into(),
                result,
                instruction: admin::SKILL_INSTRUCTION.into(),
                memo: "discord admin".into(),
            },
            Err(e) => ChannelActionOutcome::Reply(admin::failure(&persona, &e)),
        }
    }

    /// One Lucina turn through the Rust pipeline, the reply sent as a
    /// Discord reply.
    async fn turn(self: &Arc<Self>, m: &Inbound, text: String) {
        tracing::info!(
            message_id = m.message_id,
            channel_id = m.channel_id,
            author_id = m.author_id,
            is_dm = m.is_dm(),
            "lucina discord message received"
        );
        let owner = self.is_owner(m);
        let surface = self.surface(m);
        let (timeline, timeline_meta) = self.timeline_for(m);
        let persona = self.persona().to_owned();
        let reminder_chat = self
            .st
            .cfg
            .telegram
            .operator_chat_id
            .clone()
            .or_else(|| self.st.cfg.owner_chat_id.clone())
            .unwrap_or_default();
        let origin = ChannelOrigin {
            planner_platform: "discord",
            platform_label: "Discord",
            user_label: m.user_label(),
            extra_system: String::new(),
            peer_id: self.peer(m),
            recent: self.recent.clone(),
            recent_key: m.channel_id.to_string(),
            timeline,
            timeline_meta,
            timeline_text: None,
            user_attachments: if owner { attachments(m) } else { Vec::new() },
            reminder_chat_id: reminder_chat,
            reminder_source: "discord",
            reminders_refused: (!owner)
                .then(|| format!("{persona}: on Discord, reminders are only for my owner.")),
            owner: surface.is_owner(),
            share: false,
            on_job_done: Some(self.job_sink(m.channel_id)),
            channel_actions: Some(self.channel_actions(m)),
        };
        let typing = self.typing(m.channel_id);
        let started = Instant::now();
        let out = crate::chat::channel_turn(
            self.st.clone(),
            text,
            format!("discord_channel_{}", m.channel_id),
            "discord".into(),
            surface,
            origin,
        )
        .await;
        drop(typing);
        let reply = match &out {
            Ok(v) => v["reply"].as_str().unwrap_or("").to_owned(),
            Err(e) => {
                tracing::error!(error = %e.message, "discord turn failed");
                format!("Sono {persona}: ho ricevuto il messaggio, ma qualcosa è andato storto.")
            }
        };
        let reply = if reply.trim().is_empty() {
            "I could not generate a reply.".to_owned()
        } else {
            reply
        };
        self.reply(m, &reply).await;
        tracing::info!(
            message_id = m.message_id,
            channel_id = m.channel_id,
            action = out
                .as_ref()
                .ok()
                .and_then(|v| v["action"].as_str())
                .unwrap_or("error"),
            total_ms = started.elapsed().as_millis() as u64,
            reply_chars = reply.chars().count(),
            "lucina discord turn completed"
        );
    }
}

struct TypingGuard(tokio::task::JoinHandle<()>);

impl Drop for TypingGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Start the message session (`LUCINA_DISCORD_ENABLED=true` with a Lucina
/// bot token). It stops, closing the session and releasing the lease, when
/// `shutdown` turns true. A refused token (4004) or another fatal close
/// stops it without marking it dead: a delivery alert, not a restart loop.
pub fn spawn(
    st: &AppState,
    token: String,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    use lucina_discord::session::{self, OnMessage, SessionConfig, SessionEnd};
    let d = &st.cfg.discord;
    let lock_dir = match &d.lock_dir {
        Some(p) => p.clone(),
        None => lucina_discord::lease::default_lock_dir()?,
    };
    let rest: Arc<dyn DiscordRest> = Arc::new(lucina_discord::twilight_rest::TwilightRest::new(
        token.clone(),
    ));
    let cache = SharedCache::default();
    let bot = Bot::new(st.clone(), rest, cache.clone());
    let on_message: OnMessage = Arc::new(move |m: Inbound| {
        let bot = bot.clone();
        Box::pin(bot.handle(m))
    });
    let cfg = SessionConfig {
        token,
        lock_dir,
        instance: d.instance.clone(),
        lease_retry: session::LEASE_RETRY,
    };
    st.monitor.register(
        "discord",
        crate::delivery_monitor::discord_probe(st.discord_health.clone()),
    );
    let alive = st.supervisor.watch_flag("discord session");
    let health = st.discord_health.clone();
    Ok(tokio::spawn(async move {
        let end = session::run(cfg, health, cache, on_message, shutdown).await;
        let alert_only = matches!(
            end,
            SessionEnd::BadToken | SessionEnd::Fatal(_) | SessionEnd::Config(_)
        );
        if !alert_only {
            alive.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(author: u64, guild: Option<u64>) -> Inbound {
        Inbound {
            message_id: 1,
            channel_id: 10,
            guild_id: guild,
            author_id: author,
            content: "ciao".into(),
            guild_name: guild.map(|_| "Casa".into()),
            channel_name: guild.map(|_| "admin".into()),
            bot_id: 900,
            ..Inbound::default()
        }
    }

    #[test]
    fn config_names_and_defaults() {
        let c = DiscordConfig::from_lookup(&|_: &str| None, Path::new("/d"));
        assert!(!c.enabled);
        assert_eq!(c.instance, "lucina-rs");
        assert_eq!(c.lock_dir, None);
        assert_eq!(c.poko_bot_id, None);
        assert_eq!(
            c.recent_path,
            PathBuf::from("/d/lucina-rs/recent-discord-chat.json")
        );
        let c = DiscordConfig::from_lookup(
            &|k: &str| match k {
                "LUCINA_DISCORD_ENABLED" => Some("true".into()),
                "HERMES_GATEWAY_LOCK_DIR" => Some("/locks".into()),
                "DISCORD_BOT_TOKEN_POKO" => Some("MTIz.x.y".into()),
                "DISCORD_ADMIN_CHANNEL_IDS" => Some("5".into()),
                _ => None,
            },
            Path::new("/d"),
        );
        assert!(c.enabled);
        assert_eq!(c.lock_dir, Some(PathBuf::from("/locks")));
        assert_eq!(c.poko_bot_id, Some(123));
        assert_eq!(c.routing.admin_channel_ids, ["5"]);
    }

    #[test]
    fn token_env_then_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t");
        std::fs::write(&path, " file-token \n").unwrap();
        let p = path.to_string_lossy().into_owned();
        assert_eq!(
            token_from_lookup(&|k: &str| (k == "LUCINA_DISCORD_TOKEN_FILE").then(|| p.clone())),
            Some("file-token".into())
        );
        assert_eq!(
            token_from_lookup(&|k: &str| match k {
                "LUCINA_DISCORD_TOKEN_FILE" => Some(p.clone()),
                "DISCORD_BOT_TOKEN_LUCINA" => Some(" env-token ".into()),
                _ => None,
            }),
            Some("env-token".into())
        );
        assert_eq!(
            token_from_lookup(&|k: &str| (k == "HOME").then(|| "/nonexistent".into())),
            None
        );
    }

    #[test]
    fn timeline_items_for_the_owner_only() {
        let owner = Some("42");
        let dm = timeline_meta(owner, &msg(42, None)).unwrap();
        assert_eq!(dm.is_dm, Some(true));
        assert_eq!(dm.guild_name, None);
        let g = timeline_meta(owner, &msg(42, Some(5))).unwrap();
        assert_eq!(g.is_dm, Some(false));
        assert_eq!(g.guild_name.as_deref(), Some("Casa"));
        assert_eq!(g.channel_name.as_deref(), Some("admin"));
        // Someone else, in a DM or in a server: never recorded.
        assert!(timeline_meta(owner, &msg(77, None)).is_none());
        assert!(timeline_meta(owner, &msg(77, Some(5))).is_none());
        // A bot with the owner's id (impossible on Discord, but still not).
        let mut b = msg(42, None);
        b.author_bot = true;
        assert!(timeline_meta(owner, &b).is_none());
        // No owner configured: nothing is recorded.
        assert!(timeline_meta(None, &msg(42, None)).is_none());
    }

    #[test]
    fn attachment_kinds() {
        assert_eq!(attachment_kind(Some("image/png"), "a"), "image");
        assert_eq!(attachment_kind(None, "x.JPG"), "image");
        assert_eq!(attachment_kind(Some("audio/ogg"), "a"), "voice");
        assert_eq!(attachment_kind(None, "memo.m4a"), "voice");
        assert_eq!(attachment_kind(Some("application/pdf"), "a.pdf"), "file");
    }
}
