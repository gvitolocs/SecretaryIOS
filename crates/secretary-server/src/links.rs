//! The Links hub (`/v1/links`): every connection the owner has, with its
//! real state, and the connect/disconnect routes.
//!
//! Groups: calendars (Google Calendar via OAuth, the iPhone calendar via the
//! app's EventKit snapshot, ICS subscriptions), chat channels (Lucina's),
//! AI clients (the Lucina MCP's tokens and grants), Google services (Gmail,
//! Drive: same OAuth flow, extra scopes) and custom links (bookmarks and
//! remote MCP server URLs that are stored, never called).
//!
//! State lives in `$SECRETARY_DATA_DIR/links.json` (atomic 0600 writes).
//! Secrets in it (the Google refresh token, ICS URLs, MCP URLs) are sealed
//! with [`crate::secrets`] and never returned to clients; clients see only a
//! host hint.
//!
//! Google OAuth: `POST /v1/links/google/connect` (authenticated) creates a
//! single-use `state` and a PKCE verifier (memory only, 10 minutes) and
//! returns the consent URL. Google redirects the browser to
//! `GET /v1/links/google/callback` (public: the `SameSite=Strict` cookie is
//! not sent on that cross-site redirect; the unguessable `state` is the
//! binding), which exchanges the code and redirects back to the app.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono_tz::Tz;
use secretary_calendar::Item;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::auth::Identity;
use crate::error::{ApiError, JsonBody};
use crate::google::{self, GError, GoogleClient};
use crate::secrets::Sealer;
use crate::AppState;

pub const LINKS_FILE: &str = "links.json";
pub const CALENDAR_FILE: &str = "calendar.json";
const PENDING_TTL: Duration = Duration::from_secs(600);
const MAX_ICS: usize = 20;
const MAX_CUSTOM: usize = 200;

