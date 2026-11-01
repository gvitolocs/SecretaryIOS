//! The Rust Lucina server.
//!
//! - `/api/secretary/*`: identical to Lucina `docs/secretary-api.md` (Node
//!   main plus the shopping v2 and device bridge sections), same bearer token.
//! - `/health`: unauthenticated liveness and shadow-mode report.
//!
//! Shadow mode (the default): the reminder scheduler, Telegram and Discord
//! are off; reminder writes are forwarded to the Node Lucina; Honcho is read
//! only. See `README` in the crate docs of [`config`].

pub mod agenda;
pub mod agentd;
pub mod api;
pub mod au;
pub mod callers;
pub mod bot_hospital;
pub mod bridges;
pub mod capabilities;
pub mod chat;
pub mod computer;
pub mod computer_files;
pub mod computer_view;
pub mod config;
pub mod context;
pub mod decide;
pub mod cua_wire;
pub mod deals;
pub mod delegation;
pub mod delivery_monitor;
pub mod desk;
pub mod discord;
pub mod fake;
pub mod geocode;
pub mod jev_drift;
pub mod jev_route;
pub mod jev_yesno;
pub mod grocery_turn;
pub mod jobs;
pub mod notebooks;
pub mod lan;
pub mod ops;
pub mod otp;
pub mod owner_secrets;
pub mod parity;
pub mod places;
pub mod planner;
pub mod prompter;
pub mod prompts;
pub mod reach;
pub mod reminder_delivery;
pub mod rich;
pub mod secrets;
pub mod selftest;
pub mod share;
pub mod shopping_mcp_gate;
pub mod surface;
pub mod telegram;
pub mod video;
pub mod voice;

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
    pub share: crate::share::ShareRuntime,
    pub cfg: Config,
    pub timeline: Arc<TimelineStore>,
    /// Self-test and diagnostic turns (crate::selftest), never the owner's timeline.
    pub selftest_timeline: Arc<TimelineStore>,
    pub reminders: Arc<ReminderStore>,
    pub writer: Arc<dyn ReminderWriter>,
    pub jobs: Arc<JobStore>,
    pub device: DeviceStore,
    pub memory: Arc<Memory>,
    pub model: Arc<dyn ChatModel>,
    pub usage: Option<Arc<UsageTotals>>,
    pub deepseek_has_key: KeyProbe,
    pub grocery: Option<Arc<GroceryRepo>>,
    /// The grocery deals engine (`crate::deals`); `None` without home coordinates.
    pub deals: Option<Arc<lucina_deals::DealsEngine>>,
    /// Coordinates for place blocks (`LUCINA_MAPS_GEOCODE_URL`); `None`: off.
    pub geocoder: Option<Arc<geocode::Geocoder>>,
    pub tool_env: Arc<ToolEnv>,
    pub registry: Arc<Registry>,
    /// The registry for Lucina MCP turns: no owner-only tools (secrets).
    pub mcp_registry: Arc<Registry>,
    /// Trusted MCP turns (`LUCINA_MCP_TRUSTED`): everything but the owner-only tools.
    pub trusted_mcp_registry: Arc<Registry>,
    /// Infisical secrets (`LUCINA_SECRETS_CONFIG`), for tools only.
    pub secrets: Option<Arc<lucina_secrets::SecretStore>>,
    /// Meshy 3D (`LUCINA_MESHY=true`) and its one-tool registry.
    pub meshy: Option<Arc<lucina_meshy::MeshyService>>,
    pub meshy_registry: Option<Arc<Registry>>,
    /// Delegation to Claude, Grok and ChatGPT (`LUCINA_DELEGATE`).
    pub delegator: Option<Arc<lucina_delegate::Delegator>>,
    pub pending: Arc<PendingStore>,
    pub recent: Arc<RecentChat>,
    pub planner: planner::Planner,
    /// secretary-server for the notes, calendar and outfit bridges.
    pub secretary: bridges::Secretary,
    pub bridge_state: bridges::BridgeState,
    pub client_messages: Mutex<HashMap<String, (Instant, SharedChat)>>,
    /// Values the owner put in chat, waiting for "Save as secret" (memory only).
    pub held_secrets: owner_secrets::HeldSecrets,
    pub briefings: tokio::sync::Mutex<HashMap<String, api::Briefing>>,
    pub streams: AtomicUsize,
    pub started: Instant,
    pub scheduler_health: Arc<Mutex<lucina_store::scheduler::SchedulerHealth>>,
    /// The Telegram poller (`/health` → `telegram`).
    pub telegram_health: lucina_telegram::poller::SharedHealth,
    /// Node's `recent-telegram-chat.json`, written only while the Telegram bot runs here.
    pub telegram_recent: Arc<RecentChat>,
    /// The Discord message session (`/health` → `discord`).
    pub discord_health: lucina_discord::session::SharedHealth,
    pub hospital_health: Arc<Mutex<bot_hospital::HospitalHealth>>,
    /// Delivery-path alerts (`delivery_monitor`).
    pub monitor: Arc<delivery_monitor::Monitor>,
    /// Background loops checked by `/health`.
    pub supervisor: Arc<delivery_monitor::Supervisor>,
    /// The ops record (`LUCINA_OPS_VAULT_DIR`, crate::ops); `None`: off.
    pub ops: Option<Arc<lucina_ops::Ops>>,
    /// The Obsidian ingest (`LUCINA_VAULT_INGEST=on`, crate::ops); `None`: off.
    pub ingest: Option<Arc<lucina_vault_ingest::Ingest>>,
    /// The Jev client of the voice fast path when the config sets one (tests); `None`: the shared client.
    pub voice_jev: Option<Arc<lucina_jev::Jev>>,
}

