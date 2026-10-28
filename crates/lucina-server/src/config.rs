//! Configuration from the environment, using the Node Lucina's variable
//! names wherever a Node equivalent exists (port plan INVENTORY §7).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono_tz::Tz;
use lucina_llm::DeepSeekConfig;
use lucina_memory::{HonchoConfig, MemoryMode};
use lucina_tools::{Endpoints, Location, ToolSelection};

pub const DEFAULT_BIND: &str = "127.0.0.1:8790";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannerMode {
    /// Node's JSON-action planner, prompts verbatim (parity; default).
    Json,
    /// Native tool calling with the Rust tools (`LUCINA_PLANNER=tools`).
    Tools,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriterKind {
    /// Forward reminder writes to the Node Lucina (phase 1; default).
    Node,
    /// Write `reminders.json` directly (single writer only).
    Local,
}

/// `LUCINA_INSTAGRAM_URL`, `LUCINA_INSTAGRAM_GATEWAY_SECRET_FILE`, `LUCINA_PIXIV_URL`,
/// `LUCINA_PIXIV_GATEWAY_SECRET_FILE`, `LUCINA_PUBLIC_URL` (the owner-only plugin tools).
#[derive(Debug, Clone, Default)]
pub struct PluginsConfig {
    pub instagram_url: Option<String>,
    pub instagram_secret_file: Option<PathBuf>,
    pub pixiv_url: Option<String>,
    pub pixiv_secret_file: Option<PathBuf>,
    pub public_url: Option<String>,
}

#[derive(Clone)]
pub struct Config {
    pub share: crate::share::ShareConfig,
    pub bind: SocketAddr,
    /// Every address to listen on (`LUCINA_BIND`, comma-separated; the
    /// first is `bind`).
    pub binds: Vec<SocketAddr>,
    /// `LUCINA_SHADOW=true`: reported on `/health` (side-by-side with Node).
    pub shadow: bool,
    /// Tier-0 / planner settings (`LUCINA_FAST_CHAT_*`, `LUCINA_INTENT_*`).
    pub intent: lucina_intent::heuristics::Settings,
    /// `LUCINA_CAPABILITY_HINTS_ENABLED` (default true, as in Node).
    pub capability_hints: bool,
    /// `LUCINA_RICH_REPLIES` (default false): Lucina message format v1 replies
    /// and the conversational style rules (`crate::rich`).
    pub rich_replies: bool,
    /// `LUCINA_MCP_TRUSTED`: Lucina MCP turns (a caller holding an `mcp_…`
    /// token: ChatGPT, Grok, Cursor, Claude Code) act like the owner. Secrets
    /// and paid Meshy credits stay owner-only either way.
    pub mcp_trusted: bool,
    /// `LUCINA_RICH_COMPOSE_TIMEOUT_MS` (25000): one JSON-mode compose call.
    pub rich_compose_timeout: Duration,
    /// `LUCINA_MAPS_GEOCODE_URL` (+ `LUCINA_MAPS_UPSTREAM_KEY_FILE`): fill the
    /// coordinates of place blocks from the map server; unset = off.
    pub maps_geocode: Option<crate::geocode::GeocodeConfig>,
    /// `LUCINA_NOTES`, `LUCINA_CALENDAR`, `LUCINA_AVATAR_OUTFIT`,
    /// `LUCINA_APP_NOTIFY` (all default false, as in Node).
    pub notes_enabled: bool,
    pub calendar_enabled: bool,
    /// `LUCINA_TASKS`: the tasks bridge (Todoist via secretary-server).
    pub tasks_enabled: bool,
    pub outfit_enabled: bool,
    pub app_notify: bool,
    /// dsh-remote for app jobs (`DSH_REMOTE_*`; `LUCINA_JOBS=false` turns
    /// Harness jobs off, e.g. in shadow mode).
    pub dsh: crate::jobs::DshConfig,
    /// `IMAGE_PROVIDER=comfyui` with `COMFYUI_BASE_URL`: image jobs start
    /// (and fail here); otherwise Node's ComfyUI blocker text.
    pub image_jobs: bool,
    /// Node's `recent-telegram-chat.json`: secretary sessions seed the Rust
    /// recent-turn buffer (read only).
    pub recent_seed_path: Option<PathBuf>,
    /// The flags behind the "Runtime capabilities" self-description.
    pub caps: crate::capabilities::Capabilities,
    /// `LUCINA_OPERATOR_BRIEF_MAX_CHARS` (2800) and `LUCINA_POKOIN_HANDOFF_PATH`.
    pub operator_brief_max_chars: usize,
    pub pokoin_handoff_path: PathBuf,
    pub data_dir: PathBuf,
    pub persona: String,
    pub host_label: String,
    // secretary api
    pub secretary_enabled: bool,
    pub token: Option<String>,
    pub default_session: String,
    pub grocery_user_id: String,
    pub briefing_cache: Duration,
    /// This runtime's own jobs file (`SECRETARY_JOBS_PATH`, default
    /// `lucina-rs/secretary-jobs.json`): never Node's (verification P1).
    pub jobs_path: PathBuf,
    /// Node's jobs file, read-only (`LUCINA_NODE_JOBS_PATH`, default
    /// `secretary-jobs.json`).
    pub node_jobs_path: PathBuf,
    // timeline
    pub timeline_enabled: bool,
    pub timeline_path: PathBuf,
    pub timeline_max_bytes: u64,
    pub timeline_max_streams: usize,
    pub timeline_ping: Duration,
    // reminders
    pub reminders_enabled: bool,
    pub reminders_path: PathBuf,
    pub time_zone: Tz,
    pub reminder_default_hour: u32,
    pub reminder_default_minute: u32,
    pub reminder_max_stored: usize,
    pub reminders_writer: WriterKind,
    pub node_url: String,
    pub node_token: Option<String>,
    pub scheduler: bool,
    pub scheduler_poll: Duration,
    pub scheduler_initial_delay: Duration,
    pub telegram_token: Option<String>,
    // owner
    pub owner_chat_id: Option<String>,
    pub owner_peer_id: String,
    // memory
    pub memory_mode: MemoryMode,
    pub honcho: HonchoConfig,
    // grocery
    pub grocery_database_url: Option<String>,
    pub shopping_catalog_path: Option<PathBuf>,
    /// Grocery deals near home (Node's `config.grocery` deal fields).
    pub deals: lucina_deals::DealsConfig,
    /// The deal watch (`GROCERY_OFFER_WATCH_ENABLED`, `GROCERY_DEALS_*`).
    pub deal_watch: lucina_deals::WatchConfig,
    // llm + planner
    pub deepseek: DeepSeekConfig,
    pub planner: PlannerMode,
    pub tools: ToolSelection,
    pub turn_deadline: Duration,
    pub oracle_monitor_enabled: bool,
    pub persona_path: Option<PathBuf>,
    /// `LUCINA_RECOVERY_COMPANION_PATH`; unset: the copy built into the binary.
    pub recovery_companion_path: Option<PathBuf>,
    /// `LUCINA_OPERATOR_BRIEF_PATH`; unset: the copy built into the binary.
    pub operator_brief_path: Option<PathBuf>,
    // tools
    pub default_location: Location,
    pub endpoints: Endpoints,
    pub nominatim_email: Option<String>,
    // device bridge
    pub device_retention_days: i64,
    pub device_command_ttl_ms: i64,
    pub device_reachable_minutes: i64,
    // channels: Telegram (`LUCINA_TELEGRAM_ENABLED`, see `telegram`), Discord
    // (`LUCINA_DISCORD_ENABLED`, see `discord`)
    pub telegram_requested: bool,
    /// The Telegram bot (`crate::telegram`).
    pub telegram: crate::telegram::TelegramConfig,
    pub discord_requested: bool,
    /// The Discord message session (`crate::discord`).
    pub discord: crate::discord::DiscordConfig,
    /// `DISCORD_BOT_TOKEN_LUCINA`, else `LUCINA_DISCORD_TOKEN_FILE` (never logged).
    pub discord_token: Option<String>,
    // discord presence (watcher: `lucina presence-watch`)
    pub owner_discord_user_id: Option<String>,
    pub presence_dir: PathBuf,
    pub presence_context: bool,
    pub presence_max_age: Duration,
    pub presence_retention_days: i64,
    /// secretary-server base URL for the `avatar_outfit` tool (`LUCINA_SECRETARY_SERVER_URL`).
    pub secretary_url: Option<String>,
    /// Its service token file (`LUCINA_SECRETARY_SERVICE_TOKEN_FILE`, read per call).
    pub secretary_service_token_file: Option<PathBuf>,
    /// `secrets.toml` for the Infisical secrets skill (`LUCINA_SECRETS_CONFIG`;
    /// unset: no secrets, the redactor still runs).
    pub secrets_config: Option<PathBuf>,
    /// Meshy 3D (`LUCINA_MESHY`, `MESHY_*`, `LUCINA_MESHY_*`).
    pub meshy: lucina_meshy::MeshyConfig,
    /// Where the owner's Instagram and Pixiv plugin services are (all optional;
    /// defaults in `lucina_tools::tools::plugins::Plugins`).
    pub plugins: PluginsConfig,
    /// `LUCINA_DRIVE` (default false): Lucina's own Google Drive, read-only.
    pub drive_enabled: bool,
    /// `LUCINA_GOOGLE_CLIENT_FILE` (default: the Lucina Gmail OAuth client).
    pub google_client_file: PathBuf,
    /// `LUCINA_DRIVE_TOKEN_FILE` (default `~/.config/lucina-rs/google-drive-token.json`).
    pub drive_token_file: PathBuf,
    /// `LUCINA_DELEGATE` (default false): Lucina delegates to Claude, Grok
    /// and ChatGPT on her own judgement (crate `lucina-delegate`).
    pub delegate_enabled: bool,
    /// `LUCINA_DELEGATE_CONFIG`: the delegation policy (`delegate.toml`;
    /// unset or missing: the defaults).
    pub delegate_config: Option<PathBuf>,
    /// OTP requests in chat (`LUCINA_OTP_OWNER_TOKEN_FILE`, unset by default:
    /// "the OTP bridge is unavailable"), see [`crate::otp`].
    pub otp: crate::otp::OtpConfig,
    /// infisical-brokerd for "Add secret" (`LUCINA_BROKER_*`).
    pub broker: crate::owner_secrets::BrokerConfig,
    /// Bot Hospital daily Poko-news audit (`BOT_HOSPITAL_*`), see [`crate::bot_hospital`].
    pub hospital: crate::bot_hospital::HospitalConfig,
    /// `LUCINA_DELIVERY_ALERT_AFTER_MIN` (default 5): a delivery path down this long alerts the owner.
    pub delivery_alert_after: Duration,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind", &self.bind)
            .field("data_dir", &self.data_dir)
            .field("secretary_enabled", &self.secretary_enabled)
            .field("token", &self.token.as_ref().map(|_| "<set>"))
            .field("planner", &self.planner)
            .field("memory_mode", &self.memory_mode)
            .field("reminders_writer", &self.reminders_writer)
            .field("scheduler", &self.scheduler)
            .field("binds", &self.binds)
            .field("shadow", &self.shadow)
            .field("fast_chat", &self.intent.fast_chat)
            .field("notes", &self.notes_enabled)
            .field("calendar", &self.calendar_enabled)
            .field("tasks", &self.tasks_enabled)
            .field("outfit", &self.outfit_enabled)
            .field("jobs", &self.dsh.enabled)
            .field("secrets_config", &self.secrets_config)
            .field("meshy", &self.meshy.enabled)
            .field("delegate", &self.delegate_enabled)
            .field("deals", &self.deals.has_home())
            .field("deal_watch", &self.deal_watch.enabled)
            .field("otp_owner_token_file", &self.otp.owner_token_file)
            .field("drive", &self.drive_enabled)
            .field("mcp_trusted", &self.mcp_trusted)
            .field("telegram", &self.telegram.enabled)
            .field("discord", &self.discord.enabled)
            .field("maps_geocode", &self.maps_geocode.is_some())
            .finish_non_exhaustive()
    }
}