// -- stored state ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConnStatus {
    #[default]
    Ok,
    /// The refresh token stopped working: the owner must reconnect.
    Reconnect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleConn {
    /// The primary calendar id (the account's email), when known.
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Sealed (`google-refresh`).
    pub refresh_token: String,
    pub connected_at: String,
    #[serde(default)]
    pub status: ConnStatus,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_sync_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IcsSub {
    pub id: String,
    pub label: String,
    /// Sealed (`ics-url:<id>`).
    pub url: String,
    /// The URL's host, for display.
    pub host: String,
    pub added_at: String,
    #[serde(default)]
    pub last_sync_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub event_count: usize,
    #[serde(default)]
    pub etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    /// `X-WR-CALNAME` of the feed.
    #[serde(default)]
    pub feed_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    #[default]
    Bookmark,
    /// A remote MCP server for Lucina: stored (sealed), never called.
    Mcp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomLink {
    pub id: String,
    #[serde(default)]
    pub kind: LinkKind,
    pub label: String,
    /// Bookmarks: the URL. MCP servers: sealed (`mcp-url:<id>`).
    pub url: String,
    pub host: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinksState {
    #[serde(default)]
    pub google: Option<GoogleConn>,
    #[serde(default)]
    pub ics: Vec<IcsSub>,
    #[serde(default)]
    pub custom: Vec<CustomLink>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleCache {
    #[serde(default)]
    pub sync_token: Option<String>,
    #[serde(default)]
    pub calendar_tz: Option<String>,
    #[serde(default)]
    pub items: BTreeMap<String, Item>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IphoneCache {
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub reported_at: Option<String>,
    #[serde(default)]
    pub items: Vec<Item>,
}

/// An event proposed by a client that may not write (the Lucina MCP),
/// waiting for the owner's confirmation in the app.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub id: String,
    /// What would be sent to `POST /v1/calendar/events`.
    pub event: Value,
    /// Who proposed it (`mcp:claude-code`, …).
    pub from: String,
    pub created_at: String,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarCache {
    #[serde(default)]
    pub google: GoogleCache,
    #[serde(default)]
    pub ics: BTreeMap<String, Vec<Item>>,
    #[serde(default)]
    pub iphone: IphoneCache,
    #[serde(default)]
    pub proposals: Vec<Proposal>,
}

pub struct Data {
    pub links: LinksState,
    pub cache: CalendarCache,
}

struct PendingOAuth {
    state_hash: [u8; 32],
    verifier: String,
    client: String,
    created: Instant,
}

/// The AI clients' state as `secretary-mcp` reports it.
#[derive(Debug, Clone)]
pub struct McpStatusConfig {
    pub url: String,
    pub token: String,
}

pub struct Hub {
    dir: PathBuf,
    pub sealer: Sealer,
    pub data: tokio::sync::Mutex<Data>,
    pub google: Option<GoogleClient>,
    pub access: tokio::sync::Mutex<Option<(String, Instant)>>,
    pending: Mutex<Vec<PendingOAuth>>,
    pub sync_lock: tokio::sync::Mutex<()>,
    pub http: reqwest::Client,
    pub tz: Tz,
    pub ics_allow_local: bool,
    mcp: Option<McpStatusConfig>,
    mcp_cache: Mutex<Option<(Instant, Option<Value>)>>,
    channel_cache: Mutex<Option<(Instant, HashMap<String, i64>)>>,
}

fn load<T: serde::de::DeserializeOwned + Default>(path: &Path) -> anyhow::Result<T> {
    match std::fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).map_err(|e| anyhow::anyhow!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(anyhow::anyhow!("{}: {e}", path.display())),
    }
}

pub fn now_iso() -> String {
    secretary_calendar::model::iso(chrono::Utc::now())
}

pub fn random_id(prefix: &str) -> String {
    let mut b = [0u8; 12];
    let _ = getrandom::fill(&mut b);
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(b))
}

fn random_secret() -> String {
    let mut b = [0u8; 32];
    let _ = getrandom::fill(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

fn sha(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

impl Hub {
    pub fn open(cfg: &crate::Config) -> anyhow::Result<Self> {
        let links: LinksState = load(&cfg.data_dir.join(LINKS_FILE))?;
        // The cache is rebuildable: a corrupt file is dropped, not fatal.
        let cache: CalendarCache = load(&cfg.data_dir.join(CALENDAR_FILE)).unwrap_or_else(|e| {
            tracing::warn!(error = %e, "calendar cache unreadable; starting empty");
            CalendarCache::default()
        });
        let google = match &cfg.google {
            Some(g) => Some(GoogleClient::new(g.clone())?),
            None => None,
        };
        Ok(Self {
            dir: cfg.data_dir.clone(),
            sealer: Sealer::new(&cfg.data_dir),
            data: tokio::sync::Mutex::new(Data { links, cache }),
            google,
            access: tokio::sync::Mutex::new(None),
            pending: Mutex::new(Vec::new()),
            sync_lock: tokio::sync::Mutex::new(()),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(20))
                .redirect(reqwest::redirect::Policy::limited(5))
                .user_agent(concat!(
                    "secretary-server/",
                    env!("CARGO_PKG_VERSION"),
                    " (calendar subscriber)"
                ))
                .build()?,
            tz: cfg.timezone,
            ics_allow_local: cfg.ics_allow_local,
            mcp: cfg.mcp_status.clone(),
            mcp_cache: Mutex::new(None),
            channel_cache: Mutex::new(None),
        })
    }

    pub fn save_links(&self, d: &Data) -> Result<(), ApiError> {
        crate::devices::save_json_atomic(&self.dir, LINKS_FILE, &d.links).map_err(|e| {
            tracing::error!(error = %e, "links.json write failed");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_error",
                "could not save the links",
            )
        })
    }

    pub fn save_cache(&self, d: &Data) {
        if let Err(e) = crate::devices::save_json_atomic(&self.dir, CALENDAR_FILE, &d.cache) {
            tracing::error!(error = %e, "calendar.json write failed");
        }
    }

    fn seal(&self, purpose: &str, plain: &str) -> Result<String, ApiError> {
        self.sealer.seal(purpose, plain).map_err(|e| {
            tracing::error!(error = %e, "sealing failed");
            ApiError::internal("could not encrypt the secret")
        })
    }

    /// A Google access token, refreshed when needed. A revoked refresh token
    /// flips the connection to `reconnect`.
    pub async fn access_token(&self, force: bool) -> Result<String, GError> {
        let client = self.google.as_ref().ok_or(GError::NotConnected)?;
        let mut cached = self.access.lock().await;
        if !force {
            if let Some((t, exp)) = cached.as_ref() {
                if *exp > Instant::now() {
                    return Ok(t.clone());
                }
            }
        }
        let sealed = {
            let d = self.data.lock().await;
            match &d.links.google {
                None => return Err(GError::NotConnected),
                Some(g) if g.status == ConnStatus::Reconnect => return Err(GError::Revoked),
                Some(g) => g.refresh_token.clone(),
            }
        };
        let refresh = self
            .sealer
            .open("google-refresh", &sealed)
            .map_err(|_| GError::Revoked)?;
        match client.refresh(&refresh).await {
            Ok(tok) => {
                let ttl = tok.expires_in.unwrap_or(3600).saturating_sub(60).max(30);
                *cached = Some((
                    tok.access_token.clone(),
                    Instant::now() + Duration::from_secs(ttl),
                ));
                if let Some(new) = tok.refresh_token.filter(|r| r != &refresh) {
                    if let Ok(s) = self.sealer.seal("google-refresh", &new) {
                        let mut d = self.data.lock().await;
                        if let Some(g) = d.links.google.as_mut() {
                            g.refresh_token = s;
                        }
                        let _ = self.save_links(&d);
                    }
                }
                Ok(tok.access_token)
            }
            Err(GError::Revoked) => {
                *cached = None;
                self.mark_reconnect("Google access was revoked or expired")
                    .await;
                Err(GError::Revoked)
            }
            Err(e) => Err(e),
        }
    }

    pub async fn mark_reconnect(&self, why: &str) {
        let mut d = self.data.lock().await;
        if let Some(g) = d.links.google.as_mut() {
            g.status = ConnStatus::Reconnect;
            g.last_error = Some(why.to_owned());
        }
        let _ = self.save_links(&d);
    }

    /// A Calendar API call with one refresh-and-retry on 401.
    pub async fn google_api(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value, GError> {
        let client = self.google.as_ref().ok_or(GError::NotConnected)?;
        let token = self.access_token(false).await?;
        match client.api(method.clone(), path, query, body, &token).await {
            Ok(v) => Ok(v),
            Err((StatusCode::UNAUTHORIZED, _)) => {
                let token = self.access_token(true).await?;
                match client.api(method, path, query, body, &token).await {
                    Ok(v) => Ok(v),
                    Err((StatusCode::UNAUTHORIZED, _)) => {
                        self.mark_reconnect("Google refused the access token").await;
                        Err(GError::Revoked)
                    }
                    Err((_, e)) => Err(e),
                }
            }
            Err((_, e)) => Err(e),
        }
    }

    // -- MCP status ----------------------------------------------------------

    async fn mcp_clients(&self) -> Option<Value> {
        let cfg = self.mcp.as_ref()?;
        if let Some((at, v)) = self
            .mcp_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            if at.elapsed() < Duration::from_secs(30) {
                return v.clone();
            }
        }
        let v = async {
            let r = self
                .http
                .get(&cfg.url)
                .bearer_auth(&cfg.token)
                .timeout(Duration::from_secs(3))
                .send()
                .await
                .ok()?;
            if !r.status().is_success() {
                return None;
            }
            r.json::<Value>().await.ok()
        }
        .await;
        *self.mcp_cache.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((Instant::now(), v.clone()));
        v
    }
}

// -- validation --------------------------------------------------------------------

pub fn bad(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request", msg)
}

pub fn text_field(field: &str, v: &str, max: usize) -> Result<String, ApiError> {
    let t = v.trim();
    if t.is_empty() || t.chars().count() > max {
        return Err(bad(format!("{field}: 1-{max} characters")));
    }
    if t.chars().any(|c| c.is_control()) {
        return Err(bad(format!("{field}: no control characters")));
    }
    Ok(t.to_owned())
}

fn private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            v.is_loopback()
                || v.is_private()
                || v.is_link_local()
                || v.is_unspecified()
                || v.is_broadcast()
                || v.octets()[0] == 100 && (v.octets()[1] & 0xc0) == 64 // 100.64/10 (Tailscale, CGNAT)
        }
        IpAddr::V6(v) => {
            v.is_loopback()
                || v.is_unspecified()
                || (v.segments()[0] & 0xfe00) == 0xfc00
                || (v.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Validate a subscription URL: `https://` (or `webcal(s)://`, rewritten to
/// https), a public host. Returns (url, host).
pub fn ics_url(raw: &str, allow_local: bool) -> Result<(String, String), ApiError> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 2048 {
        return Err(bad("url: 1-2048 characters"));
    }
    let lower = raw.to_ascii_lowercase();
    let fixed = if let Some(rest) = lower
        .strip_prefix("webcals://")
        .or_else(|| lower.strip_prefix("webcal://"))
    {
        let cut = raw.len() - rest.len();
        format!("https://{}", &raw[cut..])
    } else {
        raw.to_owned()
    };
    let url = reqwest::Url::parse(&fixed).map_err(|_| bad("url: not a valid URL"))?;
    let scheme_ok = url.scheme() == "https" || (allow_local && url.scheme() == "http");
    if !scheme_ok {
        return Err(bad(
            "url: use the https:// (or webcal://) address of the calendar",
        ));
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if host.is_empty() {
        return Err(bad("url: no host"));
    }
    if !allow_local {
        let ip = host.trim_matches(['[', ']']).parse::<IpAddr>().ok();
        if host == "localhost"
            || host.ends_with(".localhost")
            || host.ends_with(".internal")
            || ip.is_some_and(private_ip)
        {
            return Err(bad("url: private and local addresses are not allowed"));
        }
    }
    Ok((url.to_string(), host))
}

fn link_url(raw: &str, https_only: bool) -> Result<(String, String), ApiError> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 2048 {
        return Err(bad("url: 1-2048 characters"));
    }
    let url = reqwest::Url::parse(raw).map_err(|_| bad("url: not a valid URL"))?;
    let ok = match url.scheme() {
        "https" => true,
        "http" => !https_only,
        _ => false,
    };
    if !ok {
        return Err(bad(if https_only {
            "url: must be https://"
        } else {
            "url: must be http(s)://"
        }));
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if host.is_empty() {
        return Err(bad("url: no host"));
    }
    Ok((url.to_string(), host))
}

fn icon_field(v: Option<&str>) -> Result<Option<String>, ApiError> {
    match v.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) if s.chars().count() <= 16 && !s.chars().any(char::is_control) => {
            Ok(Some(s.to_owned()))
        }
        Some(_) => Err(bad(
            "icon: at most 16 characters (an emoji or an icon name)",
        )),
    }
}

fn tags_field(v: Option<Vec<String>>) -> Result<Vec<String>, ApiError> {
    let mut out: Vec<String> = Vec::new();
    for t in v.unwrap_or_default() {
        let t = text_field("tags", &t, 24)?.to_lowercase();
        if !out.contains(&t) {
            out.push(t);
        }
    }
    if out.len() > 10 {
        return Err(bad("tags: at most 10"));
    }
    Ok(out)
}

// -- the overview ---------------------------------------------------------------------

fn short_scope(s: &str) -> String {
    s.strip_prefix("https://www.googleapis.com/auth/")
        .unwrap_or(s)
        .to_owned()
}

fn card(id: &str, kind: &str, name: &str, status: &str) -> Value {
    json!({"id": id, "kind": kind, "name": name, "status": status, "actions": []})
}

fn ts_ms(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse::<i64>().ok().or_else(|| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|t| t.timestamp_millis())
        }),
        _ => None,
    }
}

fn ms_iso(ms: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).map(secretary_calendar::model::iso)
}

