//! The gateway loop (twilight). One shard, intents `GUILDS | GUILD_PRESENCES`
//! only: no message events reach this session, so it can never reply. The
//! Node Lucina keeps its own session for messages (two sessions per bot
//! token are allowed).
//!
//! Identify and resume: twilight resumes on reconnect when the session is
//! still valid and re-identifies otherwise, through its in-memory queue (one
//! identify per 5 s, 1000 a day). Fatal close codes (bad token, disallowed
//! intents) end the loop with [`WatchError::Fatal`], which the binary maps
//! to exit code 78 so systemd does not restart into an identify loop.
//!
//! Logs never contain gateway payloads: only event kinds, close codes,
//! guild counts and the owner's status word.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use futures_util::StreamExt;
use twilight_gateway::{CloseFrame, ConfigBuilder, Intents, Message, Shard, ShardId};
use twilight_model::gateway::payload::outgoing::identify::IdentifyProperties;
use twilight_model::gateway::payload::outgoing::RequestGuildMembers;
use twilight_model::id::Id;

use crate::engine::{Engine, Outbound};
use crate::store::{PresenceStore, DEFAULT_MAX_AGE};

/// Close codes after which reconnecting cannot help.
pub const FATAL_CLOSE_CODES: &[u16] = &[4004, 4010, 4011, 4012, 4013, 4014];

#[derive(Clone)]
pub struct WatchConfig {
    pub token: String,
    pub owner_id: u64,
    pub presence_dir: PathBuf,
    pub retention_days: i64,
    /// `discord.json` refresh period.
    pub refresh: Duration,
    /// With nothing from any guild after this, the owner is offline.
    pub first_observation_timeout: Duration,
}

impl std::fmt::Debug for WatchConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchConfig")
            .field("token", &"<set>")
            .field("owner_id", &self.owner_id)
            .field("presence_dir", &self.presence_dir)
            .field("retention_days", &self.retention_days)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub enum WatchError {
    /// The gateway closed with a code that a reconnect cannot fix.
    Fatal(Option<u16>),
    Io(std::io::Error),
}

impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WatchError::Fatal(code) => write!(f, "gateway closed fatally (close code {code:?})"),
            WatchError::Io(e) => write!(f, "presence store: {e}"),
        }
    }
}

impl std::error::Error for WatchError {}

pub fn intents() -> Intents {
    Intents::GUILDS | Intents::GUILD_PRESENCES
}

/// Run until `shutdown` resolves (then log the stop and close the session)
/// or the gateway closes fatally.
pub async fn run(cfg: WatchConfig, shutdown: impl Future<Output = ()>) -> Result<(), WatchError> {
    let store = PresenceStore::new(&cfg.presence_dir, cfg.retention_days);
    let mut engine = Engine::start(
        cfg.owner_id.to_string(),
        store,
        Utc::now(),
        DEFAULT_MAX_AGE,
        cfg.first_observation_timeout,
    )
    .map_err(WatchError::Io)?;
    let config = ConfigBuilder::new(cfg.token.clone(), intents())
        .identify_properties(IdentifyProperties::new(
            "lucina-presence",
            "lucina-presence",
            std::env::consts::OS,
        ))
        .build();
    let mut shard = Shard::with_config(ShardId::ONE, config);
    tracing::info!(
        owner = cfg.owner_id,
        "presence watcher connecting (intents GUILDS | GUILD_PRESENCES)"
    );
    let mut tick = tokio::time::interval(cfg.refresh);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tokio::pin!(shutdown);
    let mut last_close: Option<u16> = None;
    loop {
        tokio::select! {
            () = &mut shutdown => {
                tracing::info!("presence watcher stopping");
                engine.stop(Utc::now());
                shard.close(CloseFrame::NORMAL);
                // Drain until the close completes (bounded).
                let _ = tokio::time::timeout(Duration::from_secs(3), async {
                    while let Some(m) = shard.next().await {
                        if matches!(m, Ok(Message::Close(_))) {
                            break;
                        }
                    }
                })
                .await;
                return Ok(());
            }
            _ = tick.tick() => engine.tick(Utc::now()),
            msg = shard.next() => match msg {
                None => {
                    engine.stop(Utc::now());
                    return Err(WatchError::Fatal(last_close));
                }
                Some(Err(e)) => {
                    // Kind only: the error may carry a raw event.
                    let kind = match e.kind() {
                        twilight_gateway::error::ReceiveMessageErrorType::Deserializing { .. } => "deserializing",
                        twilight_gateway::error::ReceiveMessageErrorType::Reconnect => "reconnect",
                        _ => "other",
                    };
                    tracing::warn!(kind, "gateway receive error");
                }
                Some(Ok(Message::Close(frame))) => {
                    let code = frame.as_ref().map(|f| f.code);
                    last_close = code;
                    if code.is_some_and(|c| FATAL_CLOSE_CODES.contains(&c)) {
                        tracing::error!(?code, "gateway closed with a fatal code");
                    } else {
                        tracing::info!(?code, "gateway closed; twilight resumes or re-identifies");
                    }
                }
                Some(Ok(Message::Text(text))) => {
                    for out in engine.handle_text(&text, Utc::now()) {
                        match out {
                            Outbound::RequestOwner { guild_id } => {
                                let (Some(g), Some(u)) = (Id::new_checked(guild_id), Id::new_checked(cfg.owner_id)) else {
                                    continue;
                                };
                                let req = RequestGuildMembers::builder(g).presences(true).user_id(u);
                                shard.command(&req);
                            }
                        }
                    }
                }
            },
        }
    }
}
