//! Wiring: config + secrets → an [`Engine`] with real sources and channels.

use std::sync::Arc;

use anyhow::Result;

use crate::channels::{self, BotApi, Channels, Telegram};
use crate::compose::{Composer, LlmComposer, LlmJudge, TemplateComposer, PERSONA_DEFAULT};
use crate::config::{Config, Secrets};
use crate::engine::{Deps, Engine};
use crate::secretary::{HttpSecretary, NoSecretary, SecretaryApi};
use crate::sources::calendar::CalendarSource;
use crate::sources::health::HealthSource;
use crate::sources::notes::NotesSource;
use crate::sources::reminders::ReminderSource;
use crate::sources::threads::{Judge, ThreadSource};
use crate::sources::{
    node_paths, DeviceFeed, FileDevice, FilePresence, FileReminders, FileTimeline, HealthProbe,
    HonchoMemory, HttpProbe, MemoryFeed, PresenceFeed, ReminderFeed, Source, TimelineFeed,
};
use crate::store::Store;

/// The parts a run can swap (the replay swaps feeds and the probe).
pub struct Parts {
    pub api: Arc<dyn SecretaryApi>,
    pub reminders: Arc<dyn ReminderFeed>,
    pub timeline: Arc<dyn TimelineFeed>,
    pub presence: Arc<dyn PresenceFeed>,
    pub device: Arc<dyn DeviceFeed>,
    pub probe: Arc<dyn HealthProbe>,
    pub memory: Option<Arc<dyn MemoryFeed>>,
    pub model: Option<Arc<dyn lucina_llm::ChatModel>>,
    pub telegram: Option<Arc<dyn Telegram>>,
}

pub fn live_parts(cfg: &Config, secrets: &Secrets) -> Result<Parts> {
    let (reminders, timeline, presence, device) = node_paths(&cfg.paths.node_data_dir);
    let api: Arc<dyn SecretaryApi> = match (&secrets.server_url, &secrets.service_token_file) {
        (Some(url), Some(tok)) => Arc::new(HttpSecretary::new(url, tok.clone())?),
        _ => Arc::new(NoSecretary),
    };
    let memory: Option<Arc<dyn MemoryFeed>> = match &secrets.honcho {
        Some(h) => lucina_memory::Memory::new(lucina_memory::MemoryMode::ReadOnly, h.clone())
            .ok()
            .map(|memory| Arc::new(HonchoMemory { memory }) as Arc<dyn MemoryFeed>),
        None => None,
    };
    let model: Option<Arc<dyn lucina_llm::ChatModel>> = if cfg.compose.llm || cfg.compose.judge {
        let ds = lucina_llm::DeepSeek::new(secrets.deepseek.clone())?;
        ds.has_key()
            .then(|| Arc::new(ds) as Arc<dyn lucina_llm::ChatModel>)
    } else {
        None
    };
    let telegram: Option<Arc<dyn Telegram>> = match &secrets.telegram_token {
        Some(t) => Some(Arc::new(BotApi::new(t.clone())?)),
        None => None,
    };
    Ok(Parts {
        api,
        reminders: Arc::new(FileReminders::new(reminders)),
        timeline: Arc::new(FileTimeline::new(timeline)),
        presence: Arc::new(FilePresence::new(presence)),
        device: Arc::new(FileDevice::new(device)),
        probe: Arc::new(HttpProbe::new()?),
        memory,
        model,
        telegram,
    })
}

pub fn persona(cfg: &Config) -> String {
    cfg.compose
        .persona_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_else(|| PERSONA_DEFAULT.to_owned())
}

pub fn engine(cfg: Arc<Config>, secrets: &Secrets, store: Arc<dyn Store>, p: Parts) -> Engine {
    let composer: Arc<dyn Composer> = match (&p.model, cfg.compose.llm) {
        (Some(m), true) => Arc::new(LlmComposer {
            model: m.clone(),
            persona: persona(&cfg),
            max_chars: cfg.compose.max_chars,
        }),
        _ => Arc::new(TemplateComposer),
    };
    let judge: Option<Arc<dyn Judge>> = match (&p.model, cfg.compose.judge) {
        (Some(m), true) => Some(Arc::new(LlmJudge {
            model: m.clone(),
            language: cfg.language.clone(),
        })),
        _ => None,
    };
    let mut sources: Vec<Arc<dyn Source>> = Vec::new();
    let sc = &cfg.sources;
    if sc.calendar.enabled {
        sources.push(Arc::new(CalendarSource {
            cfg: cfg.clone(),
            api: p.api.clone(),
            memory: p.memory.clone(),
            timeline: p.timeline.clone(),
        }));
    }
    if sc.reminders.enabled {
        sources.push(Arc::new(ReminderSource {
            cfg: cfg.clone(),
            feed: p.reminders.clone(),
            api: p.api.clone(),
        }));
    }
    if sc.notes.enabled {
        sources.push(Arc::new(NotesSource {
            cfg: cfg.clone(),
            api: p.api.clone(),
        }));
    }
    if sc.timeline.enabled {
        sources.push(Arc::new(ThreadSource {
            cfg: cfg.clone(),
            timeline: p.timeline.clone(),
            judge,
        }));
    }
    if cfg.health.enabled {
        sources.push(Arc::new(HealthSource {
            cfg: cfg.clone(),
            probe: p.probe.clone(),
            env_urls: secrets.env_urls.clone(),
        }));
    }
    let channels = Channels {
        cfg: cfg.channels.clone(),
        api: p.api.clone(),
        sink: channels::sink_from(&cfg.channels),
        telegram: p.telegram.clone(),
        chat_id: secrets.owner_chat_id.clone(),
    };
    Engine::new(Deps {
        cfg,
        store,
        api: p.api,
        sources,
        composer,
        channels,
        presence: p.presence,
        device: p.device,
        timeline: p.timeline,
        reminders: p.reminders,
    })
}