/// This runtime's jobs file. Node's file is never shared (verification P1:
/// a shared file gave false `interrupted` and lost job updates), so a
/// `SECRETARY_JOBS_PATH` naming Node's file is replaced by the default.
pub fn own_jobs_path(configured: PathBuf, node: &Path, data_dir: &Path) -> PathBuf {
    let same = configured == node
        || matches!(
            (configured.canonicalize(), node.canonicalize()),
            (Ok(a), Ok(b)) if a == b
        );
    if !same {
        return configured;
    }
    let own = data_dir.join("lucina-rs").join("secretary-jobs.json");
    tracing::warn!(
        configured = %configured.display(),
        using = %own.display(),
        "SECRETARY_JOBS_PATH names Node's jobs file; lucina-rs keeps its own"
    );
    own
}

fn read_secret_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path.trim())
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn first_csv(v: &str) -> Option<String> {
    v.split(',')
        .map(str::trim)
        .find(|s| !s.is_empty() && !s.starts_with('-'))
        .map(str::to_owned)
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
    }

    /// Everything relative to `data_dir`, as if no variable was set.
    pub fn for_data_dir(data_dir: &Path) -> Self {
        let dir = data_dir.to_string_lossy().into_owned();
        Self::from_lookup(move |k| (k == "LUCINA_DATA_DIR").then(|| dir.clone()))
            .expect("defaults are valid")
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let flag = |k: &str, default: bool| {
            get(k)
                .map(|v| v.trim().eq_ignore_ascii_case("true"))
                .unwrap_or(default)
        };
        let num = |k: &str, default: u64| {
            get(k)
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(default)
        };
        let data_dir = PathBuf::from(get("LUCINA_DATA_DIR").unwrap_or_else(|| "data".into()));
        let path_or = |k: &str, file: &str| {
            get(k)
                .map(PathBuf::from)
                .unwrap_or_else(|| data_dir.join(file))
        };
        let node_jobs_path = path_or("LUCINA_NODE_JOBS_PATH", "secretary-jobs.json");
        let jobs_path = own_jobs_path(
            path_or("SECRETARY_JOBS_PATH", "lucina-rs/secretary-jobs.json"),
            &node_jobs_path,
            &data_dir,
        );
        let owner_peer_id = get("LUCINA_OWNER_USER_PEER_ID")
            .or_else(|| get("LUCINA_GMAIL_LEARN_USER_PEER_ID"))
            .unwrap_or_else(|| "giuseppe".into());
        let tz_name = get("LUCINA_REMINDER_TIMEZONE")
            .or_else(|| get("LUCINA_TIMEZONE"))
            .unwrap_or_else(|| "Europe/Rome".into());
        let time_zone: Tz = tz_name
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("unknown time zone {tz_name}"))?;
        let token = get("SECRETARY_API_TOKEN_FILE")
            .and_then(|p| read_secret_file(&p))
            .or_else(|| get("SECRETARY_API_TOKEN"));
        let memory_mode = match get("MEMORY_PROVIDER").as_deref().map(str::trim) {
            Some(p) if p != "honcho" => MemoryMode::Off,
            _ => get("LUCINA_MEMORY_MODE")
                .and_then(|m| MemoryMode::parse(&m))
                .unwrap_or(MemoryMode::ReadOnly),
        };
        let mut default_location = Location::default();
        if let Some(n) = get("LUCINA_DEFAULT_LOCATION_NAME") {
            default_location.name = n;
        }
        if let Some(v) = get("LUCINA_DEFAULT_LOCATION_LAT").and_then(|v| v.parse().ok()) {
            default_location.latitude = v;
        }
        if let Some(v) = get("LUCINA_DEFAULT_LOCATION_LON").and_then(|v| v.parse().ok()) {
            default_location.longitude = v;
        }
        let mut endpoints = Endpoints::default();
        if let Some(v) = get("LUCINA_OPEN_METEO_URL") {
            endpoints.open_meteo_forecast = v;
        }
        if let Some(v) = get("LUCINA_OPEN_METEO_GEOCODING_URL") {
            endpoints.open_meteo_geocoding = v;
        }
        if let Some(v) = get("LUCINA_NOMINATIM_URL") {
            endpoints.nominatim = v;
        }
        let binds: Vec<SocketAddr> = get("LUCINA_BIND")
            .unwrap_or_else(|| DEFAULT_BIND.into())
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse())
            .collect::<Result<_, _>>()?;
        let bind = *binds
            .first()
            .ok_or_else(|| anyhow::anyhow!("LUCINA_BIND is empty"))?;
        let host_label = get("LUCINA_RUNTIME_HOST")
            .or_else(|| get("LUCINA_HOST_LABEL"))
            .map(|s| s.trim().to_lowercase())
            .unwrap_or_else(|| "nezopt".into());
        let oracle_monitor_enabled = flag("LUCINA_ORACLE_MONITOR_ENABLED", false);
        let intent = lucina_intent::heuristics::Settings {
            fast_chat: flag("LUCINA_FAST_CHAT_ENABLED", false),
            fast_chat_max_chars: num("LUCINA_FAST_CHAT_MAX_CHARS", 320) as usize,
            aggressive_tool_routing: flag("LUCINA_AGGRESSIVE_TOOL_ROUTING", true),
            confidence_min: get("LUCINA_INTENT_CONFIDENCE_MIN")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0.72),
            oracle_monitor: oracle_monitor_enabled,
            intent_router: flag("LUCINA_INTENT_ROUTER_ENABLED", true),
        };
        let jobs_enabled = flag("LUCINA_JOBS", true);
        let secretary_url = get("LUCINA_SECRETARY_SERVER_URL").filter(|s| !s.trim().is_empty());
        let secretary_service_token_file =
            get("LUCINA_SECRETARY_SERVICE_TOKEN_FILE").map(PathBuf::from);
        let meshy = lucina_meshy::MeshyConfig::from_lookup(
            &get,
            &data_dir,
            time_zone,
            secretary_url.clone(),
            secretary_service_token_file.clone(),
        );
        let hospital = crate::bot_hospital::HospitalConfig::from_lookup(&get, &data_dir);
        Ok(Self {
            share: crate::share::ShareConfig::from_lookup(&get),
            bind,
            binds,
            shadow: flag("LUCINA_SHADOW", false),
            intent,
            capability_hints: flag("LUCINA_CAPABILITY_HINTS_ENABLED", true),
            rich_replies: flag("LUCINA_RICH_REPLIES", false),
            mcp_trusted: flag("LUCINA_MCP_TRUSTED", false),
            rich_compose_timeout: Duration::from_millis(
                num("LUCINA_RICH_COMPOSE_TIMEOUT_MS", 25_000).max(1000),
            ),
            maps_geocode: crate::geocode::GeocodeConfig::from_lookup(&get),
            notes_enabled: flag("LUCINA_NOTES", false),
            calendar_enabled: flag("LUCINA_CALENDAR", false),
            tasks_enabled: flag("LUCINA_TASKS", false),
            outfit_enabled: flag("LUCINA_AVATAR_OUTFIT", false),
            app_notify: flag("LUCINA_APP_NOTIFY", false),
            dsh: crate::jobs::DshConfig::from_lookup(&get, jobs_enabled, &host_label),
            image_jobs: get("IMAGE_PROVIDER").as_deref().map(str::trim) == Some("comfyui")
                && get("COMFYUI_BASE_URL").is_some(),
            recent_seed_path: Some(path_or(
                "LUCINA_RECENT_SEED_PATH",
                "recent-telegram-chat.json",
            )),
            caps: crate::capabilities::Capabilities::from_lookup(&get),
            operator_brief_max_chars: num("LUCINA_OPERATOR_BRIEF_MAX_CHARS", 2800) as usize,
            pokoin_handoff_path: path_or("LUCINA_POKOIN_HANDOFF_PATH", "pokoin-handoff.md"),
            persona: get("LUCINA_PERSONA_NAME")
                .or_else(|| get("TELEGRAM_PERSONA"))
                .unwrap_or_else(|| "Lucina".into()),
            host_label,
            secretary_enabled: get("SECRETARY_API_ENABLED").as_deref() == Some("true"),
            token,
            default_session: get("SECRETARY_SESSION_ID").unwrap_or_else(|| "secretary_app".into()),
            grocery_user_id: get("SECRETARY_GROCERY_USER_ID")
                .unwrap_or_else(|| owner_peer_id.clone()),
            briefing_cache: Duration::from_millis(num("SECRETARY_BRIEFING_CACHE_MS", 900_000)),
            jobs_path,
            node_jobs_path,
            timeline_enabled: get("LUCINA_TIMELINE_ENABLED").as_deref() != Some("false"),
            timeline_path: path_or("LUCINA_TIMELINE_PATH", "timeline.jsonl"),
            timeline_max_bytes: num("LUCINA_TIMELINE_MAX_BYTES", 50 * 1024 * 1024),
            timeline_max_streams: num("LUCINA_TIMELINE_MAX_STREAMS", 8) as usize,
            timeline_ping: Duration::from_millis(num("LUCINA_TIMELINE_PING_MS", 20_000).max(10)),
            reminders_enabled: get("LUCINA_REMINDERS_ENABLED")
                .map(|v| v == "true")
                .unwrap_or(true),
            reminders_path: path_or("LUCINA_REMINDERS_STORE_PATH", "reminders.json"),
            time_zone,
            reminder_default_hour: num("LUCINA_REMINDER_DEFAULT_HOUR", 9) as u32,
            reminder_default_minute: num("LUCINA_REMINDER_DEFAULT_MINUTE", 0) as u32,
            reminder_max_stored: num("LUCINA_REMINDER_MAX_STORED", 500) as usize,
            reminders_writer: match get("LUCINA_REMINDERS_WRITER").as_deref().map(str::trim) {
                Some("local") => WriterKind::Local,
                _ => WriterKind::Node,
            },
            node_url: get("LUCINA_NODE_URL").unwrap_or_else(|| "http://127.0.0.1:8788".into()),
            node_token: get("LUCINA_NODE_TOKEN_FILE").and_then(|p| read_secret_file(&p)),
            scheduler: flag("LUCINA_SCHEDULER", false),
            scheduler_poll: Duration::from_millis(num("LUCINA_REMINDER_POLL_INTERVAL_MS", 60_000)),
            scheduler_initial_delay: Duration::from_millis(num(
                "LUCINA_REMINDER_INITIAL_DELAY_MS",
                15_000,
            )),
            telegram_token: get("TELEGRAM_BOT_TOKEN_LUCINA")
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    let path = get("LUCINA_TELEGRAM_TOKEN_FILE").or_else(|| {
                        get("HOME").map(|h| {
                            format!("{h}/.config/lucina-rs/telegram-bot-token-lucina.token")
                        })
                    })?;
                    read_secret_file(&path)
                }),
            owner_chat_id: get("LUCINA_OPERATOR_CHAT_ID")
                .or_else(|| get("LUCINA_GMAIL_LEARN_CHAT_ID"))
                .or_else(|| get("TELEGRAM_ALLOWED_CHAT_IDS").and_then(|v| first_csv(&v))),
            owner_peer_id,
            memory_mode,
            honcho: HonchoConfig::from_lookup(&get),
            grocery_database_url: get("GROCERY_DATABASE_URL_FILE")
                .and_then(|p| read_secret_file(&p))
                .or_else(|| get("GROCERY_DATABASE_URL")),
            shopping_catalog_path: get("LUCINA_SHOPPING_CATALOG_PATH").map(PathBuf::from),
            deals: lucina_deals::DealsConfig::from_lookup(&get, &data_dir),
            deal_watch: lucina_deals::WatchConfig::from_lookup(&get, &data_dir),
            deepseek: DeepSeekConfig::from_lookup(&get),
            planner: match get("LUCINA_PLANNER").as_deref().map(str::trim) {
                Some("tools") => PlannerMode::Tools,
                _ => PlannerMode::Json,
            },
            tools: ToolSelection::parse(&get("LUCINA_TOOLS").unwrap_or_else(|| "all".into())),
            turn_deadline: Duration::from_millis(num("LUCINA_TURN_DEADLINE_MS", 90_000).max(1000)),
            oracle_monitor_enabled,
            persona_path: get("LUCINA_PERSONA_PATH").map(PathBuf::from),
            recovery_companion_path: get("LUCINA_RECOVERY_COMPANION_PATH").map(PathBuf::from),
            operator_brief_path: get("LUCINA_OPERATOR_BRIEF_PATH").map(PathBuf::from),
            default_location,
            endpoints,
            nominatim_email: get("LUCINA_NOMINATIM_EMAIL"),
            device_retention_days: num("LUCINA_DEVICE_CONTEXT_DAYS", 90) as i64,
            device_command_ttl_ms: num("LUCINA_DEVICE_COMMAND_TTL_MS", 600_000) as i64,
            device_reachable_minutes: num("LUCINA_DEVICE_REACHABLE_MINUTES", 360) as i64,
            telegram_requested: flag("LUCINA_TELEGRAM_ENABLED", false),
            discord_requested: flag("LUCINA_DISCORD_ENABLED", false),
            owner_discord_user_id: get("LUCINA_OWNER_DISCORD_USER_ID")
                .or_else(|| get("LUCINA_OWNER_DISCORD_ID"))
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())),
            presence_dir: path_or("LUCINA_PRESENCE_DIR", "presence"),
            presence_context: flag("LUCINA_PRESENCE_CONTEXT", true),
            presence_max_age: Duration::from_millis(
                num("LUCINA_PRESENCE_MAX_AGE_MS", 600_000).max(60_000),
            ),
            presence_retention_days: num("LUCINA_PRESENCE_RETENTION_DAYS", 90).clamp(1, 3650)
                as i64,
            secretary_url,
            secretary_service_token_file,
            secrets_config: get("LUCINA_SECRETS_CONFIG").map(PathBuf::from),
            meshy,
            delegate_enabled: flag("LUCINA_DELEGATE", false),
            delegate_config: get("LUCINA_DELEGATE_CONFIG").map(PathBuf::from),
            otp: crate::otp::OtpConfig::from_lookup(&get),
            broker: crate::owner_secrets::BrokerConfig::from_lookup(&get),
            hospital,
            delivery_alert_after: Duration::from_secs(
                num("LUCINA_DELIVERY_ALERT_AFTER_MIN", 5).max(1) * 60,
            ),
            plugins: PluginsConfig {
                instagram_url: get("LUCINA_INSTAGRAM_URL"),
                instagram_secret_file: get("LUCINA_INSTAGRAM_GATEWAY_SECRET_FILE").map(PathBuf::from),
                pixiv_url: get("LUCINA_PIXIV_URL"),
                pixiv_secret_file: get("LUCINA_PIXIV_GATEWAY_SECRET_FILE").map(PathBuf::from),
                public_url: get("LUCINA_PUBLIC_URL"),
            },
            drive_enabled: flag("LUCINA_DRIVE", false),
            google_client_file: PathBuf::from(
                get("LUCINA_GOOGLE_CLIENT_FILE")
                    .unwrap_or_else(|| lucina_tools::google::DEFAULT_CLIENT_FILE.into()),
            ),
            drive_token_file: get("LUCINA_DRIVE_TOKEN_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    lucina_tools::google::config_file(
                        &get("HOME").unwrap_or_else(|| "/home/nez".into()),
                        "google-drive-token.json",
                    )
                }),
            telegram: crate::telegram::TelegramConfig::from_lookup(&get, &data_dir),
            discord: crate::discord::DiscordConfig::from_lookup(&get, &data_dir),
            discord_token: crate::discord::token_from_lookup(&get),
            data_dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// P1: the live cutover.env set SECRETARY_JOBS_PATH to Node's file.
    #[test]
    fn jobs_file_is_never_nodes() {
        let cfg = Config::from_lookup(|k| match k {
            "LUCINA_DATA_DIR" => Some("/srv/lucina/data".into()),
            "SECRETARY_JOBS_PATH" => Some("/srv/lucina/data/secretary-jobs.json".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(
            cfg.jobs_path,
            PathBuf::from("/srv/lucina/data/lucina-rs/secretary-jobs.json")
        );
        assert_eq!(
            cfg.node_jobs_path,
            PathBuf::from("/srv/lucina/data/secretary-jobs.json")
        );
        let d = Config::for_data_dir(Path::new("/d"));
        assert_eq!(
            d.jobs_path,
            PathBuf::from("/d/lucina-rs/secretary-jobs.json")
        );
        let own = Config::from_lookup(|k| match k {
            "LUCINA_DATA_DIR" => Some("/d".into()),
            "SECRETARY_JOBS_PATH" => Some("/elsewhere/jobs.json".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(own.jobs_path, PathBuf::from("/elsewhere/jobs.json"));
    }

    #[test]
    fn telegram_token_precedence_and_redacted_debug() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "  test-file-secret\n").unwrap();
        let cfg = Config::from_lookup(|k| match k {
            "LUCINA_TELEGRAM_TOKEN_FILE" => Some(path.to_string_lossy().into_owned()),
            _ => None,
        })
        .unwrap();
        assert_eq!(cfg.telegram_token.as_deref(), Some("test-file-secret"));
        assert!(!format!("{cfg:?}").contains("test-file-secret"));
        let cfg = Config::from_lookup(|k| match k {
            "LUCINA_TELEGRAM_TOKEN_FILE" => Some(path.to_string_lossy().into_owned()),
            "TELEGRAM_BOT_TOKEN_LUCINA" => Some(" test-env-secret ".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(cfg.telegram_token.as_deref(), Some("test-env-secret"));
        assert!(!format!("{cfg:?}").contains("test-env-secret"));
        assert_eq!(cfg.scheduler_initial_delay, Duration::from_secs(15));
    }

    #[test]
    fn defaults_and_node_names() {
        let env: HashMap<&str, &str> = [
            ("LUCINA_DATA_DIR", "/tmp/x"),
            ("TELEGRAM_ALLOWED_CHAT_IDS", " 42, 7"),
            ("SECRETARY_API_ENABLED", "true"),
            ("SECRETARY_API_TOKEN", "t"),
            ("HONCHO_WORKSPACE_ID", "hermes-peer1"),
        ]
        .into_iter()
        .collect();
        let c = Config::from_lookup(|k| env.get(k).map(|v| v.to_string())).unwrap();
        assert_eq!(c.bind.to_string(), "127.0.0.1:8790");
        assert_eq!(c.owner_chat_id.as_deref(), Some("42"));
        assert_eq!(c.timeline_path, PathBuf::from("/tmp/x/timeline.jsonl"));
        assert_eq!(c.planner, PlannerMode::Json);
        assert_eq!(c.reminders_writer, WriterKind::Node);
        assert_eq!(c.memory_mode, MemoryMode::ReadOnly);
        assert!(!c.scheduler);
        assert_eq!(c.default_session, "secretary_app");
        assert_eq!(c.time_zone.name(), "Europe/Rome");
        assert_eq!(c.presence_dir, PathBuf::from("/tmp/x/presence"));
        assert!(c.presence_context);
        assert_eq!(c.presence_retention_days, 90);
        assert_eq!(c.owner_discord_user_id, None);
        assert!(!c.delegate_enabled, "delegation is opt-in per host");
        assert!(!format!("{c:?}").contains("\"t\""));
    }
}