#[derive(Clone)]
pub struct AppState(pub Arc<Inner>);

impl std::ops::Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl Inner {
    /// The timeline a turn of `session` writes: the owner's, or for a self-test its own
    /// (crate::selftest).
    pub fn timeline_for(&self, session: &str) -> Arc<TimelineStore> {
        if selftest::is_session(session) {
            self.selftest_timeline.clone()
        } else {
            self.timeline.clone()
        }
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
        // The model is a sink: every prompt and reply goes through the redactor.
        let model: Arc<dyn ChatModel> = Arc::new(secrets::RedactingModel(model));
        let known = secrets::register_bootstrap(&cfg);
        tracing::info!(
            known,
            "redactor: bootstrap credentials registered (digests only)"
        );
        let secret_store = secrets::open_store(&cfg);
        let timeline = Arc::new(TimelineStore::new(
            &cfg.timeline_path,
            cfg.timeline_max_bytes,
            cfg.timeline_enabled,
        ));
        let selftest_timeline = Arc::new(TimelineStore::new(
            cfg.timeline_path.with_file_name(selftest::TIMELINE_FILE),
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
        let grocery = match connect_grocery(&cfg)? {
            Some(repo) => {
                // New custom shopping items are named in it/en/da by one model call
                // (DeepSeek; LUCINA_SHOPPING_TRANSLATE=off turns it off).
                let namer_model = model.clone();
                let namer: lucina_grocery::Namer = Arc::new(move |system: String, user: String| {
                    let m = namer_model.clone();
                    Box::pin(async move {
                        m.complete(lucina_llm::ChatRequest {
                            messages: vec![
                                lucina_llm::Message::system(system),
                                lucina_llm::Message::user(user),
                            ],
                            temperature: Some(0.1),
                            purpose: "shopping-item-names",
                            ..lucina_llm::ChatRequest::default()
                        })
                        .await
                        .map_err(|e| tracing::warn!(error = %e, "shopping item naming failed"))
                        .ok()
                        .map(|r| r.message.text().to_owned())
                    })
                });
                Some(Arc::new(repo.with_namer(namer)))
            }
            None => None,
        };
        let memory = Arc::new(Memory::new(cfg.memory_mode, cfg.honcho.clone())?);
        let ops = ops::open(&cfg.ops, cfg.time_zone, cfg.memory_mode, &cfg.honcho);
        let ingest = ops::open_ingest(&cfg.ops, cfg.time_zone, ops.as_ref());
        let mut env = ToolEnv::new(&cfg.data_dir, cfg.time_zone);
        env.ops = ops.clone();
        // Messages to Claude, Grok and Lucina Agents (crate::reach); the sink is set below.
        env.reach = reach::build(&cfg.data_dir, ops.clone());
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
        env.presence_dir = cfg.presence_dir.clone();
        env.presence_max_age = cfg.presence_max_age;
        env.secretary_url = cfg.secretary_url.clone();
        env.secretary_service_token_file = cfg.secretary_service_token_file.clone();
        env.secrets = secret_store.clone();
        env.au_url = cfg.au_enabled.then(|| cfg.au_url.clone());
        env.callers_public_url = cfg
            .callers_public_url
            .clone()
            .filter(|_| cfg.callers_enabled);
        env.callers = cfg.callers_enabled.then(|| {
            Arc::new(lucina_tools::tools::callers::CallerStore::in_data_dir(
                &cfg.data_dir,
            ))
        });
        // Instagram / Pixiv plugin services (loopback; gateway secrets are read per call).
        {
            let (c, p) = (&cfg.plugins, &mut env.plugins);
            if let Some(v) = &c.instagram_url {
                p.instagram.url = v.clone();
            }
            if let Some(v) = &c.instagram_secret_file {
                p.instagram.secret_file = v.clone();
            }
            if let Some(v) = &c.pixiv_url {
                p.pixiv.url = v.clone();
            }
            if let Some(v) = &c.pixiv_secret_file {
                p.pixiv.secret_file = v.clone();
            }
            if let Some(v) = &c.public_url {
                p.public_url = v.trim_end_matches('/').to_owned();
                // Plain-text image lines (Telegram) link to the apps' public API.
                lucina_store::rich::set_public_base(v);
            }
        }
        // Voice memos (lucina-memo on this host): on when its service token is readable.
        let memo_token = std::env::var("LUCINA_MEMO_SERVICE_TOKEN_FILE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| std::path::Path::new(&h).join(".config/lucina-memo/service.token"))
            })
            .filter(|p| p.is_file());
        if let Some(token) = memo_token {
            env.memo_url = Some(
                std::env::var("LUCINA_MEMO_URL").unwrap_or_else(|_| "http://127.0.0.1:8795".into()),
            );
            env.memo_token_file = Some(token);
        }
        // Crash reports (lucina-diagnostics on this host): on when its service token is readable.
        let diagnostics_token = std::env::var("LUCINA_DIAGNOSTICS_SERVICE_TOKEN_FILE")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var("HOME").ok().map(|h| {
                    std::path::Path::new(&h).join(".config/lucina-diagnostics/service.token")
                })
            })
            .filter(|p| p.is_file());
        if let Some(token) = diagnostics_token {
            env.diagnostics_url = Some(
                std::env::var("LUCINA_DIAGNOSTICS_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:8794".into()),
            );
            env.diagnostics_token_file = Some(token);
        }
        // Lucina's own Google Drive: only with a grant (`lucina google-auth drive`).
        if cfg.drive_enabled {
            match lucina_tools::google::Drive::open(
                env.http.clone(),
                lucina_tools::google::GoogleEndpoints::default(),
                &cfg.google_client_file,
                &cfg.drive_token_file,
            ) {
                Ok(d) => {
                    tracing::info!(account = d.account(), "Google Drive connected");
                    env.drive = Some(Arc::new(d));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "LUCINA_DRIVE is on but Drive is not usable: tool skipped")
                }
            }
        }
        // Owner approvals: on when secretary-server, the service token and the pinned grant key are all there.
        let approvals = match lucina_approvals::Approvals::new(cfg.approvals.clone()) {
            Ok(a) => {
                tracing::info!("owner approvals: ready (secrets guard for MCP, root_exec)");
                Some(Arc::new(a))
            }
            Err(e) => {
                tracing::info!(reason = %e, "owner approvals: off");
                None
            }
        };
        env.approvals = approvals.clone();
        let env_http = env.http.clone();
        let jobs = Arc::new(
            JobStore::new(&cfg.jobs_path).with_node_file(Some(cfg.node_jobs_path.clone())),
        );
        let meshy = cfg.meshy.enabled.then(|| {
            // The key: a secret reference (`meshy.api_key` in secrets.toml) when the
            // secrets skill has one, else MESHY_API_KEY_FILE.
            let key: Arc<dyn lucina_meshy::key::KeySource> = match secret_store
                .as_ref()
                .and_then(|s| s.reference("meshy.api_key").map(|r| (s.clone(), r)))
            {
                Some((store, reference)) => {
                    Arc::new(lucina_meshy::key::SecretRefKey::new(store, reference))
                }
                None => Arc::new(lucina_meshy::key::FileKey::new(cfg.meshy.key_file.clone())),
            };
            Arc::new(lucina_meshy::MeshyService::new(
                cfg.meshy.clone(),
                key,
                jobs.clone(),
                timeline.clone(),
            ))
        });
        // The judge and summaries use the redacting model: nothing secret
        // reaches DeepSeek either.
        let delegator = delegation::build(
            &cfg,
            model.clone(),
            env_http.clone(),
            secret_store.clone(),
            timeline.clone(),
        );
        env.delegator = delegator.clone();
        let mut registry = Registry::builtin(&cfg.tools);
        if !cfg.au_enabled {
            registry = registry.without(lucina_tools::tools::au::NAMES);
        }
        if !cfg.callers_enabled {
            registry = registry.without(lucina_tools::tools::callers::NAMES);
        }
        if delegator.is_none() {
            registry = registry.without(lucina_tools::tools::delegate::DELEGATION_TOOLS);
        }
        if env.reach.is_none() {
            registry = registry.without(lucina_tools::tools::reach::REACH_TOOLS);
        }
        if env.drive.is_some() && cfg.tools.allows("drive") {
            registry
                .register(Arc::new(lucina_tools::tools::drive::DriveTool))
                .expect("drive tool registers");
        }
        // Lucina's own computers (LUCINA_COMPUTER_USE_ENABLED=true, crate lucina_computer_use).
        // The Jev engine there words its typing and answers with her text model.
        lucina_computer_use::jev_drive::set_text_model(model.clone());
        if std::env::var("LUCINA_COMPUTER_USE_ENABLED").as_deref() == Ok("true")
            && cfg.tools.allows("computer")
        {
            registry
                .register(Arc::new(lucina_tools::tools::computer::ComputerTool))
                .expect("computer tool registers");
        }
        // The ops record: owner channels and MCP turns alike (every agent reports).
        if env.ops.is_some() && cfg.tools.allows("ops") {
            registry
                .register(Arc::new(lucina_tools::tools::ops::OpsTool))
                .expect("ops tool registers");
        }
        // Root commands after the owner's approval of the exact commands (crate lucina_approvals).
        if approvals.is_some() && cfg.tools.allows("root_exec") {
            registry
                .register(Arc::new(lucina_tools::tools::root_exec::RootExecTool))
                .expect("root_exec tool registers");
        }
        let meshy_registry = meshy.as_ref().map(|svc| {
            let tool = Arc::new(lucina_tools::tools::meshy::MeshyTool::new(svc.clone()));
            if cfg.tools.allows("meshy") {
                registry
                    .register(tool.clone())
                    .expect("meshy tool registers");
            }
            let mut only = Registry::new();
            only.register(tool).expect("meshy tool registers");
            Arc::new(only)
        });
        // MCP turns: no owner-only tools and no delegation, ever.
        let mcp_registry = Arc::new(registry.without(lucina_tools::NO_MCP_TOOLS));
        let trusted_mcp_registry = Arc::new(registry.without(lucina_tools::OWNER_ONLY_TOOLS));
        // The owner's shared files: owner turns only, so added after the MCP registries.
        let share = crate::share::ShareRuntime::new(&cfg.share);
        for tool in crate::share::tools::all(share.memory.clone()) {
            registry.register(tool).expect("attachment tools register");
        }
        let registry = Arc::new(registry);
        let prompts = prompts::Prompts::load(&cfg);
        let deals = deals::engine(&cfg, grocery.clone());
        let geocoder = cfg
            .maps_geocode
            .clone()
            .map(|c| Arc::new(geocode::Geocoder::new(c)));
        let inner = Inner {
            geocoder,
            share,
            timeline,
            selftest_timeline,
            reminders,
            writer,
            jobs,
            meshy,
            meshy_registry,
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
            deals,
            tool_env: Arc::new(env),
            registry,
            mcp_registry,
            trusted_mcp_registry,
            secrets: secret_store,
            delegator,
            pending: Arc::new(PendingStore::file(
                cfg.data_dir.join("lucina-rs/pending-actions.json"),
                DEFAULT_TTL,
            )),
            recent: {
                let recent = RecentChat::new(Some(cfg.data_dir.join("lucina-rs/recent-chat.json")));
                if let Some(seed) = &cfg.recent_seed_path {
                    let n = recent.seed_from(seed, "secretary_");
                    if n > 0 {
                        tracing::info!(sessions = n, "recent turns seeded from the Node buffer");
                    }
                }
                Arc::new(recent)
            },
            secretary: bridges::Secretary {
                http: env_http.clone(),
                url: cfg.secretary_url.clone(),
                token_file: cfg.secretary_service_token_file.clone(),
            },
            bridge_state: bridges::BridgeState::default(),
            client_messages: Mutex::new(HashMap::new()),
            held_secrets: owner_secrets::HeldSecrets::default(),
            briefings: tokio::sync::Mutex::new(HashMap::new()),
            streams: AtomicUsize::new(0),
            started: Instant::now(),
            monitor: Arc::new(delivery_monitor::Monitor::new(
                cfg.delivery_alert_after,
                cfg.data_dir.join("lucina-rs").join("delivery-monitor.json"),
            )),
            supervisor: Arc::default(),
            ops,
            ingest,
            voice_jev: cfg.voice_jev.clone().map(|c| Arc::new(lucina_jev::Jev::new(c))),
            hospital_health: Arc::new(Mutex::new(bot_hospital::HospitalHealth {
                enabled: cfg.hospital.enabled && cfg.hospital.daily_audit_enabled,
                ..Default::default()
            })),
            scheduler_health: Arc::new(Mutex::new(lucina_store::scheduler::SchedulerHealth {
                enabled: cfg.scheduler && cfg.reminders_writer == WriterKind::Local,
                ..Default::default()
            })),
            telegram_health: Default::default(),
            telegram_recent: Arc::new(RecentChat::new(Some(cfg.telegram.recent_path.clone()))),
            discord_health: Arc::new(Mutex::new(lucina_discord::session::SessionHealth {
                state: "off".into(),
                ..Default::default()
            })),
            cfg,
        };
        if let Some(r) = &inner.tool_env.reach {
            r.set_sink(Arc::new(reach::ServerSink {
                timeline: inner.timeline.clone(),
                selftest_timeline: inner.selftest_timeline.clone(),
                recent: inner.recent.clone(),
                secretary: inner.secretary.clone(),
            }));
        }
        Ok(Self(Arc::new(inner)))
    }

    /// Replace the deals engine (tests: fixtures and a fixed clock). Only
    /// before the state is shared.
    pub fn with_deals(mut self, engine: Option<Arc<lucina_deals::DealsEngine>>) -> Self {
        Arc::get_mut(&mut self.0)
            .expect("with_deals before the state is shared")
            .deals = engine;
        self
    }

    /// `LUCINA_TELEGRAM_ENABLED=true` and a Lucina bot token.
    pub fn telegram_active(&self) -> bool {
        self.cfg.telegram.enabled && self.cfg.telegram_token.is_some()
    }

    /// `LUCINA_DISCORD_ENABLED=true` and a Lucina Discord bot token.
    pub fn discord_active(&self) -> bool {
        self.cfg.discord.enabled && self.cfg.discord_token.is_some()
    }

    /// The Discord message session (`LUCINA_DISCORD_ENABLED=true`); `None`
    /// when off. It closes the session and releases its lease when
    /// `shutdown` turns true: await the handle before exiting.
    pub fn spawn_discord(
        &self,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        if !self.cfg.discord.enabled {
            return None;
        }
        let Some(token) = self.cfg.discord_token.clone() else {
            tracing::warn!("LUCINA_DISCORD_ENABLED=true but no Lucina Discord token (DISCORD_BOT_TOKEN_LUCINA or LUCINA_DISCORD_TOKEN_FILE): Discord stays off");
            return None;
        };
        match discord::spawn(self, token, shutdown) {
            Ok(handle) => Some(handle),
            Err(e) => {
                tracing::error!(error = %e, "discord message session not started");
                None
            }
        }
    }

    /// The Telegram bot's poller (`LUCINA_TELEGRAM_ENABLED=true`); `None`
    /// when off. It releases its lease when `shutdown` turns true: await
    /// the handle before exiting.
    pub fn spawn_telegram(
        &self,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        if !self.cfg.telegram.enabled {
            return None;
        }
        let Some(token) = self.cfg.telegram_token.clone() else {
            tracing::warn!("LUCINA_TELEGRAM_ENABLED=true but no Lucina bot token (TELEGRAM_BOT_TOKEN_LUCINA or LUCINA_TELEGRAM_TOKEN_FILE): Telegram stays off");
            return None;
        };
        match telegram::spawn(self, lucina_telegram::BotApi::new(token), shutdown) {
            Ok(handle) => Some(handle),
            Err(e) => {
                tracing::error!(error = %e, "telegram poller not started");
                None
            }
        }
    }

    /// Background tasks: the grocery deal watch when `GROCERY_OFFER_WATCH_ENABLED=true`,
    /// the reminder scheduler only when `LUCINA_SCHEDULER=true`.
    pub fn spawn_background(&self) {
        // Notebooks: open the store and resume unfinished ingest, insights and podcasts.
        crate::notebooks::start(self);
        // A shopping drive that reaches the checkout asks the owner (decision + app notification).
        let st = self.clone();
        lucina_computer_use::jev_drive::set_handover_sink(Arc::new(move |h| {
            let st = st.clone();
            tokio::spawn(async move { crate::computer::hand_over(&st, h).await });
        }));
        // Learn every value in Lucina's Infisical folder, so the redactor
        // masks a secret even before a tool first uses it.
        if let Some(store) = self.secrets.clone() {
            if store.config().redaction.preload {
                tokio::spawn(async move {
                    match store.preload_redaction().await {
                        Ok(n) => tracing::info!(
                            secrets = n,
                            "redactor: Infisical folder preloaded (digests only)"
                        ),
                        Err(e) => tracing::warn!(error = %e, "redactor: Infisical preload failed"),
                    }
                });
            }
        }
        // Caller screening: tellows' Scorelist refresh and the rejected-calls digest.
        if let Some(h) = callers::spawn(self) {
            self.supervisor.watch("callers", h);
        }
        // The grocery deal watch has its own switch (GROCERY_OFFER_WATCH_ENABLED).
        if let Some(h) = deals::spawn_watch(self) {
            self.supervisor.watch("deal watch", h);
        }
        if let Some(h) = bot_hospital::spawn(
            self,
            Arc::new(bot_hospital::Hospital {
                cfg: self.cfg.hospital.clone(),
                http: reqwest::Client::new(),
                health: self.hospital_health.clone(),
            }),
        ) {
            self.supervisor.watch("bot hospital", h);
        }
        self.supervisor
            .watch("delivery monitor", delivery_monitor::spawn(self));
        if let Some(h) = ops::spawn(self) {
            self.supervisor.watch("ops record", h);
        }
        if let Some(h) = ops::spawn_ingest(self) {
            self.supervisor.watch("vault ingest", h);
        }
        if self.cfg.scheduler {
            if self.cfg.reminders_writer == WriterKind::Node {
                tracing::warn!("LUCINA_SCHEDULER=true ignored: reminders.json is owned by the Node Lucina (LUCINA_REMINDERS_WRITER=node)");
                return;
            }
            let timeline = lucina_store::scheduler::TimelineSink {
                timeline: self.timeline.clone(),
                owner_chat_id: self.cfg.owner_chat_id.clone().unwrap_or_default(),
                owner_peer_id: self.cfg.owner_peer_id.clone(),
            };
            let sink: Arc<dyn lucina_store::scheduler::ReminderSink> =
                if let Some(token) = &self.cfg.telegram_token {
                    Arc::new(reminder_delivery::TelegramReminderSink {
                        bot: lucina_telegram::BotApi::new(token.clone()),
                        runtime: tokio::runtime::Handle::current(),
                        recent: self.recent.clone(),
                        // With the bot here, deliveries join the chat's turns (Node's buffer).
                        telegram_recent: self
                            .telegram_active()
                            .then(|| (self.telegram_recent.clone(), self.telegram_health.clone())),
                        timeline,
                        secretary: self.secretary.clone(),
                        app_notify: self.cfg.app_notify,
                    })
                } else {
                    tracing::warn!(
                    "reminder scheduler has no Lucina Telegram token; using timeline-only delivery"
                );
                    Arc::new(timeline)
                };
            let handle = lucina_store::scheduler::spawn(
                self.reminders.clone(),
                sink,
                self.cfg.persona.clone(),
                self.cfg.scheduler_poll,
                self.cfg.scheduler_initial_delay,
                self.scheduler_health.clone(),
            );
            self.supervisor.watch("reminder scheduler", handle);
        }
    }
}

/// The grocery repository (`GROCERY_DATABASE_URL`), connected lazily; `None` without one.
pub fn connect_grocery(cfg: &Config) -> anyhow::Result<Option<GroceryRepo>> {
    let Some(url) = &cfg.grocery_database_url else {
        return Ok(None);
    };
    let catalog = Catalog::from_paths(Catalog::default_paths(
        cfg.shopping_catalog_path.clone(),
        &cfg.data_dir,
    ));
    Ok(Some(GroceryRepo::connect_lazy(url, catalog)?))
}

/// The full router.
pub fn app(state: AppState) -> axum::Router {
    api::router(state)
}