/// Latest timeline activity per channel (best effort, cached 60 s).
async fn channel_activity(st: &AppState) -> HashMap<String, i64> {
    let hub = &st.links;
    if let Some((at, v)) = hub
        .channel_cache
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
    {
        if at.elapsed() < Duration::from_secs(60) {
            return v.clone();
        }
    }
    let mut out = HashMap::new();
    if let Some(lucina) = st.lucina.as_ref() {
        let q = secretary_core::models::TimelineQuery {
            before: None,
            limit: Some("200".into()),
        };
        if let Ok(call) = secretary_core::Call::timeline(&q) {
            let page =
                tokio::time::timeout(Duration::from_secs(4), lucina.call::<Value>(&call)).await;
            if let Ok(Ok(page)) = page {
                for it in page
                    .get("items")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let (Some(ch), Some(ts)) = (
                        it.get("channel").and_then(Value::as_str),
                        it.get("ts").and_then(ts_ms),
                    ) else {
                        continue;
                    };
                    let e = out.entry(ch.to_owned()).or_insert(ts);
                    *e = (*e).max(ts);
                }
            }
        }
    }
    *hub.channel_cache.lock().unwrap_or_else(|p| p.into_inner()) =
        Some((Instant::now(), out.clone()));
    out
}

fn ai_cards(mcp: Option<&Value>) -> Vec<Value> {
    let known: [(&str, &str, &[&str]); 3] = [
        ("claude-code", "Claude Code", &["claude-code", "claude"]),
        ("chatgpt", "ChatGPT", &["chatgpt"]),
        ("grok", "Grok Bot", &["grok", "www", "cursor"]),
    ];
    let clients: Vec<Value> = mcp
        .and_then(|v| v.get("clients"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut used = vec![false; clients.len()];
    let mut out = Vec::new();
    let summarize = |c: &[&Value], id: &str, name: &str| -> Value {
        let active: Vec<&&Value> = c
            .iter()
            .filter(|x| x.get("active").and_then(Value::as_bool).unwrap_or(false))
            .collect();
        let mut v = card(
            &format!("ai:{id}"),
            "ai",
            name,
            if active.is_empty() {
                "disconnected"
            } else {
                "connected"
            },
        );
        let mut scopes: Vec<String> = active
            .iter()
            .flat_map(|x| {
                x.get("scopes")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            })
            .filter_map(|s| s.as_str().map(str::to_owned))
            .collect();
        scopes.sort();
        scopes.dedup();
        v["scopes"] = json!(scopes);
        let vias: Vec<&str> = active
            .iter()
            .filter_map(|x| x.get("via").and_then(Value::as_str))
            .collect();
        if !vias.is_empty() {
            v["account"] = json!(if vias.contains(&"oauth") {
                "OAuth"
            } else {
                "MCP token"
            });
        }
        let last = active
            .iter()
            .filter_map(|x| x.get("lastUsedAt").and_then(ts_ms))
            .max();
        if let Some(l) = last.and_then(ms_iso) {
            v["lastSyncAt"] = json!(l);
        }
        v["detail"] = json!("Lucina MCP");
        v
    };
    if mcp.is_none() {
        for (id, name, _) in known {
            let mut v = card(&format!("ai:{id}"), "ai", name, "unknown");
            v["detail"] = json!("Lucina MCP status is not available");
            out.push(v);
        }
        return out;
    }
    for (id, name, callers) in known {
        let mine: Vec<&Value> = clients
            .iter()
            .enumerate()
            .filter(|(i, c)| {
                let hit = c
                    .get("caller")
                    .and_then(Value::as_str)
                    .is_some_and(|x| callers.contains(&x));
                if hit {
                    used[*i] = true;
                }
                hit
            })
            .map(|(_, c)| c)
            .collect();
        out.push(summarize(&mine, id, name));
    }
    let mut others: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for (i, c) in clients.iter().enumerate() {
        if !used[i] {
            if let Some(name) = c.get("caller").and_then(Value::as_str) {
                others.entry(name.to_owned()).or_default().push(c);
            }
        }
    }
    for (name, cs) in others {
        out.push(summarize(&cs, &name, &name));
    }
    out
}

async fn overview(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let hub = &st.links;
    let mcp = hub.mcp_clients().await;
    let activity = channel_activity(&st).await;
    let d = hub.data.lock().await;
    let configured = hub.google.is_some();

    // Google
    let (g_status, g_account, g_scopes, g_services) = match &d.links.google {
        None => ("disconnected", None, Vec::new(), Vec::new()),
        Some(g) => (
            if g.status == ConnStatus::Reconnect {
                "reconnect"
            } else {
                "connected"
            },
            g.account.clone(),
            g.scopes.clone(),
            google::services_of(&g.scopes),
        ),
    };
    let google_card = |id: &str, service: &str, name: &str| -> Value {
        let has = g_services.contains(&service);
        let status = if !configured {
            "unavailable"
        } else if d.links.google.is_some()
            && (has || service == "calendar")
            && g_status == "reconnect"
        {
            "reconnect"
        } else if has {
            "connected"
        } else if service == "calendar" {
            "disconnected"
        } else {
            "available"
        };
        let mut v = card(id, "google", name, status);
        v["service"] = json!(service);
        if has {
            v["account"] = json!(g_account);
            let wanted = google::service_scopes(service).unwrap_or(&[]);
            v["scopes"] = json!(g_scopes
                .iter()
                .filter(|s| wanted.contains(&s.as_str()))
                .map(|s| short_scope(s))
                .collect::<Vec<_>>());
        }
        v["actions"] = json!(match status {
            "connected" if service == "calendar" => vec!["sync", "disconnect"],
            "connected" => vec!["disconnect"],
            "reconnect" => vec!["reconnect", "disconnect"],
            "disconnected" | "available" => vec!["connect"],
            _ => vec![],
        });
        if !configured {
            v["detail"] = json!("Google sign-in is not configured on the server yet");
        }
        v
    };
    let mut gcal = google_card("google-calendar", "calendar", "Google Calendar");
    if let Some(g) = &d.links.google {
        if g_services.contains(&"calendar") {
            gcal["lastSyncAt"] = json!(g.last_sync_at);
            let n = d.cache.google.items.len();
            gcal["detail"] = json!(format!("{n} items synced"));
            gcal["count"] = json!(n);
        }
        if let Some(e) = &g.last_error {
            gcal["error"] = json!(e);
        }
    }

    // iPhone (EventKit snapshot from the app)
    let ip = &d.cache.iphone;
    let mut iphone = card(
        "iphone-calendar",
        "iphone",
        "iPhone Calendar",
        if ip.reported_at.is_some() {
            "connected"
        } else {
            "disconnected"
        },
    );
    iphone["account"] = json!(ip.device);
    iphone["lastSyncAt"] = json!(ip.reported_at);
    iphone["count"] = json!(ip.items.len());
    iphone["detail"] = json!(if ip.reported_at.is_some() {
        format!("{} events from the app's calendar bridge", ip.items.len())
    } else {
        "Open the Secretary app on the iPhone and allow Calendar access".to_owned()
    });
    iphone["scopes"] = json!(["eventkit.read", "eventkit.write"]);

    let mut calendars = vec![gcal, iphone];
    for s in &d.links.ics {
        let mut v = card(
            &format!("ics:{}", s.id),
            "ics",
            &s.label,
            if s.last_error.is_some() {
                "error"
            } else {
                "connected"
            },
        );
        v["account"] = json!(s.host);
        v["lastSyncAt"] = json!(s.last_sync_at);
        v["count"] = json!(s.event_count);
        v["detail"] = json!(s.feed_name);
        v["error"] = json!(s.last_error);
        v["scopes"] = json!(["read-only"]);
        v["actions"] = json!(["sync", "remove"]);
        calendars.push(v);
    }

    // Chat channels (Lucina's); activity from the timeline when Lucina answers.
    let channel = |id: &str, name: &str, status: &str, detail: &str| -> Value {
        let mut v = card(&format!("channel:{id}"), "channel", name, status);
        v["detail"] = json!(detail);
        if let Some(t) = activity.get(id).copied().and_then(ms_iso) {
            v["lastSyncAt"] = json!(t);
        }
        v
    };
    let channels = vec![
        channel(
            "telegram",
            "Telegram",
            "connected",
            "Lucina's Telegram bot; reminders are delivered here",
        ),
        channel(
            "discord",
            "Discord",
            "connected",
            "Lucina on Discord; presence watcher active",
        ),
        channel(
            "alexa",
            "Alexa",
            "planned",
            "Alexa skill (Rust backend) is planned",
        ),
    ];

    let ai = ai_cards(mcp.as_ref());

    let services = vec![
        google_card("google-gmail", "gmail", "Gmail"),
        google_card("google-drive", "drive", "Google Drive"),
    ];

    let custom: Vec<Value> = d
        .links
        .custom
        .iter()
        .map(|c| {
            let mut v = card(
                &format!("custom:{}", c.id),
                if c.kind == LinkKind::Mcp {
                    "mcp"
                } else {
                    "bookmark"
                },
                &c.label,
                "connected",
            );
            if c.kind == LinkKind::Bookmark {
                v["url"] = json!(c.url);
            } else {
                v["status"] = json!("saved");
                v["detail"] = json!("Remote MCP server, stored for Lucina (not called yet)");
            }
            v["account"] = json!(c.host);
            v["icon"] = json!(c.icon);
            v["tags"] = json!(c.tags);
            v["lastSyncAt"] = json!(c.created_at);
            v["actions"] = json!(["edit", "remove"]);
            v
        })
        .collect();

    let now = chrono::Utc::now().timestamp_millis();
    let proposals: Vec<&Proposal> = d
        .cache
        .proposals
        .iter()
        .filter(|p| p.expires_at_ms > now)
        .collect();
    Ok(Json(json!({
        "timeZone": hub.tz.name(),
        "google": {"configured": configured, "status": g_status, "account": g_account, "services": g_services},
        "groups": [
            {"id": "calendars", "cards": calendars},
            {"id": "channels", "cards": channels},
            {"id": "ai", "cards": ai},
            {"id": "google", "cards": services},
            {"id": "custom", "cards": custom},
        ],
        "proposals": proposals,
    })))
}

// -- Google connect / callback / disconnect ----------------------------------------------

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ConnectBody {
    #[serde(default)]
    services: Option<Vec<String>>,
    /// `web` (default) or `ios`: where the callback returns to.
    #[serde(default)]
    client: Option<String>,
}

async fn google_connect(
    State(st): State<AppState>,
    JsonBody(body): JsonBody<ConnectBody>,
) -> Result<Json<Value>, ApiError> {
    let hub = &st.links;
    let client = hub.google.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "google_not_configured",
            "Google sign-in is not configured on the server",
        )
    })?;
    let mut services = body.services.unwrap_or_else(|| vec!["calendar".into()]);
    if services.is_empty() {
        services.push("calendar".into());
    }
    let mut scopes: Vec<&str> = Vec::new();
    for s in &services {
        let sc =
            google::service_scopes(s).ok_or_else(|| bad("services: calendar, gmail, drive"))?;
        scopes.extend_from_slice(sc);
    }
    // Keep what is already granted (include_granted_scopes does it on
    // Google's side; asking again keeps the consent screen honest).
    {
        let d = hub.data.lock().await;
        if let Some(g) = &d.links.google {
            for s in google::services_of(&g.scopes) {
                scopes.extend_from_slice(google::service_scopes(s).unwrap_or(&[]));
            }
        }
    }
    scopes.sort_unstable();
    scopes.dedup();
    let app = match body.client.as_deref() {
        None | Some("web") => "web",
        Some("ios") => "ios",
        Some(_) => return Err(bad("client: web or ios")),
    };
    let state = random_secret();
    let verifier = random_secret();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    {
        let mut p = hub.pending.lock().unwrap_or_else(|e| e.into_inner());
        p.retain(|x| x.created.elapsed() < PENDING_TTL);
        if p.len() >= 5 {
            p.remove(0);
        }
        p.push(PendingOAuth {
            state_hash: sha(&state),
            verifier,
            client: app.into(),
            created: Instant::now(),
        });
    }
    Ok(Json(
        json!({ "authorizeUrl": client.authorize_url(&state, &challenge, &scopes) }),
    ))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

