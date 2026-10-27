//! The Lucina message session: one gateway shard with intents
//! `GUILDS | GUILD_MESSAGES | DIRECT_MESSAGES | MESSAGE_CONTENT` (Node's
//! Lucina client), next to the presence watcher's separate read-only
//! session on the same token (Discord allows several sessions per token).
//!
//! [`run`] (feature `gateway`):
//!
//! 1. takes the lease ([`crate::lease`]), retrying every 5 s while another
//!    session holds it; it never connects without it;
//! 2. connects and keeps the session (twilight heartbeats, resumes or
//!    re-identifies after a non-fatal close);
//! 3. hands every `MESSAGE_CREATE` to the handler in its own task, with the
//!    channel and guild names from [`crate::cache`];
//! 4. ends on shutdown (closing the session, then releasing the lease) or on
//!    a fatal close code. 4004 (the token was refused) ends with
//!    [`SessionEnd::BadToken`]: an alert, not a restart loop.
//!
//! [`SessionHealth`] is what `/health` shows under `discord` and what the
//! delivery monitor's probe reads. Logs never carry message content or the
//! token: event kinds, ids, close codes.

use std::sync::{Arc, Mutex};

use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHealth {
    pub enabled: bool,
    /// `off`, `lease_wait`, `connecting`, `connected`, `reconnecting`,
    /// `unauthorized` (4004), `fatal`, `stopped`.
    pub state: String,
    pub lease_held: bool,
    pub connected: bool,
    /// The last gateway event of any kind, heartbeat acknowledgements included.
    pub last_event_at: Option<String>,
    pub last_error: Option<String>,
    /// Non-fatal closes since the start.
    pub reconnects: u64,
    pub bot_user_id: Option<String>,
    /// Who holds the lease while this session waits for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_holder: Option<String>,
    pub messages: u64,
}

pub type SharedHealth = Arc<Mutex<SessionHealth>>;

pub fn update(health: &SharedHealth, f: impl FnOnce(&mut SessionHealth)) {
    f(&mut health.lock().unwrap_or_else(|e| e.into_inner()));
}

#[cfg(feature = "gateway")]
pub use gateway::*;

#[cfg(feature = "gateway")]
mod gateway {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use futures_util::future::BoxFuture;
    use twilight_gateway::{
        CloseFrame, ConfigBuilder, Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _,
    };
    use twilight_model::channel::Message;
    use twilight_model::gateway::payload::incoming::GuildCreate;
    use twilight_model::gateway::payload::outgoing::identify::IdentifyProperties;

    use super::{update, SharedHealth};
    use crate::cache::{CachedChannel, SharedCache};
    use crate::lease::{application_id_from_token, read_holder, DiscordLease};
    use crate::routing::{Inbound, InboundAttachment};
    use crate::twilight_rest::{channel_info, kind_of};
    use crate::watcher::FATAL_CLOSE_CODES;

    /// Lease retry while another session holds it.
    pub const LEASE_RETRY: Duration = Duration::from_secs(5);

    pub fn intents() -> Intents {
        Intents::GUILDS
            | Intents::GUILD_MESSAGES
            | Intents::DIRECT_MESSAGES
            | Intents::MESSAGE_CONTENT
    }

    pub type OnMessage = Arc<dyn Fn(Inbound) -> BoxFuture<'static, ()> + Send + Sync>;

    #[derive(Clone)]
    pub struct SessionConfig {
        pub token: String,
        pub lock_dir: PathBuf,
        /// The holder note's instance (`HERMES_GATEWAY_INSTANCE`).
        pub instance: String,
        pub lease_retry: Duration,
    }

    impl std::fmt::Debug for SessionConfig {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SessionConfig")
                .field("token", &"<set>")
                .field("lock_dir", &self.lock_dir)
                .field("instance", &self.instance)
                .finish_non_exhaustive()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum SessionEnd {
        Shutdown,
        /// Close code 4004: the token was refused.
        BadToken,
        /// Another fatal close code (disallowed intents, …): a restart cannot fix it.
        Fatal(Option<u16>),
        /// The shard stream ended without a fatal close code.
        Ended(Option<u16>),
        /// The token names no application id.
        Config(String),
    }

    fn now_iso() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    /// Resolves when shutdown is asked (or its sender is gone).
    async fn stopped(shutdown: &mut tokio::sync::watch::Receiver<bool>) {
        let _ = shutdown.wait_for(|s| *s).await;
    }

