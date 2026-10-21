//! Lucina's memory on Honcho (v3 REST API), continuing the Node Lucina's
//! memory exactly (`src/memory/honcho.js`):
//!
//! - workspace `HONCHO_WORKSPACE_ID` (default `hermes-peer1`),
//! - the owner peer `LUCINA_OWNER_USER_PEER_ID` (default `giuseppe`) and the
//!   assistant peer `HONCHO_AGENT_PEER_ID` (default `hermes`),
//! - sessions `secretary_<sessionId>` for the Secretary app (default
//!   `secretary_app`), the same ids Node writes.
//!
//! [`MemoryMode`]: `off` (no calls), `readonly` (context, representation and
//! search only: no get-or-create, no messages; the safe shadow default) and
//! `readwrite` (also `remember`).

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("honcho HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("honcho transport: {0}")]
    Transport(String),
    #[error("honcho response: {0}")]
    Decode(String),
    #[error("memory is read-only")]
    ReadOnly,
    #[error("memory is off")]
    Off,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryMode {
    Off,
    ReadOnly,
    ReadWrite,
}

impl MemoryMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "false" | "disabled" | "none" => Some(Self::Off),
            "readonly" | "read-only" | "ro" => Some(Self::ReadOnly),
            "readwrite" | "read-write" | "rw" | "on" | "true" => Some(Self::ReadWrite),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::ReadOnly => "readonly",
            Self::ReadWrite => "readwrite",
        }
    }
}

#[derive(Clone)]
pub struct HonchoConfig {
    pub base_url: String,
    pub api_key: String,
    pub workspace_id: String,
    pub agent_peer_id: String,
    pub owner_peer_id: String,
    pub use_vector_search: bool,
    pub context_tokens: u32,
    pub timeout: Duration,
}

impl std::fmt::Debug for HonchoConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HonchoConfig")
            .field("base_url", &self.base_url)
            .field("workspace_id", &self.workspace_id)
            .field("agent_peer_id", &self.agent_peer_id)
            .field("owner_peer_id", &self.owner_peer_id)
            .finish_non_exhaustive()
    }
}

impl Default for HonchoConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:8000".into(),
            api_key: "local-dev-key".into(),
            workspace_id: "hermes-peer1".into(),
            agent_peer_id: "hermes".into(),
            owner_peer_id: "giuseppe".into(),
            use_vector_search: false,
            context_tokens: 4000,
            timeout: Duration::from_secs(10),
        }
    }
}

impl HonchoConfig {
    /// Node's env names: `HONCHO_URL`, `HONCHO_API_KEY`,
    /// `HONCHO_WORKSPACE_ID`, `HONCHO_AGENT_PEER_ID`,
    /// `LUCINA_OWNER_USER_PEER_ID`, `HONCHO_USE_VECTOR_SEARCH`,
    /// `HONCHO_CONTEXT_TOKEN_LIMIT`, plus `HONCHO_TIMEOUT_MS`.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Self {
        let mut c = Self::default();
        if let Some(v) = get("HONCHO_URL") {
            c.base_url = v.trim().trim_end_matches('/').to_owned();
        }
        if let Some(v) = get("HONCHO_API_KEY").filter(|v| !v.trim().is_empty()) {
            c.api_key = v.trim().to_owned();
        }
        if let Some(v) = get("HONCHO_WORKSPACE_ID") {
            c.workspace_id = v.trim().to_owned();
        }
        if let Some(v) = get("HONCHO_AGENT_PEER_ID") {
            c.agent_peer_id = v.trim().to_owned();
        }
        if let Some(v) = get("LUCINA_OWNER_USER_PEER_ID").or_else(|| get("LUCINA_GMAIL_LEARN_USER_PEER_ID")) {
            c.owner_peer_id = v.trim().to_owned();
        }
        c.use_vector_search = get("HONCHO_USE_VECTOR_SEARCH").is_some_and(|v| v.trim() == "true");
        if let Some(n) = get("HONCHO_CONTEXT_TOKEN_LIMIT").and_then(|v| v.trim().parse().ok()) {
            c.context_tokens = n;
        }
        if let Some(ms) = get("HONCHO_TIMEOUT_MS").and_then(|v| v.trim().parse::<u64>().ok()) {
            c.timeout = Duration::from_millis(ms.max(500));
        }
        c
    }
}