fn back_to_app(client: &str, outcome: &str, reason: Option<&str>) -> Response {
    let base = if client == "ios" {
        "secretary://links"
    } else {
        "/links"
    };
    let mut url = format!("{base}?google={outcome}");
    if let Some(r) = reason {
        let r: String = r
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(40)
            .collect();
        url.push_str(&format!("&reason={r}"));
    }
    let mut resp = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = HeaderValue::from_str(&url) {
        resp.headers_mut().insert(header::LOCATION, v);
    }
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

async fn google_callback(State(st): State<AppState>, Query(q): Query<CallbackQuery>) -> Response {
    let hub = &st.links;
    let Some(state) = q.state.as_deref().filter(|s| s.len() <= 128) else {
        return back_to_app("web", "error", Some("missing_state"));
    };
    let pending = {
        let mut p = hub.pending.lock().unwrap_or_else(|e| e.into_inner());
        p.retain(|x| x.created.elapsed() < PENDING_TTL);
        let h = sha(state);
        let pos = p
            .iter()
            .position(|x| bool::from(subtle::ConstantTimeEq::ct_eq(&x.state_hash[..], &h[..])));
        pos.map(|i| p.remove(i))
    };
    let Some(pending) = pending else {
        return back_to_app("web", "error", Some("expired_or_unknown_state"));
    };
    if let Some(e) = q.error.as_deref() {
        return back_to_app(&pending.client, "error", Some(e));
    }
    let (Some(code), Some(client)) = (
        q.code.as_deref().filter(|c| c.len() <= 2048),
        hub.google.as_ref(),
    ) else {
        return back_to_app(&pending.client, "error", Some("missing_code"));
    };
    let tok = match client.exchange(code, &pending.verifier).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "google code exchange failed");
            return back_to_app(&pending.client, "error", Some("exchange_failed"));
        }
    };
    let scopes: Vec<String> = tok
        .scope
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let previous = hub.data.lock().await.links.google.clone();
    let refresh = match (&tok.refresh_token, &previous) {
        (Some(r), _) => match hub.sealer.seal("google-refresh", r) {
            Ok(s) => s,
            Err(_) => return back_to_app(&pending.client, "error", Some("storage")),
        },
        (None, Some(p)) => p.refresh_token.clone(),
        (None, None) => return back_to_app(&pending.client, "error", Some("no_refresh_token")),
    };
    *hub.access.lock().await = Some((
        tok.access_token.clone(),
        Instant::now()
            + Duration::from_secs(tok.expires_in.unwrap_or(3600).saturating_sub(60).max(30)),
    ));
    // The primary calendar's id is the account's email.
    let mut account = previous.as_ref().and_then(|p| p.account.clone());
    let mut cal_tz = None;
    if google::services_of(&scopes).contains(&"calendar") {
        if let Ok(v) = client
            .api(
                reqwest::Method::GET,
                "/calendar/v3/calendars/primary",
                &[],
                None,
                &tok.access_token,
            )
            .await
        {
            account = v
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(account);
            cal_tz = v.get("timeZone").and_then(Value::as_str).map(str::to_owned);
        }
    }
    {
        let mut d = hub.data.lock().await;
        let switched = previous.as_ref().and_then(|p| p.account.as_deref()) != account.as_deref();
        if switched {
            d.cache.google = GoogleCache::default();
        }
        if cal_tz.is_some() {
            d.cache.google.calendar_tz = cal_tz;
        }
        d.links.google = Some(GoogleConn {
            account,
            scopes,
            refresh_token: refresh,
            connected_at: now_iso(),
            status: ConnStatus::Ok,
            last_error: None,
            last_sync_at: previous.and_then(|p| if switched { None } else { p.last_sync_at }),
        });
        if hub.save_links(&d).is_err() {
            return back_to_app(&pending.client, "error", Some("storage"));
        }
        hub.save_cache(&d);
    }
    let st2 = st.clone();
    tokio::spawn(async move {
        let _ = crate::calendar::sync_all(&st2).await;
    });
    back_to_app(&pending.client, "connected", None)
}

