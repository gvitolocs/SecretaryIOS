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
pub mod bridges;
pub mod capabilities;
pub mod chat;
pub mod config;
pub mod context;
pub mod delegation;
pub mod fake;
pub mod grocery_turn;
pub mod jobs;
pub mod parity;
pub mod planner;
pub mod prompts;
pub mod rich;
pub mod secrets;

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
    pub jobs: Arc<JobStore>,
    pub device: DeviceStore,
    pub memory: Arc<Memory>,
    pub model: Arc<dyn ChatModel>,
    pub usage: Option<Arc<UsageTotals>>,
    pub deepseek_has_key: KeyProbe,
    pub grocery: Option<Arc<GroceryRepo>>,
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
                Some(Arc::new(
                    GroceryRepo::connect_lazy(url, catalog)?.with_namer(namer),
                ))
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
        env.presence_dir = cfg.presence_dir.clone();
        env.presence_max_age = cfg.presence_max_age;
        env.secretary_url = cfg.secretary_url.clone();
        env.secretary_service_token_file = cfg.secretary_service_token_file.clone();
        env.secrets = secret_store.clone();
        // Lucina's own Google Drive: only with a grant (`lucina google-auth drive`).
        if cfg.drive_enabled {
            match lucina_tools::google::Drive::open(
                env.http.clone(),
                lucina_tools::google::GoogleEndpoints::default(),
                &cfg.google_client_file,
                &cfg.drive_token_file,
            ) {
                Ok(d) => {
                    tracing::info!(account = d.account(), "Google Drive connected (read-only)");
                    env.drive = Some(Arc::new(d));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "LUCINA_DRIVE is on but Drive is not usable: tool skipped")
                }
            }
        }
        let env_http = env.http.clone();
        let jobs = Arc::new(JobStore::new(&cfg.jobs_path));
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
        if delegator.is_none() {
            registry = registry.without(lucina_tools::tools::delegate::DELEGATION_TOOLS);
        }
        if env.drive.is_some() && cfg.tools.allows("drive") {
            registry
                .register(Arc::new(lucina_tools::tools::drive::DriveTool))
                .expect("drive tool registers");
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
        let registry = Arc::new(registry);
        // MCP turns: no owner-only tools and no delegation, ever.
        let mcp_registry = Arc::new(registry.without(lucina_tools::NO_MCP_TOOLS));
        let trusted_mcp_registry = Arc::new(registry.without(lucina_tools::OWNER_ONLY_TOOLS));
        let prompts = prompts::Prompts::load(&cfg);
        let inner = Inner {
            timeline,
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
            briefings: tokio::sync::Mutex::new(HashMap::new()),
            streams: AtomicUsize::new(0),
            started: Instant::now(),
            cfg,
        };
        Ok(Self(Arc::new(inner)))
    }

    /// Background tasks: the reminder scheduler only when `LUCINA_SCHEDULER=true`.
    pub fn spawn_background(&self) {
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
