//! Lucina voice memos (`lucina-memo` on nezopt).
//!
//! A device records a memo and uploads it in resumable chunks
//! (`/v1/memos`, see [`api`]). When the upload completes, the [`worker`]
//! runs the pipeline, one memo at a time, each step resumable:
//! transcribe (local whisper.cpp, [`transcribe`]) → Lucina's summary
//! ([`summarize`]) → PDF ([`render`]) → Obsidian note ([`obsidian`]) →
//! notification ([`notify`]).
//!
//! Meetings recorded on the owner's Mac (`/v1/meetings`, see [`meetings`]) run their own
//! pipeline: per-track Whisper with word timings, speaker diarization ([`diarize`]), roles and
//! utterance kinds, a brief, the archive on the MyBook, Obsidian, memory and a notification.
//! Voice memos get speaker labels from the same diarizer (best effort).
//!
//! Recordings stay on this host and the owner's Pi; only transcript text goes
//! to Lucina's chat model for the summary.
pub mod api;
pub mod archive;
pub mod calendar;
pub mod diarize;
pub mod events;
pub mod kernel;
pub mod library;
pub mod meetings;
pub mod model;
pub mod notify;
pub mod obsidian;
pub mod prompter;
pub mod render;
pub mod search;
pub mod store;
pub mod stt;
pub mod summarize;
pub mod transcribe;
pub mod vocab;
pub mod worker;

use std::path::PathBuf;
use std::sync::Arc;

use chrono_tz::Tz;

/// Everything the service reads from its environment.
#[derive(Debug, Clone)]
pub struct Config {
    pub data_dir: PathBuf,
    pub archive_dir: Option<PathBuf>,
    pub archive_require_mount: Option<PathBuf>,
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
    /// Lucina Prompter's live speech-to-text and judge ([`prompter`]).
    pub prompter: prompter::PrompterConfig,
    pub vault: Option<obsidian::VaultTarget>,
    pub notify: notify::NotifyConfig,
    pub calendar_url: Option<String>,
    pub geocode_url: Option<String>,
    pub chrome: PathBuf,
    /// Max pipeline attempts before a memo is `failed`.
    pub max_attempts: u32,
    /// The meetings archive (`LUCINA_MEETINGS_DIR`); must exist (the MyBook mounted).
    pub meetings_dir: PathBuf,
    /// Vault folder of the meeting notes (`LUCINA_MEETINGS_OBSIDIAN_FOLDER`, default `Meetings`).
    pub meetings_folder: String,
    /// The owner's name for his own voice (`LUCINA_OWNER_NAME`).
    pub owner_name: String,
    /// Voice profiles (`LUCINA_VOICES_FILE`, default `<data_dir>/voices.json`).
    pub voices_path: PathBuf,
    /// None = diarization off.
    pub diarize: Option<diarize::DiarizeConfig>,
    /// Lucina's memory entry for each meeting (`LUCINA_MEETINGS_MEMORY_MODE`, else
    /// `LUCINA_MEMORY_MODE`; only `readwrite` writes).
    pub memory_mode: lucina_memory::MemoryMode,
    pub honcho: lucina_memory::HonchoConfig,
}

impl Config {
    pub fn from_lookup(get: impl Fn(&str) -> Option<String> + Copy) -> Self {
        let home = get("HOME").unwrap_or_else(|| "/home/nez".into());
        // One WhisperConfig, so the prompter's engine shares its Voice Learn file watcher.
        let whisper = transcribe::WhisperConfig::from_lookup(get);
        Self {
            data_dir: PathBuf::from(
                get("LUCINA_MEMO_DIR")
                    .unwrap_or_else(|| format!("{home}/Projects/Lucina/data/memos")),
            ),
            archive_dir: get("LUCINA_MEMO_ARCHIVE_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
            archive_require_mount: {
                let v = get("LUCINA_MEMO_ARCHIVE_REQUIRE_MOUNT")
                    .unwrap_or_else(|| "/home/nez/mnt/mybook".into());
                (!v.is_empty()).then(|| PathBuf::from(v))
            },
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
            prompter: prompter::PrompterConfig::from_lookup(get, &whisper),
            whisper,
            vault: obsidian::VaultTarget::from_lookup(get),
            notify: notify::NotifyConfig::from_lookup(get),
            calendar_url: get("LUCINA_SECRETARY_SERVER_URL"),
            geocode_url: get("LUCINA_MEMO_GEOCODE_URL"),
            chrome: PathBuf::from(
                get("LUCINA_MEMO_CHROME").unwrap_or_else(|| "/usr/bin/google-chrome".into()),
            ),
            max_attempts: get("LUCINA_MEMO_MAX_ATTEMPTS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(5),
            meetings_dir: PathBuf::from(
                get("LUCINA_MEETINGS_DIR")
                    .unwrap_or_else(|| format!("{home}/mnt/mybook/lucina-meetings")),
            ),
            meetings_folder: get("LUCINA_MEETINGS_OBSIDIAN_FOLDER")
                .map(|f| f.trim().trim_matches('/').to_owned())
                .filter(|f| !f.is_empty() && !f.split('/').any(|p| p == ".." || p.is_empty()))
                .unwrap_or_else(|| "Meetings".into()),
            owner_name: get("LUCINA_OWNER_NAME")
                .map(|n| n.trim().to_owned())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Giuseppe".into()),
            voices_path: get("LUCINA_VOICES_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(
                        get("LUCINA_MEMO_DIR")
                            .unwrap_or_else(|| format!("{home}/Projects/Lucina/data/memos")),
                    )
                    .join("voices.json")
                }),
            diarize: diarize::DiarizeConfig::from_lookup(get),
            memory_mode: get("LUCINA_MEETINGS_MEMORY_MODE")
                .or_else(|| get("LUCINA_MEMORY_MODE"))
                .and_then(|m| lucina_memory::MemoryMode::parse(&m))
                .unwrap_or(lucina_memory::MemoryMode::Off),
            honcho: lucina_memory::HonchoConfig::from_lookup(get),
        }
    }