async fn google_disconnect(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let hub = &st.links;
    let conn = {
        let mut d = hub.data.lock().await;
        let c = d.links.google.take();
        d.cache.google = GoogleCache::default();
        hub.save_links(&d)?;
        hub.save_cache(&d);
        c
    };
    *hub.access.lock().await = None;
    let mut revoked = false;
    if let (Some(c), Some(client)) = (conn, hub.google.as_ref()) {
        if let Ok(r) = hub.sealer.open("google-refresh", &c.refresh_token) {
            revoked = client.revoke(&r).await.is_ok();
        }
    }
    Ok(Json(
        json!({"disconnected": true, "revokedAtGoogle": revoked}),
    ))
}

// -- ICS subscriptions --------------------------------------------------------------------

#[derive(Deserialize)]
struct IcsBody {
    label: Option<String>,
    url: String,
}

async fn ics_add(
    State(st): State<AppState>,
    JsonBody(body): JsonBody<IcsBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let hub = &st.links;
    let (url, host) = ics_url(&body.url, hub.ics_allow_local)?;
    let label = match body
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(l) => text_field("label", l, 80)?,
        None => host.clone(),
    };
    if hub.data.lock().await.links.ics.len() >= MAX_ICS {
        return Err(bad(format!("at most {MAX_ICS} subscriptions")));
    }
    // Fetch once: a wrong address fails here, not silently later.
    let fetched = crate::calendar::fetch_ics(hub, &url, None, None)
        .await
        .map_err(|e| {
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "ics_unavailable",
                format!("could not read that calendar: {e}"),
            )
        })?;
    let crate::calendar::IcsFetch::Fresh {
        items,
        name,
        etag,
        last_modified,
    } = fetched
    else {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "ics_unavailable",
            "the calendar sent no data",
        ));
    };
    let id = random_id("ics_");
    let sub = IcsSub {
        id: id.clone(),
        label,
        url: hub.seal(&format!("ics-url:{id}"), &url)?,
        host,
        added_at: now_iso(),
        last_sync_at: Some(now_iso()),
        last_error: None,
        event_count: items.len(),
        etag,
        last_modified,
        feed_name: name,
    };
    let mut d = hub.data.lock().await;
    d.links.ics.push(sub.clone());
    d.cache.ics.insert(id.clone(), items);
    hub.save_links(&d)?;
    hub.save_cache(&d);
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"id": id, "label": sub.label, "host": sub.host, "eventCount": sub.event_count, "feedName": sub.feed_name}),
        ),
    ))
}

