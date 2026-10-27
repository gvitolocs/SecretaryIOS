//! The Lucina Telegram bot (@Lucina00_bot), the Node `src/telegram.js`
//! Lucina persona path on the Rust turn pipeline.
//!
//! Per update, in Node's middleware order: the private OTP middleware
//! (codes never reach the model, the timeline or memory), the allow-list
//! (`TELEGRAM_ALLOWED_USERNAMES`, else `TELEGRAM_ALLOWED_CHAT_IDS`, empty:
//! everyone) with `TELEGRAM_UNAUTHORIZED_DM=decline` (one refusal per sender
//! per 24 h in private chats, silence elsewhere), `/start`, `/cancel_pdf`,
//! `/status [full]`, then text, voice/audio/video_note (Whisper, then the
//! text path), documents and photos (the share pipeline's per-file handling,
//! then its tool loop). Text turns run [`crate::chat::channel_turn`] with
//! channel `telegram`, Honcho session `telegram_chat_<chat id>`, the peer
//! and label rules of Node, Node's `recent-telegram-chat.json` (Rust is its
//! only writer once this runs) and the owner timeline (owner `from.id`
//! only; groups get `meta.chatTitle`). A job started here sends its result
//! to the chat when it finishes.
//!
//! Not ported: PDF follow-up questions (`handlePdfFollowup`: the share
//! pipeline keeps no per-chat PDF session, so `/cancel_pdf` always answers
//! that nothing is running), provider-wait notices (the Rust planner has no
//! wait-out retry events), Node's rich multi-message replies (one plain
//! reply), Gemini/OpenAI transcription fallbacks (local Whisper only), and
//! the Node-only actions (image generation, graphs, web crawl, log
//! inspection, Ollama status, Poko posts) which the planner path answers
//! with "cannot run yet".
pub mod format;
pub mod status;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Utc;
use lucina_memory::surface::{ChannelKind, TelegramChatKind, TelegramFacts};
use lucina_memory::{TurnFacts, TurnSurface};
use lucina_store::recent::RecentChat;
use lucina_store::timeline::{Attachment, Meta, TimelineStore, TimelineTurn};
use lucina_telegram::poller::{Handler, Poller, PollerConfig, SharedHealth};
use lucina_telegram::{BotApi, SendOptions};
use serde_json::{json, Value};

use crate::chat::{ChannelOrigin, ReplySink};
use crate::AppState;

const DECLINE_REPEAT: Duration = Duration::from_secs(24 * 60 * 60);
const DECLINE_KEEP: usize = 5000;
/// The Bot API serves files up to 20 MB.
const MAX_FILE_BYTES: usize = 20 * 1024 * 1024;
const TYPING_EVERY: Duration = Duration::from_secs(4);
const AUDIO_KINDS: [&str; 3] = ["voice", "audio", "video_note"];

/// `LUCINA_TELEGRAM_ENABLED` and Node's Telegram variables.
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub allowed_chat_ids: Vec<String>,
    pub allowed_usernames: Vec<String>,
    /// `TELEGRAM_UNAUTHORIZED_DM=decline` (default `ignore`).
    pub decline_unauthorized: bool,
    /// `TELEGRAM_MAX_AUDIO_BYTES` (25 MiB; 0: no limit).
    pub max_audio_bytes: u64,
    /// `TELEGRAM_MAX_AUDIO_DURATION_SECONDS` (900; 0: no limit).
    pub max_audio_seconds: u64,
    /// Node `lucinaOperatorChatId`: `LUCINA_OPERATOR_CHAT_ID`, else
    /// `LUCINA_GMAIL_LEARN_CHAT_ID`, else the first allowed chat id.
    pub operator_chat_id: Option<String>,
    /// `LUCINA_OPERATOR_USER_PEER_ID` (default `giuseppe`).
    pub operator_peer_id: String,
    /// `HERMES_GATEWAY_LOCK_DIR` (default `~/.local/state/hermes-gateway-locks`).
    pub lock_dir: Option<PathBuf>,
    /// `HERMES_GATEWAY_INSTANCE` (default `lucina-rs`).
    pub instance: String,
    /// `LUCINA_RECENT_CHAT_PATH` (default `<data>/recent-telegram-chat.json`).
    pub recent_path: PathBuf,
    /// `HERMES_CHAT_REGISTRY_PATH` (default `<data>/telegram-chats.json`).
    pub registry_path: PathBuf,
}