// ---------------------------------------------------------------- wire types

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HonchoMessage {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub peer_id: Option<String>,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Summary {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionContext {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub messages: Vec<HonchoMessage>,
    #[serde(default)]
    pub summary: Option<Summary>,
    #[serde(default)]
    pub peer_representation: Option<Value>,
    #[serde(default)]
    pub peer_card: Option<Value>,
}

/// `GET /sessions/{id}/context` query.
#[derive(Debug, Clone, Default)]
pub struct ContextQuery {
    pub tokens: Option<u32>,
    pub summary: bool,
    pub peer_target: Option<String>,
    pub peer_perspective: Option<String>,
    pub search_query: Option<String>,
    pub search_top_k: Option<u32>,
    pub max_conclusions: Option<u32>,
    pub limit_to_session: Option<bool>,
}

/// A message to add: `peer.message(content, {metadata})`.
#[derive(Debug, Clone, Serialize)]
pub struct NewMessage {
    pub peer_id: String,
    pub content: String,
    pub metadata: Value,
}

// ---------------------------------------------------------------- client

/// Honcho v3 REST client for one workspace.
pub struct HonchoClient {
    cfg: HonchoConfig,
    http: reqwest::Client,
    ensured: tokio::sync::Mutex<HashSet<String>>,
}

impl std::fmt::Debug for HonchoClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HonchoClient").field("cfg", &self.cfg).finish()
    }
}

impl HonchoClient {
    pub fn new(cfg: HonchoConfig) -> Result<Self, MemoryError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("lucina-memory/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(3))
            .timeout(cfg.timeout)
            .build()
            .map_err(|e| MemoryError::Transport(e.to_string()))?;
        Ok(Self { cfg, http, ensured: tokio::sync::Mutex::new(HashSet::new()) })
    }

    pub fn config(&self) -> &HonchoConfig {
        &self.cfg
    }

    fn ws_url(&self, rest: &str) -> String {
        format!("{}/v3/workspaces/{}{rest}", self.cfg.base_url, self.cfg.workspace_id)
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<Value, MemoryError> {
        let resp = rb
            .bearer_auth(&self.cfg.api_key)
            .send()
            .await
            .map_err(|e| MemoryError::Transport(e.without_url().to_string()))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| MemoryError::Transport(e.without_url().to_string()))?;
        if !status.is_success() {
            return Err(MemoryError::Status { status: status.as_u16(), body: text.chars().take(200).collect() });
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| MemoryError::Decode(e.to_string()))
    }

    async fn post(&self, url: String, body: Value) -> Result<Value, MemoryError> {
        self.send(self.http.post(url).json(&body)).await
    }

    /// Get-or-create, memoized per client (like the SDK's `_ensureWorkspace`).
    async fn ensure(&self, key: String, url: String, body: Value) -> Result<(), MemoryError> {
        if self.ensured.lock().await.contains(&key) {
            return Ok(());
        }
        self.post(url, body).await?;
        self.ensured.lock().await.insert(key);
        Ok(())
    }

    pub async fn ensure_workspace(&self) -> Result<(), MemoryError> {
        let url = format!("{}/v3/workspaces", self.cfg.base_url);
        self.ensure("ws".into(), url, json!({"id": self.cfg.workspace_id})).await
    }

    pub async fn ensure_peer(&self, peer_id: &str) -> Result<(), MemoryError> {
        self.ensure_workspace().await?;
        self.ensure(format!("peer:{peer_id}"), self.ws_url("/peers"), json!({"id": peer_id})).await
    }

    pub async fn ensure_session(&self, session_id: &str) -> Result<(), MemoryError> {
        self.ensure_workspace().await?;
        self.ensure(format!("session:{session_id}"), self.ws_url("/sessions"), json!({"id": session_id})).await
    }

    /// `POST /sessions/{id}/peers` with `{peerId: {}}`.
    pub async fn add_peers(&self, session_id: &str, peers: &[&str]) -> Result<(), MemoryError> {
        let body: serde_json::Map<String, Value> = peers.iter().map(|p| ((*p).to_owned(), json!({}))).collect();
        self.post(self.ws_url(&format!("/sessions/{session_id}/peers")), Value::Object(body)).await?;
        Ok(())
    }

    /// `POST /sessions/{id}/messages` with `{messages: [{peer_id, content, metadata}]}`.
    pub async fn add_messages(&self, session_id: &str, messages: &[NewMessage]) -> Result<Vec<HonchoMessage>, MemoryError> {
        let v = self
            .post(self.ws_url(&format!("/sessions/{session_id}/messages")), json!({"messages": messages}))
            .await?;
        Ok(serde_json::from_value(v).unwrap_or_default())
    }