async fn ics_remove(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let hub = &st.links;
    let mut d = hub.data.lock().await;
    let before = d.links.ics.len();
    d.links.ics.retain(|s| s.id != id);
    if d.links.ics.len() == before {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "link_not_found",
            "no such subscription",
        ));
    }
    d.cache.ics.remove(&id);
    hub.save_links(&d)?;
    hub.save_cache(&d);
    Ok(Json(json!({"removed": true})))
}

// -- custom links --------------------------------------------------------------------------

#[derive(Deserialize)]
struct CustomBody {
    label: Option<String>,
    url: Option<String>,
    icon: Option<String>,
    tags: Option<Vec<String>>,
    kind: Option<LinkKind>,
}

fn custom_view(c: &CustomLink) -> Value {
    json!({
        "id": c.id, "kind": c.kind, "label": c.label, "host": c.host,
        "url": if c.kind == LinkKind::Bookmark { Some(c.url.clone()) } else { None },
        "icon": c.icon, "tags": c.tags, "createdAt": c.created_at,
    })
}

async fn custom_add(
    State(st): State<AppState>,
    JsonBody(b): JsonBody<CustomBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let hub = &st.links;
    let kind = b.kind.unwrap_or_default();
    let label = text_field("label", b.label.as_deref().unwrap_or(""), 80)?;
    let (url, host) = link_url(b.url.as_deref().unwrap_or(""), kind == LinkKind::Mcp)?;
    let id = random_id("lnk_");
    let stored = if kind == LinkKind::Mcp {
        hub.seal(&format!("mcp-url:{id}"), &url)?
    } else {
        url
    };
    let link = CustomLink {
        id,
        kind,
        label,
        url: stored,
        host,
        icon: icon_field(b.icon.as_deref())?,
        tags: tags_field(b.tags)?,
        created_at: now_iso(),
    };
    let mut d = hub.data.lock().await;
    if d.links.custom.len() >= MAX_CUSTOM {
        return Err(bad(format!("at most {MAX_CUSTOM} links")));
    }
    d.links.custom.push(link.clone());
    hub.save_links(&d)?;
    Ok((StatusCode::CREATED, Json(custom_view(&link))))
}