fn csv(v: Option<String>) -> Vec<String> {
    v.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

impl TelegramConfig {
    pub fn from_lookup(get: &impl Fn(&str) -> Option<String>, data_dir: &Path) -> Self {
        let num = |k: &str, d: u64| {
            get(k)
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v >= 0.0)
                .map_or(d, |v| v as u64)
        };
        let allowed_chat_ids = csv(get("TELEGRAM_ALLOWED_CHAT_IDS"));
        let operator_chat_id = get("LUCINA_OPERATOR_CHAT_ID")
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .or_else(|| get("LUCINA_GMAIL_LEARN_CHAT_ID").map(|s| s.trim().to_owned()))
            .filter(|s| !s.is_empty())
            .or_else(|| allowed_chat_ids.first().cloned());
        Self {
            enabled: get("LUCINA_TELEGRAM_ENABLED")
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("true")),
            allowed_usernames: csv(get("TELEGRAM_ALLOWED_USERNAMES")),
            allowed_chat_ids,
            decline_unauthorized: get("TELEGRAM_UNAUTHORIZED_DM")
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("decline")),
            max_audio_bytes: num("TELEGRAM_MAX_AUDIO_BYTES", 25 * 1024 * 1024),
            max_audio_seconds: num("TELEGRAM_MAX_AUDIO_DURATION_SECONDS", 15 * 60),
            operator_chat_id,
            operator_peer_id: get("LUCINA_OPERATOR_USER_PEER_ID")
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "giuseppe".into()),
            lock_dir: get("HERMES_GATEWAY_LOCK_DIR").map(PathBuf::from),
            instance: get("HERMES_GATEWAY_INSTANCE")
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "lucina-rs".into()),
            recent_path: get("LUCINA_RECENT_CHAT_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| data_dir.join("recent-telegram-chat.json")),
            registry_path: get("HERMES_CHAT_REGISTRY_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| data_dir.join("telegram-chats.json")),
        }
    }
}

/// Node `normalizeTelegramUsername`.
fn normalize_username(s: &str) -> String {
    s.trim_start_matches('@').trim().to_lowercase()
}

/// Node `safeId`.
fn safe_id(prefix: &str, value: &str) -> String {
    let v = if value.is_empty() { "unknown" } else { value };
    let clean: String = v
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{prefix}_{clean}")
}

