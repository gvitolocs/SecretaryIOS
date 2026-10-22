//! Configuration: a TOML file for behaviour (weights, thresholds, budget,
//! sources, health targets) and the environment for secrets and URLs.
//!
//! - `LUCINA_PROACTIVE_CONFIG`: the TOML file (default
//!   `~/.config/lucina-proactive/config.toml`; missing = all defaults).
//! - `LUCINA_PROACTIVE_DRY_RUN`: `true` (the default when unset) records
//!   what would be sent and sends nothing.
//! - Secrets and URLs keep the Node Lucina's names (read from the process
//!   environment, then from `paths.env_files`, whitelisted, see
//!   [`crate::envfile`]): `LUCINA_PROACTIVE_DATABASE_URL` or
//!   `GROCERY_DATABASE_URL`, `LUCINA_SECRETARY_SERVER_URL`,
//!   `LUCINA_SECRETARY_SERVICE_TOKEN_FILE`, `TELEGRAM_BOT_TOKEN_LUCINA`,
//!   `LUCINA_OPERATOR_CHAT_ID` (else `LUCINA_GMAIL_LEARN_CHAT_ID`, else the
//!   first of `TELEGRAM_ALLOWED_CHAT_IDS`), `DEEPSEEK_API_KEY` /
//!   `DEEPSEEK_KEY_FILE`, `HONCHO_*`. Values are never logged.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono_tz::Tz;
use lucina_llm::DeepSeekConfig;
use lucina_memory::HonchoConfig;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The owner's zone: the day of the budget, the Pulse time, quiet-hour
    /// fallback. The server's notification settings win for quiet hours.
    pub time_zone: String,
    /// Overridden by `LUCINA_PROACTIVE_DRY_RUN`.
    pub dry_run: bool,
    /// How often the loop wakes up; each source has its own interval.
    pub tick_seconds: u64,
    /// Fallback language (`it`, `en`, `da`) when the timeline says nothing.
    pub language: String,
    pub paths: Paths,
    pub weights: Weights,
    pub thresholds: Thresholds,
    pub budget: Budget,
    pub quiet: QuietFallback,
    pub pulse: PulseCfg,
    pub sources: SourcesCfg,
    pub health: HealthCfg,
    pub channels: ChannelsCfg,
    pub compose: ComposeCfg,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            time_zone: "Europe/Copenhagen".into(),
            dry_run: true,
            tick_seconds: 60,
            language: "it".into(),
            paths: Paths::default(),
            weights: Weights::default(),
            thresholds: Thresholds::default(),
            budget: Budget::default(),
            quiet: QuietFallback::default(),
            pulse: PulseCfg::default(),
            sources: SourcesCfg::default(),
            health: HealthCfg::default(),
            channels: ChannelsCfg::default(),
            compose: ComposeCfg::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Paths {
    /// The Node Lucina's data dir on the host (timeline, reminders,
    /// presence, device context), read only.
    pub node_data_dir: PathBuf,
    /// Overrides `LUCINA_SECRETARY_SERVICE_TOKEN_FILE` (Lucina's env holds
    /// the container path).
    pub service_token_file: Option<PathBuf>,
    /// Overrides `DEEPSEEK_KEY_FILE`.
    pub deepseek_key_file: Option<PathBuf>,
    /// Env files read for the whitelisted names (Lucina's own env files).
    pub env_files: Vec<PathBuf>,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            node_data_dir: PathBuf::from("/home/nez/Projects/Lucina/data"),
            service_token_file: None,
            deepseek_key_file: None,
            env_files: Vec::new(),
        }
    }
}

/// `score = Σ weight × input` (see [`crate::score`]).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Weights {
    pub urgency: f32,
    pub relevance: f32,
    pub confidence: f32,
    pub actionability: f32,
}

