//! The Rust Lucina server.
//!
//! - `/api/secretary/*`: identical to Lucina `docs/secretary-api.md` (Node
//!   main plus the shopping v2 and device bridge sections), same bearer token.
//! - `/health`: unauthenticated liveness and shadow-mode report.
//!
//! Shadow mode (the default): the reminder scheduler, Telegram and Discord
//! are off; reminder writes are forwarded to the Node Lucina; Honcho is read
//! only. See `README` in the crate docs of [`config`].

pub mod api;
pub mod chat;
pub mod config;
pub mod fake;
pub mod planner;
pub mod prompts;

use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use lucina_grocery::{Catalog, GroceryRepo};
use lucina_llm::{ChatModel, DeepSeek, UsageTotals};
use lucina_memory::Memory;
use lucina_store::device::DeviceStore;
use lucina_store::recent::RecentChat;
use lucina_store::{JobStore, ReminderStore, TimelineStore};
use lucina_tools::pending::{PendingStore, DEFAULT_TTL};
use lucina_tools::reminder_writer::{LocalWriter, NodeForwarder, ReminderWriter};
use lucina_tools::{Registry, ToolEnv};
use serde_json::Value;

pub use config::{Config, PlannerMode, WriterKind};

pub type SharedChat = futures_util::future::Shared<
    futures_util::future::BoxFuture<'static, Result<Value, api::ApiError>>,
>;

/// Reports whether the DeepSeek key is available (never the key).
pub type KeyProbe = Box<dyn Fn() -> bool + Send + Sync>;

pub struct Inner {
    pub cfg: Config,
    pub timeline: Arc<TimelineStore>,
    pub reminders: Arc<ReminderStore>,
    pub writer: Arc<dyn ReminderWriter>,
    pub jobs: JobStore,
    pub device: DeviceStore,
    pub memory: Arc<Memory>,
    pub model: Arc<dyn ChatModel>,
    pub usage: Option<Arc<UsageTotals>>,
    pub deepseek_has_key: KeyProbe,
    pub grocery: Option<Arc<GroceryRepo>>,
    pub tool_env: Arc<ToolEnv>,
    pub registry: Arc<Registry>,
    pub pending: Arc<PendingStore>,
    pub recent: Arc<RecentChat>,
    pub planner: planner::Planner,
    pub client_messages: Mutex<HashMap<String, (Instant, SharedChat)>>,
    pub briefings: tokio::sync::Mutex<HashMap<String, api::Briefing>>,
    pub streams: AtomicUsize,
    pub started: Instant,
}

#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl AppState {
    /// Wire everything from the config. `model: None` uses DeepSeek.
    pub fn build(cfg: Config, model: Option<Arc<dyn ChatModel>>) -> anyhow::Result<Self> {
        let (model, usage, has_key): (Arc<dyn ChatModel>, Option<Arc<UsageTotals>>, KeyProbe) =
            match model {
                Some(m) => (m, None, Box::new(|| true)),
                None => {
                    let ds = Arc::new(DeepSeek::new(cfg.deepseek.clone())?);
                    let usage = ds.usage.clone();
                    let probe = ds.clone();
                    (ds, Some(usage), Box::new(move || probe.has_key()))
                }
            };
        let timeline = Arc::new(TimelineStore::new(
            &cfg.timeline_path,
            cfg.timeline_max_bytes,
            cfg.timeline_enabled,
        ));
        let reminders = Arc::new(ReminderStore::new(
            &cfg.reminders_path,
            cfg.reminder_max_stored,
        ));
        let writer: Arc<dyn ReminderWriter> = match cfg.reminders_writer {
            WriterKind::Local => Arc::new(LocalWriter(reminders.clone())),
            WriterKind::Node => Arc::new(NodeForwarder::new(&cfg.node_url, cfg.node_token.clone())),
        };
        let grocery = match &cfg.grocery_database_url {
            Some(url) => {
                let catalog = Catalog::from_paths(Catalog::default_paths(
                    cfg.shopping_catalog_path.clone(),
                    &cfg.data_dir,
                ));
                Some(Arc::new(GroceryRepo::connect_lazy(url, catalog)?))
            }
            None => None,
        };
        let memory = Arc::new(Memory::new(cfg.memory_mode, cfg.honcho.clone())?);
        let mut env = ToolEnv::new(&cfg.data_dir, cfg.time_zone);
        env.reminders = Some(reminders.clone());
        env.reminder_writer = Some(writer.clone());
        env.reminders_enabled = cfg.reminders_enabled;
        env.reminder_default_hour = cfg.reminder_default_hour;
        env.reminder_default_minute = cfg.reminder_default_minute;
        env.owner_chat_id = cfg.owner_chat_id.clone();
        env.owner_peer_id = cfg.owner_peer_id.clone();
        env.persona = cfg.persona.clone();
        env.grocery = grocery.clone();
        env.grocery_user_id = cfg.grocery_user_id.clone();
        env.endpoints = cfg.endpoints.clone();
        env.default_location = cfg.default_location.clone();
        env.nominatim_email = cfg.nominatim_email.clone();
        let registry = Arc::new(Registry::builtin(&cfg.tools));
        let prompts = prompts::Prompts::load(&cfg);
        let inner = Inner {
            timeline,
            reminders,
            writer,
            jobs: JobStore::new(&cfg.jobs_path),
            device: DeviceStore::new(
                &cfg.data_dir,
                cfg.device_retention_days,
                cfg.device_command_ttl_ms,
                cfg.device_reachable_minutes,
            ),
            memory,
            planner: planner::Planner {
                model: model.clone(),
                prompts,
            },
            model,
            usage,
            deepseek_has_key: has_key,
            grocery,
            tool_env: Arc::new(env),
            registry,
            pending: Arc::new(PendingStore::file(
                cfg.data_dir.join("lucina-rs/pending-actions.json"),
                DEFAULT_TTL,
            )),
            recent: Arc::new(RecentChat::new(Some(
                cfg.data_dir.join("lucina-rs/recent-chat.json"),
            ))),
            client_messages: Mutex::new(HashMap::new()),
            briefings: tokio::sync::Mutex::new(HashMap::new()),
            streams: AtomicUsize::new(0),
            started: Instant::now(),
            cfg,
        };
        Ok(Self(Arc::new(inner)))
    }

    /// Background tasks: the reminder scheduler only when `LUCINA_SCHEDULER=true`.
    pub fn spawn_background(&self) {
        if self.cfg.telegram_requested || self.cfg.discord_requested {
            tracing::warn!("LUCINA_TELEGRAM_ENABLED/LUCINA_DISCORD_ENABLED: channels are not part of this build; they stay off");
        }
        if self.cfg.scheduler {
            if self.cfg.reminders_writer == WriterKind::Node {
                tracing::warn!("LUCINA_SCHEDULER=true ignored: reminders.json is owned by the Node Lucina (LUCINA_REMINDERS_WRITER=node)");
                return;
            }
            let sink = Arc::new(lucina_store::scheduler::TimelineSink {
                timeline: self.timeline.clone(),
                owner_chat_id: self.cfg.owner_chat_id.clone().unwrap_or_default(),
                owner_peer_id: self.cfg.owner_peer_id.clone(),
            });
            lucina_store::scheduler::spawn(
                self.reminders.clone(),
                sink,
                self.cfg.persona.clone(),
                self.cfg.scheduler_poll,
            );
        }
    }
}

/// The full router.
pub fn app(state: AppState) -> axum::Router {
    api::router(state)
}
