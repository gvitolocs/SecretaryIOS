//! Lucina voice memos (`lucina-memo` on nezopt).
//!
//! A device records a memo and uploads it in resumable chunks
//! (`/v1/memos`, see [`api`]). When the upload completes, the [`worker`]
//! runs the pipeline, one memo at a time, each step resumable:
//! transcribe (local whisper.cpp, [`transcribe`]) → Lucina's summary
//! ([`summarize`]) → PDF ([`render`]) → Obsidian note ([`obsidian`]) →
//! notification ([`notify`]).
//!
//! Recordings stay on this host and the owner's Pi; only transcript text goes
//! to Lucina's chat model for the summary.
pub mod api;
pub mod kernel;
pub mod library;
pub mod model;
pub mod notify;
pub mod obsidian;
pub mod render;
pub mod search;
pub mod store;
pub mod summarize;
pub mod transcribe;
pub mod worker;

use std::path::PathBuf;
use std::sync::Arc;

use chrono_tz::Tz;

/// Everything the service reads from its environment.
#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    /// Listen addresses (`127.0.0.1:8795`, the tailnet address for p1's Caddy).
    pub binds: Vec<String>,
    /// Shared secret Caddy on p1 adds as `X-Memo-Gateway` after the device
    /// passed secretary-server's auth (forward_auth).
    pub gateway_token_file: Option<PathBuf>,
    /// Bearer token for local callers (Lucina's tools, scripts).
    pub service_token_file: Option<PathBuf>,
    /// Public base for links, e.g. `https://getsecretary.tech`.
    pub public_url: String,
    pub timezone: Tz,
    pub whisper: transcribe::WhisperConfig,
    pub vault: Option<obsidian::VaultTarget>,
    pub notify: notify::NotifyConfig,
    pub chrome: PathBuf,
    /// Max pipeline attempts before a memo is `failed`.
    pub max_attempts: u32,
}

impl Config {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String> + Copy) -> Self {
        let home = get("HOME").unwrap_or_else(|| "/home/nez".into());
        Self {
            data_dir: PathBuf::from(
                get("LUCINA_MEMO_DIR")
                    .unwrap_or_else(|| format!("{home}/Projects/Lucina/data/memos")),
            ),
            binds: get("LUCINA_MEMO_BIND")
                .unwrap_or_else(|| "127.0.0.1:8795".into())
                .split(',')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect(),
            gateway_token_file: get("LUCINA_MEMO_GATEWAY_TOKEN_FILE").map(PathBuf::from),
            service_token_file: get("LUCINA_MEMO_SERVICE_TOKEN_FILE").map(PathBuf::from),
            public_url: get("LUCINA_MEMO_PUBLIC_URL")
                .unwrap_or_else(|| "https://getsecretary.tech".into())
                .trim_end_matches('/')
                .to_owned(),
            timezone: get("LUCINA_MEMO_TIMEZONE")
                .or_else(|| get("SECRETARY_TIMEZONE"))
                .or_else(|| get("LUCINA_TIMEZONE"))
                .and_then(|z| z.trim().parse().ok())
                .unwrap_or(chrono_tz::Europe::Copenhagen),
            whisper: transcribe::WhisperConfig::from_lookup(get),
            vault: obsidian::VaultTarget::from_lookup(get),
            notify: notify::NotifyConfig::from_lookup(get),
            chrome: PathBuf::from(
                get("LUCINA_MEMO_CHROME").unwrap_or_else(|| "/usr/bin/google-chrome".into()),
            ),
            max_attempts: get("LUCINA_MEMO_MAX_ATTEMPTS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
        }
    }

    pub fn from_env() -> Self {
        Self::from_lookup(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
    }

    /// The app link of a memo (`/memos/<id>` in the web app).
    pub fn memo_url(&self, id: &str) -> String {
        format!("{}/memos/{id}", self.public_url)
    }
}

/// Shared state of the service.
pub struct App {
    pub cfg: Config,
    pub store: store::Store,
    pub llm: Arc<dyn lucina_llm::ChatModel>,
    pub llm_label: String,
    /// Wakes the worker when a memo is queued.
    pub wake: tokio::sync::Notify,
    gateway_token: Option<String>,
    service_token: Option<String>,
}

fn read_token(p: &Option<PathBuf>) -> anyhow::Result<Option<String>> {
    match p {
        None => Ok(None),
        Some(p) => {
            let t = std::fs::read_to_string(p)
                .map_err(|e| anyhow::anyhow!("token file {}: {e}", p.display()))?
                .trim()
                .to_owned();
            anyhow::ensure!(t.len() >= 24, "token in {} is too short", p.display());
            Ok(Some(t))
        }
    }
}

impl App {
    pub fn new(
        cfg: Config,
        llm: Arc<dyn lucina_llm::ChatModel>,
        llm_label: String,
    ) -> anyhow::Result<Arc<Self>> {
        let store = store::Store::open(&cfg.data_dir)?;
        Ok(Arc::new(Self {
            gateway_token: read_token(&cfg.gateway_token_file)?,
            service_token: read_token(&cfg.service_token_file)?,
            cfg,
            store,
            llm,
            llm_label,
            wake: tokio::sync::Notify::new(),
        }))
    }

    /// Tokens set directly (tests).
    pub fn with_tokens(self: Arc<Self>, gateway: Option<&str>, service: Option<&str>) -> Arc<Self> {
        let mut a = Arc::try_unwrap(self).ok().expect("unshared app");
        a.gateway_token = gateway.map(str::to_owned);
        a.service_token = service.map(str::to_owned);
        Arc::new(a)
    }

    pub(crate) fn gateway_token(&self) -> Option<&str> {
        self.gateway_token.as_deref()
    }
    pub(crate) fn service_token(&self) -> Option<&str> {
        self.service_token.as_deref()
    }
}
