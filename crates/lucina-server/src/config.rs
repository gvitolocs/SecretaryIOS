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

#[derive(Clone)]
pub struct Config {
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
    /// `LUCINA_NOTES`, `LUCINA_CALENDAR`, `LUCINA_AVATAR_OUTFIT`,
    /// `LUCINA_APP_NOTIFY` (all default false, as in Node).
    pub notes_enabled: bool,
    pub calendar_enabled: bool,
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
    pub data_dir: PathBuf,
    pub persona: String,
    pub host_label: String,
    // secretary api
    pub secretary_enabled: bool,
    pub token: Option<String>,
    pub default_session: String,
    pub grocery_user_id: String,
    pub briefing_cache: Duration,
    pub jobs_path: PathBuf,
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
    // owner
    pub owner_chat_id: Option<String>,
    pub owner_peer_id: String,
    // memory
    pub memory_mode: MemoryMode,
    pub honcho: HonchoConfig,
    // grocery
    pub grocery_database_url: Option<String>,
    pub shopping_catalog_path: Option<PathBuf>,
    // llm + planner
    pub deepseek: DeepSeekConfig,
    pub planner: PlannerMode,
    pub tools: ToolSelection,
    pub turn_deadline: Duration,
    pub oracle_monitor_enabled: bool,
    pub persona_path: Option<PathBuf>,
    pub recovery_companion_path: PathBuf,
    pub operator_brief_path: PathBuf,
    // tools
    pub default_location: Location,
    pub endpoints: Endpoints,
    pub nominatim_email: Option<String>,
    // device bridge
    pub device_retention_days: i64,
    pub device_command_ttl_ms: i64,
    pub device_reachable_minutes: i64,
    // channels (not implemented in this build; always off)
    pub telegram_requested: bool,
    pub discord_requested: bool,
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
            .field("outfit", &self.outfit_enabled)
            .field("jobs", &self.dsh.enabled)
            .finish_non_exhaustive()
    }
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
        let lucina_docs = PathBuf::from("/home/nez/Projects/Lucina/docs");
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
        Ok(Self {
            bind,
            binds,
            shadow: flag("LUCINA_SHADOW", false),
            intent,
            capability_hints: flag("LUCINA_CAPABILITY_HINTS_ENABLED", true),
            notes_enabled: flag("LUCINA_NOTES", false),
            calendar_enabled: flag("LUCINA_CALENDAR", false),
            outfit_enabled: flag("LUCINA_AVATAR_OUTFIT", false),
            app_notify: flag("LUCINA_APP_NOTIFY", false),
            dsh: crate::jobs::DshConfig::from_lookup(&get, jobs_enabled, &host_label),
            image_jobs: get("IMAGE_PROVIDER").as_deref().map(str::trim) == Some("comfyui")
                && get("COMFYUI_BASE_URL").is_some(),
            recent_seed_path: Some(path_or(
                "LUCINA_RECENT_SEED_PATH",
                "recent-telegram-chat.json",
            )),
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
            jobs_path: path_or("SECRETARY_JOBS_PATH", "secretary-jobs.json"),
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
            deepseek: DeepSeekConfig::from_lookup(&get),
            planner: match get("LUCINA_PLANNER").as_deref().map(str::trim) {
                Some("tools") => PlannerMode::Tools,
                _ => PlannerMode::Json,
            },
            tools: ToolSelection::parse(&get("LUCINA_TOOLS").unwrap_or_else(|| "all".into())),
            turn_deadline: Duration::from_millis(num("LUCINA_TURN_DEADLINE_MS", 90_000).max(1000)),
            oracle_monitor_enabled,
            persona_path: get("LUCINA_PERSONA_PATH").map(PathBuf::from),
            recovery_companion_path: get("LUCINA_RECOVERY_COMPANION_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| lucina_docs.join("lucina-recovery-companion.md")),
            operator_brief_path: get("LUCINA_OPERATOR_BRIEF_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| lucina_docs.join("lucina-operator-brief.md")),
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
            secretary_url: get("LUCINA_SECRETARY_SERVER_URL").filter(|s| !s.trim().is_empty()),
            secretary_service_token_file: get("LUCINA_SECRETARY_SERVICE_TOKEN_FILE")
                .map(PathBuf::from),
            data_dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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
        assert!(!format!("{c:?}").contains("\"t\""));
    }
}