    pub fn from_env() -> Self {
        Self::from_lookup(|k| {
            std::env::var(k)
                .ok()
                .filter(|v| !v.is_empty() || k == "LUCINA_MEMO_ARCHIVE_REQUIRE_MOUNT")
        })
    }

    /// The app link of a memo (`/memos/<id>` in the web app).
    pub fn memo_url(&self, id: &str) -> String {
        format!("{}/memos/{id}", self.public_url)
    }

    /// The meeting's transcript in the browser (through p1's gateway, the owner's cookie).
    pub fn meeting_transcript_url(&self, id: &str) -> String {
        format!("{}/v1/meetings/{id}/transcript", self.public_url)
    }
}

/// Shared state of the service.
pub struct App {
    pub cfg: Config,
    pub store: store::Store,
    pub calendar: calendar::CalendarCache,
    pub(crate) archive_lock: std::sync::Mutex<()>,
    pub(crate) archive_retries:
        std::sync::Mutex<std::collections::HashMap<String, (u32, chrono::DateTime<chrono::Utc>)>>,
    pub llm: Arc<dyn lucina_llm::ChatModel>,
    pub llm_label: String,
    /// Wakes the worker when a memo is queued.
    pub wake: tokio::sync::Notify,
    /// Wakes the meetings worker.
    pub meetings_wake: tokio::sync::Notify,
    /// ffmpeg + Whisper for meetings.
    pub stt: Arc<dyn transcribe::Stt>,
    pub voices: diarize::profiles::VoiceBook,
    /// Lucina's memory (meeting entries); None when off.
    pub memory: Option<lucina_memory::Memory>,
    /// Lucina Prompter: the live speech-to-text engine and the judge client.
    pub prompter: prompter::Live,
    meetings: std::sync::Mutex<Option<Arc<meetings::store::MeetingStore>>>,
    diarizer: std::sync::Mutex<Option<Arc<dyn diarize::Diarizer>>>,
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
        let voices = diarize::profiles::VoiceBook::open(&cfg.voices_path)?;
        let memory = match cfg.memory_mode {
            lucina_memory::MemoryMode::ReadWrite => Some(lucina_memory::Memory::new(
                cfg.memory_mode,
                cfg.honcho.clone(),
            )?),
            _ => None,
        };
        Ok(Arc::new(Self {
            gateway_token: read_token(&cfg.gateway_token_file)?,
            service_token: read_token(&cfg.service_token_file)?,
            stt: Arc::new(transcribe::Whisper(cfg.whisper.clone())),
            voices,
            memory,
            prompter: prompter::Live::new(&cfg.prompter),
            meetings: std::sync::Mutex::new(None),
            diarizer: std::sync::Mutex::new(None),
            calendar: calendar::CalendarCache::open(&cfg.data_dir)?,
            archive_lock: std::sync::Mutex::new(()),
            archive_retries: std::sync::Mutex::default(),
            cfg,
            store,
            llm,
            llm_label,
            wake: tokio::sync::Notify::new(),
            meetings_wake: tokio::sync::Notify::new(),
        }))
    }

    /// The meetings archive, opened on first use and again after a failure (the MyBook may be
    /// mounted after the service started).
    pub fn meetings(&self) -> Result<Arc<meetings::store::MeetingStore>, store::StoreError> {
        let mut g = self.meetings.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(s) = g.as_ref() {
            return Ok(s.clone());
        }
        let s = Arc::new(meetings::store::MeetingStore::open(
            &self.cfg.meetings_dir,
            self.cfg.timezone,
        )?);
        *g = Some(s.clone());
        Ok(s)
    }

    /// The diarizer, loaded on first use (None when off; an error while the models or ONNX
    /// Runtime are missing).
    pub fn diarizer(&self) -> Result<Option<Arc<dyn diarize::Diarizer>>, diarize::DiarizeError> {
        let mut g = self.diarizer.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(d) = g.as_ref() {
            return Ok(Some(d.clone()));
        }
        let Some(cfg) = &self.cfg.diarize else {
            return Ok(None);
        };
        let d = diarize::load(cfg)?;
        *g = Some(d.clone());
        Ok(Some(d))
    }

    /// Fakes for tests: speech to text and the diarizer.
    pub fn with_engines(
        self: Arc<Self>,
        stt: Option<Arc<dyn transcribe::Stt>>,
        diarizer: Option<Arc<dyn diarize::Diarizer>>,
    ) -> Arc<Self> {
        let mut a = Arc::try_unwrap(self).ok().expect("unshared app");
        if let Some(s) = stt {
            a.stt = s;
        }
        if let Some(d) = diarizer {
            a.diarizer = std::sync::Mutex::new(Some(d));
            if a.cfg.diarize.is_none() {
                a.cfg.diarize = diarize::DiarizeConfig::from_lookup(|_| None);
            }
        }
        Arc::new(a)
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