    /// `GET /sessions/{id}/context` (read-only).
    pub async fn session_context(&self, session_id: &str, q: &ContextQuery) -> Result<SessionContext, MemoryError> {
        let mut query: Vec<(&str, String)> = vec![("summary", q.summary.to_string())];
        if let Some(t) = q.tokens {
            query.push(("tokens", t.to_string()));
        }
        if let Some(p) = &q.peer_target {
            query.push(("peer_target", p.clone()));
        }
        if let Some(p) = &q.peer_perspective {
            query.push(("peer_perspective", p.clone()));
        }
        if let Some(s) = q.search_query.as_ref().filter(|s| !s.trim().is_empty()) {
            query.push(("search_query", s.chars().take(1000).collect()));
        }
        if let Some(k) = q.search_top_k {
            query.push(("search_top_k", k.to_string()));
        }
        if let Some(m) = q.max_conclusions {
            query.push(("max_conclusions", m.to_string()));
        }
        if let Some(l) = q.limit_to_session {
            query.push(("limit_to_session", l.to_string()));
        }
        let v = self
            .send(self.http.get(self.ws_url(&format!("/sessions/{session_id}/context"))).query(&query))
            .await?;
        serde_json::from_value(v).map_err(|e| MemoryError::Decode(e.to_string()))
    }

    /// `POST /peers/{id}/representation` (read-only).
    pub async fn representation(&self, peer_id: &str, session_id: Option<&str>, target: Option<&str>, search_query: Option<&str>) -> Result<String, MemoryError> {
        let mut body = serde_json::Map::new();
        if let Some(s) = session_id {
            body.insert("session_id".into(), json!(s));
        }
        if let Some(t) = target {
            body.insert("target".into(), json!(t));
        }
        if let Some(q) = search_query {
            body.insert("search_query".into(), json!(q));
            body.insert("search_top_k".into(), json!(5));
        }
        let v = self.post(self.ws_url(&format!("/peers/{peer_id}/representation")), Value::Object(body)).await?;
        Ok(match v.get("representation") {
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        })
    }

    async fn search_at(&self, url: String, query: &str, limit: u32) -> Result<Vec<HonchoMessage>, MemoryError> {
        let v = self.post(url, json!({"query": query, "limit": limit.clamp(1, 100)})).await?;
        Ok(serde_json::from_value(v).unwrap_or_default())
    }

    /// Semantic message search in the whole workspace (read-only).
    pub async fn search_workspace(&self, query: &str, limit: u32) -> Result<Vec<HonchoMessage>, MemoryError> {
        self.search_at(self.ws_url("/search"), query, limit).await
    }

    pub async fn search_session(&self, session_id: &str, query: &str, limit: u32) -> Result<Vec<HonchoMessage>, MemoryError> {
        self.search_at(self.ws_url(&format!("/sessions/{session_id}/search")), query, limit).await
    }

    pub async fn search_peer(&self, peer_id: &str, query: &str, limit: u32) -> Result<Vec<HonchoMessage>, MemoryError> {
        self.search_at(self.ws_url(&format!("/peers/{peer_id}/search")), query, limit).await
    }
}

// ---------------------------------------------------------------- memory

fn clip(text: &str, max: usize) -> String {
    let v = text.trim();
    if v.chars().count() <= max {
        return v.to_owned();
    }
    let cut: String = v.chars().take(max).collect();
    format!("{cut}\n[clipped to budget]")
}

fn render_jsonish(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_owned(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// Render a session context into Lucina's labeled memory blocks (the
/// HONCHO_RECALL layout of the Node policy path), at most 6000 characters.
pub fn render_context(ctx: &SessionContext, query: Option<&str>) -> String {
    let mut parts = Vec::new();
    if let Some(s) = ctx.summary.as_ref().filter(|s| !s.content.trim().is_empty() && s.content.trim() != "null") {
        parts.push(clip(
            &format!(
                "Honcho session summary (derived {}; treat as background, not as current state):\n{}",
                s.created_at.as_deref().unwrap_or("date unknown"),
                s.content.trim()
            ),
            1500,
        ));
    }
    let rep = ctx.peer_representation.as_ref().map(render_jsonish).unwrap_or_default();
    if !rep.is_empty() && rep != "null" {
        let label = match query {
            Some(q) => format!(
                "Honcho query-matched memory (semantic search conditioned on the CURRENT message: \"{}\"; derived background — verify freshness against dates and turn metadata):",
                q.chars().take(120).collect::<String>()
            ),
            None => "Honcho peer observations:".into(),
        };
        parts.push(clip(&format!("{label}\n{rep}"), 2500));
    }
    let card = ctx.peer_card.as_ref().map(render_jsonish).unwrap_or_default();
    if !card.is_empty() && card != "null" {
        parts.push(clip(&format!("Honcho peer card:\n{card}"), 800));
    }
    let messages: Vec<String> = ctx
        .messages
        .iter()
        .filter(|m| !m.content.trim().is_empty())
        .map(|m| format!("{}: {}", m.peer_id.as_deref().unwrap_or("unknown"), m.content.trim()))
        .collect();
    if !messages.is_empty() {
        parts.push(clip(
            &format!("Honcho recent session messages (session tail):\n{}", messages.join("\n")),
            1200,
        ));
    }
    let joined = parts.join("\n\n");
    joined.chars().take(6000).collect()
}

/// Lucina's memory facade.
pub struct Memory {
    mode: MemoryMode,
    client: Option<HonchoClient>,
}

impl std::fmt::Debug for Memory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Memory").field("mode", &self.mode).finish()
    }
}