async fn custom_update(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
    JsonBody(b): JsonBody<CustomBody>,
) -> Result<Json<Value>, ApiError> {
    let hub = &st.links;
    let mut d = hub.data.lock().await;
    let pos = d
        .links
        .custom
        .iter()
        .position(|c| c.id == id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, "link_not_found", "no such link"))?;
    let mut link = d.links.custom[pos].clone();
    if b.kind.is_some_and(|k| k != link.kind) {
        return Err(bad("kind cannot change; add a new link instead"));
    }
    if let Some(l) = b.label.as_deref() {
        link.label = text_field("label", l, 80)?;
    }
    if let Some(u) = b.url.as_deref() {
        let (url, host) = link_url(u, link.kind == LinkKind::Mcp)?;
        link.url = if link.kind == LinkKind::Mcp {
            hub.seal(&format!("mcp-url:{}", link.id), &url)?
        } else {
            url
        };
        link.host = host;
    }
    if b.icon.is_some() {
        link.icon = icon_field(b.icon.as_deref())?;
    }
    if b.tags.is_some() {
        link.tags = tags_field(b.tags)?;
    }
    d.links.custom[pos] = link.clone();
    hub.save_links(&d)?;
    Ok(Json(custom_view(&link)))
}

async fn custom_remove(
    State(st): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let hub = &st.links;
    let mut d = hub.data.lock().await;
    let before = d.links.custom.len();
    d.links.custom.retain(|c| c.id != id);
    if d.links.custom.len() == before {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "link_not_found",
            "no such link",
        ));
    }
    hub.save_links(&d)?;
    Ok(Json(json!({"removed": true})))
}