fn id_str(v: &Value) -> String {
    match v {
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// The parts of a message the handlers look at.
#[derive(Debug, Clone)]
pub struct Msg {
    pub update_id: u64,
    pub message_id: i64,
    pub chat_id: String,
    pub chat_type: String,
    pub chat_title: Option<String>,
    pub from_id: String,
    pub from_is_bot: bool,
    pub username: String,
    pub first_name: String,
    pub last_name: String,
    pub language: String,
    pub raw: Value,
}

impl Msg {
    pub fn parse(update: &Value) -> Option<Self> {
        let m = update.get("message")?;
        let chat = &m["chat"];
        let from = &m["from"];
        Some(Self {
            update_id: update["update_id"].as_u64().unwrap_or_default(),
            message_id: m["message_id"].as_i64().unwrap_or_default(),
            chat_id: id_str(&chat["id"]),
            chat_type: chat["type"].as_str().unwrap_or("").into(),
            chat_title: chat["title"].as_str().map(str::to_owned),
            from_id: id_str(&from["id"]),
            from_is_bot: from["is_bot"].as_bool().unwrap_or(false),
            username: from["username"].as_str().unwrap_or("").into(),
            first_name: from["first_name"].as_str().unwrap_or("").into(),
            last_name: from["last_name"].as_str().unwrap_or("").into(),
            language: from["language_code"].as_str().unwrap_or("").into(),
            raw: m.clone(),
        })
    }

    fn text(&self) -> Option<&str> {
        self.raw["text"].as_str()
    }

    fn caption(&self) -> &str {
        self.raw["caption"].as_str().unwrap_or("").trim()
    }

    fn is_private(&self) -> bool {
        self.chat_type == "private"
    }
}

/// Node `canonicalizeUserPeerId`, against the configured owner chat (Node
/// compared against the current chat id, which made any private chat the
/// owner's; the configured id is the one the docs describe).
fn canonical_peer(peer: &str, canonical: &str, owner_chat: Option<&str>) -> String {
    if peer.is_empty() || peer == canonical {
        return peer.to_owned();
    }
    if peer == "flareon_owner" {
        return canonical.to_owned();
    }
    if let Some(id) = peer.strip_prefix("telegram_user_") {
        if !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_digit())
            && owner_chat.is_some_and(|o| o == id)
        {
            return canonical.to_owned();
        }
    }
    peer.to_owned()
}

/// The peer, session and label of a message (Node `userPeerIdFromContext`,
/// `sessionIdFromContext`, `userLabelFromContext`).
pub struct Identity {
    pub peer: String,
    pub session: String,
    pub label: String,
    pub operator: bool,
}

/// Unauthorized senders already refused (Node `declinedSenders`).
#[derive(Default)]
struct Declines {
    at: HashMap<String, Instant>,
    order: VecDeque<String>,
}

impl Declines {
    /// `true` when this sender gets the refusal now.
    fn take(&mut self, sender: &str, now: Instant) -> bool {
        if self
            .at
            .get(sender)
            .is_some_and(|t| now.duration_since(*t) < DECLINE_REPEAT)
        {
            return false;
        }
        if self.at.insert(sender.to_owned(), now).is_none() {
            self.order.push_back(sender.to_owned());
        }
        if self.at.len() > DECLINE_KEEP {
            if let Some(oldest) = self.order.pop_front() {
                self.at.remove(&oldest);
            }
        }
        true
    }
}

/// The bot: configuration, the Bot API client, the Rust state.
pub struct Bot {
    st: AppState,
    api: BotApi,
    cfg: TelegramConfig,
    health: SharedHealth,
    recent: Arc<RecentChat>,
    declines: Mutex<Declines>,
    /// For turns that are not recorded on the owner timeline.
    off_timeline: Arc<TimelineStore>,
    /// The lease acquisition the shared files were last loaded for.
    loaded_for: std::sync::atomic::AtomicU64,
}

impl Bot {
    pub fn new(st: AppState, api: BotApi) -> Arc<Self> {
        let cfg = st.cfg.telegram.clone();
        Arc::new(Self {
            health: st.telegram_health.clone(),
            recent: st.telegram_recent.clone(),
            off_timeline: Arc::new(TimelineStore::new(
                st.cfg.data_dir.join("lucina-rs/telegram-unrecorded.jsonl"),
                1,
                false,
            )),
            declines: Mutex::default(),
            loaded_for: std::sync::atomic::AtomicU64::new(0),
            cfg,
            api,
            st,
        })
    }

    fn persona(&self) -> &str {
        &self.st.cfg.persona
    }

    /// Node `isAllowedChat`.
    pub fn is_allowed(&self, m: &Msg) -> bool {
        let users: Vec<String> = self
            .cfg
            .allowed_usernames
            .iter()
            .map(|u| normalize_username(u))
            .filter(|u| !u.is_empty())
            .collect();
        if !users.is_empty() {
            let username = normalize_username(&m.username);
            return !username.is_empty() && users.contains(&username);
        }
        self.cfg.allowed_chat_ids.is_empty() || self.cfg.allowed_chat_ids.contains(&m.chat_id)
    }

    /// Node `skipUnauthorizedTelegram` + `maybeDeclineUnauthorized`.
    async fn skip_unauthorized(&self, m: &Msg) -> bool {
        if self.is_allowed(m) {
            return false;
        }
        tracing::info!(
            update_id = m.update_id,
            chat_id = %m.chat_id,
            from_id = %m.from_id,
            username = %m.username,
            unauthorized_dm = if self.cfg.decline_unauthorized { "decline" } else { "ignore" },
            "lucina telegram skipped unauthorized sender"
        );
        if self.cfg.decline_unauthorized && m.is_private() && !m.from_id.is_empty() {
            let now = Instant::now();
            let decline = self
                .declines
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(&m.from_id, now);
            if decline {
                let text = format!(
                    "Sorry, {} is a private assistant and can't chat with you.",
                    self.persona()
                );
                if let Err(e) = self
                    .api
                    .send_message(&m.chat_id, &text, &SendOptions::default())
                    .await
                {
                    tracing::warn!(error = %e, "telegram decline reply failed");
                }
            }
        }
        true
    }

    /// The owner timeline owner (`LUCINA_OPERATOR_CHAT_ID`, else the first
    /// positive allowed chat id): Node `timelineOwnerTelegramUserId`.
    fn timeline_owner(&self) -> Option<&str> {
        self.st
            .cfg
            .owner_chat_id
            .as_deref()
            .filter(|s| !s.is_empty())
    }

    /// Node `isTelegramOwnerUpdate`.
    fn is_owner_update(&self, m: &Msg) -> bool {
        !m.from_is_bot && self.timeline_owner().is_some_and(|o| o == m.from_id)
    }

    fn is_operator_chat(&self, m: &Msg) -> bool {
        self.cfg
            .operator_chat_id
            .as_deref()
            .is_some_and(|op| !op.is_empty() && op == m.chat_id)
    }

    pub fn identity(&self, m: &Msg) -> Identity {
        let canonical = self.st.cfg.owner_peer_id.as_str();
        let owner = self.timeline_owner();
        let handle = if m.username.is_empty() {
            String::new()
        } else {
            format!("@{}", m.username)
        };
        let operator = self.is_operator_chat(m);
        let (peer, label) = if operator {
            let label = ["Giuseppe (operator)", handle.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            (
                canonical_peer(&self.cfg.operator_peer_id, canonical, owner),
                label,
            )
        } else {
            let name = [m.first_name.as_str(), m.last_name.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let label = [name.as_str(), handle.as_str(), m.from_id.as_str()]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            (
                canonical_peer(&safe_id("telegram_user", &m.from_id), canonical, owner),
                label,
            )
        };
        let chat = if m.chat_id.is_empty() {
            &m.from_id
        } else {
            &m.chat_id
        };
        Identity {
            peer,
            session: safe_id("telegram_chat", chat),
            label,
            operator,
        }
    }

    fn surface(&self, m: &Msg) -> TurnSurface {
        let facts = TurnFacts {
            channel: ChannelKind::Telegram,
            telegram: Some(TelegramFacts {
                chat_id: (!m.chat_id.is_empty()).then(|| m.chat_id.clone()),
                chat_kind: TelegramChatKind::parse(&m.chat_type),
                from_user_id: (!m.from_id.is_empty()).then(|| m.from_id.clone()),
            }),
            ..TurnFacts::default()
        };
        TurnSurface::classify(&facts, &crate::surface::owner_ids(&self.st.cfg))
    }

    /// The owner timeline for this message's turn (Node's middleware rules).
    fn timeline_for(&self, m: &Msg) -> (Arc<TimelineStore>, Option<Meta>) {
        if !self.is_owner_update(m) {
            return (self.off_timeline.clone(), None);
        }
        let meta = if m.is_private() {
            Meta {
                is_dm: Some(true),
                ..Meta::default()
            }
        } else {
            Meta {
                is_dm: Some(false),
                chat_title: Some(m.chat_title.clone().unwrap_or_default()),
                ..Meta::default()
            }
        };
        (self.st.timeline.clone(), Some(meta))
    }

    fn command_timeline(&self, m: &Msg) -> TimelineTurn {
        let (store, meta) = self.timeline_for(m);
        TimelineTurn::new(store, "telegram", None).with_meta(meta)
    }

    /// Node `replyText` outside a turn: send, then the recent turn and the
    /// owner timeline (the user's item first).
    async fn reply(
        &self,
        m: &Msg,
        tl: &TimelineTurn,
        user: (&str, Vec<Attachment>),
        text: &str,
        preview_off: bool,
    ) {
        match format::reply_text(&self.api, &m.chat_id, text, preview_off).await {
            Ok(sent) => {
                self.recent
                    .append(&m.chat_id, "assistant", &sent, Utc::now());
                tl.record_user(user.0, user.1);
                tl.record_lucina(&sent, Vec::new(), None);
            }
            Err(e) => tracing::warn!(chat_id = %m.chat_id, error = %e, "telegram reply failed"),
        }
    }

    /// Node `rememberTelegramChat` (`data/telegram-chats.json`).
    fn remember_chat(&self, m: &Msg) {
        if m.chat_id.is_empty() {
            return;
        }
        let path = &self.cfg.registry_path;
        let mut registry: serde_json::Map<String, Value> = match std::fs::read(path) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "telegram chat registry read failed");
                Default::default()
            }),
            Err(_) => Default::default(),
        };
        let username = if m.username.is_empty() {
            String::new()
        } else {
            format!("@{}", m.username).to_lowercase()
        };
        let nonempty = |s: &str| (!s.is_empty()).then(|| s.to_owned());
        let entry = json!({
            "chatId": m.chat_id,
            "chatType": m.chat_type,
            "username": nonempty(&username),
            "firstName": nonempty(&m.first_name),
            "lastName": nonempty(&m.last_name),
            "updatedAt": lucina_store::timeutil::now_iso(),
        });
        registry.insert(m.chat_id.clone(), entry.clone());
        if !username.is_empty() {
            registry.insert(username, entry);
        }
        let bytes = match serde_json::to_string_pretty(&registry) {
            Ok(s) => format!("{s}\n"),
            Err(_) => return,
        };
        if let Err(e) = lucina_store::atomic::write_atomic(path, bytes.as_bytes()) {
            tracing::warn!(error = %e, "telegram chat registry write failed");
        }
    }

    /// Typing every 4 s until the guard drops.
    fn typing(&self, chat_id: &str) -> TypingGuard {
        let api = self.api.clone();
        let chat = chat_id.to_owned();
        TypingGuard(tokio::spawn(async move {
            loop {
                let _ = api.send_chat_action(&chat, "typing").await;
                tokio::time::sleep(TYPING_EVERY).await;
            }
        }))
    }

    /// After each lease acquisition: Node's buffer is read again (Node wrote
    /// it while it held the bot), and the secretary sessions it lacks come in.
    fn reload_after_handover(&self) {
        use std::sync::atomic::Ordering;
        let epoch = self
            .health
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .lease_acquisitions;
        if self.loaded_for.swap(epoch, Ordering::SeqCst) == epoch {
            return;
        }
        self.recent.reload();
        let own = self.st.cfg.data_dir.join("lucina-rs/recent-chat.json");
        let merged = self.recent.seed_from(&own, "secretary_");
        if merged > 0 {
            tracing::info!(
                sessions = merged,
                "secretary recent turns merged into recent-telegram-chat.json"
            );
        }
    }

    /// One update, as Node's middleware chain and handlers.
    pub async fn handle(self: Arc<Self>, update: Value) {
        self.reload_after_handover();
        let Some(m) = Msg::parse(&update) else {
            tracing::debug!(
                update_id = update["update_id"].as_u64(),
                "telegram update without a message ignored"
            );
            return;
        };
        if self.otp(&m).await {
            return;
        }
        if let Some(text) = m.text().map(str::to_owned) {
            self.on_text(&m, &text).await;
        } else if let Some(kind) = AUDIO_KINDS.into_iter().find(|k| m.raw.get(*k).is_some()) {
            self.on_audio(&m, kind).await;
        } else if m.raw.get("document").is_some() {
            self.on_file(&m, false).await;
        } else if m.raw["photo"].as_array().is_some_and(|p| !p.is_empty()) {
            self.on_file(&m, true).await;
        }
    }

    /// Node `privateOtpMiddleware`: `true` when the message was an OTP request.
    async fn otp(&self, m: &Msg) -> bool {
        let Some(action) = m.text().and_then(lucina_phone_core::otp_intent) else {
            return false;
        };
        if !m.is_private() {
            return true;
        }
        let owner = self.timeline_owner().unwrap_or("");
        let asker = lucina_phone_core::reply::Asker {
            surface: "telegram",
            owner_id: owner,
            user_id: &m.from_id,
            chat_id: &m.chat_id,
            chat_type: &m.chat_type,
            is_bot: m.from_is_bot,
            ..Default::default()
        };
        let r = crate::otp::respond(&self.st.cfg.otp, &asker, action).await;
        let opts = SendOptions {
            disable_web_page_preview: true,
            protect_content: true,
            ..SendOptions::default()
        };
        match self.api.send_message(&m.chat_id, &r.reply, &opts).await {
            Ok(sent) => {
                if let (Some(at), Some(id)) = (r.expires_at, sent["message_id"].as_i64()) {
                    let api = self.api.clone();
                    let chat = m.chat_id.clone();
                    let wait = at.saturating_sub(Utc::now().timestamp_millis().max(0) as u64);
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(wait)).await;
                        let _ = api.delete_message(&chat, id).await;
                    });
                }
            }
            Err(e) => tracing::warn!(error = %e, "telegram otp reply failed"),
        }
        true
    }

    /// A bot command addressed to us (`/name` or `/name@<our username>`).
    fn command<'t>(&self, text: &'t str) -> Option<&'t str> {
        let first = text.split_whitespace().next()?.strip_prefix('/')?;
        let (name, at) = first
            .split_once('@')
            .map_or((first, None), |(n, a)| (n, Some(a)));
        let ours = self
            .health
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .bot_username
            .clone();
        match (at, ours) {
            (Some(a), Some(u)) if !a.eq_ignore_ascii_case(&u) => None,
            _ => Some(name),
        }
    }

    async fn on_text(&self, m: &Msg, raw: &str) {
        let command = self.command(raw).map(str::to_owned);
        match command.as_deref() {
            Some("start") => {
                if self.skip_unauthorized(m).await {
                    return;
                }
                let tl = self.command_timeline(m);
                let text = format!("{} is online.", self.persona());
                self.reply(m, &tl, (raw, Vec::new()), &text, false).await;
                return;
            }
            Some("cancel_pdf") => {
                if self.skip_unauthorized(m).await {
                    return;
                }
                let tl = self.command_timeline(m);
                self.reply(
                    m,
                    &tl,
                    (raw, Vec::new()),
                    "No PDF analysis is running.",
                    false,
                )
                .await;
                return;
            }
            _ => {}
        }
        self.text_turn(m, raw, "text", None).await;
    }

    /// Node `handleTelegramTextMessage`. `voice`: the timeline text and
    /// attachment of a transcribed voice message.
    async fn text_turn(&self, m: &Msg, user_text: &str, source: &str, voice: Option<String>) {
        if self.skip_unauthorized(m).await {
            return;
        }
        let user_text = user_text.trim();
        if user_text.is_empty() {
            return;
        }
        if let Some(full) = status::is_status_command(user_text) {
            let (lease_held, username) = {
                let h = self.health.lock().unwrap_or_else(|e| e.into_inner());
                (h.lease_held, h.bot_username.clone().unwrap_or_default())
            };
            let report = status::build(
                &status::Facts {
                    st: &self.st,
                    bot_username: &username,
                    bot_key: &self.api.bot_key(),
                    lease_held,
                    decline: self.cfg.decline_unauthorized,
                },
                full,
            );
            let tl = self.command_timeline(m);
            self.reply(m, &tl, (user_text, Vec::new()), &report, true)
                .await;
            return;
        }
        tracing::info!(
            update_id = m.update_id,
            chat_id = %m.chat_id,
            from_id = %m.from_id,
            text_preview = %user_text.chars().take(80).collect::<String>(),
            "lucina telegram {source} received"
        );
        self.remember_chat(m);
        let (timeline_text, attachments) = match voice {
            Some(t) => (
                Some(t),
                vec![Attachment {
                    kind: "voice".into(),
                    ..Attachment::default()
                }],
            ),
            None => (None, Vec::new()),
        };
        self.run_turn(
            m,
            user_text.to_owned(),
            timeline_text,
            attachments,
            false,
            String::new(),
        )
        .await;
    }

    fn job_sink(&self, chat_id: &str) -> ReplySink {
        let api = self.api.clone();
        let chat = chat_id.to_owned();
        Arc::new(move |text: String| {
            let api = api.clone();
            let chat = chat.clone();
            Box::pin(async move {
                if let Err(e) = format::reply_text(&api, &chat, &text, true).await {
                    tracing::warn!(error = %e, "telegram job result not delivered");
                }
            })
        })
    }

    /// One turn through the Rust pipeline, its reply sent to the chat.
    async fn run_turn(
        &self,
        m: &Msg,
        message: String,
        timeline_text: Option<String>,
        attachments: Vec<Attachment>,
        share: bool,
        extra_system: String,
    ) {
        let id = self.identity(m);
        let surface = self.surface(m);
        let (timeline, timeline_meta) = self.timeline_for(m);
        let origin = ChannelOrigin {
            planner_platform: "telegram",
            platform_label: "Telegram",
            user_label: id.label.clone(),
            extra_system,
            peer_id: id.peer.clone(),
            recent: self.recent.clone(),
            recent_key: m.chat_id.clone(),
            timeline,
            timeline_meta,
            timeline_text,
            user_attachments: attachments,
            reminder_chat_id: m.chat_id.clone(),
            reminder_source: "telegram",
            reminders_refused: None,
            owner: surface.is_owner(),
            share,
            on_job_done: Some(self.job_sink(&m.chat_id)),
            channel_actions: None,
        };
        let typing = self.typing(&m.chat_id);
        let started = Instant::now();
        let out = crate::chat::channel_turn(
            self.st.clone(),
            message,
            id.session.clone(),
            "telegram".into(),
            surface,
            origin,
        )
        .await;
        drop(typing);
        let reply = match &out {
            Ok(v) => v["reply"].as_str().unwrap_or("").to_owned(),
            Err(e) => {
                tracing::error!(error = %e.message, "telegram turn failed");
                format!(
                    "{}: I misread that turn and could not recover it. Please send the message once more.",
                    self.persona()
                )
            }
        };
        let reply = if reply.trim().is_empty() {
            "I could not generate a reply.".to_owned()
        } else {
            reply
        };
        if let Err(e) = format::reply_text(&self.api, &m.chat_id, &reply, true).await {
            tracing::warn!(chat_id = %m.chat_id, error = %e, "telegram reply failed");
        }
        tracing::info!(
            update_id = m.update_id,
            chat_id = %m.chat_id,
            action = out.as_ref().ok().and_then(|v| v["action"].as_str()).unwrap_or("error"),
            total_ms = started.elapsed().as_millis() as u64,
            reply_chars = reply.chars().count(),
            "lucina telegram turn completed"
        );
    }

    /// Download a Telegram file into `dir`, at most `max` bytes.
    async fn download(
        &self,
        file_id: &str,
        max: usize,
        dir: &Path,
        name: &str,
    ) -> anyhow::Result<PathBuf> {
        let file = self
            .api
            .get_file(file_id)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        if let Some(size) = file["file_size"].as_u64() {
            if max > 0 && size > max as u64 {
                anyhow::bail!("file too large ({size} bytes > {max} bytes limit)");
            }
        }
        let path = file["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Telegram returned no file path"))?;
        let limit = if max > 0 { max } else { MAX_FILE_BYTES };
        let bytes = self
            .api
            .download_file(path, limit)
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        let out = dir.join(crate::share::sanitize(name));
        tokio::fs::write(&out, &bytes).await?;
        Ok(out)
    }

    fn work_dir(&self) -> std::io::Result<tempfile::TempDir> {
        let base = self.st.cfg.share.dir.join("telegram");
        std::fs::create_dir_all(&base)
            .and_then(|_| tempfile::Builder::new().prefix("tg-").tempdir_in(&base))
            .or_else(|_| tempfile::Builder::new().prefix("lucina-tg-").tempdir())
    }

    /// Node `handleTelegramAudioMessage`.
    async fn on_audio(&self, m: &Msg, kind: &str) {
        if self.skip_unauthorized(m).await {
            return;
        }
        self.remember_chat(m);
        let a = &m.raw[kind];
        let Some(file_id) = a["file_id"].as_str() else {
            return;
        };
        let caption: String = m.caption().chars().take(400).collect();
        let tl = self.command_timeline(m);
        let voice = vec![Attachment {
            kind: "voice".into(),
            ..Attachment::default()
        }];
        let persona = self.persona().to_owned();
        let duration = a["duration"].as_u64().unwrap_or(0);
        let max_duration = self.cfg.max_audio_seconds;
        if max_duration > 0 && duration > max_duration {
            let text = format!(
                "Sono {persona}: questo audio è troppo lungo ({duration}s). Limite attuale: {max_duration}s."
            );
            self.reply(m, &tl, (&caption, voice), &text, false).await;
            return;
        }
        let mime = a["mime_type"].as_str().unwrap_or(match kind {
            "voice" => "audio/ogg",
            "audio" => "audio/mpeg",
            _ => "video/mp4",
        });
        tracing::info!(
            update_id = m.update_id,
            chat_id = %m.chat_id,
            from_id = %m.from_id,
            duration,
            declared_mime_type = mime,
            file_size = a["file_size"].as_u64(),
            "lucina telegram {kind} received"
        );
        let typing = self.typing(&m.chat_id);
        let ext = mime.split('/').nth(1).unwrap_or("bin");
        let unique = a["file_unique_id"].as_str().unwrap_or(file_id);
        let name = a["file_name"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{kind}-{unique}.{ext}"));
        let max = usize::try_from(self.cfg.max_audio_bytes).unwrap_or(usize::MAX);
        let transcript = async {
            let dir = self.work_dir()?;
            let path = self.download(file_id, max, dir.path(), &name).await?;
            let text = crate::share::transcribe_local(&self.st, &path).await?;
            drop(dir);
            anyhow::Ok(text.trim().to_owned())
        }
        .await;
        drop(typing);
        match transcript {
            Ok(t) if !t.is_empty() => {
                tracing::info!(
                    update_id = m.update_id,
                    provider = "local-whisper",
                    "lucina telegram {kind} transcription completed"
                );
                let user_text = [
                    if caption.is_empty() {
                        String::new()
                    } else {
                        format!("Caption: {caption}")
                    },
                    format!("[transcript:{kind}] {t}"),
                ]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
                let timeline = [caption.as_str(), t.as_str()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                self.text_turn(m, &user_text, kind, Some(timeline)).await;
            }
            other => {
                if let Err(e) = other {
                    tracing::error!(update_id = m.update_id, chat_id = %m.chat_id, error = %e, "telegram {kind} handling failed");
                }
                let text = format!(
                    "Sono {persona}: ho ricevuto il messaggio audio, ma non sono riuscito a trascriverlo adesso."
                );
                self.reply(m, &tl, (&caption, voice), &text, false).await;
            }
        }
    }

    /// Node's `document` and `photo` handlers, through the share pipeline.
    async fn on_file(&self, m: &Msg, photo: bool) {
        if self.skip_unauthorized(m).await {
            return;
        }
        self.remember_chat(m);
        let caption = m.caption().to_owned();
        let (file_id, name, mime, attachment) = if photo {
            let Some(largest) = m.raw["photo"].as_array().and_then(|p| p.last()) else {
                return;
            };
            let id = largest["file_id"].as_str().unwrap_or("").to_owned();
            let unique = largest["file_unique_id"].as_str().unwrap_or(&id).to_owned();
            (
                id,
                format!("photo-{unique}.jpg"),
                "image/jpeg".to_owned(),
                Attachment {
                    kind: "image".into(),
                    ..Attachment::default()
                },
            )
        } else {
            let d = &m.raw["document"];
            let name = d["file_name"]
                .as_str()
                .unwrap_or("telegram-document")
                .to_owned();
            (
                d["file_id"].as_str().unwrap_or("").to_owned(),
                name.clone(),
                d["mime_type"].as_str().unwrap_or("").to_owned(),
                Attachment {
                    kind: "file".into(),
                    name: Some(name),
                    ..Attachment::default()
                },
            )
        };
        if file_id.is_empty() {
            return;
        }
        tracing::info!(
            update_id = m.update_id,
            chat_id = %m.chat_id,
            file_name = %name,
            mime_type = %mime,
            caption_preview = %caption.chars().take(80).collect::<String>(),
            "lucina telegram {} received",
            if photo { "photo" } else { "document" }
        );
        let typing = self.typing(&m.chat_id);
        let read = async {
            let dir = self.work_dir()?;
            let path = self
                .download(&file_id, MAX_FILE_BYTES, dir.path(), &name)
                .await?;
            let (section, _) =
                crate::share::read_local_file(&self.st, dir.path(), &path, &name, &mime).await;
            drop(dir);
            anyhow::Ok(section)
        }
        .await;
        drop(typing);
        let persona = self.persona().to_owned();
        match read {
            Ok(section) => {
                let message = crate::share::turn_message(&caption, &m.language, &section);
                let extra = crate::share::SHARE_SYSTEM.replace(
                    "owner share from the Secretary app",
                    "owner share on Telegram",
                );
                self.run_turn(m, message, Some(caption), vec![attachment], true, extra)
                    .await;
            }
            Err(e) => {
                tracing::error!(update_id = m.update_id, error = %e, "telegram file handling failed");
                let text = if photo {
                    format!("Sono {persona}: ho ricevuto l’immagine, ma non sono riuscito ad analizzarla adesso.")
                } else {
                    format!(
                        "{persona}: non sono riuscito a gestire il documento: {}",
                        e.to_string().chars().take(1000).collect::<String>()
                    )
                };
                let tl = self.command_timeline(m);
                self.reply(m, &tl, (&caption, vec![attachment]), &text, false)
                    .await;
            }
        }
    }
}

struct TypingGuard(tokio::task::JoinHandle<()>);

impl Drop for TypingGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Start the poller (`LUCINA_TELEGRAM_ENABLED=true` with a Lucina bot token).
/// It stops, and releases the lease, when `shutdown` turns true.
pub fn spawn(
    st: &AppState,
    api: BotApi,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let tg = &st.cfg.telegram;
    let lock_dir = match &tg.lock_dir {
        Some(d) => d.clone(),
        None => lucina_telegram::lease::default_lock_dir()?,
    };
    let claim = lucina_telegram::claim::ClaimFile::for_bot(&st.cfg.data_dir, &api.bot_key());
    let cfg = PollerConfig::new("Hermes", lock_dir, &tg.instance, claim);
    let bot = Bot::new(st.clone(), api.clone());
    let handler: Handler = Arc::new(move |update: Value| {
        let bot = bot.clone();
        Box::pin(bot.handle(update))
    });
    let poller = Poller::new(api, cfg, handler, st.telegram_health.clone());
    st.monitor.register(
        "telegram",
        crate::delivery_monitor::telegram_probe(st.telegram_health.clone()),
    );
    let alive = st.supervisor.watch_flag("telegram poller");
    let health = st.telegram_health.clone();
    Ok(tokio::spawn(async move {
        poller.run(shutdown).await;
        // A refused token (401) stops the poller on purpose: that is a delivery
        // alert, not a reason for the watchdog to restart the process in a loop.
        let refused = health.lock().unwrap_or_else(|e| e.into_inner()).state == "unauthorized";
        if !refused {
            alive.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_peers_and_declines() {
        assert_eq!(safe_id("telegram_chat", "-100123"), "telegram_chat_-100123");
        assert_eq!(safe_id("telegram_user", ""), "telegram_user_unknown");
        assert_eq!(
            canonical_peer("giuseppe", "giuseppe", Some("42")),
            "giuseppe"
        );
        assert_eq!(
            canonical_peer("flareon_owner", "giuseppe", None),
            "giuseppe"
        );
        assert_eq!(
            canonical_peer("telegram_user_42", "giuseppe", Some("42")),
            "giuseppe"
        );
        assert_eq!(
            canonical_peer("telegram_user_77", "giuseppe", Some("42")),
            "telegram_user_77"
        );
        let mut d = Declines::default();
        let t0 = Instant::now();
        assert!(d.take("1", t0));
        assert!(!d.take("1", t0 + Duration::from_secs(3600)));
        assert!(d.take("1", t0 + DECLINE_REPEAT));
        for i in 0..DECLINE_KEEP + 10 {
            d.take(&format!("u{i}"), t0);
        }
        assert_eq!(d.at.len(), DECLINE_KEEP);
    }

    #[test]
    fn config_names_and_defaults() {
        let c = TelegramConfig::from_lookup(&|_: &str| None, Path::new("/d"));
        assert!(!c.enabled);
        assert_eq!(c.max_audio_bytes, 25 * 1024 * 1024);
        assert_eq!(c.max_audio_seconds, 900);
        assert_eq!(c.instance, "lucina-rs");
        assert_eq!(c.recent_path, PathBuf::from("/d/recent-telegram-chat.json"));
        assert_eq!(c.registry_path, PathBuf::from("/d/telegram-chats.json"));
        assert!(!c.decline_unauthorized);
        let c = TelegramConfig::from_lookup(
            &|k: &str| match k {
                "LUCINA_TELEGRAM_ENABLED" => Some("true".into()),
                "TELEGRAM_ALLOWED_CHAT_IDS" => Some("-5, 42".into()),
                "TELEGRAM_UNAUTHORIZED_DM" => Some("decline".into()),
                "TELEGRAM_MAX_AUDIO_DURATION_SECONDS" => Some("60".into()),
                _ => None,
            },
            Path::new("/d"),
        );
        assert!(c.enabled && c.decline_unauthorized);
        assert_eq!(c.allowed_chat_ids, ["-5", "42"]);
        assert_eq!(c.operator_chat_id.as_deref(), Some("-5"));
        assert_eq!(c.max_audio_seconds, 60);
    }
}