    pub async fn run(
        cfg: SessionConfig,
        health: SharedHealth,
        cache: SharedCache,
        on_message: OnMessage,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> SessionEnd {
        update(&health, |h| {
            h.enabled = true;
            h.state = "lease_wait".into();
        });
        let Some(app_id) = application_id_from_token(&cfg.token) else {
            let e = "the Discord token names no application id".to_owned();
            update(&health, |h| {
                h.state = "fatal".into();
                h.last_error = Some(e.clone());
            });
            return SessionEnd::Config(e);
        };
        let mut waiting_logged = false;
        let lease = loop {
            if *shutdown.borrow() {
                update(&health, |h| h.state = "stopped".into());
                return SessionEnd::Shutdown;
            }
            match DiscordLease::try_acquire(&cfg.lock_dir, app_id, &cfg.instance) {
                Ok(Some(l)) => break l,
                Ok(None) => {
                    let holder = read_holder(&cfg.lock_dir, app_id);
                    if !waiting_logged {
                        tracing::info!(
                            application_id = app_id,
                            holder = holder.as_deref().unwrap_or("unknown"),
                            "discord lease held elsewhere; retrying every 5 s"
                        );
                        waiting_logged = true;
                    }
                    update(&health, |h| {
                        h.lease_held = false;
                        h.state = "lease_wait".into();
                        h.lease_holder = holder;
                        h.last_error = Some("another session holds the Discord lease".into());
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "discord lease unavailable");
                    update(&health, |h| h.last_error = Some(e.to_string()));
                }
            }
            tokio::select! {
                () = tokio::time::sleep(cfg.lease_retry) => {}
                () = stopped(&mut shutdown) => {
                    update(&health, |h| h.state = "stopped".into());
                    return SessionEnd::Shutdown;
                }
            }
        };
        tracing::info!(lock = %lease.path().display(), "discord lease acquired");
        update(&health, |h| {
            h.lease_held = true;
            h.lease_holder = None;
            h.last_error = None;
            h.state = "connecting".into();
        });
        let end = connected(&cfg, &health, &cache, &on_message, &mut shutdown).await;
        drop(lease);
        update(&health, |h| {
            h.lease_held = false;
            h.connected = false;
            h.state = match &end {
                SessionEnd::Shutdown => "stopped",
                SessionEnd::BadToken => "unauthorized",
                _ => "fatal",
            }
            .into();
        });
        tracing::info!(?end, "discord message session ended; lease released");
        end
    }

    fn wanted() -> EventTypeFlags {
        EventTypeFlags::READY
            | EventTypeFlags::RESUMED
            | EventTypeFlags::MESSAGE_CREATE
            | EventTypeFlags::GUILD_CREATE
            | EventTypeFlags::GUILD_UPDATE
            | EventTypeFlags::GUILD_DELETE
            | EventTypeFlags::CHANNEL_CREATE
            | EventTypeFlags::CHANNEL_UPDATE
            | EventTypeFlags::CHANNEL_DELETE
            | EventTypeFlags::THREAD_CREATE
            | EventTypeFlags::THREAD_UPDATE
            | EventTypeFlags::THREAD_DELETE
            | EventTypeFlags::GATEWAY_HEARTBEAT_ACK
            | EventTypeFlags::GATEWAY_HELLO
            | EventTypeFlags::GATEWAY_RECONNECT
            | EventTypeFlags::GATEWAY_INVALIDATE_SESSION
    }

    async fn connected(
        cfg: &SessionConfig,
        health: &SharedHealth,
        cache: &SharedCache,
        on_message: &OnMessage,
        shutdown: &mut tokio::sync::watch::Receiver<bool>,
    ) -> SessionEnd {
        let config = ConfigBuilder::new(cfg.token.clone(), intents())
            .identify_properties(IdentifyProperties::new(
                "lucina-rs",
                "lucina-rs",
                std::env::consts::OS,
            ))
            .build();
        let mut shard = Shard::with_config(ShardId::ONE, config);
        tracing::info!("discord message session connecting (intents GUILDS | GUILD_MESSAGES | DIRECT_MESSAGES | MESSAGE_CONTENT)");
        let mut bot_id: u64 = 0;
        let mut last_close: Option<u16> = None;
        loop {
            let item = tokio::select! {
                () = stopped(shutdown) => {
                    tracing::info!("discord message session stopping");
                    shard.close(CloseFrame::NORMAL);
                    let _ = tokio::time::timeout(Duration::from_secs(3), async {
                        while let Some(e) = shard.next_event(EventTypeFlags::empty()).await {
                            if matches!(e, Ok(Event::GatewayClose(_))) {
                                break;
                            }
                        }
                    })
                    .await;
                    return SessionEnd::Shutdown;
                }
                item = shard.next_event(wanted()) => item,
            };
            let event = match item {
                None => return SessionEnd::Ended(last_close),
                Some(Err(e)) => {
                    let kind = match e.kind() {
                        twilight_gateway::error::ReceiveMessageErrorType::Deserializing {
                            ..
                        } => "deserializing",
                        twilight_gateway::error::ReceiveMessageErrorType::Reconnect => "reconnect",
                        _ => "other",
                    };
                    tracing::warn!(kind, "discord gateway receive error");
                    update(health, |h| {
                        h.last_error = Some(format!("gateway receive error ({kind})"))
                    });
                    continue;
                }
                Some(Ok(e)) => e,
            };
            if !matches!(event, Event::GatewayClose(_)) {
                update(health, |h| h.last_event_at = Some(now_iso()));
            }
            match event {
                Event::Ready(r) => {
                    bot_id = r.user.id.get();
                    tracing::info!(
                        bot_user_id = bot_id,
                        guilds = r.guilds.len(),
                        "discord message session ready"
                    );
                    update(health, |h| {
                        h.connected = true;
                        h.state = "connected".into();
                        h.bot_user_id = Some(bot_id.to_string());
                        h.last_error = None;
                    });
                }
                Event::Resumed => update(health, |h| {
                    h.connected = true;
                    h.state = "connected".into();
                }),
                Event::GatewayClose(frame) => {
                    let code = frame.as_ref().map(|f| f.code);
                    last_close = code;
                    if code == Some(4004) {
                        tracing::error!(
                            "discord gateway refused the token (4004); the session stops"
                        );
                        update(health, |h| {
                            h.connected = false;
                            h.last_error =
                                Some("the Discord token was refused (close code 4004)".into());
                        });
                        return SessionEnd::BadToken;
                    }
                    if code.is_some_and(|c| FATAL_CLOSE_CODES.contains(&c)) {
                        tracing::error!(?code, "discord gateway closed with a fatal code");
                        update(health, |h| {
                            h.connected = false;
                            h.last_error = Some(format!("gateway closed fatally (code {code:?})"));
                        });
                        return SessionEnd::Fatal(code);
                    }
                    tracing::info!(
                        ?code,
                        "discord gateway closed; twilight resumes or re-identifies"
                    );
                    update(health, |h| {
                        h.connected = false;
                        h.state = "reconnecting".into();
                        h.reconnects += 1;
                    });
                }
                Event::GuildCreate(g) => {
                    if let GuildCreate::Available(guild) = *g {
                        let mut c = cache.lock().unwrap_or_else(|e| e.into_inner());
                        c.set_guild(guild.id.get(), &guild.name);
                        for ch in guild.channels.iter().chain(guild.threads.iter()) {
                            let mut info = channel_info(ch);
                            info.guild_id.get_or_insert(guild.id.get());
                            c.upsert_channel(info.id, CachedChannel::from(&info));
                        }
                    }
                }
                Event::GuildUpdate(g) => cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .set_guild(g.id.get(), &g.name),
                Event::GuildDelete(g) => {
                    if g.unavailable != Some(true) {
                        cache
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove_guild(g.id.get());
                    }
                }
                Event::ChannelCreate(c) => upsert(cache, &c.0),
                Event::ChannelUpdate(c) => upsert(cache, &c.0),
                Event::ThreadCreate(c) => upsert(cache, &c.0),
                Event::ThreadUpdate(c) => upsert(cache, &c.0),
                Event::ChannelDelete(c) => cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove_channel(c.0.id.get()),
                Event::ThreadDelete(t) => cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove_channel(t.id.get()),
                Event::MessageCreate(m) => {
                    if bot_id != 0 && m.author.id.get() == bot_id {
                        continue;
                    }
                    let inbound = inbound(&m.0, bot_id, cache);
                    update(health, |h| h.messages += 1);
                    tracing::debug!(
                        message_id = inbound.message_id,
                        channel_id = inbound.channel_id,
                        author_id = inbound.author_id,
                        is_dm = inbound.is_dm(),
                        "discord message received"
                    );
                    tokio::spawn(on_message(inbound));
                }
                _ => {}
            }
        }
    }

    fn upsert(cache: &SharedCache, c: &twilight_model::channel::Channel) {
        let info = channel_info(c);
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .upsert_channel(info.id, CachedChannel::from(&info));
    }

    /// A `MESSAGE_CREATE` as the handler sees it.
    pub fn inbound(m: &Message, bot_id: u64, cache: &SharedCache) -> Inbound {
        let (names, guild_name) = {
            let c = cache.lock().unwrap_or_else(|e| e.into_inner());
            (
                c.names(m.channel_id.get()),
                m.guild_id.and_then(|g| c.guild_name(g.get())),
            )
        };
        Inbound {
            message_id: m.id.get(),
            channel_id: m.channel_id.get(),
            guild_id: m.guild_id.map(|g| g.get()),
            author_id: m.author.id.get(),
            author_bot: m.author.bot,
            author_username: m.author.name.clone(),
            author_global_name: m.author.global_name.clone(),
            author_discriminator: m.author.discriminator,
            content: m.content.clone(),
            mentions: m.mentions.iter().map(|u| u.id.get()).collect(),
            reference_message_id: m
                .reference
                .as_ref()
                .and_then(|r| r.message_id)
                .map(|i| i.get()),
            attachments: m
                .attachments
                .iter()
                .map(|a| InboundAttachment {
                    name: a.filename.clone(),
                    content_type: a.content_type.clone(),
                })
                .collect(),
            timestamp_ms: Some(m.timestamp.as_micros() / 1000),
            guild_name,
            channel_name: names.name,
            parent_name: names.parent_name,
            channel_kind: if m.guild_id.is_none() {
                crate::rest::ChannelKind::Dm
            } else if names.known {
                names.kind
            } else {
                kind_of(twilight_model::channel::ChannelType::GuildText)
            },
            bot_id,
        }
    }
}