/// Who may not manage links (Lucina's service token is read/write for the
/// calendar only).
fn owner_only(ident: &Identity) -> Result<(), ApiError> {
    if ident.device_id == crate::auth::SERVICE_DEVICE_ID {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "owner_only",
            "only the owner's apps manage links",
        ));
    }
    Ok(())
}

async fn guard(
    Extension(ident): Extension<Identity>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<Response, ApiError> {
    owner_only(&ident)?;
    Ok(next.run(req).await)
}

/// Authenticated links routes (device auth; behind `require_device`).
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/links", get(overview))
        .route("/v1/links/google/connect", post(google_connect))
        .route("/v1/links/google", delete(google_disconnect))
        .route("/v1/links/ics", post(ics_add))
        .route("/v1/links/ics/{id}", delete(ics_remove))
        .route("/v1/links/custom", post(custom_add))
        .route(
            "/v1/links/custom/{id}",
            axum::routing::patch(custom_update).delete(custom_remove),
        )
        .route_layer(axum::middleware::from_fn(guard))
}

/// The OAuth callback (no auth: the `state` binds it).
pub fn public_routes() -> Router<AppState> {
    Router::new().route("/v1/links/google/callback", get(google_callback))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_rules() {
        assert_eq!(
            ics_url("webcal://p01-calendars.icloud.com/published/2/abc", false).unwrap(),
            (
                "https://p01-calendars.icloud.com/published/2/abc".into(),
                "p01-calendars.icloud.com".into()
            )
        );
        assert!(ics_url("WEBCALS://Example.com/x.ics", false)
            .unwrap()
            .0
            .starts_with("https://"));
        for bad_url in [
            "http://example.com/x.ics",
            "https://localhost/x",
            "https://127.0.0.1/x",
            "https://10.0.0.5/x",
            "https://100.107.164.33/x",
            "https://[::1]/x",
            "ftp://x/y",
            "nonsense",
            "",
        ] {
            assert!(ics_url(bad_url, false).is_err(), "{bad_url}");
        }
        assert!(ics_url("http://127.0.0.1:9/x", true).is_ok());
        assert!(link_url("https://x.dev/mcp", true).is_ok());
        assert!(link_url("http://x.dev/mcp", true).is_err());
        assert!(link_url("http://x.dev", false).is_ok());
        assert!(link_url("javascript:alert(1)", false).is_err());
        assert_eq!(
            tags_field(Some(vec!["Work".into(), "work".into()])).unwrap(),
            vec!["work"]
        );
        assert!(icon_field(Some("🗓️")).unwrap().is_some());
    }

    #[test]
    fn ai_cards_from_mcp_status() {
        let st = json!({"clients": [
            {"caller": "claude-code", "via": "token", "scopes": ["read", "chat"], "active": true, "lastUsedAt": 1792602906000_i64},
            {"caller": "chatgpt", "via": "oauth", "scopes": ["read", "write"], "active": true},
            {"caller": "www", "via": "oauth", "scopes": ["read"], "active": true},
            {"caller": "old", "via": "token", "scopes": ["read"], "active": false},
        ]});
        let cards = ai_cards(Some(&st));
        let by = |id: &str| cards.iter().find(|c| c["id"] == id).unwrap().clone();
        assert_eq!(by("ai:claude-code")["status"], "connected");
        assert_eq!(by("ai:claude-code")["scopes"], json!(["chat", "read"]));
        assert_eq!(by("ai:chatgpt")["account"], "OAuth");
        assert_eq!(by("ai:grok")["status"], "connected");
        assert_eq!(by("ai:old")["status"], "disconnected");
        assert!(ai_cards(None).iter().all(|c| c["status"] == "unknown"));
    }
}