impl Default for Weights {
    fn default() -> Self {
        Self {
            urgency: 0.35,
            relevance: 0.25,
            confidence: 0.20,
            actionability: 0.20,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Thresholds {
    /// At or above: send now (if the attention gate agrees).
    pub send: f32,
    /// At or above (and below `send`): the Daily Pulse.
    pub digest: f32,
    /// Below this confidence nothing is said, whatever the score.
    pub min_confidence: f32,
    /// At or above: `priority: high`.
    pub high_priority: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            send: 0.70,
            digest: 0.45,
            min_confidence: 0.60,
            high_priority: 0.85,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Budget {
    /// Unsolicited messages per local day (the Pulse is not counted).
    pub max_per_day: u32,
    /// Minutes between two messages of the same kind (missing: 120).
    pub cooldown_minutes: BTreeMap<String, u32>,
    /// "Not now": that kind waits this long.
    pub not_now_hours: u32,
    /// "Stop this kind": muted this long.
    pub stop_days: u32,
    /// Hold (do not interrupt) while the owner plays a game on Discord.
    pub hold_while_gaming: bool,
    /// Hold while Discord says Do Not Disturb.
    pub hold_on_discord_dnd: bool,
}

impl Default for Budget {
    fn default() -> Self {
        let cooldown_minutes = [
            ("meeting_prep", 45),
            ("reminder_overdue", 60),
            ("due_no_plan", 240),
            ("note_for_event", 240),
            ("thread_resolved", 180),
            ("thread_open", 360),
            ("service_down", 30),
            ("service_recovered", 0),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        Self {
            max_per_day: 3,
            cooldown_minutes,
            not_now_hours: 6,
            stop_days: 30,
            hold_while_gaming: true,
            hold_on_discord_dnd: true,
        }
    }
}

impl Budget {
    pub fn cooldown(&self, kind: &str) -> chrono::Duration {
        chrono::Duration::minutes(i64::from(
            self.cooldown_minutes.get(kind).copied().unwrap_or(120),
        ))
    }
}

/// Quiet hours when the server's settings cannot be read (conservative:
/// on, 22:30–07:30).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuietFallback {
    pub enabled: bool,
    pub start: String,
    pub end: String,
}

impl Default for QuietFallback {
    fn default() -> Self {
        Self {
            enabled: true,
            start: "22:30".into(),
            end: "07:30".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PulseCfg {
    pub enabled: bool,
    /// Local `HH:MM`.
    pub time: String,
    /// No Pulse after this local time (held by quiet hours or presence).
    pub deadline: String,
    pub max_important: usize,
    pub max_completed: usize,
    pub max_watch: usize,
    /// Pending reminders due within this many days are "keep an eye on".
    pub watch_reminder_days: i64,
}

impl Default for PulseCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            time: "08:00".into(),
            deadline: "11:00".into(),
            max_important: 3,
            max_completed: 3,
            max_watch: 3,
            watch_reminder_days: 3,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SourceCfg {
    pub enabled: bool,
    pub interval_secs: u64,
    /// Up to this much is added to each interval (so sources do not fire together).
    pub jitter_secs: u64,
}

impl SourceCfg {
    fn every(secs: u64) -> Self {
        Self {
            enabled: true,
            interval_secs: secs,
            jitter_secs: secs / 4,
        }
    }
}

impl Default for SourceCfg {
    fn default() -> Self {
        Self::every(300)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SourcesCfg {
    pub calendar: SourceCfg,
    pub reminders: SourceCfg,
    pub notes: SourceCfg,
    pub timeline: SourceCfg,
    /// Meeting prep: events starting between these many minutes from now.
    pub prep_min_lead_minutes: i64,
    pub prep_max_lead_minutes: i64,
    /// A pending reminder this late is "overdue" (Node did not deliver it).
    pub overdue_grace_minutes: i64,
    /// Deadlines within this many days with no plan.
    pub due_no_plan_days: i64,
    /// Open threads: owner messages from the last N days...
    pub thread_lookback_days: i64,
    /// ...that went quiet for at least this long.
    pub thread_quiet_hours: i64,
    /// DeepSeek "resolved / still open" judgements per tick and per day.
    pub judge_max_per_tick: usize,
    pub judge_max_per_day: usize,
    pub judge_recheck_hours: i64,
}

impl Default for SourcesCfg {
    fn default() -> Self {
        Self {
            calendar: SourceCfg::every(180),
            reminders: SourceCfg::every(300),
            notes: SourceCfg::every(600),
            timeline: SourceCfg::every(900),
            prep_min_lead_minutes: 20,
            prep_max_lead_minutes: 70,
            overdue_grace_minutes: 15,
            due_no_plan_days: 3,
            thread_lookback_days: 3,
            thread_quiet_hours: 12,
            judge_max_per_tick: 3,
            judge_max_per_day: 12,
            judge_recheck_hours: 12,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HealthCfg {
    pub enabled: bool,
    pub interval_secs: u64,
    pub jitter_secs: u64,
    pub timeout_secs: u64,
    /// Down after this many failed checks in a row...
    pub fail_threshold: u32,
    /// ...spanning at least this many minutes (never on a single blip).
    pub min_down_minutes: i64,
    /// Back up after this many good checks in a row.
    pub recover_threshold: u32,
    /// Slower than this counts as a failed check.
    pub slow_ms: u64,
    pub targets: Vec<HealthTarget>,
}

impl Default for HealthCfg {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_secs: 120,
            jitter_secs: 20,
            timeout_secs: 10,
            fail_threshold: 3,
            min_down_minutes: 5,
            recover_threshold: 2,
            slow_ms: 8000,
            targets: vec![
                HealthTarget {
                    name: "Secretary".into(),
                    url: Some("https://getsecretary.tech/v1/health".into()),
                    url_env: None,
                    topic: Some("secretary".into()),
                },
                HealthTarget {
                    name: "pokoin.com".into(),
                    url: Some("https://pokoin.com".into()),
                    url_env: None,
                    topic: Some("pokoin".into()),
                },
            ],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthTarget {
    pub name: String,
    /// A literal URL...
    #[serde(default)]
    pub url: Option<String>,
    /// ...or the name of an env variable holding it (Lucina's env files).
    #[serde(default)]
    pub url_env: Option<String>,
    /// `pokoin`, `secretary`, ... (relevance and wording).
    #[serde(default)]
    pub topic: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelegramMode {
    /// Never.
    Off,
    /// Only when the app push reached no device (no subscription, muted).
    Fallback,
    /// High priority always, otherwise as `fallback`.
    High,
    /// Every outreach.
    Always,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChannelsCfg {
    /// Timeline item + `POST /v1/notify`.
    pub app: bool,
    pub telegram: TelegramMode,
    /// How the timeline item is written while the Node Lucina owns
    /// `timeline.jsonl` (root-owned in its container): `docker` appends
    /// through `docker exec <container>`, `file` appends directly
    /// (`timeline_path`), `off` writes none (push only).
    pub timeline_sink: String,
    pub docker_container: String,
    pub docker_timeline_path: String,
    pub timeline_path: Option<PathBuf>,
    /// For deep links: `<public_url>/?outreach=<id>`.
    pub public_url: String,
    /// Prep summaries are saved as notes (drafts, never sent anywhere).
    pub save_prep_notes: bool,
}

impl Default for ChannelsCfg {
    fn default() -> Self {
        Self {
            app: true,
            telegram: TelegramMode::Fallback,
            timeline_sink: "docker".into(),
            docker_container: "lucina-nez".into(),
            docker_timeline_path: "/app/data/timeline.jsonl".into(),
            timeline_path: None,
            public_url: "https://getsecretary.tech".into(),
            save_prep_notes: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ComposeCfg {
    /// DeepSeek writes the messages (else templates).
    pub llm: bool,
    /// DeepSeek judges "resolved / still open" for the top open threads.
    pub judge: bool,
    pub max_chars: usize,
    /// Model override (default: lucina-llm's `deepseek-flash`).
    pub model: Option<String>,
    /// Persona file (default: the one built into lucina-server).
    pub persona_path: Option<PathBuf>,
}

impl Default for ComposeCfg {
    fn default() -> Self {
        Self {
            llm: true,
            judge: true,
            max_chars: 700,
            model: None,
            persona_path: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config file {0}: {1}")]
    Read(PathBuf, std::io::Error),
    #[error("config file {0}: {1}")]
    Parse(PathBuf, String),
    #[error("{0}")]
    Invalid(String),
}

impl Config {
    pub fn default_path() -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config/lucina-proactive/config.toml")
    }

    /// Parse TOML (no env overrides).
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let cfg: Config = toml::from_str(text).map_err(|e| e.to_string())?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// The file (missing: defaults), then `LUCINA_PROACTIVE_DRY_RUN` and
    /// `LUCINA_PROACTIVE_TZ`.
    pub fn load(
        path: Option<PathBuf>,
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ConfigError> {
        let path = path
            .or_else(|| get("LUCINA_PROACTIVE_CONFIG").map(PathBuf::from))
            .unwrap_or_else(Self::default_path);
        let mut cfg = match std::fs::read_to_string(&path) {
            Ok(text) => Self::from_toml(&text).map_err(|e| ConfigError::Parse(path.clone(), e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(ConfigError::Read(path, e)),
        };
        if let Some(v) = get("LUCINA_PROACTIVE_DRY_RUN") {
            cfg.dry_run = !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "false" | "0" | "no" | "off"
            );
        }
        if let Some(v) = get("LUCINA_PROACTIVE_TZ") {
            cfg.time_zone = v.trim().to_owned();
        }
        cfg.validate().map_err(ConfigError::Invalid)?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), String> {
        self.time_zone
            .parse::<Tz>()
            .map_err(|_| format!("time_zone: unknown zone {}", self.time_zone))?;
        if !matches!(self.language.as_str(), "it" | "en" | "da") {
            return Err("language: it, en or da".into());
        }
        let t = &self.thresholds;
        if !(0.0..=1.0).contains(&t.send) || !(0.0..=1.0).contains(&t.digest) || t.digest > t.send {
            return Err("thresholds: 0 <= digest <= send <= 1".into());
        }
        let w = &self.weights;
        let sum = w.urgency + w.relevance + w.confidence + w.actionability;
        if (sum - 1.0).abs() > 0.01 {
            return Err(format!("weights must sum to 1 (they sum to {sum:.2})"));
        }
        for (field, v) in [
            ("pulse.time", &self.pulse.time),
            ("pulse.deadline", &self.pulse.deadline),
            ("quiet.start", &self.quiet.start),
            ("quiet.end", &self.quiet.end),
        ] {
            if crate::attention::parse_hhmm(v).is_none() {
                return Err(format!("{field}: HH:MM"));
            }
        }
        if !matches!(
            self.channels.timeline_sink.as_str(),
            "docker" | "file" | "off"
        ) {
            return Err("channels.timeline_sink: docker, file or off".into());
        }
        if self.channels.timeline_sink == "file" && self.channels.timeline_path.is_none() {
            return Err("channels.timeline_path is required with timeline_sink = \"file\"".into());
        }
        for t in &self.health.targets {
            if t.url.is_none() == t.url_env.is_none() {
                return Err(format!(
                    "health target {}: set exactly one of url, url_env",
                    t.name
                ));
            }
        }
        Ok(())
    }

    pub fn tz(&self) -> Tz {
        self.time_zone
            .parse()
            .unwrap_or(chrono_tz::Europe::Copenhagen)
    }
}

/// Secrets and endpoints from the environment. `Debug` never shows values.
#[derive(Clone, Default)]
pub struct Secrets {
    pub database_url: Option<String>,
    pub server_url: Option<String>,
    pub service_token_file: Option<PathBuf>,
    pub telegram_token: Option<String>,
    pub owner_chat_id: Option<String>,
    pub deepseek: DeepSeekConfig,
    pub honcho: Option<HonchoConfig>,
    /// `url_env` health targets resolved: name → URL.
    pub env_urls: BTreeMap<String, String>,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let set = |b: bool| if b { "<set>" } else { "<unset>" };
        f.debug_struct("Secrets")
            .field("database_url", &set(self.database_url.is_some()))
            .field("server_url", &self.server_url)
            .field("service_token_file", &self.service_token_file)
            .field("telegram_token", &set(self.telegram_token.is_some()))
            .field("owner_chat_id", &set(self.owner_chat_id.is_some()))
            .field("deepseek", &self.deepseek)
            .field("honcho", &self.honcho.as_ref().map(|h| h.base_url.clone()))
            .finish()
    }
}

fn first_csv(v: &str) -> Option<String> {
    v.split(',')
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_owned)
}

impl Secrets {
    pub fn from_lookup(cfg: &Config, get: impl Fn(&str) -> Option<String>) -> Self {
        let get = |k: &str| get(k).filter(|v| !v.trim().is_empty());
        let mut deepseek = DeepSeekConfig::from_lookup(get);
        if let Some(p) = &cfg.paths.deepseek_key_file {
            if !matches!(deepseek.key, lucina_llm::KeySource::Inline(_)) {
                deepseek.key = lucina_llm::KeySource::File(p.clone());
            }
        }
        if let Some(m) = &cfg.compose.model {
            deepseek.model = m.clone();
        }
        let honcho = get("HONCHO_URL").map(|_| HonchoConfig::from_lookup(get));
        let env_urls = cfg
            .health
            .targets
            .iter()
            .filter_map(|t| t.url_env.as_ref())
            .filter_map(|name| get(name).map(|v| (name.clone(), v.trim().to_owned())))
            .collect();
        Self {
            database_url: get("LUCINA_PROACTIVE_DATABASE_URL")
                .or_else(|| get("GROCERY_DATABASE_URL")),
            server_url: get("LUCINA_SECRETARY_SERVER_URL")
                .map(|u| u.trim().trim_end_matches('/').to_owned()),
            service_token_file: cfg
                .paths
                .service_token_file
                .clone()
                .or_else(|| get("LUCINA_SECRETARY_SERVICE_TOKEN_FILE").map(PathBuf::from)),
            telegram_token: get("TELEGRAM_BOT_TOKEN_LUCINA").map(|t| t.trim().to_owned()),
            owner_chat_id: get("LUCINA_OPERATOR_CHAT_ID")
                .or_else(|| get("LUCINA_GMAIL_LEARN_CHAT_ID"))
                .or_else(|| get("TELEGRAM_ALLOWED_CHAT_IDS").and_then(|v| first_csv(&v))),
            deepseek,
            honcho,
            env_urls,
        }
    }
}

/// The env names the engine reads (and the only ones taken from env files).
pub const ENV_NAMES: &[&str] = &[
    "LUCINA_PROACTIVE_DATABASE_URL",
    "GROCERY_DATABASE_URL",
    "LUCINA_SECRETARY_SERVER_URL",
    "LUCINA_SECRETARY_SERVICE_TOKEN_FILE",
    "TELEGRAM_BOT_TOKEN_LUCINA",
    "LUCINA_OPERATOR_CHAT_ID",
    "LUCINA_GMAIL_LEARN_CHAT_ID",
    "TELEGRAM_ALLOWED_CHAT_IDS",
    "DEEPSEEK_API_KEY",
    "DEEPSEEK_KEY_FILE",
    "DEEPSEEK_BASE_URL",
    "DEEPSEEK_MODEL",
    "DEEPSEEK_TIMEOUT_MS",
    "DEEPSEEK_MAX_RETRIES",
    "HONCHO_URL",
    "HONCHO_API_KEY",
    "HONCHO_WORKSPACE_ID",
    "HONCHO_AGENT_PEER_ID",
    "LUCINA_OWNER_USER_PEER_ID",
    "HONCHO_USE_VECTOR_SEARCH",
    "HONCHO_TIMEOUT_MS",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate_and_partial_toml() {
        Config::default().validate().unwrap();
        let c = Config::from_toml(
            r#"
            time_zone = "Europe/Rome"
            [budget]
            max_per_day = 2
            [thresholds]
            send = 0.8
            [[health.targets]]
            name = "poko bot"
            url_env = "BOT_HOSPITAL_POKO_HEALTH_URL"
            "#,
        )
        .unwrap();
        assert_eq!(c.budget.max_per_day, 2);
        assert_eq!(c.thresholds.send, 0.8);
        assert_eq!(c.thresholds.digest, 0.45);
        assert_eq!(c.health.targets.len(), 1);
        assert_eq!(
            c.budget.cooldown("service_down"),
            chrono::Duration::minutes(30)
        );
        assert_eq!(c.budget.cooldown("unknown"), chrono::Duration::minutes(120));
        assert!(Config::from_toml("bogus = 1").is_err());
        assert!(Config::from_toml("[weights]\nurgency = 0.9").is_err());
        assert!(Config::from_toml("[pulse]\ntime = \"8\"").is_err());
    }

    #[test]
    fn the_example_config_parses() {
        let c = Config::from_toml(include_str!("../config.example.toml")).unwrap();
        assert_eq!(c.health.targets.len(), 3);
        assert!(c.dry_run);
    }

    #[test]
    fn env_overrides_and_secrets() {
        let env = |k: &str| match k {
            "LUCINA_PROACTIVE_DRY_RUN" => Some("false".to_owned()),
            "GROCERY_DATABASE_URL" => Some("postgres://x".to_owned()),
            "TELEGRAM_ALLOWED_CHAT_IDS" => Some(" ,42,43".to_owned()),
            "LUCINA_SECRETARY_SERVER_URL" => Some("https://s.example/".to_owned()),
            _ => None,
        };
        let c = Config::load(Some(PathBuf::from("/nonexistent.toml")), env).unwrap();
        assert!(!c.dry_run);
        let s = Secrets::from_lookup(&c, env);
        assert_eq!(s.owner_chat_id.as_deref(), Some("42"));
        assert_eq!(s.server_url.as_deref(), Some("https://s.example"));
        assert!(!format!("{s:?}").contains("postgres://x"));
        // Unset means dry-run.
        let c = Config::load(Some(PathBuf::from("/nonexistent.toml")), |_| None).unwrap();
        assert!(c.dry_run);
    }
}