impl Memory {
    pub fn new(mode: MemoryMode, cfg: HonchoConfig) -> Result<Self, MemoryError> {
        let client = match mode {
            MemoryMode::Off => None,
            _ => Some(HonchoClient::new(cfg)?),
        };
        Ok(Self { mode, client })
    }

    pub fn off() -> Self {
        Self { mode: MemoryMode::Off, client: None }
    }

    pub fn mode(&self) -> MemoryMode {
        self.mode
    }

    pub fn client(&self) -> Option<&HonchoClient> {
        self.client.as_ref()
    }

    /// Context for a turn: one read-only `session.context` call (summary,
    /// representation conditioned on the prompt when vector search is on,
    /// session tail). Never fails: errors become an empty context.
    pub async fn build_context(&self, user_peer_id: &str, session_id: &str, prompt: &str) -> String {
        let Some(c) = &self.client else { return String::new() };
        let cfg = c.config();
        let search = cfg.use_vector_search.then_some(prompt);
        let q = ContextQuery {
            tokens: Some(cfg.context_tokens),
            summary: true,
            peer_target: Some(user_peer_id.to_owned()),
            peer_perspective: Some(cfg.agent_peer_id.clone()),
            search_query: search.map(str::to_owned),
            search_top_k: search.map(|_| 5),
            max_conclusions: search.map(|_| 20),
            limit_to_session: None,
        };
        match c.session_context(session_id, &q).await {
            Ok(ctx) => render_context(&ctx, search),
            Err(e) => {
                // 404: a session Honcho has never seen (no messages yet).
                tracing::warn!(error = %e, "honcho context unavailable");
                String::new()
            }
        }
    }

    /// Store one exchange (`readwrite` only), as Node's `rememberWithClient`:
    /// get-or-create peers and session, add both peers, add two messages
    /// with `metadata.role`.
    pub async fn remember(&self, user_peer_id: &str, session_id: &str, user_text: &str, assistant_text: &str) -> Result<(), MemoryError> {
        let c = match (self.mode, &self.client) {
            (MemoryMode::ReadWrite, Some(c)) => c,
            (MemoryMode::Off, _) | (_, None) => return Err(MemoryError::Off),
            (MemoryMode::ReadOnly, _) => return Err(MemoryError::ReadOnly),
        };
        let agent = c.config().agent_peer_id.clone();
        c.ensure_peer(user_peer_id).await?;
        c.ensure_peer(&agent).await?;
        c.ensure_session(session_id).await?;
        c.add_peers(session_id, &[user_peer_id, &agent]).await?;
        c.add_messages(
            session_id,
            &[
                NewMessage { peer_id: user_peer_id.into(), content: user_text.into(), metadata: json!({"role": "user"}) },
                NewMessage { peer_id: agent, content: assistant_text.into(), metadata: json!({"role": "assistant"}) },
            ],
        )
        .await?;
        Ok(())
    }

    /// Semantic search over the owner's messages (read-only, any mode but off).
    pub async fn search(&self, query: &str, limit: u32) -> Result<Vec<HonchoMessage>, MemoryError> {
        let c = self.client.as_ref().ok_or(MemoryError::Off)?;
        c.search_peer(&c.config().owner_peer_id.clone(), query, limit).await
    }
}

/// Secretary chat session id: `secretary_<id>`, no double prefix
/// (`honchoSessionId` in `src/secretary-api.js`).
pub fn secretary_session_id(requested: &str, default_session: &str) -> String {
    let raw = if requested.is_empty() {
        if default_session.is_empty() { "app" } else { default_session }
    } else {
        requested
    };
    if raw.starts_with("secretary_") {
        raw.to_owned()
    } else {
        format!("secretary_{raw}")
    }
}
